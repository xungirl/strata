# strata

**A hybrid (lexical + vector) search database written in Rust, built from scratch, one stage at a time.**

strata is inspired by Lucene/Tantivy (segment architecture, inverted index, block-max WAND) and Qdrant (HNSW), but the engine is our own code: Tantivy shows up only as a comparison baseline in a separate bench crate. The project is built **incrementally over 10 stages**. Every number in this README was measured on one machine; see [BENCHMARK.md](BENCHMARK.md).

> Status: stages 1-3 of 10 are done. There's an analyzer, BM25, a BEIR evaluation harness cross-checked against `pytrec_eval`, a durable storage engine (WAL, compressed mmap segments, merges, deletes, snapshot readers; verified against the stage-1 index and a kill -9 chaos test), and a query engine. The query engine provides exact MaxScore and block-max WAND top-k, boolean/phrase/prefix queries, metadata filters and a query parser. There are no vectors yet; they come in stage 4.

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

### Stage 2: Storage engine (done)

**Goal.** Make the index durable and concurrent: immutable on-disk segments, a write-ahead log with crash recovery, background merges, deletes, and readers that see a consistent snapshot. Results must not change at all: the stage-1 in-memory index is the oracle.

**Decisions and alternatives considered.**

| decision | chosen | alternatives | why |
|---|---|---|---|
| segment files | one directory per segment, written once, fsynced, never modified; read via `mmap` | mutable B-tree / LSM memtable files; read with `pread` | immutability = lock-free readers and no half-modified files after a crash; mmap lets the OS cache pages (one `unsafe` call, documented) |
| term dictionary | `fst` crate: term → byte offset of its postings | sorted array + binary search; hash map | compact (shared prefixes and suffixes), ordered (prefix/range queries in stage 3), value = offset so no separate term-info file |
| postings | blocks of 128 docs: bit-packed doc gaps and tf-1; skip table with `last_doc`, `max_tf`, `min_doc_len` per block; positions as varints | varint everything; PForDelta; Roaring | bit packing is simple and fast to decode; the per-block `(max_tf, min_doc_len)` is a BM25 upper bound that needs no collection stats (feeds block-max WAND) |
| durability | WAL with CRC per record, group commit (one fsync per batch), ack only after fsync; commit = new segment + tombstones + atomic manifest rename + fsync of files *and directories* | fsync per op; no WAL (commit-only durability) | group commit amortizes the ~4.5 ms fsync of this SSD (measured); manifest rename is the single atomic commit point |
| deletes / updates | per-segment tombstone bitsets; upsert = delete + add; deleted docs keep counting in N/df until merged (Lucene semantics) | rewrite segments; exact live statistics | segments stay immutable; exact live df would need a scan per query |
| merges | adjacent segments of the same size tier (floor(log4 live docs)), 4 at a time, background thread, deletes that happen during a merge carried over | Lucene TieredMergePolicy (non-adjacent) | adjacency keeps document order, so tie-breaking and therefore results stay byte-identical |
| readers | `Arc<Snapshot>`; tombstones copy-on-write (`Arc::make_mut`); obsolete segments deleted when the last snapshot drops them | reader locks; epoch-based reclamation | simplest correct snapshot isolation in Rust: reference counting |
| scoring across segments | index-wide N, avgdl, df; global doc ids (segment base + doc) for tie-breaks | per-segment statistics | a document's score must not depend on which segment holds it |

**Built.** New crate `strata-storage`: `codec` (bit packing, varints), `segment` (writer, mmap reader, block cursor with `advance_to` / positions / shallow block lookup), `buffer` (write buffer with positions), `livedocs`, `wal`, `manifest`, `merge`, `index` (writer, commit, recovery, background merges), `snapshot` (multi-segment BM25). `strata-chaos` binary (kill -9 harness). `strata-eval run --engine disk`. `strata-bench tantivy --positions`. `scripts/run_stage2.sh` with load-average gating.

**Verified.**
- Proptests: segment files round-trip exactly against the in-memory index (postings, tf, lengths, ids, positions); `advance_to` equals a linear scan; a multi-segment index with random flushes, merges and deletes scores **bit-identically** to the in-memory oracle (deleted docs counted as in Lucene before merging, and excluded after a full merge).
- Unit tests: WAL torn tail truncated, corruption in an older WAL file rejected, tombstone checksum, snapshot isolation (an old snapshot still sees 4 segments and deleted docs after a merge; its directory disappears when it is dropped), orphan cleanup, recovery.
- BEIR: run files of the disk engine (3 segments, and force-merged) are **byte-identical** to stage 1 on all 5 datasets.
- Chaos: 100 random SIGKILLs during writes, flushes and merges. 113,480 acked adds + 2,639 acked deletes, **0 lost, 0 resurrected**. The negative control (ack before write) is caught (147 lost).

**Measured** (BENCHMARK.md, stage 2): index 3.7-4.8 B/posting with positions, within 1.02-1.24x of Tantivy with positions. Ingest 10.6k-22.2k docs/s with fsync per 1,000 docs; 224 docs/s with fsync per doc. The disk engine's exhaustive queries are 1.4-1.8x slower than in-memory at k=10. Recovery p50 23 ms.

**Problems hit and fixes.**
1. *Skip tables dominated small terms.* Half of scifact's postings file was per-term overhead (20 B skip entry + 20 B term info for terms with a single block). Moved term info inline (FST value = postings offset) and dropped skip tables for single-block terms. The existing `advance_to` proptest immediately caught the resulting bug: a single-block cursor's "last doc" is unknown, so the scan must be bounded.
2. *Orphan GC deleted live data.* Commit-time cleanup of "unreferenced" segment directories removed directories that an old snapshot (or a running merge) still used. Fixed: during operation only `Drop` of an obsolete segment deletes it; orphan GC runs only at open.
3. *Deleted docs stayed in the statistics after `force_merge`* when there was a single segment. Fixed: a single segment with deletes is rewritten too (expunge deletes).
4. *The chaos harness could report false losses*: a delete killed after its fsync but before its ACK may or may not have happened. The child now announces `TRY DEL` first and such ids are excluded. A *negative control* proves the harness detects real ack-before-persist bugs.
5. *Benchmark hygiene:* fiqa/scidocs timings first overlapped with a low-priority compile and were re-run (originals kept in `results/stage2/superseded/`). Every repeat records the 1-minute load average.

### Stage 3: Query engine (done)

**Goal.** Dynamic pruning that returns *exactly* the exhaustive top-k, and real queries: boolean, phrase, prefix, metadata filters, a query language. Measure latency against the exhaustive baseline.

**Decisions and alternatives considered.**

| decision | chosen | alternatives | why |
|---|---|---|---|
| pruning algorithms | block-max WAND **and** MaxScore, both exact; MaxScore is the default | WAND only; approximate pruning (score-at-a-time, early termination) | measured both. BMW lost to the TAAT baseline on these long BEIR queries; MaxScore won at k=10 on the three larger corpora |
| exactness under f32 | same summation order as TAAT, docs visited in global doc order (ties always lose), prune only if `bound * (1 + 1e-5) <= θ` | exact rational arithmetic; ignore rounding | bit-identical results with negligible lost pruning |
| block bounds | `score(max_tf, min_doc_len)` from the stage-2 skip table, cached per cursor; shallow lookup without decoding | per-block max score stored at index time | stats-free: stays valid across merges and global-IDF changes |
| boolean execution | document-at-a-time `Matcher` trait objects: term, phrase (positions), prefix (FST range, constant score, 1,024-term cap) | rewrite everything into term disjunctions | DAAT handles intersection, exclusion and positions naturally |
| phrase scoring | phrase frequency as tf, sum of term IDFs | span queries | same as Lucene's `PhraseQuery` |
| filters | per-doc columns (`fields.json/.bin`, segment format v2), evaluated lazily per candidate | bitset per filter; filter-driven iteration | zero cost without filters; selective filters are a known weak spot (measured) |
| query language | flat Lucene-style: `+must -not "phrase" pre* field:value field:[lo TO hi]` | full Lucene grammar with parentheses | enough for the experiments; no parentheses yet |

**Built.** New crate `strata-query` (`ast`, `parser`, `filter`, `wand` = BMW + MaxScore, `exec`, `search()` dispatch). Storage: metadata fields (keyword/int) through WAL, buffer, segments, merges, with kind validation before logging; `advance_to` binary-searches the skip table; shallow `block_for()`. `strata-eval run --strategy taat|wand|maxscore --bucket-field --filter ...`. `strata-qbench` (phrase/prefix/boolean/filter latency). `wand_debug` diagnostic example.

**Verified.**
- Proptest: BMW == MaxScore == DAAT boolean executor == exhaustive TAAT, **bit-identical**, over random skewed corpora with multiple segments, merges, deletes, filters and k ∈ {1..1000}. Mutation check: an unsafe bound (×0.7) makes it fail.
- Proptest: boolean/phrase/prefix/filter queries match a naive evaluator over raw tokens.
- BEIR: BMW and MaxScore run files byte-identical to stage 1 on 5/5 datasets, multi- and single-segment (20/20), and with 3 filter selectivities on 2 datasets (12/12 vs filtered TAAT).

**Measured.** k=10 p50, MaxScore vs exhaustive TAAT: trec-covid 1.71x faster (809 vs 1,383 µs), fiqa 1.23x, scidocs 1.09x, slower on scifact and nfcorpus. Block-max WAND was slower than TAAT in all k=10 configurations (0.47-0.71x). Filters: MaxScore is 2.3x faster than TAAT at 50% selectivity, slower at 1%. Phrase queries p50 157-207 µs.

**Problems hit and fixes.**
1. *First BMW was slower than exhaustive.* Profiling-by-counters (`wand_debug`) found three causes: cursors (with 1 KB decode buffers) were moved on every re-sort, so we now sort indexes; `advance_to` scanned the skip table linearly, so it now binary-searches; block bounds were recomputed, so they are now cached. Even then BMW did ~16k loop iterations per trec-covid query to fully score ~1.4k docs.
2. *So we implemented MaxScore*, which suits long queries with many low-IDF terms and became the default. This is an honest negative result for BMW on these corpora, not tuned away.
3. *Proptest found a single-block cursor bug* (stage 2 format change): `advance_to` past the last doc of a single-block term.
4. *A field-kind conflict could only be detected at flush time*, after the write was already acknowledged. Moved the check before the WAL append.

### Key metrics

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
| Storage parity | disk engine (3 segments and force-merged) returns byte-identical run files to the in-memory index on 5/5 datasets | `run_stage2.sh` + `compare_runs.py` + `cmp` | `results/stage2/parity/` |
| Crash consistency | 100 kill -9 iterations: 113,480 acked adds, 2,639 acked deletes, 0 lost, 0 resurrected; negative control caught (147 lost) | `strata-chaos run` | `results/stage2/chaos.json`, `chaos_negative_control.json` |
| Compressed index size | 3.7-4.8 B/posting incl. positions (trec-covid 53.8 MB); 1.02-1.24x Tantivy-with-positions | segment file sizes | `results/stage2/tables.md` |
| WAL group commit | 224 docs/s with 1 doc per fsync vs 12,224 docs/s with 1,000 (scifact) | `--batch` sweep, 3 builds | `results/stage2/scifact/strata-disk.batch*.json` |
| Recovery time | p50 23.4 ms, max 62.3 ms (open + WAL replay) | timed in chaos harness | `results/stage2/chaos.json` |
| Exact dynamic pruning | BMW and MaxScore run files byte-identical to exhaustive on 5/5 datasets × {multi, 1 segment} (20/20) and filtered (12/12) | proptest + `compare_runs.py` + `cmp` | `results/stage3/parity/` |
| MaxScore vs exhaustive, k=10 p50 | trec-covid 809 vs 1,383 µs (1.71x), fiqa 1.23x, scidocs 1.09x; slower on scifact/nfcorpus | `run_stage3.sh`, 3 builds | `results/stage3/tables.md` |
| Block-max WAND, k=10 p50 | 0.47-0.71x of TAAT in all configurations (slower); scores 0.8% of trec-covid query postings | same | `results/stage3/tables.md`, `wand_debug_trec-covid_k10.txt` |
| Structured queries (k=10 p50) | phrase 157-207 µs, boolean 258-283 µs, prefix 79-804 µs (p99 up to 18.6 ms) | `strata-qbench` | `results/stage3/qbench/` |
| Tests | 48 tests (unit + proptest vs brute force), 10/10 consecutive passes | `cargo test --workspace` ×10 | `results/test_10x.txt` |

### Next steps
Stage 4: vectors. Compute BEIR embeddings locally once (bge-small-en-v1.5), then build our own HNSW with NEON SIMD distances and int8 scalar quantization, plus filtered ANN. Measure recall@10 vs latency curves against brute force and an hnswlib baseline.

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
   │                                   │
   │ strata-storage (stage 2): WAL ─► write buffer ─► immutable mmap segments (FST, bit-packed
   │   postings + skip table, positions, tombstones) ─► manifest; background merges; Arc snapshots
   │                                   │
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
target/release/strata-eval run --dataset fiqa --engine disk [--force-merge]   # stage 2 durable index
target/release/strata-chaos run --dir data/chaos --iterations 100           # kill -9 harness
scripts/run_stage2.sh                    # every stage-2 number
target/release/strata-eval run --dataset trec-covid --engine disk --strategy maxscore   # stage 3 pruning
target/release/strata-qbench --index data/indexes/<dir> --queries data/fiqa/queries.jsonl
scripts/run_stage3.sh                    # every stage-3 number

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

## Limitations (honest list)

- **One writer process per index directory, not enforced** (no lock file). Durability is tested against process crashes (kill -9), **not power loss or OS crashes**. No checksums on segment files (only on WAL records and tombstone files).
- Deleted documents keep counting in N/df until merged (as in Lucene), so scores drift slightly with deletes.
- **Pruning gains are modest on these corpora** (MaxScore 1.1-1.7x at k=10 on the three larger datasets; BMW slower than exhaustive). Selective filters (1%) make pruning slower than exhaustive; there is no filter-driven iteration.
- Query language: flat only (no parentheses, no `AND`/`OR` keywords); prefix expansion capped at 1,024 terms; positions have no stopword gaps (differs from Lucene).
- No field weighting (title and body are concatenated, Anserini "flat"). Keyword-field dictionaries live in JSON (fine for low cardinality only).
- **Unexplained residual gap to Anserini** on nfcorpus (−0.0013 nDCG@10) and trec-covid (−0.0012). Total token counts differ by 0.07% / 0.01%. Our *guess* is tokenizer edge cases (Lucene's StandardTokenizer emits tokens for emoji-class symbols like ©/®, and the two may use different Unicode versions). I couldn't run Lucene here to diff tokens, so this is unverified.
- **Latency numbers are single-machine, single-thread, in-process** (no network, no serialization), on a laptop with heterogeneous cores and background processes. trec-covid has only 50 queries, so its "p99" is simply its maximum.
- **The Tantivy comparison is not a claim that strata is faster.** Tantivy's index is compressed (4.5–5.1× smaller), it does far more, and we did not profile it. strata's exhaustive scan over raw `u32` postings is cheap on these small corpora. The gap may well reverse at scale or once stage 2 adds compression.
- MS MARCO and other large BEIR sets were deliberately not run (disk budget).

## Roadmap

The project is built incrementally; each stage ends with tests, measured numbers and an updated README/BENCHMARK.

- [x] **1. Foundation:** analyzer, in-memory inverted index, BM25, BEIR eval + pytrec_eval cross-check, Tantivy/Anserini baselines.
- [x] **2. Storage engine:** immutable mmap segments (delta + varint/bitpacked postings, FST term dict), WAL + crash recovery (kill -9 chaos test), background tiered merge, snapshot-isolated readers, deletes via tombstones.
- [x] **3. Query engine:** WAND / block-max WAND, boolean/phrase/prefix queries, metadata filters, query parser; latency vs exhaustive.
- [ ] **4. Vector index:** own HNSW, SIMD distance, int8 scalar quantization (+ optional product quantization), filtered ANN; recall@10 vs latency curves vs brute force.
- [ ] **5. Hybrid retrieval:** RRF + weighted/normalized fusion, optional cross-encoder rerank; BEIR nDCG lexical vs dense vs hybrid.
- [ ] **6. Single-node service:** axum HTTP + gRPC (tonic) API, bulk ingest, Prometheus metrics, OpenTelemetry tracing, own load tester.
- [ ] **7. Distributed:** sharding (hash by doc id), coordinator scatter-gather top-k merge with global IDF, partial-result timeouts / hedged requests.
- [ ] **8. Replication:** Raft-replicated WAL per shard (openraft or own), leader failover, read replicas; chaos tests with 0 acknowledged-write loss.
- [ ] **9. Agent integration:** MCP server (search/ingest tools), agent memory use-case demo, eval of retrieval quality on agent tasks.
- [ ] **10. Ops & polish:** Docker/compose files (documented, not required to run), dashboards, capacity/perf report, architecture doc.

## What the next stage should watch out for

- Vectors are a separate structure from segments: decide how they get ids (external ids, so lexical and dense results can be fused) and how deletes/upserts reach the graph (HNSW cannot really delete; hide nodes).
- Recall must be measured against an *exact* ground truth computed with the same similarity and the same normalization. Cross-check that ground truth independently (numpy).
- Embedding computation is the most expensive step and runs on this laptop: do it once, cache it under `data/` (gitignored), and record the model, revision and versions.
- Keep the load-average discipline. GPU/MPS work and compiles distort timings.

## License

MIT
