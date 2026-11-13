//! Hybrid retrieval: fusing a lexical (BM25) and a dense (embedding) run.
//!
//! * **RRF** (Reciprocal Rank Fusion, Cormack et al. 2009):
//!   `score(d) = sum over runs of 1 / (k + rank_run(d))`, rank starting at 1,
//!   `k = 60`. Uses only ranks, so it needs no score calibration: BM25 scores
//!   are unbounded, cosine similarities live in [-1, 1].
//! * **Weighted min-max fusion** (a.k.a. convex combination):
//!   per query, scale each run's scores to [0, 1] with min-max over its
//!   retrieved list, then `alpha * lexical + (1 - alpha) * dense`; a doc
//!   missing from a run contributes 0 for it.
//! * **Weighted z-score fusion**: same with (s - mean) / std normalization.
//!
//! All fusion happens on run files (top-1000 lists), the way it would happen
//! in a service that queries both indexes and merges.

use std::collections::HashMap;

use crate::metrics::{trec_order, Run};

/// Score normalization for weighted fusion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Norm {
    MinMax,
    ZScore,
}

/// Reciprocal rank fusion of several runs, keeping the top `depth`.
pub fn rrf(runs: &[&Run], k: f64, depth: usize) -> Run {
    let mut out = Run::new();
    let mut qids: Vec<&String> = runs.iter().flat_map(|r| r.keys()).collect();
    qids.sort();
    qids.dedup();
    for qid in qids {
        let mut acc: HashMap<String, f64> = HashMap::new();
        for run in runs {
            if let Some(list) = run.get(qid) {
                let mut list = list.clone();
                trec_order(&mut list); // ranks as trec_eval would see them
                for (rank, (doc, _)) in list.into_iter().enumerate() {
                    // `entry` returns the slot for this key, inserting 0.0 if absent.
                    *acc.entry(doc).or_insert(0.0) += 1.0 / (k + rank as f64 + 1.0);
                }
            }
        }
        out.insert(qid.clone(), finish(acc, depth));
    }
    out
}

fn finish<K: Into<String>>(acc: HashMap<K, f64>, depth: usize) -> Vec<(String, f64)> {
    let mut list: Vec<(String, f64)> = acc.into_iter().map(|(d, s)| (d.into(), s)).collect();
    trec_order(&mut list);
    list.truncate(depth);
    list
}

fn normalize(list: &[(String, f64)], norm: Norm) -> HashMap<&str, f64> {
    if list.is_empty() {
        return HashMap::new();
    }
    let scores: Vec<f64> = list.iter().map(|(_, s)| *s).collect();
    let (a, b) = match norm {
        Norm::MinMax => {
            let min = scores.iter().copied().fold(f64::INFINITY, f64::min);
            let max = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            if max <= min {
                // Degenerate list (one hit, or all scores equal): every
                // retrieved doc is the best this retriever has to offer.
                return list.iter().map(|(d, _)| (d.as_str(), 1.0)).collect();
            }
            (min, max - min)
        }
        Norm::ZScore => {
            let n = scores.len() as f64;
            let mean = scores.iter().sum::<f64>() / n;
            let sd = (scores.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / n).sqrt();
            (mean, if sd > 0.0 { sd } else { 1.0 })
        }
    };
    list.iter()
        .map(|(d, s)| (d.as_str(), (s - a) / b))
        .collect()
}

/// `alpha * norm(lexical) + (1 - alpha) * norm(dense)` per query.
pub fn weighted(lexical: &Run, dense: &Run, alpha: f64, norm: Norm, depth: usize) -> Run {
    let mut out = Run::new();
    let mut qids: Vec<&String> = lexical.keys().chain(dense.keys()).collect();
    qids.sort();
    qids.dedup();
    let empty = Vec::new();
    for qid in qids {
        let l = normalize(lexical.get(qid).unwrap_or(&empty), norm);
        let d = normalize(dense.get(qid).unwrap_or(&empty), norm);
        // A doc missing from a run gets just *below* that run's minimum
        // normalized score (about 0 for min-max): "no evidence from this
        // retriever". Strictly below, so that alpha = 1 (or 0) reproduces
        // the lexical (or dense) ranking exactly instead of tying the
        // weakest retrieved doc with every non-retrieved one.
        const BELOW: f64 = 1e-9;
        let l_floor = l.values().copied().fold(f64::INFINITY, f64::min) - BELOW;
        let d_floor = d.values().copied().fold(f64::INFINITY, f64::min) - BELOW;
        let mut acc: HashMap<&str, f64> = HashMap::new();
        for doc in l.keys().chain(d.keys()) {
            let ls = l
                .get(doc)
                .copied()
                .unwrap_or(if l_floor.is_finite() { l_floor } else { 0.0 });
            let ds = d
                .get(doc)
                .copied()
                .unwrap_or(if d_floor.is_finite() { d_floor } else { 0.0 });
            acc.insert(doc, alpha * ls + (1.0 - alpha) * ds);
        }
        out.insert(qid.clone(), finish(acc, depth));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(pairs: &[(&str, f64)]) -> Run {
        let mut r = Run::new();
        r.insert(
            "q".into(),
            pairs.iter().map(|(d, s)| (d.to_string(), *s)).collect(),
        );
        r
    }

    #[test]
    fn rrf_hand_computed() {
        // a: ranks 1 and 2 -> 1/61 + 1/62; b: rank 2 in lex only -> 1/62; c: rank 1 in dense only.
        let lex = run(&[("a", 10.0), ("b", 5.0)]);
        let dense = run(&[("c", 0.9), ("a", 0.8)]);
        let f = rrf(&[&lex, &dense], 60.0, 10);
        let got: Vec<(&str, f64)> = f["q"].iter().map(|(d, s)| (d.as_str(), *s)).collect();
        assert_eq!(got[0].0, "a");
        assert!((got[0].1 - (1.0 / 61.0 + 1.0 / 62.0)).abs() < 1e-12);
        // b (1/62) vs c (1/61): c first.
        assert_eq!(got[1].0, "c");
        assert_eq!(got[2].0, "b");
    }

    #[test]
    fn weighted_minmax_hand_computed() {
        let lex = run(&[("a", 10.0), ("b", 0.0)]); // -> a 1.0, b 0.0
        let dense = run(&[("b", 0.9), ("c", 0.5)]); // -> b 1.0, c 0.0
        let f = weighted(&lex, &dense, 0.5, Norm::MinMax, 10);
        let s: HashMap<&str, f64> = f["q"].iter().map(|(d, s)| (d.as_str(), *s)).collect();
        assert!((s["a"] - 0.5).abs() < 1e-8); // 0.5*1 + 0.5*~0 (missing -> just below min 0)
        assert!((s["b"] - 0.5).abs() < 1e-8); // 0.5*0 + 0.5*1
        assert!(s["c"].abs() < 1e-8);
        // alpha = 1 reproduces the lexical ranking.
        let only_lex = weighted(&lex, &dense, 1.0, Norm::MinMax, 10);
        assert_eq!(only_lex["q"][0].0, "a");
    }

    #[test]
    fn extreme_alphas_reproduce_input_rankings() {
        let lex = run(&[("a", 3.0), ("b", 2.0), ("c", 1.0)]);
        let dense = run(&[("x", 0.9), ("c", 0.8), ("y", 0.7), ("a", 0.1)]);
        let ids = |r: &Run| -> Vec<String> { r["q"].iter().map(|(d, _)| d.clone()).collect() };
        for norm in [Norm::MinMax, Norm::ZScore] {
            let l = weighted(&lex, &dense, 1.0, norm, 3);
            assert_eq!(ids(&l), vec!["a", "b", "c"], "{norm:?}");
            let d = weighted(&lex, &dense, 0.0, norm, 4);
            assert_eq!(ids(&d), vec!["x", "c", "y", "a"], "{norm:?}");
        }
        // Degenerate one-hit lexical list stays on top at alpha = 1.
        let one = run(&[("z", 5.0)]);
        let r = weighted(&one, &dense, 1.0, Norm::MinMax, 2);
        assert_eq!(r["q"][0].0, "z");
    }
}
