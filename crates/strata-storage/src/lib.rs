//! # strata-storage
//!
//! Durable storage for strata (stage 2):
//!
//! | module      | responsibility                                                   |
//! |-------------|------------------------------------------------------------------|
//! | [`codec`]   | bit packing, varints, little-endian helpers                      |
//! | [`fields`]  | per-doc metadata columns (keyword / int) for filters          |
//! | [`segment`] | immutable segment files: writer + mmap reader + postings cursor  |
//! | [`buffer`]  | in-memory write buffer (with positions) flushed into segments    |
//! | [`livedocs`]| per-segment tombstone bitsets                                    |
//! | [`wal`]     | write-ahead log with group commit and torn-tail recovery         |
//! | [`manifest`]| atomically replaced list of committed segments                   |
//! | [`merge`]   | merging adjacent segments, dropping deleted docs                 |
//! | [`index`]   | the writer: commit, recovery, background merges, snapshots       |
//! | [`snapshot`]| point-in-time readers and multi-segment search with global stats |

pub mod buffer;
pub mod codec;
pub mod fields;
pub mod index;
pub mod livedocs;
pub mod manifest;
pub mod merge;
pub mod segment;
pub mod snapshot;
pub mod wal;

pub use fields::{FieldValue, Fields};
pub use index::{Document, Index, IndexConfig, OpenReport};
pub use segment::DiskSegment;
pub use snapshot::{Snapshot, SnapshotHit};
