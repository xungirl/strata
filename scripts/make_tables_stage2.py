"""Render the stage-2 tables of BENCHMARK.md from results/stage2 (stdlib only).

    python3 -I scripts/make_tables_stage2.py > results/stage2/tables.md
"""

import json
import os
import statistics

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
R = os.path.join(ROOT, "results")
S = os.path.join(R, "stage2")
DATASETS = ["scifact", "nfcorpus", "fiqa", "scidocs", "trec-covid"]
SUF = "k1_0.9.b_0.4.exactlen.porter.lucenestop"
MEM = f"strata-mem-session2.{SUF}"
DISK = f"strata-disk.f10000.m4.{SUF}"
DISKFM = f"strata-disk.f10000.m4.fm.{SUF}"
TPOS = "tantivy-0.27.0.strata-analyzer.pos"


def load(path):
    return json.load(open(path)) if os.path.exists(path) else None


def ms(xs, fmt="{:.0f}"):
    m = statistics.mean(xs)
    s = statistics.stdev(xs) if len(xs) > 1 else 0.0
    return (fmt + " ± " + fmt).format(m, s)


def reps(r, f):
    return [f(x) for x in r["repeats"]]


def mem_bytes():
    out = {}
    with open(os.path.join(R, "memory.tsv")) as f:
        next(f)
        for line in f:
            c = line.split("\t")
            out[c[0]] = (int(c[4]), int(c[6]))  # segment live bytes, postings
    return out


def main():
    mem = mem_bytes()
    print("### Parity with the stage-1 in-memory index\n")
    print("| dataset | disk, multi-segment (flush 10k, merge factor 4) | disk, force-merged to 1 segment |")
    print("|---|---|---|")
    for d in DATASETS:
        cells = []
        for tag in ("strata-disk.f10000.m4", "strata-disk.f10000.m4.fm"):
            p = os.path.join(S, "parity", f"{d}.{tag}.{SUF}.txt")
            t = open(p).read() if os.path.exists(p) else ""
            ident = "byte-identical" if "byte-identical (qid, docid, rank, score columns): YES" in t else "DIFFERENT"
            segs = load(os.path.join(S, d, f"{tag}.{SUF}.json"))
            cells.append(f"{ident} ({segs['disk']['segments']} segments)" if segs else ident)
        print(f"| {d} | {cells[0]} | {cells[1]} |")
    print()

    print("### Index size\n")
    print("| dataset | postings | in-memory heap (stage 1) | disk segment files (1 segment, with positions) | Tantivy, with positions | Tantivy, no positions (stage 1) | strata disk / Tantivy pos |")
    print("|---|---|---|---|---|---|---|")
    for d in DATASETS:
        fm = load(os.path.join(S, d, f"{DISKFM}.json"))
        tp = load(os.path.join(S, d, f"{TPOS}.json"))
        tn = load(os.path.join(R, d, "tantivy-0.27.0.strata-analyzer.json"))
        if not (fm and tp and tn):
            continue
        heap, postings = mem[d]
        seg = fm["disk"]["segment_bytes"]
        tpos = tp["repeats"][0]["index_bytes"]
        tno = tn["repeats"][0]["index_bytes"]
        mb = lambda b: f"{b / 1e6:.1f} MB ({b / postings:.2f} B/posting)"
        print(f"| {d} | {postings:,} | {mb(heap)} | {mb(seg)} | {mb(tpos)} | {mb(tno)} | {seg / tpos:.2f}x |")
    print()

    print("### Ingest throughput, docs/s (single writer thread, mean ± std of 3 builds)\n")
    print("| dataset | in-memory (no durability) | disk: WAL fsync per 1000 docs + flush/merge | disk + force-merge | Tantivy (positions, 1 thread, RAM dir) |")
    print("|---|---|---|---|---|")
    for d in DATASETS:
        rs = [load(os.path.join(S, d, f"{t}.json")) for t in (MEM, DISK, DISKFM, TPOS)]
        if not all(rs):
            continue
        print(f"| {d} | " + " | ".join(ms(reps(r, lambda x: x["docs_per_sec"])) for r in rs) + " |")
    print()

    print("### Query latency, exhaustive TAAT, single thread, same session (µs; mean ± std of 3 passes)\n")
    print("| dataset | engine | p50 k=1000 | p99 k=1000 | p50 k=10 | p99 k=10 | load avg (1 min) per repeat |")
    print("|---|---|---|---|---|---|---|")
    for d in DATASETS:
        for name, t in (("in-memory", MEM), ("disk, multi-seg", DISK), ("disk, 1 seg", DISKFM), ("Tantivy pos (BMW)", TPOS)):
            r = load(os.path.join(S, d, f"{t}.json"))
            if not r:
                continue
            loads = ", ".join(f"{x.get('load_avg_1m', float('nan')):.1f}" for x in r["repeats"]) if "load_avg_1m" in r["repeats"][0] else "n/a"
            print(f"| {d} | {name} | {ms(reps(r, lambda x: x['latency']['p50_us']))} | {ms(reps(r, lambda x: x['latency']['p99_us']))} | "
                  f"{ms(reps(r, lambda x: x['latency_k10']['p50_us']))} | {ms(reps(r, lambda x: x['latency_k10']['p99_us']))} | {loads} |")
    print()

    print("### WAL group commit: ingest throughput vs docs per fsync (scifact, 5,183 docs)\n")
    print("| docs per `add_documents` (= per fsync) | fsyncs | docs/s (mean ± std, 3 builds) |")
    print("|---|---|---|")
    for b in (1, 10, 100, 1000):
        r = load(os.path.join(S, "scifact", f"strata-disk.batch{b}.json"))
        if r:
            print(f"| {b} | {r['disk']['fsyncs_for_ingest']:,} | {ms(reps(r, lambda x: x['docs_per_sec']))} |")
    print()

    c = load(os.path.join(S, "chaos.json"))
    n = load(os.path.join(S, "chaos_negative_control.json"))
    if c and n:
        print("### Crash consistency (kill -9)\n")
        print("| run | kills | acked adds | acked deletes | lost acked adds | resurrected deletes | recovery p50 / max |")
        print("|---|---|---|---|---|---|---|")
        for name, x in (("strata (correct acks)", c), ("negative control: ack before write", n)):
            print(f"| {name} | {x['iterations']} | {x['total_acked_adds']:,} | {x['total_acked_deletes']:,} | {x['lost_acked_adds']} | "
                  f"{x['resurrected_deletes']} | {x['recovery_ms_p50']:.1f} / {x['recovery_ms_max']:.1f} ms |")
        print()


if __name__ == "__main__":
    main()
