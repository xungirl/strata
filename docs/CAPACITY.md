# Capacity and performance report

Every number in the **measured** column comes from a file under `results/`
(named in the last column) and was measured on one machine: an 8-core Apple
M2 laptop shared with other workloads, all processes on localhost. Rows marked
**arithmetic** are derived from measured numbers by the stated formula. They
are planning estimates that assume linear scaling, and **they were not
measured**. Nothing here was measured on a server, a real network or a dataset
larger than 171,332 documents.

## 1. Storage per document (trec-covid: 171,332 docs, 13.2M postings)

| component | measured total | per doc (arithmetic: total / 171,332) | source |
|---|---|---|---|
| lexical index on disk, 1 segment, with positions | 53.8 MB (4.06 B/posting) | ≈ 314 B | `results/stage2/tables.md` |
| lexical index, stage-1 in-memory heap | 122.6 MB (9.26 B/posting) | ≈ 716 B | `results/memory.tsv` |
| f32 vectors (384-d) | 263.2 MB | 1,536 B (= 384 × 4) | `results/stage4/tables.md` |
| int8 codes (SQ8) | 65.8 MB | 384 B | same |
| HNSW graph (M=16) | 34.8 MB | ≈ 203 B | same |

The per-posting cost on disk ranges from 3.7 to 4.8 B across the 5 datasets,
so the per-doc figure depends on document length. trec-covid has ≈ 77
postings per document.

**Arithmetic: 10 million trec-covid-like documents** (per-doc cost × 10^7;
assumes the same document-length distribution and linear growth):
- lexical on disk ≈ 3.1 GB;
- f32 vectors ≈ 15.4 GB;
- int8 codes ≈ 3.8 GB;
- HNSW graph ≈ 2.0 GB.

With SQ8 search plus f32 rescoring, the f32 vectors are only read for the
rescored candidates, so they could live on disk. That was not implemented or
measured: the current vector store keeps them in memory.

## 2. Write path

| measurement | measured | source |
|---|---|---|
| in-memory indexing, 1 thread | 16.3k-34.1k docs/s (trec-covid 20.6k ± 1.5k) | `results/<D>/*.json` |
| durable ingest, scifact, 1,000 docs per WAL fsync | 12,224 docs/s | `results/stage2/scifact/` |
| durable ingest, scifact, 1 doc per fsync | 224 docs/s | same |
| HTTP ingest with 384-d vectors, fiqa, 4 clients | 2,221 ± 186 docs/s (bound by single-threaded HNSW insert) | `results/stage6/tables.md` |
| MCP `remember_many`, 5,882 memories in batches of ≤ 500 | 0.82 ± 0.06 s total | `results/stage9/tables.md` |
| crash recovery (open + WAL replay) | p50 23.4 ms, max 62.3 ms | `results/stage2/chaos.json` |

The fsync rate dominates small writes: batching is the main knob. With
vectors, HNSW insertion is the bottleneck. A parallel or batched graph build
would be the first optimisation (not done).

## 3. Read path, one node

| measurement | measured | source |
|---|---|---|
| BM25 top-10, exhaustive, 1 thread, in-process | p50 4 µs (nfcorpus) … 761 µs (trec-covid) | `results/tables.md` |
| BM25 top-10, MaxScore, disk index, trec-covid | p50 809 µs vs 1,383 µs exhaustive | `results/stage3/tables.md` |
| HNSW top-10, trec-covid, ef=160 | recall 1.000, p50 515 µs (exact: 5,908 µs); SQ8 202 µs | `results/stage4/tables.md` |
| HTTP lexical search, fiqa, closed loop | 11,840 ± 93 req/s at 64 clients (p50 5.1 ms); 1,827 req/s at 1 client | `results/stage6/tables.md` |
| HTTP hybrid search, fiqa | ≈ 4,990 req/s at 16-64 clients | same |
| HTTP lexical search, open loop 4,000 req/s | p50 0.47 ms, p99 1.53 ms | same |

**Arithmetic: per-core throughput.** 11,840 req/s / 8 cores ≈ 1,480 lexical
fiqa searches per second per core. The load generator ran on the same cores,
so a dedicated server would do somewhat better; how much better was not
measured.

## 4. Cluster

| measurement | measured | source |
|---|---|---|
| sharded vs single-node quality (4 shards, global IDF) | identical metrics on 5/5 BEIR datasets | `results/stage7/parity/` |
| coordinator latency, 2 shards, 200 req/s, healthy | p50 1.34 ms, p99 9.0 ms | `results/stage7/tables.md` |
| hedging cost (hedge after 5 ms) | +1.67% shard requests; p99 56.3 → 9.3 ms with a slow replica | same |
| Raft failover (election timeout 150-300 ms) | p50 261 ms, p90 448 ms, max 782 ms (n=75) | `results/stage8/tables.md` |
| Raft restarted-node catch-up (≈ 1,770 entries) | p50 257 ms, max 567 ms | same |

Each search through the coordinator costs 2 round trips (statistics, then
search) to every shard. **Arithmetic:** with S shards, a search makes 2·S
shard requests, plus about 1.67% more with hedging at 5 ms (measured at 2 shards). Coordinator CPU
cost per request was not measured separately.

## 5. What would change at real scale (not measured)

- **Postings grow with the corpus, but top-k pruning gains grow too.**
  MaxScore was only 1.1-1.7x faster than exhaustive search on these small
  corpora; the gain is expected to grow with longer posting lists, but that
  is an expectation, not something measured here.
- **HNSW memory is the largest per-document cost** (f32 vectors). Quantized
  search with on-disk f32 rescoring, or PQ, is the usual answer.
- **Raft log growth:** there is no compaction, so restart time grows linearly
  with history. Snapshots are needed before any long-running deployment.
- **Network:** all latencies here are localhost. A real network adds at least
  one RTT per coordinator phase (2 per search), and replication adds one RTT
  per write.
