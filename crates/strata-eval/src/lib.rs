//! # strata-eval
//!
//! Evaluation harness for strata: load BEIR datasets ([`beir`]), compute IR
//! metrics exactly like `trec_eval` ([`metrics`]), read/write TREC run files
//! and summarize latencies ([`harness`]).
//!
//! The `strata-eval` binary (`src/main.rs`) ties these together; the
//! `strata-bench` crate reuses them for the Tantivy baseline so both engines
//! are judged by identical code.

pub mod beir;
pub mod fusion;
pub mod harness;
pub mod metrics;
pub mod stats;
