//! `strata-load`: HTTP load generator for strata nodes (and coordinators).
//!
//! ```text
//! strata-load search --url http://127.0.0.1:7700 --queries data/fiqa/queries.jsonl \
//!     --loop closed --concurrency 16 --duration 20
//! strata-load search ... --loop open --rate 2000 --duration 20
//! strata-load ingest --url http://127.0.0.1:7700 --dataset fiqa --batch 500 --concurrency 4
//! ```
//!
//! * **Closed loop**: N workers, each sends its next request as soon as the
//!   previous one returns. Measures capacity, but the offered load drops as
//!   latency rises.
//! * **Open loop**: requests are *scheduled* at a fixed rate whether or not
//!   earlier ones finished, and latency is measured from the scheduled send
//!   time. This avoids "coordinated omission" (a stalled server would
//!   otherwise also stall the load generator and hide the stall from the
//!   latency numbers).
//!
//! The first `--warmup` seconds are excluded from the statistics.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::json;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Serialize, Debug)]
enum Loop {
    Closed,
    Open,
}

#[derive(Subcommand)]
enum Cmd {
    Search {
        #[arg(long)]
        url: String,
        /// BEIR queries.jsonl (uses the "text" field).
        #[arg(long)]
        queries: PathBuf,
        #[arg(long, value_enum, default_value_t = Loop::Closed)]
        r#loop: Loop,
        #[arg(long, default_value_t = 8)]
        concurrency: usize,
        /// Open loop: requests per second.
        #[arg(long, default_value_t = 1000.0)]
        rate: f64,
        #[arg(long, default_value_t = 20.0)]
        duration: f64,
        #[arg(long, default_value_t = 2.0)]
        warmup: f64,
        #[arg(long, default_value_t = 10)]
        k: usize,
        /// "lexical", "dense" or "hybrid" (dense/hybrid need --embeddings).
        #[arg(long, default_value = "lexical")]
        mode: String,
        /// data/embeddings/<dataset> directory, for query vectors.
        #[arg(long)]
        embeddings: Option<PathBuf>,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run every BEIR test query once (plain bag-of-words, top `k`, self-hits
    /// dropped) and write a TREC run file, to compare against single-node runs.
    RunQueries {
        #[arg(long)]
        url: String,
        #[arg(long)]
        dataset: String,
        #[arg(long, default_value = "data")]
        data_dir: PathBuf,
        #[arg(long, default_value_t = 1000)]
        k: usize,
        #[arg(long)]
        out: PathBuf,
    },
    Ingest {
        #[arg(long)]
        url: String,
        #[arg(long)]
        dataset: String,
        #[arg(long, default_value = "data")]
        data_dir: PathBuf,
        #[arg(long, default_value_t = 500)]
        batch: usize,
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
        /// Also send the cached document embeddings.
        #[arg(long)]
        embeddings: bool,
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Serialize)]
struct LatencySummary {
    requests: usize,
    errors: usize,
    seconds: f64,
    throughput_rps: f64,
    p50_ms: f64,
    p90_ms: f64,
    p99_ms: f64,
    p999_ms: f64,
    max_ms: f64,
}

fn summarize(mut lat: Vec<Duration>, errors: usize, seconds: f64) -> LatencySummary {
    lat.sort();
    let p = |q: f64| -> f64 {
        if lat.is_empty() {
            return f64::NAN;
        }
        let rank = ((q / 100.0) * lat.len() as f64).ceil() as usize;
        lat[rank.clamp(1, lat.len()) - 1].as_secs_f64() * 1e3
    };
    LatencySummary {
        requests: lat.len(),
        errors,
        seconds,
        throughput_rps: lat.len() as f64 / seconds,
        p50_ms: p(50.0),
        p90_ms: p(90.0),
        p99_ms: p(99.0),
        p999_ms: p(99.9),
        max_ms: lat.last().map_or(f64::NAN, |d| d.as_secs_f64() * 1e3),
    }
}

fn load_queries(path: &PathBuf, embeddings: &Option<PathBuf>) -> Result<Vec<serde_json::Value>> {
    let mut vectors: std::collections::HashMap<String, Vec<f32>> = Default::default();
    if let Some(dir) = embeddings {
        let ids: Vec<String> = serde_json::from_slice(&std::fs::read(dir.join("query_ids.json"))?)?;
        let meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("meta.json"))?)?;
        let dim = meta["dim"].as_u64().context("dim")? as usize;
        let raw = std::fs::read(dir.join("queries.f32"))?;
        for (i, id) in ids.into_iter().enumerate() {
            let v = raw[i * dim * 4..(i + 1) * dim * 4]
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect();
            vectors.insert(id, v);
        }
    }
    let mut out = Vec::new();
    for line in std::fs::read_to_string(path)?.lines() {
        let v: serde_json::Value = serde_json::from_str(line)?;
        let id = v["_id"].as_str().unwrap_or_default().to_string();
        // Strip query-syntax characters so raw BEIR text is a plain bag of words.
        let text: String = v["text"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { ' ' })
            .collect();
        out.push(json!({ "query": text, "vector": vectors.get(&id) }));
    }
    Ok(out)
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Search {
            url,
            queries,
            r#loop,
            concurrency,
            rate,
            duration,
            warmup,
            k,
            mode,
            embeddings,
            out,
        } => {
            let qs = Arc::new(load_queries(&queries, &embeddings)?);
            let client = reqwest::Client::builder()
                .pool_max_idle_per_host(1024)
                .build()?;
            let endpoint = format!("{url}/v1/search");
            let body = move |i: usize| {
                let mut b = qs[i % qs.len()].clone();
                b["k"] = json!(k);
                b["mode"] = json!(mode.clone());
                b
            };
            let body = Arc::new(body);
            let start = Instant::now();
            let measure_from = start + Duration::from_secs_f64(warmup);
            let end = measure_from + Duration::from_secs_f64(duration);
            let mut send_lags: Vec<Duration> = Vec::new();
            let (lat, errors) = match r#loop {
                Loop::Closed => {
                    let next = Arc::new(AtomicUsize::new(0));
                    let mut tasks = Vec::new();
                    for _ in 0..concurrency {
                        let (client, endpoint, next, body) =
                            (client.clone(), endpoint.clone(), next.clone(), body.clone());
                        tasks.push(tokio::spawn(async move {
                            let mut lat = Vec::new();
                            let mut errors = 0;
                            while Instant::now() < end {
                                let i = next.fetch_add(1, Ordering::Relaxed);
                                let t = Instant::now();
                                let ok = matches!(client.post(&endpoint).json(&body(i)).send().await, Ok(r) if r.status().is_success());
                                if t >= measure_from {
                                    if ok {
                                        lat.push(t.elapsed());
                                    } else {
                                        errors += 1;
                                    }
                                }
                            }
                            (lat, errors)
                        }));
                    }
                    let mut all = Vec::new();
                    let mut errors = 0;
                    for t in tasks {
                        let (l, e) = t.await?;
                        all.extend(l);
                        errors += e;
                    }
                    (all, errors)
                }
                Loop::Open => {
                    // The schedule runs on its own OS thread so that spinning
                    // to the exact send instant never blocks the async worker
                    // threads that process responses. tokio timers have ~1 ms
                    // granularity, hence: sleep until shortly before the slot,
                    // then spin. Each request is spawned onto the runtime at
                    // its slot; latency is measured from the slot.
                    let interval = Duration::from_secs_f64(1.0 / rate);
                    let rt = tokio::runtime::Handle::current();
                    let scheduler = std::thread::spawn(move || {
                        let mut handles = Vec::new();
                        let mut i = 0usize;
                        loop {
                            let scheduled = start + interval * i as u32;
                            if scheduled >= end {
                                break;
                            }
                            let now = Instant::now();
                            if scheduled > now + Duration::from_micros(500) {
                                std::thread::sleep(scheduled - now - Duration::from_micros(500));
                            }
                            while Instant::now() < scheduled {
                                std::hint::spin_loop();
                            }
                            let (client, endpoint, body) =
                                (client.clone(), endpoint.clone(), body.clone());
                            handles.push(rt.spawn(async move {
                                let sent = Instant::now();
                                let ok = matches!(client.post(&endpoint).json(&body(i)).send().await, Ok(r) if r.status().is_success());
                                // Latency from the *scheduled* time (no coordinated
                                // omission); the send lag shows generator accuracy.
                                (scheduled, scheduled.elapsed(), sent - scheduled, ok)
                            }));
                            i += 1;
                        }
                        handles
                    });
                    let handles = tokio::task::spawn_blocking(move || {
                        scheduler.join().expect("scheduler thread")
                    })
                    .await?;
                    let mut all = Vec::new();
                    let mut errors = 0;
                    for h in handles {
                        let (scheduled, d, lag, ok) = h.await?;
                        if scheduled >= measure_from {
                            if ok {
                                all.push(d);
                                send_lags.push(lag);
                            } else {
                                errors += 1;
                            }
                        }
                    }
                    (all, errors)
                }
            };
            let s = summarize(lat, errors, duration);
            // Open loop: how late the generator itself sent requests.
            let send_lag = (!send_lags.is_empty()).then(|| summarize(send_lags, 0, duration));
            let report = json!({ "loop": r#loop, "concurrency": concurrency, "rate": rate, "summary": s, "send_lag": send_lag });
            println!("{}", serde_json::to_string(&report)?);
            if let Some(o) = out {
                std::fs::write(o, serde_json::to_string_pretty(&report)?)?;
            }
        }
        Cmd::RunQueries {
            url,
            dataset,
            data_dir,
            k,
            out,
        } => {
            let ds = strata_eval::beir::Dataset::load(&data_dir, &dataset, "test")?;
            let client = reqwest::Client::new();
            let mut run = strata_eval::metrics::Run::new();
            let mut partial = 0usize;
            for (qid, text) in &ds.queries {
                let body = json!({ "query": text, "plain": true, "k": k + 1 });
                let v: serde_json::Value = client
                    .post(format!("{url}/v1/search"))
                    .json(&body)
                    .send()
                    .await?
                    .json()
                    .await?;
                if v["partial"].as_bool().unwrap_or(false) {
                    partial += 1;
                }
                let hits: Vec<(String, f64)> = v["hits"]
                    .as_array()
                    .context("hits")?
                    .iter()
                    // Scores travel as the shortest decimal that round-trips the
                    // f32; going back through f32 recovers the exact value, so
                    // the run file is byte-comparable with single-node runs.
                    .map(|h| {
                        (
                            h["id"].as_str().unwrap_or_default().to_string(),
                            f64::from(h["score"].as_f64().unwrap_or(0.0) as f32),
                        )
                    })
                    .filter(|(d, _)| d != qid)
                    .take(k)
                    .collect();
                run.insert(qid.clone(), hits);
            }
            strata_eval::harness::write_trec_run(&out, &run, "strata-cluster")?;
            let e = strata_eval::metrics::evaluate(&ds.qrels, &run);
            println!(
                "{dataset}\tqueries={}\tpartial={partial}\tnDCG@10={:.4}\tR@100={:.4}",
                ds.queries.len(),
                e.mean.ndcg_at_10,
                e.mean.recall_at_100
            );
        }
        Cmd::Ingest {
            url,
            dataset,
            data_dir,
            batch,
            concurrency,
            embeddings,
            out,
        } => {
            let corpus = std::fs::read_to_string(data_dir.join(&dataset).join("corpus.jsonl"))?;
            let vecs: Option<(usize, Vec<u8>)> = if embeddings {
                let dir = data_dir.join("embeddings").join(&dataset);
                let meta: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(dir.join("meta.json"))?)?;
                Some((
                    meta["dim"].as_u64().unwrap() as usize,
                    std::fs::read(dir.join("corpus.f32"))?,
                ))
            } else {
                None
            };
            let docs: Vec<serde_json::Value> = corpus
                .lines()
                .enumerate()
                .map(|(i, l)| {
                    let d: serde_json::Value = serde_json::from_str(l).unwrap();
                    let text = format!(
                        "{} {}",
                        d["title"].as_str().unwrap_or(""),
                        d["text"].as_str().unwrap_or("")
                    );
                    let mut doc = json!({ "id": d["_id"], "text": text.trim() });
                    if let Some((dim, raw)) = &vecs {
                        let v: Vec<f32> = raw[i * dim * 4..(i + 1) * dim * 4]
                            .chunks_exact(4)
                            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                            .collect();
                        doc["vector"] = json!(v);
                    }
                    doc
                })
                .collect();
            let batches: Vec<serde_json::Value> =
                docs.chunks(batch).map(|c| json!({ "docs": c })).collect();
            let batches = Arc::new(batches);
            let next = Arc::new(AtomicUsize::new(0));
            let client = reqwest::Client::new();
            let start = Instant::now();
            let mut tasks = Vec::new();
            for _ in 0..concurrency {
                let (client, url, next, batches) =
                    (client.clone(), url.clone(), next.clone(), batches.clone());
                tasks.push(tokio::spawn(async move {
                    let mut lat = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= batches.len() {
                            break;
                        }
                        let t = Instant::now();
                        let r = client
                            .post(format!("{url}/v1/documents"))
                            .json(&batches[i])
                            .send()
                            .await?;
                        anyhow::ensure!(
                            r.status().is_success(),
                            "ingest failed: {}",
                            r.text().await?
                        );
                        lat.push(t.elapsed());
                    }
                    Ok::<_, anyhow::Error>(lat)
                }));
            }
            let mut lat = Vec::new();
            for t in tasks {
                lat.extend(t.await??);
            }
            let ingest_secs = start.elapsed().as_secs_f64();
            let r = client.post(format!("{url}/v1/commit")).send().await?;
            anyhow::ensure!(r.status().is_success());
            let total_secs = start.elapsed().as_secs_f64();
            let report = json!({
                "dataset": dataset, "docs": docs.len(), "batch": batch, "concurrency": concurrency,
                "with_vectors": embeddings, "ingest_secs": ingest_secs, "with_commit_secs": total_secs,
                "docs_per_sec": docs.len() as f64 / total_secs,
                "batch_latency": summarize(lat, 0, ingest_secs),
            });
            println!("{}", serde_json::to_string(&report)?);
            if let Some(o) = out {
                std::fs::write(o, serde_json::to_string_pretty(&report)?)?;
            }
        }
    }
    Ok(())
}
