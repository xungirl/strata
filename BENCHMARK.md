# strata benchmarks (stage 1)

All numbers below were measured by `scripts/run_all.sh` on the machine described here, on **2026-11-13 (UTC)**, at git commit `3c51408` (later commits changed only docs, a code comment and an example; engine behaviour is identical). The tables are generated from the raw JSON reports by `scripts/make_tables.py`; nothing was typed in by hand. Reference numbers for Anserini come from its published regression pages (URLs in [`results/reference/anserini_beir_flat.md`](results/reference/anserini_beir_flat.md)). They were **not** re-run here.

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


---

# Stage 2: storage engine

Measured by `scripts/run_stage2.sh` (load-average gated, see `results/stage2/load.log`). Same machine as stage 1, 2026-11-13 UTC. The fiqa and scidocs rows come from a re-run (`SKIP_EXTRA=1 scripts/run_stage2.sh fiqa scidocs`). The first attempt overlapped with a `nice -n 19` compile and was set aside in `results/stage2/superseded/` (kept for transparency). Raw outputs: `results/stage2/` (`summary.tsv`, `logs/`, per-run JSON with per-repeat timings and the 1-minute load average before each repeat, `parity/`, `chaos*.json`).

```
date: 2026-11-13T08:10:28Z
git: 3ff278a (dirty)
rustc: rustc 1.94.0 (4a4ef493e 2026-03-02)
uptime:  0:10  up 65 days, 10 mins, 3 users, load averages: 3.08 3.60 5.36
```

Engine settings: flush a segment every 10,000 docs; merge 4 adjacent segments of the same size tier (tier = floor(log4 live docs)) on a background thread; ingest in batches of 1,000 docs per `add_documents` call (one WAL fsync each); BM25 k1=0.9, b=0.4, exact lengths, Porter; exhaustive TAAT with index-wide statistics (stage 1 algorithm, unchanged). "disk, 1 seg" adds `--force-merge`. Tantivy rows: `strata-bench tantivy --positions` (strata's analyzer, positions indexed, 1 indexing thread, RAM directory, merged to 1 segment; its queries use Tantivy's block-max WAND).

### Parity with the stage-1 in-memory index

| dataset | disk, multi-segment (flush 10k, merge factor 4) | disk, force-merged to 1 segment |
|---|---|---|
| scifact | byte-identical (1 segments) | byte-identical (1 segments) |
| nfcorpus | byte-identical (1 segments) | byte-identical (1 segments) |
| fiqa | byte-identical (3 segments) | byte-identical (1 segments) |
| scidocs | byte-identical (3 segments) | byte-identical (1 segments) |
| trec-covid | byte-identical (3 segments) | byte-identical (1 segments) |

### Index size

| dataset | postings | in-memory heap (stage 1) | disk segment files (1 segment, with positions) | Tantivy, with positions | Tantivy, no positions (stage 1) | strata disk / Tantivy pos |
|---|---|---|---|---|---|---|
| scifact | 510,930 | 5.5 MB (10.70 B/posting) | 2.3 MB (4.50 B/posting) | 2.2 MB (4.35 B/posting) | 1.2 MB (2.36 B/posting) | 1.03x |
| nfcorpus | 390,712 | 4.1 MB (10.54 B/posting) | 1.7 MB (4.41 B/posting) | 1.7 MB (4.33 B/posting) | 0.9 MB (2.34 B/posting) | 1.02x |
| fiqa | 3,700,517 | 34.5 MB (9.31 B/posting) | 13.7 MB (3.71 B/posting) | 13.4 MB (3.61 B/posting) | 6.9 MB (1.88 B/posting) | 1.03x |
| scidocs | 2,107,133 | 21.3 MB (10.09 B/posting) | 10.2 MB (4.84 B/posting) | 8.2 MB (3.89 B/posting) | 4.2 MB (1.99 B/posting) | 1.24x |
| trec-covid | 13,243,081 | 122.6 MB (9.26 B/posting) | 53.8 MB (4.06 B/posting) | 48.6 MB (3.67 B/posting) | 24.4 MB (1.85 B/posting) | 1.11x |

### Ingest throughput, docs/s (single writer thread, mean ± std of 3 builds)

| dataset | in-memory (no durability) | disk: WAL fsync per 1000 docs + flush/merge | disk + force-merge | Tantivy (positions, 1 thread, RAM dir) |
|---|---|---|---|---|
| scifact | 14483 ± 3938 | 12182 ± 64 | 12070 ± 386 | 15209 ± 1262 |
| nfcorpus | 15538 ± 911 | 10635 ± 748 | 10991 ± 241 | 14245 ± 283 |
| fiqa | 31872 ± 1719 | 22164 ± 427 | 18848 ± 2531 | 29950 ± 262 |
| scidocs | 20943 ± 391 | 15250 ± 244 | 13064 ± 962 | 18262 ± 1994 |
| trec-covid | 21444 ± 538 | 14490 ± 208 | 13593 ± 1022 | 18788 ± 1633 |

### Query latency, exhaustive TAAT, single thread, same session (µs; mean ± std of 3 passes)

| dataset | engine | p50 k=1000 | p99 k=1000 | p50 k=10 | p99 k=10 | load avg (1 min) per repeat |
|---|---|---|---|---|---|---|
| scifact | in-memory | 155 ± 16 | 589 ± 658 | 31 ± 3 | 82 ± 29 | 5.2, 5.2, 5.2 |
| scifact | disk, multi-seg | 205 ± 2 | 298 ± 12 | 44 ± 0 | 105 ± 2 | 3.4, 3.6, 3.6 |
| scifact | disk, 1 seg | 208 ± 2 | 299 ± 18 | 44 ± 1 | 101 ± 1 | 3.6, 3.6, 3.6 |
| scifact | Tantivy pos (BMW) | 285 ± 32 | 956 ± 293 | 84 ± 15 | 433 ± 198 | n/a |
| nfcorpus | in-memory | 16 ± 1 | 145 ± 12 | 4 ± 0 | 28 ± 4 | 3.7, 3.7, 3.7 |
| nfcorpus | disk, multi-seg | 24 ± 1 | 196 ± 4 | 6 ± 0 | 40 ± 1 | 3.7, 3.7, 3.7 |
| nfcorpus | disk, 1 seg | 24 ± 1 | 192 ± 1 | 6 ± 0 | 41 ± 1 | 3.9, 3.9, 3.9 |
| nfcorpus | Tantivy pos (BMW) | 20 ± 1 | 223 ± 4 | 9 ± 0 | 68 ± 1 | n/a |
| fiqa | in-memory | 473 ± 19 | 1006 ± 205 | 252 ± 10 | 799 ± 372 | 3.5, 3.5, 3.8 |
| fiqa | disk, multi-seg | 920 ± 2 | 1584 ± 13 | 403 ± 7 | 1005 ± 22 | 3.8, 3.9, 3.7 |
| fiqa | disk, 1 seg | 712 ± 68 | 1810 ± 605 | 433 ± 38 | 1319 ± 324 | 3.6, 3.6, 3.6 |
| fiqa | Tantivy pos (BMW) | 1205 ± 15 | 3737 ± 142 | 467 ± 6 | 2232 ± 26 | n/a |
| scidocs | in-memory | 259 ± 2 | 697 ± 369 | 80 ± 1 | 230 ± 6 | 3.2, 3.2, 3.1 |
| scidocs | disk, multi-seg | 611 ± 6 | 1007 ± 34 | 146 ± 1 | 424 ± 10 | 3.1, 3.5, 3.4 |
| scidocs | disk, 1 seg | 360 ± 6 | 666 ± 23 | 139 ± 3 | 395 ± 17 | 3.2, 3.2, 3.0 |
| scidocs | Tantivy pos (BMW) | 577 ± 14 | 1485 ± 45 | 167 ± 8 | 657 ± 45 | n/a |
| trec-covid | in-memory | 1071 ± 48 | 1947 ± 14 | 732 ± 31 | 1497 ± 53 | 3.7, 3.8, 3.9 |
| trec-covid | disk, multi-seg | 1998 ± 5 | 3871 ± 39 | 1337 ± 19 | 3023 ± 123 | 4.0, 4.5, 4.9 |
| trec-covid | disk, 1 seg | 1776 ± 176 | 3607 ± 174 | 1338 ± 80 | 3311 ± 120 | 3.5, 3.6, 4.3 |
| trec-covid | Tantivy pos (BMW) | 3224 ± 626 | 14211 ± 3180 | 1177 ± 234 | 7523 ± 782 | n/a |

### WAL group commit: ingest throughput vs docs per fsync (scifact, 5,183 docs)

| docs per `add_documents` (= per fsync) | fsyncs | docs/s (mean ± std, 3 builds) |
|---|---|---|
| 1 | 5,183 | 224 ± 9 |
| 10 | 519 | 1726 ± 98 |
| 100 | 52 | 8276 ± 277 |
| 1000 | 6 | 12224 ± 109 |

### Crash consistency (kill -9)

| run | kills | acked adds | acked deletes | lost acked adds | resurrected deletes | recovery p50 / max |
|---|---|---|---|---|---|---|
| strata (correct acks) | 100 | 113,480 | 2,639 | 0 | 0 | 23.4 / 62.3 ms |
| negative control: ack before write | 30 | 7,961 | 186 | 147 | 0 | 11.9 / 25.4 ms |

## Reading the numbers

- **Parity.** The on-disk engine returns *byte-identical* TREC run files to the stage-1 in-memory index on all 5 datasets: same documents, ranks and f32 scores, both with 3 segments (fiqa, scidocs, trec-covid; scifact and nfcorpus have fewer than 10k docs, so they flush only once) and after merging to 1 segment. That covers compressed postings, the multi-segment scoring with index-wide N/avgdl/df, global doc-id tie-breaking, and merges preserving document order.
- **Size.** Bit-packed postings with positions shrink the index 2.1-2.5x versus stage 1's uncompressed in-memory layout (which had *no* positions), to 3.7-4.8 bytes per posting. That is within 1.02-1.24x of Tantivy 0.27 indexing the same terms *with* positions. Tantivy's file total also includes its doc store and an `ord` fast field, so the comparison is approximate. scidocs is the largest gap (1.24x). Its per-term overhead (header varints, FST) weighs more because the vocabulary is large relative to the postings, but this was not investigated further.
- **Ingest.** Durability costs 16-32% throughput versus the in-memory builder (e.g. trec-covid 21,444 → 14,490 docs/s; the low end is scifact, whose in-memory figure is noisy at ±3,938). The cost covers positions, the WAL write+fsync per 1,000 docs, segment writes with fsync, and background merges. Tantivy indexes faster than strata's disk path on all 5 datasets (e.g. trec-covid 18,788 vs 14,490 docs/s).
- **fsync dominates small batches.** With one document per WAL fsync, scifact ingests 224 docs/s, i.e. about 4.5 ms per fsync on this SSD. Batching 1,000 docs per fsync reaches 12,224 docs/s (54x). That is the reason for group commit.
- **Query latency.** The disk engine is slower than the in-memory one at k=10 by 1.4-1.8x (decode + mmap instead of reading 8-byte structs), e.g. trec-covid p50 1,338 vs 732 µs. Multiple segments cost extra at k=1000 because each segment selects its own top 1001 before the merge (scidocs p50 611 vs 360 µs). Against Tantivy (block-max WAND), strata's exhaustive TAAT has lower p50 on 4/5 datasets at k=10 and lower p99 everywhere, but higher p50 on trec-covid at k=10 (1,338 vs 1,177 µs). Same caveats as stage 1 apply: small corpora, Tantivy not profiled.
- **Crash consistency.** Over 100 real `SIGKILL`s at random points (during WAL appends, flushes, background merges and recovery itself), 113,480 acknowledged adds and 2,639 acknowledged deletes were all intact after recovery: **0 lost, 0 resurrected**. Recovery (open + WAL replay) took p50 23.4 ms, max 62.3 ms. The negative control (acknowledge *before* writing) is detected: 147 lost acks in 30 kills. The test cannot see power loss or OS crashes (the page cache survives a process kill), disk-full, or bit rot. None of the 100 kills produced a torn WAL tail. Torn-tail handling is covered by unit tests (`wal::tests::torn_tail_is_truncated`), not by this run.
- Tests: 67 tests, 10/10 consecutive green runs (`results/stage2/test_10x.txt`).


---

# Stage 3: query engine

Measured by `scripts/run_stage3.sh` on the same machine, 2026-11-13 UTC. All 50 benchmark runs started with a 1-minute load average below 4 (`results/stage3/load.log`). Raw outputs are in `results/stage3/` (per-run JSON with per-repeat latency and WAND/MaxScore work counters, `logs/`, `parity/`, `qbench/`). Every run uses the stage-2 disk engine (flush 10k docs, merge factor 4), BM25 k1=0.9 b=0.4, single-threaded search, 3 index builds × 1 query pass each.

```
date: 2026-11-13T08:20:21Z
git: 50a0716
rustc: rustc 1.94.0 (4a4ef493e 2026-03-02)
uptime:  0:20  up 65 days, 20 mins, 3 users, load averages: 2.44 3.24 4.31
```

### Exactness: pruned runs vs the stage-1 exhaustive run

| dataset | BMW, multi-seg | BMW, 1 seg | MaxScore, multi-seg | MaxScore, 1 seg |
|---|---|---|---|---|
| scifact | identical | identical | identical | identical |
| nfcorpus | identical | identical | identical | identical |
| fiqa | identical | identical | identical | identical |
| scidocs | identical | identical | identical | identical |
| trec-covid | identical | identical | identical | identical |

(identical = TREC run files byte-identical in qid, doc id, rank and score at depth 1000)

### Latency, multi-segment (flush 10k, merge factor 4) (µs, mean ± std of 3 passes)

| dataset | strategy | p50 k=10 | p99 k=10 | p50 k=1000 | p99 k=1000 | p50 speedup vs TAAT (k=10 / k=1000) |
|---|---|---|---|---|---|---|
| scifact | taat | 45 ± 1 | 105 ± 8 | 206 ± 0 | 297 ± 6 | 1.00x / 1.00x |
| scifact | wand | 89 ± 1 | 332 ± 11 | 255 ± 7 | 638 ± 30 | 0.50x / 0.81x |
| scifact | maxscore | 54 ± 2 | 175 ± 6 | 204 ± 5 | 481 ± 11 | 0.83x / 1.01x |
| nfcorpus | taat | 6 ± 0 | 42 ± 2 | 25 ± 2 | 201 ± 5 | 1.00x / 1.00x |
| nfcorpus | wand | 9 ± 0 | 86 ± 7 | 18 ± 0 | 221 ± 17 | 0.66x / 1.35x |
| nfcorpus | maxscore | 9 ± 2 | 77 ± 22 | 18 ± 1 | 297 ± 147 | 0.64x / 1.36x |
| fiqa | taat | 419 ± 2 | 1055 ± 9 | 945 ± 10 | 1617 ± 3 | 1.00x / 1.00x |
| fiqa | wand | 757 ± 14 | 2739 ± 47 | 1468 ± 23 | 4126 ± 101 | 0.55x / 0.64x |
| fiqa | maxscore | 341 ± 2 | 1181 ± 32 | 1012 ± 20 | 2492 ± 54 | 1.23x / 0.93x |
| scidocs | taat | 150 ± 1 | 437 ± 3 | 617 ± 2 | 1015 ± 11 | 1.00x / 1.00x |
| scidocs | wand | 274 ± 14 | 1074 ± 317 | 693 ± 35 | 1926 ± 372 | 0.55x / 0.89x |
| scidocs | maxscore | 137 ± 1 | 405 ± 5 | 496 ± 3 | 1116 ± 20 | 1.09x / 1.24x |
| trec-covid | taat | 1383 ± 11 | 3113 ± 29 | 1994 ± 18 | 3832 ± 117 | 1.00x / 1.00x |
| trec-covid | wand | 2018 ± 55 | 8979 ± 253 | 3770 ± 107 | 12571 ± 373 | 0.69x / 0.53x |
| trec-covid | maxscore | 809 ± 22 | 3316 ± 407 | 2326 ± 7 | 6951 ± 122 | 1.71x / 0.86x |

### Latency, 1 segment (force-merged) (µs, mean ± std of 3 passes)

| dataset | strategy | p50 k=10 | p99 k=10 | p50 k=1000 | p99 k=1000 | p50 speedup vs TAAT (k=10 / k=1000) |
|---|---|---|---|---|---|---|
| scifact | taat | 48 ± 1 | 181 ± 55 | 221 ± 2 | 514 ± 112 | 1.00x / 1.00x |
| scifact | wand | 102 ± 8 | 469 ± 109 | 273 ± 4 | 944 ± 274 | 0.47x / 0.81x |
| scifact | maxscore | 60 ± 2 | 357 ± 221 | 221 ± 10 | 721 ± 39 | 0.79x / 1.00x |
| nfcorpus | taat | 7 ± 1 | 71 ± 45 | 27 ± 1 | 283 ± 57 | 1.00x / 1.00x |
| nfcorpus | wand | 10 ± 0 | 86 ± 2 | 20 ± 2 | 254 ± 60 | 0.71x / 1.34x |
| nfcorpus | maxscore | 8 ± 0 | 54 ± 5 | 17 ± 1 | 190 ± 7 | 0.89x / 1.56x |
| fiqa | taat | 451 ± 23 | 1465 ± 368 | 711 ± 29 | 1745 ± 561 | 1.00x / 1.00x |
| fiqa | wand | 801 ± 20 | 3149 ± 466 | 1553 ± 75 | 4489 ± 225 | 0.56x / 0.46x |
| fiqa | maxscore | 331 ± 7 | 1167 ± 34 | 1000 ± 20 | 2472 ± 34 | 1.36x / 0.71x |
| scidocs | taat | 144 ± 2 | 414 ± 3 | 362 ± 3 | 665 ± 14 | 1.00x / 1.00x |
| scidocs | wand | 259 ± 3 | 872 ± 31 | 651 ± 10 | 1739 ± 31 | 0.56x / 0.56x |
| scidocs | maxscore | 130 ± 1 | 401 ± 4 | 477 ± 3 | 1102 ± 29 | 1.11x / 0.76x |
| trec-covid | taat | 1342 ± 50 | 3144 ± 314 | 1826 ± 139 | 3698 ± 308 | 1.00x / 1.00x |
| trec-covid | wand | 1983 ± 38 | 8793 ± 17 | 3693 ± 15 | 12487 ± 339 | 0.68x / 0.49x |
| trec-covid | maxscore | 809 ± 17 | 3187 ± 303 | 2314 ± 3 | 6764 ± 196 | 1.66x / 0.79x |

### Pruning work (1 segment): documents fully scored per query vs query-term postings

| dataset | postings per query | BMW scored, k=10 | MaxScore scored, k=10 | BMW scored, k=1000 | MaxScore scored, k=1000 |
|---|---|---|---|---|---|
| scifact | 4,376 | 199 (4.6%) | 139 (3.2%) | 2,342 (53.5%) | 2,385 (54.5%) |
| nfcorpus | 671 | 142 (21.2%) | 146 (21.7%) | 553 (82.3%) | 554 (82.5%) |
| fiqa | 47,742 | 883 (1.8%) | 270 (0.6%) | 9,216 (19.3%) | 8,103 (17.0%) |
| scidocs | 16,423 | 353 (2.1%) | 222 (1.4%) | 5,543 (33.8%) | 5,564 (33.9%) |
| trec-covid | 182,937 | 1,435 (0.8%) | 365 (0.2%) | 17,121 (9.4%) | 11,817 (6.5%) |

### Filtered search (synthetic `bucket` field, k=10 p50 µs, mean ± std of 3 passes)

| dataset | filter (selectivity) | TAAT + filter | BMW + filter | MaxScore + filter | BMW / MaxScore == TAAT? |
|---|---|---|---|---|---|
| trec-covid | `bucket:[0 TO 0]` (1%) | 1507 ± 23 | 3074 ± 38 | 1964 ± 8 | identical / identical |
| trec-covid | `bucket:[0 TO 9]` (10%) | 1670 ± 77 | 2473 ± 8 | 1218 ± 1 | identical / identical |
| trec-covid | `bucket:[0 TO 49]` (50%) | 2146 ± 162 | 2072 ± 22 | 916 ± 29 | identical / identical |
| fiqa | `bucket:[0 TO 0]` (1%) | 449 ± 2 | 1066 ± 5 | 739 ± 3 | identical / identical |
| fiqa | `bucket:[0 TO 9]` (10%) | 480 ± 3 | 882 ± 1 | 512 ± 1 | identical / identical |
| fiqa | `bucket:[0 TO 49]` (50%) | 614 ± 9 | 786 ± 6 | 382 ± 8 | identical / identical |

### Structured queries (multi-segment index with the bucket field; k=10; µs, mean of 3 passes)

| dataset | query type | queries | p50 | p99 | avg hits | example |
|---|---|---|---|---|---|---|
| trec-covid | disjunction | 50 | 885 | 4020 | 10.0 | `what origin covid` |
| trec-covid | phrase | 50 | 207 | 1527 | 7.7 | `"what origin"` |
| trec-covid | prefix | 50 | 804 | 18602 | 10.0 | `orig*` |
| trec-covid | boolean | 50 | 283 | 1215 | 10.0 | `+what origin -covid` |
| trec-covid | filtered | 50 | 1001 | 3931 | 10.0 | `what origin covid bucket:[0 TO 9]` |
| fiqa | disjunction | 6648 | 295 | 1088 | 10.0 | `what considered business expense business trip` |
| fiqa | phrase | 6644 | 157 | 794 | 7.1 | `"what considered"` |
| fiqa | prefix | 6646 | 79 | 1350 | 9.9 | `cons*` |
| fiqa | boolean | 6579 | 258 | 692 | 9.9 | `+what consid -busi` |
| fiqa | filtered | 6648 | 456 | 1471 | 10.0 | `what considered business expense business trip bucket:[0 TO 9]` |

Diagnostic (`results/stage3/wand_debug_trec-covid_k10.txt`, `cargo run --release -p strata-query --example wand_debug -- <trec-covid 1-segment index> data/trec-covid/queries.jsonl 10`). Over all 50 trec-covid queries at k=10, BMW ran 16,366 main-loop iterations per query on average to fully score 1,374 docs. MaxScore ran 18,577 iterations, but each is much cheaper (no re-sorting of cursors, no block-bound sums), and fully scored only 338 docs. The tool also asserts, per query, that TAAT, BMW and MaxScore return identical results. It passed on all 50.

## Reading the numbers

- **Exactness.** Block-max WAND and MaxScore return byte-identical run files to the stage-1 exhaustive search on all 5 datasets, with 3 segments and with 1 segment (20/20). They also match with filters (12/12). Property tests check the same bit-for-bit across random corpora, deletes, filters and k. A mutation that makes the bound unsafe (×0.7) fails the test, so the test can detect unsafe pruning.
- **Speed: honest result.** MaxScore beats exhaustive TAAT at k=10 on the three larger corpora: fiqa 1.23x/1.36x, scidocs 1.09x/1.11x, trec-covid 1.71x/1.66x (multi-segment / 1 segment). It is *slower* on scifact and nfcorpus (5k and 3.6k docs, where TAAT is already 6-60 µs). At k=1000 it is mixed: faster on nfcorpus and scidocs (multi-seg), slower on fiqa and trec-covid. Pruning needs a high threshold, and the 1000th score is low. **Block-max WAND was slower than TAAT in every k=10 configuration measured** (0.47-0.71x). It fully scores only 0.8-4.6% of the query-term postings on four of the datasets (21% on the tiny nfcorpus; trec-covid: 1,435 docs per query vs 182,937 postings), but its per-iteration overhead (re-sorting cursors, summing block bounds) costs more than the decoding and scoring it saves on these corpora. The BEIR queries are long (6-11 terms, many frequent low-IDF words). Our TAAT baseline is a tight loop over decoded blocks with a dense accumulator. The literature's large BMW wins come from short queries over much bigger collections. That is the reason MaxScore is the default.
- **Filters.** With selective filters (1%) pruning loses: rejected docs never raise the threshold, so MaxScore at 1% is slower than TAAT on both datasets (trec-covid 1,964 vs 1,507 µs). At 50%, MaxScore is 2.3x (trec-covid) and 1.6x (fiqa) faster than TAAT. A filter-driven iteration for very selective filters is future work.
- **Structured queries** (k=10 p50): phrase queries 157-207 µs, boolean `+a b -c` 258-283 µs. Prefix queries have long tails: trec-covid p99 18.6 ms, because short prefixes expand to hundreds of terms (up to the 1,024-term cap) unioned with constant score.
- Tests: 79 tests, 10/10 consecutive green runs (`results/stage3/test_10x.txt`).


---

# Stage 4: vector index

Embeddings were computed once with `uv run --with sentence-transformers==3.3.1 --with torch python -I scripts/embed_beir.py` (Apple GPU via MPS; per-dataset wall times are in the table and in `data/embeddings/<d>/meta.json`; they are one-off timings, not benchmarks). All other numbers come from `scripts/run_stage4.sh`: `strata-vbench` for strata and `scripts/hnswlib_baseline.py` for hnswlib 0.8.0 (PyPI wheel, `uv run --with hnswlib==0.8.0`). Every step started with a 1-minute load average below 4 (`results/stage4/load.log`). Single-threaded build and search, k=10. Raw JSON is in `results/stage4/`.

```
date: 2026-11-13T11:28:49Z
git: 65e3a2e
rustc: rustc 1.94.0 (4a4ef493e 2026-03-02)
uptime:  3:28  up 65 days,  3:28, 3 users, load averages: 2.91 2.55 2.65
```

Embeddings: `BAAI/bge-small-en-v1.5` (revision `5c38ec7c405ec4b44b94cc5a9bb96e735b38267a`), 384 dims, L2-normalized, computed on `mps` with torch 2.15.0 / sentence-transformers 3.3.1.

### Dataset, build and memory

| dataset | docs | test queries | embed docs (s) | HNSW build s (M=16, efC=200) | hnswlib build s (same params) | f32 vectors MB | int8 codes MB | graph MB | max level | avg layer-0 degree |
|---|---|---|---|---|---|---|---|---|---|---|
| scifact | 5,183 | 300 | 55 | 0.6 ± 0.0 | 2.7 ± 0.0 | 8.0 | 2.0 | 1.0 | 3 | 19.1 |
| nfcorpus | 3,633 | 323 | 42 | 0.3 ± 0.0 | 1.7 ± 0.0 | 5.6 | 1.4 | 0.6 | 3 | 15.7 |
| fiqa | 57,638 | 648 | 369 | 23.9 ± 0.2 | 55.2 ± 0.1 | 88.5 | 22.1 | 11.6 | 5 | 19.6 |
| scidocs | 25,657 | 1000 | 217 | 5.5 ± 0.0 | 19.0 ± 0.4 | 39.4 | 9.9 | 5.1 | 5 | 19.4 |
| trec-covid | 171,332 | 50 | 1577 | 90.3 ± 2.7 | 177.3 ± 0.0 | 263.2 | 65.8 | 34.8 | 5 | 19.6 |

### Dense retrieval effectiveness (bge-small-en-v1.5, exact search)

| dataset | nDCG@10 exact | nDCG@10 HNSW ef=100 | ef=200 | ef=400 | R@100 exact | exact search p50 µs (brute force, 1 thread) |
|---|---|---|---|---|---|---|
| scifact | 0.7127 | 0.7127 | 0.7127 | 0.7127 | 0.9417 | 298 ± 5 |
| nfcorpus | 0.3430 | 0.3424 | 0.3430 | 0.3430 | 0.3110 | 230 ± 1 |
| fiqa | 0.4035 | 0.4017 | 0.4029 | 0.4030 | 0.6963 | 2132 ± 16 |
| scidocs | 0.2052 | 0.2043 | 0.2051 | 0.2052 | 0.4591 | 1058 ± 17 |
| trec-covid | 0.7578 | 0.7577 | 0.7578 | 0.7578 | 0.1333 | 5908 ± 32 |

### Recall@10 vs latency: scifact (5,183 docs, 300 queries; p50 / p99 µs, mean of 3 passes)

| ef | strata f32 recall | strata f32 p50 / p99 | strata SQ8+rescore recall | SQ8 p50 / p99 | distance evals/query (f32) | hnswlib recall | hnswlib p50 / p99 (incl. Python call) |
|---|---|---|---|---|---|---|---|
| 10 | 0.8717 | 12 / 21 | 0.8260 | 6 / 11 | 246 | 0.8773 | 72 / 118 |
| 20 | 0.9423 | 20 / 31 | 0.9430 | 11 / 16 | 378 | 0.9443 | 105 / 154 |
| 40 | 0.9813 | 34 / 47 | 0.9837 | 19 / 27 | 608 | 0.9820 | 170 / 228 |
| 80 | 0.9967 | 58 / 80 | 0.9963 | 37 / 50 | 986 | 0.9960 | 277 / 372 |
| 160 | 1.0000 | 100 / 132 | 0.9997 | 65 / 86 | 1,559 | 0.9993 | 452 / 582 |
| 320 | 1.0000 | 165 / 212 | 1.0000 | 114 / 144 | 2,339 | 1.0000 | 712 / 897 |
| 640 | 1.0000 | 260 / 327 | 1.0000 | 194 / 235 | 3,286 | 1.0000 | 1094 / 1326 |

### Recall@10 vs latency: nfcorpus (3,633 docs, 323 queries; p50 / p99 µs, mean of 3 passes)

| ef | strata f32 recall | strata f32 p50 / p99 | strata SQ8+rescore recall | SQ8 p50 / p99 | distance evals/query (f32) | hnswlib recall | hnswlib p50 / p99 (incl. Python call) |
|---|---|---|---|---|---|---|---|
| 10 | 0.7777 | 11 / 20 | 0.7378 | 6 / 12 | 220 | 0.7718 | 62 / 99 |
| 20 | 0.8734 | 18 / 28 | 0.8573 | 10 / 15 | 332 | 0.8684 | 94 / 139 |
| 40 | 0.9399 | 30 / 45 | 0.9365 | 18 / 26 | 536 | 0.9341 | 156 / 206 |
| 80 | 0.9771 | 53 / 72 | 0.9765 | 34 / 46 | 882 | 0.9749 | 257 / 320 |
| 160 | 0.9950 | 92 / 122 | 0.9916 | 62 / 80 | 1,401 | 0.9938 | 421 / 529 |
| 320 | 0.9985 | 149 / 189 | 0.9985 | 108 / 139 | 2,072 | 0.9975 | 654 / 803 |
| 640 | 0.9994 | 234 / 281 | 0.9991 | 184 / 227 | 2,773 | 0.9988 | 954 / 1133 |

### Recall@10 vs latency: fiqa (57,638 docs, 648 queries; p50 / p99 µs, mean of 3 passes)

| ef | strata f32 recall | strata f32 p50 / p99 | strata SQ8+rescore recall | SQ8 p50 / p99 | distance evals/query (f32) | hnswlib recall | hnswlib p50 / p99 (incl. Python call) |
|---|---|---|---|---|---|---|---|
| 10 | 0.7705 | 47 / 91 | 0.7515 | 14 / 28 | 319 | 0.7644 | 102 / 181 |
| 20 | 0.8781 | 71 / 122 | 0.8755 | 24 / 40 | 467 | 0.8656 | 155 / 261 |
| 40 | 0.9384 | 120 / 182 | 0.9352 | 44 / 69 | 744 | 0.9299 | 250 / 347 |
| 80 | 0.9727 | 215 / 328 | 0.9715 | 84 / 132 | 1,256 | 0.9704 | 426 / 573 |
| 160 | 0.9903 | 396 / 735 | 0.9890 | 151 / 217 | 2,155 | 0.9898 | 755 / 1066 |
| 320 | 0.9963 | 739 / 1122 | 0.9963 | 279 / 402 | 3,675 | 0.9961 | 1280 / 1795 |
| 640 | 0.9980 | 1323 / 1969 | 0.9983 | 554 / 813 | 6,160 | 0.9985 | 2211 / 3143 |

### Recall@10 vs latency: scidocs (25,657 docs, 1000 queries; p50 / p99 µs, mean of 3 passes)

| ef | strata f32 recall | strata f32 p50 / p99 | strata SQ8+rescore recall | SQ8 p50 / p99 | distance evals/query (f32) | hnswlib recall | hnswlib p50 / p99 (incl. Python call) |
|---|---|---|---|---|---|---|---|
| 10 | 0.8772 | 35 / 65 | 0.8365 | 13 / 44 | 277 | 0.8778 | 89 / 151 |
| 20 | 0.9533 | 52 / 82 | 0.9453 | 18 / 29 | 413 | 0.9510 | 128 / 191 |
| 40 | 0.9854 | 90 / 136 | 0.9844 | 33 / 46 | 658 | 0.9844 | 209 / 299 |
| 80 | 0.9961 | 162 / 235 | 0.9965 | 60 / 90 | 1,088 | 0.9960 | 354 / 504 |
| 160 | 0.9995 | 273 / 386 | 0.9994 | 102 / 137 | 1,804 | 0.9993 | 589 / 811 |
| 320 | 0.9999 | 477 / 682 | 0.9999 | 194 / 266 | 2,947 | 1.0000 | 990 / 1404 |
| 640 | 1.0000 | 890 / 1610 | 1.0000 | 367 / 493 | 4,711 | 1.0000 | 1649 / 2323 |

### Recall@10 vs latency: trec-covid (171,332 docs, 50 queries; p50 / p99 µs, mean of 3 passes)

| ef | strata f32 recall | strata f32 p50 / p99 | strata SQ8+rescore recall | SQ8 p50 / p99 | distance evals/query (f32) | hnswlib recall | hnswlib p50 / p99 (incl. Python call) |
|---|---|---|---|---|---|---|---|
| 10 | 0.8660 | 59 / 111 | 0.8320 | 32 / 177 | 345 | 0.8640 | 104 / 173 |
| 20 | 0.9160 | 77 / 111 | 0.9180 | 23 / 37 | 467 | 0.9140 | 149 / 201 |
| 40 | 0.9820 | 151 / 236 | 0.9820 | 42 / 75 | 725 | 0.9700 | 235 / 342 |
| 80 | 0.9900 | 264 / 363 | 0.9900 | 94 / 141 | 1,203 | 0.9840 | 419 / 594 |
| 160 | 1.0000 | 515 / 712 | 1.0000 | 202 / 296 | 2,086 | 0.9980 | 739 / 1112 |
| 320 | 1.0000 | 893 / 1292 | 1.0000 | 394 / 517 | 3,701 | 0.9980 | 1361 / 1989 |
| 640 | 1.0000 | 1798 / 3752 | 1.0000 | 767 / 984 | 6,581 | 1.0000 | 2455 / 3552 |

### Filtered ANN (synthetic bucket field; k=10; single pass)

| dataset | selectivity | allowed docs | ef | graph-filtered recall | graph p50 / p99 µs | exact scan of allowed docs p50 / p99 µs |
|---|---|---|---|---|---|---|
| scifact | 1% | 45 | 40 | 1.0000 | 587 / 690 | 8 / 11 |
| scifact | 1% | 45 | 160 | 1.0000 | 616 / 706 | 8 / 11 |
| scifact | 1% | 45 | 640 | 1.0000 | 622 / 733 | 8 / 11 |
| scifact | 10% | 505 | 40 | 0.9993 | 159 / 211 | 28 / 38 |
| scifact | 10% | 505 | 160 | 1.0000 | 365 / 433 | 28 / 38 |
| scifact | 10% | 505 | 640 | 1.0000 | 650 / 741 | 28 / 38 |
| scifact | 50% | 2,573 | 40 | 0.9947 | 56 / 81 | 108 / 130 |
| scifact | 50% | 2,573 | 160 | 1.0000 | 152 / 199 | 108 / 130 |
| scifact | 50% | 2,573 | 640 | 1.0000 | 353 / 416 | 108 / 130 |
| nfcorpus | 1% | 47 | 40 | 1.0000 | 388 / 455 | 6 / 9 |
| nfcorpus | 1% | 47 | 160 | 1.0000 | 420 / 484 | 6 / 9 |
| nfcorpus | 1% | 47 | 640 | 1.0000 | 408 / 472 | 6 / 9 |
| nfcorpus | 10% | 393 | 40 | 0.9963 | 137 / 179 | 22 / 29 |
| nfcorpus | 10% | 393 | 160 | 1.0000 | 290 / 346 | 22 / 29 |
| nfcorpus | 10% | 393 | 640 | 1.0000 | 430 / 494 | 22 / 29 |
| nfcorpus | 50% | 1,861 | 40 | 0.9663 | 48 / 67 | 73 / 94 |
| nfcorpus | 50% | 1,861 | 160 | 0.9981 | 134 / 179 | 73 / 94 |
| nfcorpus | 50% | 1,861 | 640 | 1.0000 | 310 / 371 | 73 / 94 |
| fiqa | 1% | 543 | 40 | 0.9991 | 5344 / 7698 | 85 / 108 |
| fiqa | 1% | 543 | 160 | 0.9998 | 13364 / 15518 | 85 / 108 |
| fiqa | 1% | 543 | 640 | 0.9998 | 22576 / 25043 | 85 / 108 |
| fiqa | 10% | 5,775 | 40 | 0.9900 | 801 / 1310 | 361 / 455 |
| fiqa | 10% | 5,775 | 160 | 0.9997 | 2603 / 3844 | 361 / 455 |
| fiqa | 10% | 5,775 | 640 | 1.0000 | 7438 / 10581 | 361 / 455 |
| fiqa | 50% | 28,640 | 40 | 0.9623 | 215 / 352 | 1938 / 2158 |
| fiqa | 50% | 28,640 | 160 | 0.9952 | 692 / 1087 | 1938 / 2158 |
| fiqa | 50% | 28,640 | 640 | 0.9992 | 2302 / 3488 | 1938 / 2158 |
| scidocs | 1% | 267 | 40 | 1.0000 | 2820 / 4121 | 39 / 53 |
| scidocs | 1% | 267 | 160 | 1.0000 | 6397 / 8120 | 39 / 53 |
| scidocs | 1% | 267 | 640 | 1.0000 | 7846 / 10174 | 39 / 53 |
| scidocs | 10% | 2,580 | 40 | 0.9994 | 547 / 874 | 144 / 177 |
| scidocs | 10% | 2,580 | 160 | 1.0000 | 1628 / 2420 | 144 / 177 |
| scidocs | 10% | 2,580 | 640 | 1.0000 | 4247 / 5382 | 144 / 177 |
| scidocs | 50% | 12,872 | 40 | 0.9932 | 156 / 231 | 742 / 877 |
| scidocs | 50% | 12,872 | 160 | 0.9998 | 471 / 698 | 742 / 877 |
| scidocs | 50% | 12,872 | 640 | 1.0000 | 1562 / 3320 | 742 / 877 |
| trec-covid | 1% | 1,727 | 40 | 1.0000 | 7088 / 9981 | 264 / 766 |
| trec-covid | 1% | 1,727 | 160 | 1.0000 | 18728 / 28202 | 264 / 766 |
| trec-covid | 1% | 1,727 | 640 | 1.0000 | 43922 / 70429 | 264 / 766 |
| trec-covid | 10% | 17,212 | 40 | 0.9940 | 1070 / 2916 | 3171 / 3975 |
| trec-covid | 10% | 17,212 | 160 | 1.0000 | 3733 / 5056 | 3171 / 3975 |
| trec-covid | 10% | 17,212 | 640 | 1.0000 | 11952 / 26261 | 3171 / 3975 |
| trec-covid | 50% | 86,008 | 40 | 0.9880 | 391 / 672 | 8425 / 14658 |
| trec-covid | 50% | 86,008 | 160 | 1.0000 | 1385 / 2675 | 8425 / 14658 |
| trec-covid | 50% | 86,008 | 640 | 1.0000 | 3387 / 14308 | 8425 / 14658 |

### Dot-product kernels (384 dims, ns per dot, 2M dots over the corpus)

| dataset | f32 scalar | f32 NEON | int8 scalar loop (LLVM -> SDOT) | int8 hand NEON (widening) |
|---|---|---|---|---|
| scifact | 93.8 | 29.5 | 8.3 | 18.0 |
| nfcorpus | 93.2 | 29.2 | 8.5 | 18.1 |
| fiqa | 109.6 | 33.4 | 9.7 | 19.8 |
| scidocs | 96.6 | 31.6 | 8.4 | 19.1 |
| trec-covid | 96.0 | 32.2 | 9.1 | 18.7 |

### Ground-truth cross-check (numpy)

`uv run --with numpy python -I scripts/crosscheck_exact_knn.py ...` recomputes the exact top-10 with a numpy float32 matrix product:

```
scifact: 300 queries, identical top-10 lists 300/300, identical sets 300/300, max |score diff| 1.19e-07
nfcorpus: 323 queries, identical top-10 lists 280/323, identical sets 318/323, max |score diff| 1.19e-07
fiqa: 648 queries, identical top-10 lists 647/648, identical sets 648/648, max |score diff| 1.79e-07
scidocs: 1000 queries, identical top-10 lists 1000/1000, identical sets 1000/1000, max |score diff| 1.79e-07
trec-covid: 50 queries, identical top-10 lists 47/50, identical sets 49/50, max |score diff| 1.79e-07
Note: every set mismatch (nfcorpus 5, trec-covid 1) has its 10th and 11th scores tied within 1e-6 (checked for nfcorpus: 5/5), i.e. the cut falls inside a tie; list-order differences come from the run file's trec_eval ordering (ties by doc id descending) vs numpy's stable index order. nfcorpus contains duplicate documents.
```

## Reading the numbers

- **Model sanity.** Exact dense nDCG@10 with bge-small-en-v1.5 is scifact 0.7127, nfcorpus 0.3430, fiqa 0.4035, scidocs 0.2052, trec-covid 0.7578. Dense beats our BM25 on all five datasets (BM25: 0.6777 / 0.3202 / 0.2353 / 0.1498 / 0.5943). We did not compare these against published numbers for this model, so treat them as our own measurement.
- **HNSW recall.** With M=16, efC=200, ef=160 gives recall@10 ≥ 0.99 on all five datasets (0.9903-1.0000). ef=40 gives 0.94-0.99. HNSW at ef=200 loses at most 0.0006 nDCG@10 against exact search (fiqa: 0.4029 vs 0.4035).
- **Speed vs brute force.** On trec-covid (171k docs), ef=160 reaches recall 1.0000 on the 50 queries at p50 515 µs vs 5,908 µs exact (11.5x). SQ8 makes that 202 µs (29x).
- **int8 SQ + rescoring.** SQ8 is 1.27-3.6x faster than f32 HNSW at the same ef on these datasets. Recall is within 0.05 at ef=10 and within 0.004 from ef=40 up. The graph walk on 1-byte codes is cache-friendlier, and the final f32 rescoring of the ef candidates restores the ranking.
- **vs hnswlib.** Same M/efC and single thread: recall@10 is within 0.0125 of hnswlib at every ef (same algorithm). strata's p50 is 1.4-6.1x lower and builds are 2.0-5.0x faster. **Caveat:** hnswlib 0.8.0 enables its hand-written SIMD kernels only on x86 (`results/stage4/reference/hnswlib_simd_macros.txt`), so on this ARM machine it runs generic C++ loops. Its latency also includes a Python→C++ call. This is not a claim that strata beats hnswlib in general; on x86 the comparison could look very different.
- **Kernels.** The hand-written f32 NEON dot product is 3.2x faster than the scalar loop (29.5 vs 93.8 ns, 384 dims). For int8 the plain scalar loop, auto-vectorized by LLVM into `SDOT`, is 2.1-2.2x faster than our hand-written NEON widening kernel (8.3 vs 18.0 ns). That is why `dot_i8` uses the scalar loop.
- **Filtered ANN.** Filtering during graph traversal works well at 50% selectivity (e.g. trec-covid ef=160: recall 1.0000, p50 1.4 ms vs 8.4 ms for an exact scan of the allowed docs). It degrades badly when the filter is selective: at 1% the graph search costs 7-44 ms on trec-covid, while scanning the 1,727 allowed docs takes 0.26 ms. Hence `search_filtered` switches to the exact scan below a threshold. Around 10% selectivity the winner depends on dataset and ef (trec-covid: graph faster at ef=40, scan faster at ef ≥ 160; fiqa: scan faster at every ef). At 50% the graph wins on the three larger corpora for ef ≤ 160. The switch threshold is a parameter, not tuned here.
- Memory per vector: 1,536 B f32 + 384 B int8 + about 203 B of graph links (trec-covid: 263 MB + 66 MB + 35 MB).
- Not implemented: product quantization, parallel/incremental-delete-aware builds, persistent graph loading in the benchmarks.
- Tests: 87 tests, 10/10 consecutive green runs (`results/stage4/test_10x.txt`).


---

# Stage 5: hybrid retrieval

Measured by `scripts/run_stage5.sh` (the final run re-used the cached re-ranked runs from the first run; re-ranking depends only on the RRF run, which is identical). Inputs: the stage-1 BM25 run (k1=0.9, b=0.4, Porter, top 1000) and the stage-4 exact dense run (bge-small-en-v1.5, top 1000). Fusion settings were fixed *before* looking at results: RRF k=60, weighted fusion α=0.5 (α = BM25 weight). Re-ranking: `cross-encoder/ms-marco-MiniLM-L-6-v2`, top 100 of the RRF run, max length 512 tokens, Apple GPU (MPS). Significance: paired two-sided randomization test with 100,000 sign flips (the smallest reportable p is 1e-5), plus a paired t-test cross-checked against `scipy.stats.ttest_rel`. These are quality measurements; the only timings are the re-ranker's wall time. The environment file says `(dirty)`: the run used the fusion fix for degenerate lists (commit `eval: fusion edge cases...`, made right after this run, with no other changes).

```
date: 2026-11-13T12:38:05Z
git: 18fc715 (dirty)
uptime:  4:38  up 65 days,  4:38, 3 users, load averages: 1.97 2.16 2.33
```

### nDCG@10: lexical vs dense vs hybrid vs re-ranked

| dataset | BM25 | dense (bge-small, exact) | RRF k=60 | min-max α=0.5 | z-score α=0.5 | RRF + cross-encoder re-rank (top 100) |
|---|---|---|---|---|---|---|
| scifact | 0.6777 | 0.7127 | 0.7304 | **0.7382** | 0.7349 | 0.6858 |
| nfcorpus | 0.3202 | 0.3430 | **0.3638** | 0.3579 | 0.3572 | 0.3525 |
| fiqa | 0.2353 | **0.4035** | 0.3625 | 0.3905 | 0.3951 | 0.3695 |
| scidocs | 0.1498 | **0.2052** | 0.1923 | 0.1961 | 0.1942 | 0.1683 |
| trec-covid | 0.5943 | 0.7578 | 0.8039 | 0.7985 | **0.8051** | 0.7554 |

(bold = best per dataset)

### Recall@100

| dataset | BM25 | dense | RRF | min-max α=0.5 | z-score α=0.5 | re-ranked |
|---|---|---|---|---|---|---|
| scifact | 0.9253 | 0.9417 | 0.9650 | 0.9617 | 0.9683 | 0.9650 |
| nfcorpus | 0.2456 | 0.3110 | 0.3149 | 0.3135 | 0.3130 | 0.3149 |
| fiqa | 0.5400 | 0.6963 | 0.6962 | 0.6860 | 0.6875 | 0.6962 |
| scidocs | 0.3473 | 0.4591 | 0.4496 | 0.4558 | 0.4519 | 0.4496 |
| trec-covid | 0.1090 | 0.1333 | 0.1427 | 0.1458 | 0.1476 | 0.1427 |

### Paired significance (nDCG@10 difference, two-sided randomization test, 100,000 samples; * = p < 0.05)

| dataset | queries | RRF − BM25 | RRF − dense | min-max − dense | re-rank − RRF | re-rank − BM25 |
|---|---|---|---|---|---|---|
| scifact | 300 | +0.0527 (p=1e-05)* | +0.0177 (p=0.126) | +0.0254 (p=0.0395)* | -0.0445 (p=0.00296)* | +0.0082 (p=0.598) |
| nfcorpus | 323 | +0.0436 (p=1e-05)* | +0.0208 (p=0.00408)* | +0.0149 (p=0.0104)* | -0.0114 (p=0.0949) | +0.0322 (p=3e-05)* |
| fiqa | 648 | +0.1272 (p=1e-05)* | -0.0410 (p=3e-05)* | -0.0129 (p=0.0978) | +0.0070 (p=0.415) | +0.1342 (p=1e-05)* |
| scidocs | 1000 | +0.0424 (p=1e-05)* | -0.0129 (p=0.00053)* | -0.0091 (p=0.0128)* | -0.0239 (p=1e-05)* | +0.0185 (p=1e-05)* |
| trec-covid | 50 | +0.2095 (p=1e-05)* | +0.0461 (p=0.0271)* | +0.0407 (p=0.0642) | -0.0485 (p=0.055) | +0.1611 (p=2e-05)* |

Five datasets × five comparisons = 25 tests; with a Bonferroni correction the per-test threshold would be 0.002.

### Oracle analysis: min-max α sweep on the *test* queries (not a fair result, shows sensitivity)

| dataset | α=0.0 | α=0.1 | α=0.2 | α=0.3 | α=0.4 | α=0.5 | α=0.6 | α=0.7 | α=0.8 | α=0.9 | α=1.0 | best α |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| scifact | 0.7127 | 0.7176 | 0.7300 | 0.7353 | 0.7361 | 0.7382 | 0.7323 | 0.7203 | 0.7073 | 0.6962 | 0.6777 | 0.5 |
| nfcorpus | 0.3430 | 0.3502 | 0.3554 | 0.3580 | 0.3599 | 0.3579 | 0.3562 | 0.3503 | 0.3422 | 0.3339 | 0.3202 | 0.4 |
| fiqa | 0.4035 | 0.4148 | 0.4190 | 0.4229 | 0.4121 | 0.3905 | 0.3623 | 0.3271 | 0.2936 | 0.2600 | 0.2353 | 0.3 |
| scidocs | 0.2052 | 0.2085 | 0.2081 | 0.2062 | 0.2034 | 0.1961 | 0.1888 | 0.1789 | 0.1692 | 0.1584 | 0.1498 | 0.1 |
| trec-covid | 0.7578 | 0.7755 | 0.8006 | 0.8177 | 0.8179 | 0.7985 | 0.7780 | 0.7348 | 0.6918 | 0.6582 | 0.5943 | 0.4 |

(α = weight of BM25; α=0 is dense only, α=1 is BM25 only)

### Cross-encoder re-ranking cost (cross-encoder/ms-marco-MiniLM-L-6-v2, top 100)

| dataset | pairs | seconds | pairs/s | device |
|---|---|---|---|---|
| scifact | 30,000 | 225.7 | 133 | mps |
| nfcorpus | 32,300 | 256.5 | 126 | mps |
| fiqa | 64,800 | 543.9 | 119 | mps |
| scidocs | 100,000 | 824.0 | 121 | mps |
| trec-covid | 5,000 | 37.1 | 135 | mps |

### t-test cross-check vs SciPy (RRF vs BM25 per-query nDCG@10)

| dataset | result |
|---|---|
| scifact | MATCH (scipy  : t=4.789156 p=2.63961e-06 mean_diff=0.052689) |
| nfcorpus | MATCH (scipy  : t=6.145430 p=2.35431e-09 mean_diff=0.043599) |
| fiqa | MATCH (scipy  : t=15.877046 p=3.50292e-48 mean_diff=0.127188) |
| scidocs | MATCH (scipy  : t=13.215902 p=7.24175e-37 mean_diff=0.042444) |
| trec-covid | MATCH (scipy  : t=8.504519 p=3.26753e-11 mean_diff=0.209536) |

## Reading the numbers

- **Hybrid beats BM25 everywhere and by a lot.** RRF − BM25 is +0.04 to +0.21 nDCG@10, p ≤ 1e-5 on all five datasets, which survives Bonferroni.
- **Hybrid vs dense depends on the dataset.** Hybrid wins on scifact, nfcorpus and trec-covid: best fused nDCG@10 0.7382 / 0.3638 / 0.8051 vs dense 0.7127 / 0.3430 / 0.7578. On trec-covid, RRF − dense is +0.0461 (p=0.027). It **loses** on fiqa and scidocs, where BM25 is far behind dense (0.2353 vs 0.4035 on fiqa). Fusing in the weak retriever drags the strong one down: RRF − dense is −0.0410 on fiqa (p=3e-5) and −0.0129 on scidocs (p=5e-4). Only some of these vs-dense differences survive a Bonferroni threshold of 0.002.
- **The fixed α=0.5 is not optimal.** The oracle sweep on the test queries puts the best α at 0.1-0.5 (dense-heavy on fiqa/scidocs). Choosing α per dataset on test queries would be tuning on the test set, so we report it only as sensitivity. A real system would tune on a dev split.
- **The MS MARCO cross-encoder hurt.** Re-ranking the RRF top 100 with MiniLM-L-6 *lowered* nDCG@10 on 4 of 5 datasets (scifact −0.0445, p=0.003; scidocs −0.0239, p<1e-5). It was neutral on fiqa (+0.0070, p=0.42). It is still above plain BM25 on all 5 datasets, significantly on 4. Plausible reasons (not tested): a small 6-layer model trained only on web passages (MS MARCO) transfers poorly to scientific and biomedical text, while bge-small was trained on far more diverse data. Bigger rerankers are future work.
- Re-ranking cost: about 120-135 (query, doc) pairs/s on the M2 GPU, so ~0.75 s per query for 100 candidates. Not viable as an online path on this hardware.
- The t-test implementation matches SciPy on all 5 datasets.
- Tests: 93 tests, 10/10 consecutive green runs (`results/stage5/test_10x.txt`).
