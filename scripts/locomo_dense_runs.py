"""Exact dense (bge-small) and unfiltered/filtered runs for LoCoMo memory
retrieval, as comparison points for the MCP (BM25) recall run.

    uv run -q --with numpy python -I scripts/locomo_dense_runs.py

Reads data/embeddings/locomo-beir (from scripts/embed_beir.py) and writes
TREC runs (top 1000) to results/stage9/runs/locomo-beir/:
  dense-exact-conv.trec   cosine similarity, only memories of the question's conversation
  dense-exact-all.trec    cosine similarity over all memories (no tag filter)
Exact brute force (numpy), no ANN: this measures the embedding model, not HNSW.
"""

import json
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
E = ROOT / "data" / "embeddings" / "locomo-beir"
OUT = ROOT / "results" / "stage9" / "runs" / "locomo-beir"
OUT.mkdir(parents=True, exist_ok=True)

doc_ids = json.load(open(E / "corpus_ids.json"))
q_ids = json.load(open(E / "query_ids.json"))
D = np.fromfile(E / "corpus.f32", dtype="<f4").reshape(len(doc_ids), -1)
Q = np.fromfile(E / "queries.f32", dtype="<f4").reshape(len(q_ids), -1)
conv_of_doc = np.array([d.split("|")[0] for d in doc_ids])
S = Q @ D.T  # 1532 x 5882 cosine similarities (vectors are L2-normalised)

for name, filtered in (("dense-exact-conv", True), ("dense-exact-all", False)):
    with open(OUT / f"{name}.trec", "w") as f:
        for qi, qid in enumerate(q_ids):
            s = S[qi].astype(np.float64)
            if filtered:
                s = np.where(conv_of_doc == qid.split("|")[0], s, -np.inf)
            order = np.argsort(-s, kind="stable")[:1000]
            rank = 0
            for di in order:
                if not np.isfinite(s[di]):
                    break
                rank += 1
                f.write(f"{qid} Q0 {doc_ids[di]} {rank} {float(np.float32(s[di]))!r} {name}\n")
    print("wrote", name)
