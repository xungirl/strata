### Bulk ingest over HTTP (fiqa, 57,638 docs with 384-d vectors; batches of 500, 4 concurrent clients; 3 fresh nodes)

| metric | mean ± std |
|---|---|
| docs/s (incl. final commit) | 2221 ± 186 |
| total seconds | 26.08 ± 2.25 |
| batch latency p50 ms | 1005 ± 117 |
| batch latency p99 ms | 1605 ± 497 |

### Closed-loop search (fiqa queries, k=10, 15 s measured after 2 s warm-up; mean ± std of 3 runs)

| mode | clients | throughput req/s | p50 ms | p99 ms | p99.9 ms | errors |
|---|---|---|---|---|---|---|
| lexical | 1 | 1827 ± 48 | 0.48 ± 0.01 | 1.52 ± 0.16 | 2.81 ± 0.84 | 0 |
| lexical | 4 | 7048 ± 463 | 0.50 ± 0.02 | 1.65 ± 0.32 | 3.82 ± 2.81 | 0 |
| lexical | 16 | 10424 ± 2031 | 1.16 ± 0.09 | 10.37 ± 4.16 | 36.12 ± 26.47 | 0 |
| lexical | 64 | 11840 ± 93 | 5.08 ± 0.04 | 15.68 ± 0.27 | 44.95 ± 2.56 | 0 |
| hybrid | 1 | 848 ± 106 | 1.08 ± 0.09 | 2.93 ± 1.15 | 6.69 ± 5.90 | 0 |
| hybrid | 4 | 3211 ± 395 | 1.16 ± 0.12 | 2.92 ± 0.91 | 6.33 ± 5.40 | 0 |
| hybrid | 16 | 4969 ± 82 | 2.51 ± 0.06 | 14.31 ± 0.23 | 48.76 ± 3.21 | 0 |
| hybrid | 64 | 4989 ± 60 | 11.71 ± 0.17 | 37.24 ± 0.41 | 64.89 ± 1.28 | 0 |

### Open-loop search (lexical; latency measured from the scheduled send time)

| target rate req/s | achieved req/s | p50 ms | p99 ms | p99.9 ms | max ms | generator send lag p99 ms | errors |
|---|---|---|---|---|---|---|---|
| 500 | 500 ± 0 | 0.72 ± 0.02 | 2.34 ± 0.58 | 20.49 ± 30.60 | 28.7 ± 44.1 | 0.382 ± 0.146 | 0 |
| 1000 | 1000 ± 0 | 0.46 ± 0.01 | 1.41 ± 0.04 | 2.08 ± 0.11 | 2.9 ± 0.5 | 0.031 ± 0.012 | 0 |
| 2000 | 2000 ± 0 | 0.46 ± 0.00 | 1.38 ± 0.01 | 1.99 ± 0.01 | 2.5 ± 0.1 | 0.041 ± 0.002 | 0 |
| 4000 | 4000 ± 0 | 0.47 ± 0.00 | 1.53 ± 0.10 | 7.47 ± 7.04 | 25.1 ± 12.9 | 0.096 ± 0.010 | 0 |

