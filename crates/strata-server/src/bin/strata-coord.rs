//! `strata-coord`: a coordinator for a hash-sharded strata cluster.
//!
//! ```text
//! strata-coord --listen 127.0.0.1:7800 \
//!     --shards "http://127.0.0.1:7701,http://127.0.0.1:7711;http://127.0.0.1:7702,http://127.0.0.1:7712" \
//!     --timeout-ms 1000 --hedge-ms 20
//! ```
//!
//! `--shards` lists shards separated by `;`, each with its replicas
//! separated by `,`. (In this stage replicas are kept in sync by the
//! coordinator writing to all of them; stage 8 replaces that with Raft.)
//!
//! * **Routing**: a document lives on shard `FNV-1a(id) % shards`.
//! * **Search = scatter-gather in two phases.** Phase 1 asks every shard
//!   for its local statistics of the query's terms (`/v1/stats`) and sums
//!   them. Phase 2 sends the query plus these *global* statistics to every
//!   shard and merges the per-shard top-k by score. With global IDF and
//!   avgdl, each document gets exactly the score it would get on a single
//!   node, so the merged top-k equals the single-node top-k (verified on
//!   BEIR in BENCHMARK.md stage 7).
//! * **Timeouts / partial results**: each shard call has a deadline. If a
//!   shard fails or times out the coordinator still answers, with
//!   `"partial": true` and the failed shards listed.
//! * **Hedged requests** (Dean & Barroso, "The Tail at Scale"): send to one
//!   replica; if it has not answered after `--hedge-ms`, also send to the
//!   next replica and take whichever answers first. This trades a little
//!   extra load for a much shorter tail when one replica is slow.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use serde_json::{json, Value};
use strata_query::ScoringStats;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:7800")]
    listen: SocketAddr,
    /// Shards separated by `;`, replicas within a shard by `,`.
    #[arg(long)]
    shards: String,
    /// Per-shard-call deadline.
    #[arg(long, default_value_t = 1000)]
    timeout_ms: u64,
    /// Send a backup request to another replica after this many ms
    /// (0 = hedging off).
    #[arg(long, default_value_t = 0)]
    hedge_ms: u64,
}

struct Coord {
    shards: Vec<Vec<String>>,
    client: reqwest::Client,
    timeout: Duration,
    hedge: Option<Duration>,
    /// Round-robin start replica per call, to spread load.
    rr: AtomicU64,
    requests_sent: AtomicU64,
    hedges_fired: AtomicU64,
    hedges_won: AtomicU64,
}

fn shard_of(id: &str, n: usize) -> usize {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h % n as u64) as usize
}

impl Coord {
    async fn post_once(&self, url: &str, path: &str, body: &Value) -> Result<Value> {
        self.requests_sent.fetch_add(1, Ordering::Relaxed);
        let r = self
            .client
            .post(format!("{url}{path}"))
            .json(body)
            .send()
            .await?;
        let status = r.status();
        let v: Value = r.json().await?;
        anyhow::ensure!(status.is_success(), "{url}{path}: {status} {v}");
        Ok(v)
    }

    /// One logical call to a shard: first replica, plus a hedged backup to
    /// the next replica if the first is slow; bounded by the deadline.
    async fn call_shard(&self, shard: usize, path: &str, body: &Value) -> Result<Value> {
        let replicas = &self.shards[shard];
        let start = self.rr.fetch_add(1, Ordering::Relaxed) as usize % replicas.len();
        let primary = &replicas[start];
        let work = async {
            match (self.hedge, replicas.len() > 1) {
                (Some(delay), true) => {
                    let backup = &replicas[(start + 1) % replicas.len()];
                    let first = self.post_once(primary, path, body);
                    tokio::pin!(first);
                    tokio::select! {
                        r = &mut first => r,
                        _ = tokio::time::sleep(delay) => {
                            self.hedges_fired.fetch_add(1, Ordering::Relaxed);
                            let second = self.post_once(backup, path, body);
                            tokio::pin!(second);
                            // Whichever finishes first wins; if one fails,
                            // wait for the other.
                            tokio::select! {
                                r = &mut first => match r { Ok(v) => Ok(v), Err(_) => second.await },
                                r = &mut second => match r {
                                    Ok(v) => { self.hedges_won.fetch_add(1, Ordering::Relaxed); Ok(v) }
                                    Err(_) => first.await,
                                },
                            }
                        }
                    }
                }
                _ => self.post_once(primary, path, body).await,
            }
        };
        tokio::time::timeout(self.timeout, work)
            .await
            .context("shard deadline exceeded")?
    }

    /// Call every shard in parallel; returns per-shard results.
    async fn scatter(&self, path: &str, body: &Value) -> Vec<Result<Value>> {
        // join_all polls all futures concurrently: the shard calls overlap.
        futures::future::join_all((0..self.shards.len()).map(|s| self.call_shard(s, path, body)))
            .await
    }
}

type St = Arc<Coord>;

async fn search(State(c): State<St>, Json(mut req): Json<Value>) -> Response {
    let start = Instant::now();
    let k = req["k"].as_u64().unwrap_or(10) as usize;
    let mode = req["mode"].as_str().unwrap_or("lexical").to_string();
    let mut failed: Vec<usize> = Vec::new();
    // Phase 1: global statistics (lexical scoring only).
    if mode != "dense" {
        let stats_req =
            json!({ "query": req["query"], "plain": req["plain"].as_bool().unwrap_or(false) });
        let parts = c.scatter("/v1/stats", &stats_req).await;
        let mut ok = Vec::new();
        for (i, p) in parts.into_iter().enumerate() {
            match p.and_then(|v| Ok(serde_json::from_value::<ScoringStats>(v)?)) {
                Ok(s) => ok.push(s),
                Err(_) => failed.push(i),
            }
        }
        // With a shard missing, statistics (and scores) are approximate;
        // the response is flagged partial.
        req["global_stats"] = serde_json::to_value(ScoringStats::merge(&ok)).unwrap();
    }
    // Phase 2: scatter the query, gather top-k per shard.
    let results = c.scatter("/v1/search", &req).await;
    let mut hits: Vec<(String, f32)> = Vec::new();
    for (i, r) in results.into_iter().enumerate() {
        match r {
            Ok(v) => {
                for h in v["hits"].as_array().into_iter().flatten() {
                    hits.push((
                        h["id"].as_str().unwrap_or_default().to_string(),
                        h["score"].as_f64().unwrap_or(0.0) as f32,
                    ));
                }
            }
            Err(_) => {
                if !failed.contains(&i) {
                    failed.push(i);
                }
            }
        }
    }
    // Merge: score descending, then id (shards have disjoint documents).
    hits.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    hits.truncate(k);
    failed.sort();
    let body = json!({
        "hits": hits.iter().map(|(id, s)| json!({ "id": id, "score": s })).collect::<Vec<_>>(),
        "took_us": start.elapsed().as_micros() as u64,
        "partial": !failed.is_empty(),
        "failed_shards": failed,
    });
    Json(body).into_response()
}

async fn ingest(State(c): State<St>, Json(req): Json<Value>) -> Response {
    let n = c.shards.len();
    let mut groups: Vec<Vec<Value>> = vec![Vec::new(); n];
    for d in req["docs"].as_array().into_iter().flatten() {
        groups[shard_of(d["id"].as_str().unwrap_or_default(), n)].push(d.clone());
    }
    // Write each shard's group to every replica; acknowledge when all did.
    let mut calls = Vec::new();
    for (s, docs) in groups.iter().enumerate() {
        if docs.is_empty() {
            continue;
        }
        for r in &c.shards[s] {
            let body = json!({ "docs": docs });
            let (c, r) = (c.clone(), r.clone());
            calls.push(tokio::spawn(async move {
                c.post_once(&r, "/v1/documents", &body).await
            }));
        }
    }
    for call in calls {
        match call.await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }
    Json(json!({ "accepted": req["docs"].as_array().map_or(0, |a| a.len()) })).into_response()
}

async fn commit(State(c): State<St>) -> Response {
    for shard in &c.shards {
        for r in shard {
            if let Err(e) = c.post_once(r, "/v1/commit", &json!({})).await {
                return (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response();
            }
        }
    }
    Json(json!({ "ok": true })).into_response()
}

async fn stats(State(c): State<St>) -> Json<Value> {
    Json(json!({
        "shards": c.shards.len(),
        "requests_sent": c.requests_sent.load(Ordering::Relaxed),
        "hedges_fired": c.hedges_fired.load(Ordering::Relaxed),
        "hedges_won": c.hedges_won.load(Ordering::Relaxed),
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let shards: Vec<Vec<String>> = args
        .shards
        .split(';')
        .map(|s| {
            s.split(',')
                .map(|u| u.trim().to_string())
                .filter(|u| !u.is_empty())
                .collect()
        })
        .collect();
    anyhow::ensure!(
        shards.iter().all(|r| !r.is_empty()),
        "every shard needs at least one replica"
    );
    let coord = Arc::new(Coord {
        shards,
        client: reqwest::Client::builder()
            .pool_max_idle_per_host(256)
            .build()?,
        timeout: Duration::from_millis(args.timeout_ms),
        hedge: (args.hedge_ms > 0).then(|| Duration::from_millis(args.hedge_ms)),
        rr: AtomicU64::new(0),
        requests_sent: AtomicU64::new(0),
        hedges_fired: AtomicU64::new(0),
        hedges_won: AtomicU64::new(0),
    });
    let app = Router::new()
        .route("/v1/search", post(search))
        .route("/v1/documents", post(ingest))
        .route("/v1/commit", post(commit))
        .route("/coord/stats", get(stats))
        .route("/healthz", get(|| async { "ok" }))
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024 * 1024))
        .with_state(coord);
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
