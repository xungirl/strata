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
