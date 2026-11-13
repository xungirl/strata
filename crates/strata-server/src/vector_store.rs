//! Durable vector storage + an in-memory HNSW graph for a node.
//!
//! Vectors are appended to `vectors.log` (one CRC-checked record per vector,
//! fsynced per batch, like the lexical WAL). On startup the log is replayed
//! and the HNSW graph rebuilt from it; rebuilding is O(n log n) and takes
//! seconds for the BEIR corpora (BENCHMARK.md, stage 4). A persistent graph
//! file would make restarts instant; not done yet.
//!
//! Upserts append a new node and hide the old one: HNSW graphs do not
//! support true deletion, so replaced/deleted vectors stay in the graph as
//! routing nodes but are filtered out of results.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use anyhow::{bail, Context, Result};
use strata_vector::{Hnsw, HnswConfig, SearchContext, Vectors};

struct State {
    vectors: Vectors,
    ids: Vec<String>,
    live: Vec<bool>,
    by_id: HashMap<String, u32>,
    hnsw: Hnsw,
}

pub struct VectorStore {
    dim: usize,
    path: PathBuf,
    state: RwLock<State>,
    log: Mutex<File>,
    /// Reusable search scratch buffers (one per concurrent search).
    contexts: Mutex<Vec<SearchContext>>,
    ef_search: usize,
}

fn encode(id: &str, v: &[f32], out: &mut Vec<u8>) {
    let mut payload = Vec::with_capacity(4 + id.len() + 4 * v.len());
    payload.extend_from_slice(&(id.len() as u32).to_le_bytes());
    payload.extend_from_slice(id.as_bytes());
    for x in v {
        payload.extend_from_slice(&x.to_le_bytes());
    }
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
    out.extend_from_slice(&payload);
}

impl VectorStore {
    /// Open (or create) `dir/vectors.log` and rebuild the graph.
    pub fn open(dir: &Path, dim: usize, hnsw: HnswConfig, ef_search: usize) -> Result<VectorStore> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("vectors.log");
        let data = if path.exists() {
            std::fs::read(&path)?
        } else {
            Vec::new()
        };
        let mut state = State {
            vectors: Vectors::new(dim, Vec::new()),
            ids: Vec::new(),
            live: Vec::new(),
            by_id: HashMap::new(),
            hnsw: Hnsw::new(hnsw),
        };
        let mut ctx = SearchContext::new(0);
        let mut pos = 0;
        while pos + 8 <= data.len() {
            let len = u32::from_le_bytes(data[pos..pos + 4].try_into()?) as usize;
            let crc = u32::from_le_bytes(data[pos + 4..pos + 8].try_into()?);
            if pos + 8 + len > data.len() || crc32fast::hash(&data[pos + 8..pos + 8 + len]) != crc {
                break; // torn tail of an un-acknowledged append
            }
            let p = &data[pos + 8..pos + 8 + len];
            let id_len = u32::from_le_bytes(p[..4].try_into()?) as usize;
            let id = std::str::from_utf8(&p[4..4 + id_len])?.to_string();
            let v: Vec<f32> = p[4 + id_len..]
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect();
            if v.len() != dim {
                bail!("vector of dim {} in log, expected {dim}", v.len());
            }
            Self::insert(&mut state, &id, &v, &mut ctx);
            pos += 8 + len;
        }
        if pos < data.len() {
            OpenOptions::new()
                .write(true)
                .open(&path)?
                .set_len(pos as u64)?;
        }
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        Ok(VectorStore {
            dim,
            path,
            state: RwLock::new(state),
            log: Mutex::new(log),
            contexts: Mutex::new(Vec::new()),
            ef_search,
        })
    }

    fn insert(state: &mut State, id: &str, v: &[f32], ctx: &mut SearchContext) {
        let node = state.ids.len() as u32;
        if let Some(old) = state.by_id.insert(id.to_string(), node) {
            state.live[old as usize] = false;
        }
        state.vectors.push(v);
        state.ids.push(id.to_string());
        state.live.push(true);
        state.hnsw.insert(&state.vectors, node, ctx);
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn len(&self) -> usize {
        self.state.read().unwrap().ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Durably append `batch`, then add it to the graph.
    pub fn upsert(&self, batch: &[(String, Vec<f32>)]) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let mut buf = Vec::new();
        for (id, v) in batch {
            if v.len() != self.dim {
                bail!(
                    "vector for {id:?} has dim {}, expected {}",
                    v.len(),
                    self.dim
                );
            }
            encode(id, v, &mut buf);
        }
        {
            let mut log = self.log.lock().unwrap();
            log.write_all(&buf)?;
            log.sync_data()?;
        }
        let mut state = self.state.write().unwrap();
        let mut ctx = SearchContext::new(state.ids.len() + batch.len());
        for (id, v) in batch {
            Self::insert(&mut state, id, v, &mut ctx);
        }
        Ok(())
    }

    /// Hide a vector (it stays in the graph as a routing node).
    pub fn delete(&self, id: &str) {
        let mut state = self.state.write().unwrap();
        if let Some(node) = state.by_id.remove(id) {
            state.live[node as usize] = false;
        }
    }

    /// Approximate top-k by cosine similarity: (external id, similarity).
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<(String, f32)>> {
        if query.len() != self.dim {
            bail!(
                "query vector has dim {}, expected {}",
                query.len(),
                self.dim
            );
        }
        let state = self.state.read().unwrap();
        let mut ctx = self
            .contexts
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| SearchContext::new(0));
        let live = |id: u32| state.live[id as usize];
        let hits = state.hnsw.search(
            &state.vectors,
            query,
            k,
            self.ef_search.max(k),
            Some(&live),
            &mut ctx,
        );
        self.contexts.lock().unwrap().push(ctx);
        Ok(hits
            .into_iter()
            .map(|n| (state.ids[n.id as usize].clone(), n.sim))
            .collect())
    }
}
