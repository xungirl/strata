//! The in-memory write buffer: documents added since the last flush.
//!
//! Like Lucene's `IndexWriter` RAM buffer: new documents are analyzed into a
//! small in-memory inverted index (now with positions); `flush` turns it into
//! an immutable on-disk segment. Documents become searchable at commit time,
//! when the flushed segment is published in a new snapshot.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use strata_core::analyzer::Analyzer;
use strata_core::types::SegmentId;

use crate::fields::Fields;
use crate::segment::{PostingWithPositions, SegmentMeta, SegmentWriter};

/// Buffered documents not yet written to a segment.
pub struct WriteBuffer {
    analyzer: Analyzer,
    /// term -> postings (doc ids are buffer-local, ascending).
    terms: HashMap<String, Vec<PostingWithPositions>>,
    doc_lens: Vec<u32>,
    ids: Vec<String>,
    /// external id -> newest buffer doc with that id
    by_id: HashMap<String, u32>,
    /// buffer docs deleted (or replaced by a newer version) before flushing
    deleted: Vec<bool>,
    fields: Vec<Fields>,
    tokens: Vec<String>,
}

impl WriteBuffer {
    pub fn new(analyzer: Analyzer) -> WriteBuffer {
        WriteBuffer {
            analyzer,
            terms: HashMap::new(),
            doc_lens: Vec::new(),
            ids: Vec::new(),
            by_id: HashMap::new(),
            deleted: Vec::new(),
            fields: Vec::new(),
            tokens: Vec::new(),
        }
    }

    pub fn num_docs(&self) -> usize {
        self.doc_lens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.doc_lens.is_empty()
    }

    pub fn analyzer(&self) -> &Analyzer {
        &self.analyzer
    }

    /// Add a document without metadata fields.
    pub fn add(&mut self, id: &str, text: &str) -> u32 {
        self.add_with_fields(id, text, &Fields::new())
    }

    /// Add a document; returns its buffer-local doc id.
    pub fn add_with_fields(&mut self, id: &str, text: &str, fields: &Fields) -> u32 {
        let doc = self.doc_lens.len() as u32;
        let mut tokens = std::mem::take(&mut self.tokens);
        tokens.clear();
        self.analyzer.analyze_into(text, &mut tokens);
        self.doc_lens.push(tokens.len() as u32);

        // Positions are token indexes *after* stopword removal (no gaps), so a
        // phrase query analyzed the same way matches "cat on the mat" as
        // ["cat", "mat"] at positions 0, 1. Lucene keeps gaps for removed
        // stopwords; we document this difference instead.
        let mut by_term: Vec<(&str, u32)> = tokens
            .iter()
            .enumerate()
            .map(|(p, t)| (t.as_str(), p as u32))
            .collect();
        by_term.sort_unstable();
        for run in by_term.chunk_by(|a, b| a.0 == b.0) {
            let posting = PostingWithPositions {
                doc,
                positions: run.iter().map(|&(_, p)| p).collect(),
            };
            match self.terms.get_mut(run[0].0) {
                Some(list) => list.push(posting),
                None => {
                    self.terms.insert(run[0].0.to_string(), vec![posting]);
                }
            }
        }
        self.tokens = tokens;

        if let Some(old) = self.by_id.insert(id.to_string(), doc) {
            self.deleted[old as usize] = true; // upsert within the buffer
        }
        self.ids.push(id.to_string());
        self.deleted.push(false);
        self.fields.push(fields.clone());
        doc
    }

    /// Is a live (not deleted, not replaced) version of `id` buffered?
    pub fn contains(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    /// Number of live buffered documents.
    pub fn num_live(&self) -> usize {
        self.by_id.len()
    }

    /// Delete the buffered version of `id`, if any. Returns whether it existed.
    pub fn delete(&mut self, id: &str) -> bool {
        match self.by_id.remove(id) {
            Some(doc) => {
                self.deleted[doc as usize] = true;
                true
            }
            None => false,
        }
    }

    /// Write the buffer as segment `id` under `dir` and reset the buffer.
    /// Returns the segment metadata and the doc ids that were deleted while
    /// buffered. Those docs are still written (the caller tombstones them);
    /// this keeps segment doc ids identical to insertion order.
    pub fn flush(&mut self, dir: &Path, id: SegmentId) -> Result<(SegmentMeta, Vec<u32>)> {
        let mut writer = SegmentWriter::create(dir, id, self.analyzer.config().clone())?;
        let mut terms: Vec<(String, Vec<PostingWithPositions>)> =
            std::mem::take(&mut self.terms).into_iter().collect();
        terms.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        for (term, postings) in &terms {
            writer.add_term(term, postings, &self.doc_lens)?;
        }
        let meta = writer.finish(&self.doc_lens, &self.ids, &self.fields)?;
        let deleted = self
            .deleted
            .iter()
            .enumerate()
            .filter(|(_, &d)| d)
            .map(|(i, _)| i as u32)
            .collect();
        self.doc_lens.clear();
        self.ids.clear();
        self.by_id.clear();
        self.deleted.clear();
        self.fields.clear();
        Ok((meta, deleted))
    }
}
