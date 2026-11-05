#!/usr/bin/env python3
"""Run adjacent baseline/candidate navigation pairs; build both binaries first."""
import json
from pathlib import Path
import subprocess
import sys
import time

before, after, rows, *kinds = sys.argv[1:]
output = Path(f"navigation-results-{rows}-{time.time_ns()}")
output.mkdir()
print(output, flush=True)
for kind in kinds or ["ascii", "unicode"]:
    for case in ["viewport", "selection_text_tail", "search_first_feed"]:
        for pair in range(3):
            order = [("baseline", before), ("metadata", after)]
            if pair % 2:
                order.reverse()
            for position, (variant, binary) in enumerate(order):
                name = f"metadata-{rows}-{kind}-{case}-{pair}-{position}-{variant}"
                if (output / (name + ".jsonl")).exists():
                    raise SystemExit(f"sample already exists: {name}; use a fresh working directory")
                args = [binary, rows, kind, case]
                start = time.time_ns()
                result = subprocess.run(args, capture_output=True, text=True, check=True)
                (output / (name + ".jsonl")).write_text(result.stdout)
                (output / (name + ".stderr")).write_text(result.stderr)
                with (output / "execution.jsonl").open("a") as log:
                    log.write(json.dumps({"name": name, "args": args, "start_unix_ns": start,
                                          "end_unix_ns": time.time_ns(), "pair": pair,
                                          "position": position, "variant": variant}) + "\n")
