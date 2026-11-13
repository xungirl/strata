//! Int8 scalar quantization (SQ8).
//!
//! Each float `x` is stored as `q = round(x / scale * 127)` clamped to
//! [-127, 127], one byte instead of four. With one global `scale`,
//! `dot(a, b) ~= scale^2 / 127^2 * dot_i8(qa, qb)`, so the hot loop is an
//! integer dot product (4x less memory traffic, cheap NEON instructions).
//!
//! `scale` is a high quantile of |x| over the data rather than the max: a
//! few outlier coordinates would otherwise waste most of the 255 levels.
//! Coordinates beyond it are clipped. Because quantized similarities are
//! approximate, searches **rescore** their best candidates with the exact
//! f32 vectors ("oversampling + rescoring", as Qdrant and others do).
//!
//! Not implemented: per-dimension scales or product quantization (PQ).

use crate::distance::dot_i8;
use crate::vectors::Vectors;

#[derive(Clone, Debug)]
pub struct ScalarQuantizer {
    scale: f32,
    dim: usize,
    codes: Vec<i8>,
}

impl ScalarQuantizer {
    /// Fit `scale` as the `quantile` (e.g. 0.999) of |x| over all
    /// coordinates, then encode every vector.
    pub fn fit(vectors: &Vectors, quantile: f64) -> ScalarQuantizer {
        let mut abs: Vec<f32> = vectors.as_slice().iter().map(|x| x.abs()).collect();
        let idx = ((abs.len() as f64 - 1.0) * quantile).round() as usize;
        let (_, q, _) = abs.select_nth_unstable_by(idx, f32::total_cmp);
        let scale = q.max(f32::MIN_POSITIVE);
        let mut sq = ScalarQuantizer {
            scale,
            dim: vectors.dim(),
            codes: Vec::with_capacity(vectors.as_slice().len()),
        };
        for i in 0..vectors.len() {
            let code = sq.encode(vectors.get(i));
            sq.codes.extend_from_slice(&code);
        }
        sq
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Quantize one vector.
    pub fn encode(&self, v: &[f32]) -> Vec<i8> {
        v.iter()
            .map(|&x| (x / self.scale * 127.0).round().clamp(-127.0, 127.0) as i8)
            .collect()
    }

    /// Append an encoded vector (for incremental inserts).
    pub fn push(&mut self, v: &[f32]) {
        let code = self.encode(v);
        self.codes.extend_from_slice(&code);
    }

    #[inline]
    pub fn code(&self, i: usize) -> &[i8] {
        &self.codes[i * self.dim..(i + 1) * self.dim]
    }

    /// Approximate dot product between an encoded query and vector `i`.
    #[inline]
    pub fn approx_dot(&self, query_code: &[i8], i: usize) -> f32 {
        let s = self.scale / 127.0;
        dot_i8(query_code, self.code(i)) as f32 * s * s
    }

    /// Bytes of the codes.
    pub fn bytes(&self) -> usize {
        self.codes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distance::dot;

    #[test]
    fn approximation_error_is_small_for_unit_vectors() {
        // Deterministic pseudo-random unit vectors.
        let dim = 64;
        let mut seed = 7u64;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as f32 / (1u64 << 31) as f32) - 0.5
        };
        let mut data = Vec::new();
        for _ in 0..200 {
            let v: Vec<f32> = (0..dim).map(|_| next()).collect();
            let n = dot(&v, &v).sqrt();
            data.extend(v.iter().map(|x| x / n));
        }
        let vs = Vectors::new(dim, data);
        let sq = ScalarQuantizer::fit(&vs, 1.0);
        let q = vs.get(0);
        let qc = sq.encode(q);
        for i in 0..vs.len() {
            let err = (sq.approx_dot(&qc, i) - dot(q, vs.get(i))).abs();
            assert!(err < 0.02, "vector {i}: error {err}");
        }
    }
}
