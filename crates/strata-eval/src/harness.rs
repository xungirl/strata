//! Shared plumbing for experiments: TREC run files, latency statistics and
//! simple mean/stdev helpers.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::metrics::{trec_order, Run};

/// Write `run` in standard TREC format: `qid Q0 docid rank score tag`.
///
/// Scores are printed with Rust's shortest round-trip float formatting, so
/// reading the file back yields exactly the same `f64` values (important:
/// trec_eval breaks ties by score, so rounding scores could reorder results).
pub fn write_trec_run(path: &Path, run: &Run, tag: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out =
        BufWriter::new(File::create(path).with_context(|| format!("create {}", path.display()))?);
    let mut qids: Vec<&String> = run.keys().collect();
    qids.sort();
    for qid in qids {
        let mut results = run[qid].clone();
        trec_order(&mut results);
        for (rank, (doc, score)) in results.iter().enumerate() {
            writeln!(out, "{qid} Q0 {doc} {} {score} {tag}", rank + 1)?;
        }
    }
    out.flush()?;
    Ok(())
}

/// Read a TREC run file (whitespace separated, 6 columns).
pub fn read_trec_run(path: &Path) -> Result<Run> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut run: Run = HashMap::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.is_empty() {
            continue;
        }
        let [qid, _q0, doc, _rank, score, _tag] = cols[..] else {
            bail!("{}:{}: expected 6 columns", path.display(), i + 1);
        };
        let score: f64 = score
            .parse()
            .with_context(|| format!("{}:{}: bad score", path.display(), i + 1))?;
        run.entry(qid.to_string())
            .or_default()
            .push((doc.to_string(), score));
    }
    Ok(run)
}

/// Summary of per-query latencies, in microseconds.
#[derive(Clone, Debug, Serialize)]
pub struct LatencyStats {
    pub queries: usize,
    pub total_secs: f64,
    pub qps: f64,
    pub mean_us: f64,
    pub p50_us: f64,
    pub p90_us: f64,
    pub p99_us: f64,
    pub max_us: f64,
}

/// Percentile by the *nearest-rank* method: the smallest sample such that at
/// least `p` percent of samples are <= it. `sorted` must be ascending.
pub fn percentile(sorted: &[Duration], p: f64) -> Duration {
    assert!(!sorted.is_empty(), "percentile of empty sample");
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

impl LatencyStats {
    /// Compute stats from raw per-query durations (order does not matter).
    pub fn from_samples(samples: &[Duration]) -> LatencyStats {
        let mut sorted = samples.to_vec();
        sorted.sort();
        let total: Duration = sorted.iter().sum();
        let us = |d: Duration| d.as_secs_f64() * 1e6;
        LatencyStats {
            queries: sorted.len(),
            total_secs: total.as_secs_f64(),
            qps: sorted.len() as f64 / total.as_secs_f64(),
            mean_us: us(total) / sorted.len() as f64,
            p50_us: us(percentile(&sorted, 50.0)),
            p90_us: us(percentile(&sorted, 90.0)),
            p99_us: us(percentile(&sorted, 99.0)),
            max_us: us(*sorted.last().unwrap()),
        }
    }
}

/// Mean and sample standard deviation (n - 1 denominator).
pub fn mean_std(xs: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    if xs.len() < 2 {
        return (mean, 0.0);
    }
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
    (mean, var.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let s: Vec<Duration> = (1..=100).map(Duration::from_micros).collect();
        assert_eq!(percentile(&s, 50.0), Duration::from_micros(50));
        assert_eq!(percentile(&s, 99.0), Duration::from_micros(99));
        assert_eq!(percentile(&s, 100.0), Duration::from_micros(100));
        assert_eq!(percentile(&s, 0.0), Duration::from_micros(1));
        let one = [Duration::from_micros(7)];
        assert_eq!(percentile(&one, 99.0), Duration::from_micros(7));
    }

    #[test]
    fn mean_and_sample_std() {
        let (m, s) = mean_std(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]);
        assert!((m - 5.0).abs() < 1e-12);
        assert!((s - 2.138089935).abs() < 1e-6);
    }

    #[test]
    fn trec_run_roundtrip_preserves_scores_exactly() {
        let path = std::env::temp_dir().join(format!("strata-run-{}.trec", std::process::id()));
        let mut run = Run::new();
        run.insert(
            "q1".into(),
            vec![("d1".into(), 0.1 + 0.2), ("d2".into(), 12.345678901234567)],
        );
        write_trec_run(&path, &run, "test").unwrap();
        let back = read_trec_run(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let mut expected = run["q1"].clone();
        trec_order(&mut expected);
        assert_eq!(back["q1"], expected);
    }
}
