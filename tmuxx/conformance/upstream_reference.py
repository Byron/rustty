#!/usr/bin/env python3
"""Approved unchanged upstream scripts and real C socket-selection observations.

Runs only inside remote_c_reference's leased Linux subreaper. Its caller pins
this file, every shell source, and the independently built C executable.
"""
from __future__ import annotations

import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


SCRIPTS = (
    "input-cursor.sh", "input-edit.sh", "input-lines-region.sh", "input-modes.sh",
    "input-malformed.sh", "input-scroll.sh", "input-sgr.sh", "input-unicode.sh",
    "input-osc.sh", "format-fuzzy.sh", "format-modifiers.sh",
    "format-no-current-window.sh", "format-time-range.sh", "json.sh", "buffers.sh",
    "set-hook-B.sh", "targets.sh", "targets-panes.sh", "targets-current.sh",
    "layout-nesting-limit.sh", "control-client-exit.sh",
    "control-client-exit-stalled.sh", "control-client-run-shell-exit.sh",
    "control-client-wait-exit.sh", "control-notify-guard.sh",
    "control-subscriptions.sh", "format-strings.sh", "input-raw-controls.sh",
    "input-nowrap-collect.sh",
)


def environment(directory: Path, binary: Path) -> dict[str, str]:
    env = os.environ.copy()
    for key in ("TMUX", "TMUXX_ENDPOINT", "TMUXX_NATIVE_ENDPOINT"):
        env.pop(key, None)
    env.update(TEST_TMUX=str(binary), TMUX_TMPDIR=str(directory), TMPDIR=str(directory),
               SHELL="/bin/sh", TZ="UTC", LC_ALL="C.UTF-8")
    return env


def socket_defaults(binary: Path, directory: Path) -> dict:
    """Use actual global flags/environment; never inject a common -S override."""
    env = environment(directory, binary)
    namespace = directory / ("tmux-" + str(os.getuid()))
    labels = ["default", "named Ω &^", "third"]
    explicit = directory / "literal path Ω &^, endpoint"

    def call(*args: str, inherited: str | None = None) -> bytes:
        current = env.copy()
        if inherited is not None:
            current["TMUX"] = inherited
        result = subprocess.run([str(binary), "-f", "/dev/null", *args], env=current,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=8)
        assert result.returncode == 0, (args, result.stderr)
        assert not result.stderr, (args, result.stderr)
        return result.stdout

    try:
        call("new-session", "-d", "-s", "default", "sleep 90")
        for label, session in [(labels[1], "named"), (labels[2], "third")]:
            call("-L", label, "new-session", "-d", "-s", session, "sleep 90")
        call("-S", str(explicit), "new-session", "-d", "-s", "explicit", "sleep 90")
        names = ["list-sessions", "-F", "#{session_name}"]
        inherited = str(namespace / labels[1]) + ",123,0"
        assert call(*names) == b"default\n"
        assert call(*names, inherited=inherited) == b"named\n"
        assert call("-L", "default", *names, inherited=inherited) == b"default\n"
        assert call("-L", labels[2], *names, inherited=inherited) == b"third\n"
        assert call("-S", str(explicit), "-L", labels[1], *names,
                    inherited=inherited) == b"explicit\n"
        assert call("-L", labels[1], "-S", str(explicit), *names,
                    inherited=inherited) == b"explicit\n"
        paths = [namespace / label for label in labels] + [explicit]
        assert all(path.is_socket() for path in paths)
        identities = []
        for path in paths:
            pid, pane = map(int, call("-S", str(path), "display", "-p", "#{pid} #{pane_pid}").split())
            identities.append({"endpoint": str(path), "owner_pid": pid, "pane_pid": pane})
        assert len({value["owner_pid"] for value in identities}) == 4
        assert len({value["pane_pid"] for value in identities}) == 4
        # Each owner remains queryable after every selector probe. The explicit
        # endpoint contains a comma; -S passes its bytes literally rather than
        # interpreting it as the comma-delimited inherited TMUX variable.
        for path, value in zip(paths, identities):
            assert call("-S", str(path), "display", "-p", "#{pane_pid}") == str(value["pane_pid"]).encode() + b"\n"
        return {"case": "socket_defaults", "identities": identities,
                "default_label_environment_and_explicit_precedence": True}
    finally:
        for prefix in [["-L", label] for label in labels] + [["-S", str(explicit)]]:
            subprocess.run([str(binary), *prefix, "kill-server"], env=env,
                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, timeout=8)


def run_script(source: Path, env: dict[str, str], output, timeout=110) -> int:
    """Reap exited adopted processes while preserving the shell's own status."""
    child = subprocess.Popen(["/bin/sh", str(source)], cwd=source.parent,
                             env=env, stdin=subprocess.DEVNULL,
                             stdout=output, stderr=subprocess.STDOUT)
    deadline = time.monotonic() + timeout
    try:
        while child.poll() is None:
            # The observer is a Linux subreaper. Detached servers become its
            # children, and an unreaped exited server still satisfies kill -0
            # in the original upstream lifecycle scripts. Only reap exited
            # children: live descendants remain for the script to exercise.
            # Popen exclusively owns the shell's wait status, even if it exits
            # between poll() and this enumeration.
            children = Path(f"/proc/self/task/{os.getpid()}/children").read_text().split()
            for pid in map(int, children):
                if pid != child.pid:
                    try:
                        os.waitpid(pid, os.WNOHANG)
                    except ChildProcessError:
                        pass
            if time.monotonic() >= deadline:
                raise subprocess.TimeoutExpired(child.args, timeout)
            time.sleep(.02)
        return child.returncode
    finally:
        if child.poll() is None:
            child.kill()
        child.wait(timeout=5)


def upstream(binary: Path, directory: Path, name: str) -> dict:
    assert name in SCRIPTS, "unapproved upstream script"
    root = Path(os.environ.get("TMUXX_C_SOURCE", Path(__file__).resolve().parents[2]))
    source = root / "regress" / name
    before = hashlib.sha256(source.read_bytes()).hexdigest()
    transcript = directory / "transcript"
    with transcript.open("wb") as output:
        returncode = run_script(source, environment(directory, binary), output)
    assert hashlib.sha256(source.read_bytes()).hexdigest() == before
    data = transcript.read_bytes()
    assert len(data) <= 4 * 1024 * 1024, "upstream transcript exceeds bound"
    return {"case": name, "returncode": returncode,
            "source_sha256": before, "transcript": base64.b64encode(data).decode("ascii")}


def main() -> None:
    binary, payload, result = map(Path, sys.argv[1:])
    case = json.loads(payload.read_text(encoding="utf-8"))["case"]
    with tempfile.TemporaryDirectory(prefix="upstream-", dir=payload.parent) as temporary:
        directory = Path(temporary)
        observed = socket_defaults(binary, directory) if case == "socket_defaults" else upstream(binary, directory, case)
        result.write_text(json.dumps(observed, sort_keys=True), encoding="utf-8")


if __name__ == "__main__":
    main()
