"""Cross-check strata's paired t-test against SciPy on the same per-query data.

Recomputes per-query nDCG@10 of two runs with pytrec_eval, runs
scipy.stats.ttest_rel, and compares with the output of `strata-eval compare`:

    strata-eval compare --dataset scifact --a A.trec --b B.trec > cmp.json
    uv run --with scipy==1.14.1 --with pytrec_eval==0.5 --with numpy python -I scripts/crosscheck_stats.py \
        --qrels data/scifact/qrels/test.tsv --a A.trec --b B.trec --strata-json cmp.json
"""

import argparse
import json

import numpy as np
import pytrec_eval
from scipy import stats


def read_qrels(path):
    q = {}
    with open(path) as f:
        next(f)
        for line in f:
            a, b, r = line.rstrip("\n").split("\t")
            q.setdefault(a, {})[b] = int(r)
    return q


def read_run(path):
    r = {}
    for line in open(path):
        q, _, d, _, s, _ = line.split()
        r.setdefault(q, {})[d] = float(s)
    return r


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--qrels", required=True)
    ap.add_argument("--a", required=True)
    ap.add_argument("--b", required=True)
    ap.add_argument("--strata-json", required=True, help="output of `strata-eval compare`")
    args = ap.parse_args()
    qrels = read_qrels(args.qrels)
    ev = pytrec_eval.RelevanceEvaluator(qrels, {"ndcg_cut.10"})
    ea, eb = ev.evaluate(read_run(args.a)), ev.evaluate(read_run(args.b))
    qids = sorted(qrels)
    a = np.array([ea.get(q, {}).get("ndcg_cut_10", 0.0) for q in qids])
    b = np.array([eb.get(q, {}).get("ndcg_cut_10", 0.0) for q in qids])
    t = stats.ttest_rel(b, a)
    mine = json.load(open(args.strata_json))["ndcg_at_10"]
    print(f"scipy  : t={t.statistic:.6f} p={t.pvalue:.6g} mean_diff={np.mean(b - a):.6f}")
    print(f"strata : t={mine['t_stat']:.6f} p={mine['p_t_test']:.6g} mean_diff={mine['mean_diff']:.6f}")
    ok = abs(t.statistic - mine["t_stat"]) < 1e-6 and abs(t.pvalue - mine["p_t_test"]) < 1e-6 * max(1.0, t.pvalue) + 1e-12
    print("MATCH" if ok else "MISMATCH")


if __name__ == "__main__":
    main()
