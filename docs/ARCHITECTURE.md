# strata architecture

This document describes how the pieces fit together, from bytes on disk up to
a replicated cluster. Every component was built in a separate stage, and each
stage was verified against the previous one (see README "Project Journey" and
BENCHMARK.md).

```
                          ┌──────────────────────── clients / agents ────────────────────────┐
                          │ HTTP/JSON · gRPC · MCP (stdio) · strata-load                     │
                          └───────────────┬──────────────────────────────┬───────────────────┘
                                          │                              │
                          strata-coord (stage 7)                 strata-mcp (stage 9)
                          two-phase scatter-gather,              remember / recall / search tools
                          global IDF, hedging, deadlines                  │
                                          │                              │
            ┌─────────────────────────────┼─────────────── per shard ────┼──────────────┐
            │ strata-raft-node × 3 (stage 8): Raft log ──► apply ──► Engine               │
            │ or strata-server (stage 6):   HTTP + gRPC + /metrics + tracing ──► Engine   │
            └─────────────────────────────┬────────────────────────────────────────────────┘
                                          │
                        Engine (crates/strata-server/src/engine.rs)
             lexical (stages 2-3)         │           dense (stage 4)        hybrid (stage 5)
     ┌──────────────────────────┐        │     ┌─────────────────────┐    RRF of lexical +
     │ strata-query             │        │     │ VectorStore          │    dense top-100
     │ parser → BoolQuery       │        │     │ vectors.log + HNSW   │
     │ MaxScore / BMW / TAAT    │        │     │ (strata-vector)      │
     │ boolean/phrase/prefix    │        │     └─────────────────────┘
     │ filters                  │        │
     └────────────┬─────────────┘        │
                  ▼                      │
     strata-storage (stage 2): Index = WAL + write buffer + immutable mmap segments
       + tombstones + manifest; background merges; Arc<Snapshot> readers
                  ▼
     strata-core (stage 1): Analyzer (UAX#29, Porter), BM25, SegmentReader trait,
       in-memory segment (the correctness oracle for everything above)
```

## Crates

| crate | stage | role |
|---|---|---|
| `strata-core` | 1 | analysis, BM25 scoring, the `SegmentReader` abstraction, in-memory segment, exhaustive TAAT search |
| `strata-storage` | 2 | durable index: segments, WAL, tombstones, manifest, merges, snapshots, metadata fields |
| `strata-query` | 3 | query language, boolean/phrase/prefix execution, filters, MaxScore and block-max WAND, scoring statistics |
| `strata-vector` | 4 | SIMD dot products, int8 quantization, exact kNN, HNSW |
| `strata-eval` | 1, 5 | BEIR loading, trec_eval-exact metrics, fusion (RRF / weighted), significance tests, CLI |
| `strata-server` | 6, 7, 9 | engine, HTTP/gRPC service, metrics, tracing, load generator, coordinator, MCP server |
| `strata-raft` | 8 | Raft (own implementation), durable Raft storage, replicated shard node, chaos harness |
| `strata-bench` | 1-2 | Tantivy baselines (comparison only) |

## Data path of a write

1. Client → node (`POST /v1/documents`) or Raft leader.
2. With Raft: the batch becomes one log entry, appended and fsynced on a majority, then applied.
3. `Index::add_docs` appends WAL records (one fsync per batch) → acknowledged.
4. The write buffer analyzes text into postings with positions; field values go to columns.
5. `commit` (or every `flush_docs` docs): the buffer is written as an immutable segment (FST term
   dictionary, bit-packed 128-doc blocks with a skip table, positions, doc lengths, ids, fields), fsynced;
   changed tombstones are written; `manifest.json` is atomically replaced; old WAL files are deleted;
   a new `Arc<Snapshot>` is published.
6. A background thread merges adjacent segments of the same size tier, dropping deleted docs.

## Data path of a query

1. Parse (query language or plain bag of words) with the index's analyzer.
2. Take a snapshot (`Arc` clone) — isolated from concurrent writes and merges.
3. Statistics: snapshot-wide N/avgdl/df, or cluster-wide ones from the coordinator.
4. Term disjunctions: MaxScore (default) or block-max WAND over every segment in global doc order,
   with the skip table's per-block `(max_tf, min_doc_len)` as BM25 upper bounds. Other queries: DAAT
   boolean executor (intersection / union / exclusion / phrase positions / prefix via FST range).
5. Filters are evaluated per candidate against field columns and tombstones.
6. Dense: HNSW (optionally int8 codes + f32 rescoring); hybrid: RRF of both top-100 lists.

## Distributed search (stage 7)

`strata-coord` is stateless. It routes writes by `FNV-1a(id) % shards`, to
every replica of that shard. A search makes two rounds:

1. `POST /v1/stats` to every shard returns the shard's N, total tokens and
   per-term df for the query terms; the coordinator sums them.
2. `POST /v1/search` sends the query plus these global statistics to every
   shard. Each shard returns its top-k, and the coordinator merges them by
   (score desc, id).

Each shard call goes to one replica (round-robin), and to a second one if the
first has not answered after `--hedge-ms`. It is bounded by `--timeout-ms`; on
failure the response says `partial: true` and lists the failed shards.

## Replication (stage 8)

```
           propose(cmd) ─► RaftNode (pure state machine: tick / step / propose / read_index)
                              │ take_ready()
                              ▼
   Ready { hard_state, truncate_from, append, messages, committed, reads }
     1. RaftStorage::persist  — hard_state.json (atomic), raft.log (len|crc|json), fsync
     2. send messages         — HTTP POST /raft/msg, fire-and-forget
     3. committed → channel → applier thread → Index (strata-storage) → wake the writer
     4. reads (ReadIndex)     — answer once applied ≥ read index
```

* Leader election with randomized timeouts and the up-to-date-log vote check.
  A new leader appends a no-op, and only current-term entries are committed by
  counting replicas (Figure 8).
* A follower rejection carries a hint (the first index of the conflicting
  term). Catch-up continues only on replies that advance `matched`.
* The state machine is rebuilt at start by re-applying the committed log
  (there are no snapshots). Applying runs on its own thread, so a node that is
  catching up still votes and replicates.
* `strata-raft-node` is a separate binary with a simpler API (id + text). It
  is **not** yet wired behind `strata-coord`; in stage 7 the coordinator keeps
  replicas in sync by writing to all of them.

## Agent memory (stage 9)

`strata-mcp` is a Model Context Protocol server (`rmcp`, stdio transport)
around an `Engine`:

* `remember` / `remember_many` append the texts to `memories.jsonl` (fsync),
  then ingest them and commit, so they are visible to the next call.
* `recall` turns the question into a plain bag of words, plus an exact
  keyword filter on one tag.
* `search` takes the query language. `forget` writes a tombstone line and
  deletes the doc from the index.
* Tags become keyword fields. Hits are joined with their stored text.
  stdout carries only JSON-RPC; logs go to stderr.

## Invariants that the tests enforce

* Pruned search == exhaustive search, bit for bit (proptest + BEIR run files).
* Disk segments, merges and multi-segment search == the stage-1 in-memory index (proptest + BEIR).
* Sharded search with global statistics == single node (proptest + BEIR).
* Acknowledged writes survive kill -9 (100-kill chaos test, with a negative control).
* Raft: at most one leader per term, identical applied sequences, committed entries never change,
  ReadIndex never returns stale data, progress after healing (randomized simulation, 300 schedules,
  6/6 injected bugs caught); 0 acknowledged writes lost over 75 leader kills.
* The MCP server speaks the protocol over its real transport and keeps memories across restarts.
* Metrics == pytrec_eval; t-test == SciPy; exact kNN == numpy.
