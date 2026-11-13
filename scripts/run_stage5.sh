#!/usr/bin/env bash
# Stage 5 (hybrid retrieval). Needs the stage-1 lexical runs (scripts/run_all.sh)
# and the stage-4 dense runs (scripts/run_stage4.sh).
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
DATASETS=("$@")
[ ${#DATASETS[@]} -eq 0 ] && DATASETS=(scifact nfcorpus fiqa scidocs trec-covid)
S=results/stage5
mkdir -p $S/logs $S/compare $S/crosscheck
cargo build --release -q -p strata-eval
EVAL=target/release/strata-eval
LEX=strata.k1_0.9.b_0.4.exactlen.porter.lucenestop
{
  echo "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "git: $(git rev-parse --short HEAD)$(git diff --quiet || echo ' (dirty)')"
  echo "uptime: $(uptime)"
} > $S/ENVIRONMENT.txt

for d in "${DATASETS[@]}"; do
  echo "=================== $d"
  $EVAL hybrid --dataset "$d" --lexical "results/runs/$d/$LEX.trec" \
    --dense "results/stage4/runs/$d/dense-exact.bge-small.trec" --out-dir $S | tee "$S/logs/$d.hybrid.log"
  # Cross-encoder re-ranking of the RRF top 100 (local, MPS/CPU). Cached:
  # it depends only on the RRF run, so delete the .meta.json to redo it.
  [ -f "$S/runs/$d/rrf-k60.rerank-minilm.trec.meta.json" ] || uv run -q --with sentence-transformers==3.3.1 --with torch python -I scripts/rerank_cross_encoder.py \
    --dataset "$d" --run "$S/runs/$d/rrf-k60.trec" --depth 100 --out "$S/runs/$d/rrf-k60.rerank-minilm.trec" \
    2> "$S/logs/$d.rerank.err" | tee "$S/logs/$d.rerank.log"
  cp "$S/runs/$d/rrf-k60.rerank-minilm.trec.meta.json" "$S/logs/$d.rerank.meta.json"
  $EVAL eval --dataset "$d" --run "$S/runs/$d/rrf-k60.rerank-minilm.trec" > "$S/$d.rerank.eval.json"
  $EVAL eval --dataset "$d" --run "$S/runs/$d/lexical-bm25.trec" > "$S/logs/$d.lexical.eval.json"
  $EVAL compare --dataset "$d" --a "$S/runs/$d/rrf-k60.trec" --b "$S/runs/$d/rrf-k60.rerank-minilm.trec" > "$S/compare/$d.rerank_vs_rrf.json"
  $EVAL compare --dataset "$d" --a "$S/runs/$d/lexical-bm25.trec" --b "$S/runs/$d/rrf-k60.rerank-minilm.trec" > "$S/compare/$d.rerank_vs_lexical.json"
  $EVAL compare --dataset "$d" --a "$S/runs/$d/lexical-bm25.trec" --b "$S/runs/$d/rrf-k60.trec" > "$S/compare/$d.rrf_vs_lexical.json"
  # Independent check of our paired t-test (SciPy) on one comparison.
  uv run -q --with scipy==1.14.1 --with pytrec_eval==0.5 --with numpy python -I scripts/crosscheck_stats.py \
    --qrels "data/$d/qrels/test.tsv" --a "$S/runs/$d/lexical-bm25.trec" --b "$S/runs/$d/rrf-k60.trec" \
    --strata-json "$S/compare/$d.rrf_vs_lexical.json" | tee "$S/crosscheck/$d.ttest.txt"
done
