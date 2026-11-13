"""hnswlib baseline on the same embeddings as strata-vbench.

    uv run --with hnswlib==0.8.0 --with numpy python -I scripts/hnswlib_baseline.py scifact --out results/stage4/hnswlib/scifact.json

Same settings as strata: M=16, ef_construction=200, inner product on
normalized vectors, ONE thread for build and search, queries issued one at
a time (so per-query latency includes a Python -> C++ call, roughly a few
microseconds, reported as-is). Ground truth: exact top-10 by numpy matrix
product (float32), independent of strata's own brute force.
"""

import argparse
import json
import os
import time
from pathlib import Path

import hnswlib
import numpy as np

ROOT = Path(__file__).resolve().parent.parent


def load(name):
    d = ROOT / "data" / "embeddings" / name
    meta = json.load(open(d / "meta.json"))
    dim = meta["dim"]
    corpus = np.fromfile(d / "corpus.f32", dtype="<f4").reshape(-1, dim)
    queries = np.fromfile(d / "queries.f32", dtype="<f4").reshape(-1, dim)
    qids = json.load(open(d / "query_ids.json"))
    # Only BEIR test queries (those in qrels/test.tsv), like strata-vbench.
    test = set()
    with open(ROOT / "data" / name / "qrels" / "test.tsv") as f:
        next(f)
        for line in f:
            test.add(line.split("\t")[0])
    rows = [i for i, q in enumerate(qids) if q in test]
    return corpus, queries[rows]


def pct(xs, p):
    xs = sorted(xs)
    k = max(1, int(np.ceil(p / 100 * len(xs))))
    return xs[k - 1]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dataset")
    ap.add_argument("--out", required=True)
    ap.add_argument("--repeats", type=int, default=3)
    ap.add_argument("--efs", default="10,20,40,80,160,320,640")
    args = ap.parse_args()
    corpus, queries = load(args.dataset)
    n, dim = corpus.shape
    truth = np.argsort(-(queries @ corpus.T), axis=1, kind="stable")[:, :10]

    builds = []
    for _ in range(args.repeats):
        idx = hnswlib.Index(space="ip", dim=dim)
        idx.set_num_threads(1)
        t = time.perf_counter()
        idx.init_index(max_elements=n, M=16, ef_construction=200, random_seed=42)
        idx.add_items(corpus, np.arange(n), num_threads=1)
        builds.append(time.perf_counter() - t)
    points = []
    for ef in [int(x) for x in args.efs.split(",")]:
        idx.set_ef(ef)
        reps = []
        for _ in range(args.repeats):
            lat = []
            found = []
            for q in queries:
                t = time.perf_counter()
                labels, _ = idx.knn_query(q.reshape(1, -1), k=10, num_threads=1)
                lat.append((time.perf_counter() - t) * 1e6)
                found.append(labels[0])
            reps.append({"p50_us": pct(lat, 50), "p99_us": pct(lat, 99), "mean_us": float(np.mean(lat))})
        hits = sum(len(set(f) & set(t)) for f, t in zip(found, truth))
        points.append({"ef": ef, "recall_at_10": hits / (10 * len(queries)), "latency": reps})
        print(f"[{args.dataset}] hnswlib ef={ef}: recall@10 {points[-1]['recall_at_10']:.4f} p50 {reps[0]['p50_us']:.0f}us")
    out = {
        "dataset": args.dataset,
        "hnswlib": hnswlib.__version__ if hasattr(hnswlib, "__version__") else "0.8.0",
        "docs": n,
        "dim": dim,
        "test_queries": len(queries),
        "build_secs": builds,
        "points": points,
        "load_avg_1m": os.getloadavg()[0],
    }
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    json.dump(out, open(args.out, "w"), indent=2)


if __name__ == "__main__":
    main()
