//! `strata-bench tantivy`: index a BEIR dataset with Tantivy, run all test
//! queries as BM25 bag-of-words queries, and report quality + timings with the
//! same metrics code (`strata-eval`) and the same output format as
//! `strata-eval run`, so results can be compared side by side.
//!
//! Fairness settings:
//! * one indexing thread, index kept in RAM, merged into a single segment
//!   (strata also builds exactly one in-memory segment)
//! * single-threaded search (Tantivy's default executor)
//! * `--tokenizer strata` feeds Tantivy strata's analyzer, so both index the
//!   identical terms; `--tokenizer native` uses Tantivy's own pipeline
//!   (SimpleTokenizer + RemoveLong + LowerCaser + Lucene stopwords + Porter2)
//! * queries: one `TermQuery` per distinct term, boosted by its count in the
//!   query, OR-ed together (same semantics as strata / Anserini)
//! * Tantivy's BM25 is hard-coded to k1 = 1.2, b = 0.75 and quantizes document
//!   lengths into one byte, so compare it with strata's `--k1 1.2 --b 0.75`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, ValueEnum};
use serde::Serialize;
use strata_bench::StrataTokenizer;
use strata_core::analyzer::LUCENE_ENGLISH_STOPWORDS;
use strata_core::{Analyzer, AnalyzerConfig};
use strata_eval::beir::Dataset;
use strata_eval::harness::{mean_std, write_trec_run, LatencyStats};
use strata_eval::metrics::{evaluate, Evaluation, Run};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, BoostQuery, Occur, Query, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, FAST, STORED,
};
use tantivy::tokenizer::{
    Language, LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer, StopWordFilter, TextAnalyzer,
};
use tantivy::{doc, Index, IndexWriter, Term};

#[derive(Parser)]
#[command(name = "strata-bench", about = "Tantivy baseline on BEIR")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(clap::Subcommand)]
enum Cmd {
    /// Run the Tantivy BM25 baseline on one dataset.
    Tantivy(Args),
}

#[derive(clap::Args)]
struct Args {
    #[arg(long)]
    dataset: String,
    #[arg(long, default_value = "data")]
    data_dir: PathBuf,
    #[arg(long, value_enum, default_value_t = Tok::Strata)]
    tokenizer: Tok,
    #[arg(long, default_value_t = 1000)]
    hits: usize,
    #[arg(long, default_value_t = 3)]
    repeats: usize,
    #[arg(long, default_value = "results")]
    results_dir: PathBuf,
}

#[derive(Clone, Copy, PartialEq, ValueEnum, Serialize)]
enum Tok {
    Strata,
    Native,
}

#[derive(Serialize)]
struct Repeat {
    /// add_document for all docs + commit
    index_secs: f64,
    /// merging the committed segments into one (0 if already one)
    merge_secs: f64,
    docs_per_sec: f64,
    segments_before_merge: usize,
    index_bytes: u64,
    /// Latency of the evaluated run (top `--hits`, default 1000).
    latency: LatencyStats,
    /// Latency of a second pass with k = 10.
    latency_k10: LatencyStats,
}

#[derive(Serialize)]
struct Report {
    engine: String,
    dataset: String,
    tag: String,
    tokenizer: Tok,
    k1: f32,
    b: f32,
    hits: usize,
    remove_self_hits: bool,
    metrics: Evaluation,
    repeats: Vec<Repeat>,
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

const TOKENIZER_NAME: &str = "bench";
/// Per-thread indexing memory budget; large enough that small/medium BEIR
/// corpora usually flush as a single segment.
const WRITER_HEAP_BYTES: usize = 1_000_000_000;

struct Built {
    index: Index,
    body: Field,
    index_time: Duration,
    merge_time: Duration,
    segments_before_merge: usize,
}

fn text_analyzer(tok: Tok) -> TextAnalyzer {
    match tok {
        Tok::Strata => TextAnalyzer::from(StrataTokenizer::new(Analyzer::new(
            AnalyzerConfig::english(),
        ))),
        Tok::Native => TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(255))
            .filter(LowerCaser)
            .filter(StopWordFilter::remove(
                LUCENE_ENGLISH_STOPWORDS.iter().map(|s| s.to_string()),
            ))
            .filter(Stemmer::new(Language::English))
            .build(),
    }
}

fn build(ds: &Dataset, tok: Tok) -> Result<Built> {
    let mut sb = Schema::builder();
    // Corpus position as a fast field (columnar) so mapping hits back to ids
    // is an array lookup rather than a stored-document fetch.
    let ord = sb.add_u64_field("ord", FAST | STORED);
    let body = sb.add_text_field(
        "body",
        TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(TOKENIZER_NAME)
                .set_index_option(IndexRecordOption::WithFreqs),
        ),
    );
    let index = Index::create_in_ram(sb.build());
    index
        .tokenizers()
        .register(TOKENIZER_NAME, text_analyzer(tok));

    let start = Instant::now();
    let mut writer: IndexWriter = index.writer_with_num_threads(1, WRITER_HEAP_BYTES)?;
    for (i, d) in ds.corpus.iter().enumerate() {
        writer.add_document(doc!(ord => i as u64, body => d.flat_text()))?;
    }
    writer.commit()?;
    let index_time = start.elapsed();

    let merge_start = Instant::now();
    let ids = index.searchable_segment_ids()?;
    let segments_before_merge = ids.len();
    if ids.len() > 1 {
        writer.merge(&ids).wait()?;
    }
    writer.wait_merging_threads()?;
    let merge_time = if segments_before_merge > 1 {
        merge_start.elapsed()
    } else {
        Duration::ZERO
    };
    Ok(Built {
        index,
        body,
        index_time,
        merge_time,
        segments_before_merge,
    })
}

/// Analyze a query with the field's analyzer and build OR-of-boosted-terms.
fn build_query(analyzer: &mut TextAnalyzer, body: Field, text: &str) -> Box<dyn Query> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut stream = analyzer.token_stream(text);
    while stream.advance() {
        *counts.entry(stream.token().text.clone()).or_default() += 1;
    }
    let mut terms: Vec<(String, u32)> = counts.into_iter().collect();
    terms.sort();
    let clauses: Vec<(Occur, Box<dyn Query>)> = terms
        .into_iter()
        .map(|(t, n)| {
            let tq = TermQuery::new(
                Term::from_field_text(body, &t),
                IndexRecordOption::WithFreqs,
            );
            // Only wrap repeated terms in a BoostQuery; plain TermQuery
            // clauses keep Tantivy's fastest code path.
            let q: Box<dyn Query> = if n > 1 {
                Box::new(BoostQuery::new(Box::new(tq), n as f32))
            } else {
                Box::new(tq)
            };
            (Occur::Should, q)
        })
        .collect();
    Box::new(BooleanQuery::new(clauses))
}

fn run_once(ds: &Dataset, built: &Built, tok: Tok, hits: usize) -> Result<(Run, Vec<Duration>)> {
    let reader = built.index.reader()?;
    let searcher = reader.searcher();
    assert_eq!(searcher.segment_readers().len(), 1, "expected one segment");
    let ord_col = searcher.segment_reader(0).fast_fields().u64("ord")?;
    let mut analyzer = text_analyzer(tok);
    let collector = TopDocs::with_limit(hits + 1).order_by_score();

    let mut run: Run = HashMap::new();
    let mut latencies = Vec::with_capacity(ds.queries.len());
    for (qid, text) in &ds.queries {
        let start = Instant::now();
        let query = build_query(&mut analyzer, built.body, text);
        let top = searcher.search(&*query, &collector)?;
        latencies.push(start.elapsed());

        let results: Vec<(String, f64)> = top
            .into_iter()
            .map(|(score, addr)| {
                let i = ord_col.first(addr.doc_id).expect("ord fast field") as usize;
                (ds.corpus[i].id.clone(), f64::from(score))
            })
            .filter(|(doc, _)| doc != qid) // Anserini's -removeQuery
            .take(hits)
            .collect();
        run.insert(qid.clone(), results);
    }
    Ok((run, latencies))
}

fn main() -> Result<()> {
    let Cmd::Tantivy(args) = Cli::parse().command;
    let ds = Dataset::load(&args.data_dir, &args.dataset, "test")?;
    let tok_name = match args.tokenizer {
        Tok::Strata => "strata-analyzer",
        Tok::Native => "native-analyzer",
    };
    // "tantivy v0.27.0, index_format v7, ..." -> "tantivy-0.27.0"
    let version = tantivy::version_string()
        .split(',')
        .next()
        .unwrap_or("tantivy")
        .replace(" v", "-");
    let tag = format!("{version}.{tok_name}");

    let mut repeats = Vec::new();
    let mut first_run: Option<Run> = None;
    for r in 0..args.repeats.max(1) {
        let built = build(&ds, args.tokenizer)?;
        let searcher = built.index.reader()?.searcher();
        let index_bytes = searcher.space_usage()?.total().get_bytes();
        let (run, latencies) = run_once(&ds, &built, args.tokenizer, args.hits)?;
        let (_, latencies_k10) = run_once(&ds, &built, args.tokenizer, 10)?;
        let rep = Repeat {
            index_secs: built.index_time.as_secs_f64(),
            merge_secs: built.merge_time.as_secs_f64(),
            docs_per_sec: ds.corpus.len() as f64
                / (built.index_time + built.merge_time).as_secs_f64(),
            segments_before_merge: built.segments_before_merge,
            index_bytes,
            latency: LatencyStats::from_samples(&latencies),
            latency_k10: LatencyStats::from_samples(&latencies_k10),
        };
        eprintln!(
            "[{}] tantivy repeat {}: index {:.3}s + merge {:.3}s ({} segs, {:.0} docs/s), {} bytes, p50 {:.0}us p99 {:.0}us",
            ds.name, r + 1, rep.index_secs, rep.merge_secs, rep.segments_before_merge,
            rep.docs_per_sec, rep.index_bytes, rep.latency.p50_us, rep.latency.p99_us
        );
        eprintln!(
            "[{}]   k=10 p50 {:.0}us p99 {:.0}us",
            ds.name, rep.latency_k10.p50_us, rep.latency_k10.p99_us
        );
        repeats.push(rep);
        if first_run.is_none() {
            first_run = Some(run);
        }
    }
    let run = first_run.expect("one repeat");
    let run_path = args
        .results_dir
        .join("runs")
        .join(&ds.name)
        .join(format!("{tag}.trec"));
    write_trec_run(&run_path, &run, "tantivy")?;
    let metrics = evaluate(&ds.qrels, &run);

    let col = |f: fn(&Repeat) -> f64| repeats.iter().map(f).collect::<Vec<f64>>();
    let (dps_m, dps_s) = mean_std(&col(|r| r.docs_per_sec));
    let (p50_m, p50_s) = mean_std(&col(|r| r.latency.p50_us));
    let (p99_m, p99_s) = mean_std(&col(|r| r.latency.p99_us));
    let (qps_m, _) = mean_std(&col(|r| r.latency.qps));
    let (p50_k10, _) = mean_std(&col(|r| r.latency_k10.p50_us));
    let (p99_k10, _) = mean_std(&col(|r| r.latency_k10.p99_us));
    let m = metrics.mean;
    println!(
        "{}\t{}\tnDCG@10={:.4}\tR@100={:.4}\tR@1000={:.4}\tMRR@10={:.4}\tdocs/s={:.0}±{:.0}\tp50={:.0}us\tp99={:.0}us\tk10_p50={:.0}us\tk10_p99={:.0}us",
        ds.name, tag, m.ndcg_at_10, m.recall_at_100, m.recall_at_1000, m.mrr_at_10,
        dps_m, dps_s, p50_m, p99_m, p50_k10, p99_k10
    );
    let report = Report {
        engine: tantivy::version_string().to_string(),
        dataset: ds.name.clone(),
        tag: tag.clone(),
        tokenizer: args.tokenizer,
        k1: 1.2,
        b: 0.75,
        hits: args.hits,
        remove_self_hits: true,
        metrics,
        repeats,
        docs_per_sec_mean: dps_m,
        docs_per_sec_std: dps_s,
        p50_us_mean: p50_m,
        p50_us_std: p50_s,
        p99_us_mean: p99_m,
        p99_us_std: p99_s,
        qps_mean: qps_m,
        p50_us_k10_mean: p50_k10,
        p99_us_k10_mean: p99_k10,
    };
    let json_path = args.results_dir.join(&ds.name).join(format!("{tag}.json"));
    std::fs::create_dir_all(json_path.parent().unwrap())?;
    std::fs::write(&json_path, serde_json::to_string_pretty(&report)?)?;
    Ok(())
}
