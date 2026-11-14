#!/usr/bin/env bash
# Stage 9 (agent integration): strata as MCP agent memory on LoCoMo.
#   needs data/locomo/locomo10.json (https://github.com/snap-research/locomo, CC BY-NC 4.0; not redistributed)
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
source scripts/lib_bench.sh
S=results/stage9
mkdir -p $S/compare $S/hybrid
export LOAD_LOG=$S/load.log
cargo build --release -q -p strata-server -p strata-eval
[ -f data/locomo-beir/corpus.jsonl ] || python3 -I scripts/locomo_to_beir.py
{
  echo "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "git: $(git rev-parse --short HEAD)$(git diff --quiet || echo ' (dirty)')"
  echo "uptime: $(uptime)"
} > $S/ENVIRONMENT.txt
# 1. MCP: store every turn, recall for every question; 3 repeats for latency
#    (the ranked results are deterministic; repeat 1 is the evaluated run).
for r in 1 2 3; do
  wait_for_quiet "mcp r=$r"
  python3 -I scripts/mcp_locomo_eval.py --out $S/mcp.r$r > /dev/null
done
mkdir -p $S/runs/locomo-beir
cp $S/mcp.r1/runs/locomo-beir/mcp-recall-bm25.trec $S/runs/locomo-beir/
for r in 2 3; do cmp -s $S/mcp.r1/runs/locomo-beir/mcp-recall-bm25.trec $S/mcp.r$r/runs/locomo-beir/mcp-recall-bm25.trec \
  && echo "repeat $r run file identical to repeat 1" || echo "repeat $r run file DIFFERS"; done > $S/mcp_determinism.txt
# 2. Comparison points: BM25 without the tag filter, exact dense (bge-small), fusion.
target/release/strata-eval run --dataset locomo-beir --results-dir $S --repeats 1 --tag bm25-all > /dev/null
[ -f data/embeddings/locomo-beir/corpus.f32 ] || uv run --with sentence-transformers==3.3.1 --with torch python -I scripts/embed_beir.py locomo-beir
uv run -q --with numpy python -I scripts/locomo_dense_runs.py
R=$S/runs/locomo-beir
target/release/strata-eval hybrid --dataset locomo-beir --lexical $R/mcp-recall-bm25.trec --dense $R/dense-exact-conv.trec --out-dir $S/hybrid > $S/hybrid/summary.tsv
target/release/strata-eval compare --dataset locomo-beir --a $R/bm25-all.trec --b $R/mcp-recall-bm25.trec > $S/compare/bm25_all_vs_conv_filter.txt
target/release/strata-eval compare --dataset locomo-beir --a $R/dense-exact-all.trec --b $R/dense-exact-conv.trec > $S/compare/dense_all_vs_conv_filter.txt
python3 -I scripts/make_tables_stage9.py > $S/tables.md
