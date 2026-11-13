//! Merging segments: rewrite several segments as one, dropping deleted docs.
//!
//! Like an LSM-tree compaction: many small immutable segments (one per
//! flush) would make every query visit many term dictionaries, and deleted
//! docs would waste space forever. A merge streams the union of the term
//! dictionaries (an FST union, in sorted order) and concatenates each term's
//! postings with doc ids remapped to the new, dense numbering.
//!
//! Sources are always *adjacent* segments merged in order, so the relative
//! order of surviving documents never changes. That keeps tie-breaking (and
//! therefore results) identical to the un-merged index.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use fst::map::OpBuilder;
use fst::Streamer;
use strata_core::analyzer::AnalyzerConfig;
use strata_core::index::SegmentReader;
use strata_core::types::{DocId, SegmentId};

use crate::livedocs::LiveDocs;
use crate::segment::{DiskSegment, PostingWithPositions, SegmentMeta, SegmentWriter};

/// Marker for "this doc was deleted and has no new id".
pub const DELETED: u32 = u32::MAX;

/// Merge `sources` (with their tombstones at merge start) into segment `id`.
/// Returns the new segment's metadata and, per source, the old -> new doc map.
pub fn merge_segments(
    sources: &[(Arc<DiskSegment>, Arc<LiveDocs>)],
    parent: &Path,
    id: SegmentId,
    analyzer: AnalyzerConfig,
) -> Result<(SegmentMeta, Vec<Vec<u32>>)> {
    // 1. New doc numbering: live docs, source by source, in order.
    let mut doc_maps = Vec::with_capacity(sources.len());
    let mut doc_lens = Vec::new();
    let mut ids = Vec::new();
    let mut fields = Vec::new();
    for (seg, live) in sources {
        let mut map = Vec::with_capacity(seg.num_docs() as usize);
        for d in 0..seg.num_docs() {
            if live.is_live(d) {
                map.push(doc_lens.len() as u32);
                doc_lens.push(seg.doc_len(DocId(d)));
                ids.push(seg.external_id(DocId(d)).to_string());
                fields.push(seg.fields().doc_fields(d));
            } else {
                map.push(DELETED);
            }
        }
        doc_maps.push(map);
    }

    // 2. Terms: stream the union of all term dictionaries in sorted order.
    let mut writer = SegmentWriter::create(parent, id, analyzer)?;
    let mut op = OpBuilder::new();
    for (seg, _) in sources {
        op = op.add(seg.term_dict());
    }
    let mut union = op.union();
    let mut postings: Vec<PostingWithPositions> = Vec::new();
    // `Streamer` is fst's "lending iterator": each item borrows from the
    // stream, so it is consumed with `while let` rather than `for`.
    while let Some((term, indexed)) = union.next() {
        let term = std::str::from_utf8(term)?.to_string();
        let mut parts: Vec<(usize, u64)> = indexed.iter().map(|iv| (iv.index, iv.value)).collect();
        parts.sort_unstable(); // by source index, so docs stay ascending
        postings.clear();
        for (src, offset) in parts {
            let map = &doc_maps[src];
            let mut cursor = sources[src].0.postings_at(offset);
            while let Some(p) = cursor.next() {
                let new_doc = map[p.doc.as_usize()];
                if new_doc != DELETED {
                    postings.push(PostingWithPositions {
                        doc: new_doc,
                        positions: cursor.positions(),
                    });
                }
            }
        }
        writer.add_term(&term, &postings, &doc_lens)?;
    }
    let meta = writer.finish(&doc_lens, &ids, &fields)?;
    Ok((meta, doc_maps))
}
