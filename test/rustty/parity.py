#!/usr/bin/env python3
"""Compatibility launcher. Suite execution and comparison run entirely in Rust."""
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
target = ROOT / os.environ.get("CARGO_TARGET_DIR", "target")
binary = target / "release/examples/parity-runner"
if "--no-build" not in sys.argv[1:]:
    subprocess.run(["cargo", "+1.95.0", "build", "--release", "--offline", "-p", "rustty-vt",
                    "--example", "parity-runner"], cwd=ROOT, check=True)
if not binary.is_file():
    sys.exit("Build the runner first: cargo +1.95.0 build --release --offline -p rustty-vt --example parity-runner")
os.execv(binary, [str(binary), *sys.argv[1:]])
