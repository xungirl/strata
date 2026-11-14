"""Check that every Prometheus query in the Grafana dashboard uses metric
names and label keys that strata-server really exports (stdlib only).

    python3 -I deploy/check_dashboard.py

Reference exposition: results/stage6/metrics_after_load.txt (a real
`GET /metrics` scrape taken after the stage-6 load runs).
"""

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
dash = json.load(open(ROOT / "deploy" / "grafana" / "strata-dashboard.json"))
scrape = open(ROOT / "results" / "stage6" / "metrics_after_load.txt").read()

exported = {}  # metric name -> set of label keys
for line in scrape.splitlines():
    if not line or line.startswith("#"):
        continue
    m = re.match(r"([a-zA-Z_:][a-zA-Z0-9_:]*)(\{([^}]*)\})?", line)
    keys = set(re.findall(r'([a-zA-Z_][a-zA-Z0-9_]*)="', m.group(3) or ""))
    exported.setdefault(m.group(1), set()).update(keys)

problems = 0
for panel in dash["panels"]:
    for t in panel.get("targets", []):
        expr = t["expr"]
        for name, labels in re.findall(r"(strata_[a-z_]+)(\{[^}]*\})?", expr):
            if name not in exported:
                print(f"[{panel['title']}] unknown metric {name}")
                problems += 1
                continue
            for key in re.findall(r'([a-zA-Z_]+)\s*=~?\s*"', labels):
                if key not in exported[name]:
                    print(f"[{panel['title']}] {name} has no label {key}")
                    problems += 1
        for key in re.findall(r"by \(([^)]*)\)", expr):
            for k in [x.strip() for x in key.split(",")]:
                if k != "le" and not any(k in v for v in exported.values()):
                    print(f"[{panel['title']}] grouping by unknown label {k}")
                    problems += 1
print(f"{len(dash['panels'])} panels checked, {problems} problems")
sys.exit(1 if problems else 0)
