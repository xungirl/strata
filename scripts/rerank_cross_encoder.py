"""Re-rank the top-N of a TREC run with a cross-encoder, locally.

    uv run --with sentence-transformers==3.3.1 --with torch python -I scripts/rerank_cross_encoder.py \
        --dataset scifact --run results/stage5/runs/scifact/rrf-k60.trec --depth 100 \
        --out results/stage5/runs/scifact/rrf-k60.rerank-minilm.trec

Model: cross-encoder/ms-marco-MiniLM-L-6-v2 (22M parameters), trained on
MS MARCO passage ranking. It reads (query, document) together, which is
far more accurate than comparing two independent embeddings, and far more
expensive, so it only re-orders the first `--depth` hits. Documents ranked
below `depth` keep their original order after the re-ranked block.
Model files are cached under data/hf. Timing and versions go to
<out>.meta.json.
"""

import argparse
import json
import os
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
os.environ.setdefault("HF_HOME", str(ROOT / "data" / "hf"))

import sentence_transformers  # noqa: E402
import torch  # noqa: E402
from sentence_transformers import CrossEncoder  # noqa: E402

MODEL = "cross-encoder/ms-marco-MiniLM-L-6-v2"


def read_run(path):
    run = {}
    for line in open(path):
        q, _, d, _rank, s, _ = line.split()
        run.setdefault(q, []).append((d, float(s)))
    for q in run:
        run[q].sort(key=lambda x: (x[1], x[0]), reverse=True)
    return run


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dataset", required=True)
    ap.add_argument("--run", required=True)
    ap.add_argument("--depth", type=int, default=100)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    corpus = {}
    for line in open(ROOT / "data" / args.dataset / "corpus.jsonl"):
        d = json.loads(line)
        corpus[d["_id"]] = (d.get("title", "") + " " + d.get("text", "")).strip()
    queries = {}
    for line in open(ROOT / "data" / args.dataset / "queries.jsonl"):
        q = json.loads(line)
        queries[q["_id"]] = q["text"]
    run = read_run(args.run)
    device = "mps" if torch.backends.mps.is_available() else "cpu"
    model = CrossEncoder(MODEL, device=device, max_length=512)
    pairs, owners = [], []
    for q, hits in run.items():
        for d, _ in hits[: args.depth]:
            pairs.append((queries[q], corpus[d]))
            owners.append((q, d))
    t0 = time.time()
    scores = model.predict(pairs, batch_size=64, show_progress_bar=False)
    secs = time.time() - t0
    new = {}
    for (q, d), s in zip(owners, scores):
        new.setdefault(q, []).append((d, float(s)))
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    with open(args.out, "w") as f:
        for q in sorted(run):
            head = sorted(new.get(q, []), key=lambda x: (x[1], x[0]), reverse=True)
            # Keep the tail below `depth` after the re-ranked block: give it
            # scores strictly below the smallest cross-encoder score.
            floor = (min(s for _, s in head) if head else 0.0) - 1.0
            tail = [(d, floor - i) for i, (d, _) in enumerate(run[q][args.depth:])]
            for rank, (d, s) in enumerate(head + tail, 1):
                f.write(f"{q} Q0 {d} {rank} {s!r} rerank\n")
    meta = {
        "model": MODEL,
        "depth": args.depth,
        "pairs": len(pairs),
        "seconds": round(secs, 2),
        "pairs_per_second": round(len(pairs) / secs, 1),
        "device": device,
        "torch": torch.__version__,
        "sentence_transformers": sentence_transformers.__version__,
        "input_run": args.run,
        "date_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    json.dump(meta, open(args.out + ".meta.json", "w"), indent=2)
    print(f"[{args.dataset}] reranked {len(pairs)} pairs in {secs:.1f}s on {device}")


if __name__ == "__main__":
    main()
