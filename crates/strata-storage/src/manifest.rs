//! The manifest: the single source of truth for which files make up a
//! committed index.
//!
//! Committing writes `manifest.json.tmp`, fsyncs it, renames it over
//! `manifest.json` and fsyncs the directory. `rename` is atomic on POSIX file
//! systems, so after a crash the manifest is either entirely the old one or
//! entirely the new one. Files not referenced by it (half-written segments,
//! stale tombstone files) are garbage and are removed on open.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use strata_core::analyzer::AnalyzerConfig;

/// One segment as recorded in the manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestSegment {
    pub id: u32,
    pub num_docs: u32,
    /// Generation of the tombstone file, or 0 if no docs are deleted.
    pub del_gen: u64,
    pub num_deleted: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Incremented on every manifest write.
    pub generation: u64,
    /// Every WAL op with `seq <= committed_seq` is reflected in the segments
    /// and tombstones listed here. Recovery replays only newer ops.
    pub committed_seq: u64,
    pub next_segment_id: u32,
    pub analyzer: AnalyzerConfig,
    /// In index order (older first). Order matters: global doc order (and
    /// therefore tie-breaking) follows it.
    pub segments: Vec<ManifestSegment>,
}

pub const MANIFEST: &str = "manifest.json";

impl Manifest {
    pub fn load(dir: &Path) -> Result<Option<Manifest>> {
        let path = dir.join(MANIFEST);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    /// Atomically replace the manifest (write tmp, fsync, rename, fsync dir).
    pub fn store(&self, dir: &Path) -> Result<()> {
        let tmp = dir.join(format!("{MANIFEST}.tmp"));
        let mut f = File::create(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(self)?)?;
        f.sync_all()?;
        fs::rename(&tmp, dir.join(MANIFEST))?;
        File::open(dir)?.sync_all()?;
        Ok(())
    }
}
