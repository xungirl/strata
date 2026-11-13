"""Compute dense embeddings for BEIR corpora and queries, once, locally.

    uv run --with sentence-transformers==3.3.1 --with torch python -I scripts/embed_beir.py scifact nfcorpus ...

Model: BAAI/bge-small-en-v1.5 (384 dimensions, ~33M parameters, MIT license).
* documents: "title text" (the same "flat" text the lexical index uses)
* queries: prefixed with BGE's retrieval instruction
  "Represent this sentence for searching relevant passages: "
  (as recommended on the model card for short-query -> passage retrieval)
* embeddings are L2-normalized, so dot product == cosine similarity
* max sequence length 512 tokens (model default); longer texts are truncated

Output, per dataset, under data/embeddings/<name>/ (gitignored):
  corpus.f32 / queries.f32   row-major little-endian float32, n x 384
  corpus_ids.json / query_ids.json
  meta.json                  model id + revision, versions, device, timings

Model files are cached under data/hf (HF_HOME) so nothing is written to the
user's global Hugging Face cache.
"""

import json
import os
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
os.environ.setdefault("HF_HOME", str(ROOT / "data" / "hf"))

import numpy as np  # noqa: E402
import torch  # noqa: E402
from sentence_transformers import SentenceTransformer  # noqa: E402
import sentence_transformers  # noqa: E402

MODEL = "BAAI/bge-small-en-v1.5"
QUERY_PREFIX = "Represent this sentence for searching relevant passages: "


def read_jsonl(path):
    with open(path) as f:
        return [json.loads(line) for line in f if line.strip()]


def main():
    names = sys.argv[1:] or ["scifact", "nfcorpus", "fiqa", "scidocs", "trec-covid"]
    device = "mps" if torch.backends.mps.is_available() else "cpu"
    model = SentenceTransformer(MODEL, device=device)
    revision = None
    try:
        from huggingface_hub import model_info

        revision = model_info(MODEL).sha
    except Exception as e:  # offline: record that we could not resolve it
        revision = f"unresolved ({type(e).__name__})"
    for name in names:
        out = ROOT / "data" / "embeddings" / name
        if (out / "meta.json").exists():
            print(f"[skip] {name}")
            continue
        out.mkdir(parents=True, exist_ok=True)
        corpus = read_jsonl(ROOT / "data" / name / "corpus.jsonl")
        queries = read_jsonl(ROOT / "data" / name / "queries.jsonl")
        texts = [(d.get("title", "") + " " + d.get("text", "")).strip() for d in corpus]
        # Sort by length so batches have similar lengths (much faster), then undo.
        order = np.argsort([len(t) for t in texts])
        t0 = time.time()
        emb_sorted = model.encode([texts[i] for i in order], batch_size=64, normalize_embeddings=True,
                                  convert_to_numpy=True, show_progress_bar=False)
        doc_emb = np.empty_like(emb_sorted)
        doc_emb[order] = emb_sorted
        t_docs = time.time() - t0
        t0 = time.time()
        q_emb = model.encode([QUERY_PREFIX + q["text"] for q in queries], batch_size=64, normalize_embeddings=True,
                             convert_to_numpy=True, show_progress_bar=False)
        t_q = time.time() - t0
        doc_emb.astype("<f4").tofile(out / "corpus.f32")
        q_emb.astype("<f4").tofile(out / "queries.f32")
        json.dump([d["_id"] for d in corpus], open(out / "corpus_ids.json", "w"))
        json.dump([q["_id"] for q in queries], open(out / "query_ids.json", "w"))
        meta = {
            "model": MODEL,
            "model_revision": revision,
            "dim": int(doc_emb.shape[1]),
            "normalized": True,
            "query_prefix": QUERY_PREFIX,
            "max_seq_length": model.max_seq_length,
            "num_docs": len(corpus),
            "num_queries": len(queries),
            "device": device,
            "torch": torch.__version__,
            "sentence_transformers": sentence_transformers.__version__,
            "seconds_docs": round(t_docs, 2),
            "seconds_queries": round(t_q, 2),
            "date_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        }
        json.dump(meta, open(out / "meta.json", "w"), indent=2)
        print(f"[done] {name}: {len(corpus)} docs in {t_docs:.1f}s, {len(queries)} queries in {t_q:.1f}s on {device}")


if __name__ == "__main__":
    main()
