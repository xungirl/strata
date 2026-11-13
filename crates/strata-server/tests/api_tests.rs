//! End-to-end tests of the node's HTTP and gRPC APIs and its vector store,
//! running real servers on ephemeral localhost ports.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use strata_server::engine::{Engine, EngineConfig};
use strata_server::grpc::pb::strata_client::StrataClient;
use strata_server::grpc::pb::strata_server::StrataServer;
use strata_server::grpc::{pb, GrpcService};
use strata_server::http::{router, AppState, Faults};
use strata_server::metrics::Metrics;
use strata_server::vector_store::VectorStore;
use strata_vector::HnswConfig;

struct TempDir(PathBuf);
impl TempDir {
    fn new(name: &str) -> TempDir {
        let p = std::env::temp_dir().join(format!("strata-api-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        TempDir(p)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn engine(dir: &TempDir) -> Arc<Engine> {
    Arc::new(
        Engine::open(&EngineConfig {
            dir: dir.0.clone(),
            flush_docs: 10_000,
            merge_factor: 4,
            dim: Some(3),
            hnsw: HnswConfig::default(),
            ef_search: 50,
        })
        .unwrap(),
    )
}

fn unit(v: [f32; 3]) -> Vec<f32> {
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    v.iter().map(|x| x / n).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_ingest_commit_search_delete_metrics() {
    let dir = TempDir::new("http");
    let state = AppState {
        engine: engine(&dir),
        metrics: Arc::new(Metrics::new()),
        faults: Faults::default(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
    let c = reqwest::Client::new();

    let docs = json!({ "docs": [
        { "id": "a", "text": "rust search engine", "vector": unit([1.0, 0.0, 0.0]), "fields": { "year": { "Int": 2020 } } },
        { "id": "b", "text": "python web framework", "vector": unit([0.0, 1.0, 0.0]), "fields": { "year": { "Int": 2024 } } },
        { "id": "c", "text": "search with python", "vector": unit([0.7, 0.7, 0.0]) }
    ]});
    let r: serde_json::Value = c
        .post(format!("{url}/v1/documents"))
        .json(&docs)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["accepted"], 3);
    // Not visible before commit.
    let s: serde_json::Value = c
        .post(format!("{url}/v1/search"))
        .json(&json!({ "query": "search" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["hits"].as_array().unwrap().len(), 0);
    c.post(format!("{url}/v1/commit")).send().await.unwrap();

    let ids = |v: &serde_json::Value| -> Vec<String> {
        v["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["id"].as_str().unwrap().to_string())
            .collect()
    };
    let s: serde_json::Value = c
        .post(format!("{url}/v1/search"))
        .json(&json!({ "query": "search" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut got = ids(&s);
    got.sort();
    assert_eq!(got, vec!["a", "c"]);
    // Query language through HTTP: filters and must-not.
    let s: serde_json::Value = c
        .post(format!("{url}/v1/search"))
        .json(&json!({ "query": "search -python year:[2019 TO 2021]" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ids(&s), vec!["a"]);
    // Dense and hybrid.
    let s: serde_json::Value = c
        .post(format!("{url}/v1/search"))
        .json(&json!({ "mode": "dense", "vector": unit([0.0, 1.0, 0.1]), "k": 1 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ids(&s), vec!["b"]);
    let s: serde_json::Value = c
        .post(format!("{url}/v1/search"))
        .json(&json!({ "mode": "hybrid", "query": "python", "vector": unit([0.0, 1.0, 0.0]), "k": 2 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ids(&s)[0], "b"); // top in both lists
                                 // Errors are 400 with a message.
    let r = c
        .post(format!("{url}/v1/search"))
        .json(&json!({ "query": "\"unterminated" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    assert!(r.text().await.unwrap().contains("unterminated"));
    // Delete + commit.
    c.delete(format!("{url}/v1/documents/a"))
        .send()
        .await
        .unwrap();
    c.post(format!("{url}/v1/commit")).send().await.unwrap();
    let s: serde_json::Value = c
        .post(format!("{url}/v1/search"))
        .json(&json!({ "query": "search" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ids(&s), vec!["c"]);
    let s: serde_json::Value = c
        .post(format!("{url}/v1/search"))
        .json(&json!({ "mode": "dense", "vector": unit([1.0, 0.0, 0.0]), "k": 3 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !ids(&s).contains(&"a".to_string()),
        "deleted vector must not be returned"
    );
    // Metrics.
    let m = c
        .get(format!("{url}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(m.contains("strata_ingested_docs_total 3"));
    assert!(m.contains("strata_live_docs 2"));
    assert!(m.contains("strata_request_duration_seconds_bucket{route=\"search\""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_ingest_commit_search() {
    let dir = TempDir::new("grpc");
    let svc = GrpcService {
        engine: engine(&dir),
        metrics: Arc::new(Metrics::new()),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(StrataServer::new(svc))
            .serve_with_incoming(tokio_stream_from(listener))
            .await
            .unwrap()
    });
    let mut client = StrataClient::connect(format!("http://{addr}"))
        .await
        .unwrap();
    let doc = |id: &str, text: &str| pb::Document {
        id: id.into(),
        text: text.into(),
        fields: vec![pb::Field {
            name: "lang".into(),
            value: Some(pb::field::Value::Keyword("en".into())),
        }],
        vector: vec![],
    };
    let r = client
        .ingest(pb::IngestRequest {
            docs: vec![doc("x", "graph databases"), doc("y", "vector databases")],
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(r.accepted, 2);
    let c = client
        .commit(pb::CommitRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(c.live_docs, 2);
    let s = client
        .search(pb::SearchRequest {
            query: "vector lang:en".into(),
            k: 5,
            mode: String::new(),
            vector: vec![],
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(s.hits.len(), 1);
    assert_eq!(s.hits[0].id, "y");
    let bad = client
        .search(pb::SearchRequest {
            query: "x".into(),
            k: 1,
            mode: "nope".into(),
            vector: vec![],
        })
        .await;
    assert_eq!(bad.unwrap_err().code(), tonic::Code::InvalidArgument);
}

/// Adapt a tokio listener into the incoming-connection stream tonic wants.
fn tokio_stream_from(
    listener: tokio::net::TcpListener,
) -> impl tokio_stream::Stream<Item = std::io::Result<tokio::net::TcpStream>> {
    tokio_stream::wrappers::TcpListenerStream::new(listener)
}

#[test]
fn vector_store_survives_restart_and_torn_tail() {
    let dir = TempDir::new("vectors");
    {
        let vs = VectorStore::open(&dir.0, 3, HnswConfig::default(), 20).unwrap();
        vs.upsert(&[
            ("a".into(), unit([1.0, 0.0, 0.0])),
            ("b".into(), unit([0.0, 1.0, 0.0])),
        ])
        .unwrap();
        vs.upsert(&[("a".into(), unit([0.0, 0.0, 1.0]))]).unwrap(); // replace a
    }
    // Simulate a torn append: garbage after the last complete record.
    let log = dir.0.join("vectors.log");
    let mut bytes = std::fs::read(&log).unwrap();
    let good = bytes.len();
    bytes.extend_from_slice(&[7, 0, 0, 0, 1, 2]);
    std::fs::write(&log, &bytes).unwrap();
    let vs = VectorStore::open(&dir.0, 3, HnswConfig::default(), 20).unwrap();
    assert_eq!(std::fs::metadata(&log).unwrap().len() as usize, good);
    let top = vs.search(&unit([0.0, 0.0, 1.0]), 1).unwrap();
    assert_eq!(top[0].0, "a"); // the replacement vector won
    let top = vs.search(&unit([1.0, 0.0, 0.0]), 3).unwrap();
    assert!(
        top.iter().filter(|(id, _)| id == "a").count() <= 1,
        "old version hidden"
    );
    assert!(
        vs.search(&[1.0, 0.0], 1).is_err(),
        "wrong dimension rejected"
    );
}
