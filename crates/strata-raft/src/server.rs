//! A replicated strata shard: the Raft core + durable Raft storage + an
//! HTTP transport, with a strata `Index` as the replicated state machine.
//!
//! ```text
//! client ──POST /v1/documents──► leader ──propose(batch)──► Raft log (fsync)
//!                                     │ AppendEntries over HTTP to followers (they fsync, ack)
//!                                     ▼ committed on a majority
//!                         apply to the local Index (WalOp batch) ──► reply 200 to the client
//! followers apply the same committed entries in the same order ──► read replicas
//! ```
//!
//! * Writes are acknowledged only after the entry is **committed** (on a
//!   majority of fsynced logs) **and applied** on the leader.
//! * `GET /v1/documents/{id}?consistent=true` is a **linearizable read**:
//!   the leader runs ReadIndex (confirms leadership with a quorum
//!   heartbeat round), waits until it has applied up to that commit index,
//!   then answers. Without `consistent`, any replica answers from its own
//!   state (possibly stale).
//! * The state machine (`Index`) keeps its own WAL. On restart a node
//!   re-applies the committed Raft log from the beginning; adds are
//!   upserts and deletes are idempotent, so replaying a prefix twice gives
//!   the same final state. (Without log compaction the log grows forever;
//!   fine for the tests, not for production.)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use strata_core::analyzer::AnalyzerConfig;
use strata_storage::{Document, Index, IndexConfig};
use tokio::sync::oneshot;

use crate::core::{Entry, Envelope, NodeId, RaftNode, Role, Timing};
use crate::storage::RaftStorage;

/// A replicated command: a batch of index operations.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    Upsert(Vec<(String, String)>),
    Delete(Vec<String>),
}

pub struct Config {
    pub id: NodeId,
    /// id -> base URL, including this node.
    pub peers: HashMap<NodeId, String>,
    pub dir: PathBuf,
    pub tick: Duration,
    pub timing: Timing,
}

struct Shared {
    core: RaftNode,
    storage: RaftStorage,
    /// Leader: proposal index -> (term, reply channel).
    waiters: HashMap<u64, (u64, oneshot::Sender<Result<(), String>>)>,
    /// Leader: read id -> reply channel (gets the index to wait for).
    reads: HashMap<u64, oneshot::Sender<u64>>,
    applied: u64,
    applied_tx: tokio::sync::watch::Sender<u64>,
}

pub struct RaftShard {
    cfg: Config,
    shared: Mutex<Shared>,
    index: Arc<Index>,
    client: reqwest::Client,
    applied_rx: tokio::sync::watch::Receiver<u64>,
}

impl RaftShard {
    pub fn open(cfg: Config) -> Result<Arc<RaftShard>> {
        let (storage, hs, log) = RaftStorage::open(&cfg.dir.join("raft"))?;
        let index_dir = cfg.dir.join("index");
        // The index is a pure function of the Raft log: rebuild it from
        // scratch on start (simplest way to never double-apply or miss).
        if index_dir.exists() {
            std::fs::remove_dir_all(&index_dir)?;
        }
        let (index, _) = Index::open(
            &index_dir,
            IndexConfig {
                analyzer: AnalyzerConfig::english(),
                flush_docs: 5_000,
                merge_factor: 4,
                background_merges: true,
            },
        )?;
        let mut ids: Vec<NodeId> = cfg.peers.keys().copied().collect();
        ids.sort();
        let core = RaftNode::new(
            cfg.id,
            ids,
            hs,
            log,
            cfg.timing,
            cfg.id * 7919 + std::process::id() as u64,
        );
        let (applied_tx, applied_rx) = tokio::sync::watch::channel(0);
        Ok(Arc::new(RaftShard {
            shared: Mutex::new(Shared {
                core,
                storage,
                waiters: HashMap::new(),
                reads: HashMap::new(),
                applied: 0,
                applied_tx,
            }),
            index,
            client: reqwest::Client::builder()
                .timeout(Duration::from_millis(500))
                .build()?,
            applied_rx,
            cfg,
        }))
    }

    /// Process the core's pending work: persist, send, apply, notify.
    fn process(self: &Arc<Self>, sh: &mut Shared) -> Result<()> {
        let ready = sh.core.take_ready();
        sh.storage.persist(&ready)?; // durable BEFORE any message leaves
        for env in ready.messages {
            let me = Arc::clone(self);
            tokio::spawn(async move { me.send(env).await });
        }
        for e in &ready.committed {
            self.apply(e)?;
            sh.applied = e.index;
            if let Some((term, tx)) = sh.waiters.remove(&e.index) {
                // Our proposal at this index was replaced by another
                // leader's entry if the term differs: report failure.
                let _ = tx.send(if term == e.term {
                    Ok(())
                } else {
                    Err("leadership lost".into())
                });
            }
        }
        if !ready.committed.is_empty() {
            let _ = sh.applied_tx.send(sh.applied);
        }
        for (id, idx) in ready.reads {
            if let Some(tx) = sh.reads.remove(&id) {
                let _ = tx.send(idx);
            }
        }
        if sh.core.role() != Role::Leader {
            // Pending proposals of a deposed leader fail fast; clients retry.
            for (_, (_, tx)) in sh.waiters.drain() {
                let _ = tx.send(Err("not leader".into()));
            }
            sh.reads.clear();
        }
        Ok(())
    }

    fn apply(&self, e: &Entry) -> Result<()> {
        if e.data.is_empty() {
            return Ok(()); // leader no-op
        }
        match serde_json::from_slice::<Command>(&e.data)? {
            Command::Upsert(docs) => {
                let docs: Vec<Document> = docs
                    .into_iter()
                    .map(|(id, text)| Document {
                        id,
                        text,
                        fields: Default::default(),
                    })
                    .collect();
                self.index.add_docs(&docs)?;
            }
            Command::Delete(ids) => {
                self.index.delete(&ids)?;
            }
        }
        Ok(())
    }

    async fn send(&self, env: Envelope) {
        let Some(url) = self.cfg.peers.get(&env.to) else {
            return;
        };
        // Fire and forget: Raft tolerates lost messages (retries on ticks).
        let _ = self
            .client
            .post(format!("{url}/raft/msg"))
            .json(&env)
            .send()
            .await;
    }

    /// Drive the logical clock.
    pub async fn run_ticker(self: Arc<Self>) {
        let mut interval = tokio::time::interval(self.cfg.tick);
        loop {
            interval.tick().await;
            let mut sh = self.shared.lock().unwrap();
            sh.core.tick();
            if let Err(e) = self.process(&mut sh) {
                tracing::error!("raft processing failed: {e:#}");
                std::process::exit(1); // storage failure: crash rather than lie
            }
        }
    }

    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/raft/msg", post(on_msg))
            .route("/raft/status", get(status))
            .route("/v1/documents", post(write_docs))
            .route("/v1/delete", post(delete_docs))
            .route("/v1/documents/{id}", get(read_doc))
            .route("/v1/commit", post(commit))
            .with_state(Arc::clone(self))
    }

    /// Propose a command and wait until it is committed and applied here.
    async fn replicate(self: &Arc<Self>, cmd: &Command) -> Result<(), Response> {
        let rx = {
            let mut sh = self.shared.lock().unwrap();
            match sh.core.propose(serde_json::to_vec(cmd).unwrap()) {
                Ok(idx) => {
                    let (tx, rx) = oneshot::channel();
                    let term = sh.core.term();
                    sh.waiters.insert(idx, (term, tx));
                    if let Err(e) = self.process(&mut sh) {
                        return Err(
                            (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response()
                        );
                    }
                    rx
                }
                Err(hint) => return Err(self.not_leader(hint)),
            }
        };
        match tokio::time::timeout(Duration::from_secs(3), rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) => Err((StatusCode::SERVICE_UNAVAILABLE, e).into_response()),
            _ => Err((StatusCode::SERVICE_UNAVAILABLE, "commit timeout").into_response()),
        }
    }

    fn not_leader(&self, hint: Option<NodeId>) -> Response {
        let leader = hint.and_then(|h| self.cfg.peers.get(&h).cloned());
        (
            StatusCode::MISDIRECTED_REQUEST,
            Json(serde_json::json!({ "error": "not leader", "leader": leader })),
        )
            .into_response()
    }
}

async fn on_msg(State(s): State<Arc<RaftShard>>, Json(env): Json<Envelope>) -> StatusCode {
    let mut sh = s.shared.lock().unwrap();
    sh.core.step(env);
    match s.process(&mut sh) {
        Ok(()) => StatusCode::OK,
        Err(e) => {
            tracing::error!("raft processing failed: {e:#}");
            std::process::exit(1);
        }
    }
}

#[derive(Serialize)]
struct Status {
    id: NodeId,
    role: Role,
    term: u64,
    leader: Option<NodeId>,
    commit: u64,
    applied: u64,
    last_index: u64,
    live_docs_buffered_or_committed: usize,
}

async fn status(State(s): State<Arc<RaftShard>>) -> Json<Status> {
    let sh = s.shared.lock().unwrap();
    Json(Status {
        id: s.cfg.id,
        role: sh.core.role(),
        term: sh.core.term(),
        leader: sh.core.leader(),
        commit: sh.core.commit_index(),
        applied: sh.applied,
        last_index: sh.core.last_index(),
        live_docs_buffered_or_committed: s.index.live_doc_count(),
    })
}

#[derive(Deserialize)]
struct WriteBody {
    docs: Vec<(String, String)>,
}

async fn write_docs(State(s): State<Arc<RaftShard>>, Json(b): Json<WriteBody>) -> Response {
    match s.replicate(&Command::Upsert(b.docs)).await {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct DeleteBody {
    ids: Vec<String>,
}

async fn delete_docs(State(s): State<Arc<RaftShard>>, Json(b): Json<DeleteBody>) -> Response {
    match s.replicate(&Command::Delete(b.ids)).await {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct ReadParams {
    #[serde(default)]
    consistent: bool,
}

async fn read_doc(
    State(s): State<Arc<RaftShard>>,
    Path(id): Path<String>,
    Query(p): Query<ReadParams>,
) -> Response {
    if p.consistent {
        let rx = {
            let mut sh = s.shared.lock().unwrap();
            match sh.core.read_index() {
                Ok(read_id) => {
                    let (tx, rx) = oneshot::channel();
                    sh.reads.insert(read_id, tx);
                    if let Err(e) = s.process(&mut sh) {
                        return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
                            .into_response();
                    }
                    rx
                }
                Err(hint) => return s.not_leader(hint),
            }
        };
        let Ok(Ok(idx)) = tokio::time::timeout(Duration::from_secs(2), rx).await else {
            return (StatusCode::SERVICE_UNAVAILABLE, "read index timeout").into_response();
        };
        // Wait until the state machine has applied up to the read index.
        let mut applied = s.applied_rx.clone();
        let ok =
            tokio::time::timeout(Duration::from_secs(2), applied.wait_for(|&a| a >= idx)).await;
        if !matches!(ok, Ok(Ok(_))) {
            return (StatusCode::SERVICE_UNAVAILABLE, "apply timeout").into_response();
        }
    }
    let found = s.index.contains_live(&id);
    (
        if found {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        },
        if found { "found" } else { "missing" },
    )
        .into_response()
}

async fn commit(State(s): State<Arc<RaftShard>>) -> Response {
    let idx = s.index.clone();
    match tokio::task::spawn_blocking(move || idx.commit()).await {
        Ok(Ok(())) => (StatusCode::OK, "ok").into_response(),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "commit failed").into_response(),
    }
}
