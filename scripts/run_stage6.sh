#!/usr/bin/env bash
# Stage 6 (single-node service) benchmarks: HTTP ingest and search load,
# closed and open loop, 3 repeats each, on localhost.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
source scripts/lib_bench.sh
S=results/stage6
mkdir -p $S/logs $S/load
export LOAD_LOG=$S/load.log
cargo build --release -q -p strata-server
SRV=target/release/strata-server
LOAD=target/release/strata-load
DS=${DS:-fiqa}
PORT=7700
{
  echo "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "git: $(git rev-parse --short HEAD)$(git diff --quiet || echo ' (dirty)')"
  echo "rustc: $(rustc -V)"
  echo "uptime: $(uptime)"
  echo "dataset: $DS (corpus via HTTP ingest, with bge-small vectors)"
  echo "server: $SRV --dim 384 (tokio multi-thread runtime, $(sysctl -n hw.ncpu) cores); load generator on the same machine"
} > $S/ENVIRONMENT.txt

start_server() {
  rm -rf data/stage6-node
  $SRV --dir data/stage6-node --http 127.0.0.1:$PORT --grpc 127.0.0.1:7701 --dim 384 2> $S/logs/server.log &
  SRV_PID=$!
  for _ in $(seq 1 100); do curl -sf http://127.0.0.1:$PORT/healthz > /dev/null && return 0; sleep 0.1; done
  echo "server did not start"; exit 1
}

# 1. Bulk ingest (3 repeats, fresh node each time).
for r in 1 2 3; do
  start_server
  wait_for_quiet "ingest repeat $r"
  $LOAD ingest --url http://127.0.0.1:$PORT --dataset $DS --batch 500 --concurrency 4 --embeddings --out $S/load/ingest.$r.json
  [ $r -lt 3 ] && { kill $SRV_PID; wait $SRV_PID 2>/dev/null || true; }
done

# 2. Search load on the last node (all docs committed, merges finished).
sleep 5
curl -s http://127.0.0.1:$PORT/metrics > $S/metrics_after_ingest.txt
for mode in lexical hybrid; do
  for c in 1 4 16 64; do
    for r in 1 2 3; do
      wait_for_quiet "closed $mode c=$c r=$r"
      $LOAD search --url http://127.0.0.1:$PORT --queries data/$DS/queries.jsonl --embeddings data/embeddings/$DS \
        --mode $mode --loop closed --concurrency $c --duration 15 --warmup 2 --out $S/load/closed.$mode.c$c.$r.json
    done
  done
done
# Open loop at fixed rates (latency from the scheduled send time).
for rate in 500 1000 2000 4000; do
  for r in 1 2 3; do
    wait_for_quiet "open lexical rate=$rate r=$r"
    $LOAD search --url http://127.0.0.1:$PORT --queries data/$DS/queries.jsonl --mode lexical --loop open \
      --rate $rate --duration 15 --warmup 2 --out $S/load/open.lexical.r$rate.$r.json
  done
done
curl -s http://127.0.0.1:$PORT/metrics > $S/metrics_after_load.txt
kill $SRV_PID; wait $SRV_PID 2>/dev/null || true

# 3. Tracing: one search with the stdout span exporter, to show spans exist.
rm -rf data/stage6-trace
$SRV --dir data/stage6-trace --http 127.0.0.1:7702 --trace-stdout > $S/trace_spans.txt 2> $S/logs/trace_server.log &
TP=$!
for _ in $(seq 1 100); do curl -sf http://127.0.0.1:7702/healthz > /dev/null && break; sleep 0.1; done
curl -s -X POST http://127.0.0.1:7702/v1/documents -H 'content-type: application/json' -d '{"docs":[{"id":"a","text":"hello tracing world"}]}' > /dev/null
curl -s -X POST http://127.0.0.1:7702/v1/commit > /dev/null
curl -s -X POST http://127.0.0.1:7702/v1/search -H 'content-type: application/json' -d '{"query":"hello","k":3}' > $S/trace_search_response.json
sleep 1; kill $TP; wait $TP 2>/dev/null || true
rm -rf data/stage6-trace
