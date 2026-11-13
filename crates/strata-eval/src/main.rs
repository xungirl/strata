//! `strata-eval`: index a BEIR dataset with strata, run every test query,
//! score the run and record timings.
//!
//! ```text
//! strata-eval run  --dataset scifact [--k1 0.9 --b 0.4] [--repeats 3]
//! strata-eval eval --dataset scifact --run results/runs/scifact/some.trec
//! ```
//!
//! Outputs (per `run`):
//! * `results/runs/<dataset>/<tag>.trec` - TREC run file (top `--hits` per query)
//! * `results/<dataset>/<tag>.json`      - metrics, index stats, timings per repeat

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

use strata_core::analyzer::{StemmerKind, Stopwords};
use strata_core::scoring::DocLengthEncoding;
use strata_core::{
    Analyzer, AnalyzerConfig, Bm25Params, InMemoryIndexBuilder, InMemorySegment, Query, Searcher,
    SegmentReader,
};
use strata_eval::beir::Dataset;
use strata_eval::harness::{mean_std, read_trec_run, write_trec_run, LatencyStats};
use strata_eval::metrics::{evaluate, Evaluation, Run};

#[derive(Parser)]
#[command(
    name = "strata-eval",
    about = "BEIR evaluation for the strata search engine"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Index a dataset, run all test queries, evaluate and time everything.
    Run(RunArgs),
    /// Evaluate an existing TREC run file against a dataset's qrels.
    Eval {
        #[arg(long)]
        dataset: String,
        #[arg(long)]
        run: PathBuf,
        #[arg(long, default_value = "data")]
        data_dir: PathBuf,
        #[arg(long, default_value = "test")]
        split: String,
    },
}

#[derive(clap::Args)]
struct RunArgs {
    /// BEIR dataset name, e.g. scifact, nfcorpus, fiqa.
    #[arg(long)]
    dataset: String,
    #[arg(long, default_value = "data")]
    data_dir: PathBuf,
    #[arg(long, default_value = "test")]
    split: String,
    #[arg(long, default_value_t = 0.9)]
    k1: f32,
    #[arg(long, default_value_t = 0.4)]
    b: f32,
    #[arg(long, value_enum, default_value_t = DocLen::Exact)]
    doc_length: DocLen,
    #[arg(long, value_enum, default_value_t = Stem::Porter2)]
    stemmer: Stem,
    #[arg(long, value_enum, default_value_t = Stop::Lucene)]
    stopwords: Stop,
    /// Results kept per query (Anserini uses 1000).
    #[arg(long, default_value_t = 1000)]
    hits: usize,
    /// How many times to rebuild the index and re-run all queries.
    #[arg(long, default_value_t = 3)]
    repeats: usize,
    /// Keep hits whose doc id equals the query id (Anserini's -removeQuery
    /// drops them; so do we by default).
    #[arg(long)]
    keep_self_hits: bool,
    #[arg(long, default_value = "results")]
    results_dir: PathBuf,
    /// Name for output files; derived from the settings if omitted.
    #[arg(long)]
    tag: Option<String>,
}

#[derive(Clone, Copy, ValueEnum, Serialize)]
enum DocLen {
    Exact,
    Lucene,
}
#[derive(Clone, Copy, ValueEnum, Serialize)]
enum Stem {
    Porter2,
    None,
}
#[derive(Clone, Copy, ValueEnum, Serialize)]
enum Stop {
    Lucene,
    None,
}

impl RunArgs {
    fn analyzer_config(&self) -> AnalyzerConfig {
        AnalyzerConfig {
            stemmer: match self.stemmer {
                Stem::Porter2 => StemmerKind::Porter2,
                Stem::None => StemmerKind::None,
            },
            stopwords: match self.stopwords {
                Stop::Lucene => Stopwords::LuceneEnglish,
                Stop::None => Stopwords::None,
            },
            ..AnalyzerConfig::english()
        }
    }

    fn params(&self) -> Bm25Params {
        Bm25Params {
            k1: self.k1,
            b: self.b,
            doc_length: match self.doc_length {
                DocLen::Exact => DocLengthEncoding::Exact,
                DocLen::Lucene => DocLengthEncoding::LuceneByte4,
            },
        }
    }

    fn tag(&self) -> String {
        self.tag.clone().unwrap_or_else(|| {
            let dl = match self.doc_length {
                DocLen::Exact => "exactlen",
                DocLen::Lucene => "lucenelen",
            };
            let stem = match self.stemmer {
                Stem::Porter2 => "porter2",
                Stem::None => "nostem",
            };
            let stop = match self.stopwords {
                Stop::Lucene => "lucenestop",
                Stop::None => "nostop",
            };
            format!("strata.k1_{}.b_{}.{dl}.{stem}.{stop}", self.k1, self.b)
        })
    }
}

/// Everything recorded for one run, serialized to JSON.
#[derive(Serialize)]
struct Report {
    engine: &'static str,
    dataset: String,
    tag: String,
    analyzer: AnalyzerConfig,
    params: Bm25Params,
    hits: usize,
    remove_self_hits: bool,
    load_secs: f64,
    index: IndexStats,
    metrics: Evaluation,
    repeats: Vec<Repeat>,
    summary: Summary,
    unix_time: u64,
}

#[derive(Serialize)]
struct IndexStats {
    docs: u32,
    terms: usize,
    postings: usize,
    total_tokens: u64,
    avg_doc_len: f32,
    /// Capacity-based estimate; see `InMemorySegment::heap_size_bytes`.
    heap_size_bytes_estimate: usize,
}

#[derive(Serialize)]
struct Repeat {
    /// Analysis only (no indexing), to show how much of indexing is analysis.
    analyze_secs: f64,
    index_secs: f64,
    docs_per_sec: f64,
    /// Latency of the evaluated run (top `--hits`, default 1000).
    latency: LatencyStats,
    /// Latency of a second pass with k = 10 (a typical "first page" query).
    latency_k10: LatencyStats,
}

#[derive(Serialize)]
struct Summary {
    docs_per_sec_mean: f64,
    docs_per_sec_std: f64,
    p50_us_mean: f64,
    p50_us_std: f64,
    p99_us_mean: f64,
    p99_us_std: f64,
    qps_mean: f64,
    p50_us_k10_mean: f64,
    p99_us_k10_mean: f64,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Run(args) => run(args),
        Command::Eval {
            dataset,
            run,
            data_dir,
            split,
        } => {
            let ds = Dataset::load(&data_dir, &dataset, &split)?;
            let e = evaluate(&ds.qrels, &read_trec_run(&run)?);
            println!("{}", serde_json::to_string_pretty(&e)?);
            Ok(())
        }
    }
}

fn build_index(ds: &Dataset, config: &AnalyzerConfig) -> (InMemorySegment, Duration) {
    let start = Instant::now();
    let mut builder = InMemoryIndexBuilder::new(Analyzer::new(config.clone()));
    for doc in &ds.corpus {
        builder.add_document(&doc.id, &doc.flat_text());
    }
    let segment = builder.build();
    (segment, start.elapsed())
}

/// Run every query once; returns the run and per-query engine latencies
/// (query analysis + search; excludes turning hits into strings).
fn run_queries(
    ds: &Dataset,
    segment: &InMemorySegment,
    analyzer: &Analyzer,
    params: Bm25Params,
    hits: usize,
    remove_self_hits: bool,
) -> (Run, Vec<Duration>) {
    let searcher = Searcher::new(segment, params);
    let mut run: Run = HashMap::with_capacity(ds.queries.len());
    let mut latencies = Vec::with_capacity(ds.queries.len());
    // Ask for one extra hit so dropping a self-hit still leaves `hits`.
    let k = hits + usize::from(remove_self_hits);
    for (qid, text) in &ds.queries {
        let start = Instant::now();
        let query = Query::bag_of_words(analyzer, text);
        let top = searcher.search(&query, k);
        latencies.push(start.elapsed());

        let results: Vec<(String, f64)> = top
            .iter()
            .map(|h| (segment.external_id(h.doc).to_string(), f64::from(h.score)))
            .filter(|(doc, _)| !(remove_self_hits && doc == qid))
            .take(hits)
            .collect();
        run.insert(qid.clone(), results);
    }
    (run, latencies)
}

fn run(args: RunArgs) -> Result<()> {
    let tag = args.tag();
    let config = args.analyzer_config();
    let params = args.params();
    let remove_self_hits = !args.keep_self_hits;

    let load_start = Instant::now();
    let ds = Dataset::load(&args.data_dir, &args.dataset, &args.split)?;
    let load_secs = load_start.elapsed().as_secs_f64();
    eprintln!(
        "[{}] loaded {} docs, {} queries in {:.2}s",
        ds.name,
        ds.corpus.len(),
        ds.queries.len(),
        load_secs
    );

    let analyzer = Analyzer::new(config.clone());
    let mut repeats = Vec::new();
    let mut first: Option<(Run, IndexStats)> = None;
    for r in 0..args.repeats.max(1) {
        // Analysis-only pass.
        let a_start = Instant::now();
        let mut tokens = 0usize;
        let mut buf = Vec::new();
        for doc in &ds.corpus {
            buf.clear();
            analyzer.analyze_into(&doc.flat_text(), &mut buf);
            tokens += buf.len();
        }
        let analyze_secs = a_start.elapsed().as_secs_f64();

        let (segment, index_time) = build_index(&ds, &config);
        assert_eq!(tokens as u64, segment.total_tokens());
        let (run, latencies) = run_queries(
            &ds,
            &segment,
            &analyzer,
            params,
            args.hits,
            remove_self_hits,
        );
        let latency = LatencyStats::from_samples(&latencies);
        let (_, latencies_k10) =
            run_queries(&ds, &segment, &analyzer, params, 10, remove_self_hits);
        let latency_k10 = LatencyStats::from_samples(&latencies_k10);
        let index_secs = index_time.as_secs_f64();
        let rep = Repeat {
            analyze_secs,
            index_secs,
            docs_per_sec: ds.corpus.len() as f64 / index_secs,
            latency,
            latency_k10,
        };
        eprintln!(
            "[{}] repeat {}: index {:.3}s ({:.0} docs/s, analysis-only {:.3}s), queries p50 {:.0}us p99 {:.0}us, k=10 p50 {:.0}us p99 {:.0}us",
            ds.name, r + 1, rep.index_secs, rep.docs_per_sec, rep.analyze_secs, rep.latency.p50_us, rep.latency.p99_us,
            rep.latency_k10.p50_us, rep.latency_k10.p99_us
        );
        repeats.push(rep);

        match &first {
            None => {
                let stats = IndexStats {
                    docs: segment.num_docs(),
                    terms: segment.num_terms(),
                    postings: segment.num_postings(),
                    total_tokens: segment.total_tokens(),
                    avg_doc_len: segment.stats().avg_doc_len(),
                    heap_size_bytes_estimate: segment.heap_size_bytes(),
                };
                first = Some((run, stats));
            }
            // Search is deterministic: every repeat must give the same run.
            Some((first_run, _)) => assert!(first_run == &run, "non-deterministic results"),
        }
    }
    let (run, index) = first.expect("at least one repeat");

    let run_path = args
        .results_dir
        .join("runs")
        .join(&ds.name)
        .join(format!("{tag}.trec"));
    write_trec_run(&run_path, &run, "strata")?;
    let metrics = evaluate(&ds.qrels, &run);

    let col = |f: fn(&Repeat) -> f64| repeats.iter().map(f).collect::<Vec<f64>>();
    let (dps_m, dps_s) = mean_std(&col(|r| r.docs_per_sec));
    let (p50_m, p50_s) = mean_std(&col(|r| r.latency.p50_us));
    let (p99_m, p99_s) = mean_std(&col(|r| r.latency.p99_us));
    let (qps_m, _) = mean_std(&col(|r| r.latency.qps));
    let (p50_k10, _) = mean_std(&col(|r| r.latency_k10.p50_us));
    let (p99_k10, _) = mean_std(&col(|r| r.latency_k10.p99_us));

    let report = Report {
        engine: "strata",
        dataset: ds.name.clone(),
        tag: tag.clone(),
        analyzer: config,
        params,
        hits: args.hits,
        remove_self_hits,
        load_secs,
        index,
        metrics,
        repeats,
        summary: Summary {
            docs_per_sec_mean: dps_m,
            docs_per_sec_std: dps_s,
            p50_us_mean: p50_m,
            p50_us_std: p50_s,
            p99_us_mean: p99_m,
            p99_us_std: p99_s,
            qps_mean: qps_m,
            p50_us_k10_mean: p50_k10,
            p99_us_k10_mean: p99_k10,
        },
        unix_time: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
    };
    let json_path = args.results_dir.join(&ds.name).join(format!("{tag}.json"));
    std::fs::create_dir_all(json_path.parent().unwrap())?;
    std::fs::write(&json_path, serde_json::to_string_pretty(&report)?)?;

    let m = &report.metrics.mean;
    println!(
        "{}\t{}\tnDCG@10={:.4}\tR@100={:.4}\tR@1000={:.4}\tMRR@10={:.4}\tdocs/s={:.0}±{:.0}\tp50={:.0}us\tp99={:.0}us\tk10_p50={:.0}us\tk10_p99={:.0}us",
        ds.name, tag, m.ndcg_at_10, m.recall_at_100, m.recall_at_1000, m.mrr_at_10,
        dps_m, dps_s, p50_m, p99_m, p50_k10, p99_k10
    );
    eprintln!("wrote {} and {}", json_path.display(), run_path.display());
    Ok(())
}
