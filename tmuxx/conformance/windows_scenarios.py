#!/usr/bin/env python3
"""Native Windows equivalents of the original twelve C/R smoke scenarios.

The command/observation matrices come from runner.SCENARIOS. Only fixture child
programs and private file I/O differ by operating system; no observed result is
normalized. The C side remains a real pinned server over binary no-PTY SSH.
"""
from __future__ import annotations

import argparse
import base64
import ctypes
from ctypes import wintypes
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time
import traceback
import uuid

import remote_c_reference as oracle
import runner


def encoded(data):
    return base64.b64encode(data).decode("ascii")


# These are the exact child fixtures in the shared scenarios. Arbitrary shell
# commands never pass through this mapping or get silently approximated.
SLEEP = "exec sleep 60"
CAPTURE = "printf 'ALPHA\\n\\033[31mRED\\033[0m\\nCAPTURE-READY'; exec sleep 60"
RANGES = "printf 'FIRST\\nSECOND\\nTHIRD\\nCAPTURE-READY'; exec sleep 60"
ECHO = "stty raw -echo; printf INPUT-READY; exec cat"
ENVIRONMENT = "printf 'ENV:%s:%s:%s:END' \"${TMUXX_GLOBAL-unset}\" \"${TMUXX_GONE-unset}\" \"${TMUXX_HIDDEN-unset}\"; exec sleep 60"
NATIVE_CHILDREN = {
    SLEEP: "import time; time.sleep(60)",
    CAPTURE: "import os,time; os.write(1,b'ALPHA\\n\\x1b[31mRED\\x1b[0m\\nCAPTURE-READY'); time.sleep(60)",
    RANGES: "import os,time; os.write(1,b'FIRST\\nSECOND\\nTHIRD\\nCAPTURE-READY'); time.sleep(60)",
    ENVIRONMENT: "import os,time; os.write(1,('ENV:'+':'.join(os.environ.get(k,'unset') for k in ['TMUXX_GLOBAL','TMUXX_GONE','TMUXX_HIDDEN'])+':END').encode()); time.sleep(60)",
    ECHO: """import ctypes,msvcrt,os
from ctypes import wintypes
k=ctypes.WinDLL('kernel32',use_last_error=True)
k.GetStdHandle.argtypes=[wintypes.DWORD]; k.GetStdHandle.restype=wintypes.HANDLE
k.GetConsoleMode.argtypes=[wintypes.HANDLE,ctypes.POINTER(wintypes.DWORD)]
k.SetConsoleMode.argtypes=[wintypes.HANDLE,wintypes.DWORD]
h=k.GetStdHandle(-10 & 0xffffffff); mode=wintypes.DWORD()
if not k.GetConsoleMode(h,ctypes.byref(mode)): raise ctypes.WinError(ctypes.get_last_error())
if not k.SetConsoleMode(h,mode.value & ~7): raise ctypes.WinError(ctypes.get_last_error())
msvcrt.setmode(0,os.O_BINARY); msvcrt.setmode(1,os.O_BINARY)
os.write(1,b'INPUT-READY')
while True:
 data=os.read(0,1)
 if not data: break
 os.write(1,data)
""",
}


class NativeServer:
    def __init__(self, executable, directory):
        self.executable, self.directory = str(executable), directory
        self.instance = "scenarios 雪 & " + uuid.uuid4().hex
        self.trace, self.handles = [], {}
        self.owner = None
        self.api = ctypes.WinDLL("kernel32", use_last_error=True)
        self.api.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        self.api.OpenProcess.restype = wintypes.HANDLE
        self.api.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        self.api.CloseHandle.argtypes = [wintypes.HANDLE]
        self.env = os.environ.copy()
        for name in ("TMUX", "TMUXX_ENDPOINT"):
            self.env.pop(name, None)
        config = Path(directory) / "empty.conf"
        config.write_bytes(b"")
        self.log = (Path(directory) / "owner.log").open("wb")
        try:
            self.owner = subprocess.Popen([self.executable, "-D", "-L", self.instance, "-f", str(config)],
                stdin=subprocess.DEVNULL, stdout=self.log, stderr=self.log, env=self.env,
                creationflags=subprocess.CREATE_NO_WINDOW)
            deadline = time.monotonic() + 10
            while True:
                result = self.command("list-sessions", check=False)
                if result.returncode == 0:
                    break
                if self.owner.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("native scenario owner did not become ready")
                time.sleep(.02)
        except BaseException:
            self.close()
            raise

    def command(self, *args, check=True):
        effective = list(args)
        if effective and effective[0] in ("new-session", "new-window", "split-window"):
            child = effective[-1]
            if child not in NATIVE_CHILDREN:
                raise ValueError(f"unreviewed native scenario child fixture: {child!r}")
            effective[-1:] = [sys.executable, "-u", "-c", NATIVE_CHILDREN[child]]
        result = subprocess.run([self.executable, "-L", self.instance, *effective], env=self.env,
            input=b"", capture_output=True, timeout=10, creationflags=subprocess.CREATE_NO_WINDOW)
        self.trace.append({"argv_base64": [encoded(arg.encode("utf-8")) for arg in effective],
            "status": result.returncode, "stdout_base64": encoded(result.stdout), "stderr_base64": encoded(result.stderr)})
        if effective and effective[0] in ("new-session", "new-window", "split-window") and result.returncode == 0:
            listed = self.command("list-panes", "-a", "-F", "#{pane_pid}").stdout
            for raw in listed.splitlines():
                pid = int(raw)
                if pid not in self.handles:
                    handle = self.api.OpenProcess(0x100000, False, pid)
                    if not handle:
                        raise ctypes.WinError(ctypes.get_last_error())
                    self.handles[pid] = handle
        if check and result.returncode:
            raise AssertionError(f"{args!r}: {result.returncode}, {result.stdout!r}, {result.stderr!r}")
        return result

    def start(self, child=SLEEP):
        self.command("new-session", "-d", "-s", "parity", "-n", "first", "-x", "80", "-y", "24", child)
        self.command("has-session", "-t", "parity")

    wait_capture = runner.Server.wait_capture

    def file(self, name, data):
        path = Path(self.directory) / name
        path.write_bytes(data)
        # Forward slashes remain an exact native path while quoted config text
        # cannot reinterpret backslashes as shell/config escapes.
        return path.as_posix()

    def close(self):
        remaining, errors = [], []
        try:
            if self.owner is not None:
                if self.owner.poll() is None:
                    try:
                        self.command("kill-server", check=False)
                        self.owner.wait(timeout=5)
                    except (OSError, subprocess.SubprocessError):
                        self.owner.kill()
                        self.owner.wait(timeout=5)
                for pid, handle in self.handles.items():
                    if self.api.WaitForSingleObject(handle, 5000) != 0:
                        remaining.append(pid)
        except (OSError, subprocess.SubprocessError) as error:
            errors.append(str(error))
        finally:
            for handle in self.handles.values():
                self.api.CloseHandle(handle)
            self.handles.clear()
            self.log.close()
        return {"cleanup_remaining": remaining, "cleanup_errors": errors}


class RemoteServer:
    def __init__(self, config):
        self.trace, self.child = [], None
        self.closed = False
        self.child = subprocess.Popen([sys.executable, str(Path(oracle.__file__).resolve()), "--config", str(config)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=None, creationflags=subprocess.CREATE_NO_WINDOW)
        self.reader = oracle.TimedFrames(self.child.stdout)
        try:
            code, data, error = oracle.unpack_response(self.reader.next(30))
            if code:
                raise RuntimeError(error.decode("utf-8", "replace"))
            self.identity = json.loads(data)
            self.directory = str(Path(self.identity["socket"]).parent).replace("\\", "/")
        except BaseException:
            self.child.stdin.close()
            self.child.kill()
            self.child.wait(timeout=5)
            raise

    def command(self, *args, check=True):
        oracle.write_frame(self.child.stdin, oracle.request(1, [arg.encode("utf-8") for arg in args]))
        data = self.reader.next(30)
        if data is None:
            raise EOFError("remote C operation ended ambiguously; it will not be replayed")
        code, stdout, stderr = oracle.unpack_response(data)
        self.trace.append({"argv_base64": [encoded(arg.encode("utf-8")) for arg in args],
            "status": code, "stdout_base64": encoded(stdout), "stderr_base64": encoded(stderr)})
        if check and code:
            raise AssertionError(f"{args!r}: {code}, {stdout!r}, {stderr!r}")
        return subprocess.CompletedProcess(args, code, stdout, stderr)

    def start(self, child=SLEEP):
        self.command("new-session", "-d", "-s", "parity", "-n", "first", "-x", "80", "-y", "24", child)
        self.command("has-session", "-t", "parity")

    wait_capture = runner.Server.wait_capture

    def file(self, name, data):
        # Files are confined to this live oracle's private directory. Base64
        # and shlex.quote preserve exact bytes without interpolated shell code.
        if name not in ("child.conf", "main.conf"):
            raise ValueError("unreviewed scenario filename")
        path = self.directory + "/" + name
        script = "import base64,pathlib;pathlib.Path(" + repr(path) + ").write_bytes(base64.b64decode(" + repr(encoded(data)) + "))"
        self.command("run-shell", "python3 -c " + shlex.quote(script))
        return path

    def close(self):
        if self.closed:
            return
        self.closed = True
        try:
            oracle.write_frame(self.child.stdin, oracle.request(3))
            code, observed, diagnostic = oracle.unpack_response(self.reader.next(30))
            cleanup = json.loads(observed)
            if code or cleanup.get("cleanup_remaining") != [] or cleanup.get("cleanup_errors") != []:
                raise AssertionError(f"remote scenario cleanup failed: {observed!r}, {diagnostic!r}")
            print("TMUXX_REMOTE_C_CLEANUP=" + json.dumps(cleanup, sort_keys=True), file=sys.stderr, flush=True)
        finally:
            self.child.stdin.close()
            try:
                self.child.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait(timeout=5)
            self.child.stdout.close()


def config_order(server):
    """Original prepare/include/queue matrix, with each host's private paths."""
    server.start()
    child = server.file("child.conf", b"set-option -g @included included\n")
    source = server.file("main.conf", (
        "TMUXX_VALUE=early\nset-environment -g TMUXX_VALUE late\n"
        "set-option -g @flag 1\n%if #{@flag}\nset-option -g @branch wrong\n"
        "%else\nset-option -g @branch prepared\n%endif\n"
        "set-option -g @value \"$TMUXX_VALUE\"\n"
        f"source-file '{child}'\nset-option -gF @after '#{{@included}}'\n").encode("utf-8"))
    server.command("set-option", "-g", "@flag", "0")
    server.command("source-file", source)
    return {name: server.command("show-options", "-gqv", name).stdout
            for name in ("@branch", "@value", "@included", "@after")}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--report", required=True, type=Path)
    args = parser.parse_args()
    if sys.platform != "win32":
        parser.error("the native scenario candidate requires Windows")
    candidate = args.candidate.resolve(strict=True)
    candidate_hash = oracle.digest(candidate)
    directory = Path(tempfile.mkdtemp(prefix="windows-scenarios-", dir=args.report.parent))
    result = {"schema": 1, "kind": "remote-c-scenarios", "passed": False,
        "candidate_sha256": candidate_hash, "tests": [], "failures": [], "cleanup_remaining": [],
        "artifacts": str(directory), "scenarios": {}}
    for name, original in runner.SCENARIOS.items():
        native, reference = None, None
        details = {}
        fixture_path = directory / name
        fixture_path.mkdir()
        try:
            native = NativeServer(candidate, fixture_path)
            reference = RemoteServer(args.config)
            scenario = config_order if name == "config-order" else original
            wanted, actual = scenario(reference), scenario(native)
            details.update(reference={key: encoded(value) for key, value in wanted.items()},
                           native={key: encoded(value) for key, value in actual.items()})
            if wanted != actual:
                raise AssertionError(f"byte-exact C/native scenario mismatch: {name}")
        except (Exception, KeyboardInterrupt) as error:
            details["error"] = str(error)
            result["failures"].append({"scenario": name, "error": str(error)})
            traceback.print_exc(file=sys.stderr)
        finally:
            if native is not None:
                details["native_trace"] = native.trace
                cleanup = native.close()
                details["native_cleanup"] = cleanup
                result["cleanup_remaining"].extend(cleanup["cleanup_remaining"])
                if cleanup["cleanup_errors"] or cleanup["cleanup_remaining"]:
                    result["failures"].append({"scenario": name, "cleanup": cleanup})
            if reference is not None:
                details["reference_trace"] = reference.trace
                try:
                    reference.close()
                except Exception as error:
                    result["failures"].append({"scenario": name, "remote_cleanup": str(error)})
            result["scenarios"][name] = details
            if not any(item["scenario"] == name for item in result["failures"]):
                result["tests"].append("windows_scenarios::" + name.replace("-", "_"))
            args.report.write_text(json.dumps(result, indent=2), encoding="utf-8")
    if oracle.digest(candidate) != candidate_hash:
        result["failures"].append({"error": "candidate executable changed during scenarios"})
    result["passed"] = not result["failures"] and not result["cleanup_remaining"]
    args.report.write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(json.dumps(result), flush=True)
    return int(not result["passed"])


if __name__ == "__main__":
    raise SystemExit(main())
