//! Point-in-time, read-only views of the index, and search across segments.
//!
//! A [`Snapshot`] owns `Arc`s to its segments and tombstones, so it is
//! unaffected by later writes, commits or merges: a long-running query sees
//! one consistent state. Scoring uses *index-wide* statistics: `N`, total
//! tokens and per-term `df` are summed over all segments, so a document gets
//! exactly the score it would get in a single big segment. (As in Lucene,
//! deleted-but-not-yet-merged docs still count in these statistics.)

use std::collections::HashMap;
use std::sync::Arc;

use strata_core::index::{CollectionStats, SegmentReader};
use strata_core::scoring::Bm25Params;
use strata_core::search::{top_k, Hit, Query, Searcher};
use strata_core::types::DocId;

use crate::livedocs::LiveDocs;
use crate::segment::DiskSegment;

/// One segment as seen by a snapshot.
pub struct SnapshotSegment {
    pub segment: Arc<DiskSegment>,
    pub live: Arc<LiveDocs>,
    /// Global doc number of this segment's doc 0 (sum of earlier segments'
    /// `num_docs`). Global numbers define the tie-break order.
    pub base: u32,
}

/// A hit with its segment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapshotHit {
    pub segment: usize,
    pub doc: DocId,
    pub score: f32,
}

pub struct Snapshot {
    segments: Vec<SnapshotSegment>,
    stats: CollectionStats,
}

impl Snapshot {
    /// Build from (segment, tombstones) pairs in index order.
    pub fn from_parts(parts: Vec<(Arc<DiskSegment>, Arc<LiveDocs>)>) -> Snapshot {
        let mut base = 0;
        let mut stats = CollectionStats {
            num_docs: 0,
            total_tokens: 0,
        };
        let segments = parts
            .into_iter()
            .map(|(segment, live)| {
                let s = SnapshotSegment {
                    base,
                    segment,
                    live,
                };
                base += s.segment.num_docs();
                stats.num_docs += u64::from(s.segment.num_docs());
                stats.total_tokens += s.segment.total_tokens();
                s
            })
            .collect();
        Snapshot { segments, stats }
    }

    pub fn segments(&self) -> &[SnapshotSegment] {
        &self.segments
    }

    /// Index-wide statistics (deleted docs included, like Lucene).
    pub fn stats(&self) -> CollectionStats {
        self.stats
    }

    /// Live (not deleted) documents.
    pub fn num_live_docs(&self) -> u64 {
        self.segments
            .iter()
            .map(|s| u64::from(s.live.num_live()))
            .sum()
    }

    /// Index-wide document frequency.
    pub fn doc_freq(&self, term: &str) -> u64 {
        self.segments
            .iter()
            .map(|s| u64::from(s.segment.doc_freq(term)))
            .sum()
    }

    /// Is the doc with this external id live, and where?
    pub fn find(&self, external_id: &str) -> Option<(usize, DocId)> {
        // Newest segments last; an id is live in at most one place.
        self.segments.iter().enumerate().rev().find_map(|(i, s)| {
            s.segment
                .doc_by_external_id(external_id)
                .filter(|d| s.live.is_live(d.0))
                .map(|d| (i, d))
        })
    }

    pub fn external_id(&self, hit: &SnapshotHit) -> &str {
        self.segments[hit.segment].segment.external_id(hit.doc)
    }

    /// Exhaustive BM25 top-k over all segments with global statistics.
    /// Equivalent to searching one segment holding all live documents in
    /// index order.
    pub fn search(&self, query: &Query, params: Bm25Params, k: usize) -> Vec<SnapshotHit> {
        self.search_with(query, params, k, &|_, _| true)
    }

    /// Like [`search`](Self::search), but only docs for which
    /// `allow(segment index, doc)` is true (in addition to not being
    /// deleted) are returned. Used for filtered search and as the oracle for
    /// the query engine.
    pub fn search_with(
        &self,
        query: &Query,
        params: Bm25Params,
        k: usize,
        allow: &dyn Fn(usize, DocId) -> bool,
    ) -> Vec<SnapshotHit> {
        let df: HashMap<&str, u64> = query
            .terms()
            .iter()
            .map(|t| (t.term.as_str(), self.doc_freq(&t.term)))
            .collect();
        let df_fn = |t: &str| df.get(t).copied().unwrap_or(0);
        let mut all = Vec::new();
        for (i, s) in self.segments.iter().enumerate() {
            let live = |d: DocId| s.live.is_live(d.0) && allow(i, d);
            let hits = Searcher::with_stats(s.segment.as_ref(), params, self.stats)
                .with_global_df(&df_fn)
                .with_live_filter(&live)
                .search(query, k);
            // Re-number with global doc ids so the final top-k breaks ties
            // exactly like a single segment would.
            all.extend(hits.into_iter().map(|h| Hit {
                doc: DocId(s.base + h.doc.0),
                score: h.score,
            }));
        }
        top_k(all, k).into_iter().map(|h| self.locate(h)).collect()
    }

    fn locate(&self, h: Hit) -> SnapshotHit {
        // Segments are few; a linear scan from the back is fine.
        let i = self
            .segments
            .iter()
            .rposition(|s| s.base <= h.doc.0)
            .expect("doc in some segment");
        SnapshotHit {
            segment: i,
            doc: DocId(h.doc.0 - self.segments[i].base),
            score: h.score,
        }
    }
}
