# strata

**A hybrid (lexical + vector) search database written in Rust, built from scratch, one stage at a time.**

strata is inspired by Lucene/Tantivy (segment architecture, inverted index, block-max WAND) and Qdrant (HNSW), but the engine is our own code: Tantivy shows up only as a comparison baseline in a separate bench crate. The project is built **incrementally over 10 stages**. This README describes where it stands now (**stage 1: foundation**), and every number in it was measured on one machine; see [BENCHMARK.md](BENCHMARK.md).

> Status: stage 1 of 10 is done. There's an analyzer, an in-memory inverted index, BM25 with exhaustive top-k, a BEIR evaluation harness cross-checked against `pytrec_eval`, and a Tantivy baseline. Nothing is persisted to disk yet and there are no vectors yet. Those come in stages 2 and 4.

---

## Project Journey

### Stage 1: Foundation (done)

**Goal.** Build a lexical retrieval core and an evaluation harness that are *provably correct*. Later stages (compressed segments, WAND, HNSW, hybrid fusion, distribution) will each be checked against these baselines, so they have to be trustworthy first.

**Decisions and alternatives considered.**

| decision | chosen | alternatives | why |
|---|---|---|---|
| analysis pipeline | UAX #29 tokenizer → possessive strip → lowercase → Lucene's 33 stopwords → **classic Porter** | regex/whitespace split; Porter2 (`rust-stemmers`) | Anserini's published BM25 numbers come from Lucene's `EnglishAnalyzer`. Comparing against them only makes sense if we produce the same terms |
| stemmer | **own port of Lucene's `PorterStemmer`** (default), Porter2 optional | `rust-stemmers` only | `rust-stemmers` ships only Porter2. Switching to classic Porter is what closed most of the gap to Anserini (measured, below) |
| index layout | builder `HashMap<term, Vec<Posting>>` frozen into a **CSR layout**: sorted term vec + one flat postings array + offsets | `Vec<Vec<Posting>>`; `BTreeMap`; HashMap dictionary | one allocation instead of one per term; same shape as the on-disk format in stage 2; sorted terms give FST/prefix queries for free later |
| storage abstraction | `SegmentReader` trait with a GAT postings iterator | `Box<dyn Iterator>`; concrete type | stage 2 can add mmap segments without touching scoring; static dispatch keeps the hot loop inlinable |
| BM25 | Lucene IDF `ln(1 + (N-df+0.5)/(df+0.5))`, (k1+1) kept, optional **Lucene 1-byte length quantization** | RSJ IDF (can go negative); BM25+ | always-positive IDF, comparable to Lucene/Tantivy. The quantization switch exists only to explain the differences from Anserini and Tantivy |
| retrieval | exhaustive **term-at-a-time** accumulation + size-k min-heap | DAAT, WAND | gives the exact answer, which stage 3's WAND must reproduce |
| collection stats | passed into the `Searcher` from outside (`with_stats`) | read from the segment | multi-segment (stage 2) and multi-shard (stage 7) ranking needs *global* IDF/avgdl |
| metrics | own trec_eval-compatible nDCG/Recall/MRR, **cross-checked with pytrec_eval** | call pytrec_eval directly | metrics run in Rust inside the harness, and the cross-check shows they're exact |

**Built.**
- `strata-core`: `analyzer`, `porter`, `index` (`SegmentReader`, `InMemoryIndexBuilder`, `InMemorySegment`), `scoring` (BM25, SmallFloat port), `search` (`Query`, `Searcher`, `top_k`).
- `strata-eval`: BEIR loader, metrics, TREC run I/O, latency stats, and two binaries: `strata-eval` (run/eval) and `strata-mem` (allocator-instrumented memory measurement).
- `strata-bench`: Tantivy 0.27 baseline, with `StrataTokenizer` so Tantivy indexes the exact same terms.
- `scripts/`: dataset download, pytrec_eval cross-check, run diff, the full benchmark matrix (`run_all.sh`), table generation.

**Verified.**
- 48 tests: hand-computed BM25 and nDCG/Recall/MRR examples; a **proptest** comparing index search with naive brute-force BM25 that never touches postings (256 random corpora per run); and Porter checked against the official 23,531-word vocabulary with **0 mismatches**.
- Metrics match **pytrec_eval 0.5** on every run checked (10 runs over 5 datasets). Means and per-query values differ by at most 4e-15 (`results/crosscheck/`).
- Same terms + same BM25 + same length encoding as Tantivy → **identical nDCG@10 and R@100 on all 5 datasets** and identical top-10 lists on 100% of queries for scifact, nfcorpus, fiqa and scidocs (45/50 on trec-covid). Score gaps are at most 1.5e-5, consistent with f32 summation order (`results/parity/`).
- With Lucene length quantization, strata reproduces Anserini's published BEIR BM25 numbers **to all 4 decimals** for nDCG@10, R@100 and R@1000 on scifact, fiqa and scidocs, and to within 0.0013 / 0.0012 nDCG@10 on nfcorpus / trec-covid (R@100 and R@1000 identical there too).

**Measured.** See the key metrics table below and [BENCHMARK.md](BENCHMARK.md).

**Problems hit and fixes.**
1. *Gap to Anserini of up to 0.005 nDCG@10.* Bisected it piece by piece. Token counts matched (scifact 838,125 vs Lucene's 838,128), so the tokenizer wasn't the cause. The stemmer was: `rust-stemmers` is Porter2, Lucene uses classic Porter. Ported Lucene's `PorterStemmer` and validated it on Porter's official vocabulary. Then emulated Lucene's lossy 1-byte document-length norms (`SmallFloat.intToByte4`), which closed the rest on 3 of 5 datasets.
2. *Metrics "almost" matching.* trec_eval re-sorts runs by score and breaks ties by **doc id descending**, uses linear gain, and with `-c` counts queries with no results as 0. All of that is implemented and tested, and scores are written with round-trip float formatting so ties survive the file.
3. *Tantivy ranked a few docs differently at first.* Expected: Tantivy also quantizes lengths. Turning on strata's Lucene-length mode made rankings identical, which turned the difference into a cross-validation.
4. *Allocating in the possessive filter.* The first version built a `String` per suffix check for every token. Replaced it with a zero-allocation `char_indices` check.
5. *Running Lucene locally for a token-level diff was not possible in this environment*, so the remaining nfcorpus/trec-covid gap is **not fully explained**. See Limitations.

### Key metrics (stage 1)

| metric | value | how measured | source file |
|---|---|---|---|
| Anserini BM25 reproduction | 13 of 15 (dataset × {nDCG@10, R@100, R@1000}) cells identical to 4 decimals; other 2 within 0.0013 | `strata-eval run --doc-length lucene` vs published Anserini regression pages | `results/tables.md`, `results/reference/anserini_beir_flat.md` |
| BM25 nDCG@10 (default, k1=0.9 b=0.4) | scifact 0.6777 · nfcorpus 0.3202 · fiqa 0.2353 · scidocs 0.1498 · trec-covid 0.5943 | `strata-eval run --dataset D` | `results/<D>/strata.k1_0.9.b_0.4.exactlen.porter.lucenestop.json` |
| Metric correctness | 10/10 runs match pytrec_eval 0.5 (per-query identical, means within 4e-15) | `scripts/crosscheck_pytrec.py` | `results/crosscheck/` |
| Ranking parity with Tantivy 0.27 | identical nDCG@10 & R@100 on 5/5 datasets; identical top-10 on 100% of queries for 4 datasets, 45/50 on trec-covid | `scripts/compare_runs.py` | `results/parity/` |
| Porter stemmer port | 23,531/23,531 words match Porter's official output | `cargo test -p strata-core porter` | `crates/strata-core/src/porter.rs` |
| Indexing throughput (1 thread) | 16.3k–34.1k docs/s (trec-covid 171k docs: 20.6k ± 1.5k docs/s) | wall time of build, 3 repeats | `results/<D>/*.json` (`repeats[].docs_per_sec`) |
| Query latency, exhaustive, k=10 (1 thread) | p50 4 µs (nfcorpus) … 761 µs (trec-covid); p99 ≤ 1.7 ms | per-query `Instant`, 3 repeats | `results/tables.md` |
| Index heap size | 9.3–10.7 bytes/posting; trec-covid 122.6 MB for 13.2M postings | allocator-instrumented `strata-mem` | `results/memory.tsv` |
| Tests | 48 tests (unit + proptest vs brute force), 10/10 consecutive passes | `cargo test --workspace` ×10 | `results/test_10x.txt` |

### Next steps (stage 2)
Immutable on-disk segments: delta + varint/bitpacked postings, FST term dictionary, mmap. Then a WAL with crash recovery (kill -9 chaos test), tiered background merge, snapshot-isolated readers and tombstone deletes. Re-run every benchmark here against the on-disk segment through the same `SegmentReader` trait. See [What stage 2 should watch out for](#what-stage-2-should-watch-out-for).

---

## Architecture

```
                         strata-core (no I/O, pure engine)
 ┌─────────────────────────────────────────────────────────────────────────┐
 │                                                                         │
 │  text ──► Analyzer ──────────► InMemoryIndexBuilder ──build()──┐        │
 │           (UAX#29, possessive,  HashMap<term, Vec<Posting>>    │        │
 │            lowercase, stop,     doc_lens, external ids         ▼        │
 │            Porter)                                      InMemorySegment │
 │                                                  sorted terms + CSR     │
 │                                                  postings + doc_lens    │
 │                                                          │ impl         │
 │                                                          ▼              │
 │  query ─► Analyzer ─► Query (bag of words) ─► Searcher<S: SegmentReader>│
 │                                               │ BM25TermScorer per term │
 │                                               │ TAAT accumulate         │
 │                                               │ size-k min-heap         │
 │                                               ▼                         │
 │                                            Vec<Hit>                     │
 └─────────────────────────────────────────────────────────────────────────┘
   ▲                                   ▲
   │ strata-eval                       │ strata-bench
   │  beir.rs (corpus/queries/qrels)   │  Tantivy 0.27 + StrataTokenizer
   │  metrics.rs (trec_eval-exact)     │  same metrics / run format
   │  harness.rs (TREC runs, latency)  │
   │  bin: strata-eval, strata-mem     │
   └── scripts/: download, pytrec_eval cross-check, compare_runs, run_all, make_tables
```

Crate layout: `crates/strata-core` (library), `crates/strata-eval` (library + two binaries), `crates/strata-bench` (binary, depends on Tantivy). The engine crate depends only on `unicode-segmentation`, `rust-stemmers` and `serde`.

## Design decisions and trade-offs

- **Dense segment-local `DocId(u32)`**, with external ids stored on the side. This keeps per-doc data in plain `Vec`s and lets postings delta-encode well in stage 2. `SegmentId` and `ShardId` newtypes exist already so later stages don't have to change signatures.
- **Immutable segments.** The builder is consumed by `build(self)`, so the compiler guarantees nobody adds to a frozen segment. Immutable data can be shared across threads without locks, which is the foundation for stage 2's snapshot readers.
- **Serializable `AnalyzerConfig`.** Segments record how they were analyzed, so query-time analysis can never silently drift from index-time analysis.
- **Exact lengths by default, Lucene quantization as an option.** Exact is the textbook formula. Quantization is a storage trade-off Lucene and Tantivy make (1 byte/doc), and we use it to reproduce their numbers. It made no consistent quality difference here (BENCHMARK.md: at most ±0.0012 nDCG@10, no consistent direction).
- **Exhaustive TAAT on purpose.** Simple, obviously correct, and the oracle for stage 3. The cost: O(N) accumulator per query (685 KB on trec-covid) and every posting is scored. Fine for these corpora, not for MS MARCO.
- **Uncompressed postings (8 bytes each).** 9.3–10.7 bytes per posting overall in memory, against roughly 1.8–2.4 bytes per posting for Tantivy's entire compressed index (4.5–5.1× smaller). Compression is stage 2's job.
- **Matching Anserini is not the same as "best".** Porter2 actually scores slightly *higher* than classic Porter on scifact/fiqa/scidocs and lower on trec-covid (BENCHMARK.md ablations), and k1=1.2/b=0.75 beats Anserini's 0.9/0.4 on all five datasets. We default to Porter for comparability, not quality.

## How to run

```bash
# toolchain: Rust stable (tested 1.94.0), uv (only for the pytrec_eval cross-check)
scripts/download_beir.sh scifact nfcorpus fiqa scidocs trec-covid   # ~540 MB into data/ (gitignored)

cargo test --workspace                                  # 48 tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check

cargo build --release
target/release/strata-eval run --dataset scifact                     # BM25 k1=0.9 b=0.4, 3 repeats
target/release/strata-eval run --dataset scifact --doc-length lucene # Anserini-parity mode
target/release/strata-eval run --dataset fiqa --k1 1.2 --b 0.75 --stemmer porter2
target/release/strata-eval eval --dataset scifact --run results/runs/scifact/<tag>.trec
target/release/strata-bench tantivy --dataset scifact [--tokenizer native]
target/release/strata-mem --datasets scifact nfcorpus

scripts/run_all.sh                       # everything in BENCHMARK.md (9.5 min on this M2)
python3 -I scripts/make_tables.py        # regenerate the tables from results/*.json
```

Library use (`crates/strata-core/examples/quickstart.rs`, run with `cargo run -p strata-core --example quickstart`):

```rust
use strata_core::{Analyzer, AnalyzerConfig, Bm25Params, InMemoryIndexBuilder, Query, Searcher, SegmentReader};

let mut b = InMemoryIndexBuilder::new(Analyzer::new(AnalyzerConfig::english()));
b.add_document("d1", "Rust search engines index postings");
b.add_document("d2", "BM25 ranks documents");
let segment = b.build();

let analyzer = Analyzer::new(segment.analyzer_config().clone());
let hits = Searcher::new(&segment, Bm25Params::ANSERINI)
    .search(&Query::bag_of_words(&analyzer, "searching engine"), 10);
println!("{} {}", segment.external_id(hits[0].doc), hits[0].score);
```

## Limitations (stage 1, honest list)

- **In-memory only, single segment, no persistence, no deletes, no concurrency.** The index is rebuilt from JSONL every run.
- **Exhaustive retrieval.** Query cost grows with total postings for the query terms. No WAND yet.
- **No positions**, so no phrase queries. No field weighting (title and body are concatenated, Anserini "flat").
- **Unexplained residual gap to Anserini** on nfcorpus (−0.0013 nDCG@10) and trec-covid (−0.0012). Total token counts differ by 0.07% / 0.01%. Our *guess* is tokenizer edge cases (Lucene's StandardTokenizer emits tokens for emoji-class symbols like ©/®, and the two may use different Unicode versions). I couldn't run Lucene here to diff tokens, so this is unverified.
- **`df` is still segment-local.** `with_stats` takes global N/avgdl but not global document frequencies.
- **Latency numbers are single-machine, single-thread, in-process** (no network, no serialization), on a laptop with heterogeneous cores and background processes. trec-covid has only 50 queries, so its "p99" is simply its maximum.
- **The Tantivy comparison is not a claim that strata is faster.** Tantivy's index is compressed (4.5–5.1× smaller), it does far more, and we did not profile it. strata's exhaustive scan over raw `u32` postings is cheap on these small corpora. The gap may well reverse at scale or once stage 2 adds compression.
- MS MARCO and other large BEIR sets were deliberately not run (disk budget).

## Roadmap

The project is built incrementally; each stage ends with tests, measured numbers and an updated README/BENCHMARK.

- [x] **1. Foundation:** analyzer, in-memory inverted index, BM25, BEIR eval + pytrec_eval cross-check, Tantivy/Anserini baselines.
- [ ] **2. Storage engine:** immutable mmap segments (delta + varint/bitpacked postings, FST term dict), WAL + crash recovery (kill -9 chaos test), background tiered merge, snapshot-isolated readers, deletes via tombstones.
- [ ] **3. Query engine:** WAND / block-max WAND, boolean/phrase/prefix queries, metadata filters, query parser; latency vs exhaustive.
- [ ] **4. Vector index:** own HNSW, SIMD distance, int8 scalar quantization (+ optional product quantization), filtered ANN; recall@10 vs latency curves vs brute force.
- [ ] **5. Hybrid retrieval:** RRF + weighted/normalized fusion, optional cross-encoder rerank; BEIR nDCG lexical vs dense vs hybrid.
- [ ] **6. Single-node service:** axum HTTP + gRPC (tonic) API, bulk ingest, Prometheus metrics, OpenTelemetry tracing, own load tester.
- [ ] **7. Distributed:** sharding (hash by doc id), coordinator scatter-gather top-k merge with global IDF, partial-result timeouts / hedged requests.
- [ ] **8. Replication:** Raft-replicated WAL per shard (openraft or own), leader failover, read replicas; chaos tests with 0 acknowledged-write loss.
- [ ] **9. Agent integration:** MCP server (search/ingest tools), agent memory use-case demo, eval of retrieval quality on agent tasks.
- [ ] **10. Ops & polish:** Docker/compose files (documented, not required to run), dashboards, capacity/perf report, architecture doc.

## What stage 2 should watch out for

- Keep `InMemorySegment` as the **oracle**: the on-disk segment must return bit-identical runs on all five datasets (`compare_runs.py`), and the proptest should run against both.
- **Decide where doc-length quantization lives.** Storing 1-byte norms like Lucene/Tantivy saves 3 bytes/doc. Results above show it's quality-neutral here, but it changes scores, so it should be a format-level decision recorded in segment metadata (next to `AnalyzerConfig`).
- **Global statistics across segments.** Once there are many segments, `N`, `avgdl` *and* `df` must be aggregated (deleted docs included or not: decide and document). Lucene counts deleted docs in df until merge.
- The `SegmentReader` GAT isn't object-safe. A multi-segment searcher will need generics or an enum over segment kinds rather than `Vec<Box<dyn SegmentReader>>`.
- Indexing is dominated by analysis (70–77% of build time, BENCHMARK.md). Parallel analysis plus per-thread segments is the natural speed-up and fits the segment design.
- Compression will slow exhaustive scans. Measure decode cost separately, and re-run the Tantivy comparison afterwards to see if the latency picture changes.
- Data on this machine: ~26 GB free disk. Keep segment files under `data/` or a tmp dir and clean them up in tests.

## License

MIT
