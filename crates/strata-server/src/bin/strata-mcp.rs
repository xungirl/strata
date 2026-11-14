//! `strata-mcp`: strata as a **Model Context Protocol** server, so LLM agents
//! (Claude Desktop, Claude Code, any MCP client) can use it as long-term
//! memory and as a search tool.
//!
//! ```text
//! strata-mcp --dir ~/.strata-memory        # speaks MCP (JSON-RPC 2.0) on stdin/stdout
//! ```
//!
//! Tools:
//! * `remember` - store a memory (text + optional id and tags)
//! * `remember_many` - store a batch of memories in one durable write
//! * `recall` - find memories relevant to a natural-language question,
//!   optionally restricted to memories with a given tag value
//! * `search` - full strata query language (`+must -not "phrase" pre* field:value`)
//! * `forget` - delete a memory by id
//! * `memory_stats` - how many memories are stored
//!
//! Built on the official Rust MCP SDK (`rmcp`). Text is returned with every
//! hit, so the memory texts are kept in an append-only `memories.jsonl`
//! next to the index (the index itself stores only terms and fields).
//! Writes are durable when a tool call returns (WAL fsync + memories file
//! fsync). Logs go to stderr, as stdout is the protocol channel.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use clap::Parser;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData, ServerHandler, ServiceExt};
use serde::{Deserialize, Serialize};
use strata_query::Filter;
use strata_server::engine::{Engine, EngineConfig, IngestDoc, Mode, SearchRequest};
use strata_storage::{FieldValue, Fields};
use strata_vector::HnswConfig;

#[derive(Parser)]
struct Args {
    /// Data directory (created if missing).
    #[arg(long)]
    dir: PathBuf,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RememberArgs {
    /// The text to remember.
    text: String,
    /// Optional stable id (an existing memory with this id is replaced).
    #[serde(default)]
    id: Option<String>,
    /// Optional tags, e.g. {"user": "alice", "session": "12"}; usable as
    /// filters in `recall` and `search`.
    #[serde(default)]
    tags: HashMap<String, String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RememberManyArgs {
    memories: Vec<RememberArgs>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RecallArgs {
    /// A natural-language question or keywords.
    query: String,
    /// How many memories to return (default 5, max 100).
    #[serde(default)]
    k: Option<usize>,
    /// Only memories whose tag `tag_key` equals `tag_value`.
    #[serde(default)]
    tag_key: Option<String>,
    #[serde(default)]
    tag_value: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchArgs {
    /// strata query language: words, +must, -not, "phrases", prefix*, tag:value.
    query: String,
    #[serde(default)]
    k: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ForgetArgs {
    id: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct Memory {
    id: String,
    text: String,
    score: f32,
    tags: HashMap<String, String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct Memories {
    memories: Vec<Memory>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct Ack {
    stored: usize,
    ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct StoredMemory {
    id: String,
    text: String,
    tags: HashMap<String, String>,
    #[serde(default)]
    deleted: bool,
}

struct Store {
    engine: Engine,
    texts: Mutex<HashMap<String, StoredMemory>>,
    log: Mutex<File>,
    next_id: Mutex<u64>,
}

#[derive(Clone)]
struct MemoryServer {
    store: Arc<Store>,
    tool_router: ToolRouter<MemoryServer>,
}

fn err(e: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(format!("{e:#}"), None)
}

/// Tag names become keyword fields; keep them to the field-name alphabet.
fn field_name(tag: &str) -> String {
    tag.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

impl MemoryServer {
    fn store_batch(&self, items: Vec<RememberArgs>) -> Result<Ack> {
        let mut docs = Vec::new();
        let mut stored = Vec::new();
        {
            let mut next = self.store.next_id.lock().unwrap();
            for m in items {
                let id = m.id.unwrap_or_else(|| {
                    *next += 1;
                    format!("m{next}")
                });
                let fields: Fields = m
                    .tags
                    .iter()
                    .map(|(k, v)| (field_name(k), FieldValue::Keyword(v.clone())))
                    .collect();
                docs.push(IngestDoc {
                    id: id.clone(),
                    text: m.text.clone(),
                    fields,
                    vector: None,
                });
                stored.push(StoredMemory {
                    id,
                    text: m.text,
                    tags: m.tags,
                    deleted: false,
                });
            }
        }
        // Text first (so a recalled id always has its text), then the index.
        {
            let mut log = self.store.log.lock().unwrap();
            for s in &stored {
                writeln!(log, "{}", serde_json::to_string(s)?)?;
            }
            log.sync_data()?;
        }
        self.store.engine.ingest(docs)?;
        // Make the memories visible to the next recall right away.
        self.store.engine.commit()?;
        let ids: Vec<String> = stored.iter().map(|s| s.id.clone()).collect();
        let mut texts = self.store.texts.lock().unwrap();
        for s in stored {
            texts.insert(s.id.clone(), s);
        }
        Ok(Ack {
            stored: ids.len(),
            ids,
        })
    }

    fn run_search(&self, query: String, k: usize, plain: bool) -> Result<Memories> {
        let res = self.store.engine.search(&SearchRequest {
            query,
            k: k.clamp(1, 100),
            mode: Mode::Lexical,
            vector: None,
            plain,
            global_stats: None,
        })?;
        Ok(self.with_texts(res.hits))
    }

    fn with_texts(&self, hits: Vec<strata_server::engine::SearchHit>) -> Memories {
        let texts = self.store.texts.lock().unwrap();
        Memories {
            memories: hits
                .into_iter()
                .map(|h| {
                    let m = texts.get(&h.id);
                    Memory {
                        text: m.map(|m| m.text.clone()).unwrap_or_default(),
                        tags: m.map(|m| m.tags.clone()).unwrap_or_default(),
                        id: h.id,
                        score: h.score,
                    }
                })
                .collect(),
        }
    }
}

#[tool_router]
impl MemoryServer {
    #[tool(
        description = "Store one memory (a fact, note or message) so it can be recalled later. Optional tags (e.g. user, session) can be used to filter recall."
    )]
    fn remember(&self, Parameters(args): Parameters<RememberArgs>) -> Result<Json<Ack>, ErrorData> {
        self.store_batch(vec![args]).map(Json).map_err(err)
    }

    #[tool(description = "Store many memories in one durable write.")]
    fn remember_many(
        &self,
        Parameters(args): Parameters<RememberManyArgs>,
    ) -> Result<Json<Ack>, ErrorData> {
        self.store_batch(args.memories).map(Json).map_err(err)
    }

    #[tool(
        description = "Recall the memories most relevant to a question (BM25 over the memory texts). Optionally only memories whose tag `tag_key` equals `tag_value`."
    )]
    fn recall(
        &self,
        Parameters(args): Parameters<RecallArgs>,
    ) -> Result<Json<Memories>, ErrorData> {
        // The question is matched as a plain bag of words (no query syntax),
        // and the tag filter is added as an exact keyword filter.
        let k = args.k.unwrap_or(5);
        let run = || -> Result<Memories> {
            let mut q = self.store.engine.parse_query(&args.query, true)?;
            if let (Some(key), Some(value)) = (&args.tag_key, &args.tag_value) {
                q.filters.push(Filter::KeywordEq {
                    field: field_name(key),
                    value: value.clone(),
                });
            }
            let hits = self.store.engine.search_bool(&q, k.clamp(1, 100))?;
            Ok(self.with_texts(hits))
        };
        let res = run();
        res.map(Json).map_err(err)
    }

    #[tool(
        description = "Search memories with the strata query language: words, +must, -mustnot, \"exact phrase\", prefix*, tag:value, tag:[lo TO hi]."
    )]
    fn search(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<Json<Memories>, ErrorData> {
        self.run_search(args.query, args.k.unwrap_or(5), false)
            .map(Json)
            .map_err(err)
    }

    #[tool(description = "Delete a memory by id.")]
    fn forget(&self, Parameters(args): Parameters<ForgetArgs>) -> Result<Json<Ack>, ErrorData> {
        let run = || -> Result<Ack> {
            {
                let mut log = self.store.log.lock().unwrap();
                let tomb = StoredMemory {
                    id: args.id.clone(),
                    text: String::new(),
                    tags: HashMap::new(),
                    deleted: true,
                };
                writeln!(log, "{}", serde_json::to_string(&tomb)?)?;
                log.sync_data()?;
            }
            self.store.engine.delete(std::slice::from_ref(&args.id))?;
            self.store.engine.commit()?;
            self.store.texts.lock().unwrap().remove(&args.id);
            Ok(Ack {
                stored: 0,
                ids: vec![args.id.clone()],
            })
        };
        run().map(Json).map_err(err)
    }

    #[tool(description = "Number of stored memories.")]
    fn memory_stats(&self) -> Result<Json<serde_json::Value>, ErrorData> {
        let n = self.store.texts.lock().unwrap().len();
        Ok(Json(serde_json::json!({ "memories": n })))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MemoryServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("strata-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions("strata long-term memory: `remember` facts, `recall` them later by question; tags filter by user/session.")
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // stdout is the MCP channel: logs must go to stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let args = Args::parse();
    std::fs::create_dir_all(&args.dir)?;
    let engine = Engine::open(&EngineConfig {
        dir: args.dir.join("engine"),
        flush_docs: 5_000,
        merge_factor: 4,
        dim: None,
        hnsw: HnswConfig::default(),
        ef_search: 100,
    })?;
    // Rebuild the text map from the append-only memories file.
    let path = args.dir.join("memories.jsonl");
    let mut texts = HashMap::new();
    let mut max_auto = 0u64;
    if path.exists() {
        for line in BufReader::new(File::open(&path)?).lines() {
            let Ok(m) = serde_json::from_str::<StoredMemory>(&line?) else {
                continue; // torn last line of an unacknowledged write
            };
            if let Some(n) = m.id.strip_prefix('m').and_then(|n| n.parse::<u64>().ok()) {
                max_auto = max_auto.max(n);
            }
            if m.deleted {
                texts.remove(&m.id);
            } else {
                texts.insert(m.id.clone(), m);
            }
        }
    }
    let log = OpenOptions::new().create(true).append(true).open(&path)?;
    let server = MemoryServer {
        store: Arc::new(Store {
            engine,
            texts: Mutex::new(texts),
            log: Mutex::new(log),
            next_id: Mutex::new(max_auto),
        }),
        tool_router: MemoryServer::tool_router(),
    };
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
