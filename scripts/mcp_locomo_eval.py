"""Agent-memory evaluation of strata over the Model Context Protocol.

    python3 -I scripts/mcp_locomo_eval.py --out results/stage9
    (needs `scripts/locomo_to_beir.py` first and a release build of strata-mcp)

A deliberately independent MCP client (stdlib only, newline-delimited
JSON-RPC 2.0 over the server's stdin/stdout, as the MCP stdio transport
specifies): it does the protocol handshake (initialize -> notifications/
initialized -> tools/list), stores every LoCoMo dialogue turn as a memory
with `remember_many` (tagged with its conversation), then asks every
question with `recall` restricted to that conversation, exactly like an
agent would. Writes:

  <out>/runs/locomo-beir/mcp-recall-bm25.trec   ranked memories per question (top 100)
  <out>/mcp_transcript.jsonl                    the first protocol messages, verbatim
  <out>/mcp_eval.json                           timings, counts, tool list
"""

import argparse
import json
import subprocess
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


class McpClient:
    def __init__(self, cmd, transcript, keep=40):
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1)
        self.next_id = 1
        self.transcript = transcript
        self.keep = keep

    def _log(self, direction, msg):
        if self.keep > 0:
            self.transcript.write(json.dumps({"dir": direction, "msg": msg}) + "\n")
            self.keep -= 1

    def send(self, msg):
        self._log("client->server", msg)
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def request(self, method, params=None):
        rid = self.next_id
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}})
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("server closed stdout")
            msg = json.loads(line)
            self._log("server->client", msg)
            if msg.get("id") == rid:
                if "error" in msg:
                    raise RuntimeError(f"{method}: {msg['error']}")
                return msg["result"]

    def call(self, tool, args):
        res = self.request("tools/call", {"name": tool, "arguments": args})
        if res.get("isError"):
            raise RuntimeError(f"{tool} failed: {res}")
        # Tools return structured JSON (also mirrored as text content).
        if "structuredContent" in res:
            return res["structuredContent"]
        return json.loads(res["content"][0]["text"])

    def close(self):
        self.p.stdin.close()
        self.p.wait(timeout=10)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="results/stage9")
    ap.add_argument("--data", default="data/locomo-beir")
    ap.add_argument("--k", type=int, default=100)
    args = ap.parse_args()
    out = ROOT / args.out
    (out / "runs" / "locomo-beir").mkdir(parents=True, exist_ok=True)
    data = ROOT / args.data
    mem_dir = ROOT / "data" / "stage9-memory"
    subprocess.run(["rm", "-rf", str(mem_dir)], check=True)

    docs = [json.loads(l) for l in open(data / "corpus.jsonl")]
    queries = [json.loads(l) for l in open(data / "queries.jsonl")]
    transcript = open(out / "mcp_transcript.jsonl", "w")
    c = McpClient([str(ROOT / "target" / "release" / "strata-mcp"), "--dir", str(mem_dir)], transcript)

    init = c.request("initialize", {
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "strata-locomo-eval", "version": "0.1"},
    })
    c.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    tools = c.request("tools/list")["tools"]

    t0 = time.time()
    batch = 500
    for i in range(0, len(docs), batch):
        c.call("remember_many", {"memories": [
            {"id": d["_id"], "text": d["text"], "tags": {"conv": "c" + d["conv"]}} for d in docs[i:i + batch]
        ]})
    ingest_s = time.time() - t0

    lat = []
    with open(out / "runs" / "locomo-beir" / "mcp-recall-bm25.trec", "w") as run:
        for q in queries:
            t = time.time()
            res = c.call("recall", {"query": q["text"], "k": args.k, "tag_key": "conv", "tag_value": "c" + q["conv"]})
            lat.append((time.time() - t) * 1e3)
            for rank, m in enumerate(res["memories"], 1):
                run.write(f"{q['_id']} Q0 {m['id']} {rank} {m['score']!r} mcp\n")
    stats = c.call("memory_stats", {})
    c.close()
    lat.sort()
    report = {
        "server": init.get("serverInfo"),
        "protocol_version": init.get("protocolVersion"),
        "tools": [t["name"] for t in tools],
        "memories": len(docs),
        "questions": len(queries),
        "memory_stats": stats,
        "ingest_seconds": round(ingest_s, 2),
        "recall_latency_ms_incl_jsonrpc": {"p50": lat[len(lat) // 2], "p99": lat[int(len(lat) * 0.99)], "max": lat[-1]},
    }
    json.dump(report, open(out / "mcp_eval.json", "w"), indent=2)
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
