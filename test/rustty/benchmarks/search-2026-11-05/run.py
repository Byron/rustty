#!/usr/bin/env python3
"""Measure full history search in adjacent pairs, preserving previous runs."""
from pathlib import Path
import hashlib
import json
import subprocess
import sys
import tempfile
import time

if len(sys.argv) not in (4, 5):
    raise SystemExit("run.py BEFORE AFTER ROWS [ascii|unicode]")
binaries = dict(zip(("metadata", "search"), (str(Path(p).resolve()) for p in sys.argv[1:3])))
rows = int(sys.argv[3])
kinds = sys.argv[4:] or ["ascii", "unicode"]
out = Path(tempfile.mkdtemp(prefix="rustty-full-search-"))
print(out, flush=True)
(out / "run-manifest.json").write_text(json.dumps({
    "rows": rows, "kinds": kinds, "order": "AB,BA,AB", "case": "search_run_rare",
    "binaries": {k: {"path": p, "sha256": hashlib.sha256(Path(p).read_bytes()).hexdigest()}
                 for k, p in binaries.items()},
}, indent=2) + "\n")
for kind in kinds:
    for pair in range(3):
        variants = ["metadata", "search"] if pair % 2 == 0 else ["search", "metadata"]
        for position, variant in enumerate(variants):
            name = f"search-{rows}-{kind}-search_run_rare-{pair}-{position}-{variant}"
            args = [binaries[variant], str(rows), kind, "search_run_rare"]
            start = time.time_ns()
            result = subprocess.run(args, capture_output=True, text=True)
            (out / (name + ".jsonl")).write_text(result.stdout)
            (out / (name + ".stderr")).write_text(result.stderr)
            with (out / "execution.jsonl").open("a") as f:
                f.write(json.dumps({"name": name, "args": args, "start_unix_ns": start,
                    "end_unix_ns": time.time_ns(), "pair": pair, "position": position,
                    "variant": variant, "returncode": result.returncode}) + "\n")
            if result.returncode:
                raise RuntimeError(f"{name}: {result.stderr}")
    print(rows, kind, "three pairs complete", flush=True)
