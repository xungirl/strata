### Sharded (4 shards) vs single node, all BEIR test queries, k=1000

Single node = the stage-1 run file; cluster = 4 `strata-server` shards behind `strata-coord`, documents routed by FNV-1a(id) % 4, two-phase scatter-gather with global statistics.

| dataset | docs per shard | nDCG@10 single / cluster | R@1000 single / cluster | identical top-100 lists | identical top-1000 lists | differing lists explained by a score tie at rank 1000 | max abs score diff |
|---|---|---|---|---|---|---|---|
| scifact | 1291/1297/1314/1281 | 0.677677 / 0.677677 | 0.976667 / 0.976667 | 300/300 | 269/300 | 31/31 | 0 |
| nfcorpus | 925/910/905/893 | 0.320209 / 0.320209 | 0.370180 / 0.370180 | 308/308 | 302/308 | 6/6 | 0 |
| fiqa | 14281/14325/14510/14522 | 0.235330 / 0.235330 | 0.740260 / 0.740260 | 648/648 | 634/648 | 14/14 | 0 |
| scidocs | 6435/6353/6513/6356 | 0.149824 / 0.149824 | 0.563817 / 0.563817 | 1000/1000 | 924/1000 | 76/76 | 0 |
| trec-covid | 42897/43035/42883/42517 | 0.594341 / 0.594341 | 0.395925 / 0.395925 | 50/50 | 44/50 | 6/6 | 0 |

### Hedged requests vs an injected slow replica (fiqa, 2 shards x 2 replicas, open loop 200 req/s, 20 s; mean ± std of 3 runs)

Replica 7721 (shard 0) sleeps 50 ms before 5% of the `/v1/search` calls it receives (`/v1/stats` is not delayed). Each coordinator search makes 2 rounds (stats, search) x 2 shards = 4 shard calls; the coordinator rotates the first replica it tries, so about half of shard-0 search calls go to the faulty replica first, i.e. about 2.5% of searches hit a delay without hedging. 'healthy' uses only the 3 normal replicas (one per shard).

| config | achieved req/s | p50 ms | p90 ms | p99 ms | p99.9 ms | max ms | extra shard requests from hedging % | hedges won / fired | errors |
|---|---|---|---|---|---|---|---|---|---|
| healthy, 1 replica per shard, no hedging | 200 ± 0 | 1.34 ± 0.01 | 2.37 ± 0.15 | 9.03 ± 3.76 | 58.89 ± 23.83 | 75.3 ± 26.9 | 0.00 ± 0.00 | 0.00 ± 0.00 | 0 |
| 2 replicas, one delays 5% by 50 ms, no hedging | 200 ± 0 | 1.29 ± 0.03 | 3.27 ± 1.25 | 56.34 ± 5.82 | 82.56 ± 29.09 | 100.3 ± 39.4 | 0.00 ± 0.00 | 0.00 ± 0.00 | 0 |
| same, hedge after 5 ms | 200 ± 0 | 1.30 ± 0.07 | 2.40 ± 0.14 | 9.32 ± 1.81 | 26.69 ± 10.96 | 35.2 ± 9.8 | 1.67 ± 0.84 | 0.85 ± 0.17 | 0 |
| same, hedge after 15 ms | 200 ± 0 | 1.37 ± 0.07 | 2.52 ± 0.13 | 18.16 ± 0.11 | 24.48 ± 5.43 | 36.7 ± 11.6 | 1.26 ± 0.06 | 0.99 ± 0.02 | 0 |

### Timeouts and partial results (deadline 500 ms per shard call)

| scenario | wall time s (curl) | partial | failed_shards | hits returned |
|---|---|---|---|---|
| dead_shard | 0.00 | True | [1] | 5 |
| slow_shard.1 | 0.51 | True | [1] | 5 |
| slow_shard.2 | 0.50 | True | [1] | 5 |
| slow_shard.3 | 0.51 | True | [1] | 5 |

