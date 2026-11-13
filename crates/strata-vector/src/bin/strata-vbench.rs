//! `strata-vbench`: dense retrieval benchmarks on cached BEIR embeddings.
//!
//! ```text
//! strata-vbench --dataset scifact --out results/stage4/scifact.json
//! ```
//!
//! Measures, single-threaded:
//! 1. exact brute-force search (ground truth): latency, and BEIR metrics of
//!    the dense run (top 1000, written as a TREC run for stage 5 fusion);
//! 2. HNSW build time (repeated), graph size;
//! 3. recall@10 vs exact and latency, for a sweep of `ef`, with f32 and with
//!    int8 scalar quantization (+ f32 rescoring);
//! 4. filtered search at 1% / 10% / 50% selectivity (synthetic
//!    `bucket = FNV-1a(id) % 100` field, the same one the lexical stage-3
//!    filter benchmark uses): filter-during-traversal vs brute-force fallback;
//! 5. effectiveness of HNSW runs (nDCG@10 vs the exact dense run);
//! 6. a micro-benchmark of the dot-product kernels (scalar vs NEON).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use serde::Serialize;
use strata_eval::beir::Dataset;
use strata_eval::harness::{mean_std, write_trec_run, LatencyStats};
use strata_eval::metrics::{evaluate, Run};
use strata_vector::distance::{dot, dot_i8, dot_i8_scalar, dot_scalar};
use strata_vector::embeddings::{DatasetEmbeddings, EmbeddingMeta};
use strata_vector::{exact_top_k, Hnsw, HnswConfig, Neighbor, ScalarQuantizer, SearchContext};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    dataset: String,
    #[arg(long, default_value = "data")]
    data_dir: PathBuf,
    #[arg(long, default_value_t = 16)]
    m: usize,
    #[arg(long, default_value_t = 200)]
    ef_construction: usize,
    #[arg(long, value_delimiter = ',', default_value = "10,20,40,80,160,320,640")]
    efs: Vec<usize>,
    #[arg(long, default_value_t = 3)]
    repeats: usize,
    #[arg(long)]
    out: PathBuf,
    /// Directory for TREC run files (dense-exact and HNSW runs).
    #[arg(long, default_value = "results/stage4/runs")]
    runs_dir: PathBuf,
}

#[derive(Serialize)]
struct EfPoint {
    ef: usize,
    recall_at_10: f64,
    avg_distance_evals: f64,
    /// One LatencyStats per repeat.
    latency: Vec<LatencyStats>,
}

#[derive(Serialize)]
struct FilterPoint {
    selectivity_pct: u32,
    allowed_docs: usize,
    ef: usize,
    /// HNSW with the filter applied during traversal.
    graph_recall_at_10: f64,
    graph_latency: LatencyStats,
    graph_avg_distance_evals: f64,
    /// Exact scan over the allowed docs.
    brute_latency: LatencyStats,
}

#[derive(Serialize)]
struct Report {
    dataset: String,
    embeddings: EmbeddingMeta,
    docs: usize,
    dim: usize,
    test_queries: usize,
    m: usize,
    ef_construction: usize,
    vectors_bytes: usize,
    sq_bytes: usize,
    sq_scale: f32,
    graph_bytes: usize,
    graph_max_level: usize,
    graph_avg_degree0: f64,
    build_secs: Vec<f64>,
    build_secs_mean: f64,
    build_secs_std: f64,
    sq_fit_secs: f64,
    exact_latency: Vec<LatencyStats>,
    exact_metrics: strata_eval::metrics::QueryMetrics,
    hnsw_f32: Vec<EfPoint>,
    hnsw_sq8: Vec<EfPoint>,
    filtered: Vec<FilterPoint>,
    /// nDCG@10 etc. of HNSW runs (top 100, ef = max(ef, 100)).
    hnsw_metrics: Vec<(usize, strata_eval::metrics::QueryMetrics)>,
    kernel_ns_per_dot: HashMap<String, f64>,
    load_avg_1m: f64,
}

fn load_avg_1m() -> f64 {
    std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.split_whitespace().nth(1).map(str::to_string))
        .and_then(|s| s.parse().ok())
        .unwrap_or(f64::NAN)
}

fn bucket(id: &str) -> u32 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h % 100) as u32
}

fn recall(truth: &[Vec<u32>], got: &[Vec<Neighbor>]) -> f64 {
    let mut hit = 0usize;
    let mut total = 0usize;
    for (t, g) in truth.iter().zip(got) {
        total += t.len();
        hit += g.iter().filter(|n| t.contains(&n.id)).count();
    }
    hit as f64 / total.max(1) as f64
}

/// Turn neighbor lists into a TREC run (dropping self-hits like Anserini).
fn to_run(qids: &[String], ids: &[String], results: &[Vec<Neighbor>], k: usize) -> Run {
    let mut run = Run::new();
    for (qid, res) in qids.iter().zip(results) {
        let list: Vec<(String, f64)> = res
            .iter()
            .map(|n| (ids[n.id as usize].clone(), f64::from(n.sim)))
            .filter(|(d, _)| d != qid)
            .take(k)
            .collect();
        run.insert(qid.clone(), list);
    }
    run
}

fn main() -> Result<()> {
    let args = Args::parse();
    let load = load_avg_1m();
    let ds = Dataset::load(&args.data_dir, &args.dataset, "test")?;
    let emb = DatasetEmbeddings::load(&args.data_dir, &args.dataset)?;
    let qids: Vec<String> = ds.queries.iter().map(|(q, _)| q.clone()).collect();
    let qvecs: Vec<&[f32]> = qids
        .iter()
        .map(|q| emb.query(q).expect("query embedding"))
        .collect();
    let n = emb.corpus.len();
    eprintln!(
        "[{}] {} docs x {} dims, {} test queries, load {load:.2}",
        args.dataset,
        n,
        emb.meta.dim,
        qids.len()
    );

    // 1. Exact search: ground truth + dense run.
    let mut exact_latency = Vec::new();
    let mut exact: Vec<Vec<Neighbor>> = Vec::new();
    for _ in 0..args.repeats {
        let mut lat = Vec::new();
        exact.clear();
        for q in &qvecs {
            let t = Instant::now();
            exact.push(exact_top_k(&emb.corpus, q, 1001, &|_| true));
            lat.push(t.elapsed());
        }
        exact_latency.push(LatencyStats::from_samples(&lat));
    }
    let truth10: Vec<Vec<u32>> = exact
        .iter()
        .map(|r| r.iter().take(10).map(|n| n.id).collect())
        .collect();
    let dense_run = to_run(&qids, &emb.corpus_ids, &exact, 1000);
    write_trec_run(
        &args
            .runs_dir
            .join(&args.dataset)
            .join("dense-exact.bge-small.trec"),
        &dense_run,
        "dense",
    )?;
    let exact_metrics = evaluate(&ds.qrels, &dense_run).mean;
    eprintln!(
        "[{}] exact: nDCG@10 {:.4}, p50 {:.0}us",
        args.dataset, exact_metrics.ndcg_at_10, exact_latency[0].p50_us
    );

    // 2. Build (repeated).
    let cfg = HnswConfig {
        m: args.m,
        ef_construction: args.ef_construction,
        seed: 42,
    };
    let mut build_secs = Vec::new();
    let mut graph = None;
    for _ in 0..args.repeats {
        let t = Instant::now();
        let h = Hnsw::build(&emb.corpus, cfg);
        build_secs.push(t.elapsed().as_secs_f64());
        graph = Some(h);
    }
    let graph = graph.expect("built");
    let (bm, bs) = mean_std(&build_secs);
    eprintln!("[{}] HNSW build {:.1}s ± {:.1}", args.dataset, bm, bs);

    let t = Instant::now();
    let sq = ScalarQuantizer::fit(&emb.corpus, 0.999);
    let sq_fit_secs = t.elapsed().as_secs_f64();

    // 3. ef sweeps.
    let mut ctx = SearchContext::new(n);
    let mut sweep = |use_sq: bool| -> Vec<EfPoint> {
        let mut points = Vec::new();
        for &ef in &args.efs {
            let mut latency = Vec::new();
            let mut results = Vec::new();
            let mut evals = 0usize;
            for _ in 0..args.repeats {
                let mut lat = Vec::new();
                results.clear();
                evals = 0;
                for q in &qvecs {
                    let t = Instant::now();
                    let r = if use_sq {
                        graph.search_sq(&sq, &emb.corpus, q, 10, ef, None, &mut ctx)
                    } else {
                        graph.search(&emb.corpus, q, 10, ef, None, &mut ctx)
                    };
                    lat.push(t.elapsed());
                    evals += ctx.distance_evals;
                    results.push(r);
                }
                latency.push(LatencyStats::from_samples(&lat));
            }
            let r = recall(&truth10, &results);
            eprintln!(
                "[{}] {} ef={ef:4}: recall@10 {r:.4}, p50 {:.0}us p99 {:.0}us",
                args.dataset,
                if use_sq { "sq8" } else { "f32" },
                latency[0].p50_us,
                latency[0].p99_us
            );
            points.push(EfPoint {
                ef,
                recall_at_10: r,
                avg_distance_evals: evals as f64 / qvecs.len() as f64,
                latency,
            });
        }
        points
    };
    let hnsw_f32 = sweep(false);
    let hnsw_sq8 = sweep(true);

    // 4. Filtered.
    let buckets: Vec<u32> = emb.corpus_ids.iter().map(|id| bucket(id)).collect();
    let mut filtered = Vec::new();
    for sel in [1u32, 10, 50] {
        let allow = |id: u32| buckets[id as usize] < sel;
        let allowed = buckets.iter().filter(|&&b| b < sel).count();
        let mut btimes = Vec::new();
        let truth: Vec<Vec<u32>> = qvecs
            .iter()
            .map(|q| {
                let t = Instant::now();
                let r = exact_top_k(&emb.corpus, q, 10, &allow);
                btimes.push(t.elapsed());
                r.iter().map(|n| n.id).collect()
            })
            .collect();
        for ef in [40usize, 160, 640] {
            let mut lat = Vec::new();
            let mut evals = 0;
            let got: Vec<Vec<Neighbor>> = qvecs
                .iter()
                .map(|q| {
                    let t = Instant::now();
                    let r = graph.search(&emb.corpus, q, 10, ef, Some(&allow), &mut ctx);
                    lat.push(t.elapsed());
                    evals += ctx.distance_evals;
                    r
                })
                .collect();
            filtered.push(FilterPoint {
                selectivity_pct: sel,
                allowed_docs: allowed,
                ef,
                graph_recall_at_10: recall(&truth, &got),
                graph_latency: LatencyStats::from_samples(&lat),
                graph_avg_distance_evals: evals as f64 / qvecs.len() as f64,
                brute_latency: LatencyStats::from_samples(&btimes),
            });
            let fp = filtered.last().unwrap();
            eprintln!(
                "[{}] filter {sel}% ef={ef}: graph recall {:.4} p50 {:.0}us | brute p50 {:.0}us",
                args.dataset,
                fp.graph_recall_at_10,
                fp.graph_latency.p50_us,
                fp.brute_latency.p50_us
            );
        }
    }

    // 5. HNSW effectiveness (top 100).
    let mut hnsw_metrics = Vec::new();
    for ef in [100usize, 200, 400] {
        let res: Vec<Vec<Neighbor>> = qvecs
            .iter()
            .map(|q| graph.search(&emb.corpus, q, 101, ef, None, &mut ctx))
            .collect();
        let run = to_run(&qids, &emb.corpus_ids, &res, 100);
        write_trec_run(
            &args
                .runs_dir
                .join(&args.dataset)
                .join(format!("dense-hnsw-ef{ef}.bge-small.trec")),
            &run,
            "hnsw",
        )?;
        hnsw_metrics.push((ef, evaluate(&ds.qrels, &run).mean));
    }

    // 6. Kernel micro-benchmark: 2M dot products on real vectors.
    let mut kernel = HashMap::new();
    let a = emb.corpus.get(0).to_vec();
    let qa = sq.encode(&a);
    let reps = 2_000_000usize;
    let time = |f: &dyn Fn(usize) -> f32| -> f64 {
        let t = Instant::now();
        let mut acc = 0.0f32;
        for i in 0..reps {
            acc += f(i % n);
        }
        std::hint::black_box(acc);
        t.elapsed().as_nanos() as f64 / reps as f64
    };
    kernel.insert(
        "f32_scalar".into(),
        time(&|i| dot_scalar(&a, emb.corpus.get(i))),
    );
    kernel.insert("f32_neon".into(), time(&|i| dot(&a, emb.corpus.get(i))));
    kernel.insert(
        "i8_scalar".into(),
        time(&|i| dot_i8_scalar(&qa, sq.code(i)) as f32),
    );
    kernel.insert("i8_neon".into(), time(&|i| dot_i8(&qa, sq.code(i)) as f32));
    eprintln!("[{}] kernels ns/dot: {:?}", args.dataset, kernel);

    let report = Report {
        dataset: args.dataset.clone(),
        embeddings: emb.meta.clone(),
        docs: n,
        dim: emb.meta.dim,
        test_queries: qids.len(),
        m: args.m,
        ef_construction: args.ef_construction,
        vectors_bytes: emb.corpus.bytes(),
        sq_bytes: sq.bytes(),
        sq_scale: sq.scale(),
        graph_bytes: graph.graph_bytes(),
        graph_max_level: graph.max_level(),
        graph_avg_degree0: graph.avg_degree0(),
        build_secs_mean: bm,
        build_secs_std: bs,
        build_secs,
        sq_fit_secs,
        exact_latency,
        exact_metrics,
        hnsw_f32,
        hnsw_sq8,
        filtered,
        hnsw_metrics,
        kernel_ns_per_dot: kernel,
        load_avg_1m: load,
    };
    if let Some(p) = args.out.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(&args.out, serde_json::to_string_pretty(&report)?)?;
    Ok(())
}
