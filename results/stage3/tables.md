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

