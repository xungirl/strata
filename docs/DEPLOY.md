# Deploying strata (without Docker)

Everything in this repository was run as plain processes on one macOS laptop.
The Docker files in `Dockerfile` and `deploy/` describe the same topology for
container platforms; they were **written but not executed** in this project.

## Build

```bash
curl https://sh.rustup.rs -sSf | sh -s -- -y     # Rust toolchain (tested: 1.94.0)
cargo build --release                              # all binaries into target/release/
```

No `protoc` is needed: the gRPC stubs are generated with `protox` (a protobuf
compiler written in Rust) in `crates/strata-server/build.rs`.

## Single node

```bash
target/release/strata-server --dir /var/lib/strata --http 0.0.0.0:7700 --grpc 0.0.0.0:7701 [--dim 384]
```

* `--dir` holds `lexical/` (WAL, segments, manifest) and `vectors/` (vector log).
  Back it up by stopping the process and copying the directory (online
  backup is not implemented or tested).
* `--dim` enables dense and hybrid search for vectors of that dimension.
* Health: `GET /healthz`. Metrics: `GET /metrics` (Prometheus text format).
* Tracing: `--otlp-endpoint http://collector:4317` (OTLP/gRPC) or `--trace-stdout`.
* Durability: a write is acknowledged after its WAL fsync. Searches see writes
  after `POST /v1/commit` (or after the automatic flush every `--flush-docs`).
* As a systemd unit: `Restart=on-failure`; recovery (WAL replay) runs on start.

## Sharded cluster (stage 7)

```bash
# one strata-server per shard replica, then:
target/release/strata-coord --listen 0.0.0.0:7800 \
  --shards "http://s0a:7700,http://s0b:7700;http://s1a:7700,http://s1b:7700" \
  --timeout-ms 1000 --hedge-ms 15
```

Documents are routed by `FNV-1a(id) % shards`; the shard count is fixed for the
life of the cluster (no re-sharding yet). The coordinator is stateless; run
several behind a load balancer.

## Replicated shard (stage 8)

```bash
target/release/strata-raft-node --id 1 --peers 1=http://a:7850,2=http://b:7850,3=http://c:7850 --dir /var/lib/strata-raft
```

Run three per shard, on different machines/racks. Clients write to any node and
follow the `421 Misdirected Request` + `leader` hint to the leader. Note: the
Raft node is a separate binary with its own API (`{docs: [[id, text], ...]}`),
not yet behind `strata-coord`, and has no log compaction (restart replays the
whole log).

## Agents (stage 9)

```bash
target/release/strata-mcp --dir ~/.strata-memory     # MCP over stdio
```

Example MCP client configuration (Claude Desktop / Claude Code style):

```json
{ "mcpServers": { "strata-memory": { "command": "/path/to/strata-mcp", "args": ["--dir", "/path/to/memory"] } } }
```

## Monitoring

`deploy/prometheus.yml` scrapes `/metrics`; `deploy/grafana/strata-dashboard.json`
has panels for search latency p50/p99 (from the histogram), request rates by
route, error ratio, ingest rate, live documents, segment count (merge health)
and HNSW size. The queries use the metric names and labels that
`strata-server` exports (checked against `results/stage6/metrics_after_load.txt`;
`deploy/check_dashboard.py` verifies this). The coordinator and the Raft nodes
do not export Prometheus metrics yet (the coordinator has JSON counters at
`/coord/stats`). Neither Grafana nor Prometheus was run in this project.
