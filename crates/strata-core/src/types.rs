//! Strongly-typed identifiers.
//!
//! Using a "newtype" (`struct DocId(u32)`) instead of a bare `u32` costs nothing
//! at runtime but lets the compiler stop us from, say, passing a segment id
//! where a document id is expected. These ids are introduced now, even though
//! stage 1 has a single in-memory segment and no shards, so that later stages
//! (multi-segment storage, sharding, replication) do not have to change every
//! function signature.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Dense, segment-local document id: `0..segment.num_docs()`.
///
/// Dense ids are what make the postings compact (stage 2 delta-encodes them)
/// and let per-document data such as lengths live in a plain `Vec` indexed by
/// id. The *external* id from the dataset (e.g. `"MED-10"`) is stored
/// separately and looked up only for the final results.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DocId(pub u32);

impl DocId {
    /// The id as a `usize`, for indexing into `Vec`s.
    #[inline]
    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Display for DocId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Identifies an immutable segment inside one index (stage 2: one per flush or
/// merge). Stage 1 always uses `SegmentId(0)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SegmentId(pub u32);

/// Identifies a shard in a distributed deployment (stage 7). Unused in stage 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ShardId(pub u16);
