//! `strata-raft-node`: one replica of a Raft-replicated strata shard.
//!
//! ```text
//! strata-raft-node --id 1 --peers 1=http://127.0.0.1:7850,2=http://127.0.0.1:7851,3=http://127.0.0.1:7852 --dir data/r1
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use strata_raft::core::Timing;
use strata_raft::server::{Config, RaftShard};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    id: u64,
    /// Comma-separated `id=url` for every member, including this node.
    #[arg(long)]
    peers: String,
    #[arg(long)]
    dir: PathBuf,
    /// Milliseconds per Raft tick.
    #[arg(long, default_value_t = 10)]
    tick_ms: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    let mut peers = HashMap::new();
    for p in args.peers.split(',') {
        let (id, url) = p.split_once('=').context("peer must be id=url")?;
        peers.insert(id.parse()?, url.to_string());
    }
    let addr = peers[&args.id].trim_start_matches("http://").to_string();
    let shard = RaftShard::open(Config {
        id: args.id,
        peers,
        dir: args.dir,
        tick: Duration::from_millis(args.tick_ms),
        // 10 ms ticks: heartbeat every 50 ms, election timeout 150-300 ms
        // (the Raft paper's recommended range).
        timing: Timing {
            heartbeat: 5,
            election_min: 15,
            election_max: 30,
            max_batch: 64,
        },
    })?;
    tokio::spawn(shard.clone().run_ticker());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, shard.router()).await?;
    Ok(())
}
