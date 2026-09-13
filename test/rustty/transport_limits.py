#!/usr/bin/env python3
"""Check an oracle's JSON payload limit, including its newline boundary."""
import json
import subprocess
import sys

from parity import MAX_REQUEST


def check(binary):
    request = b'{"kind":"capabilities"}'
    for size in (MAX_REQUEST - 1, MAX_REQUEST, MAX_REQUEST + 1):
        result = subprocess.run(
            [binary], input=request + b" " * (size - len(request)) + b"\n",
            capture_output=True, timeout=60,
        )
        if size <= MAX_REQUEST:
            assert result.returncode == 0, (binary, size, result.stderr)
            assert json.loads(result.stdout)["ok"], (binary, size, result.stdout)
        else:
            assert result.returncode != 0, (binary, size)


if __name__ == "__main__":
    if len(sys.argv) < 2:
        raise SystemExit("usage: transport_limits.py ORACLE [ORACLE ...]")
    for binary in sys.argv[1:]:
        check(binary)
        print(f"{binary}: accepts {MAX_REQUEST} payload bytes and rejects one more")
