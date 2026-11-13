"""Cross-check strata's metric implementation against pytrec_eval.

pytrec_eval wraps the reference C trec_eval and is what BEIR itself uses, so
agreement here means our nDCG/Recall/MRR numbers are comparable to published
BEIR numbers.

Usage (no virtualenv needed, uv fetches pytrec_eval into a throwaway env):

    strata-eval eval --dataset scifact --run RUN.trec > ours.json
    uv run --with pytrec_eval python scripts/crosscheck_pytrec.py \
        --qrels data/scifact/qrels/test.tsv --run RUN.trec --ours ours.json

Both sides average over *all* queries in the qrels, counting queries without
results as 0 (trec_eval -c semantics). MRR@10 is pytrec_eval's `recip_rank`
computed on the run truncated to its top 10 in trec_eval order (score desc,
doc id desc), which is how BEIR defines MRR@k.
"""

import argparse
import json
import sys
from importlib.metadata import version

import pytrec_eval

TOLERANCE = 5e-5  # "match to 4 decimals"


def read_qrels(path):
    qrels = {}
    with open(path) as f:
        for i, line in enumerate(f):
            if i == 0 and line.startswith("query-id"):
                continue
            if not line.strip():
                continue
            qid, did, rel = line.rstrip("\n").split("\t")
            qrels.setdefault(qid, {})[did] = int(rel)
    return qrels


def read_run(path):
    run = {}
    with open(path) as f:
        for line in f:
            if not line.strip():
                continue
            qid, _q0, did, _rank, score, _tag = line.split()
            run.setdefault(qid, {})[did] = float(score)
    return run


def truncate(run, k):
    out = {}
    for qid, docs in run.items():
        # trec_eval order: score descending, then doc id descending.
        ranked = sorted(docs.items(), key=lambda kv: (kv[1], kv[0]), reverse=True)
        out[qid] = dict(ranked[:k])
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--qrels", required=True)
    ap.add_argument("--run", required=True)
    ap.add_argument("--ours", required=True, help="JSON printed by `strata-eval eval`")
    args = ap.parse_args()

    qrels = read_qrels(args.qrels)
    run = read_run(args.run)
    ours = json.load(open(args.ours))

    full = pytrec_eval.RelevanceEvaluator(qrels, {"ndcg_cut.10", "recall.100", "recall.1000"}).evaluate(run)
    top10 = pytrec_eval.RelevanceEvaluator(qrels, {"recip_rank"}).evaluate(truncate(run, 10))

    pairs = [
        ("ndcg_at_10", lambda q: full.get(q, {}).get("ndcg_cut_10", 0.0)),
        ("recall_at_100", lambda q: full.get(q, {}).get("recall_100", 0.0)),
        ("recall_at_1000", lambda q: full.get(q, {}).get("recall_1000", 0.0)),
        ("mrr_at_10", lambda q: top10.get(q, {}).get("recip_rank", 0.0)),
    ]
    qids = sorted(qrels)
    print(f"pytrec_eval {version('pytrec_eval')}, {len(qids)} queries, run={args.run}")
    print(f"{'metric':<16}{'strata':>10}{'pytrec_eval':>13}{'|diff|':>12}{'max per-query |diff|':>24}")
    ok = True
    for name, theirs in pairs:
        ref = sum(theirs(q) for q in qids) / len(qids)
        mine = ours["mean"][name]
        per_q = max(abs(ours["per_query"][q][name] - theirs(q)) for q in qids)
        diff = abs(mine - ref)
        ok &= diff <= TOLERANCE
        print(f"{name:<16}{mine:>10.4f}{ref:>13.4f}{diff:>12.2e}{per_q:>24.2e}")
    print("MATCH (all means within 5e-5)" if ok else "MISMATCH")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
