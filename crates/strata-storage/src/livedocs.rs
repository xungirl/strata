//! Tombstones: which documents of an immutable segment are still live.
//!
//! Segments are never rewritten, so a delete just sets a bit in a per-segment
//! bitset ("tombstone"). Deleted docs keep occupying space (and, like Lucene,
//! keep counting in `N`/`df` for scoring) until a merge drops them.
//!
//! File format (`deletes/del-<segment>-<generation>.bin`):
//! `b"STDL" | num_docs u32 | num_deleted u32 | words (u64 LE) ... | crc32 u32`.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::codec::{read_u32, read_u64};

/// A bitset with one bit per doc: 1 = deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveDocs {
    deleted: Vec<u64>,
    num_docs: u32,
    num_deleted: u32,
}

impl LiveDocs {
    /// All `num_docs` documents live.
    pub fn all_live(num_docs: u32) -> LiveDocs {
        LiveDocs {
            deleted: vec![0; (num_docs as usize).div_ceil(64)],
            num_docs,
            num_deleted: 0,
        }
    }

    pub fn num_docs(&self) -> u32 {
        self.num_docs
    }

    pub fn num_deleted(&self) -> u32 {
        self.num_deleted
    }

    pub fn num_live(&self) -> u32 {
        self.num_docs - self.num_deleted
    }

    #[inline]
    pub fn is_live(&self, doc: u32) -> bool {
        self.deleted[(doc / 64) as usize] & (1u64 << (doc % 64)) == 0
    }

    /// Mark `doc` deleted; returns true if it was live.
    pub fn delete(&mut self, doc: u32) -> bool {
        let (w, bit) = ((doc / 64) as usize, 1u64 << (doc % 64));
        if self.deleted[w] & bit != 0 {
            return false;
        }
        self.deleted[w] |= bit;
        self.num_deleted += 1;
        true
    }

    /// Durably write to `path` (write, fsync).
    pub fn write(&self, path: &Path) -> Result<()> {
        let mut buf = Vec::with_capacity(12 + self.deleted.len() * 8 + 4);
        buf.extend_from_slice(b"STDL");
        buf.extend_from_slice(&self.num_docs.to_le_bytes());
        buf.extend_from_slice(&self.num_deleted.to_le_bytes());
        for w in &self.deleted {
            buf.extend_from_slice(&w.to_le_bytes());
        }
        let crc = crc32fast::hash(&buf);
        buf.extend_from_slice(&crc.to_le_bytes());
        let mut f = File::create(path).with_context(|| format!("create {}", path.display()))?;
        f.write_all(&buf)?;
        f.sync_all()?;
        Ok(())
    }

    /// Read a file written by [`write`](Self::write), verifying its checksum.
    pub fn read(path: &Path) -> Result<LiveDocs> {
        let buf = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        if buf.len() < 16 || &buf[..4] != b"STDL" {
            bail!("{}: not a deletes file", path.display());
        }
        let body = &buf[..buf.len() - 4];
        if crc32fast::hash(body) != read_u32(&buf, buf.len() - 4) {
            bail!("{}: checksum mismatch", path.display());
        }
        let num_docs = read_u32(&buf, 4);
        let num_deleted = read_u32(&buf, 8);
        let words = (num_docs as usize).div_ceil(64);
        let deleted = (0..words).map(|i| read_u64(&buf, 12 + i * 8)).collect();
        Ok(LiveDocs {
            deleted,
            num_docs,
            num_deleted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_and_roundtrip() {
        let mut l = LiveDocs::all_live(130);
        assert!(l.is_live(0) && l.is_live(129));
        assert!(l.delete(3));
        assert!(!l.delete(3)); // idempotent
        assert!(l.delete(129));
        assert_eq!((l.num_deleted(), l.num_live()), (2, 128));
        let path = std::env::temp_dir().join(format!("strata-livedocs-{}.bin", std::process::id()));
        l.write(&path).unwrap();
        let back = LiveDocs::read(&path).unwrap();
        assert_eq!(back, l);
        // Corrupt one byte -> checksum error.
        let mut bytes = fs::read(&path).unwrap();
        bytes[14] ^= 0xff;
        fs::write(&path, &bytes).unwrap();
        assert!(LiveDocs::read(&path).is_err());
        fs::remove_file(&path).unwrap();
    }
}
