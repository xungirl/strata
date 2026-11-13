//! # strata-core
//!
//! The search-engine core of **strata**: text analysis, the inverted index and
//! BM25 ranking. Everything here is deliberately independent of I/O so that the
//! same code runs against the in-memory index built in stage 1 and against the
//! on-disk segments planned for stage 2.
//!
//! Module map (read in this order):
//!
//! | module       | responsibility                                              |
//! |--------------|-------------------------------------------------------------|
//! | [`types`]    | small strongly-typed ids (`DocId`, `SegmentId`, `ShardId`)  |
//! | [`analyzer`] | text -> tokens (tokenize, lowercase, stopwords, stemming)   |
//! | [`index`]    | the [`index::SegmentReader`] trait + in-memory segment      |
//! | [`scoring`]  | BM25 math (IDF, term-frequency saturation, length norm)     |
//! | [`search`]   | bag-of-words queries and exhaustive top-k retrieval         |

pub mod analyzer;
pub mod index;
pub mod scoring;
pub mod search;
pub mod types;

// Re-export the handful of types most callers need, so users can write
// `use strata_core::{Analyzer, Searcher}` instead of long module paths.
pub use analyzer::{Analyzer, AnalyzerConfig};
pub use index::{InMemoryIndexBuilder, InMemorySegment, Posting, SegmentReader};
pub use scoring::Bm25Params;
pub use search::{Hit, Query, Searcher};
pub use types::{DocId, SegmentId, ShardId};
