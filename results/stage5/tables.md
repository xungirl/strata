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

