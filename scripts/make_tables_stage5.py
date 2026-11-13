"""Render the stage-5 tables of BENCHMARK.md from results/stage5 (stdlib only).

    python3 -I scripts/make_tables_stage5.py > results/stage5/tables.md
"""

import json
import os

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
S = os.path.join(ROOT, "results", "stage5")
DATASETS = ["scifact", "nfcorpus", "fiqa", "scidocs", "trec-covid"]


def load(p):
    return json.load(open(p)) if os.path.exists(p) else None


def sig(t):
    """Mark significance at 0.05 (randomization test, two-sided)."""
    p = t["p_randomization"]
    return f"{t['mean_diff']:+.4f} (p={p:.3g}){'*' if p < 0.05 else ''}"


def main():
    print("### nDCG@10: lexical vs dense vs hybrid vs re-ranked\n")
    print("| dataset | BM25 | dense (bge-small, exact) | RRF k=60 | min-max α=0.5 | z-score α=0.5 | RRF + cross-encoder re-rank (top 100) |")
    print("|---|---|---|---|---|---|---|")
    for d in DATASETS:
        r = load(os.path.join(S, f"{d}.json"))
        rr = load(os.path.join(S, f"{d}.rerank.eval.json"))
        if not r:
            continue
        m = r["metrics"]
        cells = [m[k]["ndcg_at_10"] for k in ("lexical-bm25", "dense", "rrf-k60", "minmax-a0.5", "zscore-a0.5")]
        best = max(cells + ([rr["mean"]["ndcg_at_10"]] if rr else []))
        fmt = lambda v: f"**{v:.4f}**" if v == best else f"{v:.4f}"
        print(f"| {d} | " + " | ".join(fmt(v) for v in cells) + f" | {fmt(rr['mean']['ndcg_at_10']) if rr else '-'} |")
    print("\n(bold = best per dataset)\n")

    print("### Recall@100\n")
    print("| dataset | BM25 | dense | RRF | min-max α=0.5 | z-score α=0.5 | re-ranked |")
    print("|---|---|---|---|---|---|---|")
    for d in DATASETS:
        r = load(os.path.join(S, f"{d}.json"))
        rr = load(os.path.join(S, f"{d}.rerank.eval.json"))
        if not r:
            continue
        m = r["metrics"]
        cells = [m[k]["recall_at_100"] for k in ("lexical-bm25", "dense", "rrf-k60", "minmax-a0.5", "zscore-a0.5")]
        tail = f" | {rr['mean']['recall_at_100']:.4f} |" if rr else " | - |"
        print(f"| {d} | " + " | ".join(f"{v:.4f}" for v in cells) + tail)
    print()

    print("### Paired significance (nDCG@10 difference, two-sided randomization test, 100,000 samples; * = p < 0.05)\n")
    print("| dataset | queries | RRF − BM25 | RRF − dense | min-max − dense | re-rank − RRF | re-rank − BM25 |")
    print("|---|---|---|---|---|---|---|")
    for d in DATASETS:
        r = load(os.path.join(S, f"{d}.json"))
        c1 = load(os.path.join(S, "compare", f"{d}.rerank_vs_rrf.json"))
        c2 = load(os.path.join(S, "compare", f"{d}.rerank_vs_lexical.json"))
        if not (r and c1 and c2):
            continue
        t = r["tests"]
        print(f"| {d} | {t['rrf-k60_vs_lexical']['ndcg_at_10']['n']} | {sig(t['rrf-k60_vs_lexical']['ndcg_at_10'])} | "
              f"{sig(t['rrf-k60_vs_dense']['ndcg_at_10'])} | {sig(t['minmax-a0.5_vs_dense']['ndcg_at_10'])} | "
              f"{sig(c1['ndcg_at_10'])} | {sig(c2['ndcg_at_10'])} |")
    print("\nFive datasets × five comparisons = 25 tests; with a Bonferroni correction the per-test threshold would be 0.002.\n")

    print("### Oracle analysis: min-max α sweep on the *test* queries (not a fair result, shows sensitivity)\n")
    print("| dataset | " + " | ".join(f"α={a / 10:.1f}" for a in range(11)) + " | best α |")
    print("|---|" + "---|" * 12)
    for d in DATASETS:
        r = load(os.path.join(S, f"{d}.json"))
        if not r:
            continue
        vals = [r["metrics"][f"sweep-minmax-a{a / 10:.1f}"]["ndcg_at_10"] for a in range(11)]
        best = max(range(11), key=lambda i: vals[i])
        print(f"| {d} | " + " | ".join(f"{v:.4f}" for v in vals) + f" | {best / 10:.1f} |")
    print("\n(α = weight of BM25; α=0 is dense only, α=1 is BM25 only)\n")

    print("### Cross-encoder re-ranking cost (cross-encoder/ms-marco-MiniLM-L-6-v2, top 100)\n")
    print("| dataset | pairs | seconds | pairs/s | device |")
    print("|---|---|---|---|---|")
    for d in DATASETS:
        m = load(os.path.join(S, "logs", f"{d}.rerank.meta.json"))
        if m:
            print(f"| {d} | {m['pairs']:,} | {m['seconds']:.1f} | {m['pairs_per_second']:.0f} | {m['device']} |")
    print()

    print("### t-test cross-check vs SciPy (RRF vs BM25 per-query nDCG@10)\n")
    print("| dataset | result |")
    print("|---|---|")
    for d in DATASETS:
        p = os.path.join(S, "crosscheck", f"{d}.ttest.txt")
        if os.path.exists(p):
            lines = open(p).read().strip().splitlines()
            print(f"| {d} | {lines[-1]} ({lines[0].strip()}) |")
    print()


if __name__ == "__main__":
    main()
