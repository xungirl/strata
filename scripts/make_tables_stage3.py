"""Render the stage-3 tables of BENCHMARK.md from results/stage3 (stdlib only).

    python3 -I scripts/make_tables_stage3.py > results/stage3/tables.md
"""

import json
import os
import statistics

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
S = os.path.join(ROOT, "results", "stage3")
DATASETS = ["scifact", "nfcorpus", "fiqa", "scidocs", "trec-covid"]
SUF = "k1_0.9.b_0.4.exactlen.porter.lucenestop"
STRATS = [("taat", ""), ("wand", ".wand"), ("maxscore", ".maxscore")]


def load(path):
    return json.load(open(path)) if os.path.exists(path) else None


def mean(xs):
    return statistics.mean(xs)


def ms(xs, fmt="{:.0f}"):
    s = statistics.stdev(xs) if len(xs) > 1 else 0.0
    return (fmt + " ± " + fmt).format(mean(xs), s)


def rep(r, f):
    return [f(x) for x in r["repeats"]]


def tag(fm, st_suffix, extra=""):
    return f"strata-disk.f10000.m4{'.fm' if fm else ''}{st_suffix}{extra}.{SUF}"


def ident(path):
    if not os.path.exists(path):
        return "missing"
    return "identical" if "byte-identical (qid, docid, rank, score columns): YES" in open(path).read() else "DIFFERENT"


def main():
    print("### Exactness: pruned runs vs the stage-1 exhaustive run\n")
    print("| dataset | BMW, multi-seg | BMW, 1 seg | MaxScore, multi-seg | MaxScore, 1 seg |")
    print("|---|---|---|---|---|")
    for d in DATASETS:
        cells = [ident(os.path.join(S, "parity", f"{d}.{st}{seg}.txt")) for st in ("wand", "maxscore") for seg in ("", ".fm")]
        print(f"| {d} | " + " | ".join(cells) + " |")
    print("\n(identical = TREC run files byte-identical in qid, doc id, rank and score at depth 1000)\n")

    for fm in (False, True):
        print(f"### Latency, {'1 segment (force-merged)' if fm else 'multi-segment (flush 10k, merge factor 4)'} (µs, mean ± std of 3 passes)\n")
        print("| dataset | strategy | p50 k=10 | p99 k=10 | p50 k=1000 | p99 k=1000 | p50 speedup vs TAAT (k=10 / k=1000) |")
        print("|---|---|---|---|---|---|---|")
        for d in DATASETS:
            base = load(os.path.join(S, d, f"{tag(fm, '')}.json"))
            for name, suf in STRATS:
                r = load(os.path.join(S, d, f"{tag(fm, suf)}.json"))
                if not (r and base):
                    continue
                p10 = mean(rep(base, lambda x: x["latency_k10"]["p50_us"])) / mean(rep(r, lambda x: x["latency_k10"]["p50_us"]))
                pk = mean(rep(base, lambda x: x["latency"]["p50_us"])) / mean(rep(r, lambda x: x["latency"]["p50_us"]))
                print(f"| {d} | {name} | {ms(rep(r, lambda x: x['latency_k10']['p50_us']))} | {ms(rep(r, lambda x: x['latency_k10']['p99_us']))} | "
                      f"{ms(rep(r, lambda x: x['latency']['p50_us']))} | {ms(rep(r, lambda x: x['latency']['p99_us']))} | {p10:.2f}x / {pk:.2f}x |")
        print()

    print("### Pruning work (1 segment): documents fully scored per query vs query-term postings\n")
    print("| dataset | postings per query | BMW scored, k=10 | MaxScore scored, k=10 | BMW scored, k=1000 | MaxScore scored, k=1000 |")
    print("|---|---|---|---|---|---|")
    for d in DATASETS:
        w = load(os.path.join(S, d, f"{tag(True, '.wand')}.json"))
        m = load(os.path.join(S, d, f"{tag(True, '.maxscore')}.json"))
        if not (w and m):
            continue
        ww, mw = w["wand_work"][0], m["wand_work"][0]
        pct = lambda x: f"{x:,.0f} ({100 * x / ww['postings_per_query']:.1f}%)"
        print(f"| {d} | {ww['postings_per_query']:,.0f} | {pct(ww['docs_scored_per_query_k10'])} | {pct(mw['docs_scored_per_query_k10'])} | "
              f"{pct(ww['docs_scored_per_query_k'])} | {pct(mw['docs_scored_per_query_k'])} |")
    print()

    print("### Filtered search (synthetic `bucket` field, k=10 p50 µs, mean ± std of 3 passes)\n")
    print("| dataset | filter (selectivity) | TAAT + filter | BMW + filter | MaxScore + filter | BMW / MaxScore == TAAT? |")
    print("|---|---|---|---|---|---|")
    for d in ("trec-covid", "fiqa"):
        for f, sel in (("bucket:[0 TO 0]", "1%"), ("bucket:[0 TO 9]", "10%"), ("bucket:[0 TO 49]", "50%")):
            ft = "".join(c if c.isalnum() else "_" for c in f)
            cells = []
            for _, suf in STRATS:
                r = load(os.path.join(S, d, f"{tag(False, suf, '.bucket.filter-' + ft)}.json"))
                cells.append(ms(rep(r, lambda x: x["latency_k10"]["p50_us"])) if r else "-")
            par = " / ".join(ident(os.path.join(S, "parity", f"{d}.filter-{ft}-{st}.txt")) for st in ("wand", "maxscore"))
            print(f"| {d} | `{f}` ({sel}) | " + " | ".join(cells) + f" | {par} |")
    print()

    print("### Structured queries (multi-segment index with the bucket field; k=10; µs, mean of 3 passes)\n")
    print("| dataset | query type | queries | p50 | p99 | avg hits | example |")
    print("|---|---|---|---|---|---|---|")
    for d in ("trec-covid", "fiqa"):
        q = load(os.path.join(S, "qbench", f"{d}.json"))
        if not q:
            continue
        for t in q:
            ex = t["examples"][0].replace("|", "\\|") if t["examples"] else ""
            print(f"| {d} | {t['query_type']} | {t['queries']} | {mean(t['p50_us']):.0f} | {mean(t['p99_us']):.0f} | {t['avg_hits']:.1f} | `{ex}` |")
    print()


if __name__ == "__main__":
    main()
