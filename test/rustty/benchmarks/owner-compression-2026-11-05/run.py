#!/usr/bin/env python3
"""Run three adjacent owner-probe pairs into a fresh result directory."""
from pathlib import Path
import hashlib
import json
import subprocess
import sys
import tempfile
import time

if len(sys.argv) not in (6, 7):
    raise SystemExit("run.py BEFORE AFTER detached|remote|detached-multi ROWS ascii|unicode [OUTPUT_ROOT]")
binaries = dict(zip(("before", "after"), (str(Path(p).resolve()) for p in sys.argv[1:3])))
mode, rows, kind = sys.argv[3], int(sys.argv[4]), sys.argv[5]
root = Path(sys.argv[6]) if len(sys.argv) == 7 else Path(tempfile.gettempdir())
out = Path(tempfile.mkdtemp(prefix="rustty-owner-paired-", dir=root))
print(out, flush=True)
(out / "run-manifest.json").write_text(json.dumps({
    "mode": mode, "rows": rows, "kind": kind, "order": "AB,BA,AB",
    "binaries": {k: {"path": p, "sha256": hashlib.sha256(Path(p).read_bytes()).hexdigest()}
                 for k, p in binaries.items()},
}, indent=2) + "\n")
for pair in range(3):
    variants = ["before", "after"] if pair % 2 == 0 else ["after", "before"]
    for position, variant in enumerate(variants):
        name = f"{mode}-{rows}-{kind}-{pair}-{position}-{variant}"
        args = ["/usr/bin/time", "-v", "-o", str(out / (name + ".time")),
                binaries[variant], str(out / name), mode, str(rows), kind]
        started = time.time_ns()
        completed = subprocess.run(args, capture_output=True, text=True)
        (out / (name + ".json")).write_text(completed.stdout)
        (out / (name + ".stderr")).write_text(completed.stderr)
        with (out / "execution.jsonl").open("a") as f:
            f.write(json.dumps({"name": name, "args": args, "pair": pair,
                "position": position, "variant": variant, "mode": mode,
                "rows": rows, "kind": kind, "start_unix_ns": started,
                "end_unix_ns": time.time_ns(), "returncode": completed.returncode}) + "\n")
        if completed.returncode:
            raise RuntimeError(f"{name}: {completed.stderr}")
    print(mode, rows, kind, "pair", pair, "complete", flush=True)
