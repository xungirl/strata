#!/usr/bin/env bash
# Stage 2 (storage engine) benchmarks. Reproduces every stage-2 number in
# BENCHMARK.md. Requires the stage-1 run files (scripts/run_all.sh) for the
# parity checks.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
source scripts/lib_bench.sh

DATASETS=("$@")
[ ${#DATASETS[@]} -eq 0 ] && DATASETS=(scifact nfcorpus fiqa scidocs trec-covid)
R=results
S=$R/stage2
mkdir -p $S/logs $S/parity
export LOAD_LOG=$S/load.log
cargo build --release -q -p strata-eval -p strata-bench -p strata-storage
EVAL=target/release/strata-eval
BASE=strata.k1_0.9.b_0.4.exactlen.porter.lucenestop   # stage-1 reference tag
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
  out=$($EVAL run --dataset "$d" --repeats 3 --results-dir $S "$@" 2> $S/logs/tmp.log)
  local tag; tag=$(echo "$out" | cut -f2)
  { echo "# uptime: $(uptime)"; cat $S/logs/tmp.log; echo "$out"; } > "$S/logs/$d.$tag.log"
  echo "$out" | tee -a $S/summary.tsv
}

parity() { # dataset tag
  local d=$1 tag=$2 f=$S/parity/$d.$2.txt
  {
    echo "stage-1 in-memory run vs $tag"
    python3 -I scripts/compare_runs.py --depth 1000 "$R/runs/$d/$BASE.trec" "$S/runs/$d/$tag.trec"
    if cmp -s <(cut -d' ' -f1-5 "$R/runs/$d/$BASE.trec") <(cut -d' ' -f1-5 "$S/runs/$d/$tag.trec"); then
      echo "run files byte-identical (qid, docid, rank, score columns): YES"
    else
      echo "run files byte-identical (qid, docid, rank, score columns): NO"
    fi
  } | tee "$f"
}

for d in "${DATASETS[@]}"; do
  echo "=================== $d"
  run_eval "$d" --tag "strata-mem-session2.$SUFFIX"     # in-memory, same session (latency reference)
  run_eval "$d" --engine disk                          # flush 10k docs, merge factor 4
  parity "$d" strata-disk.f10000.m4.$SUFFIX
  run_eval "$d" --engine disk --force-merge            # one segment
  parity "$d" strata-disk.f10000.m4.fm.$SUFFIX
  wait_for_quiet "$d tantivy positions"
  target/release/strata-bench tantivy --dataset "$d" --positions --repeats 3 --results-dir $S 2> "$S/logs/$d.tantivy-pos.log" | tee -a $S/summary.tsv
done

# WAL group-commit: ingest cost vs batch size (= docs per fsync), scifact.
for b in 1 10 100 1000; do
  run_eval scifact --engine disk --batch $b --hits 10 --tag "strata-disk.batch$b"
done

# Crash consistency: 100 kill -9 iterations, plus the negative control.
wait_for_quiet chaos
target/release/strata-chaos run --dir data/chaos --iterations 100 --out $S/chaos.json 2> $S/logs/chaos.log | tee $S/chaos.txt
set +e
target/release/strata-chaos run --dir data/chaos-negative --iterations 30 --max-ms 600 --broken-ack-before-write \
  --out $S/chaos_negative_control.json 2> $S/logs/chaos_negative.log | tee $S/chaos_negative_control.txt
echo "negative control exit code: ${PIPESTATUS[0]} (non-zero expected: violations detected)" | tee -a $S/chaos_negative_control.txt
set -e
rm -rf data/chaos data/chaos-negative $S/logs/tmp.log
