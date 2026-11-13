//! The inverted index abstraction.
//!
//! An *inverted index* maps each term to the list of documents containing it
//! (its *postings list*), the reverse of a forward index (doc -> terms):
//!
//! ```text
//!   term dictionary            postings (doc_id, term frequency)
//!   ---------------            ---------------------------------
//!   "bm25"   --------------->  [(0, 2), (7, 1)]
//!   "index"  --------------->  [(0, 1), (3, 4), (7, 2), (9, 1)]
//!   "rust"   --------------->  [(3, 1)]
//!
//!   doc lengths: [12, 40, 7, 33, ...]   (tokens per doc, for BM25 length norm)
//!   collection stats: N = num docs, total tokens (-> average doc length)
//! ```
//!
//! Search code talks only to the [`SegmentReader`] trait. Stage 1 provides an
//! in-memory implementation ([`InMemorySegment`]); stage 2 will add an
//! mmap-backed on-disk segment implementing the same trait, so BM25 and the
//! query engine do not change when storage does.

mod memory;

pub use memory::{InMemoryIndexBuilder, InMemorySegment};

use crate::types::{DocId, SegmentId};

/// One entry of a postings list: "term occurs `tf` times in document `doc`".
///
/// Postings lists are sorted by `doc` ascending; later stages rely on that
/// (delta encoding, skipping, WAND).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Posting {
    pub doc: DocId,
    /// Term frequency: occurrences of the term in this document (>= 1).
    pub tf: u32,
}

/// Collection-level statistics BM25 needs.
///
/// Kept separate from the segment on purpose: with several segments (stage 2)
/// or shards (stage 7) ranking must use *global* statistics, otherwise the
/// same document would score differently depending on where it lives. The
/// searcher therefore accepts stats from the outside.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CollectionStats {
    /// Number of documents `N`.
    pub num_docs: u64,
    /// Sum of all document lengths (in tokens, after analysis).
    pub total_tokens: u64,
}

impl CollectionStats {
    /// Average document length `avgdl`. Returns 1.0 for an empty collection so
    /// callers never divide by zero.
    pub fn avg_doc_len(&self) -> f32 {
        if self.num_docs == 0 {
            1.0
        } else {
            (self.total_tokens as f64 / self.num_docs as f64) as f32
        }
    }
}

/// Read-only view of one index segment.
///
/// A *trait* is Rust's interface (like a Java interface / Go interface).
/// Anything implementing these methods can be searched.
///
/// `type Postings<'a>` is an *associated type* with a lifetime parameter
/// (a "generic associated type", GAT). It lets each implementation choose its
/// own iterator type, e.g. a slice iterator for the in-memory segment or a
/// varint decoder over mmapped bytes for the on-disk one, while the borrow
/// checker guarantees the iterator does not outlive the segment it reads from
/// (`where Self: 'a`). Using an associated type instead of
/// `Box<dyn Iterator>` keeps the hot loop free of dynamic dispatch.
pub trait SegmentReader {
    /// Iterator over one term's postings, sorted by doc id.
    type Postings<'a>: Iterator<Item = Posting>
    where
        Self: 'a;

    /// Which segment this is.
    fn segment_id(&self) -> SegmentId;

    /// Number of documents; valid doc ids are `0..num_docs()`.
    fn num_docs(&self) -> u32;

    /// Sum of all document lengths in this segment.
    fn total_tokens(&self) -> u64;

    /// Number of tokens in `doc` after analysis. Panics if `doc` is out of range.
    fn doc_len(&self, doc: DocId) -> u32;

    /// The dataset's own id for `doc` (e.g. `"MED-10"`).
    fn external_id(&self, doc: DocId) -> &str;

    /// Document frequency: how many documents contain `term` (0 if none).
    fn doc_freq(&self, term: &str) -> u32;

    /// Postings for `term`, or `None` if the term is not in the dictionary.
    fn postings(&self, term: &str) -> Option<Self::Postings<'_>>;

    /// Number of distinct terms.
    fn num_terms(&self) -> usize;

    /// This segment's statistics. A *default method*: implementors get it for
    /// free but may override it.
    fn stats(&self) -> CollectionStats {
        CollectionStats {
            num_docs: self.num_docs() as u64,
            total_tokens: self.total_tokens(),
        }
    }
}
