//! The durable, multi-segment index: writer, commits, recovery, merges and
//! point-in-time snapshots.
//!
//! ```text
//! index_dir/
//!   manifest.json            committed state (see manifest.rs)
//!   segments/seg-000001/...  immutable segments (see segment.rs)
//!   deletes/del-000001-0000000000000003.bin   tombstones per (segment, generation)
//!   wal/wal-...log           write-ahead log since the last commit
//! ```
//!
//! Write path: `add_documents` / `delete` -> append to WAL + fsync (= ack)
//! -> apply to the in-memory buffer and tombstones. `commit` flushes the
//! buffer into a new segment, writes changed tombstone files and atomically
//! replaces the manifest; after that the WAL before the commit point is
//! deleted.
//!
//! Read path: `snapshot()` returns an `Arc<Snapshot>`, an immutable list of
//! (segment, tombstones) pairs. Readers keep using their snapshot no matter
//! what the writer or merger does afterwards ("snapshot isolation" for
//! reads). Segments merged away are deleted from disk only when the last
//! snapshot referencing them is dropped (reference counting via `Arc`).
//!
//! Concurrency: one writer process per index directory (not enforced with a
//! lock file in this stage). Inside the process the writer state is behind a
//! `Mutex`; the current snapshot behind an `RwLock<Arc<Snapshot>>`, so
//! taking a snapshot is a cheap reference-count bump.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use strata_core::analyzer::{Analyzer, AnalyzerConfig};
use strata_core::index::SegmentReader;
use strata_core::types::SegmentId;

use crate::buffer::WriteBuffer;
use crate::fields::{FieldKind, Fields};
use crate::livedocs::LiveDocs;
use crate::manifest::{Manifest, ManifestSegment};
use crate::merge::{merge_segments, DELETED};
use crate::segment::{segment_dir_name, DiskSegment};
use crate::snapshot::Snapshot;
use crate::wal::{replay, ReplayReport, Wal, WalOp};

/// A document to index: external id, analyzed text, metadata fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    pub id: String,
    pub text: String,
    pub fields: Fields,
}

/// Tuning knobs.
#[derive(Clone, Debug)]
pub struct IndexConfig {
    pub analyzer: AnalyzerConfig,
    /// Automatically commit when the buffer holds this many docs (0 = never;
    /// the caller commits).
    pub flush_docs: usize,
    /// Merge when this many adjacent segments fall in the same size tier
    /// (0 = never merge automatically).
    pub merge_factor: usize,
    /// Run automatic merges on a background thread (otherwise inline in
    /// `commit`).
    pub background_merges: bool,
}

impl Default for IndexConfig {
    fn default() -> Self {
        IndexConfig {
            analyzer: AnalyzerConfig::english(),
            flush_docs: 10_000,
            merge_factor: 4,
            background_merges: true,
        }
    }
}

/// What happened while opening an index.
#[derive(Debug, Default)]
pub struct OpenReport {
    pub segments: usize,
    pub wal: ReplayReport,
    pub removed_orphans: usize,
    pub elapsed: Duration,
}

struct SegState {
    seg: Arc<DiskSegment>,
    live: Arc<LiveDocs>,
    del_gen: u64,
    /// Tombstones changed since the last persisted generation.
    dirty: bool,
}

struct WriterState {
    wal: Wal,
    buffer: WriteBuffer,
    segments: Vec<SegState>,
    next_seq: u64,
    committed_seq: u64,
    next_segment_id: u32,
    generation: u64,
    /// Segment ids currently being merged.
    merging: HashSet<u32>,
    /// Kind of every metadata field name used in the index.
    field_kinds: HashMap<String, FieldKind>,
}

/// A durable index. Create with [`Index::open`]; share via `Arc<Index>`.
pub struct Index {
    dir: PathBuf,
    config: IndexConfig,
    state: Mutex<WriterState>,
    snapshot: RwLock<Arc<Snapshot>>,
    /// Serializes merges (one at a time).
    merge_lock: Mutex<()>,
    merge_thread: Mutex<Option<JoinHandle<()>>>,
    /// First error from the background merge thread, if any.
    merge_error: Mutex<Option<String>>,
}

fn segments_dir(dir: &Path) -> PathBuf {
    dir.join("segments")
}
fn deletes_dir(dir: &Path) -> PathBuf {
    dir.join("deletes")
}
fn wal_dir(dir: &Path) -> PathBuf {
    dir.join("wal")
}
fn del_file(dir: &Path, seg: u32, gen: u64) -> PathBuf {
    deletes_dir(dir).join(format!("del-{seg:06}-{gen:016}.bin"))
}

impl Index {
    /// Open the index in `dir`, creating it if needed, and recover from the
    /// WAL: every acknowledged change since the last commit is re-applied.
    pub fn open(dir: &Path, config: IndexConfig) -> Result<(Arc<Index>, OpenReport)> {
        let start = Instant::now();
        fs::create_dir_all(segments_dir(dir))?;
        fs::create_dir_all(deletes_dir(dir))?;
        fs::create_dir_all(wal_dir(dir))?;
        let manifest = match Manifest::load(dir)? {
            Some(m) => {
                if m.analyzer != config.analyzer {
                    bail!("index was built with a different analyzer configuration");
                }
                m
            }
            None => {
                let m = Manifest {
                    generation: 0,
                    committed_seq: 0,
                    next_segment_id: 1,
                    analyzer: config.analyzer.clone(),
                    segments: Vec::new(),
                };
                m.store(dir)?;
                m
            }
        };

        let mut report = OpenReport::default();
        // Load committed segments and their tombstones.
        let mut segments = Vec::new();
        for ms in &manifest.segments {
            let seg =
                DiskSegment::open(&segments_dir(dir).join(segment_dir_name(SegmentId(ms.id))))?;
            let live = if ms.del_gen == 0 {
                LiveDocs::all_live(ms.num_docs)
            } else {
                LiveDocs::read(&del_file(dir, ms.id, ms.del_gen))?
            };
            segments.push(SegState {
                seg: Arc::new(seg),
                live: Arc::new(live),
                del_gen: ms.del_gen,
                dirty: false,
            });
        }
        report.segments = segments.len();
        report.removed_orphans = remove_orphans(dir, &manifest, true)?;

        // Replay the WAL tail.
        let (ops, wal_report) = replay(&wal_dir(dir), manifest.committed_seq)?;
        let next_seq = wal_report.max_seq.max(manifest.committed_seq) + 1;
        report.wal = wal_report;
        let wal = Wal::create(&wal_dir(dir), next_seq)?;
        let mut state = WriterState {
            wal,
            buffer: WriteBuffer::new(Analyzer::new(config.analyzer.clone())),
            segments,
            next_seq,
            committed_seq: manifest.committed_seq,
            next_segment_id: manifest.next_segment_id,
            generation: manifest.generation,
            merging: HashSet::new(),
            field_kinds: HashMap::new(),
        };
        for s in &state.segments {
            for (name, kind) in s.seg.fields().kinds() {
                state.field_kinds.insert(name.to_string(), kind);
            }
        }
        for (_, op) in &ops {
            state.apply(op);
        }

        let snapshot = Arc::new(state.snapshot());
        let index = Arc::new(Index {
            dir: dir.to_path_buf(),
            config,
            state: Mutex::new(state),
            snapshot: RwLock::new(snapshot),
            merge_lock: Mutex::new(()),
            merge_thread: Mutex::new(None),
            merge_error: Mutex::new(None),
        });
        report.elapsed = start.elapsed();
        Ok((index, report))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Add (or replace) documents `(external id, text)` without metadata.
    /// Durable when this returns `Ok`. Returns the sequence number of the
    /// last op.
    pub fn add_documents(self: &Arc<Self>, docs: &[(String, String)]) -> Result<u64> {
        let ops: Vec<WalOp> = docs
            .iter()
            .map(|(id, text)| WalOp::Add {
                id: id.clone(),
                text: text.clone(),
                fields: Fields::new(),
            })
            .collect();
        self.log_and_apply(&ops)
    }

    /// Add (or replace) documents with metadata fields. A field name must
    /// keep one kind (keyword or int) across the whole index; a batch
    /// violating that is rejected *before* anything is logged.
    pub fn add_docs(self: &Arc<Self>, docs: &[Document]) -> Result<u64> {
        let ops: Vec<WalOp> = docs
            .iter()
            .map(|d| WalOp::Add {
                id: d.id.clone(),
                text: d.text.clone(),
                fields: d.fields.clone(),
            })
            .collect();
        self.log_and_apply(&ops)
    }

    /// Delete documents by external id. Durable when this returns `Ok`.
    pub fn delete(self: &Arc<Self>, ids: &[String]) -> Result<u64> {
        let ops: Vec<WalOp> = ids
            .iter()
            .map(|id| WalOp::Delete { id: id.clone() })
            .collect();
        self.log_and_apply(&ops)
    }

    fn log_and_apply(self: &Arc<Self>, ops: &[WalOp]) -> Result<u64> {
        let should_commit = {
            let mut st = self.state.lock().unwrap();
            st.check_field_kinds(ops)?;
            let first = st.next_seq;
            st.wal.append(first, ops)?; // fsync: from here on the ops are acknowledged
            for op in ops {
                st.apply(op);
            }
            st.next_seq += ops.len() as u64;
            self.config.flush_docs > 0 && st.buffer.num_docs() >= self.config.flush_docs
        };
        if should_commit {
            self.commit()?;
        }
        Ok(self.state.lock().unwrap().next_seq - 1)
    }

    /// Flush the buffer, persist tombstones, write the manifest, truncate the
    /// WAL and publish a new snapshot. Then trigger merges if configured.
    pub fn commit(self: &Arc<Self>) -> Result<()> {
        {
            let mut st = self.state.lock().unwrap();
            if !st.buffer.is_empty() {
                let id = SegmentId(st.next_segment_id);
                st.next_segment_id += 1;
                let (meta, deleted) = st.buffer.flush(&segments_dir(&self.dir), id)?;
                let seg = DiskSegment::open(&segments_dir(&self.dir).join(segment_dir_name(id)))?;
                let mut live = LiveDocs::all_live(meta.num_docs);
                for d in &deleted {
                    live.delete(*d);
                }
                st.segments.push(SegState {
                    seg: Arc::new(seg),
                    live: Arc::new(live),
                    del_gen: 0,
                    dirty: !deleted.is_empty(),
                });
            }
            st.committed_seq = st.next_seq - 1;
            self.write_manifest(&mut st)?;
            // The new manifest covers everything up to committed_seq: start a
            // fresh WAL file and drop the old ones.
            st.wal = Wal::create(&wal_dir(&self.dir), st.next_seq)?;
            st.wal.remove_obsolete(st.committed_seq)?;
            self.publish(&st);
        }
        self.maybe_merge()
    }

    /// Persist dirty tombstones and the manifest. Caller holds the lock.
    fn write_manifest(&self, st: &mut WriterState) -> Result<()> {
        let gen = st.generation + 1;
        for s in st.segments.iter_mut().filter(|s| s.dirty) {
            s.live
                .write(&del_file(&self.dir, s.seg.meta().segment_id, gen))?;
            s.del_gen = gen;
            s.dirty = false;
        }
        let manifest = Manifest {
            generation: gen,
            committed_seq: st.committed_seq,
            next_segment_id: st.next_segment_id,
            analyzer: self.config.analyzer.clone(),
            segments: st
                .segments
                .iter()
                .map(|s| ManifestSegment {
                    id: s.seg.meta().segment_id,
                    num_docs: s.seg.num_docs(),
                    del_gen: s.del_gen,
                    num_deleted: s.live.num_deleted(),
                })
                .collect(),
        };
        manifest.store(&self.dir)?;
        st.generation = gen;
        remove_orphans(&self.dir, &manifest, false)?;
        Ok(())
    }

    fn publish(&self, st: &WriterState) {
        *self.snapshot.write().unwrap() = Arc::new(st.snapshot());
    }

    /// The current point-in-time view (cheap: clones an `Arc`).
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.read().unwrap().clone()
    }

    /// Docs buffered but not yet committed.
    pub fn buffered_docs(&self) -> usize {
        self.state.lock().unwrap().buffer.num_docs()
    }

    /// Error raised by the background merge thread, if any.
    pub fn merge_error(&self) -> Option<String> {
        self.merge_error.lock().unwrap().clone()
    }

    fn maybe_merge(self: &Arc<Self>) -> Result<()> {
        if self.config.merge_factor < 2 {
            return Ok(());
        }
        if !self.config.background_merges {
            while self.merge_once(false)? {}
            return Ok(());
        }
        let mut handle = self.merge_thread.lock().unwrap();
        if handle.as_ref().is_some_and(|h| !h.is_finished()) {
            return Ok(()); // the running thread re-checks for work when done
        }
        let index = Arc::clone(self);
        *handle = Some(std::thread::spawn(move || loop {
            match index.merge_once(false) {
                Ok(true) => continue,
                Ok(false) => break,
                Err(e) => {
                    *index.merge_error.lock().unwrap() = Some(format!("{e:#}"));
                    break;
                }
            }
        }));
        Ok(())
    }

    /// Wait for the background merge thread (if running) to finish.
    pub fn wait_for_merges(&self) -> Result<()> {
        let handle = self.merge_thread.lock().unwrap().take();
        if let Some(h) = handle {
            h.join()
                .map_err(|_| anyhow::anyhow!("merge thread panicked"))?;
        }
        match self.merge_error() {
            Some(e) => bail!("background merge failed: {e}"),
            None => Ok(()),
        }
    }

    /// Wait for background merges, then run any merges still pending
    /// according to the merge policy (inline).
    pub fn merge_pending(self: &Arc<Self>) -> Result<()> {
        self.wait_for_merges()?;
        if self.config.merge_factor >= 2 {
            while self.merge_once(false)? {}
        }
        Ok(())
    }

    /// Merge all segments into one, dropping all deleted docs (after waiting
    /// for background merges).
    pub fn force_merge(self: &Arc<Self>) -> Result<()> {
        self.wait_for_merges()?;
        while self.merge_once(true)? {}
        Ok(())
    }

    /// Pick and run one merge. `all = true` merges every segment into one.
    /// Returns whether a merge happened.
    fn merge_once(&self, all: bool) -> Result<bool> {
        let _serial = self.merge_lock.lock().unwrap();
        // 1. Pick under the lock (fast).
        let (sources, ids, new_id) = {
            let mut st = self.state.lock().unwrap();
            let window = if all {
                // Merge everything into one segment; a single segment is
                // rewritten too if it has deletes (expunging them).
                let n = st.segments.len();
                (n > 1 || (n == 1 && st.segments[0].live.num_deleted() > 0)).then_some((0, n))
            } else {
                pick_window(&st.segments, self.config.merge_factor)
            };
            let Some((start, len)) = window else {
                return Ok(false);
            };
            let picked = &st.segments[start..start + len];
            let sources: Vec<(Arc<DiskSegment>, Arc<LiveDocs>)> = picked
                .iter()
                .map(|s| (Arc::clone(&s.seg), Arc::clone(&s.live)))
                .collect();
            let ids: Vec<u32> = picked.iter().map(|s| s.seg.meta().segment_id).collect();
            st.merging.extend(ids.iter().copied());
            let new_id = SegmentId(st.next_segment_id);
            st.next_segment_id += 1;
            (sources, ids, new_id)
        };

        // 2. Merge without holding the writer lock: adds/deletes/commits continue.
        let merged = merge_segments(
            &sources,
            &segments_dir(&self.dir),
            new_id,
            self.config.analyzer.clone(),
        );
        let (meta, doc_maps) = match merged {
            Ok(m) => m,
            Err(e) => {
                let mut st = self.state.lock().unwrap();
                for id in &ids {
                    st.merging.remove(id);
                }
                return Err(e);
            }
        };
        let seg = Arc::new(DiskSegment::open(
            &segments_dir(&self.dir).join(segment_dir_name(new_id)),
        )?);

        // 3. Swap in under the lock, carrying over deletes that happened
        //    while the merge was running.
        let mut st = self.state.lock().unwrap();
        let start = st
            .segments
            .iter()
            .position(|s| s.seg.meta().segment_id == ids[0])
            .context("merged segment vanished")?;
        let mut live = LiveDocs::all_live(meta.num_docs);
        for (i, (_, live_at_start)) in sources.iter().enumerate() {
            let now = &st.segments[start + i];
            debug_assert_eq!(now.seg.meta().segment_id, ids[i]);
            for d in 0..now.live.num_docs() {
                if live_at_start.is_live(d) && !now.live.is_live(d) {
                    let new_doc = doc_maps[i][d as usize];
                    debug_assert_ne!(new_doc, DELETED);
                    live.delete(new_doc);
                }
            }
        }
        let dirty = live.num_deleted() > 0;
        let replaced: Vec<SegState> = st
            .segments
            .splice(
                start..start + ids.len(),
                [SegState {
                    seg,
                    live: Arc::new(live),
                    del_gen: 0,
                    dirty,
                }],
            )
            .collect();
        for id in &ids {
            st.merging.remove(id);
        }
        self.write_manifest(&mut st)?;
        self.publish(&st);
        for r in replaced {
            r.seg.mark_obsolete(); // deleted when the last snapshot drops it
        }
        Ok(true)
    }
}

impl WriterState {
    fn snapshot(&self) -> Snapshot {
        Snapshot::from_parts(
            self.segments
                .iter()
                .map(|s| (Arc::clone(&s.seg), Arc::clone(&s.live)))
                .collect(),
        )
    }

    /// Reject ops that would use a field name with a second kind.
    fn check_field_kinds(&self, ops: &[WalOp]) -> Result<()> {
        let mut seen: HashMap<&str, FieldKind> = HashMap::new();
        for op in ops {
            if let WalOp::Add { fields, .. } = op {
                for (name, v) in fields {
                    let known = self
                        .field_kinds
                        .get(name)
                        .or_else(|| seen.get(name.as_str()));
                    if let Some(&k) = known {
                        if k != v.kind() {
                            bail!("field {name:?} is {k:?} in this index, got {:?}", v.kind());
                        }
                    }
                    seen.insert(name, v.kind());
                }
            }
        }
        Ok(())
    }

    /// Apply one logged op to the buffer and tombstones (no I/O).
    fn apply(&mut self, op: &WalOp) {
        match op {
            WalOp::Add { id, text, fields } => {
                self.delete_everywhere(id); // upsert semantics
                for (name, v) in fields {
                    self.field_kinds.entry(name.clone()).or_insert(v.kind());
                }
                self.buffer.add_with_fields(id, text, fields);
            }
            WalOp::Delete { id } => self.delete_everywhere(id),
        }
    }

    fn delete_everywhere(&mut self, id: &str) {
        self.buffer.delete(id);
        for s in &mut self.segments {
            if let Some(doc) = s.seg.doc_by_external_id(id) {
                if s.live.is_live(doc.0) {
                    // Copy-on-write: snapshots holding the old Arc keep seeing
                    // the doc; only new snapshots see the delete.
                    Arc::make_mut(&mut s.live).delete(doc.0);
                    s.dirty = true;
                }
            }
        }
    }
}

/// Find `factor` adjacent segments in the same size tier (tier =
/// floor(log_factor(live docs))). Returns (start, len).
fn pick_window(segments: &[SegState], factor: usize) -> Option<(usize, usize)> {
    let tier = |s: &SegState| {
        ((s.live.num_live().max(1) as f64).ln() / (factor as f64).ln()).floor() as i64
    };
    if segments.len() < factor {
        return None;
    }
    (0..=segments.len() - factor).find_map(|start| {
        let w = &segments[start..start + factor];
        let t = tier(&w[0]);
        w.iter().all(|s| tier(s) == t).then_some((start, factor))
    })
}

/// Remove tombstone files the manifest does not reference and, when
/// `gc_segments` (only at open, when no snapshot can exist yet), segment
/// directories it does not reference: leftovers of a crash mid-flush or
/// mid-merge. While the index is running, merged-away segments are removed
/// by `DiskSegment`'s `Drop` once the last snapshot using them is gone.
/// Returns the number of removed entries.
fn remove_orphans(dir: &Path, manifest: &Manifest, gc_segments: bool) -> Result<usize> {
    let live_segs: HashSet<String> = manifest
        .segments
        .iter()
        .map(|s| segment_dir_name(SegmentId(s.id)))
        .collect();
    let live_dels: HashSet<PathBuf> = manifest
        .segments
        .iter()
        .filter(|s| s.del_gen > 0)
        .map(|s| del_file(dir, s.id, s.del_gen))
        .collect();
    let mut removed = 0;
    if gc_segments {
        for e in fs::read_dir(segments_dir(dir))? {
            let path = e?.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let id: Option<u32> = name.strip_prefix("seg-").and_then(|n| n.parse().ok());
            if !live_segs.contains(&name) && id.is_some() {
                fs::remove_dir_all(&path)?;
                removed += 1;
            }
        }
    }
    for e in fs::read_dir(deletes_dir(dir))? {
        let path = e?.path();
        if !live_dels.contains(&path) {
            fs::remove_file(&path)?;
            removed += 1;
        }
    }
    Ok(removed)
}
