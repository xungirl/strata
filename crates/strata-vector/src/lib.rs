//! # strata-vector
//!
//! Dense-vector search (stage 4):
//!
//! | module       | responsibility                                                |
//! |--------------|---------------------------------------------------------------|
//! | [`distance`] | dot products: scalar + NEON SIMD (f32 and int8)               |
//! | [`vectors`]  | contiguous storage of n x dim f32 vectors                     |
//! | [`quant`]    | int8 scalar quantization with rescoring                       |
//! | [`flat`]     | exact brute-force top-k (ground truth, filter fallback)       |
//! | [`hnsw`]     | the HNSW graph index: build, search, filtered and SQ search   |
//! | [`embeddings`] | loading the cached BEIR embeddings                          |

pub mod distance;
pub mod embeddings;
pub mod flat;
pub mod hnsw;
pub mod quant;
pub mod vectors;

pub use flat::{exact_top_k, Neighbor};
pub use hnsw::{Hnsw, HnswConfig, SearchContext};
pub use quant::ScalarQuantizer;
pub use vectors::Vectors;
