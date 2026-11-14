# strata progress log

## Stage 1: Foundation (tag `stage-1`)
- Tests: 48 tests (`cargo test --workspace`), 10/10 consecutive green runs (`results/test_10x.txt`); clippy -D warnings + fmt clean.
- Headline: Anserini BEIR BM25 reproduced to 4 decimals on 13/15 cells, other 2 within 0.0013 (`results/tables.md`, `BENCHMARK.md`).
- Headline: metrics == pytrec_eval 0.5 on 10/10 runs (`results/crosscheck/`); rankings == Tantivy 0.27 (nDCG@10/R@100 identical, 5/5 datasets) (`results/parity/`).
- Caveats: in-memory only; exhaustive TAAT; nfcorpus/trec-covid residual gap unexplained (tokenizer edge cases suspected, not verified).

## Stage 2: Storage engine (tag `stage-2`)
- Tests: 67 tests, 10/10 consecutive green runs (`results/stage2/test_10x.txt`); clippy -D warnings + fmt clean.
- Headline: disk engine (WAL + bit-packed mmap segments + merges) returns byte-identical BEIR run files to stage 1 on 5/5 datasets (`results/stage2/parity/`).
- Headline: 100 kill -9 iterations, 113,480 acked adds + 2,639 acked deletes, 0 lost / 0 resurrected; negative control caught 147 lost (`results/stage2/chaos*.json`). Index 3.7-4.8 B/posting with positions, 1.02-1.24x Tantivy-with-positions (`results/stage2/tables.md`).
- Caveats: process-crash durability only (not power loss); no writer lock file; disk queries 1.4-1.8x slower than in-memory; fiqa/scidocs timings re-run after compile interference (originals in `results/stage2/superseded/`).

## Stage 3: Query engine (tag `stage-3`)
- Tests: 79 tests, 10/10 consecutive green runs (`results/stage3/test_10x.txt`); clippy -D warnings + fmt clean.
- Headline: block-max WAND and MaxScore are exact: run files byte-identical to exhaustive on 5/5 datasets × {multi, 1 segment} (20/20) and filtered (12/12) (`results/stage3/parity/`); proptest + mutation check.
- Headline: MaxScore k=10 p50 vs exhaustive TAAT: trec-covid 1.71x (809 vs 1,383 µs), fiqa 1.23x, scidocs 1.09x (`results/stage3/tables.md`).
- Caveats: BMW slower than TAAT in every k=10 config (0.47-0.71x); pruning slower than TAAT on tiny corpora, at k=1000 on fiqa/trec-covid, and with 1% filters; flat query language only.

## Stage 4: Vector index (tag `stage-4`)
- Tests: 87 tests, 10/10 consecutive green runs (`results/stage4/test_10x.txt`); clippy -D warnings + fmt clean.
- Headline: own HNSW on trec-covid (171k bge-small vectors): ef=160 recall@10 1.0000 at p50 515 µs vs 5,908 µs exact (11.5x); SQ8+rescore 202 µs (29x) (`results/stage4/tables.md`).
- Headline: recall within 0.0125 of hnswlib 0.8.0 at every ef; exact top-10 cross-checked against numpy (`results/stage4/crosscheck_exact_knn.txt`); f32 NEON 3.2x scalar, int8 auto-vectorized SDOT 2.1x faster than hand NEON.
- Caveats: hnswlib has no ARM SIMD (comparison flattering on this machine); single-threaded build; filtered ANN at 1% needs the brute-force fallback; no PQ.

## Stage 5: Hybrid retrieval (tag `stage-5`)
- Tests: 93 tests, 10/10 consecutive green runs (`results/stage5/test_10x.txt`); clippy -D warnings + fmt clean.
- Headline: hybrid (fixed RRF k=60 / α=0.5) beats BM25 by +0.04..+0.21 nDCG@10 on 5/5 datasets (p ≤ 1e-5); best hybrid beats dense on scifact/nfcorpus/trec-covid (trec-covid 0.8051 vs 0.7578) (`results/stage5/tables.md`).
- Headline: paired t-test matches SciPy on 5/5 datasets (`results/stage5/crosscheck/`).
- Caveats: dense alone is best on fiqa/scidocs (fusion with weak BM25 hurts, significant); MiniLM-L-6 cross-encoder lowered nDCG@10 vs RRF on 4/5 datasets; fusion params not tuned on dev splits; reranking offline in Python.

## Stage 6: Single-node service (tag `stage-6`)
- Tests: 96 tests, 10/10 consecutive green runs (`results/stage6/test_10x.txt`); clippy -D warnings + fmt clean.
- Headline: closed-loop lexical search 11,840 ± 93 req/s at 64 clients (p50 5.1 ms), 1,827 req/s at 1 client (p50 0.48 ms); hybrid ~4,990 req/s (`results/stage6/tables.md`).
- Headline: open loop at 4,000 req/s p50 0.47 ms / p99 1.53 ms with generator send-lag p99 0.10 ms; HTTP + gRPC APIs, Prometheus metrics, OTel spans verified via stdout exporter.
- Caveats: server and load generator share one 8-core laptop; open-loop generator fixed twice (superseded runs kept); ingest bound by single-threaded HNSW inserts (2,221 docs/s); OTLP export not tested against a collector; no auth/backpressure.
