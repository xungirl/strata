//! HNSW: Hierarchical Navigable Small World graphs (Malkov & Yashunin, 2016).
//!
//! An approximate nearest-neighbour index. Every vector is a node; each node
//! links to up to `M` "good" neighbours (`2M` on the bottom layer). Nodes are
//! also assigned a random top layer (geometric distribution), so upper layers
//! are sparse "express lanes":
//!
//! ```text
//! layer 2:  A ─────────────── F                 few nodes, long links
//! layer 1:  A ──── C ──── E ── F ──── H
//! layer 0:  A─B─C─D─E─F─G─H─I─J─K ...           every node, short links
//! ```
//!
//! Search: start at the entry point on the top layer, greedily walk to the
//! neighbour most similar to the query, drop a layer, repeat; on layer 0 run a
//! best-first search keeping the `ef` best candidates (beam width). Larger `ef`
//! = higher recall, slower. Insertion runs the same search with
//! `ef_construction` and links the new node to neighbours chosen by the
//! paper's *heuristic* (Algorithm 4): keep a candidate only if it is closer to
//! the new node than to every neighbour already kept, which spreads links in
//! different directions and keeps the graph navigable.
//!
//! This implementation follows the paper and hnswlib's choices (heuristic
//! selection without "keepPrunedConnections", M0 = 2M, mL = 1/ln M), is
//! single-threaded and deterministic for a given seed.
//! Similarity is the dot product of normalized vectors (cosine).

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{bail, Result};

use crate::distance::dot;
use crate::flat::{exact_top_k, Neighbor};
use crate::quant::ScalarQuantizer;
use crate::vectors::Vectors;

#[derive(Clone, Copy, Debug)]
pub struct HnswConfig {
    /// Max links per node on layers >= 1 (layer 0 allows 2M).
    pub m: usize,
    /// Beam width while inserting.
    pub ef_construction: usize,
    /// RNG seed for layer assignment (determinism).
    pub seed: u64,
}

impl Default for HnswConfig {
    fn default() -> Self {
        HnswConfig {
            m: 16,
            ef_construction: 200,
            seed: 42,
        }
    }
}

/// Reusable per-search scratch: "visited" marks stamped with an epoch, so a
/// search never has to clear an n-sized array.
pub struct SearchContext {
    marks: Vec<u32>,
    epoch: u32,
    /// Similarity evaluations done by the last search.
    pub distance_evals: usize,
}

impl SearchContext {
    pub fn new(n: usize) -> SearchContext {
        SearchContext {
            marks: vec![0; n],
            epoch: 0,
            distance_evals: 0,
        }
    }

    fn reset(&mut self, n: usize) {
        if self.marks.len() < n {
            self.marks.resize(n, 0);
        }
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.marks.fill(0);
            self.epoch = 1;
        }
        self.distance_evals = 0;
    }

    /// Mark `id` visited; returns true if it was not visited before.
    #[inline]
    fn visit(&mut self, id: u32) -> bool {
        let m = &mut self.marks[id as usize];
        if *m == self.epoch {
            false
        } else {
            *m = self.epoch;
            true
        }
    }
}

pub struct Hnsw {
    m: usize,
    m0: usize,
    ef_construction: usize,
    ml: f64,
    /// links[node][layer] = neighbour ids.
    links: Vec<Vec<Vec<u32>>>,
    entry: Option<u32>,
    max_level: usize,
    rng: u64,
}

impl Hnsw {
    pub fn new(config: HnswConfig) -> Hnsw {
        assert!(config.m >= 2);
        Hnsw {
            m: config.m,
            m0: 2 * config.m,
            ef_construction: config.ef_construction.max(config.m),
            ml: 1.0 / (config.m as f64).ln(),
            links: Vec::new(),
            entry: None,
            max_level: 0,
            rng: config.seed | 1,
        }
    }

    /// Build over all `vectors` (ids 0..n), inserting in order.
    pub fn build(vectors: &Vectors, config: HnswConfig) -> Hnsw {
        let mut h = Hnsw::new(config);
        let mut ctx = SearchContext::new(vectors.len());
        for i in 0..vectors.len() {
            h.insert(vectors, i as u32, &mut ctx);
        }
        h
    }

    pub fn len(&self) -> usize {
        self.links.len()
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// Uniform (0, 1] from xorshift64*.
    fn uniform(&mut self) -> f64 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let x = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11; // 53 bits
        (x as f64 + 1.0) / (1u64 << 53) as f64
    }

    fn max_links(&self, layer: usize) -> usize {
        if layer == 0 {
            self.m0
        } else {
            self.m
        }
    }

    /// Insert vector `id` (must be the next id: `id == self.len()`).
    pub fn insert(&mut self, vectors: &Vectors, id: u32, ctx: &mut SearchContext) {
        assert_eq!(
            id as usize,
            self.links.len(),
            "ids must be inserted densely in order"
        );
        let level = (-self.uniform().ln() * self.ml).floor() as usize;
        self.links.push(vec![Vec::new(); level + 1]);
        let q = vectors.get(id as usize);
        let sim = |j: u32| dot(q, vectors.get(j as usize));

        let Some(entry) = self.entry else {
            self.entry = Some(id);
            self.max_level = level;
            return;
        };
        ctx.reset(self.links.len());
        // Greedy descent through layers above the new node's level.
        let mut ep = Neighbor {
            id: entry,
            sim: sim(entry),
        };
        for layer in (level + 1..=self.max_level).rev() {
            ep = self.greedy(ep, layer, &sim, ctx);
        }
        let mut eps = vec![ep];
        for layer in (0..=level.min(self.max_level)).rev() {
            ctx.reset(self.links.len());
            let mut found = self.search_layer(&eps, self.ef_construction, layer, &sim, None, ctx);
            found.sort_by(|a, b| b.cmp(a));
            let chosen = self.select_heuristic(vectors, &found, self.max_links(layer));
            self.links[id as usize][layer] = chosen.iter().map(|n| n.id).collect();
            for n in &chosen {
                self.add_link(vectors, n.id, id, layer);
            }
            eps = found;
        }
        if level > self.max_level {
            self.max_level = level;
            self.entry = Some(id);
        }
    }

    /// Add `new` to `node`'s links on `layer`, re-pruning with the heuristic
    /// if the node has too many.
    fn add_link(&mut self, vectors: &Vectors, node: u32, new: u32, layer: usize) {
        let max = self.max_links(layer);
        let list = &mut self.links[node as usize][layer];
        if list.len() < max {
            list.push(new);
            return;
        }
        let base = vectors.get(node as usize);
        let mut cands: Vec<Neighbor> = list
            .iter()
            .chain(std::iter::once(&new))
            .map(|&j| Neighbor {
                id: j,
                sim: dot(base, vectors.get(j as usize)),
            })
            .collect();
        cands.sort_by(|a, b| b.cmp(a));
        let kept = self.select_heuristic(vectors, &cands, max);
        self.links[node as usize][layer] = kept.into_iter().map(|n| n.id).collect();
    }

    /// Paper's Algorithm 4: `cands` sorted best-first by similarity to the
    /// base point; keep `c` only if it is at least as similar to the base
    /// as to every already-kept neighbour.
    fn select_heuristic(&self, vectors: &Vectors, cands: &[Neighbor], m: usize) -> Vec<Neighbor> {
        let mut kept: Vec<Neighbor> = Vec::with_capacity(m);
        for c in cands {
            if kept.len() >= m {
                break;
            }
            let cv = vectors.get(c.id as usize);
            if kept
                .iter()
                .all(|r| dot(cv, vectors.get(r.id as usize)) <= c.sim)
            {
                kept.push(*c);
            }
        }
        kept
    }

    /// Walk to the most similar neighbour until no neighbour improves.
    fn greedy<F: Fn(u32) -> f32>(
        &self,
        mut cur: Neighbor,
        layer: usize,
        sim: &F,
        ctx: &mut SearchContext,
    ) -> Neighbor {
        loop {
            let mut improved = false;
            for &nb in &self.links[cur.id as usize][layer] {
                ctx.distance_evals += 1;
                let s = sim(nb);
                if s > cur.sim {
                    cur = Neighbor { id: nb, sim: s };
                    improved = true;
                }
            }
            if !improved {
                return cur;
            }
        }
    }

    /// Best-first beam search on one layer. Returns up to `ef` best nodes
    /// (unordered) among those passing `allow`; nodes failing `allow` are
    /// still traversed, so the graph stays connected under filters.
    fn search_layer<F: Fn(u32) -> f32>(
        &self,
        eps: &[Neighbor],
        ef: usize,
        layer: usize,
        sim: &F,
        allow: Option<&dyn Fn(u32) -> bool>,
        ctx: &mut SearchContext,
    ) -> Vec<Neighbor> {
        let ok = |id: u32| allow.is_none_or(|f| f(id));
        let mut candidates: BinaryHeap<Neighbor> = BinaryHeap::new(); // best first
        let mut results: BinaryHeap<Reverse<Neighbor>> = BinaryHeap::new(); // worst first
        for &ep in eps {
            if ctx.visit(ep.id) {
                candidates.push(ep);
                if ok(ep.id) {
                    results.push(Reverse(ep));
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }
        while let Some(c) = candidates.pop() {
            if results.len() >= ef && c.sim < results.peek().map_or(f32::MIN, |r| r.0.sim) {
                break; // the best remaining candidate is worse than our worst result
            }
            for &nb in &self.links[c.id as usize][layer] {
                if !ctx.visit(nb) {
                    continue;
                }
                ctx.distance_evals += 1;
                let s = sim(nb);
                let worst = results.peek().map_or(f32::MIN, |r| r.0.sim);
                if results.len() < ef || s > worst {
                    let n = Neighbor { id: nb, sim: s };
                    candidates.push(n);
                    if ok(nb) {
                        results.push(Reverse(n));
                        if results.len() > ef {
                            results.pop();
                        }
                    }
                }
            }
        }
        results.into_iter().map(|r| r.0).collect()
    }

    fn descend<F: Fn(u32) -> f32>(&self, sim: &F, ctx: &mut SearchContext) -> Option<Neighbor> {
        let entry = self.entry?;
        let mut ep = Neighbor {
            id: entry,
            sim: sim(entry),
        };
        for layer in (1..=self.max_level).rev() {
            ep = self.greedy(ep, layer, sim, ctx);
        }
        Some(ep)
    }

    /// Approximate top-k by exact f32 similarity with beam width `ef`.
    pub fn search(
        &self,
        vectors: &Vectors,
        query: &[f32],
        k: usize,
        ef: usize,
        allow: Option<&dyn Fn(u32) -> bool>,
        ctx: &mut SearchContext,
    ) -> Vec<Neighbor> {
        ctx.reset(self.links.len());
        let sim = |j: u32| dot(query, vectors.get(j as usize));
        let Some(ep) = self.descend(&sim, ctx) else {
            return Vec::new();
        };
        let mut found = self.search_layer(&[ep], ef.max(k), 0, &sim, allow, ctx);
        found.sort_by(|a, b| b.cmp(a));
        found.truncate(k);
        found
    }

    /// Like [`search`](Self::search) but the graph is traversed with int8
    /// similarities; the `ef` best candidates are then rescored with exact
    /// f32 similarity and the top `k` returned.
    #[allow(clippy::too_many_arguments)]
    pub fn search_sq(
        &self,
        sq: &ScalarQuantizer,
        vectors: &Vectors,
        query: &[f32],
        k: usize,
        ef: usize,
        allow: Option<&dyn Fn(u32) -> bool>,
        ctx: &mut SearchContext,
    ) -> Vec<Neighbor> {
        ctx.reset(self.links.len());
        let code = sq.encode(query);
        let sim = |j: u32| sq.approx_dot(&code, j as usize);
        let Some(ep) = self.descend(&sim, ctx) else {
            return Vec::new();
        };
        let found = self.search_layer(&[ep], ef.max(k), 0, &sim, allow, ctx);
        let mut rescored: Vec<Neighbor> = found
            .into_iter()
            .map(|n| Neighbor {
                id: n.id,
                sim: dot(query, vectors.get(n.id as usize)),
            })
            .collect();
        rescored.sort_by(|a, b| b.cmp(a));
        rescored.truncate(k);
        rescored
    }

    /// Filtered search: HNSW with the filter applied during traversal, or
    /// exact search over the allowed vectors when fewer than
    /// `brute_force_below` of them pass (graph search degenerates when
    /// almost every node is filtered out, while a scan of a few vectors is
    /// cheap).
    #[allow(clippy::too_many_arguments)]
    pub fn search_filtered(
        &self,
        vectors: &Vectors,
        query: &[f32],
        k: usize,
        ef: usize,
        allow: &dyn Fn(u32) -> bool,
        allowed_count: usize,
        brute_force_below: usize,
        ctx: &mut SearchContext,
    ) -> Vec<Neighbor> {
        if allowed_count < brute_force_below {
            ctx.distance_evals = allowed_count;
            return exact_top_k(vectors, query, k, allow);
        }
        self.search(vectors, query, k, ef, Some(allow), ctx)
    }

    /// Approximate heap bytes of the graph links.
    pub fn graph_bytes(&self) -> usize {
        self.links
            .iter()
            .map(|layers| 24 + layers.iter().map(|l| 24 + l.capacity() * 4).sum::<usize>())
            .sum()
    }

    /// Average number of layer-0 links per node.
    pub fn avg_degree0(&self) -> f64 {
        self.links.iter().map(|l| l[0].len()).sum::<usize>() as f64 / self.links.len().max(1) as f64
    }

    pub fn max_level(&self) -> usize {
        self.max_level
    }

    /// Serialize: header + per node (levels, then per layer: count, ids).
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut out = Vec::new();
        out.extend_from_slice(b"STHNSW01");
        for v in [
            self.m as u64,
            self.ef_construction as u64,
            self.links.len() as u64,
            self.max_level as u64,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.entry.map_or(u32::MAX, |e| e).to_le_bytes());
        out.extend_from_slice(&self.rng.to_le_bytes());
        for layers in &self.links {
            out.push(layers.len() as u8);
            for l in layers {
                out.extend_from_slice(&(l.len() as u32).to_le_bytes());
                for id in l {
                    out.extend_from_slice(&id.to_le_bytes());
                }
            }
        }
        std::fs::File::create(path)?.write_all(&out)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Hnsw> {
        let mut buf = Vec::new();
        std::fs::File::open(path)?.read_to_end(&mut buf)?;
        if buf.len() < 52 || &buf[..8] != b"STHNSW01" {
            bail!("{}: not an HNSW file", path.display());
        }
        let u64_at = |p: usize| u64::from_le_bytes(buf[p..p + 8].try_into().unwrap());
        let u32_at = |p: usize| u32::from_le_bytes(buf[p..p + 4].try_into().unwrap());
        let m = u64_at(8) as usize;
        let ef_c = u64_at(16) as usize;
        let n = u64_at(24) as usize;
        let max_level = u64_at(32) as usize;
        let entry = u32_at(40);
        let rng = u64_at(44);
        let mut pos = 52;
        let mut links = Vec::with_capacity(n);
        for _ in 0..n {
            let nl = buf[pos] as usize;
            pos += 1;
            let mut layers = Vec::with_capacity(nl);
            for _ in 0..nl {
                let c = u32_at(pos) as usize;
                pos += 4;
                layers.push((0..c).map(|i| u32_at(pos + 4 * i)).collect());
                pos += 4 * c;
            }
            links.push(layers);
        }
        let mut h = Hnsw::new(HnswConfig {
            m,
            ef_construction: ef_c,
            seed: 1,
        });
        h.links = links;
        h.max_level = max_level;
        h.entry = (entry != u32::MAX).then_some(entry);
        h.rng = rng;
        Ok(h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_unit_vectors(n: usize, dim: usize, seed: u64) -> Vectors {
        let mut s = seed;
        let mut next = || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((s >> 33) as f32 / (1u64 << 31) as f32) - 0.5
        };
        let mut data = Vec::with_capacity(n * dim);
        for _ in 0..n {
            let v: Vec<f32> = (0..dim).map(|_| next()).collect();
            let norm = dot(&v, &v).sqrt();
            data.extend(v.iter().map(|x| x / norm));
        }
        Vectors::new(dim, data)
    }

    fn recall(h: &Hnsw, data: &Vectors, queries: &Vectors, ef: usize) -> f64 {
        let mut ctx = SearchContext::new(data.len());
        let mut hit = 0;
        for i in 0..queries.len() {
            let q = queries.get(i);
            let truth: Vec<u32> = exact_top_k(data, q, 10, &|_| true)
                .iter()
                .map(|n| n.id)
                .collect();
            let got = h.search(data, q, 10, ef, None, &mut ctx);
            hit += got.iter().filter(|n| truth.contains(&n.id)).count();
        }
        hit as f64 / (10 * queries.len()) as f64
    }

    #[test]
    fn high_recall_on_random_data_and_monotone_in_ef() {
        let data = random_unit_vectors(3000, 32, 1);
        let queries = random_unit_vectors(100, 32, 2);
        let h = Hnsw::build(&data, HnswConfig::default());
        let r10 = recall(&h, &data, &queries, 10);
        let r200 = recall(&h, &data, &queries, 200);
        assert!(r200 > 0.97, "recall@10 with ef=200 was {r200}");
        assert!(r200 >= r10);
        assert!(h.avg_degree0() <= 32.0);
    }

    #[test]
    fn filtered_search_returns_only_allowed() {
        let data = random_unit_vectors(2000, 16, 3);
        let h = Hnsw::build(&data, HnswConfig::default());
        let mut ctx = SearchContext::new(data.len());
        let allow = |id: u32| id.is_multiple_of(7);
        let q = data.get(5);
        let got = h.search(&data, q, 10, 100, Some(&allow), &mut ctx);
        assert_eq!(got.len(), 10);
        assert!(got.iter().all(|n| n.id.is_multiple_of(7)));
        // Brute-force fallback for a very selective filter is exact.
        let rare = |id: u32| id.is_multiple_of(500);
        let got = h.search_filtered(&data, q, 3, 100, &rare, 4, 1000, &mut ctx);
        let want = exact_top_k(&data, q, 3, &rare);
        assert_eq!(got, want);
    }

    #[test]
    fn deterministic_and_save_load_roundtrip() {
        let data = random_unit_vectors(500, 8, 4);
        let a = Hnsw::build(&data, HnswConfig::default());
        let b = Hnsw::build(&data, HnswConfig::default());
        assert_eq!(a.links, b.links);
        let path = std::env::temp_dir().join(format!("strata-hnsw-{}.bin", std::process::id()));
        a.save(&path).unwrap();
        let c = Hnsw::load(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(a.links, c.links);
        assert_eq!(a.entry, c.entry);
        let mut ctx = SearchContext::new(data.len());
        assert_eq!(
            a.search(&data, data.get(1), 5, 50, None, &mut ctx),
            c.search(&data, data.get(1), 5, 50, None, &mut ctx)
        );
    }

    #[test]
    fn sq_search_has_high_recall() {
        let data = random_unit_vectors(3000, 32, 5);
        let queries = random_unit_vectors(50, 32, 6);
        let h = Hnsw::build(&data, HnswConfig::default());
        let sq = ScalarQuantizer::fit(&data, 0.999);
        let mut ctx = SearchContext::new(data.len());
        let mut hit = 0;
        for i in 0..queries.len() {
            let q = queries.get(i);
            let truth: Vec<u32> = exact_top_k(&data, q, 10, &|_| true)
                .iter()
                .map(|n| n.id)
                .collect();
            hit += h
                .search_sq(&sq, &data, q, 10, 200, None, &mut ctx)
                .iter()
                .filter(|n| truth.contains(&n.id))
                .count();
        }
        let r = hit as f64 / 500.0;
        assert!(r > 0.95, "SQ recall {r}");
    }
}
