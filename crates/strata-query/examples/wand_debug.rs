//! Print WAND work counters and timings per query vs exhaustive search for
//! an existing disk index. Diagnostic tool, not part of the benchmarks.
//! `cargo run --release -p strata-query --example wand_debug -- <index dir> <queries.jsonl> [k] [max queries]`
use std::time::Instant;
use strata_core::analyzer::{Analyzer, AnalyzerConfig};
use strata_core::scoring::Bm25Params;
use strata_core::search::Query;
use strata_storage::{Index, IndexConfig};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let k: usize = args.get(3).map_or(10, |s| s.parse().unwrap());
    let (index, _) = Index::open(
        std::path::Path::new(&args[1]),
        IndexConfig {
            merge_factor: 0,
            ..Default::default()
        },
    )?;
    let snap = index.snapshot();
    let analyzer = Analyzer::new(AnalyzerConfig::english());
    let limit: usize = args.get(4).map_or(usize::MAX, |s| s.parse().unwrap());
    for line in std::fs::read_to_string(&args[2])?.lines().take(limit) {
        let v: serde_json::Value = serde_json::from_str(line)?;
        let q = Query::bag_of_words(&analyzer, v["text"].as_str().unwrap());
        let terms: Vec<(String, u32)> = q
            .terms()
            .iter()
            .map(|t| (t.term.clone(), t.count))
            .collect();
        let t0 = Instant::now();
        let ex = snap.search(&q, Bm25Params::ANSERINI, k);
        let t_ex = t0.elapsed();
        let t1 = Instant::now();
        let (w, st) = strata_query::wand_search(&snap, &terms, &[], Bm25Params::ANSERINI, k);
        let t_w = t1.elapsed();
        assert_eq!(ex, w);
        let t2 = Instant::now();
        let (m, st2) = strata_query::maxscore_search(&snap, &terms, &[], Bm25Params::ANSERINI, k);
        let t_m = t2.elapsed();
        assert_eq!(ex, m);
        println!("   maxscore {:>6.0}us | {:?}", t_m.as_secs_f64() * 1e6, st2);
        println!(
            "{:>6.0}us taat {:>6.0}us wand | {:?} | terms {:?}",
            t_ex.as_secs_f64() * 1e6,
            t_w.as_secs_f64() * 1e6,
            st,
            terms
                .iter()
                .map(|(t, _)| format!("{t}:{}", snap.doc_freq(t)))
                .collect::<Vec<_>>()
        );
    }
    Ok(())
}
