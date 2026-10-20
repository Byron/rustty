#!/usr/bin/env python3
"""Native desktop/local-pipe/real-OpenSSH qualification with private fixtures.

The desktop observer runs under the GUI account. The collector runs under an
independent account with its own SSH credential; secrets never cross accounts.
SSH uses exec without a PTY and selects the existing instance in protocol data.
"""
from __future__ import annotations

import argparse
import base64
import ctypes
from ctypes import wintypes as w
import hashlib
import json
import msvcrt
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import uuid
from windows_native_views import wait_json

K = ctypes.WinDLL("kernel32", use_last_error=True)
A = ctypes.WinDLL("advapi32", use_last_error=True)
K.CreateFileW.argtypes = [w.LPCWSTR, w.DWORD, w.DWORD, w.LPVOID, w.DWORD, w.DWORD, w.HANDLE]
K.CreateFileW.restype = w.HANDLE
K.CloseHandle.argtypes = [w.HANDLE]
K.PeekNamedPipe.argtypes = [w.HANDLE, w.LPVOID, w.DWORD, w.LPVOID, ctypes.POINTER(w.DWORD), w.LPVOID]
K.GetNamedPipeServerProcessId.argtypes = [w.HANDLE, ctypes.POINTER(w.ULONG)]
K.ProcessIdToSessionId.argtypes = [w.DWORD, ctypes.POINTER(w.DWORD)]
K.GetCurrentProcess.restype = w.HANDLE
K.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]
K.OpenProcess.restype = w.HANDLE
K.TerminateProcess.argtypes = [w.HANDLE, w.UINT]
K.K32EnumProcesses.argtypes = [ctypes.POINTER(w.DWORD), w.DWORD, ctypes.POINTER(w.DWORD)]
K.QueryFullProcessImageNameW.argtypes = [w.HANDLE, w.DWORD, w.LPWSTR, ctypes.POINTER(w.DWORD)]
K.CreateToolhelp32Snapshot.argtypes = [w.DWORD, w.DWORD]
K.CreateToolhelp32Snapshot.restype = w.HANDLE
K.WaitForSingleObject.argtypes = [w.HANDLE, w.DWORD]
K.WaitForSingleObject.restype = w.DWORD
K.LocalFree.argtypes = [w.HANDLE]
A.OpenProcessToken.argtypes = [w.HANDLE, w.DWORD, ctypes.POINTER(w.HANDLE)]
A.GetTokenInformation.argtypes = [w.HANDLE, ctypes.c_int, w.LPVOID, w.DWORD, ctypes.POINTER(w.DWORD)]
A.ConvertSidToStringSidW.argtypes = [w.LPVOID, ctypes.POINTER(w.LPWSTR)]
INVALID = ctypes.c_void_p(-1).value
TIMEOUT = 8.0
GRAPH = ("list-windows", "-a", "-F", "#{session_id}|#{window_index}|#{window_id}|#{window_name}|#{window_panes}")
PANES = ("list-panes", "-a", "-F", "#{pane_id}|#{pane_pid}|#{window_id}")


class ProcessEntry(ctypes.Structure):
    _fields_ = [("size", w.DWORD), ("usage", w.DWORD), ("pid", w.DWORD), ("heap", ctypes.c_size_t),
                ("module", w.DWORD), ("threads", w.DWORD), ("parent", w.DWORD), ("priority", w.LONG),
                ("flags", w.DWORD), ("exe", w.WCHAR * 260)]


K.Process32FirstW.argtypes = [w.HANDLE, ctypes.POINTER(ProcessEntry)]
K.Process32NextW.argtypes = [w.HANDLE, ctypes.POINTER(ProcessEntry)]


def descendants(pid):
    snapshot = K.CreateToolhelp32Snapshot(2, 0)
    if snapshot == INVALID:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        entry, rows = ProcessEntry(), []
        entry.size = ctypes.sizeof(entry)
        present = K.Process32FirstW(snapshot, ctypes.byref(entry))
        while present:
            rows.append({"pid": entry.pid, "parent": entry.parent, "exe": entry.exe})
            present = K.Process32NextW(snapshot, ctypes.byref(entry))
        family = {pid}
        while True:
            expanded = family | {row["pid"] for row in rows if row["parent"] in family}
            if expanded == family:
                return [row for row in rows if row["pid"] in family and row["pid"] != pid]
            family = expanded
    finally:
        K.CloseHandle(snapshot)


def checked(result):
    if not result:
        raise ctypes.WinError(ctypes.get_last_error())
    return result


def identity(pid=None):
    process = K.GetCurrentProcess() if pid is None else K.OpenProcess(0x1000, False, pid)
    checked(process)
    try:
        return identity_from_handle(process, os.getpid() if pid is None else pid)
    finally:
        if pid is not None:
            K.CloseHandle(process)


def identity_from_handle(process, pid):
    """Read the primary token of this retained process, without reopening its PID."""
    token = w.HANDLE()
    try:
        checked(A.OpenProcessToken(process, 8, ctypes.byref(token)))
        length = w.DWORD()
        A.GetTokenInformation(token, 1, None, 0, ctypes.byref(length))
        user = ctypes.create_string_buffer(length.value)
        checked(A.GetTokenInformation(token, 1, user, length.value, ctypes.byref(length)))
        sid = ctypes.cast(user, ctypes.POINTER(w.LPVOID))[0]
        text = w.LPWSTR()
        checked(A.ConvertSidToStringSidW(sid, ctypes.byref(text)))
        try:
            account_sid = text.value
        finally:
            K.LocalFree(ctypes.cast(text, w.HANDLE))
        statistics = ctypes.create_string_buffer(128)
        checked(A.GetTokenInformation(token, 10, statistics, len(statistics), ctypes.byref(length)))
        authentication_id = int.from_bytes(statistics.raw[8:16], "little")
        session = w.DWORD()
        checked(A.GetTokenInformation(token, 12, ctypes.byref(session), ctypes.sizeof(session),
                                      ctypes.byref(length)))  # TokenSessionId
        return {"account_sid": account_sid, "logon_id": authentication_id, "session_id": session.value,
                "pid": pid}
    finally:
        if token:
            K.CloseHandle(token)


def raw_pipe_probe(path):
    handle = K.CreateFileW(path, 0xC0000000, 0, None, 3, 0, None)
    if handle == INVALID:
        return {"connected": False, "winerror": ctypes.get_last_error()}
    K.CloseHandle(handle)
    return {"connected": True, "winerror": 0}


def process_exited(handle):
    state = K.WaitForSingleObject(handle, 0)
    if state == 0xFFFFFFFF:
        raise ctypes.WinError(ctypes.get_last_error())
    assert state in (0, 258), ("unexpected process wait result", state)
    return state == 0


def identities_for_image(path):
    """Match image and token while retaining one process handle through both reads."""
    count = 1024
    while True:
        pids, used = (w.DWORD * count)(), w.DWORD()
        checked(K.K32EnumProcesses(pids, ctypes.sizeof(pids), ctypes.byref(used)))
        if used.value < ctypes.sizeof(pids):
            break
        count *= 2
        assert count <= 65536, "process inventory exceeded bounded fixture limit"
    found = []
    for pid in pids[:used.value // ctypes.sizeof(w.DWORD)]:
        handle = K.OpenProcess(0x00101000, False, pid)  # QUERY_LIMITED_INFORMATION | SYNCHRONIZE
        if not handle:
            continue  # Protected/system processes cannot be this user's fixture.
        try:
            size = w.DWORD(32768)
            value = ctypes.create_unicode_buffer(size.value)
            if K.QueryFullProcessImageNameW(handle, 0, value, ctypes.byref(size)):
                try:
                    # Windows image queries may return a device/extended path
                    # for the same file. Compare file identity, not spelling.
                    matched = Path(value.value).samefile(path)
                except (FileNotFoundError, PermissionError):
                    continue
                if not matched or process_exited(handle):
                    continue
                try:
                    item = identity_from_handle(handle, pid)
                except OSError:
                    # Exit can dispose of the primary token between the image
                    # and token reads. Suppress only a confirmed process exit;
                    # denied token access to a live matched image is a failure.
                    if process_exited(handle):
                        continue
                    raise
                if not process_exited(handle):
                    found.append(item)
        finally:
            K.CloseHandle(handle)
    return found


def processes_for_image(path):
    """Read-only cleanup audit for the unique private bridge executable."""
    return [item["pid"] for item in identities_for_image(path)]


def peek(file):
    available = w.DWORD()
    if not K.PeekNamedPipe(msvcrt.get_osfhandle(file.fileno()), None, 0, None, ctypes.byref(available), None):
        code = ctypes.get_last_error()
        if code in (109, 232):
            return None
        raise ctypes.WinError(code)
    return available.value


class Connection:
    def __init__(self, reader, writer, process=None, pipe_handle=None):
        self.reader, self.writer, self.process = reader, writer, process
        self.buffer, self.stderr = bytearray(), bytearray()
        self.next_id, self.hello, self.owner_pid = 1, None, None
        if pipe_handle is not None:
            owner = w.ULONG()
            checked(K.GetNamedPipeServerProcessId(pipe_handle, ctypes.byref(owner)))
            self.owner_pid = owner.value

    @classmethod
    def local(cls, endpoint):
        handle = K.CreateFileW(endpoint, 0xC0000000, 0, None, 3, 0, None)
        if handle == INVALID:
            raise ctypes.WinError(ctypes.get_last_error())
        file = os.fdopen(msvcrt.open_osfhandle(handle, os.O_RDWR | os.O_BINARY), "r+b", buffering=0)
        return cls(file, file, pipe_handle=handle)

    @classmethod
    def ssh(cls, options, command):
        process = subprocess.Popen(options + [command], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW, bufsize=0)
        return cls(process.stdout, process.stdin, process=process)

    def write(self, record, fragment=False):
        payload = json.dumps(record, ensure_ascii=False, separators=(",", ":")).encode("utf-8") + b"\n"
        for at in range(0, len(payload), 3 if fragment else len(payload)):
            self.writer.write(payload[at:at + (3 if fragment else len(payload))])
        self.writer.flush()

    def record(self, timeout=TIMEOUT):
        deadline = time.monotonic() + timeout
        while True:
            if b"\n" in self.buffer:
                line, _, self.buffer = self.buffer.partition(b"\n")
                return json.loads(line)
            if len(self.buffer) > 2 * 1024 * 1024:
                raise AssertionError("unbounded native protocol record")
            if self.process and self.process.stderr:
                available = peek(self.process.stderr)
                if available:
                    self.stderr.extend(self.process.stderr.read(min(available, 65536)))
                    assert len(self.stderr) <= 65536, "unbounded bridge diagnostics"
            available = peek(self.reader)
            if available is None:
                raise EOFError(self.stderr.decode("utf-8", "replace"))
            if available:
                self.buffer.extend(self.reader.read(min(available, 65536)))
                continue
            if time.monotonic() >= deadline:
                raise TimeoutError("native response timed out: " + self.stderr.decode("utf-8", "replace"))
            time.sleep(0.005)

    def handshake(self, instance, incarnation=None):
        self.write({"version": 1, "instance": instance, "instance_id": incarnation,
                    "snapshot_stream": True, "host_effects": False, "control_commands": True}, fragment=True)
        self.hello = self.record()
        assert self.hello["version"] == 1 and self.hello.get("instance_id"), self.hello
        if incarnation:
            assert self.hello["instance_id"] == incarnation
        return self.hello

    def request(self, *arguments, subscribe=False, stdin=None):
        request_id = self.next_id
        self.next_id += 1
        # This helper sends exactly one command. Quote tmux's trailing argv
        # separator while preserving the literal semicolon in names/content.
        arguments = [arg[:-1] + "\\;" if arg.endswith(";") else arg for arg in arguments]
        self.write({"request_id": request_id, "argv": [list(arg.encode("utf-8")) for arg in arguments],
                    "stdin": None if stdin is None else list(stdin), "subscribe": subscribe, "control": False}, fragment=True)
        snapshot, stream, expected, sequence, reply = None, None, None, None, None
        while reply is None or (subscribe and snapshot is None):
            record = self.record()
            kind = record["type"]
            if kind == "reply" and record["request_id"] == request_id:
                assert record.get("error") is None, record
                reply = bytes(record["output"])
            elif kind == "snapshot":
                snapshot = record["state"]
            elif kind == "snapshot_start":
                assert stream is None
                stream, expected, sequence = bytearray(), record["bytes"], record["sequence"]
                assert record["instance_id"] == self.hello["instance_id"]
                assert 0 <= expected <= 64 * 1024 * 1024
            elif kind == "snapshot_chunk":
                assert stream is not None and record["offset"] == len(stream)
                stream.extend(base64.b64decode(record["data"], validate=True))
                assert len(stream) <= expected
            elif kind == "snapshot_end":
                assert stream is not None and len(stream) == expected and record["sequence"] == sequence
                snapshot, stream = json.loads(stream), None
            elif kind in ("snapshot_aborted", "exit"):
                raise AssertionError(record)
        return (reply, snapshot) if subscribe else reply

    def close(self):
        if self.process:
            if self.process.poll() is None:
                self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        for file in {self.reader, self.writer, self.process.stderr if self.process else None}:
            if file:
                file.close()


def ps_source(script):
    return "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand " + base64.b64encode(script.encode("utf-16-le")).decode("ascii")


def ps_quote(value):
    return "'" + value.replace("'", "''") + "'"


def options(args, key=None):
    return [args.ssh, "-i", str(key or args.key), "-oBatchMode=yes", "-oIdentitiesOnly=yes",
            "-oStrictHostKeyChecking=yes", "-oUserKnownHostsFile=" + str(args.known_hosts),
            "-oConnectTimeout=5", "-T", args.host]


def run_ssh_json(args, script):
    command = ps_source("& " + ps_quote(sys.executable) + " " + ps_quote(str(Path(__file__).resolve())) + " " + script + "; exit $LASTEXITCODE")
    result = subprocess.run(options(args) + [command], capture_output=True, timeout=10,
                            creationflags=subprocess.CREATE_NO_WINDOW)
    assert result.returncode == 0, result.stderr.decode("utf-8", "replace")
    return json.loads(result.stdout)


def remote_images(args, path):
    # OpenSSH children may carry an elevated token/DACL inaccessible to the
    # medium-integrity desktop token. Inspect them in a real authenticated SSH
    # logon, rather than treating local OpenProcess denial as process absence.
    return run_ssh_json(args, "--process-image " + ps_quote(str(path)))


def rustty_bridge_command(path):
    # One complete trusted command; instance selection remains binary protocol
    # data and never becomes part of the remote shell command.
    return ps_source("& " + ps_quote(str(path)) + " tmux --native-stdio; exit $LASTEXITCODE")


def dispatch_receipts(directory, previous, command, bridge, *, accepted):
    """Require fresh dispatcher evidence bound to the exact command and image."""
    digest = hashlib.sha256(command.encode("utf-8")).hexdigest()
    receipts = []
    for path in sorted(set(directory.glob("*.json")) - previous):
        value = json.loads(path.read_text(encoding="utf-8"))
        assert value["expected_command_sha256"] == digest, value
        assert Path(value["bridge_path"]).samefile(bridge), value
        if value["accepted"] is accepted:
            if accepted:
                assert value["original_command_sha256"] == digest, value
                assert type(value["child_pid"]) is int and value["child_pid"] > 0, value
            else:
                assert value["original_command_sha256"] != digest and value["child_pid"] is None, value
            receipts.append({"path": str(path), **value})
        else:
            raise AssertionError("unexpected dispatcher disposition: " + repr(value))
    assert receipts, "no fresh matching forced-command dispatcher receipt"
    return receipts


def trusted_bridge_evidence(args, previous, command, desktop, *, connections=2):
    directory = args.rustty_viewer_key.parent / "dispatch-receipts"
    deadline = time.monotonic() + TIMEOUT
    while len(bridges := remote_images(args, args.rustty_bridge)) != connections:
        assert time.monotonic() < deadline, ("expected primary plus one resize bridge per native session", bridges)
        time.sleep(.05)
    receipts = dispatch_receipts(directory, previous, command, args.rustty_bridge, accepted=True)
    assert bridges, "actual native SSH viewer did not launch the configured Rustty bridge"
    assert all(item["account_sid"] == desktop["account_sid"]
               and item["logon_id"] != desktop["logon_id"]
               and item["session_id"] != desktop["session_id"] for item in bridges), bridges
    trees = {item["child_pid"]: run_ssh_json(args, "--descendants " + str(item["child_pid"]))
             for item in receipts}
    assert all(any(bridge["pid"] in {row["pid"] for row in tree} for tree in trees.values())
               for bridge in bridges), (bridges, trees)
    return {"receipts": receipts, "bridge_identities": bridges, "command_descendants": trees,
            "bridge_sha256": hashlib.sha256(args.rustty_bridge.read_bytes()).hexdigest()}


def atomic_json(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, ensure_ascii=True, indent=2), encoding="utf-8")
    temporary.replace(path)


def wait_observer(args, predicate=lambda item: item.get("status") == "active", timeout=TIMEOUT):
    deadline, latest = time.monotonic() + timeout, None
    while time.monotonic() < deadline:
        if args.observer.is_file():
            try:
                latest = json.loads(args.observer.read_text(encoding="utf-8"))
            except (PermissionError, json.JSONDecodeError):
                continue
            if latest.get("error"):
                raise AssertionError("desktop observer: " + latest["error"])
            if predicate(latest):
                return latest
        time.sleep(0.025)
    raise TimeoutError("desktop observer did not reach expected state: " + repr(latest))


def denial_helper(directory):
    """Run under a real, distinct account while the desktop suite is active."""
    directory.mkdir(parents=True, exist_ok=True)
    started = time.time()
    deadline = time.monotonic() + 180
    while time.monotonic() < deadline:
        request = directory / "request.json"
        if request.is_file():
            value = json.loads(request.read_text(encoding="utf-8"))
            if value["created_at"] >= started:
                atomic_json(directory / "response.json", {
                    "nonce": value["nonce"], "endpoint": value["endpoint"], "incarnation": value["incarnation"],
                    "script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                    "identity": identity(), "probe": raw_pipe_probe(value["endpoint"]),
                })
                return 0
        time.sleep(0.05)
    raise TimeoutError("no fresh desktop denial request within 180 seconds")


def denied_probe(args, report, endpoint, incarnation):
    if report["collector_identity"]["account_sid"] != report["desktop_identity"]["account_sid"]:
        evidence = {"identity": identity(), "probe": raw_pipe_probe(endpoint)}
    else:
        if not args.denial_directory:
            raise AssertionError("desktop collection needs a distinct-account --deny-helper in --denial-directory")
        nonce = uuid.uuid4().hex
        args.denial_directory.mkdir(parents=True, exist_ok=True)
        atomic_json(args.denial_directory / "request.json", {
            "created_at": time.time(), "nonce": nonce, "endpoint": endpoint, "incarnation": incarnation,
        })
        deadline = time.monotonic() + 30
        response = args.denial_directory / "response.json"
        while time.monotonic() < deadline:
            if response.is_file():
                evidence = json.loads(response.read_text(encoding="utf-8"))
                if evidence.get("nonce") == nonce:
                    break
            time.sleep(0.05)
        else:
            raise TimeoutError("distinct-account denial helper did not answer the fresh fixture nonce")
        assert evidence["endpoint"] == endpoint and evidence["incarnation"] == incarnation
        assert evidence["script_sha256"] == hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    assert evidence["identity"]["account_sid"] != report["desktop_identity"]["account_sid"]
    assert evidence["probe"] == {"connected": False, "winerror": 5}, evidence
    return evidence


class NativeSshViewer:
    """A second real Rustty process, with only its SSH children in a fixture job."""
    def __init__(self, args, instance, suffix="", *, key=None, command=None):
        from runner import WindowsSuiteJob
        self.directory = args.observer.parent / ("ssh-native-viewer" + suffix)
        self.directory.mkdir()
        self.hold = self.directory / "hold"
        self.hold.write_text("hold", encoding="ascii")
        # -F disables the user's default configuration. Percent is an OpenSSH
        # path token even inside quotes, so preserve it explicitly.
        def config_path(path):
            return '"' + str(path.resolve()).replace("\\", "/").replace("%", "%%") + '"'
        config = self.directory / "ssh config 雪 &%.conf"
        config.write_text("Host *\n  BatchMode yes\n  IdentitiesOnly yes\n"
                          "  StrictHostKeyChecking yes\n  ConnectTimeout 5\n"
                          "  RequestTTY no\n  ClearAllForwardings yes\n"
                          "  IdentityFile " + config_path(key or args.viewer_key) + "\n"
                          "  UserKnownHostsFile " + config_path(args.known_hosts) + "\n", encoding="utf-8")
        env = os.environ.copy()
        for name in ("TMUXX_ENDPOINT", "TMUXX_CONPTY_RESOURCES", "RUSTTY_RESOURCES_DIR", "RUSTTY_SMOKE_SECONDS"):
            env.pop(name, None)
        env.update(RUSTTY_TMUXX_SSH_CONFIG=str(config), RUSTTY_TMUXX_SMOKE_DIR=str(self.directory),
                   RUSTTY_TMUXX_SMOKE_HOLD_FILE=str(self.hold), RUSTTY_TMUXX_SMOKE_VISIBLE="1",
                   RUSTTY_TMUXX_SMOKE_ATTACH_ONLY="1")
        env["PATH"] = str(Path(args.ssh).parent) + os.pathsep + env["PATH"]
        self.argv = [str(args.gui), "--tmuxx=ssh:" + args.host, "--tmuxx-instance=" + instance,
                     "--renderer=software", "--config-default-files=false", "--stream-deck=false"]
        if command is not None:
            self.argv.append("--tmuxx-ssh-command=" + command)
        self.job, self.worker, self.result = WindowsSuiteJob(), None, None
        self.log = (self.directory / "viewer.log").open("wb")
        try:
            self.worker = subprocess.Popen([sys.executable, str(Path(__file__).with_name("runner.py")),
                                            "--suite-child", *self.argv], stdin=subprocess.PIPE,
                                           stdout=self.log, stderr=self.log, env=env,
                                           creationflags=subprocess.CREATE_NO_WINDOW)
            self.job.assign(self.worker)
            self.worker.stdin.write(b"\x01")
            self.worker.stdin.close()
            ready = self.directory / "ready.json"
            self.ready = wait_json(ready, self.worker, seconds=30)
            assert not self.ready["hidden"] and self.ready["view_graph_consistent"] and not self.ready["errors"], self.ready
            from windows_native_views import assert_rendered_workspaces
            assert_rendered_workspaces(self.ready)
        except BaseException:
            self.close(verify=False)
            raise

    def abrupt(self):
        pid = self.ready["process_id"]
        children = descendants(pid)
        assert any(row["exe"].lower() == "ssh.exe" for row in children), children
        handles = {}
        try:
            handles[pid] = checked(K.OpenProcess(0x00100001, False, pid))
            for row in children:
                handles[row["pid"]] = checked(K.OpenProcess(0x00100000, False, row["pid"]))
            checked(K.TerminateProcess(handles[pid], 91))
            # The fixture job deliberately remains open here. Only product
            # ownership can terminate these SSH children after GUI death.
            remaining = [child for child, handle in handles.items() if K.WaitForSingleObject(handle, 5000) != 0]
            assert not remaining, "SSH children survived abrupt GUI death: " + repr(remaining)
            return {"gui_pid": pid, "children": children, "remaining": remaining,
                    "checked_before_fixture_job_close": True}
        finally:
            for handle in handles.values():
                K.CloseHandle(handle)

    def close(self, verify=True):
        if self.job is None:
            return self.result
        try:
            self.hold.unlink(missing_ok=True)
            if verify:
                self.worker.wait(timeout=20)
                assert self.worker.returncode == 0, "SSH Rustty viewer failed at exit"
                self.result = json.loads((self.directory / "result.json").read_text(encoding="utf-8"))
                assert self.result["view_graph_consistent"] and not self.result["errors"], self.result
                assert self.result["rendered_hosts"] and self.result["ui_labels_supported"], self.result
                from windows_native_views import assert_rendered_workspaces
                assert_rendered_workspaces(self.result)
                for host in self.result["rendered_hosts"]:
                    assert (self.directory / ("window-" + host + ".png")).is_file()
            return self.result
        finally:
            self.job.close()
            self.job = None
            if self.worker:
                self.worker.wait(timeout=5)
            self.log.close()


def desktop_observer(args):
    from runner import WindowsSuiteJob
    directory = args.observer.parent
    directory.mkdir(parents=True, exist_ok=True)
    hold = directory / "hold"
    hold.write_text("hold", encoding="ascii")
    env = os.environ.copy()
    for name in ("TMUXX_ENDPOINT", "TMUXX_CONPTY_RESOURCES", "RUSTTY_RESOURCES_DIR", "RUSTTY_TMUXX_SMOKE_ATTACH_ONLY"):
        env.pop(name, None)
    env.update(RUSTTY_TMUXX_SMOKE_DIR=str(directory), RUSTTY_TMUXX_SMOKE_HOLD_FILE=str(hold),
               RUSTTY_TMUXX_SMOKE_VISIBLE="1", RUSTTY_SMOKE_SECONDS="200")
    label = "ssh-" + uuid.uuid4().hex
    argv = [str(args.gui), "--tmuxx=embedded", "--tmuxx-instance=" + label,
            "--renderer=software", "--config-default-files=false", "--stream-deck=false"]
    job, worker, connection, handles = WindowsSuiteJob(), None, None, {}
    report = {"schema": 1, "status": "starting", "observer_identity": identity(),
              "gui_sha256": hashlib.sha256(args.gui.read_bytes()).hexdigest(), "cleanup_remaining": []}
    try:
        with (directory / "desktop.log").open("wb") as log:
            worker = subprocess.Popen([sys.executable, str(Path(__file__).with_name("runner.py")), "--suite-child", *argv],
                                      stdin=subprocess.PIPE, stdout=log, stderr=log, env=env,
                                      creationflags=subprocess.CREATE_NO_WINDOW)
            job.assign(worker)
            worker.stdin.write(b"\x01")
            worker.stdin.close()
            # Default and trusted-command viewers each exercise graceful and
            # abrupt shutdown while this desktop-owned graph stays alive.
            deadline = time.monotonic() + 180
            ready_path = directory / "ready.json"
            ready = wait_json(ready_path, worker, seconds=max(0.0, deadline - time.monotonic()))
            assert not ready["hidden"] and ready["view_graph_consistent"] and not ready["errors"], ready
            connection = Connection.local(ready["endpoint"])
            connection.handshake(label)
            report.update(ready=ready, owner_pid=connection.owner_pid, desktop_identity=identity(connection.owner_pid),
                          incarnation=connection.hello["instance_id"], baseline=connection.request(*PANES).decode())
            while hold.exists() and time.monotonic() < deadline:
                panes = connection.request(*PANES).decode()
                for line in panes.splitlines():
                    pid = int(line.split("|")[1])
                    if pid and pid not in handles:
                        handles[pid] = checked(K.OpenProcess(0x00100000, False, pid))
                report.update(status="active", sampled_at=time.time(), panes=panes,
                              graph=connection.request(*GRAPH).decode(),
                              captures={line.split("|")[0]: connection.request("capture-pane", "-p", "-t", line.split("|")[0]).decode()
                                        for line in panes.splitlines()},
                              live_pids=[pid for pid, handle in handles.items() if K.WaitForSingleObject(handle, 0) == 258])
                atomic_json(args.observer, report)
                time.sleep(0.15)
            hold.unlink(missing_ok=True)
            connection.close()
            connection = None
            worker.wait(timeout=15)
            assert worker.returncode == 0, "desktop GUI failed at exit"
            result = json.loads((directory / "result.json").read_text(encoding="utf-8"))
            assert result["view_graph_consistent"] and not result["errors"], result
            report.update(result=result, status="finished")
    except BaseException as error:
        report.update(status="failed", error=repr(error))
    finally:
        hold.unlink(missing_ok=True)
        if connection:
            connection.close()
        job.close()
        if worker:
            worker.wait(timeout=5)
        for pid, handle in handles.items():
            if K.WaitForSingleObject(handle, 5000) != 0:
                report["cleanup_remaining"].append(pid)
            K.CloseHandle(handle)
        report["cleanup_verified"] = not report["cleanup_remaining"]
        atomic_json(args.observer, report)
    return 0 if report["status"] == "finished" and report["cleanup_verified"] else 1


def exercise(args, report):
    observer = wait_observer(args, timeout=35)
    ready = observer["ready"]
    endpoint, incarnation = ready["endpoint"], observer["incarnation"]
    instance = bytes.fromhex(endpoint.rsplit("-", 1)[1]).decode("utf-8")
    report.update(ready=ready, desktop_identity=observer["desktop_identity"],
                  desktop_observer_identity=observer["observer_identity"], collector_identity=identity(),
                  ssh_identity=run_ssh_json(args, "--identity"), incarnation=incarnation,
                  candidate_sha256=hashlib.sha256(args.tmuxx.read_bytes()).hexdigest(), gui_sha256=observer["gui_sha256"])
    desktop, remote = report["desktop_identity"], report["ssh_identity"]
    assert desktop["account_sid"] == remote["account_sid"], "SSH must use the desktop account"
    assert desktop["logon_id"] != remote["logon_id"], "same token is not cross-logon evidence"
    assert desktop["session_id"] != remote["session_id"], "same session is not desktop-to-SSH evidence"
    assert observer["observer_identity"]["logon_id"] == desktop["logon_id"]
    connections, created_panes, viewer, trusted_viewer = [], [], None, None
    bridge_directory = args.observer.parent / "bridge 雪 &^ % !;"
    bridge_directory.mkdir()
    bridge_executable = bridge_directory / "tmuxx.exe"
    shutil.copy2(args.tmuxx, bridge_executable)
    assert hashlib.sha256(bridge_executable.read_bytes()).hexdigest() == report["candidate_sha256"]
    bridge = ps_source("& " + ps_quote(str(bridge_executable)) + " --native-stdio; exit $LASTEXITCODE")
    try:
        assert args.gui and args.viewer_key and args.viewer_bridge, "native SSH view needs --gui, --viewer-key and --viewer-bridge"
        assert args.rustty_viewer_key and args.rustty_bridge, "trusted Rustty view needs --rustty-viewer-key and --rustty-bridge"
        assert not remote_images(args, args.viewer_bridge), "previous forced-command viewer bridge is still running"
        args.viewer_bridge.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(args.tmuxx, args.viewer_bridge)
        assert hashlib.sha256(args.viewer_bridge.read_bytes()).hexdigest() == report["candidate_sha256"]
        assert not remote_images(args, args.rustty_bridge), "previous trusted-command Rustty bridge is still running"
        args.rustty_bridge.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(args.gui, args.rustty_bridge)
        assert hashlib.sha256(args.rustty_bridge.read_bytes()).hexdigest() == report["gui_sha256"]
        trusted_command = rustty_bridge_command(args.rustty_bridge)
        receipts_directory = args.rustty_viewer_key.parent / "dispatch-receipts"
        previous_receipts = set(receipts_directory.glob("*.json"))
        report["unauthorized_probe"] = denied_probe(args, report, endpoint, incarnation)
        report["tests"].append("windows_ssh::unauthorized_account_is_denied")
        local_probe = run_ssh_json(args, "--probe-pipe " + ps_quote(endpoint))
        assert local_probe == {"connected": True, "winerror": 0}, local_probe
        network_endpoint = endpoint.replace("\\\\.\\", "\\\\127.0.0.1\\", 1)
        network_probe = run_ssh_json(args, "--probe-pipe " + ps_quote(network_endpoint))
        report["network_probe"] = {"endpoint": network_endpoint, **network_probe}
        assert network_probe == {"connected": False, "winerror": 5}, "network pipe prerequisite or refusal failed: " + repr(network_probe)
        report["tests"].append("windows_ssh::direct_network_pipe_is_denied")
        baseline = observer["baseline"].encode()
        assert baseline, "desktop fixture must already own live panes"
        baseline_pids = sorted({int(line.split(b"|")[1]) for line in baseline.splitlines()})
        report["baseline_pids"] = baseline_pids
        for shell, command in [("powershell", bridge), ("cmd", "cmd.exe /d /s /c " + bridge)]:
            peer = Connection.ssh(options(args), command)
            connections.append(peer)
            peer.handshake(instance, incarnation)
            assert peer.request(*PANES) == baseline
            assert int(peer.request("display-message", "-p", "#{pid}").strip()) == observer["owner_pid"]
            report.setdefault("shell_forms", []).append(shell)
        ssh = connections[-1]
        viewer = NativeSshViewer(args, instance)
        assert viewer.ready["instance"] == incarnation and viewer.ready["endpoint"] is None, viewer.ready
        assert sorted(viewer.ready["pane_processes"].values()) == baseline_pids, viewer.ready
        bridge_identities = remote_images(args, args.viewer_bridge)
        assert bridge_identities, "actual native SSH viewer did not launch the pinned forced-command bridge"
        assert all(item["account_sid"] == desktop["account_sid"] and item["logon_id"] != desktop["logon_id"]
                   and item["session_id"] != desktop["session_id"] for item in bridge_identities), bridge_identities
        report["ssh_viewer"] = {"argv": viewer.argv, "directory": str(viewer.directory), "ready": viewer.ready,
                                "bridge_identities": bridge_identities,
                                "bridge_sha256": hashlib.sha256(args.viewer_bridge.read_bytes()).hexdigest()}
        trusted_viewer = NativeSshViewer(args, instance, "-trusted", key=args.rustty_viewer_key,
                                        command=trusted_command)
        assert trusted_viewer.ready["instance"] == incarnation and trusted_viewer.ready["endpoint"] is None
        assert sorted(trusted_viewer.ready["pane_processes"].values()) == baseline_pids
        report["trusted_rustty_viewer"] = {
            "argv": trusted_viewer.argv, "directory": str(trusted_viewer.directory), "ready": trusted_viewer.ready,
            **trusted_bridge_evidence(args, previous_receipts, trusted_command, desktop),
        }
        sid = ssh.request("list-sessions", "-F", "#{session_id}").splitlines()[0].decode("ascii")
        name = "SSH tab 雪 & %NAME% ! ^ ;"
        application = args.observer.parent / "pane 雪 &^ % !;.py"
        application.write_text("import os,sys\nprint('SSH_LIVE_PID='+str(os.getpid()),flush=True)\n"
                               "print('SSH_ENV='+os.environ['TMUXX_SSH_FIXTURE'],flush=True)\n"
                               "for line in sys.stdin: print('SSH_ECHO='+line.strip(),flush=True)\n", encoding="utf-8")
        ssh.request("set-environment", "-g", "TMUXX_SSH_FIXTURE", "雪 &|<>^%!;")
        created = ssh.request("new-window", "-d", "-P", "-F", "#{window_id}|#{pane_id}", "-t", sid + ":", "-n", name,
                              sys.executable, "-u", str(application)).decode("ascii").strip().split("|")
        window, pane = created
        created_panes.append(int(ssh.request("display-message", "-p", "-t", pane, "#{pane_pid}").strip()))
        ssh.request("link-window", "-s", window, "-t", sid + ":37")
        ssh.request("new-session", "-d", "-s", "SSH session 雪 & ;", "-t", sid)
        split = ssh.request("split-window", "-d", "-P", "-F", "#{pane_id}", "-t", pane,
                            sys.executable, "-u", str(application)).decode("ascii").strip()
        created_panes.append(int(ssh.request("display-message", "-p", "-t", split, "#{pane_pid}").strip()))
        marker = "雪 &|<>^%!"
        ssh.request("send-keys", "-t", pane, "-l", marker)
        ssh.request("send-keys", "-t", pane, "Enter")
        graph = ssh.request(*GRAPH).decode()
        assert sum("|" + window + "|" in line for line in graph.splitlines()) >= 4, graph
        observer = wait_observer(args, lambda item: item.get("graph") == graph
                                 and "SSH_ECHO=" + marker in item.get("captures", {}).get(pane, ""))
        capture = observer["captures"][pane]
        assert "SSH_ENV=雪 &|<>^%!;" in capture
        assert ssh.request("capture-pane", "-p", "-t", pane).decode() == capture
        assert sorted(observer["live_pids"]) == sorted(baseline_pids + created_panes)
        _, checkpoint = ssh.request("list-panes", "-a", "-F", "#{pane_id}|#{pane_pid}", subscribe=True)
        report.update(shared_graph=graph, live_capture=capture, new_pane_pids=created_panes, window_id=window, pane_id=pane,
                      snapshot_sha256=hashlib.sha256(json.dumps(checkpoint, sort_keys=True).encode()).hexdigest())
        report["tests"].append("windows_ssh::desktop_cross_logon_shared_live_graph")
        # Release only the second GUI; its final screenshots must include every
        # externally created native session, shared tab, duplicate link and split.
        atomic_json(viewer.directory / "expected.json", {"sessions": 2, "windows": 2, "panes": 4, "links": 6})
        view_result = viewer.close()
        report["ssh_viewer"]["result"] = view_result
        assert view_result["instance"] == incarnation and view_result["endpoint"] is None, view_result
        assert sorted(view_result["pane_processes"].values()) == sorted(baseline_pids + created_panes), view_result
        assert view_result["windows"] == 2 and len(view_result["rendered_hosts"]) == 2, view_result
        assert sum(len(session["windows"]) for session in view_result["server_sessions"]) == 6, view_result
        assert len(view_result["view_panes"]) == 12, view_result
        assert viewer.ready["pane_processes"].items() <= view_result["pane_processes"].items(), view_result
        released = time.time()
        observer = wait_observer(args, lambda item: item.get("sampled_at", 0) > released and item.get("graph") == graph)
        assert sorted(observer["live_pids"]) == sorted(baseline_pids + created_panes)
        assert ssh.request(*GRAPH).decode() == graph
        report["tests"].append("windows_ssh::second_rustty_native_viewer_reconciles_without_replacement")
        # This configured bridge viewer has stayed attached throughout the same
        # external graph mutations as the default-command viewer.
        trusted_evidence = trusted_bridge_evidence(args, previous_receipts, trusted_command, desktop, connections=3)
        before_denial = set(receipts_directory.glob("*.json"))
        denied_command = trusted_command + " deliberately-rejected"
        rejected = subprocess.run(options(args, args.rustty_viewer_key) + [denied_command],
                                  stdin=subprocess.DEVNULL, capture_output=True, timeout=10,
                                  creationflags=subprocess.CREATE_NO_WINDOW)
        assert rejected.returncode != 0, "forced dispatcher accepted a different remote command"
        denied_receipts = dispatch_receipts(receipts_directory, before_denial, trusted_command,
                                           args.rustty_bridge, accepted=False)
        assert all(item["original_command_sha256"] == hashlib.sha256(denied_command.encode("utf-8")).hexdigest()
                   for item in denied_receipts), denied_receipts
        assert sorted(item["pid"] for item in remote_images(args, args.rustty_bridge)) == sorted(
            item["pid"] for item in trusted_evidence["bridge_identities"])
        assert ssh.request(*GRAPH).decode() == graph
        assert int(ssh.request("display-message", "-p", "#{pid}").strip()) == observer["owner_pid"]
        atomic_json(trusted_viewer.directory / "expected.json", {"sessions": 2, "windows": 2, "panes": 4, "links": 6})
        trusted_result = trusted_viewer.close()
        assert trusted_result["instance"] == incarnation and trusted_result["endpoint"] is None
        assert sorted(trusted_result["pane_processes"].values()) == sorted(baseline_pids + created_panes)
        assert trusted_result["windows"] == 2 and len(trusted_result["rendered_hosts"]) == 2
        assert sum(len(session["windows"]) for session in trusted_result["server_sessions"]) == 6
        assert len(trusted_result["view_panes"]) == 12
        report["trusted_rustty_viewer"].update(trusted_evidence, result=trusted_result,
                                              denied_command_receipts=denied_receipts,
                                              denied_command_returncode=rejected.returncode)
        report["tests"].append("windows_ssh::trusted_rustty_command_preserves_shared_native_views")
        viewer = NativeSshViewer(args, instance, "-abrupt")
        assert viewer.ready["instance"] == incarnation, viewer.ready
        assert sorted(viewer.ready["pane_processes"].values()) == sorted(baseline_pids + created_panes), viewer.ready
        abrupt = viewer.abrupt()
        deadline = time.monotonic() + 5
        while (remaining := remote_images(args, args.viewer_bridge)) and time.monotonic() < deadline:
            time.sleep(0.05)
        assert not remaining, "remote bridges survived abrupt SSH GUI death: " + repr(remaining)
        abrupt["remote_bridges_remaining"] = remaining
        report["ssh_viewer_abrupt"] = abrupt
        killed = time.time()
        observer = wait_observer(args, lambda item: item.get("sampled_at", 0) > killed and item.get("graph") == graph)
        assert sorted(observer["live_pids"]) == sorted(baseline_pids + created_panes)
        assert ssh.request(*GRAPH).decode() == graph
        viewer.close(verify=False)
        report["tests"].append("windows_ssh::abrupt_ssh_viewer_reclaims_helpers_and_preserves_owner")
        previous_receipts = set(receipts_directory.glob("*.json"))
        trusted_viewer = NativeSshViewer(args, instance, "-trusted-abrupt", key=args.rustty_viewer_key,
                                        command=trusted_command)
        assert trusted_viewer.ready["instance"] == incarnation
        assert sorted(trusted_viewer.ready["pane_processes"].values()) == sorted(baseline_pids + created_panes)
        trusted_evidence = trusted_bridge_evidence(args, previous_receipts, trusted_command, desktop, connections=3)
        abrupt = trusted_viewer.abrupt()
        deadline = time.monotonic() + 5
        while (remaining := remote_images(args, args.rustty_bridge)) and time.monotonic() < deadline:
            time.sleep(0.05)
        assert not remaining, "configured Rustty bridge survived abrupt SSH GUI death: " + repr(remaining)
        abrupt.update(trusted_evidence, remote_bridges_remaining=remaining)
        report["trusted_rustty_viewer_abrupt"] = abrupt
        killed = time.time()
        observer = wait_observer(args, lambda item: item.get("sampled_at", 0) > killed and item.get("graph") == graph)
        assert sorted(observer["live_pids"]) == sorted(baseline_pids + created_panes)
        assert ssh.request(*GRAPH).decode() == graph
        trusted_viewer.close(verify=False)
        report["tests"].append("windows_ssh::trusted_rustty_command_abrupt_cleanup_preserves_owner")
        for peer in connections:
            peer.close()
        connections = []
        disconnected = time.time()
        observer = wait_observer(args, lambda item: item.get("sampled_at", 0) > disconnected and item.get("graph") == graph)
        assert sorted(observer["live_pids"]) == sorted(baseline_pids + created_panes)
        reconnect = Connection.ssh(options(args), bridge)
        connections.append(reconnect)
        reconnect.handshake(instance, incarnation)
        assert reconnect.request(*GRAPH).decode() == graph
        reconnect_capture = reconnect.request("capture-pane", "-p", "-t", pane).decode()
        assert "SSH_ECHO=" + marker in reconnect_capture, reconnect_capture
        # Native window creation may legitimately resize the shared grid. Both
        # viewers must converge on the same resulting capture, including blanks.
        observer = wait_observer(args, lambda item: item.get("captures", {}).get(pane) == reconnect_capture)
        report["tests"].append("windows_ssh::disconnect_reconnect_preserves_owner_jobs")
        incompatible = Connection.ssh(options(args), bridge)
        connections.append(incompatible)
        incompatible.write({"version": 999, "instance": instance})
        try:
            incompatible.record(timeout=5)
            raise AssertionError("bridge accepted incompatible protocol")
        except EOFError:
            pass
        assert reconnect.request(*GRAPH).decode() == graph
        report["tests"].append("windows_ssh::fixed_shell_forms_fragmentation_and_version_rejection")
        report["owner_alive_before_release"] = True
    finally:
        report["created_pane_pids"] = created_panes
        for active_viewer in (trusted_viewer, viewer):
            if not active_viewer:
                continue
            try:
                active_viewer.close(verify=False)
            except Exception as error:
                report["failures"].append("SSH viewer cleanup: " + repr(error))
        for peer in reversed(connections):
            try:
                peer.close()
            except Exception as error:
                report["failures"].append("SSH connection cleanup: " + repr(error))
        for field, path in [("bridge_processes_remaining", bridge_executable),
                            ("ssh_viewer_bridges_remaining", args.viewer_bridge),
                            ("trusted_rustty_bridges_remaining", args.rustty_bridge)]:
            if path is None:
                continue
            try:
                deadline = time.monotonic() + 5
                while (remaining := remote_images(args, path)) and time.monotonic() < deadline:
                    time.sleep(0.05)
                report[field] = [item["pid"] for item in remaining]
                report["cleanup_remaining"].extend(item["pid"] for item in remaining)
            except Exception as error:
                report["failures"].append(field + " audit failed: " + repr(error))
        (args.observer.parent / "hold").unlink(missing_ok=True)
        report["gui_hold_released"] = True
        try:
            final = wait_observer(args, lambda item: item.get("status") == "finished", timeout=25)
            report["desktop_final"] = final
            report["cleanup_remaining"].extend(final["cleanup_remaining"])
            assert final["cleanup_verified"]
            result = final["result"]
            assert result["view_graph_consistent"] and not result["errors"]
            if "window_id" in report:
                assert result["windows"] >= 2 and len(result["pane_processes"]) >= 4, result
                assert len(result["rendered_hosts"]) >= 2, result
            report["tests"].append("windows_ssh::desktop_reconciles_and_owned_processes_cleanup")
        except Exception as error:
            report["failures"].append("desktop cleanup/capture: " + repr(error))
            if args.observer.is_file():
                final = json.loads(args.observer.read_text(encoding="utf-8"))
                report["desktop_final"] = final
                report["cleanup_remaining"].extend(final.get("cleanup_remaining", []))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--identity", action="store_true")
    parser.add_argument("--probe-pipe")
    parser.add_argument("--process-image", type=Path)
    parser.add_argument("--descendants", type=int)
    parser.add_argument("--desktop-observer", action="store_true")
    parser.add_argument("--deny-helper", type=Path)
    parser.add_argument("--denial-directory", type=Path)
    parser.add_argument("--observer", type=Path)
    parser.add_argument("--gui", type=Path)
    parser.add_argument("--tmuxx", type=Path)
    parser.add_argument("--key", type=Path)
    parser.add_argument("--viewer-key", type=Path, help="private loopback-only forced-command bridge credential")
    parser.add_argument("--viewer-bridge", type=Path, help="exact forced-command executable path reserved for this fixture")
    parser.add_argument("--rustty-viewer-key", type=Path, help="private loopback-only trusted Rustty dispatcher credential")
    parser.add_argument("--rustty-bridge", type=Path, help="exact Rustty executable path reserved for the trusted dispatcher")
    parser.add_argument("--known-hosts", type=Path)
    parser.add_argument("--host", help="explicit desktop-account@SSH-host for the collector")
    parser.add_argument("--ssh", default=str(Path(os.environ["SystemRoot"]) / "System32/OpenSSH/ssh.exe"))
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    if args.identity:
        print(json.dumps(identity()))
        return 0
    if args.probe_pipe:
        print(json.dumps(raw_pipe_probe(args.probe_pipe)))
        return 0
    if args.process_image:
        print(json.dumps(identities_for_image(args.process_image)))
        return 0
    if args.descendants:
        print(json.dumps(descendants(args.descendants)))
        return 0
    if args.deny_helper:
        return denial_helper(args.deny_helper)
    if args.desktop_observer:
        return desktop_observer(args)
    if not args.host:
        parser.error("the SSH collector requires --host desktop-account@host")
    report = {"schema": 1, "kind": "windows-ssh", "platform": sys.platform, "tests": [],
              "failures": [], "cleanup_remaining": [], "passed": False}
    observer_process = None
    try:
        if args.gui:
            if args.observer.exists():
                raise AssertionError("observer path must be fresh for a new desktop owner")
            observer_process = subprocess.Popen([sys.executable, str(Path(__file__).resolve()), "--desktop-observer",
                                                 "--gui", str(args.gui), "--observer", str(args.observer)],
                                                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                                creationflags=subprocess.CREATE_NO_WINDOW)
        exercise(args, report)
    except (Exception, KeyboardInterrupt) as error:
        report["failures"].append(f"{type(error).__name__}: {error}")
        if args.observer:
            (args.observer.parent / "hold").unlink(missing_ok=True)
    finally:
        if observer_process:
            (args.observer.parent / "hold").unlink(missing_ok=True)
            try:
                _, stderr = observer_process.communicate(timeout=25)
            except subprocess.TimeoutExpired:
                observer_process.kill()
                _, stderr = observer_process.communicate(timeout=5)
                report["failures"].append("desktop observer did not terminate within cleanup timeout")
            if observer_process.returncode:
                report["failures"].append("desktop observer failed: " + stderr.decode("utf-8", "replace"))
        report["passed"] = not report["failures"] and not report["cleanup_remaining"]
        if args.report:
            args.report.parent.mkdir(parents=True, exist_ok=True)
            atomic_json(args.report, report)
        print(json.dumps(report, ensure_ascii=True, separators=(",", ":")))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
