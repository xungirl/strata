//! Randomized property test: the inverted-index search must return exactly
//! what a naive "score every document from its raw tokens" implementation
//! returns. The naive side never touches postings lists, so a bug in the
//! index (wrong tf, missing posting, wrong doc length, bad top-k) shows up as
//! a mismatch. `proptest` generates random corpora/queries and, on failure,
//! *shrinks* them to a minimal counter-example.

use proptest::prelude::*;
use strata_core::analyzer::{Analyzer, AnalyzerConfig};
use strata_core::scoring::{Bm25Params, DocLengthEncoding};
use strata_core::{DocId, InMemoryIndexBuilder, Query, Searcher};

/// Small vocabulary so terms collide often (high df, many ties).
const VOCAB: &[&str] = &[
    "alpha", "beta", "gamma", "delta", "eps", "zeta", "eta", "theta",
];

fn words(ids: &[usize]) -> Vec<&'static str> {
    ids.iter().map(|&i| VOCAB[i]).collect()
}

/// Naive BM25 in f64, written directly from the formula.
fn naive_scores(docs: &[Vec<usize>], query: &[usize], k1: f64, b: f64) -> Vec<f64> {
    let n = docs.len() as f64;
    let avgdl = if docs.is_empty() {
        1.0
    } else {
        docs.iter().map(|d| d.len()).sum::<usize>() as f64 / n
    };
    // Distinct query terms with their counts.
    let mut qtf = [0u32; 8];
    for &t in query {
        qtf[t] += 1;
    }
    docs.iter()
        .map(|doc| {
            let dl = doc.len() as f64;
            (0..VOCAB.len())
                .filter(|&t| qtf[t] > 0)
                .map(|t| {
                    let tf = doc.iter().filter(|&&x| x == t).count() as f64;
                    if tf == 0.0 {
                        return 0.0;
                    }
                    let df = docs.iter().filter(|d| d.contains(&t)).count() as f64;
                    let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                    qtf[t] as f64 * idf * tf * (k1 + 1.0) / (tf + k1 * (1.0 - b + b * dl / avgdl))
                })
                .sum()
        })
        .collect()
}

fn corpus() -> impl Strategy<Value = Vec<Vec<usize>>> {
    prop::collection::vec(prop::collection::vec(0..VOCAB.len(), 0..30), 0..40)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn index_search_equals_brute_force(
        docs in corpus(),
        query in prop::collection::vec(0..VOCAB.len(), 1..6),
        k in 1usize..50,
        k1 in prop::sample::select(vec![0.0f32, 0.9, 1.2, 2.0]),
        b in prop::sample::select(vec![0.0f32, 0.4, 0.75, 1.0]),
    ) {
        let mut builder = InMemoryIndexBuilder::new(Analyzer::new(AnalyzerConfig::simple()));
        for (i, d) in docs.iter().enumerate() {
            builder.add_document(&format!("doc{i}"), &words(d).join(" "));
        }
        let seg = builder.build();
        let params = Bm25Params { k1, b, doc_length: DocLengthEncoding::Exact };
        let q = Query::from_terms(words(&query).into_iter().map(String::from));
        let hits = Searcher::new(&seg, params).search(&q, k);

        let naive = naive_scores(&docs, &query, f64::from(k1), f64::from(b));
        let matching: Vec<usize> = (0..docs.len()).filter(|&d| naive[d] > 0.0).collect();

        // 1. Right number of results.
        prop_assert_eq!(hits.len(), matching.len().min(k));

        // 2. Every returned score equals the naive score (f32 vs f64 tolerance).
        for h in &hits {
            let expected = naive[h.doc.0 as usize];
            prop_assert!((f64::from(h.score) - expected).abs() <= 1e-4 * expected.max(1.0),
                "doc {} got {} expected {}", h.doc, h.score, expected);
        }

        // 3. Sorted best-first.
        for w in hits.windows(2) {
            prop_assert!(w[0].score >= w[1].score);
        }

        // 4. Nothing better was left out: the k-th returned score is >= every
        //    non-returned matching doc's score (up to float tolerance).
        if let Some(last) = hits.last() {
            let returned: Vec<DocId> = hits.iter().map(|h| h.doc).collect();
            for &d in &matching {
                if !returned.contains(&DocId(d as u32)) {
                    prop_assert!(naive[d] <= f64::from(last.score) + 1e-4,
                        "doc {} (score {}) missing from top-{}", d, naive[d], k);
                }
            }
        }
    }
}
