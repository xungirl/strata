//! Queries and exhaustive top-k retrieval.
//!
//! Stage 1 uses *term-at-a-time* (TAAT) exhaustive evaluation:
//!
//! ```text
//! scores = [0.0; num_docs]                // one accumulator per document
//! for each query term t:
//!     for each (doc, tf) in postings(t):  // walk the whole list
//!         scores[doc] += bm25(t, tf, doc_len[doc])
//! return the k largest scores             // binary min-heap of size k
//! ```
//!
//! Every posting of every query term is scored, so this is the *exact* answer
//! and the correctness baseline for the dynamic-pruning algorithms of stage 3
//! (WAND / block-max WAND), which skip documents that provably cannot enter
//! the top k.

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

use crate::analyzer::Analyzer;
use crate::index::{CollectionStats, SegmentReader};
use crate::scoring::{idf, Bm25Params, Bm25TermScorer};
use crate::types::DocId;

/// One analyzed query term and how many times it occurred in the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryTerm {
    pub term: String,
    pub count: u32,
}

/// A bag-of-words query: a set of terms with multiplicities, OR semantics
/// (a document matches if it contains any term).
///
/// Repeated query words count multiple times (`qtf` in BM25), exactly like
/// Anserini's `BagOfWordsQueryGenerator`, which boosts each term clause by its
/// count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    terms: Vec<QueryTerm>,
}

impl Query {
    /// Analyze `text` with the *same* analyzer used at indexing time and group
    /// equal terms. Terms are kept in sorted order so scoring is deterministic.
    pub fn bag_of_words(analyzer: &Analyzer, text: &str) -> Query {
        Query::from_terms(analyzer.analyze(text))
    }

    /// Build a query from already-analyzed terms.
    ///
    /// `impl IntoIterator<Item = String>` accepts a `Vec<String>`, an array,
    /// or any iterator of `String`s: generic code without naming a type.
    pub fn from_terms(terms: impl IntoIterator<Item = String>) -> Query {
        let mut all: Vec<String> = terms.into_iter().collect();
        all.sort_unstable();
        let terms = all
            .chunk_by(|a, b| a == b)
            .map(|run| QueryTerm {
                term: run[0].clone(),
                count: run.len() as u32,
            })
            .collect();
        Query { terms }
    }

    /// The distinct terms, sorted.
    pub fn terms(&self) -> &[QueryTerm] {
        &self.terms
    }

    /// True if analysis removed everything (e.g. the query was only stopwords).
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
}

/// A scored result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub doc: DocId,
    pub score: f32,
}

/// Total order used for ranking: higher score first; on equal scores the
/// *lower* doc id wins, so results are deterministic.
///
/// `f32` is not `Ord` in Rust because of NaN, so we wrap `Hit` and implement
/// the ordering ourselves using `f32::total_cmp`.
#[derive(Clone, Copy, Debug)]
struct Ranked(Hit);

impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .score
            .total_cmp(&other.0.score)
            .then_with(|| other.0.doc.cmp(&self.0.doc)) // reversed: low id = better
    }
}
impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Ranked {}

/// Select the `k` best hits, best first.
///
/// Keeps a *min*-heap of the best `k` seen so far (`Reverse` flips Rust's
/// max-heap). Each new hit is compared against the heap's minimum, the
/// current k-th best, and replaces it if better: O(n log k) time, O(k) memory,
/// instead of sorting all n candidates.
pub fn top_k(hits: impl IntoIterator<Item = Hit>, k: usize) -> Vec<Hit> {
    if k == 0 {
        return Vec::new();
    }
    let mut heap: BinaryHeap<Reverse<Ranked>> = BinaryHeap::with_capacity(k + 1);
    for hit in hits {
        let candidate = Ranked(hit);
        if heap.len() < k {
            heap.push(Reverse(candidate));
        } else if let Some(mut worst) = heap.peek_mut() {
            // `peek_mut` gives mutable access to the top; writing through it
            // and dropping the guard re-heapifies (one sift-down instead of
            // pop + push).
            if candidate > worst.0 {
                *worst = Reverse(candidate);
            }
        }
    }
    // Ascending order of `Reverse<Ranked>` == descending order of `Ranked`.
    heap.into_sorted_vec()
        .into_iter()
        .map(|Reverse(Ranked(hit))| hit)
        .collect()
}

/// Runs BM25 queries against one segment.
///
/// `Searcher<'s, S>` is generic over any `S: SegmentReader` (static dispatch:
/// the compiler generates a specialized copy per segment type) and borrows
/// the segment for lifetime `'s`, so the segment cannot be dropped while a
/// searcher still uses it.
pub struct Searcher<'s, S: SegmentReader> {
    segment: &'s S,
    params: Bm25Params,
    stats: CollectionStats,
    /// Optional source of *global* document frequencies (multi-segment /
    /// multi-shard search). `&dyn Fn` is a borrowed closure: any function
    /// or closure with this signature works.
    global_df: Option<&'s dyn Fn(&str) -> u64>,
    /// Optional liveness filter: documents for which it returns `false`
    /// (e.g. deleted ones) are never returned.
    is_live: Option<&'s dyn Fn(DocId) -> bool>,
}

impl<'s, S: SegmentReader> Searcher<'s, S> {
    /// Search with the segment's own collection statistics.
    pub fn new(segment: &'s S, params: Bm25Params) -> Self {
        Self::with_stats(segment, params, segment.stats())
    }

    /// Search with externally supplied statistics (`N`, `avgdl`). With
    /// several segments or shards, pass the *global* statistics so a document's
    /// score does not depend on where it is stored. (Stage 1 still takes `df`
    /// from the segment unless [`with_global_df`](Self::with_global_df) is
    /// also used.)
    pub fn with_stats(segment: &'s S, params: Bm25Params, stats: CollectionStats) -> Self {
        Searcher {
            segment,
            params,
            stats,
            global_df: None,
            is_live: None,
        }
    }

    /// Use `df(term)` instead of the segment's own document frequency, so IDF
    /// is computed over the whole index (all segments / shards).
    pub fn with_global_df(mut self, df: &'s dyn Fn(&str) -> u64) -> Self {
        self.global_df = Some(df);
        self
    }

    /// Only return documents for which `is_live(doc)` is true (deletes).
    pub fn with_live_filter(mut self, is_live: &'s dyn Fn(DocId) -> bool) -> Self {
        self.is_live = Some(is_live);
        self
    }

    /// Statistics used for scoring.
    pub fn stats(&self) -> CollectionStats {
        self.stats
    }

    /// BM25 parameters in use.
    pub fn params(&self) -> Bm25Params {
        self.params
    }

    /// Exhaustive top-`k` search. Results are sorted best-first; only
    /// documents matching at least one query term are returned.
    pub fn search(&self, query: &Query, k: usize) -> Vec<Hit> {
        let mut scores = vec![0.0f32; self.segment.num_docs() as usize];
        // Remember which accumulators became non-zero so selecting the top k
        // costs O(matching docs), not O(all docs).
        let mut touched: Vec<DocId> = Vec::new();
        self.accumulate(query, &mut scores, &mut touched);
        let live = |doc: &DocId| self.is_live.is_none_or(|f| f(*doc));
        top_k(
            touched.into_iter().filter(live).map(|doc| Hit {
                doc,
                score: scores[doc.as_usize()],
            }),
            k,
        )
    }

    /// Term-at-a-time scoring into `scores` (len = num_docs). Doc ids whose
    /// score goes from 0 to positive are appended to `touched`. BM25
    /// contributions are always > 0 (positive IDF, tf >= 1), so "touched" is
    /// exactly "matched at least one term".
    fn accumulate(&self, query: &Query, scores: &mut [f32], touched: &mut Vec<DocId>) {
        let avgdl = self.stats.avg_doc_len();
        for qt in query.terms() {
            // `let ... else` : bind on success, otherwise run the else block,
            // which must leave the loop iteration (`continue`).
            let Some(postings) = self.segment.postings(&qt.term) else {
                continue; // term not in the index
            };
            let df = match self.global_df {
                Some(global) => global(&qt.term),
                None => u64::from(self.segment.doc_freq(&qt.term)),
            };
            let scorer =
                Bm25TermScorer::new(self.params, idf(self.stats.num_docs, df), qt.count, avgdl);
            for p in postings {
                let slot = &mut scores[p.doc.as_usize()];
                if *slot == 0.0 {
                    touched.push(p.doc);
                }
                *slot += scorer.score(p.tf, self.segment.doc_len(p.doc));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::AnalyzerConfig;
    use crate::index::{InMemoryIndexBuilder, InMemorySegment};

    fn tiny() -> InMemorySegment {
        // Same corpus as the hand-computed example in `scoring::tests`.
        let mut b = InMemoryIndexBuilder::new(Analyzer::new(AnalyzerConfig::simple()));
        b.add_document("d0", "rust search rust");
        b.add_document("d1", "search engine");
        b.add_document("d2", "");
        b.add_document("d3", "Rust ENGINE engine engine");
        b.build()
    }

    fn q(text: &str) -> Query {
        Query::bag_of_words(&Analyzer::new(AnalyzerConfig::simple()), text)
    }

    #[test]
    fn hand_computed_single_term() {
        let seg = tiny();
        let hits = Searcher::new(&seg, Bm25Params::CLASSIC).search(&q("rust"), 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].doc, DocId(0));
        assert!((hits[0].score - 0.871385).abs() < 1e-5);
        assert_eq!(hits[1].doc, DocId(3));
        assert!((hits[1].score - 0.525836).abs() < 1e-5);
    }

    #[test]
    fn multi_term_scores_are_sums_of_single_term_scores() {
        let seg = tiny();
        let s = Searcher::new(&seg, Bm25Params::ANSERINI);
        let both = s.search(&q("rust engine"), 10);
        let rust = s.search(&q("rust"), 10);
        let engine = s.search(&q("engine"), 10);
        let single = |hits: &[Hit], d: u32| {
            hits.iter()
                .find(|h| h.doc == DocId(d))
                .map_or(0.0, |h| h.score)
        };
        for h in &both {
            let expected = single(&rust, h.doc.0) + single(&engine, h.doc.0);
            assert!((h.score - expected).abs() < 1e-5);
        }
        // d0 (rust), d1 (engine), d3 (both) match; d2 (empty) never does.
        assert_eq!(both.len(), 3);
        assert_eq!(both[0].doc, DocId(3));
    }

    #[test]
    fn repeated_query_terms_count_multiple_times() {
        let query = q("rust rust engine");
        assert_eq!(
            query.terms(),
            &[
                QueryTerm {
                    term: "engine".into(),
                    count: 1
                },
                QueryTerm {
                    term: "rust".into(),
                    count: 2
                },
            ]
        );
        let seg = tiny();
        let s = Searcher::new(&seg, Bm25Params::ANSERINI);
        let once = s.search(&q("rust"), 1)[0].score;
        let twice = s.search(&q("rust rust"), 1)[0].score;
        assert!((twice - 2.0 * once).abs() < 1e-6);
    }

    #[test]
    fn k_truncates_and_zero_k_or_unknown_terms_give_nothing() {
        let seg = tiny();
        let s = Searcher::new(&seg, Bm25Params::ANSERINI);
        assert_eq!(s.search(&q("search engine rust"), 2).len(), 2);
        assert!(s.search(&q("rust"), 0).is_empty());
        assert!(s.search(&q("unknown words"), 10).is_empty());
        assert!(s.search(&Query::default(), 10).is_empty());
    }

    #[test]
    fn top_k_orders_by_score_then_lower_doc_id() {
        let h = |d: u32, s: f32| Hit {
            doc: DocId(d),
            score: s,
        };
        let input = vec![
            h(5, 1.0),
            h(1, 3.0),
            h(9, 2.0),
            h(2, 2.0),
            h(7, 0.5),
            h(0, 2.0),
        ];
        let docs: Vec<u32> = top_k(input.clone(), 4).iter().map(|h| h.doc.0).collect();
        assert_eq!(docs, vec![1, 0, 2, 9]);
        // k larger than input returns everything, sorted.
        let all: Vec<u32> = top_k(input, 100).iter().map(|h| h.doc.0).collect();
        assert_eq!(all, vec![1, 0, 2, 9, 5, 7]);
    }

    #[test]
    fn global_df_and_live_filter() {
        let seg = tiny();
        let df = |_: &str| 1u64; // pretend every term is rare
        let live = |d: DocId| d != DocId(0);
        let s = Searcher::new(&seg, Bm25Params::ANSERINI)
            .with_global_df(&df)
            .with_live_filter(&live);
        let hits = s.search(&q("rust"), 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc, DocId(3)); // doc 0 filtered out
        let local = Searcher::new(&seg, Bm25Params::ANSERINI).search(&q("rust"), 10);
        assert!(hits[0].score > local[1].score); // df 1 < df 2 -> higher idf
    }

    #[test]
    fn external_stats_change_scores() {
        let seg = tiny();
        let local = Searcher::new(&seg, Bm25Params::ANSERINI).search(&q("rust"), 1)[0].score;
        let global = CollectionStats {
            num_docs: 1_000,
            total_tokens: 2_250,
        };
        let s = Searcher::with_stats(&seg, Bm25Params::ANSERINI, global);
        let with_global = s.search(&q("rust"), 1)[0].score;
        // Rarer in a bigger collection -> higher IDF -> higher score.
        assert!(with_global > local);
    }
}
