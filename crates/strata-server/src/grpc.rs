//! gRPC API (tonic), generated from `proto/strata.proto` at build time.
//! Same engine and semantics as the HTTP API.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::engine::{Engine, IngestDoc, Mode, SearchRequest};
use crate::metrics::Metrics;

/// Generated types and service traits (`tonic::include_proto!` pastes the
/// code that `build.rs` produced into this module).
pub mod pb {
    tonic::include_proto!("strata.v1");
}

use pb::strata_server::Strata;

pub struct GrpcService {
    pub engine: Arc<Engine>,
    pub metrics: Arc<Metrics>,
}

fn status(e: anyhow::Error) -> Status {
    Status::invalid_argument(format!("{e:#}"))
}

/// Run blocking engine work on tokio's blocking pool and map errors.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, Status> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Status::internal(e.to_string()))?
        .map_err(status)
}

#[tonic::async_trait]
impl Strata for GrpcService {
    async fn ingest(
        &self,
        req: Request<pb::IngestRequest>,
    ) -> Result<Response<pb::IngestResponse>, Status> {
        let docs: Vec<IngestDoc> = req
            .into_inner()
            .docs
            .into_iter()
            .map(|d| IngestDoc {
                id: d.id,
                text: d.text,
                fields: d
                    .fields
                    .into_iter()
                    .filter_map(|f| {
                        let v = match f.value? {
                            pb::field::Value::Keyword(s) => strata_storage::FieldValue::Keyword(s),
                            pb::field::Value::Int(i) => strata_storage::FieldValue::Int(i),
                        };
                        Some((f.name, v))
                    })
                    .collect(),
                vector: (!d.vector.is_empty()).then_some(d.vector),
            })
            .collect();
        let n = docs.len();
        let engine = self.engine.clone();
        let seq = blocking(move || engine.ingest(docs)).await?;
        self.metrics.ingested_docs.inc_by(n as u64);
        Ok(Response::new(pb::IngestResponse {
            seq,
            accepted: n as u32,
        }))
    }

    async fn commit(
        &self,
        _: Request<pb::CommitRequest>,
    ) -> Result<Response<pb::CommitResponse>, Status> {
        let engine = self.engine.clone();
        blocking(move || engine.commit()).await?;
        let (live, segments, _) = self.engine.stats();
        Ok(Response::new(pb::CommitResponse {
            segments: segments as u32,
            live_docs: live,
        }))
    }

    async fn search(
        &self,
        req: Request<pb::SearchRequest>,
    ) -> Result<Response<pb::SearchResponse>, Status> {
        let r = req.into_inner();
        let mode = match r.mode.as_str() {
            "" | "lexical" => Mode::Lexical,
            "dense" => Mode::Dense,
            "hybrid" => Mode::Hybrid,
            other => return Err(Status::invalid_argument(format!("unknown mode {other:?}"))),
        };
        let sr = SearchRequest {
            query: r.query,
            k: if r.k == 0 { 10 } else { r.k as usize },
            mode,
            vector: (!r.vector.is_empty()).then_some(r.vector),
        };
        let engine = self.engine.clone();
        let start = std::time::Instant::now();
        let res = blocking(move || engine.search(&sr)).await;
        self.metrics
            .latency
            .with_label_values(&["grpc_search"])
            .observe(start.elapsed().as_secs_f64());
        let res = res?;
        Ok(Response::new(pb::SearchResponse {
            hits: res
                .hits
                .into_iter()
                .map(|h| pb::Hit {
                    id: h.id,
                    score: h.score,
                })
                .collect(),
            took_us: res.took_us,
        }))
    }
}
