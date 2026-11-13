//! Write-ahead log (WAL): every change is appended and fsynced *before* it is
//! acknowledged, so a crash can lose only un-acknowledged work.
//!
//! ```text
//! wal/wal-<first seq, 20 digits>.log    a sequence of records:
//!   len u32 | crc32(payload) u32 | payload
//!   payload = seq u64 | op u8 | varint len + id bytes | [varint len + text bytes]
//! ```
//!
//! *Group commit*: [`Wal::append`] writes a whole batch of records and calls
//! `sync_data` once, amortizing the expensive fsync over the batch.
//!
//! *Recovery*: [`replay`] reads all WAL files in order and returns records with
//! `seq > committed_seq` (older ones are already in segments). A crash in the
//! middle of `append` can leave a torn record at the end of the newest file;
//! a short read or checksum mismatch there marks the end of the log, and the
//! file is truncated to the last good record. Such a record was never
//! acknowledged (acks happen only after `sync_data` returns), so dropping it
//! loses nothing that a client was promised.
//!
//! A checksum error in an *older* file cannot be a torn write and is reported
//! as corruption instead of silently truncated.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::codec::{read_u32, read_u64, read_varint, write_varint};

/// One logged change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WalOp {
    /// Add (or replace, if the id exists) a document.
    Add { id: String, text: String },
    /// Delete a document by external id.
    Delete { id: String },
}

const OP_ADD: u8 = 1;
const OP_DELETE: u8 = 2;

fn file_name(first_seq: u64) -> String {
    format!("wal-{first_seq:020}.log")
}

fn encode(seq: u64, op: &WalOp, out: &mut Vec<u8>) {
    let mut payload = Vec::new();
    payload.extend_from_slice(&seq.to_le_bytes());
    match op {
        WalOp::Add { id, text } => {
            payload.push(OP_ADD);
            write_varint(id.len() as u64, &mut payload);
            payload.extend_from_slice(id.as_bytes());
            write_varint(text.len() as u64, &mut payload);
            payload.extend_from_slice(text.as_bytes());
        }
        WalOp::Delete { id } => {
            payload.push(OP_DELETE);
            write_varint(id.len() as u64, &mut payload);
            payload.extend_from_slice(id.as_bytes());
        }
    }
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
    out.extend_from_slice(&payload);
}

fn decode(payload: &[u8]) -> Result<(u64, WalOp)> {
    let seq = read_u64(payload, 0);
    let mut pos = 9;
    let read_str = |pos: &mut usize| -> Result<String> {
        let len = read_varint(payload, pos) as usize;
        let s = std::str::from_utf8(&payload[*pos..*pos + len])?.to_string();
        *pos += len;
        Ok(s)
    };
    let op = match payload[8] {
        OP_ADD => {
            let id = read_str(&mut pos)?;
            let text = read_str(&mut pos)?;
            WalOp::Add { id, text }
        }
        OP_DELETE => WalOp::Delete {
            id: read_str(&mut pos)?,
        },
        other => bail!("unknown WAL op {other}"),
    };
    Ok((seq, op))
}

/// An open WAL file being appended to.
pub struct Wal {
    dir: PathBuf,
    file: File,
    bytes_written: u64,
}

impl Wal {
    /// Start a new WAL file whose first record will have `first_seq`.
    pub fn create(dir: &Path, first_seq: u64) -> Result<Wal> {
        fs::create_dir_all(dir)?;
        let path = dir.join(file_name(first_seq));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        File::open(dir)?.sync_all()?; // make the new file's directory entry durable
        Ok(Wal {
            dir: dir.to_path_buf(),
            file,
            bytes_written: 0,
        })
    }

    /// Append `ops` with sequence numbers `first_seq, first_seq + 1, ...` and
    /// fsync. When this returns `Ok`, the ops survive a crash.
    pub fn append(&mut self, first_seq: u64, ops: &[WalOp]) -> Result<()> {
        let mut buf = Vec::new();
        for (i, op) in ops.iter().enumerate() {
            encode(first_seq + i as u64, op, &mut buf);
        }
        self.file.write_all(&buf)?;
        self.file.sync_data()?;
        self.bytes_written += buf.len() as u64;
        Ok(())
    }

    /// Bytes appended through this handle.
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Delete WAL files that only contain records `<= committed_seq`, i.e.
    /// every file older than the one starting at `committed_seq + 1`.
    pub fn remove_obsolete(&self, committed_seq: u64) -> Result<()> {
        for (first, path) in list_files(&self.dir)? {
            if first <= committed_seq {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }
}

fn list_files(dir: &Path) -> Result<Vec<(u64, PathBuf)>> {
    let mut files = Vec::new();
    if !dir.exists() {
        return Ok(files);
    }
    for e in fs::read_dir(dir)? {
        let path = e?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if let Some(num) = name
            .strip_prefix("wal-")
            .and_then(|n| n.strip_suffix(".log"))
        {
            files.push((num.parse::<u64>()?, path));
        }
    }
    files.sort();
    Ok(files)
}

/// What [`replay`] found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReplayReport {
    pub files: usize,
    pub records_total: u64,
    pub records_replayed: u64,
    /// Bytes cut off the end of the newest file (torn write).
    pub truncated_bytes: u64,
    /// Highest sequence number seen (0 if none).
    pub max_seq: u64,
}

/// Read all WAL files in `dir`; return ops with `seq > committed_seq`.
pub fn replay(dir: &Path, committed_seq: u64) -> Result<(Vec<(u64, WalOp)>, ReplayReport)> {
    let files = list_files(dir)?;
    let mut report = ReplayReport {
        files: files.len(),
        ..Default::default()
    };
    let mut ops = Vec::new();
    let mut last_seq = committed_seq;
    for (i, (_, path)) in files.iter().enumerate() {
        let is_last = i + 1 == files.len();
        let data = fs::read(path)?;
        let mut pos = 0;
        while pos < data.len() {
            let valid = pos + 8 <= data.len() && {
                let len = read_u32(&data, pos) as usize;
                pos + 8 + len <= data.len()
                    && crc32fast::hash(&data[pos + 8..pos + 8 + len]) == read_u32(&data, pos + 4)
            };
            if !valid {
                if !is_last {
                    bail!("corrupt WAL record in {} at byte {pos}", path.display());
                }
                // Torn tail: drop it so the next append starts from a clean end.
                report.truncated_bytes = (data.len() - pos) as u64;
                OpenOptions::new()
                    .write(true)
                    .open(path)?
                    .set_len(pos as u64)?;
                break;
            }
            let len = read_u32(&data, pos) as usize;
            let (seq, op) = decode(&data[pos + 8..pos + 8 + len])?;
            pos += 8 + len;
            report.records_total += 1;
            report.max_seq = report.max_seq.max(seq);
            if seq > committed_seq {
                if seq != last_seq + 1 {
                    bail!("WAL sequence gap: expected {}, found {seq}", last_seq + 1);
                }
                last_seq = seq;
                ops.push((seq, op));
            }
        }
    }
    report.records_replayed = ops.len() as u64;
    Ok((ops, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("strata-wal-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn add(id: &str) -> WalOp {
        WalOp::Add {
            id: id.into(),
            text: format!("text of {id} ünïcode"),
        }
    }

    #[test]
    fn append_and_replay() {
        let dir = tmp("basic");
        let mut w = Wal::create(&dir, 1).unwrap();
        w.append(1, &[add("a"), add("b")]).unwrap();
        w.append(3, &[WalOp::Delete { id: "a".into() }]).unwrap();
        let (ops, rep) = replay(&dir, 0).unwrap();
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[2], (3, WalOp::Delete { id: "a".into() }));
        assert_eq!(rep.max_seq, 3);
        // Already-committed records are skipped.
        let (ops, _) = replay(&dir, 2).unwrap();
        assert_eq!(ops, vec![(3, WalOp::Delete { id: "a".into() })]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn torn_tail_is_truncated() {
        let dir = tmp("torn");
        let mut w = Wal::create(&dir, 1).unwrap();
        w.append(1, &[add("a"), add("b")]).unwrap();
        let path = dir.join(file_name(1));
        let full = fs::read(&path).unwrap();
        // Simulate a crash mid-write: chop the last 5 bytes.
        fs::write(&path, &full[..full.len() - 5]).unwrap();
        let (ops, rep) = replay(&dir, 0).unwrap();
        assert_eq!(ops.len(), 1);
        assert!(rep.truncated_bytes > 0);
        // File now ends cleanly; a second replay sees no torn tail.
        let (_, rep2) = replay(&dir, 0).unwrap();
        assert_eq!(rep2.truncated_bytes, 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn corruption_in_old_file_is_an_error() {
        let dir = tmp("corrupt");
        let mut w = Wal::create(&dir, 1).unwrap();
        w.append(1, &[add("a")]).unwrap();
        let mut w2 = Wal::create(&dir, 2).unwrap();
        w2.append(2, &[add("b")]).unwrap();
        let path = dir.join(file_name(1));
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&path, &bytes).unwrap();
        assert!(replay(&dir, 0).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn remove_obsolete_keeps_newer_files() {
        let dir = tmp("rotate");
        let mut w = Wal::create(&dir, 1).unwrap();
        w.append(1, &[add("a"), add("b")]).unwrap();
        let w2 = Wal::create(&dir, 3).unwrap();
        w2.remove_obsolete(2).unwrap();
        let names: Vec<_> = list_files(&dir)
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, vec![3]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
