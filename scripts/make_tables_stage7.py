"""Render the stage-7 tables of BENCHMARK.md from results/stage7 (stdlib only).

    python3 -I scripts/make_tables_stage7.py > results/stage7/tables.md
"""

import glob
import json
import os
import re
import statistics

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
S = os.path.join(ROOT, "results", "stage7")
DATASETS = ["scifact", "nfcorpus", "fiqa", "scidocs", "trec-covid"]


def load(p):
    return json.load(open(p))


def ms(xs, fmt="{:.2f}"):
    s = statistics.stdev(xs) if len(xs) > 1 else 0.0
    return (fmt + " ± " + fmt).format(statistics.mean(xs), s)


def grab(path, pattern):
    m = re.search(pattern, open(path).read())
    return m.group(1) if m else "?"


def parity():
    print("### Sharded (4 shards) vs single node, all BEIR test queries, k=1000\n")
    print("Single node = the stage-1 run file; cluster = 4 `strata-server` shards behind `strata-coord`, "
          "documents routed by FNV-1a(id) % 4, two-phase scatter-gather with global statistics.\n")
    print("| dataset | docs per shard | nDCG@10 single / cluster | R@1000 single / cluster | identical top-100 lists | identical top-1000 lists | differing lists explained by a score tie at rank 1000 | max abs score diff |")
    print("|---|---|---|---|---|---|---|---|")
    for ds in DATASETS:
        p = os.path.join(S, "parity")
        if not os.path.exists(os.path.join(p, f"{ds}.txt")):
            continue
        sizes = [l.split()[-1] for l in open(os.path.join(p, f"{ds}.shard_sizes.txt")) if l.strip()]
        a = load(os.path.join(p, f"{ds}.eval_single.txt"))["mean"]
        b = load(os.path.join(p, f"{ds}.eval_cluster.txt"))["mean"]
        txt = open(os.path.join(p, f"{ds}.txt")).read()
        top = re.findall(r"identical top-(\d+) lists: (\d+/\d+)", txt)
        diffs = re.findall(r"max \|score diff\| on shared top-\d+ docs: (\S+)", txt)
        ties = open(os.path.join(p, f"{ds}.ties.txt")).read()
        differ = grab(os.path.join(p, f"{ds}.ties.txt"), r"lists differ: (\d+)")
        expl = grab(os.path.join(p, f"{ds}.ties.txt"), r"explained by score ties only: (\d+)")
        notexpl = grab(os.path.join(p, f"{ds}.ties.txt"), r"NOT explained by ties: (\d+)")
        assert notexpl == "0", ties
        t = dict((k, v) for k, v in top)
        print(f"| {ds} | {'/'.join(sizes)} | {a['ndcg_at_10']:.6f} / {b['ndcg_at_10']:.6f} | {a['recall_at_1000']:.6f} / {b['recall_at_1000']:.6f} | "
              f"{t.get('100')} | {t.get('1000')} | {expl}/{differ} | {max(float(x) for x in diffs):g} |")
    print()


def tail():
    names = [("healthy-1replica", "healthy, 1 replica per shard, no hedging"),
             ("faulty-nohedge", "2 replicas, one delays 5% by 50 ms, no hedging"),
             ("faulty-hedge5ms", "same, hedge after 5 ms"),
             ("faulty-hedge15ms", "same, hedge after 15 ms")]
    print("### Hedged requests vs an injected slow replica (fiqa, 2 shards x 2 replicas, open loop 200 req/s, 20 s; mean ± std of 3 runs)\n")
    print("Replica 7721 (shard 0) sleeps 50 ms before 5% of the `/v1/search` calls it receives (`/v1/stats` is not delayed). "
          "Each coordinator search makes 2 rounds (stats, search) x 2 shards = 4 shard calls; the coordinator rotates the "
          "first replica it tries, so about half of shard-0 search calls go to the faulty replica first, i.e. about 2.5% of "
          "searches hit a delay without hedging. 'healthy' uses only the 3 normal replicas (one per shard).\n")
    print("| config | achieved req/s | p50 ms | p90 ms | p99 ms | p99.9 ms | max ms | extra shard requests from hedging % | hedges won / fired | errors |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for key, label in names:
        runs = sorted(glob.glob(os.path.join(S, "tail", f"{key}.[0-9].json")))
        if not runs:
            continue
        full = [load(p) for p in runs]
        rs = [f["summary"] for f in full]
        cs = [load(p.replace(".json", ".coord.json")) for p in runs]
        # Coordinator counters also cover the warm-up, so use ratios:
        # extra load = backup requests / primary requests.
        extra = [100.0 * c["hedges_fired"] / max(1, c["requests_sent"] - c["hedges_fired"]) for c in cs]
        won = [c["hedges_won"] / c["hedges_fired"] if c["hedges_fired"] else 0.0 for c in cs]
        p90 = [r.get("p90_ms", float("nan")) for r in rs]
        print(f"| {label} | {ms([r['throughput_rps'] for r in rs], '{:.0f}')} | {ms([r['p50_ms'] for r in rs])} | {ms(p90)} | "
              f"{ms([r['p99_ms'] for r in rs])} | {ms([r['p999_ms'] for r in rs])} | {ms([r['max_ms'] for r in rs], '{:.1f}')} | "
              f"{ms(extra)} | {ms(won)} | {sum(r['errors'] for r in rs)} |")
    print()


def partial():
    p = os.path.join(S, "partial")
    if not os.path.isdir(p):
        return
    print("### Timeouts and partial results (deadline 500 ms per shard call)\n")
    print("| scenario | wall time s (curl) | partial | failed_shards | hits returned |")
    print("|---|---|---|---|---|")
    for f in sorted(glob.glob(os.path.join(p, "*.json"))):
        name = os.path.basename(f)[:-5]
        body = load(f)
        real = grab(f[:-5] + ".time", r"real\s+(\S+)")
        print(f"| {name} | {real} | {body['partial']} | {body['failed_shards']} | {len(body['hits'])} |")
    print()


if __name__ == "__main__":
    parity()
    tail()
    partial()
