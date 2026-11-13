//! HTTP/JSON API (axum).
//!
//! ```text
//! POST   /v1/documents        {"docs": [{"id", "text", "fields"?, "vector"?}]}  -> {"seq", "accepted"}
//! DELETE /v1/documents/{id}                                                    -> {"seq"}
//! POST   /v1/commit                                                            -> {"segments", "live_docs"}
//! POST   /v1/search           {"query", "k"?, "mode"?, "vector"?}               -> {"hits", "took_us"}
//! GET    /metrics             Prometheus text format
//! GET    /healthz
//! ```
//!
//! Index work is synchronous and may block on fsync, so handlers run it on
//! tokio's blocking thread pool (`spawn_blocking`) instead of stalling the
//! async worker threads that serve other connections.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::engine::{Engine, IngestDoc, SearchRequest, SearchResponse};
use crate::metrics::Metrics;

/// Fault injection for distributed-systems experiments: delay a fraction
/// of search requests.
#[derive(Clone, Copy, Debug, Default)]
pub struct Faults {
    pub delay: Duration,
    pub probability: f64,
}

#[derive(Clone)]
pub struct AppState {
    pub engine: Arc<Engine>,
    pub metrics: Arc<Metrics>,
    pub faults: Faults,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/documents", post(ingest))
        .route("/v1/documents/{id}", delete(remove))
        .route("/v1/commit", post(commit))
        .route("/v1/search", post(search))
        .route("/metrics", get(metrics))
        .route("/healthz", get(|| async { "ok" }))
        // Bulk ingest batches with embeddings are large (500 docs x 384
        // floats as JSON is several MB); axum's default limit is 2 MB.
        .layer(DefaultBodyLimit::max(256 * 1024 * 1024))
        .with_state(state)
}

/// Turns any error into a 400 response with a JSON message.
pub struct ApiError(anyhow::Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({ "error": format!("{:#}", self.0) }));
        (StatusCode::BAD_REQUEST, body).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError(e.into())
    }
}

/// Run blocking engine work off the async threads, recording latency and
/// outcome under `route`.
async fn timed<T: Send + 'static>(
    m: &Metrics,
    route: &str,
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    let start = Instant::now();
    let res = tokio::task::spawn_blocking(work)
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    m.latency
        .with_label_values(&[route])
        .observe(start.elapsed().as_secs_f64());
    m.requests
        .with_label_values(&[route, if res.is_ok() { "ok" } else { "error" }])
        .inc();
    Ok(res?)
}

#[derive(Deserialize)]
struct IngestBody {
    docs: Vec<IngestDoc>,
}

#[derive(Serialize)]
struct IngestReply {
    seq: u64,
    accepted: usize,
}

async fn ingest(
    State(s): State<AppState>,
    Json(body): Json<IngestBody>,
) -> Result<Json<IngestReply>, ApiError> {
    let n = body.docs.len();
    let engine = s.engine.clone();
    let seq = timed(&s.metrics, "ingest", move || engine.ingest(body.docs)).await?;
    s.metrics.ingested_docs.inc_by(n as u64);
    Ok(Json(IngestReply { seq, accepted: n }))
}

async fn remove(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let engine = s.engine.clone();
    let seq = timed(&s.metrics, "delete", move || engine.delete(&[id])).await?;
    Ok(Json(serde_json::json!({ "seq": seq })))
}

async fn commit(State(s): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    let engine = s.engine.clone();
    timed(&s.metrics, "commit", move || engine.commit()).await?;
    let (live, segments, _) = s.engine.stats();
    Ok(Json(
        serde_json::json!({ "segments": segments, "live_docs": live }),
    ))
}

async fn search(
    State(s): State<AppState>,
    Json(req): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, ApiError> {
    if s.faults.probability > 0.0 && rand_unit() < s.faults.probability {
        tokio::time::sleep(s.faults.delay).await; // injected slowness
    }
    let engine = s.engine.clone();
    Ok(Json(
        timed(&s.metrics, "search", move || engine.search(&req)).await?,
    ))
}

async fn metrics(State(s): State<AppState>) -> String {
    let (live, segments, vectors) = s.engine.stats();
    s.metrics.live_docs.set(live as i64);
    s.metrics.segments.set(segments as i64);
    s.metrics.vectors.set(vectors as i64);
    s.metrics.render()
}

/// Thread-local xorshift in [0, 1), for fault injection only.
fn rand_unit() -> f64 {
    use std::cell::Cell;
    thread_local!(static STATE: Cell<u64> = const { Cell::new(0x9E37_79B9_7F4A_7C15) });
    STATE.with(|st| {
        let mut x = st.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        st.set(x);
        (x >> 11) as f64 / (1u64 << 53) as f64
    })
}
