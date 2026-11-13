//! Integration tests for the storage engine. The in-memory index from stage 1
//! is the oracle: whatever goes through segments, merges, deletes and
//! recovery must score exactly like it.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use proptest::prelude::*;
use strata_core::analyzer::{Analyzer, AnalyzerConfig};
use strata_core::index::{InMemoryIndexBuilder, SegmentReader};
use strata_core::scoring::Bm25Params;
use strata_core::search::{Query, Searcher};
use strata_core::types::{DocId, SegmentId};
use strata_storage::buffer::WriteBuffer;
use strata_storage::segment::DiskSegment;
use strata_storage::{Index, IndexConfig};

const VOCAB: &[&str] = &[
    "alpha", "beta", "gamma", "delta", "eps", "zeta", "eta", "theta", "iota", "kappa",
];

struct TempDir(PathBuf);
impl TempDir {
    fn new(name: &str) -> TempDir {
        // Unique per test + process + a counter so parallel tests never collide.
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "strata-st-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn text(ids: &[usize]) -> String {
    ids.iter().map(|&i| VOCAB[i]).collect::<Vec<_>>().join(" ")
}

fn simple() -> AnalyzerConfig {
    AnalyzerConfig::simple()
}

fn config(flush_docs: usize, merge_factor: usize) -> IndexConfig {
    IndexConfig {
        analyzer: simple(),
        flush_docs,
        merge_factor,
        background_merges: false,
    }
}

/// Top-k from the in-memory oracle as (external id, score bits).
fn oracle(docs: &[(String, String)], query: &str, k: usize) -> Vec<(String, u32)> {
    let mut b = InMemoryIndexBuilder::new(Analyzer::new(simple()));
    for (id, t) in docs {
        b.add_document(id, t);
    }
    let seg = b.build();
    let q = Query::bag_of_words(&Analyzer::new(simple()), query);
    Searcher::new(&seg, Bm25Params::ANSERINI)
        .search(&q, k)
        .into_iter()
        .map(|h| (seg.external_id(h.doc).to_string(), h.score.to_bits()))
        .collect()
}

fn search_index(index: &Index, query: &str, k: usize) -> Vec<(String, u32)> {
    let snap = index.snapshot();
    let q = Query::bag_of_words(&Analyzer::new(simple()), query);
    snap.search(&q, Bm25Params::ANSERINI, k)
        .iter()
        .map(|h| (snap.external_id(h).to_string(), h.score.to_bits()))
        .collect()
}

fn corpus() -> impl Strategy<Value = Vec<Vec<usize>>> {
    prop::collection::vec(prop::collection::vec(0..VOCAB.len(), 0..40), 1..300)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Segment files round-trip: same postings, tf, doc lengths, ids, and
    /// positions equal to the token offsets in the text.
    #[test]
    fn segment_matches_in_memory_index(docs in corpus()) {
        let dir = TempDir::new("seg");
        let mut buf = WriteBuffer::new(Analyzer::new(simple()));
        let mut mem = InMemoryIndexBuilder::new(Analyzer::new(simple()));
        for (i, d) in docs.iter().enumerate() {
            buf.add(&format!("d{i}"), &text(d));
            mem.add_document(&format!("d{i}"), &text(d));
        }
        buf.flush(&dir.0, SegmentId(1)).unwrap();
        let disk = DiskSegment::open(&dir.0.join("seg-000001")).unwrap();
        let mem = mem.build();
        prop_assert_eq!(disk.num_docs(), mem.num_docs());
        prop_assert_eq!(disk.total_tokens(), mem.total_tokens());
        prop_assert_eq!(disk.num_terms(), mem.num_terms());
        for d in 0..mem.num_docs() {
            prop_assert_eq!(disk.doc_len(DocId(d)), mem.doc_len(DocId(d)));
            prop_assert_eq!(disk.external_id(DocId(d)), mem.external_id(DocId(d)));
            prop_assert_eq!(disk.doc_by_external_id(&format!("d{d}")), Some(DocId(d)));
        }
        for (t, term) in VOCAB.iter().enumerate() {
            let want: Vec<_> = mem.postings(term).map(|p| p.collect()).unwrap_or_default();
            let mut cur = match disk.cursor(term) { Some(c) => c, None => { prop_assert!(want.is_empty()); continue; } };
            let mut got = Vec::new();
            while let Some(p) = cur.next() {
                let expected: Vec<u32> = docs[p.doc.as_usize()].iter().enumerate()
                    .filter(|(_, &x)| x == t).map(|(i, _)| i as u32).collect();
                prop_assert_eq!(cur.positions(), expected);
                got.push(p);
            }
            prop_assert_eq!(got, want);
        }
    }

    /// `advance_to` == linear scan to the first doc >= target.
    #[test]
    fn advance_to_matches_linear_scan(docs in prop::collection::vec(prop::collection::vec(0..3usize, 1..4), 1..1500),
                                      targets in prop::collection::vec(0u32..1600, 1..40)) {
        let dir = TempDir::new("adv");
        let mut buf = WriteBuffer::new(Analyzer::new(simple()));
        for (i, d) in docs.iter().enumerate() { buf.add(&format!("d{i}"), &text(d)); }
        buf.flush(&dir.0, SegmentId(1)).unwrap();
        let disk = DiskSegment::open(&dir.0.join("seg-000001")).unwrap();
        let all: Vec<u32> = disk.cursor("alpha").map(|c| c.map(|p| p.doc.0).collect()).unwrap_or_default();
        let mut targets = targets; targets.sort();
        if let Some(mut cur) = disk.cursor("alpha") {
            let mut floor = 0; // advance_to never goes backwards
            for t in targets {
                let got = cur.advance_to(t).map(|p| p.doc.0);
                let want = all.iter().copied().find(|&d| d >= t.max(floor));
                prop_assert_eq!(got, want);
                match got { Some(d) => floor = d + 1, None => break }
            }
        }
    }

    /// Multi-segment index (flushes + merges) scores bit-identically to the
    /// single in-memory segment, with deletes applied.
    #[test]
    fn index_matches_oracle(docs in corpus(), flush in 1usize..60, factor in prop::sample::select(vec![0usize, 2, 3, 4]),
                            deletes in prop::collection::vec(any::<prop::sample::Index>(), 0..20),
                            query in prop::collection::vec(0..VOCAB.len(), 1..4), k in 1usize..40) {
        let dir = TempDir::new("idx");
        let (index, _) = Index::open(&dir.0, config(flush, factor)).unwrap();
        let all: Vec<(String, String)> = docs.iter().enumerate().map(|(i, d)| (format!("d{i}"), text(d))).collect();
        for chunk in all.chunks(7) { index.add_documents(chunk).unwrap(); }
        let deleted: std::collections::HashSet<usize> = deletes.iter().map(|ix| ix.index(all.len())).collect();
        let del_ids: Vec<String> = deleted.iter().map(|&i| all[i].0.clone()).collect();
        index.delete(&del_ids).unwrap();
        index.commit().unwrap();

        let qtext = text(&query);
        if factor == 0 {
            // No merges: deleted docs still count in N/df (Lucene semantics),
            // so the oracle indexes everything and we filter its results.
            let want: Vec<_> = oracle(&all, &qtext, all.len()).into_iter()
                .filter(|(id, _)| !del_ids.contains(id)).take(k).collect();
            prop_assert_eq!(search_index(&index, &qtext, k), want);
        }
        prop_assert_eq!(index.snapshot().num_live_docs() as usize, all.len() - deleted.len());
        // After merging everything, deleted docs are physically gone and no
        // longer count: the oracle is the in-memory index of live docs only.
        index.force_merge().unwrap();
        let live: Vec<_> = all.iter().filter(|(id, _)| !del_ids.contains(id)).cloned().collect();
        prop_assert_eq!(search_index(&index, &qtext, k), oracle(&live, &qtext, k));
        prop_assert!(index.snapshot().segments().len() <= 1);
    }
}

#[test]
fn upsert_replaces_and_counts_once() {
    let dir = TempDir::new("upsert");
    let (index, _) = Index::open(&dir.0, config(0, 0)).unwrap();
    index
        .add_documents(&[("a".into(), "alpha beta".into())])
        .unwrap();
    index.commit().unwrap();
    index
        .add_documents(&[("a".into(), "gamma".into())])
        .unwrap();
    index.commit().unwrap();
    assert_eq!(search_index(&index, "alpha", 10).len(), 0);
    assert_eq!(search_index(&index, "gamma", 10)[0].0, "a");
    assert_eq!(index.snapshot().num_live_docs(), 1);
}

#[test]
fn snapshots_are_isolated_from_later_writes_and_merges() {
    let dir = TempDir::new("snap");
    let (index, _) = Index::open(&dir.0, config(0, 0)).unwrap();
    for i in 0..4 {
        index
            .add_documents(&[(format!("d{i}"), "alpha".into())])
            .unwrap();
        index.commit().unwrap(); // one segment per doc
    }
    let old = index.snapshot();
    assert_eq!(old.segments().len(), 4);
    index.delete(&["d0".into()]).unwrap();
    index.commit().unwrap();
    index.force_merge().unwrap();
    let new = index.snapshot();
    assert_eq!(new.segments().len(), 1);
    assert_eq!(new.num_live_docs(), 3);
    // The old snapshot still sees all 4 docs in 4 segments, and its files
    // are still on disk even though the merge made them obsolete.
    assert_eq!(old.num_live_docs(), 4);
    assert!(old.find("d0").is_some());
    let old_dir = old.segments()[0].segment.dir().to_path_buf();
    assert!(old_dir.exists());
    drop(old);
    assert!(
        !old_dir.exists(),
        "obsolete segment removed after last snapshot dropped"
    );
}

#[test]
fn recovery_replays_uncommitted_wal() {
    let dir = TempDir::new("recover");
    {
        let (index, _) = Index::open(&dir.0, config(0, 0)).unwrap();
        index
            .add_documents(&[("a".into(), "alpha".into()), ("b".into(), "beta".into())])
            .unwrap();
        index.commit().unwrap();
        index
            .add_documents(&[("c".into(), "gamma".into())])
            .unwrap();
        index.delete(&["a".into()]).unwrap();
        // Dropped without commit: like a crash after the acks.
    }
    let (index, report) = Index::open(&dir.0, config(0, 0)).unwrap();
    assert_eq!(report.wal.records_replayed, 2);
    index.commit().unwrap();
    let snap = index.snapshot();
    assert!(snap.find("a").is_none());
    assert!(snap.find("b").is_some());
    assert!(snap.find("c").is_some());
}

#[test]
fn orphaned_segment_from_crashed_flush_is_removed() {
    let dir = TempDir::new("orphan");
    {
        let (index, _) = Index::open(&dir.0, config(0, 0)).unwrap();
        index
            .add_documents(&[("a".into(), "alpha".into())])
            .unwrap();
        index.commit().unwrap();
    }
    // A half-written segment directory with a fresh id, not in the manifest.
    let orphan = dir.0.join("segments").join("seg-000099");
    fs::create_dir_all(&orphan).unwrap();
    fs::write(orphan.join("postings.bin"), b"partial").unwrap();
    let (index, report) = Index::open(&dir.0, config(0, 0)).unwrap();
    assert_eq!(report.removed_orphans, 1);
    assert!(!orphan.exists());
    assert!(index.snapshot().find("a").is_some());
}

#[test]
fn background_merges_keep_results_identical() {
    let dir = TempDir::new("bg");
    let cfg = IndexConfig {
        analyzer: simple(),
        flush_docs: 25,
        merge_factor: 3,
        background_merges: true,
    };
    let (index, _) = Index::open(&dir.0, cfg).unwrap();
    let docs: Vec<(String, String)> = (0..1000)
        .map(|i| {
            (
                format!("d{i}"),
                text(&[i % 10, (i * 7) % 10, (i * 3) % 10, i % 4]),
            )
        })
        .collect();
    for chunk in docs.chunks(13) {
        index.add_documents(chunk).unwrap();
    }
    index.commit().unwrap();
    index.merge_pending().unwrap();
    assert!(index.snapshot().segments().len() < 1000 / 25);
    assert_eq!(
        search_index(&index, "alpha zeta kappa", 50),
        oracle(&docs, "alpha zeta kappa", 50)
    );
    let _ = Arc::strong_count(&index);
}
