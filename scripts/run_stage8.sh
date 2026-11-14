#!/usr/bin/env bash
# Stage 8 (replication): Raft leader-kill chaos test, 3 runs x 25 kills, on a
# 3-node replicated shard (all on localhost).
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
source scripts/lib_bench.sh
S=results/stage8
mkdir -p $S
export LOAD_LOG=$S/load.log
cargo build --release -q -p strata-raft
{
  echo "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "git: $(git rev-parse --short HEAD)$(git diff --quiet || echo ' (dirty)')"
  echo "uptime: $(uptime)"
  echo "3 strata-raft-node processes + chaos client on one machine (localhost); tick 10 ms, heartbeat 50 ms, election timeout 150-300 ms"
} > $S/ENVIRONMENT.txt
for r in 1 2 3; do
  wait_for_quiet "chaos r=$r"
  target/release/strata-raft-chaos --dir data/raft-chaos --kills 25 --out $S/chaos.$r.json 2> $S/chaos.$r.log | tee $S/chaos.$r.summary.txt
done
rm -rf data/raft-chaos
