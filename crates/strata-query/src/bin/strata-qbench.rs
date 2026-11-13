//! `strata-qbench`: latency of structured queries (phrase, prefix, boolean,
//! filtered) on an existing disk index, built from a BEIR query file.
//!
//! ```text
//! strata-qbench --index data/indexes/<dir> --queries data/trec-covid/queries.jsonl --repeats 3
//! ```
//!
//! For every BEIR query text, after analysis into terms t1, t2, t3, ...:
//! * `phrase`:   `"w1 w2"` from the first two non-stopword words of the text
//! * `prefix`:   first 4 characters of the longest word, plus `*`
//! * `boolean`:  `+t1 t2 -t3` (needs >= 3 terms)
//! * `filtered`: the full query plus `bucket:[0 TO 9]` (10% of docs; index
//!   must have been built with `strata-eval run --bucket-field`)
//! * `disjunction`: the plain query (MaxScore), for reference
//!
//! Each query type runs over all queries `--repeats` times; reported per
//! repeat: p50 / p99 / mean latency and average hits.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use serde::Serialize;
use strata_core::analyzer::{Analyzer, AnalyzerConfig};
use strata_core::scoring::Bm25Params;
use strata_query::{parse, search, Strategy};
use strata_storage::{Index, IndexConfig};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    index: PathBuf,
    #[arg(long)]
    queries: PathBuf,
    #[arg(long, default_value_t = 3)]
    repeats: usize,
    #[arg(long, default_value_t = 10)]
    k: usize,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Serialize)]
struct TypeResult {
    query_type: String,
    queries: usize,
    /// One entry per repeat.
    p50_us: Vec<f64>,
    p99_us: Vec<f64>,
    mean_us: Vec<f64>,
    avg_hits: f64,
    examples: Vec<String>,
}

fn pct(sorted: &[Duration], p: f64) -> f64 {
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1].as_secs_f64() * 1e6
}

fn main() -> Result<()> {
    let args = Args::parse();
    let analyzer = Analyzer::new(AnalyzerConfig::english());
    let cfg = IndexConfig {
        merge_factor: 0,
        ..Default::default()
    };
    let (index, _) = Index::open(&args.index, cfg)?;
    let snap = index.snapshot();

    // Raw query texts (all queries in the file; BEIR test queries are a subset
    // but every query is a fine latency probe).
    let texts: Vec<String> = std::fs::read_to_string(&args.queries)?
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v["text"].as_str().map(str::to_string))
        .collect();

    let mut by_type: Vec<(&str, Vec<String>)> = vec![
        ("disjunction", Vec::new()),
        ("phrase", Vec::new()),
        ("prefix", Vec::new()),
        ("boolean", Vec::new()),
        ("filtered", Vec::new()),
    ];
    for text in &texts {
        // Words with letters only, so the generated syntax stays valid.
        let words: Vec<String> = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 1 && w.chars().all(char::is_alphabetic))
            .map(str::to_lowercase)
            .filter(|w| !analyzer.analyze(w).is_empty())
            .collect();
        let terms = analyzer.analyze(&words.join(" "));
        by_type[0].1.push(words.join(" "));
        if words.len() >= 2 {
            by_type[1].1.push(format!("\"{} {}\"", words[0], words[1]));
        }
        if let Some(w) = words
            .iter()
            .max_by_key(|w| w.len())
            .filter(|w| w.len() >= 4)
        {
            by_type[2].1.push(format!("{}*", &w[..4]));
        }
        if terms.len() >= 3 {
            by_type[3]
                .1
                .push(format!("+{} {} -{}", terms[0], terms[1], terms[2]));
        }
        by_type[4]
            .1
            .push(format!("{} bucket:[0 TO 9]", words.join(" ")));
    }

    let mut results = Vec::new();
    for (name, queries) in &by_type {
        let parsed: Vec<_> = queries
            .iter()
            .map(|q| parse(q, &analyzer))
            .collect::<Result<_>>()?;
        let mut res = TypeResult {
            query_type: name.to_string(),
            queries: parsed.len(),
            p50_us: Vec::new(),
            p99_us: Vec::new(),
            mean_us: Vec::new(),
            avg_hits: 0.0,
            examples: queries.iter().take(3).cloned().collect(),
        };
        for _ in 0..args.repeats {
            let mut lat = Vec::with_capacity(parsed.len());
            let mut hits = 0usize;
            for q in &parsed {
                let t = Instant::now();
                let h = search(&snap, q, Bm25Params::ANSERINI, args.k, Strategy::Auto)?;
                lat.push(t.elapsed());
                hits += h.len();
            }
            lat.sort();
            res.p50_us.push(pct(&lat, 50.0));
            res.p99_us.push(pct(&lat, 99.0));
            res.mean_us
                .push(lat.iter().sum::<Duration>().as_secs_f64() * 1e6 / lat.len() as f64);
            res.avg_hits = hits as f64 / parsed.len() as f64;
        }
        eprintln!(
            "{name:12} n={:4} p50 {:?} us  p99 {:?} us  avg hits {:.1}",
            res.queries,
            res.p50_us.iter().map(|v| v.round()).collect::<Vec<_>>(),
            res.p99_us.iter().map(|v| v.round()).collect::<Vec<_>>(),
            res.avg_hits
        );
        results.push(res);
    }
    let json = serde_json::to_string_pretty(&results)?;
    match args.out {
        Some(p) => std::fs::write(p, json)?,
        None => println!("{json}"),
    }
    Ok(())
}
