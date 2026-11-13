# strata progress log

## Stage 1: Foundation (tag `stage-1`)
- Tests: 48 tests (`cargo test --workspace`), 10/10 consecutive green runs (`results/test_10x.txt`); clippy -D warnings + fmt clean.
- Headline: Anserini BEIR BM25 reproduced to 4 decimals on 13/15 cells, other 2 within 0.0013 (`results/tables.md`, `BENCHMARK.md`).
- Headline: metrics == pytrec_eval 0.5 on 10/10 runs (`results/crosscheck/`); rankings == Tantivy 0.27 (nDCG@10/R@100 identical, 5/5 datasets) (`results/parity/`).
- Caveats: in-memory only; exhaustive TAAT; nfcorpus/trec-covid residual gap unexplained (tokenizer edge cases suspected, not verified).
