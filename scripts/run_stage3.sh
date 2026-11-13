#!/usr/bin/env bash
# Stage 3 (query engine) benchmarks. Reproduces every stage-3 number in
# BENCHMARK.md. Needs the stage-1 run files for parity checks.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
source scripts/lib_bench.sh

DATASETS=("$@")
[ ${#DATASETS[@]} -eq 0 ] && DATASETS=(scifact nfcorpus fiqa scidocs trec-covid)
R=results
S=$R/stage3
mkdir -p $S/logs $S/parity $S/qbench
export LOAD_LOG=$S/load.log
cargo build --release -q -p strata-eval -p strata-query
EVAL=target/release/strata-eval
BASE=strata.k1_0.9.b_0.4.exactlen.porter.lucenestop
SUFFIX=k1_0.9.b_0.4.exactlen.porter.lucenestop
{
  echo "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "git: $(git rev-parse --short HEAD)$(git diff --quiet || echo ' (dirty)')"
  echo "rustc: $(rustc -V)"
  echo "uptime: $(uptime)"
} > $S/ENVIRONMENT.txt
[ -f $S/summary.tsv ] || echo -e "dataset\ttag\tmetrics+timings" > $S/summary.tsv

run_eval() { # dataset args...
  local d=$1; shift
  wait_for_quiet "$d $*"
  local out
  out=$($EVAL run --dataset "$d" --repeats 3 --engine disk --results-dir $S "$@" 2> $S/logs/tmp.log)
  local tag; tag=$(echo "$out" | cut -f2)
  { echo "# uptime: $(uptime)"; cat $S/logs/tmp.log; echo "$out"; } > "$S/logs/$d.$tag.log"
  echo "$out" | tee -a $S/summary.tsv
}

parity() { # dataset reference-run test-run label
  local d=$1 ref=$2 run=$3 f=$S/parity/$d.$4.txt
  {
    echo "$ref vs $run"
    python3 -I scripts/compare_runs.py --depth 1000 "$ref" "$run"
    if cmp -s <(cut -d' ' -f1-5 "$ref") <(cut -d' ' -f1-5 "$run"); then
      echo "run files byte-identical (qid, docid, rank, score columns): YES"
    else
      echo "run files byte-identical (qid, docid, rank, score columns): NO"
    fi
  } | tee "$f"
}

for d in "${DATASETS[@]}"; do
  echo "=================== $d"
  for fm in "" "--force-merge"; do
    for st in taat wand maxscore; do
      run_eval "$d" --strategy $st $fm
    done
  done
  for st in wand maxscore; do
    for seg in "" ".fm"; do
      parity "$d" "$R/runs/$d/$BASE.trec" "$S/runs/$d/strata-disk.f10000.m4$seg.$st.$SUFFIX.trec" "$st$seg"
    done
  done
done

# Filters (synthetic bucket field = FNV-1a(id) % 100), trec-covid + fiqa.
for d in trec-covid fiqa; do
  for f in "bucket:[0 TO 0]" "bucket:[0 TO 9]" "bucket:[0 TO 49]"; do
    for st in taat wand maxscore; do
      run_eval "$d" --strategy $st --bucket-field --filter "$f"
    done
    ft=$(echo "$f" | sed 's/[^A-Za-z0-9]/_/g')
    for st in wand maxscore; do
      parity "$d" "$S/runs/$d/strata-disk.f10000.m4.bucket.filter-$ft.$SUFFIX.trec" \
        "$S/runs/$d/strata-disk.f10000.m4.$st.bucket.filter-$ft.$SUFFIX.trec" "filter-$ft-$st"
    done
  done
  # Structured query latency on the bucket index built above.
  wait_for_quiet "$d qbench"
  target/release/strata-qbench --index "data/indexes/$d.strata-disk.f10000.m4.maxscore.bucket.filter-bucket__0_TO_49_.$SUFFIX" \
    --queries "data/$d/queries.jsonl" --repeats 3 --out "$S/qbench/$d.json" 2> "$S/logs/$d.qbench.log"
  cat "$S/logs/$d.qbench.log"
done
rm -f $S/logs/tmp.log
