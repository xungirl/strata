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

