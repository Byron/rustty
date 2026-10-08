#!/usr/bin/env python3
"""macOS agent-sim checks on disposable PTYs; no device, settings or agents."""
import argparse
import base64
import errno
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import subprocess
import time


FRAME = re.compile(rb"\x1b\]777;rustty-agent;1;([A-Za-z0-9+/=]+)\x1b\\")


class Simulator:
    def __init__(self, binary, *args):
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.execv(binary, [binary, "agent-sim", *args])
        self.output = bytearray()
        self.reaped = False

    def packets(self):
        return [json.loads(base64.b64decode(m[1], validate=True)) for m in FRAME.finditer(self.output)]

    def read(self):
        if select.select([self.fd], [], [], 0.02)[0]:
            try:
                self.output.extend(os.read(self.fd, 65536))
            except OSError as error:
                if error.errno != errno.EIO:
                    raise

    def until(self, predicate, timeout=3):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.read()
            if predicate():
                return
        raise AssertionError(f"simulator timeout: {self.output!r}")

    def exited(self):
        pid, status = os.waitpid(self.pid, os.WNOHANG)
        if pid:
            self.reaped = True
            assert os.waitstatus_to_exitcode(status) == 0, status
        return bool(pid)

    def close(self):
        if not self.reaped:
            os.kill(self.pid, signal.SIGCONT)
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        os.close(self.fd)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rustty", default="target/debug/rustty")
    args = parser.parse_args()
    binary = str(Path(args.rustty).resolve())

    captured = subprocess.run([binary, "agent-sim"], capture_output=True, timeout=3)
    assert captured.returncode != 0 and not captured.stdout
    assert b"not a terminal" in captured.stderr

    fixed = Simulator(binary, "--state", "working", "--interval", "60")
    try:
        fixed.until(lambda: len(fixed.packets()) == 1)
        assert fixed.packets()[0]["state"] == "working"
        started = time.monotonic()
        os.write(fixed.fd, b"\x03")  # Real terminal Ctrl-C, not a signal-handler unit mock.
        fixed.until(fixed.exited, timeout=2)
        fixed.read()
        assert time.monotonic() - started < 2
        assert [p["op"] for p in fixed.packets()] == ["begin", "end"]
    finally:
        fixed.close()

    suspended = Simulator(binary, "--state", "done", "--interval", "0.05")
    try:
        suspended.until(lambda: len(suspended.packets()) >= 3)
        before = suspended.packets()
        assert all(p["turn_id"] == before[0]["turn_id"] for p in before)
        os.kill(suspended.pid, signal.SIGTSTP)
        suspended.until(lambda: suspended.packets()[-1] == {"op": "end"})

        def stopped():
            pid, status = os.waitpid(suspended.pid, os.WNOHANG | os.WUNTRACED)
            assert not pid or os.WIFSTOPPED(status), status
            return bool(pid)

        suspended.until(stopped)
        os.kill(suspended.pid, signal.SIGCONT)
        suspended.until(lambda: sum(p["op"] == "begin" for p in suspended.packets()) == 2)
        os.kill(suspended.pid, signal.SIGTERM)
        suspended.until(suspended.exited)
        suspended.read()
        assert [p["op"] for p in suspended.packets() if p["op"] != "update"] == [
            "begin", "end", "begin", "end"
        ]
    finally:
        suspended.close()

    cycling = Simulator(binary, "--interval", "0.05")
    try:
        started = time.monotonic()
        cycling.until(lambda: len(cycling.packets()) >= 11)
        reports = cycling.packets()
        assert [p["state"] for p in reports[:7]] == [
            "idle", "working", "needs_input", "done", "error", "paused", "unknown"
        ]
        assert reports[3]["turn_id"] != reports[10]["turn_id"]
        assert time.monotonic() - started >= 0.45
        os.write(cycling.fd, b"\x03")
        cycling.until(cycling.exited)
        cycling.read()
        assert cycling.packets()[-1] == {"op": "end"}
    finally:
        cycling.close()
    print("Passed agent-sim stdout guard, fixed/cycling states, cadence, Ctrl-C, suspend/resume and SIGTERM cleanup.")


if __name__ == "__main__":
    main()
