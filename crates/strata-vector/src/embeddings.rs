//! Cached BEIR embeddings written by `scripts/embed_beir.py`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::vectors::Vectors;

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct EmbeddingMeta {
    pub model: String,
    pub model_revision: Option<String>,
    pub dim: usize,
    pub normalized: bool,
    pub query_prefix: String,
    pub device: String,
    pub torch: String,
    pub sentence_transformers: String,
    pub seconds_docs: f64,
    pub seconds_queries: f64,
}

pub struct DatasetEmbeddings {
    pub meta: EmbeddingMeta,
    pub corpus: Vectors,
    pub corpus_ids: Vec<String>,
    pub queries: Vectors,
    pub query_ids: Vec<String>,
    query_row: HashMap<String, usize>,
}

impl DatasetEmbeddings {
    /// Load `<data_dir>/embeddings/<name>/`.
    pub fn load(data_dir: &Path, name: &str) -> Result<DatasetEmbeddings> {
        let dir = data_dir.join("embeddings").join(name);
        let meta: EmbeddingMeta = serde_json::from_slice(
            &std::fs::read(dir.join("meta.json"))
                .with_context(|| format!("{}: run scripts/embed_beir.py first", dir.display()))?,
        )?;
        let corpus = Vectors::load(&dir.join("corpus.f32"), meta.dim)?;
        let queries = Vectors::load(&dir.join("queries.f32"), meta.dim)?;
        let corpus_ids: Vec<String> =
            serde_json::from_slice(&std::fs::read(dir.join("corpus_ids.json"))?)?;
        let query_ids: Vec<String> =
            serde_json::from_slice(&std::fs::read(dir.join("query_ids.json"))?)?;
        anyhow::ensure!(
            corpus.len() == corpus_ids.len() && queries.len() == query_ids.len(),
            "row/id count mismatch"
        );
        let query_row = query_ids
            .iter()
            .enumerate()
            .map(|(i, q)| (q.clone(), i))
            .collect();
        Ok(DatasetEmbeddings {
            meta,
            corpus,
            corpus_ids,
            queries,
            query_ids,
            query_row,
        })
    }

    /// Embedding of query `qid`.
    pub fn query(&self, qid: &str) -> Option<&[f32]> {
        self.query_row.get(qid).map(|&i| self.queries.get(i))
    }
}
