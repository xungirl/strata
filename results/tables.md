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

