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
pub mod wand;

use anyhow::Result;
use strata_core::scoring::Bm25Params;
use strata_core::search::Query;
use strata_storage::snapshot::{Snapshot, SnapshotHit};

pub use ast::{BoolQuery, Clause, Filter};
pub use parser::parse;
pub use wand::{maxscore_search, wand_search, WandStats};

/// How to execute a (filtered) term disjunction. Other queries always use
/// the boolean executor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// MaxScore pruning (the default: fastest on the BEIR queries, see
    /// BENCHMARK.md stage 3).
    Auto,
    /// Block-max WAND pruning.
    BlockMaxWand,
    /// Never prune: term-at-a-time.
    Exhaustive,
}

/// Run `q` against `snap`.
pub fn search(
    snap: &Snapshot,
    q: &BoolQuery,
    params: Bm25Params,
    k: usize,
    strategy: Strategy,
) -> Result<Vec<SnapshotHit>> {
    match (q.as_disjunction(), strategy) {
        (Some(terms), Strategy::Auto) => Ok(maxscore_search(snap, &terms, &q.filters, params, k).0),
        (Some(terms), Strategy::BlockMaxWand) => {
            Ok(wand_search(snap, &terms, &q.filters, params, k).0)
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
            Ok(snap.search_with(&query, params, k, &|seg, doc| filters[seg].allows(doc.0)))
        }
        (None, _) => exec::execute(snap, q, params, k),
    }
}
