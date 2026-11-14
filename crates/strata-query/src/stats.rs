//! Scoring statistics, separate from the index they came from.
//!
//! BM25 needs `N` (documents), the total token count (for the average
//! length) and each query term's document frequency. A single node takes
//! them from its snapshot ([`ScoringStats::local`]). In a sharded cluster
//! (stage 7) the coordinator first asks every shard for its local numbers,
//! sums them ([`ScoringStats::merge`]) and sends the *global* statistics
//! back with the query, so that every shard scores exactly as one big index
//! would ("distributed IDF").

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use strata_core::index::CollectionStats;
use strata_storage::snapshot::Snapshot;

use crate::ast::{BoolQuery, Clause};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoringStats {
    pub num_docs: u64,
    pub total_tokens: u64,
    /// Document frequency per scoring term (missing = 0).
    pub df: HashMap<String, u64>,
}

impl ScoringStats {
    /// This snapshot's statistics for `terms`.
    pub fn local<'a>(snap: &Snapshot, terms: impl IntoIterator<Item = &'a str>) -> ScoringStats {
        let cs = snap.stats();
        ScoringStats {
            num_docs: cs.num_docs,
            total_tokens: cs.total_tokens,
            df: terms
                .into_iter()
                .map(|t| (t.to_string(), snap.doc_freq(t)))
                .collect(),
        }
    }

    /// Sum several shards' statistics.
    pub fn merge<'a>(parts: impl IntoIterator<Item = &'a ScoringStats>) -> ScoringStats {
        let mut out = ScoringStats::default();
        for p in parts {
            out.num_docs += p.num_docs;
            out.total_tokens += p.total_tokens;
            for (t, d) in &p.df {
                *out.df.entry(t.clone()).or_default() += d;
            }
        }
        out
    }

    pub fn collection(&self) -> CollectionStats {
        CollectionStats {
            num_docs: self.num_docs,
            total_tokens: self.total_tokens,
        }
    }

    pub fn doc_freq(&self, term: &str) -> u64 {
        self.df.get(term).copied().unwrap_or(0)
    }
}

impl BoolQuery {
    /// Terms whose statistics affect scoring (terms and phrase terms of
    /// scoring clauses; prefixes are constant-score).
    pub fn scoring_terms(&self) -> Vec<String> {
        let mut out = Vec::new();
        for c in self.must.iter().chain(&self.should) {
            match c {
                Clause::Term { term, .. } => out.push(term.clone()),
                Clause::Phrase(ts) => out.extend(ts.iter().cloned()),
                Clause::Prefix(_) => {}
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_sums_everything() {
        let a = ScoringStats {
            num_docs: 10,
            total_tokens: 100,
            df: [("x".to_string(), 3), ("y".to_string(), 1)].into(),
        };
        let b = ScoringStats {
            num_docs: 5,
            total_tokens: 40,
            df: [("x".to_string(), 2)].into(),
        };
        let m = ScoringStats::merge([&a, &b]);
        assert_eq!((m.num_docs, m.total_tokens), (15, 140));
        assert_eq!(
            (m.doc_freq("x"), m.doc_freq("y"), m.doc_freq("z")),
            (5, 1, 0)
        );
    }
}
