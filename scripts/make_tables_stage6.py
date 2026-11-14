"""Render the stage-6 tables of BENCHMARK.md from results/stage6 (stdlib only).

    python3 -I scripts/make_tables_stage6.py > results/stage6/tables.md
"""

import glob
import json
import os
import statistics

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
S = os.path.join(ROOT, "results", "stage6", "load")


def load(p):
    return json.load(open(p))


def ms(xs, fmt="{:.2f}"):
    s = statistics.stdev(xs) if len(xs) > 1 else 0.0
    return (fmt + " ± " + fmt).format(statistics.mean(xs), s)


def main():
    ing = [load(p) for p in sorted(glob.glob(os.path.join(S, "ingest.*.json")))]
    if ing:
        print("### Bulk ingest over HTTP (fiqa, 57,638 docs with 384-d vectors; batches of 500, 4 concurrent clients; 3 fresh nodes)\n")
        print("| metric | mean ± std |")
        print("|---|---|")
        print(f"| docs/s (incl. final commit) | {ms([r['docs_per_sec'] for r in ing], '{:.0f}')} |")
        print(f"| total seconds | {ms([r['with_commit_secs'] for r in ing])} |")
        print(f"| batch latency p50 ms | {ms([r['batch_latency']['p50_ms'] for r in ing], '{:.0f}')} |")
        print(f"| batch latency p99 ms | {ms([r['batch_latency']['p99_ms'] for r in ing], '{:.0f}')} |")
        print()
    print("### Closed-loop search (fiqa queries, k=10, 15 s measured after 2 s warm-up; mean ± std of 3 runs)\n")
    print("| mode | clients | throughput req/s | p50 ms | p99 ms | p99.9 ms | errors |")
    print("|---|---|---|---|---|---|---|")
    for mode in ("lexical", "hybrid"):
        for c in (1, 4, 16, 64):
            rs = [load(p)["summary"] for p in sorted(glob.glob(os.path.join(S, f"closed.{mode}.c{c}.*.json")))]
            if rs:
                print(f"| {mode} | {c} | {ms([r['throughput_rps'] for r in rs], '{:.0f}')} | {ms([r['p50_ms'] for r in rs])} | "
                      f"{ms([r['p99_ms'] for r in rs])} | {ms([r['p999_ms'] for r in rs])} | {sum(r['errors'] for r in rs)} |")
    print()
    print("### Open-loop search (lexical; latency measured from the scheduled send time)\n")
    print("| target rate req/s | achieved req/s | p50 ms | p99 ms | p99.9 ms | max ms | generator send lag p99 ms | errors |")
    print("|---|---|---|---|---|---|---|---|")
    for rate in (500, 1000, 2000, 4000):
        full = [load(p) for p in sorted(glob.glob(os.path.join(S, f"open.lexical.r{rate}.*.json")))]
        rs = [f["summary"] for f in full]
        lags = [f["send_lag"]["p99_ms"] for f in full if f.get("send_lag")]
        if rs:
            print(f"| {rate} | {ms([r['throughput_rps'] for r in rs], '{:.0f}')} | {ms([r['p50_ms'] for r in rs])} | {ms([r['p99_ms'] for r in rs])} | "
                  f"{ms([r['p999_ms'] for r in rs])} | {ms([r['max_ms'] for r in rs], '{:.1f}')} | {ms(lags, '{:.3f}') if lags else '-'} | {sum(r['errors'] for r in rs)} |")
    print()


if __name__ == "__main__":
    main()
