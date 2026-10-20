#!/usr/bin/env python3
"""Build the actual Rustty desktop and verify a fresh hidden native Metal frame."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import struct
import subprocess
import tempfile


def run():
    parser = argparse.ArgumentParser()
    parser.add_argument("--rustty", type=Path, required=True)
    args = parser.parse_args()
    built = subprocess.run(["cargo", "+1.95.0", "build", "--locked", "--offline", "-p", "rustty-app", "--bin", "rustty",
                            "--message-format=json-render-diagnostics"], cwd=args.rustty, capture_output=True, text=True, timeout=600)
    if built.returncode:
        raise RuntimeError(built.stdout + built.stderr)
    artifacts = [json.loads(line) for line in built.stdout.splitlines() if line.startswith("{")]
    executable = next(Path(item["executable"]) for item in artifacts if item.get("executable") and item.get("target", {}).get("name") == "rustty")
    with tempfile.TemporaryDirectory(prefix="rustty-native-frame-") as temporary:
        root = Path(temporary)
        env = dict(os.environ, RUSTTY_TMUXX_SMOKE_DIR=str(root), RUSTTY_TMUXX_SMOKE_FLOATING="1", TMUXX_ENDPOINT=str(root / "native"))
        result = subprocess.run([str(executable), "--tmuxx=embedded", "--config-default-files=false", "--stream-deck=false"],
                                cwd=args.rustty, env=env, capture_output=True, timeout=30)
        if result.returncode:
            raise RuntimeError(result.stderr.decode(errors="replace"))
        state = json.loads((root / "result.json").read_text())
        assert state["panes"] == 2 and state["windows"] == 1 and state["floating_panes"] == 1, state
        assert state["hidden"] and not state["focused"] and not state["errors"], state
        assert len(state["draw_order"]) == 2 and len(set(state["draw_order"])) == 2 and state["instance"], state
        png = (root / "window.png").read_bytes()
        assert png[:8] == b"\x89PNG\r\n\x1a\n" and len(png) > 1000
        width, height = struct.unpack(">II", png[16:24])
        assert width > 100 and height > 100, (width, height)
        print(json.dumps({"state": state, "frame_size": [width, height], "frame_sha256": hashlib.sha256(png).hexdigest(),
                          "executable_sha256": hashlib.sha256(executable.read_bytes()).hexdigest()}))
        print("test desktop::hidden_native_frame ... ok")


if __name__ == "__main__":
    run()
