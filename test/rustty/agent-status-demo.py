#!/usr/bin/env python3
"""Disposable, serialized agent-status reporter; no settings or workspace writes."""

import argparse
import base64
import json
import signal
import subprocess
import time


STEPS = [
    ("begin", "idle", "A", None, "Review Δ 🦀"),
    ("update", "working", "A", None, "Review Δ 🦀"),
    ("update", "needs_input", "A", None, "Review Δ 🦀"),
    ("update", "working", "A", None, "Input resolved"),
    ("update", "idle", "A", None, "Turn cancelled"),
    ("update", "working", "A", None, "Review Δ 🦀"),
    ("update", "done", "A", "T1", "Review Δ 🦀"),
    ("update", "done", "A", "T1", "Duplicate T1"),
    ("update", "idle", "B", None, "Other thread"),
    ("update", "done", "A", "T1", "Back to A/T1"),
    ("update", "working", "A", None, "New turn"),
    ("update", "done", "A", "T2", "New A/T2"),
    ("update", "error", "A", None, "Explicit error"),
    ("update", "paused", "A", None, "Explicit pause"),
    ("update", "unknown", "A", None, "Connection lost"),
    ("update", "idle", "A", None, "Reattached"),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rustty", default="target/debug/rustty")
    parser.add_argument("--delay", type=float, default=2.5)
    parser.add_argument("--self-test", action="store_true", help="validate captured fixtures; no terminal needed")
    args = parser.parse_args()
    if args.delay < 0:
        parser.error("--delay must be nonnegative")

    def emit(op, state=None, thread=None, turn=None, label=None):
        command = [args.rustty, "agent-status", op]
        expected = {"op": op}
        if state:
            command.append(state)
            expected["state"] = state
        for field, value in [("label", label), ("thread_id", thread), ("turn_id", turn)]:
            if value is not None:
                command.extend(["--" + field.replace("_", "-"), value])
                expected[field] = value
        if args.self_test:
            frame = subprocess.check_output(command + ["--raw"])
            prefix, suffix = b"\x1b]777;rustty-agent;1;", b"\x1b\\"
            assert frame.startswith(prefix) and frame.endswith(suffix), frame
            decoded = base64.b64decode(frame[len(prefix):-len(suffix)], validate=True)
            assert len(decoded) <= 1024
            assert json.loads(decoded) == expected
        else:
            subprocess.run(command, check=True)

    def interrupt(_signal, _frame):
        raise KeyboardInterrupt

    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, interrupt)
    try:
        for step in STEPS:
            emit(*step)
            if not args.self_test:
                time.sleep(args.delay)
    except KeyboardInterrupt:
        pass
    finally:
        emit("end")
    if args.self_test:
        print(f"Validated {len(STEPS) + 1} whole status frames, Unicode, switching, and cleanup.")


if __name__ == "__main__":
    main()
