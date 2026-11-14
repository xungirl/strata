//! # strata-query
//!
//! The query engine (stage 3):
//!
//! | module     | responsibility                                                      |
//! |------------|---------------------------------------------------------------------|
//! | [`ast`]    | `BoolQuery`: must / should / must_not clauses + metadata filters     |
//! | [`parser`] | Lucene-flavoured query syntax -> `BoolQuery`                        |
//! | [`filter`] | compile filters against a segment's field columns                   |
//! | [`wand`]   | block-max WAND and MaxScore: exact top-k for term disjunctions      |
//! | [`exec`]   | exhaustive document-at-a-time boolean / phrase / prefix execution   |
//!
//! [`search`] picks the strategy: term disjunctions (the BEIR case) go to a
//! dynamic-pruning algorithm (MaxScore by default, or block-max WAND),
//! everything else to the boolean executor.

pub mod ast;
pub mod exec;
pub mod filter;
pub mod parser;
pub mod stats;
pub mod wand;

use anyhow::Result;
use strata_core::scoring::Bm25Params;
use strata_core::search::Query;
use strata_storage::snapshot::{Snapshot, SnapshotHit};

pub use ast::{BoolQuery, Clause, Filter};
pub use parser::parse;
pub use stats::ScoringStats;
pub use wand::{
    maxscore_search, maxscore_search_with_stats, wand_search, wand_search_with_stats, WandStats,
};

/// How to execute a (filtered) term disjunction. Other queries always use
/// the boolean executor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// MaxScore pruning (the default: on this machine it beat exhaustive
    /// TAAT at k=10 on fiqa, scidocs and trec-covid, 1.1-1.7x, and beat
    /// block-max WAND everywhere; on the two tiny corpora and at k=1000 it
    /// can be slower than TAAT, see BENCHMARK.md stage 3).
    Auto,
    /// Block-max WAND pruning.
    BlockMaxWand,
    /// Never prune: term-at-a-time.
    Exhaustive,
}

/// Run `q` against `snap` with the snapshot's own statistics.
pub fn search(
    snap: &Snapshot,
    q: &BoolQuery,
    params: Bm25Params,
    k: usize,
    strategy: Strategy,
) -> Result<Vec<SnapshotHit>> {
    let local = ScoringStats::local(snap, q.scoring_terms().iter().map(String::as_str));
    search_with_stats(snap, q, params, k, strategy, &local)
}

/// Run `q` with explicit scoring statistics (a shard of a cluster passes
/// the cluster-wide ones; results then equal a single big index).
pub fn search_with_stats(
    snap: &Snapshot,
    q: &BoolQuery,
    params: Bm25Params,
    k: usize,
    strategy: Strategy,
    stats: &ScoringStats,
) -> Result<Vec<SnapshotHit>> {
    match (q.as_disjunction(), strategy) {
        (Some(terms), Strategy::Auto) => {
            Ok(maxscore_search_with_stats(snap, &terms, &q.filters, params, k, stats).0)
        }
        (Some(terms), Strategy::BlockMaxWand) => {
            Ok(wand_search_with_stats(snap, &terms, &q.filters, params, k, stats).0)
        }
        (Some(terms), Strategy::Exhaustive) => {
            let query = Query::from_terms(
                terms
                    .iter()
                    .flat_map(|(t, c)| std::iter::repeat_n(t.clone(), *c as usize)),
            );
            let filters: Vec<filter::SegmentFilter> = snap
                .segments()
                .iter()
                .map(|s| filter::SegmentFilter::new(s, &q.filters))
                .collect();
            let df = |t: &str| stats.doc_freq(t);
            Ok(snap.search_scored(
                &query,
                params,
                k,
                &|seg, doc| filters[seg].allows(doc.0),
                stats.collection(),
                &df,
            ))
        }
        (None, _) => exec::execute_with_stats(snap, q, params, k, stats),
    }
}
