//! Document-at-a-time execution of general boolean queries: `must` /
//! `should` / `must_not` clauses over terms, phrases and prefixes, plus
//! metadata filters. Exhaustive (no pruning): every candidate is scored.
//!
//! Every clause compiles to a [`Matcher`], an iterator over matching doc ids
//! (ascending) that can also score its current doc. The boolean executor
//! combines matchers:
//!
//! ```text
//! candidates = intersection(must)            if there is a must clause
//!            = union(should)                 otherwise
//! reject     if any must_not matches, or a filter / tombstone says no
//! score      = sum of scores of matching must and should clauses (clause order)
//! ```

use anyhow::{bail, Result};
use fst::{IntoStreamer, Streamer};
use strata_core::index::SegmentReader;
use strata_core::scoring::{idf, Bm25Params, Bm25TermScorer};
use strata_core::search::{top_k, Hit};
use strata_core::types::DocId;
use strata_storage::segment::{DiskPostings, DiskSegment};
use strata_storage::snapshot::{Snapshot, SnapshotHit};

use crate::ast::{BoolQuery, Clause};
use crate::filter::SegmentFilter;
use crate::stats::ScoringStats;

/// Doc id meaning "no more documents".
pub const END: u32 = u32::MAX;

/// Maximum number of terms a prefix may expand to (Lucene's default
/// `maxClauseCount` is 1024 too). Beyond that the query is rejected
/// instead of silently truncated.
pub const MAX_PREFIX_TERMS: usize = 1024;

/// A clause positioned on a document.
trait Matcher {
    /// Move to the first matching doc >= `target` (no-op if already there).
    fn advance_to(&mut self, target: u32) -> u32;
    /// Score of the current doc.
    fn score(&mut self, doc_len: u32) -> f32;
}

// ----- term ---------------------------------------------------------------

struct TermMatcher<'a> {
    postings: DiskPostings<'a>,
    doc: u32,
    tf: u32,
    scorer: Bm25TermScorer,
}

impl<'a> TermMatcher<'a> {
    fn new(mut postings: DiskPostings<'a>, scorer: Bm25TermScorer) -> TermMatcher<'a> {
        let (doc, tf) = postings.next().map_or((END, 0), |p| (p.doc.0, p.tf));
        TermMatcher {
            postings,
            doc,
            tf,
            scorer,
        }
    }
}

impl Matcher for TermMatcher<'_> {
    fn advance_to(&mut self, target: u32) -> u32 {
        if self.doc < target {
            let (doc, tf) = self
                .postings
                .advance_to(target)
                .map_or((END, 0), |p| (p.doc.0, p.tf));
            self.doc = doc;
            self.tf = tf;
        }
        self.doc
    }
    fn score(&mut self, doc_len: u32) -> f32 {
        self.scorer.score(self.tf, doc_len)
    }
}

// ----- phrase -------------------------------------------------------------

/// Docs where the terms occur at consecutive positions. Scored with BM25
/// using the *phrase frequency* as tf and the sum of the terms' IDFs (as
/// Lucene's `PhraseQuery` does).
struct PhraseMatcher<'a> {
    parts: Vec<DiskPostings<'a>>,
    /// Current doc of each part (END when exhausted).
    docs: Vec<u32>,
    doc: u32,
    freq: u32,
    scorer: Bm25TermScorer,
    positions: Vec<Vec<u32>>,
}

impl<'a> PhraseMatcher<'a> {
    fn new(mut parts: Vec<DiskPostings<'a>>, scorer: Bm25TermScorer) -> PhraseMatcher<'a> {
        let docs = parts
            .iter_mut()
            .map(|p| p.next().map_or(END, |x| x.doc.0))
            .collect();
        let n = parts.len();
        let mut m = PhraseMatcher {
            parts,
            docs,
            doc: 0,
            freq: 0,
            scorer,
            positions: vec![Vec::new(); n],
        };
        m.find_from(0);
        m
    }

    /// Find the first doc >= target containing the phrase.
    fn find_from(&mut self, mut target: u32) {
        loop {
            // Leapfrog: bring every part to a common doc >= target.
            let mut agreed = true;
            for i in 0..self.parts.len() {
                if self.docs[i] < target {
                    self.docs[i] = self.parts[i].advance_to(target).map_or(END, |p| p.doc.0);
                }
                if self.docs[i] == END {
                    self.doc = END;
                    return;
                }
                if self.docs[i] > target {
                    target = self.docs[i];
                    agreed = false;
                }
            }
            if !agreed {
                continue;
            }
            // All parts on `target`: count positions p with part j at p + j.
            for (i, part) in self.parts.iter_mut().enumerate() {
                part.positions_into(&mut self.positions[i]);
            }
            let freq = self.positions[0]
                .iter()
                .filter(|&&p0| {
                    self.positions[1..]
                        .iter()
                        .enumerate()
                        .all(|(j, pos)| pos.binary_search(&(p0 + j as u32 + 1)).is_ok())
                })
                .count() as u32;
            if freq > 0 {
                self.doc = target;
                self.freq = freq;
                return;
            }
            target += 1;
        }
    }
}

impl Matcher for PhraseMatcher<'_> {
    fn advance_to(&mut self, target: u32) -> u32 {
        if self.doc < target {
            self.find_from(target);
        }
        self.doc
    }
    fn score(&mut self, doc_len: u32) -> f32 {
        self.scorer.score(self.freq, doc_len)
    }
}

// ----- prefix / disjunction of constant-score terms --------------------

/// Union of several terms' postings, constant score 1.0 per matching doc.
struct PrefixMatcher<'a> {
    parts: Vec<(DiskPostings<'a>, u32)>,
    doc: u32,
}

impl<'a> PrefixMatcher<'a> {
    fn new(parts: Vec<DiskPostings<'a>>) -> PrefixMatcher<'a> {
        let mut parts: Vec<(DiskPostings<'a>, u32)> = parts
            .into_iter()
            .map(|mut p| {
                let d = p.next().map_or(END, |x| x.doc.0);
                (p, d)
            })
            .collect();
        let doc = parts.iter_mut().map(|(_, d)| *d).min().unwrap_or(END);
        PrefixMatcher { parts, doc }
    }
}

impl Matcher for PrefixMatcher<'_> {
    fn advance_to(&mut self, target: u32) -> u32 {
        if self.doc >= target {
            return self.doc;
        }
        let mut min = END;
        for (p, d) in &mut self.parts {
            if *d < target {
                *d = p.advance_to(target).map_or(END, |x| x.doc.0);
            }
            min = min.min(*d);
        }
        self.doc = min;
        min
    }
    fn score(&mut self, _doc_len: u32) -> f32 {
        1.0
    }
}

// ----- compilation ----------------------------------------------------------

/// Index-wide scoring context.
struct Ctx<'s> {
    stats: &'s ScoringStats,
    params: Bm25Params,
}

impl Ctx<'_> {
    fn scorer(&self, idf_value: f32, count: u32) -> Bm25TermScorer {
        Bm25TermScorer::new(
            self.params,
            idf_value,
            count,
            self.stats.collection().avg_doc_len(),
        )
    }
    fn idf(&self, term: &str) -> f32 {
        idf(self.stats.num_docs, self.stats.doc_freq(term))
    }
}

/// Compile a clause for one segment. `None` = matches nothing here.
fn compile<'a>(
    ctx: &Ctx,
    seg: &'a DiskSegment,
    clause: &Clause,
) -> Result<Option<Box<dyn Matcher + 'a>>> {
    Ok(match clause {
        Clause::Term { term, count } => seg.cursor(term).map(|p| {
            Box::new(TermMatcher::new(p, ctx.scorer(ctx.idf(term), *count)))
                as Box<dyn Matcher + 'a>
        }),
        Clause::Phrase(terms) => {
            let parts: Option<Vec<DiskPostings<'a>>> =
                terms.iter().map(|t| seg.cursor(t)).collect();
            parts.map(|parts| {
                let idf_sum: f32 = terms.iter().map(|t| ctx.idf(t)).sum();
                Box::new(PhraseMatcher::new(parts, ctx.scorer(idf_sum, 1))) as Box<dyn Matcher + 'a>
            })
        }
        Clause::Prefix(prefix) => {
            let mut stream = seg.term_dict().range().ge(prefix.as_bytes()).into_stream();
            let mut parts = Vec::new();
            while let Some((term, offset)) = stream.next() {
                if !term.starts_with(prefix.as_bytes()) {
                    break; // sorted dictionary: past the prefix range
                }
                if parts.len() == MAX_PREFIX_TERMS {
                    bail!("prefix {prefix:?} matches more than {MAX_PREFIX_TERMS} terms");
                }
                parts.push(seg.postings_at(offset));
            }
            (!parts.is_empty())
                .then(|| Box::new(PrefixMatcher::new(parts)) as Box<dyn Matcher + 'a>)
        }
    })
}

/// Exhaustive DAAT execution of a general boolean query.
pub fn execute(
    snap: &Snapshot,
    q: &BoolQuery,
    params: Bm25Params,
    k: usize,
) -> Result<Vec<SnapshotHit>> {
    let local = ScoringStats::local(snap, q.scoring_terms().iter().map(String::as_str));
    execute_with_stats(snap, q, params, k, &local)
}

/// [`execute`] with explicit (e.g. cluster-wide) scoring statistics.
pub fn execute_with_stats(
    snap: &Snapshot,
    q: &BoolQuery,
    params: Bm25Params,
    k: usize,
    stats: &ScoringStats,
) -> Result<Vec<SnapshotHit>> {
    let ctx = Ctx { stats, params };
    let mut all: Vec<Hit> = Vec::new();
    for seg in snap.segments() {
        let disk = seg.segment.as_ref();
        let filter = SegmentFilter::new(seg, &q.filters);
        let mut must = Vec::new();
        for c in &q.must {
            match compile(&ctx, disk, c)? {
                Some(m) => must.push(m),
                None => {
                    must.clear();
                    must.push(Box::new(Empty) as Box<dyn Matcher>);
                    break; // a required clause that matches nothing here
                }
            }
        }
        let mut should = Vec::new();
        for c in &q.should {
            if let Some(m) = compile(&ctx, disk, c)? {
                should.push(m);
            }
        }
        let mut must_not = Vec::new();
        for c in &q.must_not {
            if let Some(m) = compile(&ctx, disk, c)? {
                must_not.push(m);
            }
        }
        let hits = run(disk, &filter, must, should, must_not, k);
        all.extend(hits.into_iter().map(|h| Hit {
            doc: DocId(seg.base + h.doc.0),
            score: h.score,
        }));
    }
    let hits = top_k(all, k);
    Ok(hits
        .into_iter()
        .map(|h| {
            let i = snap
                .segments()
                .iter()
                .rposition(|s| s.base <= h.doc.0)
                .expect("segment");
            SnapshotHit {
                segment: i,
                doc: DocId(h.doc.0 - snap.segments()[i].base),
                score: h.score,
            }
        })
        .collect())
}

/// A matcher that matches nothing.
struct Empty;
impl Matcher for Empty {
    fn advance_to(&mut self, _: u32) -> u32 {
        END
    }
    fn score(&mut self, _: u32) -> f32 {
        0.0
    }
}

fn run<'a>(
    seg: &DiskSegment,
    filter: &SegmentFilter,
    mut must: Vec<Box<dyn Matcher + 'a>>,
    mut should: Vec<Box<dyn Matcher + 'a>>,
    mut must_not: Vec<Box<dyn Matcher + 'a>>,
    k: usize,
) -> Vec<Hit> {
    let mut hits: Vec<Hit> = Vec::new();
    if must.is_empty() && should.is_empty() {
        return hits; // nothing to score (pure filter queries are not supported)
    }
    let mut target = 0u32;
    loop {
        // 1. Next candidate >= target.
        let cand = if !must.is_empty() {
            // Leapfrog intersection of required clauses.
            let mut doc = target;
            loop {
                let mut agreed = true;
                for m in must.iter_mut() {
                    let d = m.advance_to(doc);
                    if d == END {
                        return top_k(hits, k);
                    }
                    if d > doc {
                        doc = d;
                        agreed = false;
                    }
                }
                if agreed {
                    break doc;
                }
            }
        } else {
            let mut min = END;
            for m in should.iter_mut() {
                min = min.min(m.advance_to(target));
            }
            if min == END {
                return top_k(hits, k);
            }
            min
        };
        // 2. Exclusions.
        let excluded = must_not.iter_mut().any(|m| m.advance_to(cand) == cand);
        if !excluded && filter.allows(cand) {
            // 3. Score: required clauses, then optional ones on this doc.
            let dl = seg.doc_len(DocId(cand));
            let mut score = 0.0f32;
            for m in must.iter_mut() {
                score += m.score(dl);
            }
            for m in should.iter_mut() {
                if m.advance_to(cand) == cand {
                    score += m.score(dl);
                }
            }
            hits.push(Hit {
                doc: DocId(cand),
                score,
            });
            if hits.len() >= 4 * k.max(64) {
                hits = top_k(hits, k); // keep memory bounded
            }
        }
        if cand == END - 1 {
            return top_k(hits, k);
        }
        target = cand + 1;
    }
}
