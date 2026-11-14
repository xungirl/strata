"""Render the stage-8 tables of BENCHMARK.md from results/stage8 (stdlib only).

    python3 -I scripts/make_tables_stage8.py > results/stage8/tables.md
"""

import glob
import json
import os
import statistics

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
S = os.path.join(ROOT, "results", "stage8")


def pct(xs, q):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(q * len(xs)))]


def main():
    runs = [json.load(open(p)) for p in sorted(glob.glob(os.path.join(S, "chaos.[0-9].json")))]
    print("### Leader-kill chaos test (3-node shard, SIGKILL the leader, 25 kills per run, 3 runs)\n")
    print("| run | kills | acked writes (ids) | lost acked ids | unverified | read-after-ack (consistent) violations / checks | "
          "read-after-failover violations / checks | follower plain-read misses / checks | failover p50 / max ms | catch-up p50 / max ms | replicas missing acked ids |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for i, r in enumerate(runs, 1):
        print(f"| {i} | {r['kills']} | {r['acked_ids']} | {r['lost_acked_ids_leader_consistent_read']} | {r['unverified_checks']} | "
              f"{r['consistent_read_after_ack_violations']} / {r['consistent_read_after_ack_checks']} | "
              f"{r['read_after_failover_violations']} / {r['read_after_failover_checks']} | "
              f"{r['follower_plain_read_misses']} / {r['follower_plain_read_checks']} | "
              f"{r['failover_ms_p50']:.0f} / {r['failover_ms_max']:.0f} | {r['catchup_ms_p50']:.0f} / {r['catchup_ms_max']:.0f} | "
              f"{r['replicas_missing_acked_ids']} |")
    tot = lambda k: sum(r[k] for r in runs)
    print(f"| **total** | {tot('kills')} | {tot('acked_ids')} | {tot('lost_acked_ids_leader_consistent_read')} | {tot('unverified_checks')} | "
          f"{tot('consistent_read_after_ack_violations')} / {tot('consistent_read_after_ack_checks')} | "
          f"{tot('read_after_failover_violations')} / {tot('read_after_failover_checks')} | "
          f"{tot('follower_plain_read_misses')} / {tot('follower_plain_read_checks')} | | | |")
    print()
    print("### Failover time distribution (SIGKILL of the leader -> next write acknowledged by another replica)\n")
    print("| quantity | n | min | p50 | p90 | max | mean ± std | per-run p50 (mean ± std of 3) |")
    print("|---|---|---|---|---|---|---|---|")
    for key, label in (("failover_ms", "failover ms"), ("catchup_ms", "restarted node catch-up ms")):
        xs = [x for r in runs for x in r[key]]
        p50s = [statistics.median(r[key]) for r in runs]
        print(f"| {label} | {len(xs)} | {min(xs):.0f} | {pct(xs, 0.5):.0f} | {pct(xs, 0.9):.0f} | {max(xs):.0f} | "
              f"{statistics.mean(xs):.0f} ± {statistics.stdev(xs):.0f} | {statistics.mean(p50s):.0f} ± {statistics.stdev(p50s):.0f} |")
    print()
    mut = os.path.join(S, "mutations.txt")
    if os.path.exists(mut):
        print("### Mutation tests (inject a known Raft bug, expect the simulation tests to fail)\n")
        print("| injected bug | result |")
        print("|---|---|")
        for line in open(mut):
            name, _, rest = line.strip().partition(": ")
            res = "detected" if rest.startswith("detected") else rest
            where = rest.split("panicked at ")[-1].rstrip(":") if "panicked at" in rest else ""
            print(f"| {name} | {res}{' (' + where + ')' if where else ''} |")
        print()


if __name__ == "__main__":
    main()
