#!/usr/bin/env bash
# Stage 4 (vector index) benchmarks. Requires cached embeddings:
#   uv run --with sentence-transformers==3.3.1 --with torch python -I scripts/embed_beir.py
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
source scripts/lib_bench.sh

DATASETS=("$@")
[ ${#DATASETS[@]} -eq 0 ] && DATASETS=(scifact nfcorpus fiqa scidocs trec-covid)
S=results/stage4
mkdir -p $S/logs $S/hnswlib
export LOAD_LOG=$S/load.log
cargo build --release -q -p strata-vector
{
  echo "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "git: $(git rev-parse --short HEAD)$(git diff --quiet || echo ' (dirty)')"
  echo "rustc: $(rustc -V)"
  echo "uptime: $(uptime)"
} > $S/ENVIRONMENT.txt

for d in "${DATASETS[@]}"; do
  wait_for_quiet "$d vbench"
  target/release/strata-vbench --dataset "$d" --out "$S/$d.json" --runs-dir "$S/runs" 2> "$S/logs/$d.vbench.log"
  tail -n 40 "$S/logs/$d.vbench.log"
  wait_for_quiet "$d hnswlib"
  uv run -q --with hnswlib==0.8.0 --with numpy python -I scripts/hnswlib_baseline.py "$d" --out "$S/hnswlib/$d.json" \
    2> "$S/logs/$d.hnswlib.err" | tee "$S/logs/$d.hnswlib.log"
done
