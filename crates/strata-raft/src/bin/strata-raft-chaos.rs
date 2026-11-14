//! `strata-raft-chaos`: kill the leader of a 3-node replicated shard over
//! and over while a client writes, and check that no acknowledged write is
//! lost and that consistent reads never miss an acknowledged write.
//!
//! ```text
//! strata-raft-chaos --dir data/raft-chaos --kills 25 --out results/stage8/chaos.json
//! ```
//!
//! Each round: find the leader, SIGKILL it, measure the time until a write
//! is acknowledged again (by the new leader), restart the killed node after
//! a short delay. A writer task writes batches of new ids continuously,
//! following "not leader" redirects. After every acknowledged batch it
//! immediately performs a *linearizable* read of one id from that batch;
//! a miss would be a linearizability violation. At the end, with all nodes
//! up, every acknowledged id is checked with a consistent read on the
//! leader and with a plain local read on every replica (convergence).

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use clap::Parser;
use serde::Serialize;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    dir: PathBuf,
    #[arg(long, default_value_t = 25)]
    kills: usize,
    #[arg(long, default_value_t = 7850)]
    base_port: u16,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Serialize, Default)]
struct Report {
    kills: usize,
    acked_batches: usize,
    acked_ids: usize,
    failed_writes: usize,
    /// Milliseconds from SIGKILL of the leader to the next acknowledged write.
    failover_ms: Vec<f64>,
    failover_ms_p50: f64,
    failover_ms_max: f64,
    consistent_read_after_ack_checks: usize,
    consistent_read_after_ack_violations: usize,
    /// After each failover: consistent reads, on the *new* leader, of the
    /// last ids acknowledged by the *killed* leader.
    read_after_failover_checks: usize,
    read_after_failover_violations: usize,
    /// Contrast: plain (non-consistent) reads of a just-acknowledged id on
    /// another replica. Misses here are allowed (stale reads) and show why
    /// the consistent path exists.
    follower_plain_read_checks: usize,
    follower_plain_read_misses: usize,
    lost_acked_ids_leader_consistent_read: usize,
    replicas_missing_acked_ids: Vec<usize>,
    /// Final checks that never got a definite answer after retries.
    unverified_checks: usize,
    /// Time for the restarted node to catch up (all replicas applied the
    /// commit index seen when it came back).
    catchup_ms: Vec<f64>,
    catchup_ms_p50: f64,
    catchup_ms_max: f64,
}

struct Cluster {
    exe: PathBuf,
    dir: PathBuf,
    ports: Vec<u16>,
    procs: Vec<Option<Child>>,
}

impl Cluster {
    fn url(&self, i: usize) -> String {
        format!("http://127.0.0.1:{}", self.ports[i])
    }

    fn peers(&self) -> String {
        (0..self.ports.len())
            .map(|i| format!("{}={}", i + 1, self.url(i)))
            .collect::<Vec<_>>()
            .join(",")
    }

    fn start(&mut self, i: usize) -> Result<()> {
        let child = Command::new(&self.exe)
            .args([
                "--id",
                &(i + 1).to_string(),
                "--peers",
                &self.peers(),
                "--dir",
            ])
            .arg(self.dir.join(format!("node{}", i + 1)))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        self.procs[i] = Some(child);
        Ok(())
    }

    fn kill(&mut self, i: usize) -> Result<()> {
        if let Some(mut c) = self.procs[i].take() {
            c.kill()?; // SIGKILL
            c.wait()?;
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let _ = std::fs::remove_dir_all(&args.dir);
    std::fs::create_dir_all(&args.dir)?;
    let exe = std::env::current_exe()?.with_file_name("strata-raft-node");
    let mut cluster = Cluster {
        exe,
        dir: args.dir.clone(),
        ports: (0..3).map(|i| args.base_port + i).collect(),
        procs: vec![None, None, None],
    };
    for i in 0..3 {
        cluster.start(i)?;
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .build()?;
    let urls: Vec<String> = (0..3).map(|i| cluster.url(i)).collect();

    // Shared record of acknowledged ids and the time of the last ack.
    let acked: Arc<Mutex<Vec<String>>> = Arc::default();
    let last_ack: Arc<Mutex<Instant>> = Arc::new(Mutex::new(Instant::now()));
    let stats: Arc<Mutex<Report>> = Arc::default();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let writer = {
        let (client, urls, acked, last_ack, stats, stop) = (
            client.clone(),
            urls.clone(),
            acked.clone(),
            last_ack.clone(),
            stats.clone(),
            stop.clone(),
        );
        tokio::spawn(async move {
            let mut next = 0u64;
            let mut target = 0usize;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let ids: Vec<String> = (next..next + 10).map(|i| format!("doc-{i}")).collect();
                let docs: Vec<(String, String)> = ids
                    .iter()
                    .map(|id| (id.clone(), format!("payload of {id}")))
                    .collect();
                let res = client
                    .post(format!("{}/v1/documents", urls[target]))
                    .json(&serde_json::json!({ "docs": docs }))
                    .send()
                    .await;
                match res {
                    Ok(r) if r.status().is_success() => {
                        *last_ack.lock().unwrap() = Instant::now();
                        acked.lock().unwrap().extend(ids.iter().cloned());
                        stats.lock().unwrap().acked_batches += 1;
                        next += 10;
                        // Linearizable read right after the ack.
                        let probe = &ids[ids.len() / 2];
                        let r = client
                            .get(format!(
                                "{}/v1/documents/{probe}?consistent=true",
                                urls[target]
                            ))
                            .send()
                            .await;
                        let other = client
                            .get(format!(
                                "{}/v1/documents/{probe}",
                                urls[(target + 1) % urls.len()]
                            ))
                            .send()
                            .await;
                        let mut s = stats.lock().unwrap();
                        if let Ok(o) = other {
                            if o.status() == 200 || o.status() == 404 {
                                s.follower_plain_read_checks += 1;
                                s.follower_plain_read_misses += usize::from(o.status() == 404);
                            }
                        }
                        if let Ok(r) = r {
                            if r.status() == 200 {
                                s.consistent_read_after_ack_checks += 1;
                            } else if r.status() == 404 {
                                s.consistent_read_after_ack_checks += 1;
                                s.consistent_read_after_ack_violations += 1;
                            } // 421/503: leadership changed in between; inconclusive, skipped
                        }
                    }
                    Ok(r) if r.status() == 421 => {
                        let body: serde_json::Value = r.json().await.unwrap_or_default();
                        match body["leader"]
                            .as_str()
                            .and_then(|l| urls.iter().position(|u| u == l))
                        {
                            Some(i) => target = i,
                            None => {
                                target = (target + 1) % urls.len();
                                tokio::time::sleep(Duration::from_millis(20)).await;
                            }
                        }
                        stats.lock().unwrap().failed_writes += 1;
                        // Not acknowledged; the same ids are retried (upsert).
                    }
                    _ => {
                        stats.lock().unwrap().failed_writes += 1;
                        target = (target + 1) % urls.len();
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            }
        })
    };

    let find_leader = |client: reqwest::Client, urls: Vec<String>| async move {
        for _ in 0..200 {
            for (i, u) in urls.iter().enumerate() {
                if let Ok(r) = client.get(format!("{u}/raft/status")).send().await {
                    if let Ok(v) = r.json::<serde_json::Value>().await {
                        if v["role"] == "Leader" {
                            return Some(i);
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    };

    tokio::time::sleep(Duration::from_secs(2)).await;
    for k in 0..args.kills {
        let Some(leader) = find_leader(client.clone(), urls.clone()).await else {
            bail!("no leader elected")
        };
        cluster.kill(leader)?;
        let killed_at = Instant::now();
        // Everything acknowledged so far was acknowledged by the old leader
        // (or earlier ones); the new leader must have all of it.
        let before: Vec<String> = {
            let a = acked.lock().unwrap();
            a[a.len().saturating_sub(50)..].to_vec()
        };
        // Wait for the next acknowledged write after the kill.
        let failover = loop {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let t = *last_ack.lock().unwrap();
            if t > killed_at {
                break t - killed_at;
            }
            if killed_at.elapsed() > Duration::from_secs(20) {
                bail!("no write acknowledged within 20 s after kill {k}");
            }
        };
        stats
            .lock()
            .unwrap()
            .failover_ms
            .push(failover.as_secs_f64() * 1e3);
        if let Some(new_leader) = find_leader(client.clone(), urls.clone()).await {
            for id in &before {
                if let Ok(r) = client
                    .get(format!(
                        "{}/v1/documents/{id}?consistent=true",
                        urls[new_leader]
                    ))
                    .send()
                    .await
                {
                    let mut s = stats.lock().unwrap();
                    if r.status() == 200 || r.status() == 404 {
                        s.read_after_failover_checks += 1;
                        s.read_after_failover_violations += usize::from(r.status() == 404);
                    }
                }
            }
        }
        eprintln!(
            "kill {k:2}: node {} -> write acked again after {:.0} ms",
            leader + 1,
            failover.as_secs_f64() * 1e3
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        cluster.start(leader)?; // rejoin (replays its log, catches up)
                                // Kill the next leader only once the cluster is whole again: all
                                // three replicas have applied what was committed when the node came
                                // back. (Otherwise the next kill can leave a node that is still
                                // catching up as the only partner, and we would be timing catch-up,
                                // not failover.) The catch-up time is reported separately.
        let t = Instant::now();
        caught_up(&client, &urls, Duration::from_secs(120)).await?;
        stats
            .lock()
            .unwrap()
            .catchup_ms
            .push(t.elapsed().as_secs_f64() * 1e3);
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    writer.await?;

    // Settle (every replica applied everything committed), then verify
    // every acknowledged id. A request that errors (timeout, 5xx, 421) is
    // retried; only a definite "not found" counts as lost.
    caught_up(&client, &urls, Duration::from_secs(120)).await?;
    let acked = acked.lock().unwrap().clone();
    let mut lost = 0;
    let mut unverified = 0;
    for id in &acked {
        match check(&client, &urls, id, true).await {
            Some(true) => {}
            Some(false) => lost += 1,
            None => unverified += 1,
        }
    }
    let mut missing = Vec::new();
    for u in &urls {
        let mut m = 0;
        for id in &acked {
            match check(&client, std::slice::from_ref(u), id, false).await {
                Some(true) => {}
                Some(false) => m += 1,
                None => unverified += 1,
            }
        }
        missing.push(m);
    }
    for i in 0..3 {
        cluster.kill(i)?;
    }
    let mut report = std::mem::take(&mut *stats.lock().unwrap());
    report.kills = args.kills;
    report.acked_ids = acked.len();
    report.lost_acked_ids_leader_consistent_read = lost;
    report.replicas_missing_acked_ids = missing;
    report.unverified_checks = unverified;
    let mut c = report.catchup_ms.clone();
    c.sort_by(f64::total_cmp);
    if !c.is_empty() {
        report.catchup_ms_p50 = c[c.len() / 2];
        report.catchup_ms_max = *c.last().unwrap();
    }
    let mut f = report.failover_ms.clone();
    f.sort_by(f64::total_cmp);
    report.failover_ms_p50 = f[f.len() / 2];
    report.failover_ms_max = *f.last().unwrap();
    println!(
        "kills={} acked_ids={} LOST={} unverified={} read-after-ack violations={}/{} read-after-failover violations={}/{} follower plain-read misses={}/{} failover p50={:.0}ms max={:.0}ms catch-up p50={:.0}ms max={:.0}ms replicas missing={:?}",
        report.kills,
        report.acked_ids,
        report.lost_acked_ids_leader_consistent_read,
        report.unverified_checks,
        report.consistent_read_after_ack_violations,
        report.consistent_read_after_ack_checks,
        report.read_after_failover_violations,
        report.read_after_failover_checks,
        report.follower_plain_read_misses,
        report.follower_plain_read_checks,
        report.failover_ms_p50,
        report.failover_ms_max,
        report.catchup_ms_p50,
        report.catchup_ms_max,
        report.replicas_missing_acked_ids
    );
    if let Some(o) = args.out {
        std::fs::create_dir_all(o.parent().unwrap())?;
        std::fs::write(o, serde_json::to_string_pretty(&report)?)?;
    }
    Ok(())
}

/// Is `id` present? `consistent` asks the leader (found via redirects among
/// `urls`) for a linearizable read; otherwise asks `urls[0]` directly.
/// `Some(true/false)` is a definite answer; `None` = no answer in 10 tries.
async fn check(
    client: &reqwest::Client,
    urls: &[String],
    id: &str,
    consistent: bool,
) -> Option<bool> {
    let mut target = 0;
    for _ in 0..10 {
        let url = if consistent {
            format!("{}/v1/documents/{id}?consistent=true", urls[target])
        } else {
            format!("{}/v1/documents/{id}", urls[target])
        };
        if let Ok(r) = client.get(url).send().await {
            match r.status().as_u16() {
                200 => return Some(true),
                404 => return Some(false),
                421 => {
                    let body: serde_json::Value = r.json().await.unwrap_or_default();
                    if let Some(i) = body["leader"]
                        .as_str()
                        .and_then(|l| urls.iter().position(|u| u == l))
                    {
                        target = i;
                        continue;
                    }
                    target = (target + 1) % urls.len();
                }
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

/// Wait until every replica has applied at least the highest commit index
/// any of them reports right now.
async fn caught_up(client: &reqwest::Client, urls: &[String], limit: Duration) -> Result<()> {
    let start = Instant::now();
    let mut target: Option<u64> = None;
    loop {
        let mut applied = Vec::new();
        let mut commit = 0;
        for u in urls {
            if let Ok(r) = client.get(format!("{u}/raft/status")).send().await {
                if let Ok(v) = r.json::<serde_json::Value>().await {
                    applied.push(v["applied"].as_u64().unwrap_or(0));
                    commit = commit.max(v["commit"].as_u64().unwrap_or(0));
                }
            }
        }
        if target.is_none() && commit > 0 {
            target = Some(commit);
        }
        let goal = target.unwrap_or(u64::MAX);
        if applied.len() == urls.len() && applied.iter().all(|&a| a >= goal) {
            return Ok(());
        }
        if start.elapsed() > limit {
            bail!("replicas did not catch up to {goal} within {limit:?} (applied {applied:?})");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
