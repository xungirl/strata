//! Durable Raft state: the hard state (term, vote) and the log.
//!
//! ```text
//! <dir>/hard_state.json   replaced atomically (write tmp, fsync, rename, fsync dir)
//! <dir>/raft.log          records: len u32 | crc32 u32 | JSON(Entry)
//! ```
//!
//! [`RaftStorage::persist`] applies one `Ready`'s storage part: write the
//! hard state, then truncate a conflicting suffix, append new entries, fsync.
//! Only after it returns may the node send the `Ready`'s messages.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::core::{Entry, HardState, Ready};

pub struct RaftStorage {
    dir: PathBuf,
    log: File,
    /// Byte offset where entry `i + 1` starts, plus the end offset.
    offsets: Vec<u64>,
}

impl RaftStorage {
    /// Open `dir`, returning the storage plus the recovered hard state and log.
    pub fn open(dir: &Path) -> Result<(RaftStorage, HardState, Vec<Entry>)> {
        fs::create_dir_all(dir)?;
        let hs_path = dir.join("hard_state.json");
        let hs = if hs_path.exists() {
            serde_json::from_slice(&fs::read(&hs_path)?)?
        } else {
            HardState::default()
        };
        let log_path = dir.join("raft.log");
        let data = if log_path.exists() {
            fs::read(&log_path)?
        } else {
            Vec::new()
        };
        let mut entries = Vec::new();
        let mut offsets = vec![0u64];
        let mut pos = 0usize;
        while pos + 8 <= data.len() {
            let len = u32::from_le_bytes(data[pos..pos + 4].try_into()?) as usize;
            let crc = u32::from_le_bytes(data[pos + 4..pos + 8].try_into()?);
            if pos + 8 + len > data.len() || crc32fast::hash(&data[pos + 8..pos + 8 + len]) != crc {
                break; // torn tail: never acknowledged to anyone
            }
            let e: Entry = serde_json::from_slice(&data[pos + 8..pos + 8 + len])?;
            if e.index != entries.len() as u64 + 1 {
                bail!("raft log not dense at entry {}", e.index);
            }
            entries.push(e);
            pos += 8 + len;
            offsets.push(pos as u64);
        }
        let log = OpenOptions::new()
            .create(true)
            .truncate(false) // keep the log; a torn tail is cut below
            .read(true)
            .write(true)
            .open(&log_path)
            .with_context(|| format!("open {}", log_path.display()))?;
        log.set_len(pos as u64)?; // drop a torn tail
        let s = RaftStorage {
            dir: dir.to_path_buf(),
            log,
            offsets,
        };
        s.log.sync_all()?;
        File::open(dir)?.sync_all()?;
        Ok((s, hs, entries))
    }

    /// Durably apply the storage part of a `Ready` (before sending messages).
    pub fn persist(&mut self, ready: &Ready) -> Result<()> {
        // Hard state first: the durable term must never be lower than the
        // term of a durable log entry (a crash between the two writes then
        // leaves a higher term with fewer entries, which is always safe).
        if let Some(hs) = ready.hard_state {
            let tmp = self.dir.join("hard_state.json.tmp");
            let mut f = File::create(&tmp)?;
            f.write_all(&serde_json::to_vec(&hs)?)?;
            f.sync_all()?;
            fs::rename(&tmp, self.dir.join("hard_state.json"))?;
            File::open(&self.dir)?.sync_all()?;
        }
        let mut dirty = false;
        if let Some(t) = ready.truncate_from {
            let keep = (t as usize - 1).min(self.offsets.len() - 1);
            self.offsets.truncate(keep + 1);
            self.log.set_len(self.offsets[keep])?;
            dirty = true;
        }
        if !ready.append.is_empty() {
            let mut buf = Vec::new();
            let mut end = *self.offsets.last().unwrap();
            for e in &ready.append {
                assert_eq!(
                    e.index as usize,
                    self.offsets.len(),
                    "append must continue the log"
                );
                let payload = serde_json::to_vec(e)?;
                buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                buf.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
                buf.extend_from_slice(&payload);
                end += 8 + payload.len() as u64;
                self.offsets.push(end);
            }
            use std::io::{Seek, SeekFrom};
            self.log.seek(SeekFrom::Start(end - buf.len() as u64))?;
            self.log.write_all(&buf)?;
            dirty = true;
        }
        if dirty {
            self.log.sync_data()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(term: u64, index: u64) -> Entry {
        Entry {
            term,
            index,
            data: format!("cmd{index}").into_bytes(),
        }
    }

    #[test]
    fn persist_reopen_truncate_and_torn_tail() {
        let dir = std::env::temp_dir().join(format!("strata-raft-storage-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let (mut s, hs, log) = RaftStorage::open(&dir).unwrap();
        assert_eq!((hs, log.len()), (HardState::default(), 0));
        let mut r = Ready {
            hard_state: Some(HardState {
                term: 2,
                voted_for: Some(3),
            }),
            append: (1..=4).map(|i| entry(1, i)).collect(),
            ..Ready::default()
        };
        s.persist(&r).unwrap();
        // Conflict: replace entries 3.. with term-2 entries.
        r = Ready {
            truncate_from: Some(3),
            append: vec![entry(2, 3)],
            ..Ready::default()
        };
        s.persist(&r).unwrap();
        drop(s);
        // A torn record at the tail (crash mid-write) is dropped on open.
        let mut f = OpenOptions::new()
            .append(true)
            .open(dir.join("raft.log"))
            .unwrap();
        f.write_all(&[7, 0, 0, 0, 1, 2]).unwrap();
        drop(f);
        let (_, hs, log) = RaftStorage::open(&dir).unwrap();
        assert_eq!(
            hs,
            HardState {
                term: 2,
                voted_for: Some(3)
            }
        );
        assert_eq!(log, vec![entry(1, 1), entry(1, 2), entry(2, 3)]);
        let _ = fs::remove_dir_all(&dir);
    }
}
