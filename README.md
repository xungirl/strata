# strata

**A hybrid (lexical + vector) search database written in Rust, built from scratch, one stage at a time.**

strata is inspired by Lucene/Tantivy (segment architecture, inverted index, block-max WAND) and Qdrant (HNSW), but the engine is our own code: Tantivy shows up only as a comparison baseline in a separate bench crate. The project is built **incrementally over 10 stages**. Every number in this README was measured on one machine; see [BENCHMARK.md](BENCHMARK.md).

> Status: stages 1-9 of 10 are done. There's an analyzer, BM25, a BEIR evaluation harness cross-checked against `pytrec_eval`, a durable storage engine (WAL, compressed mmap segments, merges, deletes, snapshot readers; verified against the stage-1 index and a kill -9 chaos test), and a query engine. The query engine provides exact MaxScore and block-max WAND top-k, boolean/phrase/prefix queries, metadata filters and a query parser. It also has a vector index: its own HNSW with NEON distances, int8 quantization and filtered search, over locally computed bge-small embeddings. Hybrid retrieval (RRF and weighted fusion, plus a local cross-encoder re-ranker) is evaluated with paired significance tests. It runs as a service: HTTP + gRPC, Prometheus metrics, OpenTelemetry spans, and its own open- and closed-loop load generator. Stage 7 shards it behind a coordinator (two-phase scatter-gather with global IDF, verified equal to single node on BEIR; deadlines, partial results, hedged requests). Stage 8 replicates a shard with its own Raft implementation (simulation-tested, 0 acknowledged-write loss over 75 leader kills). Stage 9 exposes it to LLM agents as an MCP memory server, evaluated on the LoCoMo agent-memory benchmark. Stage 10 adds ops material (Docker, dashboards, capacity report, architecture doc).

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

### Stage 4: Vector index (done)

**Goal.** Dense retrieval with our own approximate-nearest-neighbour index, measured as recall@10 vs latency against exact search and against hnswlib.

**Decisions and alternatives considered.**

| decision | chosen | alternatives | why |
|---|---|---|---|
| embedding model | `BAAI/bge-small-en-v1.5` (384-d), computed once locally on the M2 GPU (MPS), cached in `data/embeddings` with model revision and versions | larger models; API embeddings | 37.7 min for all five corpora on this laptop (measured); small enough to iterate |
| similarity | dot product of L2-normalized vectors (= cosine) | L2 distance | one kernel, no square roots |
| ANN index | own HNSW (heuristic neighbour selection, M0 = 2M, mL = 1/ln M), single-threaded deterministic build | IVF, DiskANN/Vamana, hnswlib bindings | the most widely deployed graph index (Qdrant, Lucene, pgvector); deterministic builds make tests exact |
| SIMD | hand NEON for f32 (4 FMA accumulators); plain loop for int8 (LLVM emits SDOT) | `std::simd` (nightly), hand NEON everywhere | measured: hand f32 NEON 3.2x scalar; hand int8 NEON 2.1x *slower* than the compiler's SDOT |
| quantization | global int8 scalar quantization, graph traversal on codes, f32 rescoring of the ef candidates | per-dimension scales; product quantization | 4x smaller codes, 1.27-3.6x faster search, recall within 0.004 from ef=40; PQ not implemented |
| filtered ANN | filter during traversal (blocked nodes still route), exact scan of the allowed set below a size threshold | pre-filtering only; post-filtering | measured: graph search degrades badly at 1% selectivity while a scan of ~1% of docs is cheap |

**Built.** New crate `strata-vector`: `distance` (scalar + NEON, the only `unsafe` code, documented and proptested), `vectors`, `quant` (SQ8), `flat` (exact top-k), `hnsw` (build, search, SQ8 search with rescoring, filtered search with brute-force fallback, save/load), `embeddings`; `strata-vbench`; `scripts/embed_beir.py`; `scripts/hnswlib_baseline.py`; `scripts/crosscheck_exact_knn.py`.

**Verified.** NEON kernels vs scalar (proptest: f32 within 1e-4, int8 exact). HNSW: recall > 0.97 on random data, determinism, save/load round trip, filtered results only contain allowed ids, brute-force fallback is exact, SQ8 recall > 0.95. Our exact dense top-10 matches numpy on 300/300, 648/648 and 1000/1000 queries (scifact, fiqa, scidocs). The 6 set differences on nfcorpus/trec-covid are all ties at rank 10. Recall vs hnswlib is within 0.0125 at every ef.

**Measured.** trec-covid (171k docs): exact search p50 5.9 ms; HNSW ef=160 recall@10 1.0000 at 515 µs (11.5x); SQ8 202 µs (29x). Build 90 s single-threaded. Dense nDCG@10 beats BM25 on all five datasets (e.g. trec-covid 0.7578 vs 0.5943). Filtered ANN at 1% selectivity: the graph takes 7-44 ms versus 0.26 ms for an exact scan of the allowed docs, so a fallback is essential.

**Problems hit and fixes.**
1. *Hand-written int8 NEON was slower than a plain loop.* The assembly showed LLVM compiles the scalar loop to `SDOT` (the dotprod extension is enabled by default on `aarch64-apple-darwin`, while the `vdotq_s32` intrinsic is still unstable). `dot_i8` now uses the loop; the hand kernel is kept and benchmarked for comparison.
2. *hnswlib looked surprisingly slow.* Reading its source: hnswlib 0.8.0 only has x86 SIMD kernels, so on ARM it runs generic loops. Documented rather than claimed as a win.
3. *Tie-induced "mismatches" against numpy* (nfcorpus has duplicate documents). Checked: every set mismatch is a tie at rank 10.
4. *Benchmark hygiene:* embeddings ran on the GPU while CPU benchmarks were held back; all stage-4 timing steps started below load 4.

### Stage 5: Hybrid retrieval (done)

**Goal.** Combine BM25 and dense retrieval, optionally re-rank with a cross-encoder, and find out *with significance tests* when it helps.

**Decisions and alternatives considered.**

| decision | chosen | alternatives | why |
|---|---|---|---|
| fusion | RRF (k=60) and weighted min-max / z-score (α=0.5), fixed a priori | tune α/k per dataset; learned fusion | tuning on test queries would inflate results; an α sweep is reported separately as an oracle analysis |
| missing documents | strictly below the run's lowest normalized score | 0; the run's minimum | lets α=1/α=0 reproduce the input rankings exactly (a bug we hit) |
| re-ranker | `cross-encoder/ms-marco-MiniLM-L-6-v2` on the RRF top 100, run offline in Python on MPS | ONNX/candle in Rust; bigger models | the cheapest well-known cross-encoder; Rust inference left for later |
| significance | paired randomization test (100k sign flips) + paired t-test (own Student-t CDF, cross-checked with SciPy) | report means only | trec-covid has 50 queries; differences of 0.01 are often noise |

**Built.** `strata-eval/src/fusion.rs`, `stats.rs`, the `strata-eval hybrid` and `strata-eval compare` subcommands, `scripts/rerank_cross_encoder.py`, `scripts/crosscheck_stats.py`, `scripts/run_stage5.sh`.

**Verified.** Hand-computed RRF and min-max examples; α=1/α=0 reproduce the input rankings exactly; t-distribution reference values (df=10 critical value, Cauchy, normal limit); our t-test equals `scipy.stats.ttest_rel` on all 5 datasets (to 1e-6).

**Measured.** nDCG@10. Best hybrid vs best single retriever: scifact 0.7382 (min-max) vs 0.7127 dense; nfcorpus 0.3638 (RRF) vs 0.3430; trec-covid 0.8051 (z-score) vs 0.7578. On fiqa and scidocs dense alone is best (0.4035, 0.2052). RRF − BM25 is +0.04 to +0.21 (p ≤ 1e-5 everywhere). The MS MARCO cross-encoder *lowered* nDCG@10 versus RRF on 4/5 datasets.

**Problems hit and fixes.**
1. *α=1 did not reproduce BM25* (nfcorpus 0.2985 vs 0.3202). Single-hit lists normalized to 0 and tied with every non-retrieved doc. Fixed and tested.
2. *The driver script was edited while bash was executing it*, so bash read a shifted file and failed at the very end. The results of that run were complete. Re-ran the script fully (re-ranking cached) after the fusion fix; lesson recorded.
3. *A negative result kept as-is*: the re-ranker hurts on these corpora. It was not tuned away or hidden.

### Stage 6: Single-node service (done)

**Goal.** Run the engine as a network service with production basics (two APIs, bulk ingest, metrics, tracing) and measure it under load honestly.

**Decisions and alternatives considered.**

| decision | chosen | alternatives | why |
|---|---|---|---|
| HTTP | axum 0.8, JSON, engine calls in `spawn_blocking` | actix-web; doing index work on async threads | fsync and CPU-heavy search must not stall the async workers |
| gRPC | tonic 0.14 from `proto/strata.proto`, compiled by `protox` in `build.rs` | `protoc` + prost-build | no system protoc needed (pure Rust build) |
| metrics | `prometheus` crate: request counter and latency histogram by route (17 exponential buckets from 50 µs), docs/segments/vectors gauges | metrics + exporter crates | simple, standard text exposition |
| tracing | `tracing` + `tracing-opentelemetry`; OTLP/gRPC exporter or a stdout exporter | logs only | spans for ingest/search; verified with the stdout exporter |
| vectors in the service | append-only CRC'd `vectors.log` (fsynced) + in-memory HNSW rebuilt at start; upsert = new node + hide the old one | vectors in the lexical WAL; persisted graph | simple and durable; HNSW cannot delete |
| load generator | own `strata-load`: closed loop (N workers) and open loop (fixed-rate schedule, latency from the scheduled time, send lag recorded) | wrk / k6 | open-loop measurement without coordinated omission, plus hybrid queries with vectors |

**Built.** New crate `strata-server`: `engine` (lexical / dense / hybrid search, ingest, delete, commit), `vector_store`, `http`, `grpc`, `metrics`, `telemetry`; binaries `strata-server` and `strata-load`; `scripts/run_stage6.sh`.

**Verified.** End-to-end API tests on ephemeral ports: HTTP ingest, visibility after commit, query language with filters, dense and hybrid search, 400 errors, delete hides both the lexical doc and its vector, `/metrics` content. gRPC ingest, commit, search and error codes. Vector store restart with a torn tail. OpenTelemetry spans observed with the stdout exporter.

**Measured** (fiqa, localhost, generator on the same 8 cores): closed-loop lexical 1,827 req/s with 1 client (p50 0.48 ms) up to about 11,800 req/s with 64 clients (p50 5.1 ms); hybrid up to about 4,990 req/s. Open loop at 4,000 req/s: p50 0.47 ms, p99 1.5 ms, generator send-lag p99 0.1 ms. HTTP ingest with vectors 2,221 docs/s, bound by single-threaded HNSW inserts.

**Problems hit and fixes.**
1. *2 MB default body limit*: bulk batches with vectors were rejected. Raised the limit explicitly.
2. *The load generator lied, twice.* Timer granularity added ~1 ms to open-loop latencies. Then spinning on a tokio worker starved response handling (send-lag p99 31 ms). Fixed with a dedicated scheduler thread, and the send lag is now reported next to every result. Superseded runs are kept.
3. *Ingest is HNSW-bound.* Documented; parallel/batched graph inserts are future work.

### Stage 7: Distributed (done)

**Goal.** Split the index across shards behind a coordinator, and prove that sharded results equal single-node results. Then handle slow and dead shards: deadlines, partial results, and hedged requests measured against an injected slow replica.

**Decisions and alternatives considered.**

| decision | chosen | alternatives | why |
|---|---|---|---|
| partitioning | hash by doc id, FNV-1a % shards | range partitioning; consistent hashing | even spread, no directory service; FNV is deterministic across processes (Rust's default SipHash is randomly seeded) |
| scoring across shards | two phases: gather N / total tokens / df for the query terms, then search with those global statistics | local IDF per shard (one round trip, scores differ); a central statistics service | exact single-node scores (bit-identical), at the cost of one extra round trip |
| merge | each shard returns its top-k, coordinator sorts by score then id | ask shards for more than k | any global top-k doc is in its own shard's top-k, so k per shard is enough |
| slow shards | per-call deadline, `partial: true` + `failed_shards` | fail the whole request; wait forever | availability first, but say so explicitly |
| tail latency | hedged request to the next replica after a fixed delay (`tokio::select!`) | always send to all replicas; adaptive (p95-based) delay | a few % extra load instead of 2x |
| replication in this stage | coordinator writes every replica | consensus | deliberately simple; replaced by Raft in stage 8 |

**Built.**
- `ScoringStats` (`strata-query/src/stats.rs`), with stats-aware variants of MaxScore, BMW and the exhaustive scorers, plus `Snapshot::search_scored`.
- Server: `POST /v1/stats`, `global_stats` and `plain` fields on search requests.
- New binary `strata-coord`: scatter-gather, routing, deadlines, hedging, counters at `/coord/stats`.
- `strata-load run-queries` now runs BEIR queries through any HTTP endpoint.
- `scripts/run_stage7.sh` and `scripts/tie_analysis.py`.

**Verified.**
- A proptest: random hash partitions into 2-4 shards, scored with merged statistics, give bit-identical scores to one index.
- An end-to-end engine test, with a negative control showing that local statistics give different scores.
- On BEIR (5 datasets, all test queries, k=1000), a 4-shard cluster gives the same nDCG@10 / R@100 / R@1000 / MRR@10 as the stage-1 run, to 6 decimals. Top-100 lists are identical for 100% of queries, and score difference is 0.

**Measured** (fiqa, 2 shards × 2 replicas, open loop 200 req/s, 3 runs). One replica delays 5% of its searches by 50 ms:
- Without hedging, p99 rises from 9.0 ms (healthy) to 56.3 ms.
- Hedging after 5 ms brings p99 back to 9.3 ms, for 1.67% extra shard requests.
- A shard that is 3 s slow behind a 500 ms deadline still gets an answer in 0.50 s, flagged partial.

**Problems hit and fixes.**
1. *Run files were not byte-identical to stage 1.* 133 of 2,306 queries differ in their top-1000 list. I wrote `tie_analysis.py` to prove that every difference is a different choice among equal-score documents at rank 1000 (each shard breaks ties on its own). Metrics are unaffected.
2. *The healthy baseline is noisy at p99.9* (58.9 ± 23.8 ms) because the laptop is shared with other workloads. So I compare configurations by p99, and say so.

### Stage 8: Replication (done)

**Goal.** Replicate a shard with consensus, so that losing the leader loses no acknowledged write. Measure failover over many leader kills and check reads for linearizability.

**Decisions and alternatives considered.**

| decision | chosen | alternatives | why |
|---|---|---|---|
| consensus | own Raft, written from the paper as a pure state machine (`tick` / `step` / `propose` → `Ready`) | openraft; etcd-style library | small enough to explain line by line; I/O-free, so it can be tested deterministically with randomized schedules |
| ordering | `Ready` is handled as persist (fsync term/vote, then log) → send → apply | send and persist in parallel (etcd leader optimization) | the simplest order that is obviously safe across crashes |
| commit | leader no-op on election + commit only current-term entries (Figure 8) | — | required for safety |
| reads | ReadIndex on the leader (quorum heartbeat round, then wait for apply); plain reads on any replica are allowed to be stale | leader leases (clock assumptions); log every read | linearizable without clock assumptions |
| state machine | the strata `Index`; rebuilt by replaying the committed log at start; applied on a dedicated thread | snapshots + compaction | simplest correct; growth documented |
| transport | JSON over HTTP (axum + reqwest), fire-and-forget | gRPC streaming | Raft tolerates loss; easy to inspect |

**Built.** New crate `strata-raft`:
- `core` (the algorithm).
- `storage` (CRC'd log + atomic hard state, torn-tail recovery).
- `server` (replicated shard: writes, deletes, consistent reads, status).
- Binaries `strata-raft-node` and `strata-raft-chaos`.
- Scripts `run_stage8.sh` and `raft_mutations.sh`.

**Verified.**
- **Simulation:** 300 randomized simulated schedules (3 and 5 nodes; message loss, duplication, reordering, partitions, crash/restart from persisted state; batch sizes 1/2/64) check election safety, state-machine safety, commit durability, linearizable ReadIndex and liveness after healing. A deterministic replay of the paper's Figure 8 is included.
- **Mutation tests:** 6 of 6 injected bugs are detected.
- **Chaos test:** 75 SIGKILLs of the leader with 53,130 acknowledged ids: **0 lost**. 0 of 3,750 consistent reads on the new leader missed an id acknowledged before the kill.

**Measured.**
- Failover (kill → next write acknowledged by another replica): p50 261 ms, p90 448 ms, max 782 ms, with a 150-300 ms election timeout.
- Restarted-node catch-up: p50 257 ms.
- Follower plain reads were stale 3.9% of the time right after an ack, whereas consistent reads never were.

**Problems hit and fixes.**
1. *Stale ReadIndex after an election* (found by the simulation's read check): the leader now waits for at least its current commit index.
2. *Catch-up chains multiplied with every heartbeat* (>10,000 messages in flight in simulation). Only replies that advance `matched` now continue a chain, and stale rejections are ignored.
3. *Failover grew to 17 s in the first chaos run*: a restarted node replayed its log under the Raft lock and could not vote. Apply now runs on its own thread.
4. *The harness lied twice.*
   - It counted timeouts as lost writes (2,713 "lost" that every replica actually had).
   - It counted the dead leader's last response as the end of failover (10-30 ms, impossible).

   Both were fixed, and the superseded runs are kept with explanations.
5. *The simulation first missed 3 of 6 injected bugs.* Partitions were made longer and batch sizes varied, and a Figure-8 replay test was added.

### Stage 9: Agent integration (done)

**Goal.** Make strata usable by LLM agents as long-term memory through the Model Context Protocol, and measure retrieval quality on a real agent-memory benchmark.

**Decisions and alternatives considered.**

| decision | chosen | alternatives | why |
|---|---|---|---|
| protocol | MCP over stdio via the official Rust SDK `rmcp` 3.10 (`#[tool_router]`, structured JSON output with schemas) | hand-written JSON-RPC; HTTP transport | spec-compliant handshake and schemas for free; stdio is what desktop agents launch |
| tools | `remember`, `remember_many`, `recall` (plain question + tag filter), `search` (query language), `forget`, `memory_stats` | a single generic `query` tool | agents pick tools by description; `recall` hides query syntax, while `search` exposes it for power use |
| memory text | append-only, fsynced `memories.jsonl` next to the index | store text in the index | the index stores only terms and fields; hits return their text |
| evaluation | LoCoMo evidence retrieval (does recall return the turns that hold the answer?), scored with the BEIR harness | LLM answer accuracy with an LLM judge | measures what strata controls; needs no LLM, key or budget |
| client | an independent stdlib-Python MCP client | the server's own SDK | tests the wire protocol, not the SDK against itself |

**Built.**
- Binary `strata-mcp`, plus an end-to-end test that drives the real binary over JSON-RPC (handshake, every tool, tag filter, restart durability, stdout purity).
- Scripts: `locomo_to_beir.py`, `mcp_locomo_eval.py`, `locomo_dense_runs.py`, `run_stage9.sh`, `make_tables_stage9.py`.

**Measured** (LoCoMo: 5,882 dialogue turns as memories, 1,532 questions):
- `recall` over MCP (BM25 + conversation tag filter): nDCG@10 0.494, R@100 0.806.
- Without the tag filter: 0.457 / 0.753.
- Exact bge-small dense: 0.451 / 0.873.
- Hybrid min-max α=0.5: **0.534 / 0.901**. All differences are significant except RRF's nDCG@10.
- `recall` round trip p50 0.44 ms, p99 0.98 ms.

**Problems hit and fixes.**
1. *The rmcp handler rebuilt its tool router on every call* by default (the stored router was unused; the compiler warned). It now uses `#[tool_handler(router = self.tool_router)]`.
2. *The protocol transcript contained dataset text.* LoCoMo is non-commercial and must not be redistributed, so the transcript now keeps message shapes and lengths only (and the history was rewritten before anything was shared).

Use it from an MCP client (for example Claude Code or Claude Desktop):

```json
{ "mcpServers": { "strata-memory": { "command": "/path/to/target/release/strata-mcp", "args": ["--dir", "/path/to/memory-dir"] } } }
```

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
| HNSW recall vs latency | trec-covid ef=160: recall@10 1.0000, p50 515 µs vs 5,908 µs exact (11.5x); SQ8 202 µs (29x) | `strata-vbench`, 3 passes | `results/stage4/tables.md` |
| HNSW vs hnswlib 0.8.0 (same M/efC, 1 thread, ARM) | recall within 0.0125; p50 1.4-6.1x lower; build 2.0-5.0x faster (hnswlib has no ARM SIMD) | `hnswlib_baseline.py` | `results/stage4/hnswlib/`, `reference/hnswlib_simd_macros.txt` |
| SIMD kernels (384-d dot) | f32 NEON 29.5 ns vs scalar 93.8 ns; int8 auto-vectorized SDOT 8.3 ns vs hand NEON 18.0 ns | microbenchmark, 2M dots | `results/stage4/*.json` |
| Dense retrieval (bge-small, exact) nDCG@10 | scifact 0.7127, nfcorpus 0.3430, fiqa 0.4035, scidocs 0.2052, trec-covid 0.7578 | `strata-vbench` + `metrics.rs` | `results/stage4/*.json` |
| Hybrid retrieval nDCG@10 (fixed RRF k=60 / α=0.5) | trec-covid 0.8051 vs dense 0.7578 vs BM25 0.5943; RRF − BM25 +0.04..+0.21 (p ≤ 1e-5, 5/5 datasets); dense alone best on fiqa/scidocs | `strata-eval hybrid`, paired randomization test | `results/stage5/tables.md` |
| Cross-encoder re-rank (MiniLM-L-6, top 100) | lowered nDCG@10 vs RRF on 4/5 datasets (scifact −0.0445, p=0.003); ~125 pairs/s on MPS | `rerank_cross_encoder.py` | `results/stage5/` |
| Significance test correctness | paired t-test equals SciPy `ttest_rel` on 5/5 datasets | `crosscheck_stats.py` | `results/stage5/crosscheck/` |
| Service throughput (fiqa, localhost, shared 8 cores) | lexical 11,840 ± 93 req/s @64 clients (p50 5.1 ms); 1,827 req/s @1 client (p50 0.48 ms); hybrid 4,989 req/s @64 | `strata-load` closed loop, 3 runs | `results/stage6/tables.md` |
| Open-loop latency at 4,000 req/s | p50 0.47 ms, p99 1.53 ms, p99.9 7.5 ms; generator send-lag p99 0.10 ms | `strata-load --loop open`, 3 runs | `results/stage6/load/open.*.json` |
| Structured queries (k=10 p50) | phrase 157-207 µs, boolean 258-283 µs, prefix 79-804 µs (p99 up to 18.6 ms) | `strata-qbench` | `results/stage3/qbench/` |
| Sharded = single node (4 shards, global IDF) | same nDCG@10 / R@100 / R@1000 / MRR@10 to 6 decimals on 5/5 datasets; top-100 identical for 100% of queries; top-1000 differences are only cutoff ties (133/2,306 queries) | `run_stage7.sh` + `compare_runs.py` + `tie_analysis.py` | `results/stage7/parity/` |
| Hedged requests (1 replica slow 5% × 50 ms) | p99 56.3 → 9.3 ms (healthy 9.0 ms) with 1.67% extra shard requests (hedge at 5 ms) | `strata-load --loop open` 200 req/s, 3 runs | `results/stage7/tables.md` |
| Partial results | 3 s-slow shard behind a 500 ms deadline: answer in 0.50-0.51 s, `partial: true` | `curl` ×3 | `results/stage7/partial/` |
| Raft chaos (3 replicas, SIGKILL the leader) | 75 kills, 53,130 acked ids, **0 lost**; 0/3,750 consistent reads on the new leader missed a pre-kill ack | `strata-raft-chaos`, 3 runs × 25 | `results/stage8/chaos.*.json` |
| Raft failover (election timeout 150-300 ms) | p50 261 ms, p90 448 ms, max 782 ms (n=75); per-run p50 251 ± 18 ms | same | `results/stage8/tables.md` |
| Raft simulation sensitivity | 6/6 injected bugs (Figure 8, vote checks, ReadIndex, follower commit) detected | `scripts/raft_mutations.sh` | `results/stage8/mutations.txt` |
| Agent memory over MCP (LoCoMo, 1,532 questions) | `recall` (BM25 + tag filter) nDCG@10 0.494 / R@100 0.806; hybrid with bge-small 0.534 / 0.901; tag filter +0.038 nDCG@10 (p < 1e-5) | `mcp_locomo_eval.py` + `strata-eval` | `results/stage9/tables.md` |
| MCP `recall` latency (client-side, stdio) | p50 0.437 ± 0.006 ms, p99 0.98 ± 0.05 ms | 3 runs × 1,532 calls | `results/stage9/mcp.r*/mcp_eval.json` |
| Tests | 105 tests (unit + proptest vs brute force / single index, end-to-end API and MCP tests, Raft simulation), 10/10 consecutive passes at stage 9 | `cargo test --workspace` ×10 | `results/stage9/test_10x.txt` (earlier stages: `results/stage*/test_10x.txt`) |

### Next steps
Stage 10: ops and polish. A Dockerfile and compose file (documented; running them is not required), a Grafana dashboard for the Prometheus metrics, a capacity/performance report built only from measured numbers, and docs/ARCHITECTURE.md.

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
uv run --with sentence-transformers==3.3.1 --with torch python -I scripts/embed_beir.py   # embeddings (once)
target/release/strata-vbench --dataset fiqa --out results/stage4/fiqa.json
scripts/run_stage4.sh                    # every stage-4 number
target/release/strata-eval hybrid --dataset trec-covid --lexical <bm25.trec> --dense <dense.trec>
scripts/run_stage5.sh                    # every stage-5 number (re-ranking needs uv + torch)
target/release/strata-server --dir data/node --http 127.0.0.1:7700 --grpc 127.0.0.1:7701 --dim 384
target/release/strata-load search --url http://127.0.0.1:7700 --queries data/fiqa/queries.jsonl --loop open --rate 2000
scripts/run_stage6.sh                    # every stage-6 number

# stage 7: two shards behind a coordinator (global IDF, 1 s deadline, hedge after 5 ms)
target/release/strata-server --dir data/s0 --http 127.0.0.1:7711 &
target/release/strata-server --dir data/s1 --http 127.0.0.1:7712 &
target/release/strata-coord --listen 127.0.0.1:7800 --shards "http://127.0.0.1:7711;http://127.0.0.1:7712" --timeout-ms 1000 --hedge-ms 5
scripts/run_stage7.sh                    # every stage-7 number
scripts/run_stage8.sh                    # Raft chaos (3 x 25 leader kills)
scripts/raft_mutations.sh                # inject 6 Raft bugs, expect the simulation to catch them
scripts/run_stage9.sh                    # MCP agent memory on LoCoMo (needs data/locomo/locomo10.json)

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
- Vectors: single-threaded HNSW build; no PQ; filtered ANN switches strategies by a fixed threshold; the vector index is separate from the lexical segments (no shared WAL yet).
- Hybrid: fusion parameters are fixed, not tuned on dev splits; re-ranking runs offline in Python.
- Service: no auth, rate limiting or backpressure; lexical WAL and vector log are two separate fsyncs (not one atomic commit); HNSW is rebuilt on start; load generator and server share the machine.
- Distributed: all shards on one laptop (no real network or machine failures); replicas kept in sync by the coordinator writing to all of them (no consensus until stage 8); no rebalancing; hedging has no budget/limit; dense/hybrid merge across shards is exact only for exact vector search.
- Replication: Raft has no log compaction or snapshots (restart replays the whole log; the log grows forever), no membership changes, no pre-vote; the Raft shard is a separate binary not yet wired behind the stage-7 coordinator; only process kills were tested in the real cluster (partitions only in simulation); macOS fsync is not `F_FULLFSYNC`; linearizability is checked with targeted read checks, not a full history checker.
- Agent memory: the MCP server is lexical only (dense/hybrid results on LoCoMo are offline comparisons); one process per memory directory; no LLM-in-the-loop answer accuracy measured; LoCoMo has only 10 conversations, and the category meanings were not verified.
- MS MARCO and other large BEIR sets were deliberately not run (disk budget).

## Roadmap

The project is built incrementally; each stage ends with tests, measured numbers and an updated README/BENCHMARK.

- [x] **1. Foundation:** analyzer, in-memory inverted index, BM25, BEIR eval + pytrec_eval cross-check, Tantivy/Anserini baselines.
- [x] **2. Storage engine:** immutable mmap segments (delta + varint/bitpacked postings, FST term dict), WAL + crash recovery (kill -9 chaos test), background tiered merge, snapshot-isolated readers, deletes via tombstones.
- [x] **3. Query engine:** WAND / block-max WAND, boolean/phrase/prefix queries, metadata filters, query parser; latency vs exhaustive.
- [x] **4. Vector index:** own HNSW, SIMD distance, int8 scalar quantization (+ optional product quantization), filtered ANN; recall@10 vs latency curves vs brute force.
- [x] **5. Hybrid retrieval:** RRF + weighted/normalized fusion, optional cross-encoder rerank; BEIR nDCG lexical vs dense vs hybrid.
- [x] **6. Single-node service:** axum HTTP + gRPC (tonic) API, bulk ingest, Prometheus metrics, OpenTelemetry tracing, own load tester.
- [x] **7. Distributed:** sharding (hash by doc id), coordinator scatter-gather top-k merge with global IDF, partial-result timeouts / hedged requests.
- [x] **8. Replication:** Raft-replicated WAL per shard (openraft or own), leader failover, read replicas; chaos tests with 0 acknowledged-write loss.
- [x] **9. Agent integration:** MCP server (search/ingest tools), agent memory use-case demo, eval of retrieval quality on agent tasks.
- [ ] **10. Ops & polish:** Docker/compose files (documented, not required to run), dashboards, capacity/perf report, architecture doc.

## What the next stage should watch out for

- A capacity report may only combine measured numbers. Label every extrapolation as arithmetic, with its assumptions, and keep it separate from measurements.
- The Docker/compose files cannot be run here (Docker isn't used on this machine for this project). Say clearly that they are untested.
- Dashboard queries must match the metric names the server actually exports (check `/metrics` output in `results/stage6/metrics_after_load.txt`).

## License

MIT
