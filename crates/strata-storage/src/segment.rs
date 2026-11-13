//! Immutable on-disk segments: writer and memory-mapped reader.
//!
//! A segment is a directory of files written once and never modified:
//!
//! ```text
//! seg-000007/
//!   meta.json      SegmentMeta (format version, counts, analyzer config)
//!   terms.fst      FST: term bytes -> term ordinal
//!   terms.bin      per ordinal, 20 bytes: df u32 | postings offset u64 | positions offset u64
//!   postings.bin   per term: varint num_blocks, skip table, bit-packed blocks (see below)
//!   positions.bin  per term, per posting: tf varints (first absolute, then gaps)
//!   doclens.bin    u32 per doc (exact token count)
//!   ids.fst        FST: external id -> doc id
//!   ids.bin        u32 offsets (num_docs + 1) followed by the concatenated id bytes
//! ```
//!
//! Postings of one term, in blocks of [`BLOCK_SIZE`] (=128) docs:
//!
//! ```text
//! varint num_blocks
//! skip entry * num_blocks, 20 bytes each:
//!     last_doc u32 | block byte offset u32 | max_tf u32 | min_doc_len u32 | positions offset u32
//! block * num_blocks:
//!     doc_bits u8 | tf_bits u8 | doc gaps (bit-packed) | tf - 1 (bit-packed)
//! ```
//!
//! Doc ids are stored as gaps minus one (`doc - prev_doc - 1`; the first doc
//! of a block is relative to the previous block's `last_doc + 1`), so dense
//! postings pack into very few bits. The skip table lets a cursor jump to the
//! block containing a target doc without decoding the ones before it, and its
//! `max_tf` / `min_doc_len` give an upper bound on any BM25 score inside the
//! block (BM25 grows with tf and shrinks with length). Stage 3 uses that
//! bound for block-max WAND. The bound does not depend on collection
//! statistics, so it stays valid when segments are merged.
//!
//! Reading uses `mmap`: the OS pages file contents in on demand and caches
//! them, so a cold segment costs no RAM until it is queried. The single
//! `unsafe` call that creates a mapping lives in [`map_file`].

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};
use fst::{Map, MapBuilder};
use memmap2::Mmap;
use serde::{Deserialize, Serialize};
use strata_core::analyzer::AnalyzerConfig;
use strata_core::index::{Posting, SegmentReader};
use strata_core::types::{DocId, SegmentId};

use crate::codec::{bits_needed, pack, read_u32, read_u64, read_varint, unpack, write_varint};

/// Documents per postings block.
pub const BLOCK_SIZE: usize = 128;
/// On-disk format version written into `meta.json`.
pub const FORMAT_VERSION: u32 = 1;
const SKIP_ENTRY_BYTES: usize = 20;
const TERM_INFO_BYTES: usize = 20;

/// Contents of `meta.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SegmentMeta {
    pub format_version: u32,
    pub segment_id: u32,
    pub num_docs: u32,
    pub num_terms: u64,
    pub num_postings: u64,
    pub total_tokens: u64,
    pub analyzer: AnalyzerConfig,
}

/// Name of a segment's directory inside an index directory.
pub fn segment_dir_name(id: SegmentId) -> String {
    format!("seg-{:06}", id.0)
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// One posting plus its positions, as fed to [`SegmentWriter::add_term`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostingWithPositions {
    pub doc: u32,
    /// Token positions of the term in the doc, ascending; `len()` is the tf.
    pub positions: Vec<u32>,
}

/// Streams terms (in sorted order) and per-doc data into segment files.
pub struct SegmentWriter {
    dir: PathBuf,
    id: SegmentId,
    analyzer: AnalyzerConfig,
    terms_fst: MapBuilder<BufWriter<File>>,
    term_infos: BufWriter<File>,
    postings: BufWriter<File>,
    positions: BufWriter<File>,
    postings_len: u64,
    positions_len: u64,
    num_terms: u64,
    num_postings: u64,
    last_term: Option<Vec<u8>>,
    /// Scratch buffers reused across terms.
    block_buf: Vec<u8>,
    pos_buf: Vec<u8>,
}

impl SegmentWriter {
    /// Create `parent/seg-XXXXXX/` and open its files. Fails if it exists.
    pub fn create(parent: &Path, id: SegmentId, analyzer: AnalyzerConfig) -> Result<SegmentWriter> {
        let dir = parent.join(segment_dir_name(id));
        fs::create_dir(&dir).with_context(|| format!("create {}", dir.display()))?;
        let open = |name: &str| -> Result<BufWriter<File>> {
            Ok(BufWriter::new(File::create(dir.join(name))?))
        };
        Ok(SegmentWriter {
            terms_fst: MapBuilder::new(open("terms.fst")?)?,
            term_infos: open("terms.bin")?,
            postings: open("postings.bin")?,
            positions: open("positions.bin")?,
            dir,
            id,
            analyzer,
            postings_len: 0,
            positions_len: 0,
            num_terms: 0,
            num_postings: 0,
            last_term: None,
            block_buf: Vec::new(),
            pos_buf: Vec::new(),
        })
    }

    /// Add one term's postings. Terms must arrive in strictly increasing byte
    /// order; postings in increasing doc order; `doc_lens[doc]` must be known
    /// (it feeds the per-block `min_doc_len`).
    pub fn add_term(
        &mut self,
        term: &str,
        postings: &[PostingWithPositions],
        doc_lens: &[u32],
    ) -> Result<()> {
        if postings.is_empty() {
            return Ok(()); // a term whose docs were all deleted (merge)
        }
        if let Some(prev) = &self.last_term {
            if prev.as_slice() >= term.as_bytes() {
                bail!(
                    "terms out of order: {:?} after {:?}",
                    term,
                    String::from_utf8_lossy(prev)
                );
            }
        }
        self.last_term = Some(term.as_bytes().to_vec());

        // Positions first, remembering where each block's positions begin.
        self.pos_buf.clear();
        let mut block_pos_offsets = Vec::new();
        for (i, p) in postings.iter().enumerate() {
            if i % BLOCK_SIZE == 0 {
                block_pos_offsets.push(self.pos_buf.len() as u32);
            }
            let mut prev = 0;
            for (j, &pos) in p.positions.iter().enumerate() {
                let v = if j == 0 { pos } else { pos - prev };
                write_varint(u64::from(v), &mut self.pos_buf);
                prev = pos;
            }
        }

        // Blocks + skip table.
        let num_blocks = postings.len().div_ceil(BLOCK_SIZE);
        self.block_buf.clear();
        let mut skips: Vec<[u32; 5]> = Vec::with_capacity(num_blocks);
        let mut base: u32 = 0; // previous block's last doc + 1
        let mut gaps = [0u32; BLOCK_SIZE];
        let mut tfs = [0u32; BLOCK_SIZE];
        for (b, chunk) in postings.chunks(BLOCK_SIZE).enumerate() {
            let mut prev_plus_one = base;
            let mut max_tf = 0;
            let mut min_len = u32::MAX;
            for (i, p) in chunk.iter().enumerate() {
                assert!(p.doc >= prev_plus_one, "postings must be sorted by doc");
                gaps[i] = p.doc - prev_plus_one;
                prev_plus_one = p.doc + 1;
                let tf = p.positions.len() as u32;
                assert!(tf >= 1, "posting without positions");
                tfs[i] = tf - 1;
                max_tf = max_tf.max(tf);
                min_len = min_len.min(doc_lens[p.doc as usize]);
            }
            let n = chunk.len();
            let doc_bits = bits_needed(gaps[..n].iter().copied().max().unwrap_or(0));
            let tf_bits = bits_needed(tfs[..n].iter().copied().max().unwrap_or(0));
            let offset = self.block_buf.len() as u32;
            self.block_buf.push(doc_bits);
            self.block_buf.push(tf_bits);
            pack(&gaps[..n], doc_bits, &mut self.block_buf);
            pack(&tfs[..n], tf_bits, &mut self.block_buf);
            let last_doc = chunk[n - 1].doc;
            skips.push([last_doc, offset, max_tf, min_len, block_pos_offsets[b]]);
            base = last_doc + 1;
        }

        let mut header = Vec::new();
        write_varint(num_blocks as u64, &mut header);
        for s in &skips {
            for v in s {
                header.extend_from_slice(&v.to_le_bytes());
            }
        }

        // Term info: df | postings offset | positions offset.
        self.term_infos
            .write_all(&(postings.len() as u32).to_le_bytes())?;
        self.term_infos
            .write_all(&self.postings_len.to_le_bytes())?;
        self.term_infos
            .write_all(&self.positions_len.to_le_bytes())?;

        self.postings.write_all(&header)?;
        self.postings.write_all(&self.block_buf)?;
        self.postings_len += (header.len() + self.block_buf.len()) as u64;
        self.positions.write_all(&self.pos_buf)?;
        self.positions_len += self.pos_buf.len() as u64;

        self.terms_fst.insert(term.as_bytes(), self.num_terms)?;
        self.num_terms += 1;
        self.num_postings += postings.len() as u64;
        Ok(())
    }

    /// Write per-document files and `meta.json`, then fsync everything.
    /// `ids[doc]` is the external id of `doc`; duplicates map to the last doc.
    pub fn finish(self, doc_lens: &[u32], ids: &[String]) -> Result<SegmentMeta> {
        assert_eq!(doc_lens.len(), ids.len());
        let SegmentWriter {
            dir,
            id,
            analyzer,
            terms_fst,
            mut term_infos,
            mut postings,
            mut positions,
            num_terms,
            num_postings,
            ..
        } = self;
        terms_fst.finish()?;
        term_infos.flush()?;
        postings.flush()?;
        positions.flush()?;

        let mut lens = BufWriter::new(File::create(dir.join("doclens.bin"))?);
        for l in doc_lens {
            lens.write_all(&l.to_le_bytes())?;
        }
        lens.flush()?;

        // doc -> id: offsets table + bytes.
        let mut out = BufWriter::new(File::create(dir.join("ids.bin"))?);
        let mut offset = 0u32;
        out.write_all(&offset.to_le_bytes())?;
        for s in ids {
            offset += s.len() as u32;
            out.write_all(&offset.to_le_bytes())?;
        }
        for s in ids {
            out.write_all(s.as_bytes())?;
        }
        out.flush()?;

        // id -> doc: FST needs sorted unique keys; keep the last doc per id.
        let mut sorted: Vec<(&str, u32)> = ids
            .iter()
            .enumerate()
            .map(|(d, s)| (s.as_str(), d as u32))
            .collect();
        sorted.sort();
        sorted.dedup_by(|later, earlier| {
            // `dedup_by` keeps `earlier`; copy the larger doc id into it.
            if later.0 == earlier.0 {
                earlier.1 = earlier.1.max(later.1);
                true
            } else {
                false
            }
        });
        let mut ids_fst = MapBuilder::new(BufWriter::new(File::create(dir.join("ids.fst"))?))?;
        for (s, d) in sorted {
            ids_fst.insert(s.as_bytes(), u64::from(d))?;
        }
        ids_fst.finish()?;

        let meta = SegmentMeta {
            format_version: FORMAT_VERSION,
            segment_id: id.0,
            num_docs: doc_lens.len() as u32,
            num_terms,
            num_postings,
            total_tokens: doc_lens.iter().map(|&l| u64::from(l)).sum(),
            analyzer,
        };
        fs::write(dir.join("meta.json"), serde_json::to_vec_pretty(&meta)?)?;

        // Durability: fsync every file and the directory itself, so a crash
        // after this returns can never observe a half-written segment.
        for entry in fs::read_dir(&dir)? {
            File::open(entry?.path())?.sync_all()?;
        }
        File::open(&dir)?.sync_all()?;
        Ok(meta)
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// File contents: either memory-mapped or (for empty files, which cannot be
/// mapped on every OS) an owned empty buffer.
pub enum Bytes {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Mapped(m) => m,
            Bytes::Owned(v) => v,
        }
    }
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

/// Memory-map a file read-only.
pub fn map_file(path: &Path) -> Result<Bytes> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    if file.metadata()?.len() == 0 {
        return Ok(Bytes::Owned(Vec::new()));
    }
    // SAFETY: `Mmap::map` is unsafe because the mapped memory would change
    // (or the process could crash with SIGBUS) if another process modified or
    // truncated the file while it is mapped. Segment files are written once,
    // fsynced, and never modified or truncated afterwards; they are only
    // deleted (unlinking a mapped file is safe on Unix, the pages stay valid
    // until unmapped). Nothing outside strata is expected to write into an
    // index directory.
    let map = unsafe { Mmap::map(&file) }.with_context(|| format!("mmap {}", path.display()))?;
    Ok(Bytes::Mapped(map))
}

/// A read-only, memory-mapped segment.
pub struct DiskSegment {
    dir: PathBuf,
    meta: SegmentMeta,
    terms: Map<Bytes>,
    term_infos: Bytes,
    postings: Bytes,
    positions: Bytes,
    doc_lens: Bytes,
    ids_offsets_and_bytes: Bytes,
    ids: Map<Bytes>,
    /// Set when the segment has been merged away; the directory is removed
    /// when the last reference (e.g. an old snapshot) is dropped.
    obsolete: AtomicBool,
}

/// Per-term metadata from `terms.bin`.
#[derive(Clone, Copy, Debug)]
pub struct TermInfo {
    pub doc_freq: u32,
    postings_offset: u64,
    positions_offset: u64,
}

impl DiskSegment {
    /// Open the segment stored in `dir`.
    pub fn open(dir: &Path) -> Result<DiskSegment> {
        let meta: SegmentMeta = serde_json::from_slice(
            &fs::read(dir.join("meta.json"))
                .with_context(|| format!("read {}/meta.json", dir.display()))?,
        )?;
        if meta.format_version != FORMAT_VERSION {
            bail!(
                "unsupported segment format {} in {}",
                meta.format_version,
                dir.display()
            );
        }
        Ok(DiskSegment {
            terms: Map::new(map_file(&dir.join("terms.fst"))?)?,
            term_infos: map_file(&dir.join("terms.bin"))?,
            postings: map_file(&dir.join("postings.bin"))?,
            positions: map_file(&dir.join("positions.bin"))?,
            doc_lens: map_file(&dir.join("doclens.bin"))?,
            ids_offsets_and_bytes: map_file(&dir.join("ids.bin"))?,
            ids: Map::new(map_file(&dir.join("ids.fst"))?)?,
            dir: dir.to_path_buf(),
            meta,
            obsolete: AtomicBool::new(false),
        })
    }

    pub fn meta(&self) -> &SegmentMeta {
        &self.meta
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Mark for deletion once no longer referenced.
    pub fn mark_obsolete(&self) {
        self.obsolete.store(true, Ordering::SeqCst);
    }

    /// Total bytes of this segment's files on disk.
    pub fn size_on_disk(&self) -> Result<u64> {
        let mut total = 0;
        for e in fs::read_dir(&self.dir)? {
            total += e?.metadata()?.len();
        }
        Ok(total)
    }

    /// Look up a term's metadata.
    pub fn term_info(&self, term: &str) -> Option<TermInfo> {
        let ord = self.terms.get(term.as_bytes())? as usize;
        Some(self.term_info_at(ord))
    }

    fn term_info_at(&self, ord: usize) -> TermInfo {
        let at = ord * TERM_INFO_BYTES;
        TermInfo {
            doc_freq: read_u32(&self.term_infos, at),
            postings_offset: read_u64(&self.term_infos, at + 4),
            positions_offset: read_u64(&self.term_infos, at + 12),
        }
    }

    /// The term dictionary FST (for prefix/range queries and merging).
    pub fn term_dict(&self) -> &Map<Bytes> {
        &self.terms
    }

    /// Postings cursor for a term ordinal from [`term_dict`](Self::term_dict).
    pub fn postings_by_ordinal(&self, ord: u64) -> DiskPostings<'_> {
        DiskPostings::new(self, self.term_info_at(ord as usize))
    }

    /// Doc id of an external id (the newest one if it occurs several times).
    pub fn doc_by_external_id(&self, id: &str) -> Option<DocId> {
        self.ids.get(id.as_bytes()).map(|d| DocId(d as u32))
    }

    /// Cursor over a term's postings (with positions and block skipping).
    pub fn cursor(&self, term: &str) -> Option<DiskPostings<'_>> {
        self.term_info(term)
            .map(|info| DiskPostings::new(self, info))
    }
}

impl Drop for DiskSegment {
    fn drop(&mut self) {
        if self.obsolete.load(Ordering::SeqCst) {
            // Best effort: a leftover directory is cleaned up on next open.
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

impl SegmentReader for DiskSegment {
    type Postings<'a> = DiskPostings<'a>;

    fn segment_id(&self) -> SegmentId {
        SegmentId(self.meta.segment_id)
    }

    fn num_docs(&self) -> u32 {
        self.meta.num_docs
    }

    fn total_tokens(&self) -> u64 {
        self.meta.total_tokens
    }

    fn doc_len(&self, doc: DocId) -> u32 {
        read_u32(&self.doc_lens, doc.as_usize() * 4)
    }

    fn external_id(&self, doc: DocId) -> &str {
        let n = self.meta.num_docs as usize;
        let table = &self.ids_offsets_and_bytes;
        let start = read_u32(table, doc.as_usize() * 4) as usize;
        let end = read_u32(table, doc.as_usize() * 4 + 4) as usize;
        let bytes_base = (n + 1) * 4;
        std::str::from_utf8(&table[bytes_base + start..bytes_base + end]).expect("ids are UTF-8")
    }

    fn doc_freq(&self, term: &str) -> u32 {
        self.term_info(term).map_or(0, |t| t.doc_freq)
    }

    fn postings(&self, term: &str) -> Option<DiskPostings<'_>> {
        self.cursor(term)
    }

    fn num_terms(&self) -> usize {
        self.meta.num_terms as usize
    }
}

/// One entry of a term's skip table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkipEntry {
    pub last_doc: u32,
    block_offset: u32,
    /// Largest tf in the block.
    pub max_tf: u32,
    /// Smallest document length in the block.
    pub min_doc_len: u32,
    positions_offset: u32,
}

/// Block-decoding cursor over one term's postings.
///
/// Implements `Iterator<Item = Posting>` for the generic searcher, plus
/// `advance_to` (skip to the first doc >= target using the skip table) and
/// `positions` (decode positions of the current posting on demand).
pub struct DiskPostings<'a> {
    seg: &'a DiskSegment,
    df: u32,
    num_blocks: usize,
    skips: &'a [u8],
    blocks: &'a [u8],
    positions: &'a [u8],
    /// Index of the block currently decoded in `docs`/`tfs` (`usize::MAX` = none yet).
    block: usize,
    block_len: usize,
    /// Index within the current block of the *next* posting to return.
    idx: usize,
    docs: [u32; BLOCK_SIZE],
    tfs: [u32; BLOCK_SIZE],
    /// Positions decoding state: the positions of block posting `pos_idx`
    /// start at byte `pos_byte` of `positions`. Advanced lazily, so reading
    /// positions while iterating sequentially costs O(total positions).
    pos_idx: usize,
    pos_byte: usize,
}

impl<'a> DiskPostings<'a> {
    fn new(seg: &'a DiskSegment, info: TermInfo) -> DiskPostings<'a> {
        let data = &seg.postings[info.postings_offset as usize..];
        let mut pos = 0;
        let num_blocks = read_varint(data, &mut pos) as usize;
        let skips = &data[pos..pos + num_blocks * SKIP_ENTRY_BYTES];
        let blocks = &data[pos + num_blocks * SKIP_ENTRY_BYTES..];
        DiskPostings {
            seg,
            df: info.doc_freq,
            num_blocks,
            skips,
            blocks,
            positions: &seg.positions[info.positions_offset as usize..],
            block: usize::MAX,
            block_len: 0,
            idx: 0,
            docs: [0; BLOCK_SIZE],
            tfs: [0; BLOCK_SIZE],
            pos_idx: 0,
            pos_byte: 0,
        }
    }

    /// Document frequency of this term in the segment.
    pub fn doc_freq(&self) -> u32 {
        self.df
    }

    /// Number of blocks.
    pub fn num_blocks(&self) -> usize {
        self.num_blocks
    }

    /// Skip entry of block `b`.
    pub fn skip_entry(&self, b: usize) -> SkipEntry {
        let at = b * SKIP_ENTRY_BYTES;
        SkipEntry {
            last_doc: read_u32(self.skips, at),
            block_offset: read_u32(self.skips, at + 4),
            max_tf: read_u32(self.skips, at + 8),
            min_doc_len: read_u32(self.skips, at + 12),
            positions_offset: read_u32(self.skips, at + 16),
        }
    }

    /// Index of the current block (valid after the first `next`/`advance_to`).
    pub fn current_block(&self) -> usize {
        self.block
    }

    fn decode_block(&mut self, b: usize) {
        let entry = self.skip_entry(b);
        let data = &self.blocks[entry.block_offset as usize..];
        let doc_bits = data[0];
        let tf_bits = data[1];
        let n = if b + 1 == self.num_blocks {
            self.df as usize - BLOCK_SIZE * (self.num_blocks - 1)
        } else {
            BLOCK_SIZE
        };
        let used = unpack(&data[2..], doc_bits, &mut self.docs[..n]);
        unpack(&data[2 + used..], tf_bits, &mut self.tfs[..n]);
        // Gaps -> absolute doc ids; stored tf - 1 -> tf.
        let mut next = if b == 0 {
            0
        } else {
            self.skip_entry(b - 1).last_doc + 1
        };
        for i in 0..n {
            let doc = next + self.docs[i];
            self.docs[i] = doc;
            next = doc + 1;
            self.tfs[i] += 1;
        }
        self.block = b;
        self.block_len = n;
        self.idx = 0;
        self.pos_idx = 0;
        self.pos_byte = entry.positions_offset as usize;
    }

    /// Move to the first posting with `doc >= target` (never backwards) and
    /// return it, or `None` when exhausted. Blocks whose `last_doc < target`
    /// are skipped without decoding.
    pub fn advance_to(&mut self, target: u32) -> Option<Posting> {
        // Fast path: still inside the current block.
        if self.block != usize::MAX
            && self.idx < self.block_len
            && self.docs[self.block_len - 1] >= target
        {
            while self.docs[self.idx] < target {
                self.idx += 1;
            }
            return self.next();
        }
        let start = if self.block == usize::MAX {
            0
        } else {
            self.block + 1
        };
        let b = (start..self.num_blocks).find(|&b| self.skip_entry(b).last_doc >= target)?;
        self.decode_block(b);
        while self.docs[self.idx] < target {
            self.idx += 1;
        }
        self.next()
    }

    /// Positions of the posting most recently returned by `next`/`advance_to`.
    /// Takes `&mut self` because it advances the internal positions cursor.
    pub fn positions(&mut self) -> Vec<u32> {
        let mut out = Vec::new();
        self.positions_into(&mut out);
        out
    }

    /// Like [`positions`](Self::positions) but reuses `out`.
    pub fn positions_into(&mut self, out: &mut Vec<u32>) {
        assert!(
            self.block != usize::MAX && self.idx > 0,
            "no current posting"
        );
        let current = self.idx - 1;
        assert!(
            self.pos_idx <= current,
            "positions already consumed for this posting"
        );
        // Skip the varints of postings we passed without reading positions.
        while self.pos_idx < current {
            for _ in 0..self.tfs[self.pos_idx] {
                read_varint(self.positions, &mut self.pos_byte);
            }
            self.pos_idx += 1;
        }
        out.clear();
        let mut prev = 0u32;
        for j in 0..self.tfs[current] {
            let v = read_varint(self.positions, &mut self.pos_byte) as u32;
            let p = if j == 0 { v } else { prev + v };
            out.push(p);
            prev = p;
        }
        self.pos_idx = current + 1;
    }

    /// The segment this cursor reads from.
    pub fn segment(&self) -> &'a DiskSegment {
        self.seg
    }
}

impl Iterator for DiskPostings<'_> {
    type Item = Posting;

    fn next(&mut self) -> Option<Posting> {
        if self.block == usize::MAX || self.idx == self.block_len {
            let next_block = if self.block == usize::MAX {
                0
            } else {
                self.block + 1
            };
            if next_block >= self.num_blocks {
                return None;
            }
            self.decode_block(next_block);
        }
        let p = Posting {
            doc: DocId(self.docs[self.idx]),
            tf: self.tfs[self.idx],
        };
        self.idx += 1;
        Some(p)
    }
}
