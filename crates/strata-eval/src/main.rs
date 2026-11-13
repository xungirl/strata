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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

use strata_core::analyzer::{StemmerKind, Stopwords};
use strata_core::scoring::DocLengthEncoding;
use strata_core::Hit;
use strata_core::{
    Analyzer, AnalyzerConfig, Bm25Params, InMemoryIndexBuilder, InMemorySegment, Query, Searcher,
    SegmentReader,
};
use strata_eval::beir::Dataset;
use strata_eval::harness::{mean_std, read_trec_run, write_trec_run, LatencyStats};
use strata_eval::metrics::{evaluate, Evaluation, Run};
use strata_query::{BoolQuery, Clause};
use strata_storage::{Document, FieldValue, Fields, Index, IndexConfig, Snapshot, SnapshotHit};

/// The index built for one repeat.
enum Built {
    Memory(InMemorySegment),
    Disk(Arc<Snapshot>),
}

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
    #[arg(long, value_enum, default_value_t = Stem::Porter)]
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
    /// `memory`: stage-1 in-memory segment. `disk`: stage-2 durable index
    /// (WAL + mmap segments + merges).
    #[arg(long, value_enum, default_value_t = Engine::Memory)]
    engine: Engine,
    /// Parent directory for disk indexes (one subdirectory per dataset/tag).
    #[arg(long, default_value = "data/indexes")]
    index_dir: PathBuf,
    /// Disk engine: docs per flushed segment.
    #[arg(long, default_value_t = 10_000)]
    flush_docs: usize,
    /// Disk engine: merge factor (0 = no merges).
    #[arg(long, default_value_t = 4)]
    merge_factor: usize,
    /// Disk engine: docs per `add_documents` call (= per WAL fsync).
    #[arg(long, default_value_t = 1000)]
    batch: usize,
    /// Disk engine: merge down to one segment after ingest.
    #[arg(long)]
    force_merge: bool,
    /// Disk engine: `taat` (exhaustive term-at-a-time, stage 2), `wand`
    /// (block-max WAND, stage 3) or `maxscore` (MaxScore, stage 3).
    #[arg(long, value_enum, default_value_t = Strat::Taat)]
    strategy: Strat,
    /// Disk engine: attach a synthetic `bucket` field (FNV-1a(id) % 100).
    #[arg(long)]
    bucket_field: bool,
    /// Disk engine: filter in query syntax, e.g. "bucket:[0 TO 9]".
    #[arg(long)]
    filter: Option<String>,
}

#[derive(Clone, Copy, PartialEq, ValueEnum, Serialize)]
enum Strat {
    Taat,
    Wand,
    Maxscore,
}

/// Average WAND work per query in one repeat.
#[derive(Serialize, Clone)]
struct WandWork {
    /// Docs fully scored per query at k = --hits.
    docs_scored_per_query_k: f64,
    /// ... and at k = 10.
    docs_scored_per_query_k10: f64,
    /// Sum of query-term document frequencies per query (= docs an
    /// exhaustive scorer touches, counting a doc once per term).
    postings_per_query: f64,
}

#[derive(Clone, Copy, PartialEq, ValueEnum, Serialize)]
enum Engine {
    Memory,
    Disk,
}

#[derive(Clone, Copy, ValueEnum, Serialize)]
enum DocLen {
    Exact,
    Lucene,
}
#[derive(Clone, Copy, ValueEnum, Serialize)]
enum Stem {
    Porter,
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
                Stem::Porter => StemmerKind::Porter,
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
                Stem::Porter => "porter",
                Stem::Porter2 => "porter2",
                Stem::None => "nostem",
            };
            let stop = match self.stopwords {
                Stop::Lucene => "lucenestop",
                Stop::None => "nostop",
            };
            let engine = match self.engine {
                Engine::Memory => "strata".to_string(),
                Engine::Disk => format!(
                    "strata-disk.f{}.m{}{}{}{}{}",
                    self.flush_docs,
                    self.merge_factor,
                    if self.force_merge { ".fm" } else { "" },
                    match self.strategy {
                        Strat::Taat => "",
                        Strat::Wand => ".wand",
                        Strat::Maxscore => ".maxscore",
                    },
                    if self.bucket_field { ".bucket" } else { "" },
                    match &self.filter {
                        Some(f) => format!(
                            ".filter-{}",
                            f.replace(|c: char| !c.is_ascii_alphanumeric(), "_")
                        ),
                        None => String::new(),
                    }
                ),
            };
            format!("{engine}.k1_{}.b_{}.{dl}.{stem}.{stop}", self.k1, self.b)
        })
    }
}

/// Everything recorded for one run, serialized to JSON.
#[derive(Serialize)]
struct Report {
    engine: Engine,
    disk: Option<DiskStats>,
    /// Per repeat (WAND only).
    wand_work: Vec<WandWork>,
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

/// Disk-engine facts, from the first repeat.
#[derive(Serialize, Clone)]
struct DiskStats {
    flush_docs: usize,
    merge_factor: usize,
    batch: usize,
    force_merge: bool,
    segments: usize,
    /// Bytes of all segment files (WAL and tombstones excluded).
    segment_bytes: u64,
    /// Bytes of the whole index directory after the final commit.
    dir_bytes: u64,
    fsyncs_for_ingest: usize,
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
    /// 1-minute load average before this repeat (`sysctl vm.loadavg`), to
    /// flag noisy measurements.
    load_avg_1m: f64,
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

/// Build a durable on-disk index from scratch in `dir`. The timed region
/// covers WAL appends (one fsync per batch), flushes, background merges and
/// the final commit / merges.
fn build_disk_index(
    ds: &Dataset,
    config: &AnalyzerConfig,
    args: &RunArgs,
    dir: &Path,
) -> Result<(Arc<Index>, Duration, DiskStats)> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    let start = Instant::now();
    let cfg = IndexConfig {
        analyzer: config.clone(),
        flush_docs: args.flush_docs,
        merge_factor: args.merge_factor,
        background_merges: true,
    };
    let (index, _) = Index::open(dir, cfg)?;
    let mut fsyncs = 0;
    for chunk in ds.corpus.chunks(args.batch.max(1)) {
        let docs: Vec<Document> = chunk
            .iter()
            .map(|d| Document {
                id: d.id.clone(),
                text: d.flat_text(),
                fields: if args.bucket_field {
                    bucket_fields(&d.id)
                } else {
                    Fields::new()
                },
            })
            .collect();
        index.add_docs(&docs)?;
        fsyncs += 1;
    }
    index.commit()?;
    index.merge_pending()?;
    if args.force_merge {
        index.force_merge()?;
    }
    let elapsed = start.elapsed();
    let snap = index.snapshot();
    let mut segment_bytes = 0;
    for s in snap.segments() {
        segment_bytes += s.segment.size_on_disk()?;
    }
    let stats = DiskStats {
        flush_docs: args.flush_docs,
        merge_factor: args.merge_factor,
        batch: args.batch,
        force_merge: args.force_merge,
        segments: snap.segments().len(),
        segment_bytes,
        dir_bytes: dir_size(dir)?,
        fsyncs_for_ingest: fsyncs,
    };
    Ok((index, elapsed, stats))
}

fn dir_size(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let md = e.metadata()?;
        total += if md.is_dir() {
            dir_size(&e.path())?
        } else {
            md.len()
        };
    }
    Ok(total)
}

/// Synthetic metadata for filter benchmarks: `bucket` = FNV-1a(id) mod 100,
/// so `bucket:[0 TO s-1]` keeps a pseudo-random s% of the corpus.
fn bucket_fields(id: &str) -> Fields {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let mut f = Fields::new();
    f.insert("bucket".into(), FieldValue::Int((h % 100) as i64));
    f
}

/// 1-minute load average via `sysctl -n vm.loadavg` (macOS), or NaN.
fn load_avg_1m() -> f64 {
    std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| {
            s.trim_matches(|c| c == '{' || c == '}' || c == ' ' || c == '\n')
                .split_whitespace()
                .next()
                .map(str::to_string)
        })
        .and_then(|s| s.parse().ok())
        .unwrap_or(f64::NAN)
}

/// Run every query once with `search`; returns the run and per-query engine
/// latencies (query analysis + search). `resolve` (turning a hit into an
/// external id + score) runs outside the timed region.
fn run_queries<H>(
    ds: &Dataset,
    analyzer: &Analyzer,
    hits: usize,
    remove_self_hits: bool,
    search: impl Fn(&Query, usize) -> Vec<H>,
    resolve: impl Fn(&H) -> (String, f64),
) -> (Run, Vec<Duration>) {
    let mut run: Run = HashMap::with_capacity(ds.queries.len());
    let mut latencies = Vec::with_capacity(ds.queries.len());
    // Ask for one extra hit so dropping a self-hit still leaves `hits`.
    let k = hits + usize::from(remove_self_hits);
    for (qid, text) in &ds.queries {
        let start = Instant::now();
        let query = Query::bag_of_words(analyzer, text);
        let top = search(&query, k);
        latencies.push(start.elapsed());

        let results: Vec<(String, f64)> = top
            .iter()
            .map(&resolve)
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
    let mut disk_stats: Option<DiskStats> = None;
    let mut work: Vec<WandWork> = Vec::new();
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

        let load_avg = load_avg_1m();
        let (built, index_time) = match args.engine {
            Engine::Memory => {
                let (segment, t) = build_index(&ds, &config);
                assert_eq!(tokens as u64, segment.total_tokens());
                (Built::Memory(segment), t)
            }
            Engine::Disk => {
                let dir = args.index_dir.join(format!("{}.{tag}", ds.name));
                let (index, t, stats) = build_disk_index(&ds, &config, &args, &dir)?;
                assert_eq!(tokens as u64, index.snapshot().stats().total_tokens);
                disk_stats.get_or_insert(stats);
                (Built::Disk(index.snapshot()), t)
            }
        };
        let (run, latencies, latencies_k10) = match &built {
            Built::Memory(segment) => {
                let searcher = Searcher::new(segment, params);
                let search = |q: &Query, k: usize| searcher.search(q, k);
                let resolve =
                    |h: &Hit| (segment.external_id(h.doc).to_string(), f64::from(h.score));
                let (run, lat) =
                    run_queries(&ds, &analyzer, args.hits, remove_self_hits, search, resolve);
                let (_, lat10) = run_queries(&ds, &analyzer, 10, remove_self_hits, search, resolve);
                (run, lat, lat10)
            }
            Built::Disk(snap) => {
                let filters = match &args.filter {
                    Some(f) => strata_query::parse(f, &analyzer)?.filters,
                    None => Vec::new(),
                };
                let wand_work = std::cell::RefCell::new((0u64, 0u64));
                let search = |q: &Query, k: usize| -> Vec<SnapshotHit> {
                    match args.strategy {
                        Strat::Taat if filters.is_empty() => snap.search(q, params, k),
                        Strat::Taat => {
                            let bq = BoolQuery {
                                should: q
                                    .terms()
                                    .iter()
                                    .map(|t| Clause::Term {
                                        term: t.term.clone(),
                                        count: t.count,
                                    })
                                    .collect(),
                                filters: filters.clone(),
                                ..Default::default()
                            };
                            strata_query::search(
                                snap,
                                &bq,
                                params,
                                k,
                                strata_query::Strategy::Exhaustive,
                            )
                            .expect("query")
                        }
                        Strat::Wand | Strat::Maxscore => {
                            let terms: Vec<(String, u32)> = q
                                .terms()
                                .iter()
                                .map(|t| (t.term.clone(), t.count))
                                .collect();
                            let (hits, st) = if args.strategy == Strat::Wand {
                                strata_query::wand_search(snap, &terms, &filters, params, k)
                            } else {
                                strata_query::maxscore_search(snap, &terms, &filters, params, k)
                            };
                            let mut w = wand_work.borrow_mut();
                            w.0 += st.docs_scored;
                            w.1 += st.postings_total;
                            hits
                        }
                    }
                };
                let resolve =
                    |h: &SnapshotHit| (snap.external_id(h).to_string(), f64::from(h.score));
                let (run, lat) =
                    run_queries(&ds, &analyzer, args.hits, remove_self_hits, search, resolve);
                let (scored_k, postings_k) = *wand_work.borrow();
                let (_, lat10) = run_queries(&ds, &analyzer, 10, remove_self_hits, search, resolve);
                let (scored_all, _) = *wand_work.borrow();
                if args.strategy != Strat::Taat {
                    work.push(WandWork {
                        docs_scored_per_query_k: scored_k as f64 / ds.queries.len() as f64,
                        docs_scored_per_query_k10: (scored_all - scored_k) as f64
                            / ds.queries.len() as f64,
                        postings_per_query: postings_k as f64 / ds.queries.len() as f64,
                    });
                }
                (run, lat, lat10)
            }
        };
        let latency = LatencyStats::from_samples(&latencies);
        let latency_k10 = LatencyStats::from_samples(&latencies_k10);
        let index_secs = index_time.as_secs_f64();
        let rep = Repeat {
            analyze_secs,
            load_avg_1m: load_avg,
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
                let stats = match &built {
                    Built::Memory(segment) => IndexStats {
                        docs: segment.num_docs(),
                        terms: segment.num_terms(),
                        postings: segment.num_postings(),
                        total_tokens: segment.total_tokens(),
                        avg_doc_len: segment.stats().avg_doc_len(),
                        heap_size_bytes_estimate: segment.heap_size_bytes(),
                    },
                    Built::Disk(snap) => IndexStats {
                        docs: snap.stats().num_docs as u32,
                        // Sum over segments (a term in two segments counts twice).
                        terms: snap.segments().iter().map(|s| s.segment.num_terms()).sum(),
                        postings: snap
                            .segments()
                            .iter()
                            .map(|s| s.segment.meta().num_postings as usize)
                            .sum(),
                        total_tokens: snap.stats().total_tokens,
                        avg_doc_len: snap.stats().avg_doc_len(),
                        heap_size_bytes_estimate: 0,
                    },
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
        engine: args.engine,
        disk: disk_stats,
        wand_work: work,
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
