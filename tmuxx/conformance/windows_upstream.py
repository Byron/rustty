#!/usr/bin/env python3
"""Run upstream shell assertions with native Windows explicit-owner fixtures.

Git for Windows supplies the POSIX test shell and utility processes. All tested
tmux commands use the real MSVC tmuxx CLI, named pipes and ConPTY owners. The
adapter replaces only fixture startup/configuration and transport selection;
it never rewrites command output or retries a submitted command.
"""
from __future__ import annotations

import argparse
import base64
import ctypes
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
import uuid


def exchange(value):
    with socket.create_connection(("127.0.0.1", int(os.environ["TMUXX_UPSTREAM_PORT"])), timeout=15) as peer:
        peer.sendall(json.dumps(value | {"token": os.environ["TMUXX_UPSTREAM_TOKEN"]}).encode() + b"\n")
        with peer.makefile("rb") as stream:
            result = json.loads(stream.readline(65536))
        if "error" in result:
            raise RuntimeError(result["error"])
        return result


def bridge(arguments):
    # The shim itself joins this nested job before launching a production CLI.
    # Script termination/kill closes it, including its native control client.
    import ctypes
    from runner import WindowsSuiteJob
    job = WindowsSuiteJob()
    kernel = ctypes.WinDLL("kernel32")
    kernel.GetCurrentProcess.restype = ctypes.c_void_p
    class Current:
        _handle = kernel.GetCurrentProcess()
    job.assign(Current())
    label, options = "default", []
    index = 0
    while index < len(arguments):
        word = arguments[index]
        if word in ("-L", "-S", "-f"):
            assert index + 1 < len(arguments)
            if word in ("-L", "-S"):
                label = word + arguments[index + 1]
            else:
                assert arguments[index + 1] == "/dev/null", "unexpected upstream startup config"
            index += 2
        elif word.startswith("-L") and len(word) > 2:
            label = word
            index += 1
        elif word.startswith("-S") and len(word) > 2:
            label = word
            index += 1
        elif word.startswith("-f") and len(word) > 2:
            assert word[2:] == "/dev/null", "unexpected upstream startup config"
            index += 1
        elif word in ("-C", "-CC", "-N", "-u", "-2", "-8"):
            options.append(word)
            index += 1
        else:
            break
    command = arguments[index:]
    killing = command == ["kill-server"]
    selected = exchange({"operation": "select", "label": label, "start": not killing})
    env = os.environ.copy()
    for key in ("TMUX", "TMUXX_ENDPOINT"):
        env.pop(key, None)
    env["PATH"] = os.environ["TMUXX_UPSTREAM_NATIVE_PATH"]
    child = subprocess.Popen([os.environ["TMUXX_UPSTREAM_EXE"], "-L", selected["instance"], *options, *command],
                             stdin=sys.stdin.buffer, stdout=sys.stdout.buffer, stderr=sys.stderr.buffer,
                             env=env, creationflags=subprocess.CREATE_NO_WINDOW)
    code = child.wait()
    relay = os.environ.get("TMUXX_UPSTREAM_INPUT_RELAY")
    if relay:
        deadline = time.monotonic() + 2
        while not Path(relay).exists() and time.monotonic() < deadline:
            time.sleep(.005)
        if Path(relay).exists():
            pid = Path(relay).read_text(encoding="ascii").strip()
            assert pid.isdecimal(), "invalid fixture input relay pid"
            subprocess.run([os.environ["TMUXX_UPSTREAM_SHELL"], "-c", 'kill "$1" 2>/dev/null || :', "relay", pid],
                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, timeout=3)
    if killing:
        exchange({"operation": "reap", "label": label})
    sys.stdout.flush()
    sys.stderr.flush()
    os._exit(code)


class Owners:
    def __init__(self, candidate, shell, directory):
        self.candidate, self.shell, self.directory = candidate, shell, directory
        self.token, self.prefix = uuid.uuid4().hex, "upstream-" + uuid.uuid4().hex[:16]
        self.owners, self.logs, self.identities = {}, [], []
        self.lock = threading.Lock()
        self.closing = False
        from runner import WindowsSuiteJob
        self.job = WindowsSuiteJob()
        self.env = os.environ.copy()
        for key in ("TMUX", "TMUXX_ENDPOINT"):
            self.env.pop(key, None)
        self.env["SHELL"] = str(shell)
        self.env["PATH"] = str(shell.parent) + os.pathsep + self.env["PATH"]
        self.empty = directory / "empty.conf"
        self.empty.write_bytes(b"")

    def request(self, request):
        assert request.pop("token") == self.token
        label = request["label"]
        assert isinstance(label, str) and len(label) <= 256
        instance = self.prefix + "-" + hashlib.sha256(label.encode()).hexdigest()[:24]
        with self.lock:
            assert not self.closing, "upstream fixture is shutting down"
            owner = self.owners.get(label)
            if request["operation"] == "reap":
                if owner:
                    owner.wait(timeout=8)
                return {"instance": instance}
            assert request["operation"] == "select"
            if request["start"] and (owner is None or owner.poll() is not None):
                log = (self.directory / (instance + ".log")).open("ab")
                self.logs.append(log)
                owner = subprocess.Popen([str(self.candidate), "-D", "-L", instance, "-f", str(self.empty)],
                                         env=self.env, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                         creationflags=subprocess.CREATE_NO_WINDOW)
                try:
                    # Startup config only selects fixture options. The first
                    # pane command follows assignment and the readiness probe.
                    self.job.assign(owner)
                except BaseException:
                    owner.kill()
                    owner.wait(timeout=8)
                    raise
                self.owners[label] = owner
                self.identities.append({"label": label, "instance": instance, "pid": owner.pid})
                deadline = time.monotonic() + 8
                while True:
                    assert owner.poll() is None, "explicit upstream fixture owner exited during startup: " + Path(log.name).read_text(encoding="utf-8")
                    result = subprocess.run([str(self.candidate), "-L", instance, "list-sessions"],
                                            env=self.env, stdin=subprocess.DEVNULL, capture_output=True, timeout=2)
                    if result.returncode == 0:
                        break
                    assert time.monotonic() < deadline, "explicit upstream owner did not become ready"
                    time.sleep(.01)
            return {"instance": instance}

    def job_processes(self):
        from ctypes import wintypes as w
        maximum = 131072
        class Processes(ctypes.Structure):
            _fields_ = [("assigned", w.DWORD), ("count", w.DWORD),
                        ("pids", ctypes.c_size_t * maximum)]
        value = Processes()
        query = self.job.api.QueryInformationJobObject
        query.argtypes = [w.HANDLE, ctypes.c_int, w.LPVOID, w.DWORD, w.LPVOID]
        query.restype = w.BOOL
        if not query(self.job.handle, 3, ctypes.byref(value), ctypes.sizeof(value), None):
            raise ctypes.WinError(ctypes.get_last_error())
        assert value.count <= maximum, "fixture process inventory exceeded bound"
        return list(value.pids[:value.count])

    def cleanup(self):
        # A timed-out shell can leave a request already accepted by the fixture
        # TCP worker. Wait for that bounded startup before enumerating owners,
        # and forbid queued workers from starting an owner after this inventory.
        with self.lock:
            self.closing = True
            return self._cleanup()

    def _cleanup(self):
        before = self.job_processes()
        try:
            for owner in self.owners.values():
                if owner.poll() is None:
                    owner.kill()
                owner.wait(timeout=8)
            # This precedes fixture-job rescue: the product owner must dispose
            # of OpenConsole and every pane's foreground/background generation.
            deadline = time.monotonic() + 8
            while self.job_processes() and time.monotonic() < deadline:
                time.sleep(.02)
            remaining = self.job_processes()
            assert not remaining, ("owner cleanup left fixture descendants", remaining)
            return {"observed_owned_pids": before, "cleanup_remaining": remaining}
        finally:
            self.job.close()
            for log in self.logs:
                log.close()


def run(candidate: Path, shell: Path, script: Path, *, timeout=120, cleanup_sink=None):
    from runner import run_suite_process
    source_digest = hashlib.sha256(script.read_bytes()).hexdigest()
    with tempfile.TemporaryDirectory(prefix="windows-upstream-") as temporary:
        directory = Path(temporary)
        digest = hashlib.sha256(candidate.read_bytes()).hexdigest()
        executable = directory / "tmuxx.exe"
        shutil.copyfile(candidate, executable)
        assert hashlib.sha256(executable.read_bytes()).hexdigest() == digest
        owners = Owners(executable, shell, directory)
        fixture_options = {}
        if script.name == "format-modifiers.sh":
            # Its pane-z tie assertions rely on all idle panes retaining the
            # same initial title. Git's interactive prompt changes titles to
            # the cwd, which differs for the first pane and later splits.
            # Real silent native sleep processes preserve this fixture premise;
            # explicitly supplied script commands (including cat) are unchanged.
            owners.empty.write_text("set -g default-command 'exec sleep 120'\n", encoding="utf-8")
            fixture_options["default-command"] = "exec sleep 120"
        if "TMUXX_CONPTY_RESOURCES" not in owners.env:
            owners.env["TMUXX_CONPTY_RESOURCES"] = str(candidate.parent / "resources")
        class Handler(socketserver.StreamRequestHandler):
            def handle(self):
                self.connection.settimeout(15)
                try:
                    result = owners.request(json.loads(self.rfile.readline(65536)))
                except Exception as error:
                    result = {"error": str(error)}
                self.wfile.write(json.dumps(result).encode() + b"\n")
        server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), Handler)
        server.daemon_threads = True
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        wrapper = directory / "tmux-driver"
        # Git's sh accepts drive-qualified executable paths. Disable its argv
        # path heuristics: format values such as /usr/local/bin/foo are literals.
        def quoted(value):
            return "'" + str(value).replace("\\", "/").replace("'", "'\\''") + "'"
        driver = quoted(sys.executable) + " -X utf8 " + quoted(Path(__file__).resolve()) + " --bridge \"$@\""
        wrapper.write_text("#!/bin/sh\nset -o pipefail\ncontrol=0\nfor arg do\n case \"$arg\" in -C|-CC) control=1 ;; esac\ndone\n"
                           "if [ \"$control\" = 1 ]; then\n"
                           " TMUXX_UPSTREAM_INPUT_RELAY=$(cygpath -m \"$(mktemp)\")\n rm -f \"$TMUXX_UPSTREAM_INPUT_RELAY\"\n export TMUXX_UPSTREAM_INPUT_RELAY\n"
                           " { cat <&0 & relay=$!; echo \"$relay\" >\"$TMUXX_UPSTREAM_INPUT_RELAY.tmp\"; mv -- \"$TMUXX_UPSTREAM_INPUT_RELAY.tmp\" \"$TMUXX_UPSTREAM_INPUT_RELAY\"; wait \"$relay\"; } | " + driver + " | cat\n"
                           " status=$?\n rm -f -- \"$TMUXX_UPSTREAM_INPUT_RELAY\" \"$TMUXX_UPSTREAM_INPUT_RELAY.tmp\"\n exit \"$status\"\nelse\n exec " + driver + "\nfi\n",
                           encoding="utf-8")
        env = owners.env | {"TEST_TMUX": str(wrapper).replace("\\", "/"),
                            "TMUXX_UPSTREAM_PORT": str(server.server_address[1]),
                            "TMUXX_UPSTREAM_TOKEN": owners.token,
                            "TMUXX_UPSTREAM_EXE": str(executable),
                            "TMUXX_UPSTREAM_SHELL": str(shell),
                            "TMUXX_UPSTREAM_NATIVE_PATH": owners.env["PATH"],
                            "TMUXX_UPSTREAM_DIRECTORY": str(directory).replace("\\", "/"),
                            "MSYS_NO_PATHCONV": "1", "MSYS2_ARG_CONV_EXCL": "*",
                            # Git's grep fails literal non-BMP ERE matches in
                            # its UTF-8 locale. Its byte locale preserves the
                            # original UTF-8 pattern/input bytes exactly.
                            "TZ": "UTC", "LC_ALL": "C", "TMPDIR": str(directory).replace("\\", "/")}
        report = None
        try:
            # Set TMPDIR inside MSYS so its native-launch environment adapter
            # cannot turn the private absolute path into a Unix /tmp alias.
            result = run_suite_process([str(shell), "-c", 'TMPDIR=$TMUXX_UPSTREAM_DIRECTORY; export TMPDIR; . "$1"', "upstream", str(script)],
                                       cwd=script.parent, env=env, timeout=timeout)
            assert hashlib.sha256(executable.read_bytes()).hexdigest() == digest
            assert hashlib.sha256(script.read_bytes()).hexdigest() == source_digest
            report = {"returncode": result.returncode,
                    "stdout": base64.b64encode(result.stdout).decode(),
                    "stderr": base64.b64encode(result.stderr).decode(),
                    "explicit_owner_starts": owners.identities,
                    "candidate_sha256": digest,
                    "script_sha256": source_digest,
                    "fixture_options": fixture_options,
                    "shell": str(shell), "shell_sha256": hashlib.sha256(shell.read_bytes()).hexdigest()}
            return report
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)
            cleanup = owners.cleanup()
            if report is not None:
                report.update(cleanup)
            if cleanup_sink is not None:
                cleanup_sink.append(cleanup)


def cleanup_regression(candidate: Path, shell: Path):
    unrelated = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(180)"],
                                 creationflags=subprocess.CREATE_NO_WINDOW)
    cases = []
    try:
        with tempfile.TemporaryDirectory(prefix="upstream-cleanup-") as temporary:
            for name, suffix, timeout in [("success", "exit 0", 20), ("failure", "exit 7", 20),
                                          ("timeout", "sleep 30", 2)]:
                script = Path(temporary) / (name + ".sh")
                script.write_text("#!/bin/sh\n" +
                                  r'''"$TEST_TMUX" -Lowned -f/dev/null new-session -d -s flood "cat /dev/zero | tr '\000' x" || exit 1''' +
                                  "\nsleep .2\n" + suffix + "\n",
                                  encoding="utf-8")
                cleanup = []
                try:
                    result = run(candidate, shell, script, timeout=timeout, cleanup_sink=cleanup)
                    assert name != "timeout", "stalled fixture did not time out"
                    assert result["returncode"] == (7 if name == "failure" else 0), result
                except subprocess.TimeoutExpired:
                    assert name == "timeout", "ordinary fixture unexpectedly timed out"
                assert len(cleanup) == 1 and not cleanup[0]["cleanup_remaining"]
                assert len(cleanup[0]["observed_owned_pids"]) >= 3, "fixture never created live flood descendants"
                assert unrelated.poll() is None, "cleanup killed an unrelated process"
                cases.append({"case": name, **cleanup[0]})
        return {"returncode": 0, "stdout": "", "stderr": "", "cleanup_remaining": [], "cases": cases}
    finally:
        if unrelated.poll() is None:
            unrelated.kill()
        unrelated.wait(timeout=5)


def malformed(candidate: Path, shell: Path, script: Path):
    """Preserve raw parser vectors before ConPTY's terminal reserialization.

The original C script still runs unchanged on the pinned oracle. Native empty
panes receive every original byte through display-message -I. A separate actual
ConPTY application reads the negative DECRQSS reply instead of depending on Unix
tty caret-echo presentation, which is not a Windows console mechanism.
"""
    digest = hashlib.sha256(candidate.read_bytes()).hexdigest()
    source_digest = hashlib.sha256(script.read_bytes()).hexdigest()
    with tempfile.TemporaryDirectory(prefix="windows-malformed-") as temporary:
        directory = Path(temporary)
        executable = directory / "tmuxx.exe"
        shutil.copyfile(candidate, executable)
        owners = Owners(executable, shell, directory)
        if "TMUXX_CONPTY_RESOURCES" not in owners.env:
            owners.env["TMUXX_CONPTY_RESOURCES"] = str(candidate.parent / "resources")
        report = None
        try:
            selected = owners.request({"token": owners.token, "label": "malformed", "operation": "select", "start": True})
            def command(*args, data=None):
                result = subprocess.run([str(executable), "-L", selected["instance"], *args],
                                        env=owners.env, input=data, capture_output=True, timeout=20)
                assert result.returncode == 0 and not result.stderr, (args, result.returncode, result.stderr)
                return result.stdout
            def capture(name):
                # This is upstream input-common.inc's exact capture trimming:
                # remove trailing spaces/tabs and trailing empty lines only.
                lines = [line.rstrip(b" \t") for line in command("capture-pane", "-pN", "-t", name + ":", "-S", "0", "-E", "-").split(b"\n")]
                while lines and not lines[-1]:
                    lines.pop()
                return b"\n".join(lines) + b"\n"
            cases = [
                ("csi-param-discard", b"\x1b[" + b"1" * 80 + b"\x18OK", b"OK\n"),
                ("csi-interm-discard", b"\x1b[    \x18OK", b"OK\n"),
                ("osc-discard", b"\x1b]2;" + b"x" * 1100000 + b"\x1b\\\x1b[H\x1b[2JOK", b"OK\n"),
                ("apc-discard", b"\x1b_" + b"x" * 1100000 + b"\x1b\\\x1b[H\x1b[2JOK", b"OK\n"),
                ("unknown-csi", b"\x1b[?9999zOK", b"OK\n"),
                ("unknown-osc", b"\x1b]999;bad\x07OK", b"OK\n"),
                ("malformed-osc", b"\x1b]8;id=a:id=b;http://bad\x07X\x1b]8;id=no-separator\x07Y\x1b]9;4;5;200\x07\x1b]9;4;z\x07\x1b]10;notacolour\x07\x1b]11;notacolour\x07\x1b]12;notacolour\x07\x1b]4;999;red\x07\x1b]104;999\x07\x1b]52bad\x07\x1b]52;c;@@@\x07OK", b"XYOK\n"),
                ("malformed-dcs", b"\x1bP$qBAD\x1b\\OK", b"OK\n"),
                ("malformed-utf8", b"\xf0\x80\x80\x80A\xed\xa0\x80B", "\ufffdA\ufffdB\n".encode()),
            ]
            command("set-option", "-g", "history-limit", "0")
            for name, data, expected in cases:
                command("new-session", "-d", "-x", "8", "-y", "3", "-s", name, "")
                command("respawn-pane", "-k", "-E", "-t", name + ":")
                command("select-pane", "-t", name + ":", "-T", "discard-test")
                command("display-message", "-I", "-t", name + ":", data=data)
                assert capture(name) == expected, (name, capture(name), expected)
                if name in ("osc-discard", "apc-discard"):
                    assert command("display-message", "-p", "-t", name + ":", "#{pane_title}") == b"discard-test\n"
                if name == "malformed-osc":
                    raw = command("capture-pane", "-pR", "-t", name + ":")
                    for column, byte in enumerate(b"XYOK"):
                        row = next(line for line in raw.splitlines() if line.startswith(f"\t\tC 0,{column} ".encode()))
                        assert f"data=(1,1,{chr(byte)})".encode() in row and b"link=NONE linkid=NONE" in row, row
            reply = directory / "negative-decrqss.bin"
            application = r'''
import ctypes,msvcrt,os,pathlib,sys,threading,time
k=ctypes.WinDLL('kernel32',use_last_error=True)
k.GetStdHandle.restype=ctypes.c_void_p
k.SetConsoleMode.argtypes=[ctypes.c_void_p,ctypes.c_ulong]
assert k.SetConsoleCP(65001) and k.SetConsoleOutputCP(65001)
assert k.SetConsoleMode(k.GetStdHandle(-10),0x200)
assert k.SetConsoleMode(k.GetStdHandle(-11),0xD)
msvcrt.setmode(0,os.O_BINARY); msvcrt.setmode(1,os.O_BINARY)
received=bytearray()
def read():
 while True:
  data=os.read(0,1)
  if not data: return
  received.extend(data)
threading.Thread(target=read,daemon=True).start()
os.write(1,b'\x1bP$qBAD\x1b\\OK')
deadline=time.monotonic()+6
while not received and time.monotonic()<deadline: time.sleep(.005)
time.sleep(.2)
reply=pathlib.Path(sys.argv[1]); pending=reply.with_name(reply.name+'.tmp')
pending.write_bytes(received); pending.replace(reply)
time.sleep(90)
'''
            command("new-session", "-d", "-s", "real-query", sys.executable, "-u", "-c", application, str(reply))
            deadline = time.monotonic() + 10
            while not reply.is_file():
                assert time.monotonic() < deadline, "native ConPTY query application did not complete"
                time.sleep(.02)
            assert reply.read_bytes() == b"\x1bP0$r\x1b\\", reply.read_bytes()
            assert hashlib.sha256(executable.read_bytes()).hexdigest() == digest
            assert hashlib.sha256(script.read_bytes()).hexdigest() == source_digest
            report = {"returncode": 0, "stdout": "", "stderr": "", "cases": [case[0] for case in cases],
                      "candidate_sha256": digest, "script_sha256": source_digest,
                      "real_conpty_negative_decrqss_reply": base64.b64encode(reply.read_bytes()).decode(),
                      "parser_input": "native display-message -I empty panes"}
            return report
        finally:
            cleanup = owners.cleanup()
            if report is not None:
                report.update(cleanup)


def main():
    if sys.argv[1:2] == ["--bridge"]:
        bridge(sys.argv[2:])
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--script", type=Path)
    parser.add_argument("--cleanup-regression", action="store_true")
    parser.add_argument("--shell", type=Path, default=Path(shutil.which("sh") or ""))
    args = parser.parse_args()
    assert sys.platform == "win32" and args.shell.is_file()
    candidate, shell = args.candidate.resolve(strict=True), args.shell.resolve(strict=True)
    assert bool(args.script) != args.cleanup_regression
    if args.cleanup_regression:
        result = cleanup_regression(candidate, shell)
    else:
        script = args.script.resolve(strict=True)
        if script.name == "input-malformed.sh":
            result = malformed(candidate, shell, script)
        elif script.name == "buffers.sh":
            from windows_upstream_buffers import run as run_buffers
            result = run_buffers(candidate, shell, script)
        elif script.name in ("control-client-exit.sh", "control-client-exit-stalled.sh"):
            from windows_upstream_control import run as run_control
            result = run_control(candidate, shell, script)
        else:
            result = run(candidate, shell, script)
    print(json.dumps(result))
    return result["returncode"]


if __name__ == "__main__":
    raise SystemExit(main())
