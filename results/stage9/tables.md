### Agent memory over MCP (LoCoMo, 10 conversations, 5,882 dialogue turns as memories, 1,532 questions)

MCP server `strata-mcp` 0.1.0 (built on the official Rust SDK `rmcp` 3.10.0; protocol 2025-06-18), driven over stdio by an independent stdlib-Python JSON-RPC client. Tools listed: `forget`, `memory_stats`, `recall`, `remember`, `remember_many`, `search`.

Mean ± std over 3 fresh runs.

| measurement | value |
|---|---|
| memories stored (12 `remember_many` calls of ≤ 500, each fsynced + committed) | 5882 |
| time to store all memories, s | 0.82 ± 0.06 |
| `recall` latency p50 ms (client-side, incl. JSON-RPC over pipes) | 0.437 ± 0.006 |
| `recall` latency p99 ms | 0.984 ± 0.046 |
| `recall` latency max ms | 3.0 ± 0.9 |

### Retrieval of the gold evidence turns (all 1,532 questions)

| system | nDCG@10 | R@100 | MRR@10 |
|---|---|---|---|
| BM25, all memories (no tag filter) | 0.4566 | 0.7533 | 0.4445 |
| BM25 via MCP `recall`, tag filter conv (top 100) | 0.4942 | 0.8055 | 0.4806 |
| dense bge-small exact, all memories | 0.4374 | 0.8553 | 0.4175 |
| dense bge-small exact, tag filter conv | 0.4506 | 0.8727 | 0.4280 |
| hybrid RRF k=60 (MCP BM25 + dense conv) | 0.5041 | 0.8903 | 0.4871 |
| hybrid min-max α=0.5 (MCP BM25 + dense conv) | 0.5337 | 0.9013 | 0.5163 |

### nDCG@10 by LoCoMo question category (category numbers as in the dataset file)

| system | cat 1 (n=282) | cat 2 (n=320) | cat 3 (n=89) | cat 4 (n=841) |
|---|---|---|---|---|
| BM25, all memories (no tag filter) | 0.2322 | 0.5456 | 0.2105 | 0.5241 |
| BM25 via MCP `recall`, tag filter conv (top 100) | 0.2826 | 0.5854 | 0.2429 | 0.5571 |
| dense bge-small exact, all memories | 0.2935 | 0.5322 | 0.2457 | 0.4698 |
| dense bge-small exact, tag filter conv | 0.3150 | 0.5381 | 0.2696 | 0.4819 |
| hybrid RRF k=60 (MCP BM25 + dense conv) | 0.3434 | 0.5901 | 0.2634 | 0.5507 |
| hybrid min-max α=0.5 (MCP BM25 + dense conv) | 0.3335 | 0.6133 | 0.2786 | 0.5975 |

### Paired significance (randomization test, 100,000 samples)

| comparison | metric | mean A | mean B | diff | p (randomization) | p (t-test) |
|---|---|---|---|---|---|---|
| tag filter, BM25 (A = all, B = conv) | ndcg_at_10 | 0.4566 | 0.4942 | +0.0376 | 1e-05 | 6.4e-41 |
| tag filter, BM25 (A = all, B = conv) | recall_at_100 | 0.7533 | 0.8055 | +0.0522 | 1e-05 | 1.8e-24 |
| tag filter, dense (A = all, B = conv) | ndcg_at_10 | 0.4374 | 0.4506 | +0.0132 | 1e-05 | 2.1e-13 |
| tag filter, dense (A = all, B = conv) | recall_at_100 | 0.8553 | 0.8727 | +0.0173 | 1e-05 | 2e-09 |
| dense vs lexical (A = second) | ndcg_at_10 | 0.4942 | 0.4506 | -0.0436 | 4e-05 | 2.1e-05 |
| dense vs lexical (A = second) | recall_at_100 | 0.8055 | 0.8727 | +0.0672 | 1e-05 | 2.5e-12 |
| rrf-k60 vs lexical (A = second) | ndcg_at_10 | 0.4942 | 0.5041 | +0.0099 | 0.14 | 0.14 |
| rrf-k60 vs lexical (A = second) | recall_at_100 | 0.8055 | 0.8903 | +0.0848 | 1e-05 | 2.5e-35 |
| minmax-a0.5 vs lexical (A = second) | ndcg_at_10 | 0.4942 | 0.5337 | +0.0394 | 1e-05 | 2.6e-25 |
| minmax-a0.5 vs lexical (A = second) | recall_at_100 | 0.8055 | 0.9013 | +0.0959 | 1e-05 | 5.2e-35 |
| minmax-a0.5 vs dense (A = second) | ndcg_at_10 | 0.4506 | 0.5337 | +0.0830 | 1e-05 | 3.9e-20 |
| minmax-a0.5 vs dense (A = second) | recall_at_100 | 0.8727 | 0.9013 | +0.0286 | 1e-05 | 2.3e-07 |

