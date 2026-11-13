//! `strata-chaos`: crash-consistency test with real `kill -9`.
//!
//! ```text
//! strata-chaos run --dir /tmp/chaos --iterations 100 --out results/stage2/chaos.json
//! ```
//!
//! Each iteration:
//! 1. spawn a **child** process (`strata-chaos child ...`) that opens the index
//!    (running recovery), then writes forever: batches of new documents,
//!    occasional deletes of documents it added earlier, periodic commits,
//!    and background merges. After every successful (fsynced) WAL append it
//!    prints `ACK ...` on stdout;
//! 2. after a random delay, the parent sends **SIGKILL** (`Child::kill` on
//!    Unix), so the child gets no chance to clean up: buffers, half-written
//!    segments, torn WAL records and running merges are all possible;
//! 3. the parent reopens the index itself (recovery), commits, and checks
//!    every acknowledged operation: each acked add whose id was not acked as
//!    deleted later must be findable; each acked delete must be absent.
//!
//! Unacknowledged operations may or may not survive; both are correct.
//! What this does **not** cover: power loss / OS crash (the page cache
//! survives a process kill), disk-full, or bit rot.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use strata_core::analyzer::AnalyzerConfig;
use strata_storage::{Index, IndexConfig};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Parent: kill/restart loop and verification.
    Run {
        #[arg(long)]
        dir: PathBuf,
        #[arg(long, default_value_t = 100)]
        iterations: usize,
        #[arg(long, default_value_t = 30)]
        min_ms: u64,
        #[arg(long, default_value_t = 1500)]
        max_ms: u64,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long)]
        out: Option<PathBuf>,
        /// Negative control: make the child acknowledge adds *before*
        /// writing them. The harness must then report lost writes.
        #[arg(long)]
        broken_ack_before_write: bool,
    },
    /// Child: write until killed.
    Child {
        #[arg(long)]
        dir: PathBuf,
        #[arg(long)]
        start: u64,
        #[arg(long)]
        seed: u64,
        #[arg(long)]
        broken_ack_before_write: bool,
    },
}

/// Tiny deterministic PRNG (xorshift64*), so runs are reproducible from a seed
/// without pulling in a `rand` dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn config() -> IndexConfig {
    IndexConfig {
        analyzer: AnalyzerConfig::english(),
        flush_docs: 500, // commit often: crashes hit flushes and merges too
        merge_factor: 4,
        background_merges: true,
    }
}

fn doc_text(rng: &mut Rng) -> String {
    (0..40)
        .map(|_| format!("w{}", rng.below(3000)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn child(dir: &Path, start: u64, seed: u64, broken: bool) -> Result<()> {
    let (index, _) = Index::open(dir, config())?;
    let mut rng = Rng(seed | 1);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut next = start;
    let mut mine: Vec<u64> = Vec::new();
    loop {
        let n = 1 + rng.below(20);
        let docs: Vec<(String, String)> = (next..next + n)
            .map(|i| (format!("doc-{i}"), doc_text(&mut rng)))
            .collect();
        if broken {
            writeln!(out, "ACK ADD {next} {}", next + n - 1)?; // lie: not durable yet
            out.flush()?;
            // Model an ack-before-persist bug with a window wide enough for a
            // random kill to land in (as with an asynchronous write queue).
            std::thread::sleep(Duration::from_millis(5));
            index.add_documents(&docs)?;
        } else {
            index.add_documents(&docs)?; // returns after fsync
            writeln!(out, "ACK ADD {next} {}", next + n - 1)?;
            out.flush()?;
        }
        mine.extend(next..next + n);
        next += n;
        if !mine.is_empty() && rng.below(4) == 0 {
            let victim = mine[rng.below(mine.len() as u64) as usize];
            // Announce first: if we are killed after the WAL fsync but before
            // the ACK, the delete may or may not have happened, and the
            // parent must not count that id either way.
            writeln!(out, "TRY DEL {victim}")?;
            out.flush()?;
            index.delete(&[format!("doc-{victim}")])?;
            writeln!(out, "ACK DEL {victim}")?;
            out.flush()?;
        }
    }
}

#[derive(Serialize, Default)]
struct Iteration {
    kill_after_ms: u64,
    acked_adds: usize,
    acked_deletes: usize,
    /// Deletes in flight at the kill (outcome legitimately unknown; the id is
    /// excluded from further checks).
    unacked_deletes: usize,
    recovery_ms: f64,
    wal_records_replayed: u64,
    wal_truncated_bytes: u64,
    segments_after: usize,
    /// Acked-and-not-deleted ids missing after this recovery (cumulative
    /// state, not new losses).
    lost_acked_adds: usize,
    resurrected_deletes: usize,
}

#[derive(Serialize)]
struct Summary {
    broken_ack_before_write: bool,
    iterations: usize,
    total_acked_adds: usize,
    total_acked_deletes: usize,
    lost_acked_adds: usize,
    resurrected_deletes: usize,
    iterations_with_torn_wal_tail: usize,
    final_live_docs: u64,
    recovery_ms_p50: f64,
    recovery_ms_max: f64,
    per_iteration: Vec<Iteration>,
}

fn run(
    dir: &Path,
    iterations: usize,
    min_ms: u64,
    max_ms: u64,
    seed: u64,
    out: Option<PathBuf>,
    broken: bool,
) -> Result<()> {
    let _ = std::fs::remove_dir_all(dir);
    let exe = std::env::current_exe()?;
    let mut rng = Rng(seed | 1);
    // Ground truth of acknowledged state: id -> live?
    let mut expected: HashMap<u64, bool> = HashMap::new();
    let mut next_id = 0u64;
    let mut iters = Vec::new();
    // Distinct ids ever found violating durability.
    let mut lost_ids: HashSet<u64> = HashSet::new();
    let mut resurrected_ids: HashSet<u64> = HashSet::new();
    for it in 0..iterations {
        let mut child = Command::new(&exe)
            .args(["child", "--dir"])
            .arg(dir)
            .args([
                "--start",
                &next_id.to_string(),
                "--seed",
                &rng.next().to_string(),
            ])
            .args(if broken {
                &["--broken-ack-before-write"][..]
            } else {
                &[][..]
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("spawn child")?;
        let acks: Arc<Mutex<Vec<String>>> = Arc::default();
        let reader = {
            let acks = Arc::clone(&acks);
            let stdout = child.stdout.take().unwrap();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    acks.lock().unwrap().push(line);
                }
            })
        };
        let wait = min_ms + rng.below(max_ms - min_ms + 1);
        std::thread::sleep(Duration::from_millis(wait));
        child.kill()?; // SIGKILL
        child.wait()?;
        reader.join().unwrap(); // pipe closed: all acks the child printed are read

        let mut uncertain: HashSet<u64> = HashSet::new();
        let mut record = Iteration {
            kill_after_ms: wait,
            ..Default::default()
        };
        for line in acks.lock().unwrap().iter() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            match parts.as_slice() {
                ["ACK", "ADD", a, b] => {
                    let (a, b): (u64, u64) = (a.parse()?, b.parse()?);
                    for id in a..=b {
                        expected.insert(id, true);
                    }
                    record.acked_adds += (b - a + 1) as usize;
                    next_id = next_id.max(b + 1);
                }
                ["TRY", "DEL", id] => {
                    uncertain.insert(id.parse()?);
                }
                ["ACK", "DEL", id] => {
                    let id: u64 = id.parse()?;
                    uncertain.remove(&id);
                    expected.insert(id, false);
                    record.acked_deletes += 1;
                }
                _ => {}
            }
        }

        // A delete that was attempted but not acknowledged: unknown outcome.
        for id in &uncertain {
            expected.remove(id);
        }
        record.unacked_deletes = uncertain.len();

        // Recover in this process and verify.
        let t = Instant::now();
        let (index, report) = Index::open(dir, config())?;
        record.recovery_ms = t.elapsed().as_secs_f64() * 1e3;
        record.wal_records_replayed = report.wal.records_replayed;
        record.wal_truncated_bytes = report.wal.truncated_bytes;
        index.commit()?;
        index.wait_for_merges()?;
        let snap = index.snapshot();
        record.segments_after = snap.segments().len();
        for (&id, &live) in &expected {
            let found = snap.find(&format!("doc-{id}")).is_some();
            if live && !found {
                record.lost_acked_adds += 1;
                lost_ids.insert(id);
            }
            if !live && found {
                record.resurrected_deletes += 1;
                resurrected_ids.insert(id);
            }
        }
        eprintln!(
            "iter {it:3}: killed after {wait:4} ms, acks +{} -{}, recovery {:.1} ms (replayed {}, torn {} B), lost {}, resurrected {}",
            record.acked_adds,
            record.acked_deletes,
            record.recovery_ms,
            record.wal_records_replayed,
            record.wal_truncated_bytes,
            record.lost_acked_adds,
            record.resurrected_deletes
        );
        drop(snap);
        drop(index);
        iters.push(record);
    }

    let (index, _) = Index::open(dir, config())?;
    let mut rec: Vec<f64> = iters.iter().map(|i| i.recovery_ms).collect();
    rec.sort_by(f64::total_cmp);
    let summary = Summary {
        broken_ack_before_write: broken,
        iterations,
        total_acked_adds: iters.iter().map(|i| i.acked_adds).sum(),
        total_acked_deletes: iters.iter().map(|i| i.acked_deletes).sum(),
        lost_acked_adds: lost_ids.len(),
        resurrected_deletes: resurrected_ids.len(),
        iterations_with_torn_wal_tail: iters.iter().filter(|i| i.wal_truncated_bytes > 0).count(),
        final_live_docs: index.snapshot().num_live_docs(),
        recovery_ms_p50: rec[rec.len() / 2],
        recovery_ms_max: *rec.last().unwrap(),
        per_iteration: iters,
    };
    let expected_live = expected.values().filter(|&&l| l).count();
    println!(
        "iterations={} acked_adds={} acked_deletes={} LOST={} RESURRECTED={} torn_tails={} live_docs={} (expected live >= {}) recovery p50={:.1}ms max={:.1}ms",
        summary.iterations,
        summary.total_acked_adds,
        summary.total_acked_deletes,
        summary.lost_acked_adds,
        summary.resurrected_deletes,
        summary.iterations_with_torn_wal_tail,
        summary.final_live_docs,
        expected_live,
        summary.recovery_ms_p50,
        summary.recovery_ms_max
    );
    if let Some(out) = out {
        std::fs::create_dir_all(out.parent().unwrap())?;
        std::fs::write(out, serde_json::to_string_pretty(&summary)?)?;
    }
    if summary.lost_acked_adds + summary.resurrected_deletes > 0 {
        anyhow::bail!("durability violation");
    }
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Run {
            dir,
            iterations,
            min_ms,
            max_ms,
            seed,
            out,
            broken_ack_before_write,
        } => run(
            &dir,
            iterations,
            min_ms,
            max_ms,
            seed,
            out,
            broken_ack_before_write,
        ),
        Cmd::Child {
            dir,
            start,
            seed,
            broken_ack_before_write,
        } => child(&dir, start, seed, broken_ack_before_write),
    }
}
