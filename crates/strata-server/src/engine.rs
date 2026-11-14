//! A strata node's engine: the durable lexical index (stages 2-3) plus an
//! optional vector store (stage 4), with lexical / dense / hybrid search
//! (stage 5's RRF). Shared by the HTTP and gRPC front ends.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use strata_core::analyzer::{Analyzer, AnalyzerConfig};
use strata_core::scoring::Bm25Params;
use strata_core::search::Query;
use strata_query::{parse, search, search_with_stats, BoolQuery, Clause, ScoringStats, Strategy};
use strata_storage::{Document, Fields, Index, IndexConfig};
use strata_vector::HnswConfig;

use crate::vector_store::VectorStore;

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub dir: PathBuf,
    pub flush_docs: usize,
    pub merge_factor: usize,
    /// Embedding dimension; `None` disables the vector store.
    pub dim: Option<usize>,
    pub hnsw: HnswConfig,
    pub ef_search: usize,
}

/// A document as sent by clients.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IngestDoc {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub fields: Fields,
    #[serde(default)]
    pub vector: Option<Vec<f32>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Lexical,
    Dense,
    Hybrid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchRequest {
    #[serde(default)]
    pub query: String,
    #[serde(default = "default_k")]
    pub k: usize,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub vector: Option<Vec<f32>>,
    /// Treat `query` as plain text (a bag of words, exactly like the BEIR
    /// runs of stages 1-5) instead of the query language.
    #[serde(default)]
    pub plain: bool,
    /// Cluster-wide scoring statistics (sent by the coordinator, stage 7).
    /// Without them the node uses its own.
    #[serde(default)]
    pub global_stats: Option<ScoringStats>,
}

/// Body of `POST /v1/stats`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatsRequest {
    pub query: String,
    #[serde(default)]
    pub plain: bool,
}

fn default_k() -> usize {
    10
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchHit {
    pub id: String,
    pub score: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    pub took_us: u64,
}

pub struct Engine {
    index: Arc<Index>,
    vectors: Option<VectorStore>,
    analyzer: Analyzer,
    params: Bm25Params,
}

/// Candidates taken from each retriever before RRF fusion.
const FUSION_DEPTH: usize = 100;

impl Engine {
    pub fn open(cfg: &EngineConfig) -> Result<Engine> {
        let analyzer = AnalyzerConfig::english();
        let (index, report) = Index::open(
            &cfg.dir.join("lexical"),
            IndexConfig {
                analyzer: analyzer.clone(),
                flush_docs: cfg.flush_docs,
                merge_factor: cfg.merge_factor,
                background_merges: true,
            },
        )?;
        tracing::info!(
            segments = report.segments,
            replayed = report.wal.records_replayed,
            "lexical index opened"
        );
        let vectors = match cfg.dim {
            Some(dim) => Some(VectorStore::open(
                &cfg.dir.join("vectors"),
                dim,
                cfg.hnsw,
                cfg.ef_search,
            )?),
            None => None,
        };
        Ok(Engine {
            index,
            vectors,
            analyzer: Analyzer::new(analyzer),
            params: Bm25Params::ANSERINI,
        })
    }

    pub fn index(&self) -> &Arc<Index> {
        &self.index
    }

    /// Durably add/replace documents. Vectors are appended first, then the
    /// lexical WAL; the call returns (= acknowledges) after both fsyncs.
    #[tracing::instrument(skip_all, fields(docs = docs.len()))]
    pub fn ingest(&self, docs: Vec<IngestDoc>) -> Result<u64> {
        let with_vectors: Vec<(String, Vec<f32>)> = docs
            .iter()
            .filter_map(|d| d.vector.clone().map(|v| (d.id.clone(), v)))
            .collect();
        if !with_vectors.is_empty() {
            match &self.vectors {
                Some(vs) => vs.upsert(&with_vectors)?,
                None => bail!("this node has no vector store (start it with --dim)"),
            }
        }
        let docs: Vec<Document> = docs
            .into_iter()
            .map(|d| Document {
                id: d.id,
                text: d.text,
                fields: d.fields,
            })
            .collect();
        self.index.add_docs(&docs)
    }

    pub fn delete(&self, ids: &[String]) -> Result<u64> {
        if let Some(vs) = &self.vectors {
            for id in ids {
                vs.delete(id);
            }
        }
        self.index.delete(ids)
    }

    pub fn commit(&self) -> Result<()> {
        self.index.commit()
    }

    #[tracing::instrument(skip_all, fields(mode = ?req.mode, k = req.k))]
    pub fn search(&self, req: &SearchRequest) -> Result<SearchResponse> {
        let start = Instant::now();
        let k = req.k.clamp(1, 1000);
        let hits = match req.mode {
            Mode::Lexical => self.lexical(req, k)?,
            Mode::Dense => self.dense(req, k)?,
            Mode::Hybrid => {
                let lex = self.lexical(req, FUSION_DEPTH.max(k))?;
                let den = self.dense(req, FUSION_DEPTH.max(k))?;
                rrf(&[lex, den], k)
            }
        };
        Ok(SearchResponse {
            hits,
            took_us: start.elapsed().as_micros() as u64,
        })
    }

    /// Run an already-built boolean query (lexical) and return id + score.
    pub fn search_bool(&self, q: &BoolQuery, k: usize) -> Result<Vec<SearchHit>> {
        let snap = self.index.snapshot();
        Ok(
            search(&snap, q, self.params, k.clamp(1, 1000), Strategy::Auto)?
                .iter()
                .map(|h| SearchHit {
                    id: snap.external_id(h).to_string(),
                    score: h.score,
                })
                .collect(),
        )
    }

    /// Parse a request's query text (query language, or plain bag of words).
    pub fn parse_query(&self, query: &str, plain: bool) -> Result<BoolQuery> {
        if plain {
            let q = Query::bag_of_words(&self.analyzer, query);
            return Ok(BoolQuery {
                should: q
                    .terms()
                    .iter()
                    .map(|t| Clause::Term {
                        term: t.term.clone(),
                        count: t.count,
                    })
                    .collect(),
                ..Default::default()
            });
        }
        parse(query, &self.analyzer)
    }

    fn lexical(&self, req: &SearchRequest, k: usize) -> Result<Vec<SearchHit>> {
        let q = self.parse_query(&req.query, req.plain)?;
        let snap = self.index.snapshot();
        let hits = match &req.global_stats {
            Some(stats) => search_with_stats(&snap, &q, self.params, k, Strategy::Auto, stats)?,
            None => search(&snap, &q, self.params, k, Strategy::Auto)?,
        };
        Ok(hits
            .iter()
            .map(|h| SearchHit {
                id: snap.external_id(h).to_string(),
                score: h.score,
            })
            .collect())
    }

    /// This node's local scoring statistics for a query's terms (phase 1 of
    /// a distributed search).
    pub fn scoring_stats(&self, req: &StatsRequest) -> Result<ScoringStats> {
        let q = self.parse_query(&req.query, req.plain)?;
        let terms = q.scoring_terms();
        Ok(ScoringStats::local(
            &self.index.snapshot(),
            terms.iter().map(String::as_str),
        ))
    }

    fn dense(&self, req: &SearchRequest, k: usize) -> Result<Vec<SearchHit>> {
        let Some(vs) = &self.vectors else {
            bail!("dense search needs a node started with --dim");
        };
        let Some(v) = &req.vector else {
            bail!("dense search needs a query vector");
        };
        Ok(vs
            .search(v, k)?
            .into_iter()
            .map(|(id, score)| SearchHit { id, score })
            .collect())
    }

    /// Live documents and segments, for metrics.
    pub fn stats(&self) -> (u64, usize, usize) {
        let snap = self.index.snapshot();
        (
            snap.num_live_docs(),
            snap.segments().len(),
            self.vectors.as_ref().map_or(0, |v| v.len()),
        )
    }
}

/// Reciprocal rank fusion (k = 60) over ranked lists of ids.
pub fn rrf(lists: &[Vec<SearchHit>], k: usize) -> Vec<SearchHit> {
    let mut acc: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    for list in lists {
        for (rank, h) in list.iter().enumerate() {
            *acc.entry(h.id.as_str()).or_insert(0.0) += 1.0 / (60.0 + rank as f64 + 1.0);
        }
    }
    let mut out: Vec<SearchHit> = acc
        .into_iter()
        .map(|(id, s)| SearchHit {
            id: id.to_string(),
            score: s as f32,
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    out.truncate(k);
    out
}
