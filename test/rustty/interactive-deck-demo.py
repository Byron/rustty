#!/usr/bin/env python3
"""Disposable, explicitly simulated agent reports for physical dashboard testing."""

import argparse
import base64
import json
import os
from pathlib import Path
import signal
import sys
import time

STATES = ("working", "needs_input", "done", "idle", "error", "paused", "unknown",
          "working", "needs_input", "done", "idle", "working")


def frame(operation, index=1):
    message = {"op": operation}
    if operation != "end":
        state = STATES[(index - 1) % len(STATES)]
        message.update(state=state, label=f"Demo {index:02d} Δ", thread_id=f"demo-{index}")
        if state == "done":
            message["turn_id"] = "turn-1"
    encoded = json.dumps(message, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    assert len(encoded) <= 1024
    payload = "777;rustty-agent;1;" + base64.b64encode(encoded).decode("ascii")
    assert len(payload) < 2048
    return "\x1b]" + payload + "\x1b\\"


def claim_index(directory):
    # One shared test directory deliberately proves cwd is not pane identity.
    lock = directory / ".deck-counter-lock"
    for _ in range(200):
        try:
            lock.mkdir()
            break
        except FileExistsError:
            time.sleep(0.01)
    else:
        raise TimeoutError("fixture counter lock remained busy for two seconds")
    try:
        counter = directory / ".deck-counter"
        index = int(counter.read_text()) + 1 if counter.exists() else 1
        counter.write_text(str(index))
        return index
    finally:
        lock.rmdir()


def emit(operation, index):
    sys.stdout.write(frame(operation, index))
    sys.stdout.flush()


def self_test():
    import concurrent.futures
    import tempfile
    for index, state in enumerate(STATES, 1):
        encoded = frame("begin", index).removeprefix("\x1b]777;rustty-agent;1;").removesuffix("\x1b\\")
        message = json.loads(base64.b64decode(encoded, validate=True))
        assert message["state"] == state
        assert message["label"] == f"Demo {index:02d} Δ"
        assert ("turn_id" in message) == (state == "done")
    assert json.loads(base64.b64decode(frame("end").split(";")[-1][:-2])) == {"op": "end"}
    with tempfile.TemporaryDirectory(prefix="rustty-deck-self-test-") as directory:
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            indexes = list(pool.map(claim_index, [Path(directory)] * 12))
        assert sorted(indexes) == list(range(1, 13))
    print("interactive deck demo: protocol and concurrent pane numbering passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if args.directory is None or not sys.stdout.isatty():
        parser.error("requires --directory and its own terminal stdout")
    directory = args.directory.resolve(strict=True)
    index = claim_index(directory)
    state = STATES[(index - 1) % len(STATES)]
    marker = directory / ".deck-ended"
    print(f"\x1b[2J\x1b[H\x1b[1;36mSIMULATED agent {index:02d} — {state}\x1b[0m")
    print("No Codex process or conversation is connected to this fixture.")
    print(f"Same directory in every pane: {os.getcwd()}")
    print("Tap a deck tile to focus this exact pane. Hold two seconds, then tap to swap.")
    print("Page and Light work during moves; state cycle keys are disabled.")
    print(f"Create {marker} to end every report; remove it to restore the board.")
    print("Ctrl-C ends this pane's report. Close the test windows to finish.")
    registered = False
    def stop(_signal, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, stop)
    try:
        while True:
            should_report = not marker.exists()
            if should_report != registered:
                emit("begin" if should_report else "end", index)
                registered = should_report
                print("Reporting simulated status." if registered else "Report ended; position reserved.", flush=True)
            time.sleep(0.25)
    except KeyboardInterrupt:
        pass
    finally:
        if registered:
            emit("end", index)


if __name__ == "__main__":
    main()
