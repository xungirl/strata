# strata benchmarks (stage 1)

All numbers below were measured by `scripts/run_all.sh` on the machine described here, on **2026-11-13 (UTC)**, at git commit `3c51408` (code identical to the stage-1 release; later commits only touched docs). The tables are generated from the raw JSON reports by `scripts/make_tables.py`; nothing was typed in by hand. Reference numbers for Anserini come from its published regression pages (URLs in [`results/reference/anserini_beir_flat.md`](results/reference/anserini_beir_flat.md)). They were **not** re-run here.

## Machine and toolchain

```
date: 2026-11-13T06:56:53Z
git: 3c51408
rustc: rustc 1.94.0 (4a4ef493e 2026-03-02)
cargo: cargo 1.94.0 (85eff7c80 2026-01-15)
os: macOS 15.4 (arm64)
cpu: Apple M2, 8 cores (4 performance + 4 efficiency)
memory: 24 GiB
tantivy: 0.27.0
build: cargo --release (opt-level 3, debug=1), single-threaded indexing and search
```

- Single machine, everything in-process: no network, no RPC, no serialization. Laptop-class hardware with performance/efficiency cores and normal background processes. No core pinning and no CPU frequency control.
- Indexing and search are **single-threaded** for both engines.
- "Latency" = analyzing the query text + retrieving top-k (`Searcher::search` / Tantivy `searcher.search`). It excludes turning hits into string ids and writing results.
- **3 repeats per configuration, in one process**: the index is rebuilt and every query re-run each time. Tables show mean ± sample standard deviation (n−1) over the 3 repeats. Per-repeat raw values are in `results/<dataset>/<tag>.json` (`repeats[]`), and stderr logs are in `results/logs/`.
- Percentiles use the nearest-rank method. trec-covid has only **50 queries, so its p99 is the maximum**. nfcorpus has 323 queries, 15 of which match no indexed term at all (e.g. "amnesia", "eggnog") and return nothing. They count as 0 in every metric (trec_eval `-c` semantics), just as in Anserini.
- Effectiveness is computed on the top 1000 hits per query (Anserini uses `-hits 1000`). Self-hits (doc id == query id) are removed, like Anserini's `-removeQuery`.

## Exact commands

```bash
scripts/download_beir.sh scifact nfcorpus fiqa scidocs trec-covid
scripts/run_all.sh                     # took 9.5 minutes wall-clock for all 5 datasets
python3 -I scripts/make_tables.py > results/tables.md
```

`run_all.sh` runs, per dataset:

| tag | command |
|---|---|
| `strata.k1_0.9.b_0.4.exactlen.porter.lucenestop` (default) | `strata-eval run --dataset D --repeats 3` |
| `strata.k1_0.9.b_0.4.lucenelen.porter.lucenestop` (Anserini-parity) | `... --doc-length lucene` |
| `strata.k1_1.2.b_0.75.exactlen.porter.lucenestop` | `... --k1 1.2 --b 0.75` |
| `strata.k1_1.2.b_0.75.lucenelen.porter.lucenestop` (Tantivy-parity) | `... --k1 1.2 --b 0.75 --doc-length lucene` |
| ablations | `... --stemmer porter2`, `--stemmer none`, `--stopwords none` |
| `tantivy-0.27.0.strata-analyzer` | `strata-bench tantivy --dataset D --tokenizer strata --repeats 3` |
| `tantivy-0.27.0.native-analyzer` | `strata-bench tantivy --dataset D --tokenizer native --repeats 3` |

plus `crosscheck_pytrec.py` (pytrec_eval 0.5 via `uv run --with pytrec_eval==0.5 --with numpy`), `compare_runs.py` at depth 10 and 100, and `strata-mem` for heap size.

Tests (`cargo test --workspace`, 48 tests) passed 10 runs in a row: [`results/test_10x.txt`](results/test_10x.txt).

## Results

### BM25 effectiveness vs. Anserini (k1=0.9, b=0.4)

| dataset | metric | Anserini (published) | strata exact lengths | strata Lucene-quantized lengths |
|---|---|---|---|---|
| scifact | nDCG@10 | 0.6789 | 0.6777 (-0.0012) | 0.6789 (=) |
| scifact | R@100 | 0.9253 | 0.9253 (=) | 0.9253 (=) |
| scifact | R@1000 | 0.9767 | 0.9767 (=) | 0.9767 (=) |
| nfcorpus | nDCG@10 | 0.3218 | 0.3202 (-0.0016) | 0.3205 (-0.0013) |
| nfcorpus | R@100 | 0.2457 | 0.2456 (-0.0001) | 0.2457 (=) |
| nfcorpus | R@1000 | 0.3704 | 0.3702 (-0.0002) | 0.3704 (=) |
| fiqa | nDCG@10 | 0.2361 | 0.2353 (-0.0008) | 0.2361 (=) |
| fiqa | R@100 | 0.5395 | 0.5400 (+0.0005) | 0.5395 (=) |
| fiqa | R@1000 | 0.7393 | 0.7403 (+0.0010) | 0.7393 (=) |
| scidocs | nDCG@10 | 0.1490 | 0.1498 (+0.0008) | 0.1490 (=) |
| scidocs | R@100 | 0.3477 | 0.3473 (-0.0004) | 0.3477 (=) |
| scidocs | R@1000 | 0.5638 | 0.5638 (=) | 0.5638 (=) |
| trec-covid | nDCG@10 | 0.5947 | 0.5943 (-0.0004) | 0.5935 (-0.0012) |
| trec-covid | R@100 | 0.1091 | 0.1090 (-0.0001) | 0.1091 (=) |
| trec-covid | R@1000 | 0.3955 | 0.3959 (+0.0004) | 0.3955 (=) |

| dataset | docs (strata / Anserini non-empty) | total tokens strata | total tokens Lucene | diff |
|---|---|---|---|---|
| scifact | 5183 / 5183 | 838,125 | 838,128 | -3 (-0.000%) |
| nfcorpus | 3633 / 3633 | 637,056 | 637,485 | -429 (-0.067%) |
| fiqa | 57638 / 57600 | 5,290,277 | 5,288,635 | +1,642 (+0.031%) |
| scidocs | 25657 / 25657 | 3,266,270 | 3,266,767 | -497 (-0.015%) |
| trec-covid | 171332 / 171331 | 20,819,956 | 20,822,821 | -2,865 (-0.014%) |

### Parameter and analyzer ablations (nDCG@10)

| dataset | default k1=0.9 b=0.4 | k1=1.2 b=0.75 | Porter2 stemmer | no stemming | no stopwords |
|---|---|---|---|---|---|
| scifact | 0.6777 | 0.6830 | 0.6821 | 0.6651 | 0.6807 |
| nfcorpus | 0.3202 | 0.3212 | 0.3203 | 0.3060 | 0.3215 |
| fiqa | 0.2353 | 0.2523 | 0.2365 | 0.2357 | 0.2340 |
| scidocs | 0.1498 | 0.1553 | 0.1504 | 0.1465 | 0.1495 |
| trec-covid | 0.5943 | 0.6139 | 0.5898 | 0.5912 | 0.5958 |

### strata vs. Tantivy 0.27 (k1=1.2, b=0.75, identical terms)

Quality (strata with Lucene-quantized lengths, which is also how Tantivy stores lengths):

| dataset | strata nDCG@10 | Tantivy nDCG@10 | strata R@100 | Tantivy R@100 | Tantivy (native analyzer) nDCG@10 |
|---|---|---|---|---|---|
| scifact | 0.6826 | 0.6826 | 0.9282 | 0.9282 | 0.6838 |
| nfcorpus | 0.3207 | 0.3207 | 0.2462 | 0.2462 | 0.3205 |
| fiqa | 0.2537 | 0.2537 | 0.5489 | 0.5489 | 0.2495 |
| scidocs | 0.1545 | 0.1545 | 0.3601 | 0.3601 | 0.1561 |
| trec-covid | 0.6146 | 0.6146 | 0.1119 | 0.1119 | 0.6049 |

Speed, mean ± sample std over 3 in-process repeats (single thread; latency = query analysis + search):

| dataset | engine | index docs/s | p50 µs (k=1000) | p99 µs (k=1000) | p50 µs (k=10) | p99 µs (k=10) | index bytes |
|---|---|---|---|---|---|---|---|
| scifact | strata | 15391 ± 2707 | 142 ± 2 | 206 ± 5 | 29 ± 1 | 67 ± 2 | 5,465,393 (uncompressed heap) |
| scifact | Tantivy | 17531 ± 791 | 252 ± 6 | 630 ± 37 | 75 ± 2 | 307 ± 13 | 1,208,222 (compressed, in RAM) |
| nfcorpus | strata | 14496 ± 3145 | 15 ± 1 | 134 ± 6 | 5 ± 0 | 29 ± 4 | 4,119,546 (uncompressed heap) |
| nfcorpus | Tantivy | 17328 ± 115 | 19 ± 0 | 212 ± 7 | 9 ± 0 | 68 ± 1 | 914,059 (compressed, in RAM) |
| fiqa | strata | 34502 ± 506 | 461 ± 9 | 861 ± 50 | 261 ± 10 | 611 ± 27 | 34,458,194 (uncompressed heap) |
| fiqa | Tantivy | 23798 ± 3851 | 1873 ± 142 | 7776 ± 1119 | 618 ± 134 | 3408 ± 1037 | 6,938,756 (compressed, in RAM) |
| scidocs | strata | 21728 ± 243 | 258 ± 2 | 468 ± 5 | 88 ± 2 | 252 ± 6 | 21,268,093 (uncompressed heap) |
| scidocs | Tantivy | 20507 ± 1378 | 567 ± 20 | 1455 ± 56 | 165 ± 5 | 645 ± 11 | 4,201,402 (compressed, in RAM) |
| trec-covid | strata | 19734 ± 944 | 1384 ± 33 | 2399 ± 31 | 962 ± 22 | 2004 ± 112 | 122,630,611 (uncompressed heap) |
| trec-covid | Tantivy | 20500 ± 328 | 2874 ± 58 | 12488 ± 505 | 1085 ± 59 | 7148 ± 359 | 24,434,700 (compressed, in RAM) |

### strata default configuration: indexing and query latency (3 repeats)

| dataset | docs | queries | analysis-only s | index s (incl. analysis) | docs/s | p50 µs | p99 µs | QPS | p50 µs k=10 | p99 µs k=10 |
|---|---|---|---|---|---|---|---|---|---|---|
| scifact | 5,183 | 300 | 0.226 ± 0.009 | 0.293 ± 0.005 | 17695 ± 329 | 146 ± 2 | 1145 ± 1607 | 6277 ± 1476 | 31 ± 4 | 94 ± 47 |
| nfcorpus | 3,633 | 323 | 0.169 ± 0.002 | 0.223 ± 0.006 | 16295 ± 400 | 15 ± 1 | 135 ± 1 | 29029 ± 426 | 4 ± 0 | 27 ± 4 |
| fiqa | 57,638 | 648 | 1.185 ± 0.008 | 1.690 ± 0.042 | 34121 ± 851 | 441 ± 12 | 804 ± 47 | 2217 ± 59 | 238 ± 15 | 551 ± 48 |
| scidocs | 25,657 | 1000 | 0.900 ± 0.016 | 1.200 ± 0.032 | 21388 ± 562 | 256 ± 2 | 482 ± 36 | 3789 ± 61 | 81 ± 2 | 234 ± 8 |
| trec-covid | 171,332 | 50 | 6.075 ± 0.738 | 8.354 ± 0.579 | 20577 ± 1468 | 1119 ± 112 | 2080 ± 97 | 870 ± 79 | 761 ± 124 | 1658 ± 359 |

Raw per-repeat values (docs/s; p50/p99 µs at k=1000):

| dataset | repeat 1 | repeat 2 | repeat 3 |
|---|---|---|---|
| scifact | 17660 docs/s; 144/207 µs | 17384 docs/s; 145/227 µs | 18039 docs/s; 148/3001 µs |
| nfcorpus | 16474 docs/s; 16/134 µs | 16574 docs/s; 15/135 µs | 15837 docs/s; 15/136 µs |
| fiqa | 35031 docs/s; 440/789 µs | 33988 docs/s; 430/766 µs | 33345 docs/s; 454/857 µs |
| scidocs | 20833 docs/s; 253/453 µs | 21957 docs/s; 258/523 µs | 21375 docs/s; 256/472 µs |
| trec-covid | 22222 docs/s; 1043/2006 µs | 20107 docs/s; 1066/2045 µs | 19401 docs/s; 1247/2190 µs |

Index shape:

| dataset | docs | terms | postings | total tokens | avg doc len |
|---|---|---|---|---|---|
| scifact | 5,183 | 28,870 | 510,930 | 838,125 | 161.7 |
| nfcorpus | 3,633 | 22,123 | 390,712 | 637,056 | 175.4 |
| fiqa | 57,638 | 67,016 | 3,700,517 | 5,290,277 | 91.8 |
| scidocs | 25,657 | 63,658 | 2,107,133 | 3,266,270 | 127.3 |
| trec-covid | 171,332 | 202,705 | 13,243,081 | 20,819,956 | 121.5 |

### Index heap size (allocator-measured, `strata-mem`)

| dataset | corpus text MB | builder live MB | segment live MB | segment estimate MB | bytes/posting | segment / text |
|---|---|---|---|---|---|---|
| scifact | 7.8 | 9.5 | 5.5 | 5.5 | 10.70 | 0.70× |
| nfcorpus | 5.8 | 6.4 | 4.1 | 4.1 | 10.54 | 0.71× |
| fiqa | 44.3 | 51.7 | 34.5 | 34.5 | 9.31 | 0.78× |
| scidocs | 30.9 | 32.7 | 21.3 | 21.3 | 10.09 | 0.69× |
| trec-covid | 191.8 | 175.3 | 122.6 | 122.6 | 9.26 | 0.64× |

### Metric cross-check vs. pytrec_eval 0.5

| run | nDCG@10 | R@100 | R@1000 | MRR@10 | max per-query \|diff\| | verdict |
|---|---|---|---|---|---|---|
| fiqa.strata.k1_0.9.b_0.4.exactlen.porter.lucenestop | 0.2353 / 0.2353 | 0.5400 / 0.5400 | 0.7403 / 0.7403 | 0.2958 / 0.2958 | 0.0e+00 | MATCH |
| fiqa.tantivy-0.27.0.strata-analyzer | 0.2537 / 0.2537 | 0.5489 / 0.5489 | 0.7591 / 0.7591 | 0.3089 / 0.3089 | 0.0e+00 | MATCH |
| nfcorpus.strata.k1_0.9.b_0.4.exactlen.porter.lucenestop | 0.3202 / 0.3202 | 0.2456 / 0.2456 | 0.3702 / 0.3702 | 0.5215 / 0.5215 | 0.0e+00 | MATCH |
| nfcorpus.tantivy-0.27.0.strata-analyzer | 0.3207 / 0.3207 | 0.2462 / 0.2462 | 0.3717 / 0.3717 | 0.5189 / 0.5189 | 0.0e+00 | MATCH |
| scidocs.strata.k1_0.9.b_0.4.exactlen.porter.lucenestop | 0.1498 / 0.1498 | 0.3473 / 0.3473 | 0.5638 / 0.5638 | 0.2604 / 0.2604 | 0.0e+00 | MATCH |
| scidocs.tantivy-0.27.0.strata-analyzer | 0.1545 / 0.1545 | 0.3601 / 0.3601 | 0.5699 / 0.5699 | 0.2672 / 0.2672 | 0.0e+00 | MATCH |
| scifact.strata.k1_0.9.b_0.4.exactlen.porter.lucenestop | 0.6777 / 0.6777 | 0.9253 / 0.9253 | 0.9767 / 0.9767 | 0.6445 / 0.6445 | 0.0e+00 | MATCH |
| scifact.tantivy-0.27.0.strata-analyzer | 0.6826 / 0.6826 | 0.9282 / 0.9282 | 0.9767 / 0.9767 | 0.6464 / 0.6464 | 0.0e+00 | MATCH |
| trec-covid.strata.k1_0.9.b_0.4.exactlen.porter.lucenestop | 0.5943 / 0.5943 | 0.1090 / 0.1090 | 0.3959 / 0.3959 | 0.8562 / 0.8562 | 0.0e+00 | MATCH |
| trec-covid.tantivy-0.27.0.strata-analyzer | 0.6146 / 0.6146 | 0.1119 / 0.1119 | 0.4074 / 0.4074 | 0.8840 / 0.8840 | 0.0e+00 | MATCH |

(cells: strata / pytrec_eval)

## Reading the numbers

**Correctness.**
- *Anserini.* In Lucene-quantized-length mode, strata matches the published Anserini BM25 numbers to 4 decimals on **13 of 15** (dataset, metric) cells. That includes every R@100 and R@1000 cell, plus nDCG@10 on scifact, fiqa and scidocs. The two remaining cells are nDCG@10 on nfcorpus (−0.0013) and trec-covid (−0.0012). Total token counts there differ from Lucene's by −0.067% and −0.014%, so the most likely cause is tokenizer edge cases. That is **not verified**: Lucene was not run locally for a token-level diff.
- *Tantivy.* Given identical terms, identical BM25 parameters and the same length quantization, Tantivy and strata produce **identical nDCG@10 and R@100 on all 5 datasets**. Top-10 lists are identical for 100% of queries on scifact/nfcorpus/fiqa/scidocs and for 45/50 on trec-covid. Top-100 *sets* are identical everywhere. The order differences (largest score gap on shared docs ≈ 1.5e-5, see `results/parity/`) are what f32 rounding with a different summation order would produce on near-ties.
- *pytrec_eval.* All 10 cross-checked runs match: identical per-query values and means within 4e-15 (`results/crosscheck/`).

**Effectiveness.**
- Lucene length quantization moves nDCG@10 by at most ±0.0012 relative to exact lengths, with no consistent direction. It doesn't buy quality. It exists to save bytes.
- k1=1.2/b=0.75 beats Anserini's 0.9/0.4 on all 5 datasets here (+0.0010 nfcorpus to +0.0196 trec-covid nDCG@10).
- Stemming helps clearly on scifact (+0.0126 vs no stemming) and nfcorpus (+0.0142), and barely matters on fiqa. Porter2 beats classic Porter on scifact, fiqa and scidocs and loses on trec-covid. Removing stopwords changes nDCG@10 by −0.0013 to +0.0030 while making queries slower (e.g. fiqa p50 441 → 823 µs in the first pass, see `summary.tsv`).

**Speed.**
- Indexing runs at 14k–35k docs/s single-threaded. Text analysis alone (tokenize + lowercase + stopwords + Porter) takes **70–77% of indexing time**: e.g. fiqa 1.185 s of 1.690 s, trec-covid 6.08 s of 8.35 s. Building postings is the minority.
- Exhaustive query latency scales with the postings touched: p50 15 µs (nfcorpus) to 1.1 ms (trec-covid, 13.2M postings) at k=1000. At k=10 it drops to 4 µs–0.76 ms, because selecting a smaller top-k is cheaper. The scoring work is the same.
- *vs. Tantivy* (same machine, same terms, single thread). strata's exhaustive TAAT search had lower p50 and much lower p99 on every dataset (e.g. trec-covid k=10 p50 962 vs 1085 µs, p99 2.0 vs 7.1 ms). Indexing throughput was comparable: fiqa was faster with strata, scifact/nfcorpus faster with Tantivy, the rest within noise. **Caveats:** Tantivy's index is compressed and **4.5–5.1× smaller** (e.g. trec-covid 24.4 MB vs 122.6 MB). It stores more (fast field, doc store), and every query pays for weight construction, FST term lookup and block decoding. We did not profile Tantivy, so these causes are hypotheses. On corpora this small, a fixed per-query overhead can dominate. That is not evidence that strata would be faster at scale or after stage 2 adds compression.
- Variance: most cells have <5% std. Exceptions: the scifact default run has one repeat with a 3.0 ms p99 outlier (p99 1145 ± 1607 µs; the other two repeats are 207 and 227 µs). Tantivy on fiqa is noisy (p99 7.8 ± 1.1 ms). Some indexing runs show ±18–22% (the scifact/nfcorpus Tantivy-parity rows). Those are laptop scheduling effects, not something we controlled for.

**Memory.**
- The frozen segment holds 9.3–10.7 bytes per posting (8 bytes per `Posting` plus the term dictionary, offsets, doc lengths and external ids). That comes to 0.64–0.78× the raw corpus text size.
- The capacity-based estimate (`InMemorySegment::heap_size_bytes`) equals the allocator-measured live bytes exactly on all 5 datasets.
- The mutable builder (`HashMap` + per-term `Vec`s with growth slack) is 1.4–1.8× the frozen segment. Peak memory during `build()` (builder and segment alive at once) was not measured.
