"""Explain differences between two TREC runs that have identical scores.

For every query whose ranked lists differ, check that the differences are
only tie-breaking: (a) the sequence of scores is identical rank by rank,
and (b) the two lists contain the same documents except inside the tied
group of scores at the cutoff (the last score in the list).

usage: tie_analysis.py RUN_A RUN_B
"""
import sys
from collections import defaultdict


def load(path):
    runs = defaultdict(list)
    with open(path) as f:
        for line in f:
            q, _, d, rank, score, _ = line.split()
            runs[q].append((int(rank), d, score))
    return {q: [(d, s) for _, d, s in sorted(v)] for q, v in runs.items()}


a, b = load(sys.argv[1]), load(sys.argv[2])
differ = only_ties = order_within_ties = cutoff_ties = 0
bad = []
for q in a:
    la, lb = a[q], b.get(q, [])
    if la == lb:
        continue
    differ += 1
    same_scores = [s for _, s in la] == [s for _, s in lb]
    groups_a, groups_b = defaultdict(set), defaultdict(set)
    for d, s in la:
        groups_a[s].add(d)
    for d, s in lb:
        groups_b[s].add(d)
    cutoff = la[-1][1]
    inner_same = all(groups_a[s] == groups_b.get(s) for s in groups_a if s != cutoff)
    if same_scores and inner_same:
        only_ties += 1
        if groups_a[cutoff] == groups_b[cutoff]:
            order_within_ties += 1
        else:
            cutoff_ties += 1
    else:
        bad.append(q)
print(f"queries: {len(a)}; lists differ: {differ}")
print(f"  differences explained by score ties only: {only_ties}")
print(f"    same docs, different order inside a tie group: {order_within_ties}")
print(f"    different docs chosen from the tie group at the cutoff: {cutoff_ties}")
print(f"  NOT explained by ties: {len(bad)} {bad[:10]}")
sys.exit(1 if bad else 0)
