"""Cross-check strata's exact (brute-force) dense top-10 against numpy.

    uv run --with numpy python -I scripts/crosscheck_exact_knn.py scifact nfcorpus fiqa scidocs trec-covid

For every BEIR test query, compares the top-10 doc ids of strata's exact
dense run (results/stage4/runs/<d>/dense-exact.bge-small.trec, self-hits
removed) with numpy's float32 matrix product + argsort (self-hits removed
too). Reports identical-list and identical-set fractions and the largest
absolute score difference on shared docs.
"""

import json
import sys
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent

for name in sys.argv[1:]:
    d = ROOT / "data" / "embeddings" / name
    dim = json.load(open(d / "meta.json"))["dim"]
    corpus = np.fromfile(d / "corpus.f32", dtype="<f4").reshape(-1, dim)
    queries = np.fromfile(d / "queries.f32", dtype="<f4").reshape(-1, dim)
    cids = json.load(open(d / "corpus_ids.json"))
    qids = json.load(open(d / "query_ids.json"))
    qrow = {q: i for i, q in enumerate(qids)}
    run = {}
    for line in open(ROOT / "results" / "stage4" / "runs" / name / "dense-exact.bge-small.trec"):
        q, _, doc, rank, score, _ = line.split()
        if int(rank) <= 10:
            run.setdefault(q, []).append((doc, float(score)))
    same_list = same_set = 0
    max_diff = 0.0
    for q, hits in run.items():
        sims = corpus @ queries[qrow[q]]
        order = np.argsort(-sims, kind="stable")
        top = [cids[i] for i in order[:11] if cids[i] != q][:10]
        mine = [h for h, _ in hits]
        same_list += mine == top
        same_set += set(mine) == set(top)
        by_id = {cids[i]: float(sims[i]) for i in order[:11]}
        for doc, s in hits:
            if doc in by_id:
                max_diff = max(max_diff, abs(s - by_id[doc]))
    n = len(run)
    print(f"{name}: {n} queries, identical top-10 lists {same_list}/{n}, identical sets {same_set}/{n}, max |score diff| {max_diff:.2e}")
