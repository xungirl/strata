//! Exact (brute-force) k-nearest-neighbour search: the ground truth that
//! HNSW recall is measured against, and the fallback for very selective
//! filters.

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

use crate::distance::dot;
use crate::vectors::Vectors;

/// (vector index, similarity), ordered by similarity then *lower* index
/// first, so results are deterministic under ties.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Neighbor {
    pub id: u32,
    pub sim: f32,
}

impl Eq for Neighbor {}
impl Ord for Neighbor {
    fn cmp(&self, o: &Self) -> Ordering {
        self.sim.total_cmp(&o.sim).then_with(|| o.id.cmp(&self.id))
    }
}
impl PartialOrd for Neighbor {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// Exact top-k by dot product among vectors with `allow(id)`; best first.
pub fn exact_top_k(
    vectors: &Vectors,
    query: &[f32],
    k: usize,
    allow: &dyn Fn(u32) -> bool,
) -> Vec<Neighbor> {
    let mut heap: BinaryHeap<Reverse<Neighbor>> = BinaryHeap::with_capacity(k + 1);
    for i in 0..vectors.len() {
        let id = i as u32;
        if !allow(id) {
            continue;
        }
        let n = Neighbor {
            id,
            sim: dot(query, vectors.get(i)),
        };
        if heap.len() < k {
            heap.push(Reverse(n));
        } else if let Some(mut worst) = heap.peek_mut() {
            if n > worst.0 {
                *worst = Reverse(n);
            }
        }
    }
    let mut out: Vec<Neighbor> = heap.into_iter().map(|r| r.0).collect();
    out.sort_by(|a, b| b.cmp(a));
    out
}
