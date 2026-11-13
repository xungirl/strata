//! In-memory inverted index: a mutable builder and an immutable segment.
//!
//! Two-phase design (the same shape Lucene/Tantivy use):
//!
//! 1. [`InMemoryIndexBuilder`] accepts documents one at a time and keeps a
//!    `HashMap<term, Vec<Posting>>`, which is easy to append to.
//! 2. [`InMemoryIndexBuilder::build`] *freezes* it into an
//!    [`InMemorySegment`]: terms sorted, all postings concatenated into one
//!    flat array. Immutable data is trivially safe to share between threads
//!    and maps directly onto the on-disk layout planned for stage 2.
//!
//! Frozen layout ("CSR" / compressed sparse row):
//!
//! ```text
//! terms:         ["apple", "bm25",  "rust"]        (sorted, binary-searched)
//! term_offsets:  [0,        2,       6,      7]    (len = terms + 1)
//! postings:      [p0, p1 | p2, p3, p4, p5 | p6]
//!                 apple    bm25             rust
//! ```
//!
//! Postings of term `i` are `postings[term_offsets[i]..term_offsets[i + 1]]`.
//! One big allocation instead of one `Vec` per term saves 24 bytes of `Vec`
//! header per term plus allocator overhead, and is cache-friendlier.

use std::collections::HashMap;

use super::{Posting, SegmentReader};
use crate::analyzer::{Analyzer, AnalyzerConfig};
use crate::types::{DocId, SegmentId};

/// Accumulates documents and produces an [`InMemorySegment`].
pub struct InMemoryIndexBuilder {
    analyzer: Analyzer,
    postings: HashMap<String, Vec<Posting>>,
    doc_lens: Vec<u32>,
    external_ids: Vec<String>,
    total_tokens: u64,
    /// Scratch buffer reused across documents to avoid re-allocating.
    token_buf: Vec<String>,
}

impl InMemoryIndexBuilder {
    /// Create a builder that analyzes documents with `analyzer`.
    pub fn new(analyzer: Analyzer) -> Self {
        InMemoryIndexBuilder {
            analyzer,
            postings: HashMap::new(),
            doc_lens: Vec::new(),
            external_ids: Vec::new(),
            total_tokens: 0,
            token_buf: Vec::new(),
        }
    }

    /// The analyzer documents are processed with (queries must use the same).
    pub fn analyzer(&self) -> &Analyzer {
        &self.analyzer
    }

    /// Number of documents added so far.
    pub fn num_docs(&self) -> u32 {
        self.doc_lens.len() as u32
    }

    /// Analyze and add one document. Returns the dense [`DocId`] assigned to it.
    ///
    /// Doc ids are handed out in insertion order (0, 1, 2, ...), so every
    /// postings list is automatically sorted by doc id: we only ever append
    /// the current (largest) id.
    ///
    /// # Panics
    /// If more than `u32::MAX` documents are added.
    pub fn add_document(&mut self, external_id: &str, text: &str) -> DocId {
        let doc = DocId(u32::try_from(self.doc_lens.len()).expect("too many documents"));

        // `std::mem::take` moves the buffer out of `self` (leaving an empty
        // Vec behind). That way we can borrow `self.analyzer` and mutate the
        // buffer at the same time without fighting the borrow checker.
        let mut tokens = std::mem::take(&mut self.token_buf);
        tokens.clear();
        self.analyzer.analyze_into(text, &mut tokens);

        let doc_len = tokens.len() as u32;
        self.doc_lens.push(doc_len);
        self.total_tokens += u64::from(doc_len);
        self.external_ids.push(external_id.to_string());

        // Count term frequencies: sort, then each run of equal tokens is one
        // term. `chunk_by` yields those runs as sub-slices.
        tokens.sort_unstable();
        for run in tokens.chunk_by(|a, b| a == b) {
            let posting = Posting {
                doc,
                tf: run.len() as u32,
            };
            let term = &run[0];
            // `get_mut` first so we only allocate a new `String` key for terms
            // we have never seen (`entry()` would need an owned key every time).
            match self.postings.get_mut(term) {
                Some(list) => list.push(posting),
                None => {
                    self.postings.insert(term.clone(), vec![posting]);
                }
            }
        }

        self.token_buf = tokens; // give the buffer back for the next document
        doc
    }

    /// Freeze into an immutable, searchable segment. Consumes the builder
    /// (`self` by value), so nobody can keep adding documents afterwards.
    pub fn build(self) -> InMemorySegment {
        // Collect (term, postings) pairs and sort by term.
        let mut entries: Vec<(String, Vec<Posting>)> = self.postings.into_iter().collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        let total_postings: usize = entries.iter().map(|(_, p)| p.len()).sum();
        let mut terms = Vec::with_capacity(entries.len());
        let mut term_offsets = Vec::with_capacity(entries.len() + 1);
        let mut postings = Vec::with_capacity(total_postings);
        term_offsets.push(0);
        for (term, list) in entries {
            terms.push(term);
            postings.extend_from_slice(&list);
            term_offsets.push(postings.len());
        }

        InMemorySegment {
            id: SegmentId(0),
            analyzer_config: self.analyzer.config().clone(),
            terms,
            term_offsets,
            postings,
            doc_lens: self.doc_lens,
            external_ids: self.external_ids,
            total_tokens: self.total_tokens,
        }
    }
}

/// An immutable, fully in-memory segment. See the module docs for the layout.
pub struct InMemorySegment {
    id: SegmentId,
    analyzer_config: AnalyzerConfig,
    terms: Vec<String>,
    term_offsets: Vec<usize>,
    postings: Vec<Posting>,
    doc_lens: Vec<u32>,
    external_ids: Vec<String>,
    total_tokens: u64,
}

impl InMemorySegment {
    /// The analyzer configuration used to build this segment.
    pub fn analyzer_config(&self) -> &AnalyzerConfig {
        &self.analyzer_config
    }

    /// Total number of postings (sum of all document frequencies).
    pub fn num_postings(&self) -> usize {
        self.postings.len()
    }

    /// All terms in sorted order, each with its postings slice. Stage 2's
    /// segment writer will consume this to serialize the segment.
    pub fn terms(&self) -> impl Iterator<Item = (&str, &[Posting])> + '_ {
        self.terms
            .iter()
            .enumerate()
            .map(move |(i, t)| (t.as_str(), self.postings_at(i)))
    }

    /// Index of `term` in the sorted dictionary, via binary search
    /// (O(log |terms|) string comparisons).
    fn term_ordinal(&self, term: &str) -> Option<usize> {
        self.terms.binary_search_by(|t| t.as_str().cmp(term)).ok()
    }

    fn postings_at(&self, ordinal: usize) -> &[Posting] {
        &self.postings[self.term_offsets[ordinal]..self.term_offsets[ordinal + 1]]
    }

    /// Postings of `term` as a slice (in-memory only; generic code should use
    /// [`SegmentReader::postings`]).
    pub fn postings_slice(&self, term: &str) -> Option<&[Posting]> {
        self.term_ordinal(term).map(|i| self.postings_at(i))
    }

    /// Estimated heap bytes owned by this segment, computed from the *capacity*
    /// of every container (what is actually reserved), not just its length.
    /// Allocator bookkeeping is not included; `strata-mem` measures the real
    /// allocator footprint for comparison.
    pub fn heap_size_bytes(&self) -> usize {
        use std::mem::size_of;
        let strings = |v: &Vec<String>| {
            v.capacity() * size_of::<String>() + v.iter().map(|s| s.capacity()).sum::<usize>()
        };
        strings(&self.terms)
            + strings(&self.external_ids)
            + self.term_offsets.capacity() * size_of::<usize>()
            + self.postings.capacity() * size_of::<Posting>()
            + self.doc_lens.capacity() * size_of::<u32>()
    }
}

impl SegmentReader for InMemorySegment {
    // For the in-memory segment, postings are a slice, so the iterator is
    // just "iterate the slice and copy each element out".
    type Postings<'a> = std::iter::Copied<std::slice::Iter<'a, Posting>>;

    fn segment_id(&self) -> SegmentId {
        self.id
    }

    fn num_docs(&self) -> u32 {
        self.doc_lens.len() as u32
    }

    fn total_tokens(&self) -> u64 {
        self.total_tokens
    }

    fn doc_len(&self, doc: DocId) -> u32 {
        self.doc_lens[doc.as_usize()]
    }

    fn external_id(&self, doc: DocId) -> &str {
        &self.external_ids[doc.as_usize()]
    }

    fn doc_freq(&self, term: &str) -> u32 {
        self.postings_slice(term).map_or(0, |p| p.len() as u32)
    }

    fn postings(&self, term: &str) -> Option<Self::Postings<'_>> {
        self.postings_slice(term).map(|p| p.iter().copied())
    }

    fn num_terms(&self) -> usize {
        self.terms.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> InMemorySegment {
        let mut b = InMemoryIndexBuilder::new(Analyzer::new(AnalyzerConfig::simple()));
        b.add_document("d0", "rust search rust");
        b.add_document("d1", "search engine");
        b.add_document("d2", "");
        b.add_document("d3", "Rust ENGINE engine engine");
        b.build()
    }

    fn p(doc: u32, tf: u32) -> Posting {
        Posting {
            doc: DocId(doc),
            tf,
        }
    }

    #[test]
    fn postings_have_doc_ids_and_term_frequencies() {
        let s = tiny();
        assert_eq!(s.postings_slice("rust").unwrap(), &[p(0, 2), p(3, 1)]);
        assert_eq!(s.postings_slice("search").unwrap(), &[p(0, 1), p(1, 1)]);
        assert_eq!(s.postings_slice("engine").unwrap(), &[p(1, 1), p(3, 3)]);
        assert!(s.postings_slice("missing").is_none());
    }

    #[test]
    fn doc_lengths_and_stats() {
        let s = tiny();
        assert_eq!(s.num_docs(), 4);
        assert_eq!(
            (0..4).map(|d| s.doc_len(DocId(d))).collect::<Vec<_>>(),
            vec![3, 2, 0, 4]
        );
        assert_eq!(s.total_tokens(), 9);
        assert_eq!(s.stats().avg_doc_len(), 9.0 / 4.0);
        assert_eq!(s.external_id(DocId(3)), "d3");
    }

    #[test]
    fn dictionary_is_sorted_and_doc_freq_matches() {
        let s = tiny();
        let terms: Vec<&str> = s.terms().map(|(t, _)| t).collect();
        assert_eq!(terms, vec!["engine", "rust", "search"]);
        assert_eq!(s.num_terms(), 3);
        assert_eq!(s.doc_freq("engine"), 2);
        assert_eq!(s.doc_freq("nope"), 0);
        assert_eq!(s.num_postings(), 6);
    }

    #[test]
    fn trait_iterator_matches_slice() {
        let s = tiny();
        let via_trait: Vec<Posting> = s.postings("rust").unwrap().collect();
        assert_eq!(via_trait, s.postings_slice("rust").unwrap());
    }

    #[test]
    fn segment_stores_analyzer_config() {
        assert_eq!(tiny().analyzer_config(), &AnalyzerConfig::simple());
    }

    #[test]
    fn empty_index() {
        let s = InMemoryIndexBuilder::new(Analyzer::new(AnalyzerConfig::simple())).build();
        assert_eq!(s.num_docs(), 0);
        assert_eq!(s.num_terms(), 0);
        assert_eq!(s.stats().avg_doc_len(), 1.0);
        assert!(s.postings("x").is_none());
    }
}
