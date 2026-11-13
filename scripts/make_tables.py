"""Render the markdown tables in BENCHMARK.md from results/*.json.

    python3 -I scripts/make_tables.py > results/tables.md

Every number in BENCHMARK.md tables comes from this script's output, so
nothing is copied by hand.
"""

import json
import os
import re
import statistics
import sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
RES = os.path.join(ROOT, "results")
DATASETS = ["scifact", "nfcorpus", "fiqa", "scidocs", "trec-covid"]

DEFAULT = "strata.k1_0.9.b_0.4.exactlen.porter.lucenestop"
ANSERINI_MODE = "strata.k1_0.9.b_0.4.lucenelen.porter.lucenestop"
CLASSIC = "strata.k1_1.2.b_0.75.exactlen.porter.lucenestop"
TANTIVY_MODE = "strata.k1_1.2.b_0.75.lucenelen.porter.lucenestop"
T_STRATA = "tantivy-0.27.0.strata-analyzer"
T_NATIVE = "tantivy-0.27.0.native-analyzer"
PORTER2 = "strata.k1_0.9.b_0.4.exactlen.porter2.lucenestop"
NOSTEM = "strata.k1_0.9.b_0.4.exactlen.nostem.lucenestop"
NOSTOP = "strata.k1_0.9.b_0.4.exactlen.porter.nostop"


def load(ds, tag):
    path = os.path.join(RES, ds, tag + ".json")
    if not os.path.exists(path):
        return None
    with open(path) as f:
        return json.load(f)


def anserini():
    ref = {}
    with open(os.path.join(RES, "reference", "anserini_beir_flat.md")) as f:
        for line in f:
            cols = [c.strip() for c in line.strip().strip("|").split("|")]
            if len(cols) >= 6 and cols[0] in DATASETS:
                ref[cols[0]] = {
                    "ndcg": float(cols[1]), "r100": float(cols[2]), "r1000": float(cols[3]),
                    "docs": int(cols[4]), "terms": int(cols[5]),
                }
    return ref


def m(r, key):
    return r["metrics"]["mean"][key]


def ms(values, fmt="{:.0f}"):
    mean = statistics.mean(values)
    sd = statistics.stdev(values) if len(values) > 1 else 0.0
    return (fmt + " ± " + fmt).format(mean, sd)


def reps(r, f):
    return [f(x) for x in r["repeats"]]


def table_anserini(ref):
    print("### BM25 effectiveness vs. Anserini (k1=0.9, b=0.4)\n")
    print("| dataset | metric | Anserini (published) | strata exact lengths | strata Lucene-quantized lengths |")
    print("|---|---|---|---|---|")
    for ds in DATASETS:
        a, b = load(ds, DEFAULT), load(ds, ANSERINI_MODE)
        if not (a and b and ds in ref):
            continue
        for key, name, rk in [("ndcg_at_10", "nDCG@10", "ndcg"), ("recall_at_100", "R@100", "r100"),
                              ("recall_at_1000", "R@1000", "r1000")]:
            want = ref[ds][rk]
            got_a, got_b = m(a, key), m(b, key)
            mark = lambda g: f"{g:.4f}" + (" (=)" if f"{g:.4f}" == f"{want:.4f}" else f" ({g - want:+.4f})")
            print(f"| {ds} | {name} | {want:.4f} | {mark(got_a)} | {mark(got_b)} |")
    print()
    print("| dataset | docs (strata / Anserini non-empty) | total tokens strata | total tokens Lucene | diff |")
    print("|---|---|---|---|---|")
    for ds in DATASETS:
        a = load(ds, DEFAULT)
        if not (a and ds in ref):
            continue
        t, lt = a["index"]["total_tokens"], ref[ds]["terms"]
        print(f"| {ds} | {a['index']['docs']} / {ref[ds]['docs']} | {t:,} | {lt:,} | {t - lt:+,} ({100 * (t - lt) / lt:+.3f}%) |")
    print()


def table_params():
    print("### Parameter and analyzer ablations (nDCG@10)\n")
    cols = [(DEFAULT, "default k1=0.9 b=0.4"), (CLASSIC, "k1=1.2 b=0.75"), (PORTER2, "Porter2 stemmer"),
            (NOSTEM, "no stemming"), (NOSTOP, "no stopwords")]
    print("| dataset | " + " | ".join(c[1] for c in cols) + " |")
    print("|---|" + "---|" * len(cols))
    for ds in DATASETS:
        row = []
        for tag, _ in cols:
            r = load(ds, tag)
            row.append(f"{m(r, 'ndcg_at_10'):.4f}" if r else "-")
        print(f"| {ds} | " + " | ".join(row) + " |")
    print()


def table_tantivy():
    print("### strata vs. Tantivy 0.27 (k1=1.2, b=0.75, identical terms)\n")
    print("Quality (strata with Lucene-quantized lengths, which is also how Tantivy stores lengths):\n")
    print("| dataset | strata nDCG@10 | Tantivy nDCG@10 | strata R@100 | Tantivy R@100 | Tantivy (native analyzer) nDCG@10 |")
    print("|---|---|---|---|---|---|")
    for ds in DATASETS:
        s, t, n = load(ds, TANTIVY_MODE), load(ds, T_STRATA), load(ds, T_NATIVE)
        if not (s and t and n):
            continue
        print(f"| {ds} | {m(s, 'ndcg_at_10'):.4f} | {m(t, 'ndcg_at_10'):.4f} | {m(s, 'recall_at_100'):.4f} | "
              f"{m(t, 'recall_at_100'):.4f} | {m(n, 'ndcg_at_10'):.4f} |")
    print()
    print("Speed, mean ± sample std over 3 in-process repeats (single thread; latency = query analysis + search):\n")
    print("| dataset | engine | index docs/s | p50 µs (k=1000) | p99 µs (k=1000) | p50 µs (k=10) | p99 µs (k=10) | index bytes |")
    print("|---|---|---|---|---|---|---|---|")
    for ds in DATASETS:
        s, t = load(ds, TANTIVY_MODE), load(ds, T_STRATA)
        if not (s and t):
            continue
        for name, r, size in [("strata", s, f"{s['index']['heap_size_bytes_estimate']:,} (uncompressed heap)"),
                              ("Tantivy", t, f"{t['repeats'][0]['index_bytes']:,} (compressed, in RAM)")]:
            print(f"| {ds} | {name} | {ms(reps(r, lambda x: x['docs_per_sec']))} | "
                  f"{ms(reps(r, lambda x: x['latency']['p50_us']))} | {ms(reps(r, lambda x: x['latency']['p99_us']))} | "
                  f"{ms(reps(r, lambda x: x['latency_k10']['p50_us']))} | {ms(reps(r, lambda x: x['latency_k10']['p99_us']))} | {size} |")
    print()


def table_perf():
    print("### strata default configuration: indexing and query latency (3 repeats)\n")
    print("| dataset | docs | queries | analysis-only s | index s (incl. analysis) | docs/s | p50 µs | p99 µs | QPS | p50 µs k=10 | p99 µs k=10 |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for ds in DATASETS:
        r = load(ds, DEFAULT)
        if not r:
            continue
        q = r["repeats"][0]["latency"]["queries"]
        print(f"| {ds} | {r['index']['docs']:,} | {q} | {ms(reps(r, lambda x: x['analyze_secs']), '{:.3f}')} | "
              f"{ms(reps(r, lambda x: x['index_secs']), '{:.3f}')} | {ms(reps(r, lambda x: x['docs_per_sec']))} | "
              f"{ms(reps(r, lambda x: x['latency']['p50_us']))} | {ms(reps(r, lambda x: x['latency']['p99_us']))} | "
              f"{ms(reps(r, lambda x: x['latency']['qps']))} | {ms(reps(r, lambda x: x['latency_k10']['p50_us']))} | "
              f"{ms(reps(r, lambda x: x['latency_k10']['p99_us']))} |")
    print()
    print("Raw per-repeat values (docs/s; p50/p99 µs at k=1000):\n")
    print("| dataset | repeat 1 | repeat 2 | repeat 3 |")
    print("|---|---|---|---|")
    for ds in DATASETS:
        r = load(ds, DEFAULT)
        if not r:
            continue
        cells = [f"{x['docs_per_sec']:.0f} docs/s; {x['latency']['p50_us']:.0f}/{x['latency']['p99_us']:.0f} µs" for x in r["repeats"]]
        print(f"| {ds} | " + " | ".join(cells) + " |")
    print()
    print("Index shape:\n")
    print("| dataset | docs | terms | postings | total tokens | avg doc len |")
    print("|---|---|---|---|---|---|")
    for ds in DATASETS:
        r = load(ds, DEFAULT)
        if not r:
            continue
        i = r["index"]
        print(f"| {ds} | {i['docs']:,} | {i['terms']:,} | {i['postings']:,} | {i['total_tokens']:,} | {i['avg_doc_len']:.1f} |")
    print()


def table_memory():
    path = os.path.join(RES, "memory.tsv")
    if not os.path.exists(path):
        return
    print("### Index heap size (allocator-measured, `strata-mem`)\n")
    print("| dataset | corpus text MB | builder live MB | segment live MB | segment estimate MB | bytes/posting | segment / text |")
    print("|---|---|---|---|---|---|---|")
    with open(path) as f:
        next(f)
        for line in f:
            d, docs, text, builder, seg, est, post, bpp = line.split("\t")
            mb = lambda x: f"{int(x) / 1e6:.1f}"
            print(f"| {d} | {mb(text)} | {mb(builder)} | {mb(seg)} | {mb(est)} | {float(bpp):.2f} | {int(seg) / int(text):.2f}× |")
    print()


def table_crosscheck():
    print("### Metric cross-check vs. pytrec_eval 0.5\n")
    print("| run | nDCG@10 | R@100 | R@1000 | MRR@10 | max per-query \\|diff\\| | verdict |")
    print("|---|---|---|---|---|---|---|")
    d = os.path.join(RES, "crosscheck")
    for name in sorted(os.listdir(d)):
        text = open(os.path.join(d, name)).read()
        vals, worst = {}, 0.0
        for line in text.splitlines():
            parts = line.split()
            if parts and parts[0] in ("ndcg_at_10", "recall_at_100", "recall_at_1000", "mrr_at_10"):
                vals[parts[0]] = f"{parts[1]} / {parts[2]}"
                worst = max(worst, float(parts[4]))
        verdict = "MATCH" if "MATCH (" in text else "MISMATCH"
        print(f"| {name[:-4]} | {vals.get('ndcg_at_10')} | {vals.get('recall_at_100')} | {vals.get('recall_at_1000')} | "
              f"{vals.get('mrr_at_10')} | {worst:.1e} | {verdict} |")
    print("\n(cells: strata / pytrec_eval)\n")


def main():
    ref = anserini()
    table_anserini(ref)
    table_params()
    table_tantivy()
    table_perf()
    table_memory()
    table_crosscheck()


if __name__ == "__main__":
    sys.exit(main())
