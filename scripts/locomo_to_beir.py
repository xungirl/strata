"""Convert LoCoMo (long-term conversational memory benchmark) into BEIR
format, so strata's evaluation tools can score memory retrieval.

    python3 -I scripts/locomo_to_beir.py   # reads data/locomo/locomo10.json

LoCoMo (Maharana et al., 2024; https://github.com/snap-research/locomo,
CC BY-NC 4.0, used locally and not redistributed) has 10 long multi-session
conversations between two people and ~2,000 questions about them. Every
question lists the dialogue turns ("evidence") that contain the answer.

Retrieval task here: an agent remembers every turn (one memory per turn,
tagged with the conversation) and, for each question, must recall the
evidence turns of *that* conversation.

Output data/locomo-beir/:
  corpus.jsonl   {"_id": "c<conv>|<dia_id>", "title": "", "text": "<date> <speaker>: <text>", "conv": "<conv>"}
  queries.jsonl  {"_id": "c<conv>|q<n>", "text": question, "conv", "category"}
  qrels/test.tsv evidence turns, relevance 1
Questions of category 5 (adversarial / unanswerable) and questions without
resolvable evidence are left out.
"""

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
src = json.load(open(ROOT / "data" / "locomo" / "locomo10.json"))
out = ROOT / "data" / "locomo-beir"
(out / "qrels").mkdir(parents=True, exist_ok=True)

docs, queries, qrels = [], [], []
skipped = 0
for ci, sample in enumerate(src):
    conv = sample["conversation"]
    turn_ids = set()
    for key, turns in conv.items():
        m = re.fullmatch(r"session_(\d+)", key)
        if not m:
            continue
        date = conv.get(f"{key}_date_time", "")
        for t in turns:
            did = f"c{ci}|{t['dia_id']}"
            turn_ids.add(t["dia_id"])
            docs.append({"_id": did, "title": "", "text": f"{date} {t['speaker']}: {t['text']}", "conv": str(ci)})
    for qi, qa in enumerate(sample["qa"]):
        if qa.get("category") == 5:
            continue
        # Some evidence strings list several ids ("D1:3; D1:5") or have typos.
        ev = []
        for e in qa.get("evidence", []):
            ev.extend(x.strip() for x in re.split(r"[;,]", e) if x.strip())
        ev = [e for e in ev if e in turn_ids]
        if not ev:
            skipped += 1
            continue
        qid = f"c{ci}|q{qi}"
        queries.append({"_id": qid, "text": qa["question"], "conv": str(ci), "category": qa["category"]})
        for e in sorted(set(ev)):
            qrels.append((qid, f"c{ci}|{e}"))

with open(out / "corpus.jsonl", "w") as f:
    for d in docs:
        f.write(json.dumps(d) + "\n")
with open(out / "queries.jsonl", "w") as f:
    for q in queries:
        f.write(json.dumps(q) + "\n")
with open(out / "qrels" / "test.tsv", "w") as f:
    f.write("query-id\tcorpus-id\tscore\n")
    for q, d in qrels:
        f.write(f"{q}\t{d}\t1\n")
print(f"{len(docs)} memories, {len(queries)} questions ({skipped} skipped: no resolvable evidence), {len(qrels)} evidence links")
