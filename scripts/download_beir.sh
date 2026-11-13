#!/usr/bin/env bash
# Download BEIR datasets into ./data/<name>/ (corpus.jsonl, queries.jsonl, qrels/test.tsv).
#
# Usage: scripts/download_beir.sh scifact nfcorpus fiqa
#
# Source: the official BEIR mirror hosted by UKP TU Darmstadt
# (https://github.com/beir-cellar/beir#beers-available-datasets).
# MS MARCO is deliberately NOT supported here (multi-GB, too big for this machine).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DATA="$ROOT/data"
BASE="https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets"
mkdir -p "$DATA"

if [ "$#" -eq 0 ]; then
  set -- scifact nfcorpus fiqa
fi

for name in "$@"; do
  case "$name" in
    msmarco*) echo "refusing to download $name (too large)"; exit 1 ;;
  esac
  if [ -f "$DATA/$name/corpus.jsonl" ]; then
    echo "[skip] $name already present"
    continue
  fi
  echo "[get ] $name"
  curl -fsSL --retry 3 -o "$DATA/$name.zip" "$BASE/$name.zip"
  unzip -q -o "$DATA/$name.zip" -d "$DATA"
  rm "$DATA/$name.zip"
  echo "[done] $name: $(wc -l < "$DATA/$name/corpus.jsonl") docs"
done
