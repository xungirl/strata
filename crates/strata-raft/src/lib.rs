//! # strata-raft
//!
//! Raft-replicated strata shards (stage 8), implemented from the paper:
//!
//! | module      | responsibility                                                     |
//! |-------------|--------------------------------------------------------------------|
//! | [`core`]    | the Raft algorithm as a pure, deterministic state machine          |
//! | [`storage`] | durable hard state + log (fsync, torn-tail recovery)               |
//! | [`server`]  | HTTP transport, replicated strata `Index` state machine, ReadIndex |
//!
//! Binaries: `strata-raft-node` (one replica) and `strata-raft-chaos`
//! (leader-kill chaos test).

pub mod core;
pub mod server;
pub mod storage;
