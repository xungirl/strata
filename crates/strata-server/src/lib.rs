//! # strata-server
//!
//! A strata node as a network service (stage 6):
//!
//! | module          | responsibility                                                  |
//! |-----------------|-----------------------------------------------------------------|
//! | [`engine`]      | lexical index + vector store + lexical/dense/hybrid search     |
//! | [`vector_store`]| durable vector log + in-memory HNSW                            |
//! | [`http`]        | axum HTTP/JSON API                                             |
//! | [`grpc`]        | tonic gRPC API generated from `proto/strata.proto`             |
//! | [`metrics`]     | Prometheus metrics                                             |
//! | [`telemetry`]   | logs + OpenTelemetry tracing                                   |
//!
//! Binaries: `strata-server` (the node) and `strata-load` (load generator).

pub mod engine;
pub mod grpc;
pub mod http;
pub mod metrics;
pub mod telemetry;
pub mod vector_store;
