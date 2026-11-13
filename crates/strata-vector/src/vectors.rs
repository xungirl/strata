//! A dense set of fixed-dimension f32 vectors, loaded from the `.f32`
//! files written by `scripts/embed_beir.py` (row-major, little-endian).

use std::path::Path;

use anyhow::{bail, Context, Result};

/// `n` vectors of `dim` floats in one contiguous buffer (row `i` is
/// `data[i * dim .. (i + 1) * dim]`): one allocation, cache-friendly scans.
#[derive(Clone, Debug)]
pub struct Vectors {
    dim: usize,
    data: Vec<f32>,
}

impl Vectors {
    pub fn new(dim: usize, data: Vec<f32>) -> Vectors {
        assert!(
            dim > 0 && data.len().is_multiple_of(dim),
            "data length must be a multiple of dim"
        );
        Vectors { dim, data }
    }

    /// Read a raw little-endian f32 file with `dim` columns.
    pub fn load(path: &Path, dim: usize) -> Result<Vectors> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        if bytes.len() % (4 * dim) != 0 {
            bail!(
                "{}: size {} is not a multiple of 4 * {dim}",
                path.display(),
                bytes.len()
            );
        }
        let data = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        Ok(Vectors { dim, data })
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn len(&self) -> usize {
        self.data.len() / self.dim
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    #[inline]
    pub fn get(&self, i: usize) -> &[f32] {
        &self.data[i * self.dim..(i + 1) * self.dim]
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.data
    }

    /// Append one vector.
    pub fn push(&mut self, v: &[f32]) {
        assert_eq!(v.len(), self.dim);
        self.data.extend_from_slice(v);
    }

    /// Heap bytes of the vector data.
    pub fn bytes(&self) -> usize {
        self.data.len() * 4
    }
}
