#!/usr/bin/env bash
# Stage 7 (distributed): sharded == single-node parity on BEIR, hedged
# requests vs an injected slow replica, partial results on timeout.
# All processes run on this one machine (localhost).
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
source scripts/lib_bench.sh
S=results/stage7
mkdir -p $S/logs $S/parity $S/tail $S/partial $S/runs
export LOAD_LOG=$S/load.log
cargo build --release -q -p strata-server
SRV=target/release/strata-server
COORD=target/release/strata-coord
LOAD=target/release/strata-load
BASE=strata.k1_0.9.b_0.4.exactlen.porter.lucenestop
D=data/stage7
PIDS=()
cleanup() { for p in ${PIDS[@]+"${PIDS[@]}"}; do kill "$p" 2>/dev/null || true; done; wait 2>/dev/null || true; PIDS=(); }
trap cleanup EXIT
up() { for _ in $(seq 1 200); do curl -sf "$1/healthz" > /dev/null && return 0; sleep 0.05; done; echo "$1 not up"; exit 1; }
node() { # port extra-args...
  local port=$1; shift
  $SRV --dir $D/n$port --http 127.0.0.1:$port "$@" 2> $S/logs/node$port.log &
  PIDS+=($!); up http://127.0.0.1:$port
}
COORD_PID=""
coord() { # shards-spec extra-args...
  local spec=$1; shift
  $COORD --listen 127.0.0.1:7800 --shards "$spec" "$@" 2> $S/logs/coord.log &
  COORD_PID=$!; PIDS+=($COORD_PID); up http://127.0.0.1:7800
}
stop_coord() { kill "$COORD_PID" 2>/dev/null || true; wait "$COORD_PID" 2>/dev/null || true; }
{
  echo "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "git: $(git rev-parse --short HEAD)$(git diff --quiet || echo ' (dirty)')"
  echo "uptime: $(uptime)"
  echo "all nodes, coordinator and load generator on one machine (localhost)"
} > $S/ENVIRONMENT.txt

# A. Parity: 4 shards x 1 replica, ingest through the coordinator, run all
#    BEIR test queries through it, compare with the stage-1 single-node run.
for ds in scifact nfcorpus fiqa scidocs trec-covid; do
  rm -rf $D; mkdir -p $D
  for p in 7711 7712 7713 7714; do node $p; done
  coord "http://127.0.0.1:7711;http://127.0.0.1:7712;http://127.0.0.1:7713;http://127.0.0.1:7714" --timeout-ms 30000
  $LOAD ingest --url http://127.0.0.1:7800 --dataset $ds --batch 1000 --concurrency 4 > $S/logs/$ds.ingest.json
  for p in 7711 7712 7713 7714; do curl -s http://127.0.0.1:$p/metrics | grep '^strata_live_docs' | sed "s/^/shard $p /"; done > $S/parity/$ds.shard_sizes.txt
  $LOAD run-queries --url http://127.0.0.1:7800 --dataset $ds --k 1000 --out $S/runs/$ds.cluster4.trec | tee $S/parity/$ds.metrics.txt
  {
    echo "stage-1 single-node run vs 4-shard cluster run"
    python3 -I scripts/compare_runs.py --depth 1000 results/runs/$ds/$BASE.trec $S/runs/$ds.cluster4.trec
    python3 -I scripts/compare_runs.py --depth 100 results/runs/$ds/$BASE.trec $S/runs/$ds.cluster4.trec
    if cmp -s <(cut -d' ' -f1-5 results/runs/$ds/$BASE.trec) <(cut -d' ' -f1-5 $S/runs/$ds.cluster4.trec); then
      echo "run files byte-identical (qid, docid, rank, score columns): YES"
    else
      echo "run files byte-identical (qid, docid, rank, score columns): NO"
    fi
  } | tee $S/parity/$ds.txt
  cleanup
done

# B. Tail latency: fiqa, 2 shards x 2 replicas; replica 7721 (shard 0)
#    delays 5% of searches by 50 ms.
rm -rf $D; mkdir -p $D
node 7721 --inject-delay-prob 0.05 --inject-delay-ms 50
for p in 7722 7731 7732; do node $p; done
coord "http://127.0.0.1:7721,http://127.0.0.1:7722;http://127.0.0.1:7731,http://127.0.0.1:7732" --timeout-ms 30000
$LOAD ingest --url http://127.0.0.1:7800 --dataset fiqa --batch 1000 --concurrency 4 > $S/logs/tail.ingest.json
stop_coord
run_tail() { # name shards hedge
  local name=$1 spec=$2 hedge=$3
  for r in 1 2 3; do
    coord "$spec" --timeout-ms 5000 --hedge-ms $hedge
    wait_for_quiet "tail $name r=$r"
    $LOAD search --url http://127.0.0.1:7800 --queries data/fiqa/queries.jsonl --mode lexical --loop open \
      --rate 200 --duration 20 --warmup 2 --out $S/tail/$name.$r.json > /dev/null
    curl -s http://127.0.0.1:7800/coord/stats > $S/tail/$name.$r.coord.json
    stop_coord
  done
}
run_tail healthy-1replica "http://127.0.0.1:7722;http://127.0.0.1:7731" 0
run_tail faulty-nohedge "http://127.0.0.1:7721,http://127.0.0.1:7722;http://127.0.0.1:7731,http://127.0.0.1:7732" 0
run_tail faulty-hedge5ms "http://127.0.0.1:7721,http://127.0.0.1:7722;http://127.0.0.1:7731,http://127.0.0.1:7732" 5
run_tail faulty-hedge15ms "http://127.0.0.1:7721,http://127.0.0.1:7722;http://127.0.0.1:7731,http://127.0.0.1:7732" 15

# C. Partial results: a shard that never answers in time (delays 100% of
#    searches by 3 s) behind a 500 ms deadline, and a dead shard.
node 7741 --inject-delay-prob 1.0 --inject-delay-ms 3000
coord "http://127.0.0.1:7722;http://127.0.0.1:7741" --timeout-ms 500
for i in 1 2 3; do
  /usr/bin/time -p curl -s -X POST http://127.0.0.1:7800/v1/search -H 'content-type: application/json' \
    -d '{"query":"mortgage interest deduction","k":5,"plain":true}' > $S/partial/slow_shard.$i.json 2> $S/partial/slow_shard.$i.time
done
stop_coord
coord "http://127.0.0.1:7722;http://127.0.0.1:7799" --timeout-ms 500
/usr/bin/time -p curl -s -X POST http://127.0.0.1:7800/v1/search -H 'content-type: application/json' \
  -d '{"query":"mortgage interest deduction","k":5,"plain":true}' > $S/partial/dead_shard.json 2> $S/partial/dead_shard.time
cleanup
rm -rf $D
