"""Compare two TREC run files ranking-by-ranking (no third-party deps).

Used to check that strata (with Lucene-style length quantization) and Tantivy
produce the same rankings when given identical terms and BM25 parameters.

    python3 scripts/compare_runs.py A.trec B.trec [--depth 10]

Reports, over queries present in both runs:
  * identical top-`depth` doc-id lists (order-sensitive)
  * identical top-`depth` doc-id sets
  * mean overlap@depth
  * max |score_A - score_B| for docs both runs returned in their top `depth`
"""

import argparse


def read_run(path):
    run = {}
    with open(path) as f:
        for line in f:
            parts = line.split()
            if len(parts) != 6:
                continue
            qid, _q0, did, _rank, score, _tag = parts
            run.setdefault(qid, []).append((did, float(score)))
    for qid in run:
        run[qid].sort(key=lambda kv: (kv[1], kv[0]), reverse=True)  # trec_eval order
    return run


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("a")
    ap.add_argument("b")
    ap.add_argument("--depth", type=int, default=10)
    args = ap.parse_args()
    a, b = read_run(args.a), read_run(args.b)
    qids = sorted(set(a) & set(b))
    same_list = same_set = 0
    overlap = 0.0
    max_diff = 0.0
    for q in qids:
        ta, tb = a[q][: args.depth], b[q][: args.depth]
        la, lb = [d for d, _ in ta], [d for d, _ in tb]
        same_list += la == lb
        same_set += set(la) == set(lb)
        overlap += len(set(la) & set(lb)) / max(len(la), 1)
        sb = dict(tb)
        for d, s in ta:
            if d in sb:
                max_diff = max(max_diff, abs(s - sb[d]))
    n = len(qids)
    print(f"queries compared: {n} (only in A: {len(set(a) - set(b))}, only in B: {len(set(b) - set(a))})")
    print(f"identical top-{args.depth} lists: {same_list}/{n} ({100 * same_list / n:.1f}%)")
    print(f"identical top-{args.depth} sets:  {same_set}/{n} ({100 * same_set / n:.1f}%)")
    print(f"mean overlap@{args.depth}: {overlap / n:.4f}")
    print(f"max |score diff| on shared top-{args.depth} docs: {max_diff:.3g}")


if __name__ == "__main__":
    main()
