//! Block-max WAND: exact top-k for disjunctive (OR) term queries that skips
//! documents which provably cannot enter the top k.
//!
//! Background. Exhaustive evaluation (stage 1) scores every posting of every
//! query term. But once we hold k results, the k-th best score `θ` is a bar a
//! new document must clear. If we know an *upper bound* on what a document
//! could still score, we can skip it without decoding or scoring it.
//!
//! * **WAND** (Broder et al., 2003) uses one bound per term: the maximum BM25
//!   contribution of the term over its whole postings list (`UB_t`). Keep the
//!   cursors sorted by current doc; walk them accumulating `UB_t` until the sum
//!   exceeds `θ`. That cursor's doc is the *pivot*: no doc before it can beat
//!   `θ` (only the terms before the pivot can match it, and their bounds sum to
//!   at most `θ`), so lagging cursors jump straight to the pivot.
//! * **Block-max WAND** (Ding & Suel, 2011) refines this with per-block
//!   bounds from the skip table (`max_tf`, `min_doc_len` of each 128-doc
//!   block, written by the stage-2 segment writer). Before scoring the pivot
//!   we sum the bounds of the blocks that contain it; if even that cannot beat
//!   `θ`, we skip to the end of the shortest of those blocks.
//!
//! Exactness. This returns *bit-identical* results to the exhaustive
//! [`Snapshot::search`]: same docs, same order, same f32 scores.
//! 1. Scores are computed with the same scorer and summed in the same term
//!    order as the term-at-a-time accumulator.
//! 2. Docs are visited in increasing global doc id across all segments into a
//!    single heap, so a candidate equal to `θ` always loses the doc-id
//!    tie-break and pruning `bound <= θ` loses nothing.
//! 3. Bounds are computed in f32 by the same monotone formula, and a pruning
//!    decision requires `bound * (1 + 1e-5) <= θ`, a safety margin far above
//!    the accumulated rounding error of a few-term f32 sum. This is checked by
//!    a property test against the exhaustive search and on all five BEIR
//!    datasets (identical run files).

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

use strata_core::index::SegmentReader;
use strata_core::scoring::{idf, Bm25Params, Bm25TermScorer};
use strata_core::types::DocId;
use strata_storage::segment::DiskPostings;
use strata_storage::snapshot::{Snapshot, SnapshotHit};

use crate::ast::Filter;
use crate::filter::SegmentFilter;

/// Relative safety margin applied to every upper bound before pruning.
const BOUND_SLACK: f32 = 1e-5;

/// Work counters, to show how much pruning saved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WandStats {
    /// Sum of query terms' document frequencies (what exhaustive scoring reads).
    pub postings_total: u64,
    /// Documents fully scored.
    pub docs_scored: u64,
    /// Pivot candidates rejected by the block-max check.
    pub block_skips: u64,
    /// Main-loop iterations (pivot selections).
    pub iterations: u64,
}

/// Heap entry ordered like `strata_core::search::top_k`: higher score
/// first, then lower (global) doc id.
#[derive(Clone, Copy, PartialEq)]
struct Entry {
    score: f32,
    doc: u32,
}
impl Eq for Entry {}
impl Ord for Entry {
    fn cmp(&self, o: &Self) -> Ordering {
        self.score
            .total_cmp(&o.score)
            .then_with(|| o.doc.cmp(&self.doc))
    }
}
impl PartialOrd for Entry {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// Global top-k heap shared by all segments.
struct TopK {
    k: usize,
    heap: BinaryHeap<Reverse<Entry>>,
}

impl TopK {
    /// Current bar: a doc must score strictly more to enter (0 until full).
    fn threshold(&self) -> f32 {
        if self.heap.len() < self.k {
            0.0
        } else {
            self.heap.peek().map_or(0.0, |r| r.0.score)
        }
    }

    fn offer(&mut self, e: Entry) {
        if self.heap.len() < self.k {
            self.heap.push(Reverse(e));
        } else if let Some(mut worst) = self.heap.peek_mut() {
            if e > worst.0 {
                *worst = Reverse(e);
            }
        }
    }
}

/// One query term's cursor in one segment.
struct TermCursor<'a> {
    postings: DiskPostings<'a>,
    /// Current doc (u32::MAX when exhausted) and its tf.
    doc: u32,
    tf: u32,
    scorer: Bm25TermScorer,
    /// Upper bound of this term's contribution anywhere in the segment.
    ub: f32,
    /// Position of the term in the (sorted) query: defines summation order.
    term_index: usize,
    /// Cache of the last block bound: (block last doc, bound). Valid while
    /// queries stay inside that block (targets are non-decreasing).
    cached_block: Option<(u32, f32)>,
}

impl TermCursor<'_> {
    fn next(&mut self) {
        match self.postings.next() {
            Some(p) => {
                self.doc = p.doc.0;
                self.tf = p.tf;
            }
            None => self.doc = u32::MAX,
        }
    }

    fn advance_to(&mut self, target: u32) {
        if self.doc >= target {
            return;
        }
        match self.postings.advance_to(target) {
            Some(p) => {
                self.doc = p.doc.0;
                self.tf = p.tf;
            }
            None => self.doc = u32::MAX,
        }
    }

    /// Bound for docs in the block containing `target` and that block's
    /// last doc (u32::MAX = rest of the list). `None` if exhausted.
    fn block_bound(&mut self, target: u32) -> Option<(f32, u32)> {
        if let Some((last, bound)) = self.cached_block {
            // Targets only grow, so a cached block still covers `target` iff
            // its last doc is >= target.
            if target <= last {
                return Some((bound, last));
            }
        }
        let e = self.postings.block_for(target)?;
        let bound = self.scorer.score(e.max_tf, e.min_doc_len);
        self.cached_block = Some((e.last_doc, bound));
        Some((bound, e.last_doc))
    }
}

#[inline]
fn exceeds(bound: f32, threshold: f32) -> bool {
    bound * (1.0 + BOUND_SLACK) > threshold
}

/// Exact top-k for `terms` (sorted `(term, query count)`) over the
/// snapshot, with index-wide statistics, deleted docs and `filters` excluded.
pub fn wand_search(
    snap: &Snapshot,
    terms: &[(String, u32)],
    filters: &[Filter],
    params: Bm25Params,
    k: usize,
) -> (Vec<SnapshotHit>, WandStats) {
    let mut stats = WandStats::default();
    if k == 0 || terms.is_empty() {
        return (Vec::new(), stats);
    }
    let cs = snap.stats();
    let avgdl = cs.avg_doc_len();
    // Index-wide df -> one scorer per term, shared by all segments.
    let scorers: Vec<Bm25TermScorer> = terms
        .iter()
        .map(|(t, count)| {
            let df = snap.doc_freq(t);
            stats.postings_total += df;
            Bm25TermScorer::new(params, idf(cs.num_docs, df), *count, avgdl)
        })
        .collect();

    let mut top = TopK {
        k,
        heap: BinaryHeap::with_capacity(k + 1),
    };
    for seg in snap.segments() {
        let filter = SegmentFilter::new(seg, filters);
        let mut cursors: Vec<TermCursor> = Vec::with_capacity(terms.len());
        for (i, (t, _)) in terms.iter().enumerate() {
            if let Some(postings) = seg.segment.cursor(t) {
                let scorer = scorers[i];
                let ub = postings
                    .skip_entries()
                    .map(|e| scorer.score(e.max_tf, e.min_doc_len))
                    .fold(0.0f32, f32::max);
                let mut c = TermCursor {
                    postings,
                    doc: 0,
                    tf: 0,
                    scorer,
                    ub,
                    term_index: i,
                    cached_block: None,
                };
                c.next();
                cursors.push(c);
            }
        }
        run_segment(
            seg.segment.as_ref(),
            seg.base,
            &filter,
            &mut cursors,
            &mut top,
            &mut stats,
        );
    }

    let mut hits: Vec<Entry> = top.heap.into_iter().map(|r| r.0).collect();
    hits.sort_by(|a, b| b.cmp(a)); // best first
    let hits = hits
        .into_iter()
        .map(|e| {
            let i = snap
                .segments()
                .iter()
                .rposition(|s| s.base <= e.doc)
                .expect("segment");
            SnapshotHit {
                segment: i,
                doc: DocId(e.doc - snap.segments()[i].base),
                score: e.score,
            }
        })
        .collect();
    (hits, stats)
}

fn run_segment<S: SegmentReader>(
    seg: &S,
    base: u32,
    filter: &SegmentFilter,
    cursors: &mut [TermCursor],
    top: &mut TopK,
    stats: &mut WandStats,
) {
    // Scratch: contributions by term index, so sums follow term order.
    let mut contrib: Vec<Option<f32>> =
        vec![None; cursors.iter().map(|c| c.term_index + 1).max().unwrap_or(0)];
    // `order` holds cursor indexes sorted by current doc. We sort these small
    // integers rather than the cursors themselves: a cursor embeds two
    // 128-entry decode buffers (> 1 KB), so moving cursors around would cost
    // more than the pruning saves.
    let mut order: Vec<usize> = (0..cursors.len()).collect();
    loop {
        stats.iterations += 1;
        order.sort_unstable_by_key(|&i| cursors[i].doc);
        let theta = top.threshold();

        // 1. Pivot: first position where the summed list bounds beat θ.
        let mut acc = 0.0f32;
        let mut pivot = None;
        for (pos, &i) in order.iter().enumerate() {
            let c = &cursors[i];
            if c.doc == u32::MAX {
                break;
            }
            acc += c.ub;
            if exceeds(acc, theta) {
                pivot = Some(pos);
                break;
            }
        }
        let Some(mut p) = pivot else {
            return; // nothing left can enter the top k
        };
        let pivot_doc = cursors[order[p]].doc;
        // Include every cursor already sitting on the pivot doc.
        while p + 1 < order.len() && cursors[order[p + 1]].doc == pivot_doc {
            p += 1;
        }

        // 2. Block-max check over cursors order[0..=p] (at or before the pivot).
        let mut block_sum = 0.0f32;
        let mut block_end = u32::MAX;
        for &i in &order[..=p] {
            if let Some((b, last)) = cursors[i].block_bound(pivot_doc) {
                block_sum += b;
                block_end = block_end.min(last);
            }
        }
        if !exceeds(block_sum, theta) {
            // No doc in [pivot_doc, next) can beat θ: in that range only these
            // terms can match and their block bounds hold until `block_end`;
            // the next cursor starts at `order[p+1]`'s doc.
            stats.block_skips += 1;
            let mut next = block_end.saturating_add(1);
            if let Some(&i) = order.get(p + 1) {
                next = next.min(cursors[i].doc);
            }
            debug_assert!(next > pivot_doc);
            for &i in &order[..=p] {
                cursors[i].advance_to(next);
            }
            continue;
        }

        if cursors[order[0]].doc == pivot_doc {
            // 3. All candidate terms are on the pivot: score it fully.
            if filter.allows(pivot_doc) {
                stats.docs_scored += 1;
                let dl = seg.doc_len(DocId(pivot_doc));
                for &i in &order[..=p] {
                    let c = &cursors[i];
                    contrib[c.term_index] = Some(c.scorer.score(c.tf, dl));
                }
                // Same order as the exhaustive accumulator: 0.0 + s_t0 + s_t1 ...
                let mut score = 0.0f32;
                for s in contrib.iter_mut() {
                    if let Some(v) = s.take() {
                        score += v;
                    }
                }
                top.offer(Entry {
                    score,
                    doc: base + pivot_doc,
                });
            }
            for &i in &order[..=p] {
                cursors[i].next();
            }
        } else {
            // 4. Move lagging cursors up to the pivot.
            for &i in &order[..p] {
                if cursors[i].doc < pivot_doc {
                    cursors[i].advance_to(pivot_doc);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// MaxScore (Turtle & Flood, 1995), the other classic dynamic-pruning
// algorithm, also used by Lucene for disjunctions since 9.x.
// ---------------------------------------------------------------------------

/// Exact top-k for `terms` with **MaxScore** pruning. Same contract and
/// exactness argument as [`wand_search`].
///
/// Idea: sort terms by upper bound. Once the threshold θ is known, the
/// lowest-bound terms whose bounds *together* cannot beat θ are
/// "non-essential": a document matching only those can never enter the top
/// k. So candidates are drawn only from the essential terms' postings; the
/// non-essential terms are looked up (with `advance_to`) just for those
/// candidates, highest bound first, stopping as soon as the remaining bound
/// cannot lift the candidate above θ. With long queries full of frequent,
/// low-IDF words, those words quickly become non-essential and their long
/// postings lists are mostly skipped.
pub fn maxscore_search(
    snap: &Snapshot,
    terms: &[(String, u32)],
    filters: &[Filter],
    params: Bm25Params,
    k: usize,
) -> (Vec<SnapshotHit>, WandStats) {
    let mut stats = WandStats::default();
    if k == 0 || terms.is_empty() {
        return (Vec::new(), stats);
    }
    let cs = snap.stats();
    let avgdl = cs.avg_doc_len();
    let scorers: Vec<Bm25TermScorer> = terms
        .iter()
        .map(|(t, count)| {
            let df = snap.doc_freq(t);
            stats.postings_total += df;
            Bm25TermScorer::new(params, idf(cs.num_docs, df), *count, avgdl)
        })
        .collect();
    let mut top = TopK {
        k,
        heap: BinaryHeap::with_capacity(k + 1),
    };
    for seg in snap.segments() {
        let filter = SegmentFilter::new(seg, filters);
        let mut cursors: Vec<TermCursor> = Vec::new();
        for (i, (t, _)) in terms.iter().enumerate() {
            if let Some(postings) = seg.segment.cursor(t) {
                let scorer = scorers[i];
                let ub = postings
                    .skip_entries()
                    .map(|e| scorer.score(e.max_tf, e.min_doc_len))
                    .fold(0.0f32, f32::max);
                let mut c = TermCursor {
                    postings,
                    doc: 0,
                    tf: 0,
                    scorer,
                    ub,
                    term_index: i,
                    cached_block: None,
                };
                c.next();
                cursors.push(c);
            }
        }
        maxscore_segment(
            seg.segment.as_ref(),
            seg.base,
            &filter,
            &mut cursors,
            &mut top,
            &mut stats,
            terms.len(),
        );
    }
    let mut hits: Vec<Entry> = top.heap.into_iter().map(|r| r.0).collect();
    hits.sort_by(|a, b| b.cmp(a));
    let hits = hits
        .into_iter()
        .map(|e| {
            let i = snap
                .segments()
                .iter()
                .rposition(|s| s.base <= e.doc)
                .expect("segment");
            SnapshotHit {
                segment: i,
                doc: DocId(e.doc - snap.segments()[i].base),
                score: e.score,
            }
        })
        .collect();
    (hits, stats)
}

fn maxscore_segment<S: SegmentReader>(
    seg: &S,
    base: u32,
    filter: &SegmentFilter,
    cursors: &mut [TermCursor],
    top: &mut TopK,
    stats: &mut WandStats,
    num_terms: usize,
) {
    // Ascending upper bound; prefix[i] = ub[0] + ... + ub[i-1].
    cursors.sort_by(|a, b| a.ub.total_cmp(&b.ub));
    let n = cursors.len();
    let mut prefix = vec![0.0f32; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + cursors[i].ub;
    }
    let mut contrib: Vec<Option<f32>> = vec![None; num_terms];
    // Cursors [0, essential) are non-essential for the current θ.
    let mut essential = 0;
    loop {
        stats.iterations += 1;
        let theta = top.threshold();
        while essential < n && !exceeds(prefix[essential + 1], theta) {
            essential += 1;
        }
        if essential == n {
            return; // even all terms together cannot beat θ
        }
        // Next candidate: smallest doc among essential cursors.
        let cand = cursors[essential..]
            .iter()
            .map(|c| c.doc)
            .min()
            .unwrap_or(u32::MAX);
        if cand == u32::MAX {
            return;
        }
        let dl = seg.doc_len(DocId(cand));
        let mut partial = 0.0f32; // sum of known contributions (any order; only for bounding)
        for c in cursors[essential..].iter() {
            if c.doc == cand {
                let s = c.scorer.score(c.tf, dl);
                contrib[c.term_index] = Some(s);
                partial += s;
            }
        }
        // Non-essential terms, highest bound first, while they can still matter.
        let mut complete = true;
        for i in (0..essential).rev() {
            if !exceeds(partial + prefix[i + 1], theta) {
                complete = false; // the rest cannot lift `cand` above θ
                break;
            }
            let c = &mut cursors[i];
            c.advance_to(cand);
            if c.doc == cand {
                let s = c.scorer.score(c.tf, dl);
                contrib[c.term_index] = Some(s);
                partial += s;
            }
        }
        if complete && filter.allows(cand) {
            stats.docs_scored += 1;
            let mut score = 0.0f32;
            for s in contrib.iter_mut() {
                if let Some(v) = s.take() {
                    score += v;
                }
            }
            top.offer(Entry {
                score,
                doc: base + cand,
            });
        } else {
            contrib.iter_mut().for_each(|s| *s = None);
        }
        for c in cursors[essential..].iter_mut() {
            if c.doc == cand {
                c.next();
            }
        }
    }
}
