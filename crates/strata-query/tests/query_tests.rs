//! Property tests for the query engine.
//!
//! 1. Block-max WAND must return *bit-identical* results to the exhaustive
//!    term-at-a-time search (same ids, order and f32 scores), across many
//!    segments, deletes, filters and k.
//! 2. The boolean executor (must / should / must_not / phrase / prefix /
//!    filters) must agree with a naive evaluator that works directly on the
//!    raw token lists.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use proptest::prelude::*;
use strata_core::analyzer::{Analyzer, AnalyzerConfig};
use strata_core::scoring::{idf, Bm25Params, Bm25TermScorer};
use strata_query::{search, BoolQuery, Clause, Filter, Strategy as Mode};
use strata_storage::{Document, FieldValue, Fields, Index, IndexConfig};

const VOCAB: &[&str] = &[
    "ant", "bee", "cat", "cow", "dog", "eel", "elk", "fox", "gnu", "hen", "owl", "pig",
];

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> TempDir {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "strata-q-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&p);
        TempDir(p)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fields_of(i: usize) -> Fields {
    let mut f = Fields::new();
    f.insert("bucket".into(), FieldValue::Int((i * 37 % 10) as i64));
    if !i.is_multiple_of(3) {
        f.insert(
            "kind".into(),
            FieldValue::Keyword(if i.is_multiple_of(2) { "even" } else { "odd" }.into()),
        );
    }
    f
}

/// Build an index with small flushes so there are many segments.
fn build(
    docs: &[Vec<usize>],
    flush: usize,
    merge_factor: usize,
    deletes: &[usize],
) -> (TempDir, Arc<Index>) {
    let dir = TempDir::new();
    let cfg = IndexConfig {
        analyzer: AnalyzerConfig::simple(),
        flush_docs: flush,
        merge_factor,
        background_merges: false,
    };
    let (index, _) = Index::open(&dir.0, cfg).unwrap();
    let all: Vec<Document> = docs
        .iter()
        .enumerate()
        .map(|(i, d)| Document {
            id: format!("d{i}"),
            text: d.iter().map(|&w| VOCAB[w]).collect::<Vec<_>>().join(" "),
            fields: fields_of(i),
        })
        .collect();
    for chunk in all.chunks(11) {
        index.add_docs(chunk).unwrap();
    }
    let del: Vec<String> = deletes
        .iter()
        .filter(|&&i| i < docs.len())
        .map(|i| format!("d{i}"))
        .collect();
    index.delete(&del).unwrap();
    index.commit().unwrap();
    (dir, index)
}

fn ids_scores(index: &Index, hits: &[strata_storage::SnapshotHit]) -> Vec<(String, u32)> {
    let snap = index.snapshot();
    hits.iter()
        .map(|h| (snap.external_id(h).to_string(), h.score.to_bits()))
        .collect()
}

fn filter_strategy() -> impl Strategy<Value = Vec<Filter>> {
    prop::collection::vec(
        prop_oneof![
            (0i64..10, 0i64..10).prop_map(|(a, b)| Filter::IntRange {
                field: "bucket".into(),
                lo: Some(a.min(b)),
                hi: Some(a.max(b))
            }),
            prop::sample::select(vec!["even", "odd", "none"]).prop_map(|v| Filter::KeywordEq {
                field: "kind".into(),
                value: v.into()
            }),
            (0i64..10).prop_map(|a| Filter::Not(Box::new(Filter::IntRange {
                field: "bucket".into(),
                lo: None,
                hi: Some(a)
            }))),
        ],
        0..3,
    )
}

fn corpus() -> impl Strategy<Value = Vec<Vec<usize>>> {
    // Skewed word distribution: low ids are frequent, giving long postings
    // lists (several 128-doc blocks) next to rare terms.
    prop::collection::vec(
        prop::collection::vec(
            (0usize..VOCAB.len() * VOCAB.len()).prop_map(|x| (x as f64).sqrt() as usize),
            0..30,
        ),
        1..700,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn wand_equals_exhaustive(docs in corpus(), flush in prop::sample::select(vec![50usize, 200, 100_000]),
                              deletes in prop::collection::vec(0usize..700, 0..40),
                              terms in prop::collection::btree_map(0..VOCAB.len(), 1u32..3, 1..5),
                              filters in filter_strategy(), k in prop::sample::select(vec![1usize, 3, 10, 50, 1000])) {
        let (_dir, index) = build(&docs, flush, 3, &deletes);
        let snap = index.snapshot();
        let q = BoolQuery {
            should: terms.iter().map(|(&t, &c)| Clause::Term { term: VOCAB[t].into(), count: c }).collect(),
            filters,
            ..Default::default()
        };
        let params = Bm25Params::ANSERINI;
        let exhaustive = search(&snap, &q, params, k, Mode::Exhaustive).unwrap();
        let wand = search(&snap, &q, params, k, Mode::BlockMaxWand).unwrap();
        prop_assert_eq!(ids_scores(&index, &wand), ids_scores(&index, &exhaustive));
        let maxscore = search(&snap, &q, params, k, Mode::Auto).unwrap();
        prop_assert_eq!(ids_scores(&index, &maxscore), ids_scores(&index, &exhaustive));
        // The DAAT boolean executor must agree too (same summation order).
        let daat = strata_query::exec::execute(&snap, &q, params, k).unwrap();
        prop_assert_eq!(ids_scores(&index, &daat), ids_scores(&index, &exhaustive));
    }

    #[test]
    fn boolean_queries_match_naive(docs in corpus(), deletes in prop::collection::vec(0usize..700, 0..20),
                                   must in prop::collection::vec(0..VOCAB.len(), 0..2),
                                   should in prop::collection::vec(0..VOCAB.len(), 0..3),
                                   must_not in prop::collection::vec(0..VOCAB.len(), 0..2),
                                   phrase in prop::option::of((0..VOCAB.len(), 0..VOCAB.len())),
                                   prefix in prop::option::of(prop::sample::select(vec!["c", "e", "co", "zz"])),
                                   filters in filter_strategy()) {
        // No merges: deleted docs keep counting in N/df, as the naive oracle assumes.
        let (_dir, index) = build(&docs, 150, 0, &deletes);
        let snap = index.snapshot();
        let group = |v: &[usize]| -> Vec<Clause> {
            let mut m: BTreeMap<usize, u32> = BTreeMap::new();
            for &t in v { *m.entry(t).or_default() += 1; }
            m.into_iter().map(|(t, c)| Clause::Term { term: VOCAB[t].into(), count: c }).collect()
        };
        let mut q = BoolQuery { must: group(&must), should: group(&should), must_not: group(&must_not), filters, };
        if let Some((a, b)) = phrase { q.should.push(Clause::Phrase(vec![VOCAB[a].into(), VOCAB[b].into()])); }
        if let Some(p) = prefix { q.must.push(Clause::Prefix(p.into())); }

        let n = docs.len();
        let got = search(&snap, &q, Bm25Params::ANSERINI, n, Mode::Auto).unwrap();
        let got: HashMap<String, f32> = got.iter().map(|h| (snap.external_id(h).to_string(), h.score)).collect();
        let want = naive(&docs, &deletes, &q);
        prop_assert_eq!(got.len(), want.len(), "result sets differ: got {:?} want {:?}", got, want);
        for (id, s) in &want {
            let g = got.get(id).copied();
            prop_assert!(g.is_some_and(|g| (f64::from(g) - s).abs() <= 1e-4 * s.max(1.0)), "{id}: got {g:?} want {s}");
        }
    }
}

/// Naive boolean evaluation over raw token lists (no index). Statistics
/// count every doc, deleted ones too, which matches an index whose deleted
/// docs have not been merged away (the test builds with merges off).
fn naive(docs: &[Vec<usize>], deletes: &[usize], q: &BoolQuery) -> HashMap<String, f64> {
    let n = docs.len() as u64;
    let total: usize = docs.iter().map(|d| d.len()).sum();
    let avgdl = (total as f64 / n as f64) as f32;
    let df = |w: &str| {
        docs.iter()
            .filter(|d| d.iter().any(|&x| VOCAB[x] == w))
            .count() as u64
    };
    let scorer = |idf_v: f32, c: u32| Bm25TermScorer::new(Bm25Params::ANSERINI, idf_v, c, avgdl);
    // Score of a clause in doc d, None if it does not match.
    let clause_score = |c: &Clause, d: &[usize]| -> Option<f64> {
        let dl = d.len() as u32;
        match c {
            Clause::Term { term, count } => {
                let tf = d.iter().filter(|&&x| VOCAB[x] == term).count() as u32;
                (tf > 0).then(|| f64::from(scorer(idf(n, df(term)), *count).score(tf, dl)))
            }
            Clause::Phrase(ts) => {
                let freq = (0..d.len().saturating_sub(ts.len() - 1))
                    .filter(|&p| ts.iter().enumerate().all(|(j, t)| VOCAB[d[p + j]] == t))
                    .count() as u32;
                let idf_sum: f32 = ts.iter().map(|t| idf(n, df(t))).sum();
                (freq > 0).then(|| f64::from(scorer(idf_sum, 1).score(freq, dl)))
            }
            Clause::Prefix(p) => d
                .iter()
                .any(|&x| VOCAB[x].starts_with(p.as_str()))
                .then_some(1.0),
        }
    };
    let passes = |f: &Filter, i: usize| -> bool {
        fn eval(f: &Filter, fields: &Fields) -> bool {
            match f {
                Filter::IntRange { field, lo, hi } => {
                    matches!(fields.get(field), Some(FieldValue::Int(v)) if lo.is_none_or(|l| *v >= l) && hi.is_none_or(|h| *v <= h))
                }
                Filter::KeywordEq { field, value } => {
                    matches!(fields.get(field), Some(FieldValue::Keyword(v)) if v == value)
                }
                Filter::Not(inner) => !eval(inner, fields),
            }
        }
        eval(f, &fields_of(i))
    };
    let mut out = HashMap::new();
    for (i, d) in docs.iter().enumerate() {
        if deletes.contains(&i) || !q.filters.iter().all(|f| passes(f, i)) {
            continue;
        }
        let must: Vec<Option<f64>> = q.must.iter().map(|c| clause_score(c, d)).collect();
        if must.iter().any(|s| s.is_none()) {
            continue;
        }
        if q.must_not.iter().any(|c| clause_score(c, d).is_some()) {
            continue;
        }
        let should: Vec<Option<f64>> = q.should.iter().map(|c| clause_score(c, d)).collect();
        if q.must.is_empty() && should.iter().all(|s| s.is_none()) {
            continue;
        }
        if q.must.is_empty() && q.should.is_empty() {
            continue;
        }
        let score: f64 = must.iter().chain(should.iter()).flatten().sum();
        out.insert(format!("d{i}"), score);
    }
    out
}

#[test]
fn parser_to_execution_end_to_end() {
    let docs: Vec<Vec<usize>> = vec![vec![2, 4, 2], vec![4, 2], vec![2, 3], vec![9, 10]];
    let (_dir, index) = build(&docs, 100, 0, &[]);
    let snap = index.snapshot();
    let a = Analyzer::new(AnalyzerConfig::simple());
    let run = |s: &str| -> Vec<String> {
        let q = strata_query::parse(s, &a).unwrap();
        search(&snap, &q, Bm25Params::ANSERINI, 10, Mode::Auto)
            .unwrap()
            .iter()
            .map(|h| snap.external_id(h).to_string())
            .collect()
    };
    assert_eq!(run(r#""cat dog""#), vec!["d0"]); // d1 has "dog cat", wrong order
    let mut both = run("+cat +dog");
    both.sort();
    assert_eq!(both, vec!["d0", "d1"]);
    assert_eq!(run("+cat -dog"), vec!["d2"]);
    let mut c = run("c*");
    c.sort();
    assert_eq!(c, vec!["d0", "d1", "d2"]); // cat, cow
    assert_eq!(run("cat bucket:[0 TO 0]"), vec!["d0"]); // bucket(0) = 0
}

#[test]
fn wand_actually_prunes_on_skewed_data() {
    // 3000 docs; "ant" (id 0) is in every doc, "pig" (11) in a few long docs.
    let docs: Vec<Vec<usize>> = (0..3000)
        .map(|i| {
            let mut d = vec![0; 1 + i % 5];
            if i.is_multiple_of(97) {
                d.extend([11, 11, 11]);
            }
            d
        })
        .collect();
    let (_dir, index) = build(&docs, 100_000, 0, &[]);
    let snap = index.snapshot();
    let terms = vec![("ant".to_string(), 1), ("pig".to_string(), 1)];
    let (hits, stats) = strata_query::wand_search(&snap, &terms, &[], Bm25Params::ANSERINI, 10);
    assert_eq!(hits.len(), 10);
    // Exhaustive scoring would score all 3000 matching docs. WAND has to
    // score docs until ten "pig" docs (one every 97) have raised the
    // threshold above anything "ant" alone can reach (~doc 900); after that
    // only "pig" docs are candidates.
    assert_eq!(stats.postings_total, 3000 + 31);
    assert!(
        stats.docs_scored < 1000,
        "scored {} docs",
        stats.docs_scored
    );
}
