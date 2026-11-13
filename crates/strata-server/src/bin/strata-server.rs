//! `strata-server`: one strata node with HTTP and gRPC APIs.
//!
//! ```text
//! strata-server --dir data/node1 --http 127.0.0.1:7700 --grpc 127.0.0.1:7701 [--dim 384]
//!               [--otlp-endpoint http://localhost:4317 | --trace-stdout]
//! ```

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use strata_server::engine::{Engine, EngineConfig};
use strata_server::grpc::{pb::strata_server::StrataServer, GrpcService};
use strata_server::http::{router, AppState, Faults};
use strata_server::metrics::Metrics;
use strata_vector::HnswConfig;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    dir: PathBuf,
    #[arg(long, default_value = "127.0.0.1:7700")]
    http: SocketAddr,
    /// gRPC listen address (omit to disable gRPC).
    #[arg(long)]
    grpc: Option<SocketAddr>,
    /// Embedding dimension; enables dense and hybrid search.
    #[arg(long)]
    dim: Option<usize>,
    #[arg(long, default_value_t = 10_000)]
    flush_docs: usize,
    #[arg(long, default_value_t = 4)]
    merge_factor: usize,
    #[arg(long, default_value_t = 100)]
    ef_search: usize,
    /// Export OpenTelemetry spans to this OTLP/gRPC endpoint.
    #[arg(long)]
    otlp_endpoint: Option<String>,
    /// Print OpenTelemetry spans (human-readable) on stdout.
    #[arg(long)]
    trace_stdout: bool,
    /// Fault injection: delay this fraction of searches ...
    #[arg(long, default_value_t = 0.0)]
    inject_delay_prob: f64,
    /// ... by this many milliseconds.
    #[arg(long, default_value_t = 0)]
    inject_delay_ms: u64,
    /// Worker threads for the async runtime (default: number of cores).
    #[arg(long)]
    threads: Option<usize>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut rt = tokio::runtime::Builder::new_multi_thread();
    rt.enable_all();
    if let Some(t) = args.threads {
        rt.worker_threads(t);
    }
    rt.build()?.block_on(serve(args))
}

async fn serve(args: Args) -> Result<()> {
    let _telemetry = strata_server::telemetry::init(
        "strata-server",
        args.otlp_endpoint.as_deref(),
        args.trace_stdout,
    )?;
    let cfg = EngineConfig {
        dir: args.dir.clone(),
        flush_docs: args.flush_docs,
        merge_factor: args.merge_factor,
        dim: args.dim,
        hnsw: HnswConfig::default(),
        ef_search: args.ef_search,
    };
    let engine = Arc::new(tokio::task::spawn_blocking(move || Engine::open(&cfg)).await??);
    let metrics = Arc::new(Metrics::new());
    let faults = Faults {
        delay: Duration::from_millis(args.inject_delay_ms),
        probability: args.inject_delay_prob,
    };
    if let Some(addr) = args.grpc {
        let svc = GrpcService {
            engine: engine.clone(),
            metrics: metrics.clone(),
        };
        tokio::spawn(async move {
            if let Err(e) = tonic::transport::Server::builder()
                .add_service(StrataServer::new(svc))
                .serve(addr)
                .await
            {
                tracing::error!("gRPC server failed: {e}");
            }
        });
        tracing::info!(%addr, "gRPC listening");
    }
    let app = router(AppState {
        engine,
        metrics,
        faults,
    });
    let listener = tokio::net::TcpListener::bind(args.http).await?;
    tracing::info!(addr = %args.http, "HTTP listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
