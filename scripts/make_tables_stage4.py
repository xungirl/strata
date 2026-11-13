"""Render the stage-4 tables of BENCHMARK.md from results/stage4 (stdlib only).

    python3 -I scripts/make_tables_stage4.py > results/stage4/tables.md
"""

import json
import os
import statistics

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
S = os.path.join(ROOT, "results", "stage4")
DATASETS = ["scifact", "nfcorpus", "fiqa", "scidocs", "trec-covid"]


def load(p):
    return json.load(open(p)) if os.path.exists(p) else None


def ms(xs, fmt="{:.0f}"):
    m = statistics.mean(xs)
    s = statistics.stdev(xs) if len(xs) > 1 else 0.0
    return (fmt + " ± " + fmt).format(m, s)


def main():
    rs = {d: load(os.path.join(S, f"{d}.json")) for d in DATASETS}
    hl = {d: load(os.path.join(S, "hnswlib", f"{d}.json")) for d in DATASETS}

    r0 = next(r for r in rs.values() if r)
    e = r0["embeddings"]
    print(f"Embeddings: `{e['model']}` (revision `{e['model_revision']}`), {r0['dim']} dims, L2-normalized, "
          f"computed on `{e['device']}` with torch {e['torch']} / sentence-transformers {e['sentence_transformers']}.\n")

    print("### Dataset, build and memory\n")
    print("| dataset | docs | test queries | embed docs (s) | HNSW build s (M=16, efC=200) | hnswlib build s (same params) | f32 vectors MB | int8 codes MB | graph MB | max level | avg layer-0 degree |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for d in DATASETS:
        r, h = rs[d], hl[d]
        if not r:
            continue
        hb = ms(h["build_secs"], "{:.1f}") if h else "-"
        print(f"| {d} | {r['docs']:,} | {r['test_queries']} | {r['embeddings']['seconds_docs']:.0f} | {ms(r['build_secs'], '{:.1f}')} | {hb} | "
              f"{r['vectors_bytes'] / 1e6:.1f} | {r['sq_bytes'] / 1e6:.1f} | {r['graph_bytes'] / 1e6:.1f} | {r['graph_max_level']} | {r['graph_avg_degree0']:.1f} |")
    print()

    print("### Dense retrieval effectiveness (bge-small-en-v1.5, exact search)\n")
    print("| dataset | nDCG@10 exact | nDCG@10 HNSW ef=100 | ef=200 | ef=400 | R@100 exact | exact search p50 µs (brute force, 1 thread) |")
    print("|---|---|---|---|---|---|---|")
    for d in DATASETS:
        r = rs[d]
        if not r:
            continue
        hm = {ef: m for ef, m in r["hnsw_metrics"]}
        print(f"| {d} | {r['exact_metrics']['ndcg_at_10']:.4f} | {hm[100]['ndcg_at_10']:.4f} | {hm[200]['ndcg_at_10']:.4f} | {hm[400]['ndcg_at_10']:.4f} | "
              f"{r['exact_metrics']['recall_at_100']:.4f} | {ms([x['p50_us'] for x in r['exact_latency']])} |")
    print()

    for d in DATASETS:
        r, h = rs[d], hl[d]
        if not r:
            continue
        print(f"### Recall@10 vs latency: {d} ({r['docs']:,} docs, {r['test_queries']} queries; p50 / p99 µs, mean of 3 passes)\n")
        print("| ef | strata f32 recall | strata f32 p50 / p99 | strata SQ8+rescore recall | SQ8 p50 / p99 | distance evals/query (f32) | hnswlib recall | hnswlib p50 / p99 (incl. Python call) |")
        print("|---|---|---|---|---|---|---|---|")
        hp = {p["ef"]: p for p in h["points"]} if h else {}
        for f, q in zip(r["hnsw_f32"], r["hnsw_sq8"]):
            lat = lambda p, k: statistics.mean(x[k] for x in p["latency"])
            hcell = (f"{hp[f['ef']]['recall_at_10']:.4f} | {lat(hp[f['ef']], 'p50_us'):.0f} / {lat(hp[f['ef']], 'p99_us'):.0f}"
                     if f["ef"] in hp else "- | -")
            print(f"| {f['ef']} | {f['recall_at_10']:.4f} | {lat(f, 'p50_us'):.0f} / {lat(f, 'p99_us'):.0f} | {q['recall_at_10']:.4f} | "
                  f"{lat(q, 'p50_us'):.0f} / {lat(q, 'p99_us'):.0f} | {f['avg_distance_evals']:,.0f} | {hcell} |")
        print()

    print("### Filtered ANN (synthetic bucket field; k=10; single pass)\n")
    print("| dataset | selectivity | allowed docs | ef | graph-filtered recall | graph p50 / p99 µs | exact scan of allowed docs p50 / p99 µs |")
    print("|---|---|---|---|---|---|---|")
    for d in DATASETS:
        r = rs[d]
        if not r:
            continue
        for p in r["filtered"]:
            print(f"| {d} | {p['selectivity_pct']}% | {p['allowed_docs']:,} | {p['ef']} | {p['graph_recall_at_10']:.4f} | "
                  f"{p['graph_latency']['p50_us']:.0f} / {p['graph_latency']['p99_us']:.0f} | {p['brute_latency']['p50_us']:.0f} / {p['brute_latency']['p99_us']:.0f} |")
    print()

    print("### Dot-product kernels (384 dims, ns per dot, 2M dots over the corpus)\n")
    print("| dataset | f32 scalar | f32 NEON | int8 scalar loop (LLVM -> SDOT) | int8 hand NEON (widening) |")
    print("|---|---|---|---|---|")
    for d in DATASETS:
        r = rs[d]
        if r:
            k = r["kernel_ns_per_dot"]
            print(f"| {d} | {k['f32_scalar']:.1f} | {k['f32_neon']:.1f} | {k['i8_autovec_sdot']:.1f} | {k['i8_neon_widening']:.1f} |")
    print()


if __name__ == "__main__":
    main()
