"""Render the stage-9 tables of BENCHMARK.md (stdlib only; calls the
release `strata-eval` binary to score the run files).

    python3 -I scripts/make_tables_stage9.py > results/stage9/tables.md
"""

import json
import os
import subprocess
from collections import defaultdict

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
S = os.path.join(ROOT, "results", "stage9")
R = os.path.join(S, "runs", "locomo-beir")
H = os.path.join(S, "hybrid", "runs", "locomo-beir")
EVAL = os.path.join(ROOT, "target", "release", "strata-eval")

SYSTEMS = [
    ("BM25, all memories (no tag filter)", os.path.join(R, "bm25-all.trec")),
    ("BM25 via MCP `recall`, tag filter conv (top 100)", os.path.join(R, "mcp-recall-bm25.trec")),
    ("dense bge-small exact, all memories", os.path.join(R, "dense-exact-all.trec")),
    ("dense bge-small exact, tag filter conv", os.path.join(R, "dense-exact-conv.trec")),
    ("hybrid RRF k=60 (MCP BM25 + dense conv)", os.path.join(H, "rrf-k60.trec")),
    ("hybrid min-max α=0.5 (MCP BM25 + dense conv)", os.path.join(H, "minmax-a0.5.trec")),
]


def evaluate(run):
    out = subprocess.run([EVAL, "eval", "--dataset", "locomo-beir", "--data-dir", os.path.join(ROOT, "data"), "--run", run],
                         check=True, capture_output=True, text=True).stdout
    return json.loads(out)


def main():
    cats = {}
    for line in open(os.path.join(ROOT, "data", "locomo-beir", "queries.jsonl")):
        q = json.loads(line)
        cats[q["_id"]] = q["category"]
    ncat = defaultdict(int)
    for c in cats.values():
        ncat[c] += 1
    reps = [json.load(open(os.path.join(S, f"mcp.r{r}", "mcp_eval.json"))) for r in (1, 2, 3)
            if os.path.exists(os.path.join(S, f"mcp.r{r}", "mcp_eval.json"))]
    rep = reps[0]
    print("### Agent memory over MCP (LoCoMo, 10 conversations, 5,882 dialogue turns as memories, 1,532 questions)\n")
    print(f"MCP server `{rep['server'].get('name')}` {rep['server'].get('version')} (built on the official Rust SDK `rmcp` 3.10.0; protocol {rep['protocol_version']}), driven over stdio by an "
          "independent stdlib-Python JSON-RPC client. Tools listed: " + ", ".join(f"`{t}`" for t in rep["tools"]) + ".\n")
    import statistics

    def ms(xs, f="{:.2f}"):
        sd = statistics.stdev(xs) if len(xs) > 1 else 0.0
        return (f + " ± " + f).format(statistics.mean(xs), sd)

    lat = [r["recall_latency_ms_incl_jsonrpc"] for r in reps]
    print(f"Mean ± std over {len(reps)} fresh runs.\n")
    print("| measurement | value |")
    print("|---|---|")
    print(f"| memories stored (12 `remember_many` calls of ≤ 500, each fsynced + committed) | {rep['memories']} |")
    print(f"| time to store all memories, s | {ms([r['ingest_seconds'] for r in reps])} |")
    print(f"| `recall` latency p50 ms (client-side, incl. JSON-RPC over pipes) | {ms([l['p50'] for l in lat], '{:.3f}')} |")
    print(f"| `recall` latency p99 ms | {ms([l['p99'] for l in lat], '{:.3f}')} |")
    print(f"| `recall` latency max ms | {ms([l['max'] for l in lat], '{:.1f}')} |")
    print()
    print("### Retrieval of the gold evidence turns (all 1,532 questions)\n")
    print("| system | nDCG@10 | R@100 | MRR@10 |")
    print("|---|---|---|---|")
    per = {}
    for name, run in SYSTEMS:
        r = evaluate(run)
        m = r["mean"]
        per[name] = r["per_query"]
        print(f"| {name} | {m['ndcg_at_10']:.4f} | {m['recall_at_100']:.4f} | {m['mrr_at_10']:.4f} |")
    print()
    print("### nDCG@10 by LoCoMo question category (category numbers as in the dataset file)\n")
    order = sorted(ncat)
    print("| system | " + " | ".join(f"cat {c} (n={ncat[c]})" for c in order) + " |")
    print("|---|" + "---|" * len(order))
    for name, _ in SYSTEMS:
        vals = defaultdict(list)
        for qid, m in per[name].items():
            vals[cats[qid]].append(m["ndcg_at_10"])
        # Questions with no results are missing from per_query; count them as 0.
        cells = []
        for c in order:
            cells.append(f"{sum(vals[c]) / ncat[c]:.4f}")
        print(f"| {name} | " + " | ".join(cells) + " |")
    print()
    print("### Paired significance (randomization test, 100,000 samples)\n")
    print("| comparison | metric | mean A | mean B | diff | p (randomization) | p (t-test) |")
    print("|---|---|---|---|---|---|---|")
    tests = json.load(open(os.path.join(S, "hybrid", "locomo-beir.json")))["tests"]
    rows = [(f"tag filter, BM25 (A = all, B = conv)", json.load(open(os.path.join(S, "compare", "bm25_all_vs_conv_filter.txt")))),
            (f"tag filter, dense (A = all, B = conv)", json.load(open(os.path.join(S, "compare", "dense_all_vs_conv_filter.txt"))))]
    for k in ("dense_vs_lexical", "rrf-k60_vs_lexical", "minmax-a0.5_vs_lexical", "minmax-a0.5_vs_dense"):
        rows.append((k.replace("_vs_", " vs ") + " (A = second)", tests[k]))
    for label, t in rows:
        for metric in ("ndcg_at_10", "recall_at_100"):
            x = t[metric]
            print(f"| {label} | {metric} | {x['mean_a']:.4f} | {x['mean_b']:.4f} | {x['mean_diff']:+.4f} | {x['p_randomization']:.2g} | {x['p_t_test']:.2g} |")
    print()


if __name__ == "__main__":
    main()
