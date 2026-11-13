//! IR evaluation metrics, matching `trec_eval` / `pytrec_eval` (which BEIR uses).
//!
//! * **nDCG@k** (graded): `DCG@k = sum_{i=1..k} gain(d_i) / log2(i + 1)` with
//!   `gain = relevance label` (trec_eval's default *linear* gain, not
//!   `2^rel - 1`), divided by the DCG of the ideal ordering of *all judged*
//!   documents for the query.
//! * **Recall@k**: relevant documents in the top k / all relevant documents.
//! * **MRR@k**: `1 / rank` of the first relevant document within the top k,
//!   0 if none.
//!
//! "Relevant" means label >= 1 (trec_eval's default `relevance_level`).
//!
//! Two trec_eval details matter for reproducing numbers to 4 decimals:
//! 1. trec_eval **re-sorts** each run by score descending and breaks ties by
//!    **document id descending** (string compare), ignoring the rank column.
//!    [`trec_order`] does the same.
//! 2. With `-c`, queries in the qrels that have no results count as 0. We
//!    always average over every query in the qrels (Anserini uses `-c`).

use std::collections::{BTreeMap, HashMap};

/// Relevance judgments: query id -> (doc id -> graded label).
pub type Qrels = HashMap<String, HashMap<String, i32>>;

/// A run: query id -> retrieved (doc id, score) pairs.
pub type Run = HashMap<String, Vec<(String, f64)>>;

/// Metric values for one query.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
pub struct QueryMetrics {
    pub ndcg_at_10: f64,
    pub recall_at_100: f64,
    pub recall_at_1000: f64,
    pub mrr_at_10: f64,
}

/// Averages over all qrels queries plus per-query values.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Evaluation {
    pub num_queries: usize,
    /// Queries in the qrels with no results in the run (scored as 0).
    pub num_queries_without_results: usize,
    pub mean: QueryMetrics,
    /// Per-query values, keyed by query id (`BTreeMap` = sorted keys, so the
    /// JSON output is deterministic).
    pub per_query: BTreeMap<String, QueryMetrics>,
}

/// Sort `(doc, score)` pairs the way trec_eval does: score descending, then
/// doc id descending. Sorting is stable and total (`f64::total_cmp`).
pub fn trec_order(results: &mut [(String, f64)]) {
    results.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
}

/// nDCG@k for one query. `ranked` must already be in rank order.
pub fn ndcg_at_k(ranked: &[(String, f64)], judgments: &HashMap<String, i32>, k: usize) -> f64 {
    let gain = |doc: &str| judgments.get(doc).copied().unwrap_or(0).max(0) as f64;
    // Rank i (1-based) is discounted by log2(i + 1); with 0-based `i`, log2(i + 2).
    let dcg: f64 = ranked
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, (doc, _))| gain(doc) / ((i + 2) as f64).log2())
        .sum();

    let mut ideal: Vec<f64> = judgments
        .values()
        .filter(|&&rel| rel > 0)
        .map(|&rel| rel as f64)
        .collect();
    ideal.sort_by(|a, b| b.total_cmp(a));
    let idcg: f64 = ideal
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, g)| g / ((i + 2) as f64).log2())
        .sum();

    if idcg == 0.0 {
        0.0
    } else {
        dcg / idcg
    }
}

/// Recall@k for one query. Returns 0 when the query has no relevant documents.
pub fn recall_at_k(ranked: &[(String, f64)], judgments: &HashMap<String, i32>, k: usize) -> f64 {
    let num_relevant = judgments.values().filter(|&&rel| rel > 0).count();
    if num_relevant == 0 {
        return 0.0;
    }
    let found = ranked
        .iter()
        .take(k)
        .filter(|(doc, _)| judgments.get(doc).is_some_and(|&rel| rel > 0))
        .count();
    found as f64 / num_relevant as f64
}

/// MRR@k (reciprocal rank of the first relevant hit) for one query.
pub fn mrr_at_k(ranked: &[(String, f64)], judgments: &HashMap<String, i32>, k: usize) -> f64 {
    ranked
        .iter()
        .take(k)
        .position(|(doc, _)| judgments.get(doc).is_some_and(|&rel| rel > 0))
        .map_or(0.0, |i| 1.0 / (i + 1) as f64)
}

/// Evaluate a whole run. The run is re-sorted with [`trec_order`] first, so
/// the result does not depend on the order the caller stored results in.
pub fn evaluate(qrels: &Qrels, run: &Run) -> Evaluation {
    let mut per_query = BTreeMap::new();
    let mut without_results = 0;
    for (qid, judgments) in qrels {
        let mut ranked = run.get(qid).cloned().unwrap_or_default();
        if ranked.is_empty() {
            without_results += 1;
        }
        trec_order(&mut ranked);
        per_query.insert(
            qid.clone(),
            QueryMetrics {
                ndcg_at_10: ndcg_at_k(&ranked, judgments, 10),
                recall_at_100: recall_at_k(&ranked, judgments, 100),
                recall_at_1000: recall_at_k(&ranked, judgments, 1000),
                mrr_at_10: mrr_at_k(&ranked, judgments, 10),
            },
        );
    }

    let n = per_query.len().max(1) as f64;
    let mut mean = QueryMetrics::default();
    for m in per_query.values() {
        mean.ndcg_at_10 += m.ndcg_at_10 / n;
        mean.recall_at_100 += m.recall_at_100 / n;
        mean.recall_at_1000 += m.recall_at_1000 / n;
        mean.mrr_at_10 += m.mrr_at_10 / n;
    }
    Evaluation {
        num_queries: per_query.len(),
        num_queries_without_results: without_results,
        mean,
        per_query,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranked(docs: &[&str]) -> Vec<(String, f64)> {
        // Strictly decreasing scores so trec_order keeps this order.
        docs.iter()
            .enumerate()
            .map(|(i, d)| (d.to_string(), 100.0 - i as f64))
            .collect()
    }

    fn judgments(pairs: &[(&str, i32)]) -> HashMap<String, i32> {
        pairs.iter().map(|(d, r)| (d.to_string(), *r)).collect()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// Hand-computed graded example.
    /// Ranking: [a(2), b(0), c(1), d(unjudged)], judged: a=2, c=1, e=2, b=0.
    /// DCG@10  = 2/log2(2) + 0 + 1/log2(4)       = 2 + 0.5            = 2.5
    /// IDCG@10 = 2/log2(2) + 2/log2(3) + 1/log2(4) = 2 + 1.261860 + 0.5 = 3.761860
    /// nDCG@10 = 2.5 / 3.761860 = 0.664565
    #[test]
    fn ndcg_hand_computed() {
        let r = ranked(&["a", "b", "c", "d"]);
        let j = judgments(&[("a", 2), ("b", 0), ("c", 1), ("e", 2)]);
        let expected = 2.5 / (2.0 + 2.0 / 3f64.log2() + 0.5);
        assert!(close(ndcg_at_k(&r, &j, 10), expected));
        assert!((ndcg_at_k(&r, &j, 10) - 0.664565).abs() < 1e-6);
        // Cutoff 1: DCG = 2, IDCG = 2 -> 1.0
        assert!(close(ndcg_at_k(&r, &j, 1), 1.0));
    }

    #[test]
    fn ndcg_perfect_and_zero() {
        let j = judgments(&[("a", 3), ("b", 1)]);
        assert!(close(ndcg_at_k(&ranked(&["a", "b"]), &j, 10), 1.0));
        assert!(close(ndcg_at_k(&ranked(&["x", "y"]), &j, 10), 0.0));
        // No relevant docs at all -> 0, not NaN.
        assert!(close(
            ndcg_at_k(&ranked(&["a"]), &judgments(&[("a", 0)]), 10),
            0.0
        ));
    }

    #[test]
    fn recall_hand_computed() {
        let j = judgments(&[("a", 1), ("c", 2), ("z", 1), ("b", 0)]);
        let r = ranked(&["a", "b", "c", "d"]);
        assert!(close(recall_at_k(&r, &j, 100), 2.0 / 3.0));
        assert!(close(recall_at_k(&r, &j, 1), 1.0 / 3.0));
        assert!(close(recall_at_k(&r, &judgments(&[]), 100), 0.0));
    }

    #[test]
    fn mrr_hand_computed() {
        let j = judgments(&[("c", 1), ("b", 0)]);
        assert!(close(
            mrr_at_k(&ranked(&["a", "b", "c"]), &j, 10),
            1.0 / 3.0
        ));
        assert!(close(mrr_at_k(&ranked(&["a", "b", "c"]), &j, 2), 0.0));
    }

    #[test]
    fn trec_order_breaks_ties_by_doc_id_descending() {
        let mut r = vec![
            ("a".to_string(), 1.0),
            ("c".to_string(), 2.0),
            ("b".to_string(), 1.0),
            ("d".to_string(), 1.0),
        ];
        trec_order(&mut r);
        let ids: Vec<&str> = r.iter().map(|(d, _)| d.as_str()).collect();
        assert_eq!(ids, vec!["c", "d", "b", "a"]);
    }

    #[test]
    fn evaluate_averages_over_all_qrels_queries() {
        let mut qrels = Qrels::new();
        qrels.insert("q1".into(), judgments(&[("a", 1)]));
        qrels.insert("q2".into(), judgments(&[("b", 1)]));
        let mut run = Run::new();
        run.insert("q1".into(), ranked(&["a"]));
        // q2 has no results -> counts as 0 (trec_eval -c).
        run.insert("q3_not_in_qrels".into(), ranked(&["x"]));
        let e = evaluate(&qrels, &run);
        assert_eq!(e.num_queries, 2);
        assert_eq!(e.num_queries_without_results, 1);
        assert!(close(e.mean.ndcg_at_10, 0.5));
        assert!(close(e.mean.mrr_at_10, 0.5));
    }

    #[test]
    fn evaluate_resorts_run_by_score() {
        let mut qrels = Qrels::new();
        qrels.insert("q".into(), judgments(&[("good", 1)]));
        let mut run = Run::new();
        // Stored worst-first; evaluate must not trust the stored order.
        run.insert("q".into(), vec![("bad".into(), 1.0), ("good".into(), 5.0)]);
        assert!(close(evaluate(&qrels, &run).mean.mrr_at_10, 1.0));
    }
}
