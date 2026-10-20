"""Native slow/unread pipe counterparts of the two upstream exit scripts."""
from __future__ import annotations

import base64
import ctypes
from ctypes import wintypes as w
import hashlib
import msvcrt
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time


def run(candidate, shell, script):
    from windows_upstream import Owners
    from runner import WindowsSuiteJob
    digest = hashlib.sha256(candidate.read_bytes()).hexdigest()
    source_digest = hashlib.sha256(script.read_bytes()).hexdigest()
    with tempfile.TemporaryDirectory(prefix="windows-control-exit-") as temporary:
        directory = Path(temporary)
        executable = directory / "tmuxx.exe"
        shutil.copyfile(candidate, executable)
        owners = Owners(executable, shell, directory)
        owners.env.setdefault("TMUXX_CONPTY_RESOURCES", str(candidate.parent / "resources"))
        client_job, client, report = WindowsSuiteJob(), None, None
        try:
            selected = owners.request({"token":owners.token,"label":"control-exit","operation":"select","start":True})
            args = [str(executable), "-L", selected["instance"]]
            def command(*words):
                value = subprocess.run([*args,*words],env=owners.env,stdin=subprocess.DEVNULL,capture_output=True,timeout=10)
                assert value.returncode == 0 and not value.stderr, (words,value)
                return value.stdout
            # Sustain ten times the nominal 2.5 KiB/s reader rate. The exact
            # live-client check below establishes that no independent eviction
            # has occurred before the tested action. This real ConPTY application
            # keeps producing until owner cleanup.
            command("new", "-d", "-x", "80", "-y", "24", "-s", "rt", sys.executable, "-u", "-c",
                    "import os,time\nwhile True:\n os.write(1,b'x'*512)\n time.sleep(.02)")
            command("setw", "-g", "automatic-rename", "off")
            owner_pid = int(command("display", "-pt", "rt", "#{pid}"))
            owner = owners.owners["control-exit"]
            assert owner_pid == owner.pid
            pane_pid = int(command("display", "-pt", "rt", "#{pane_pid}"))
            producer_deadline = time.monotonic() + 8
            while b"x" * 80 not in command("capture-pane", "-p", "-t", "rt:0"):
                assert owner.poll() is None and time.monotonic() < producer_deadline, "real ConPTY producer did not fill a terminal row before attachment"
                time.sleep(.02)
            client = subprocess.Popen([*args,"-C","attach","-t","rt"],env=owners.env,
                                      stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,
                                      creationflags=subprocess.CREATE_NO_WINDOW)
            client_job.assign(client)
            deadline = time.monotonic() + 8
            while not command("lsc", "-F", "#{client_name}").strip():
                assert client.poll() is None and time.monotonic() < deadline, "native control client did not attach"
                time.sleep(.02)
            client_identity = command("lsc", "-F", "#{client_name}|#{client_pid}")
            assert len(client_identity.splitlines()) == 1
            assert client_identity.rstrip().endswith(f"|{client.pid}".encode()), client_identity
            kernel = ctypes.WinDLL("kernel32",use_last_error=True)
            kernel.PeekNamedPipe.argtypes=[w.HANDLE,w.LPVOID,w.DWORD,w.LPVOID,ctypes.POINTER(w.DWORD),w.LPVOID]
            kernel.PeekNamedPipe.restype=w.BOOL
            handle = msvcrt.get_osfhandle(client.stdout.fileno())
            pipe_eof = False
            def read(limit):
                nonlocal pipe_eof
                available=w.DWORD()
                if not kernel.PeekNamedPipe(handle,None,0,None,ctypes.byref(available),None):
                    assert ctypes.get_last_error() in (109,232), ctypes.WinError(ctypes.get_last_error())
                    pipe_eof = True
                    return b""
                value = os.read(client.stdout.fileno(),min(limit,available.value)) if available.value else b""
                if available.value and limit and not value:
                    pipe_eof = True
                return value
            def assert_backlogged_client():
                assert client.poll() is None, "control client exited before the tested action"
                current_identity = command("lsc", "-F", "#{client_name}|#{client_pid}")
                assert current_identity == client_identity, ("control client was replaced or evicted before the tested action", client_identity, current_identity)
                assert b"x" * 80 in command("capture-pane", "-p", "-t", "rt:0"), "real ConPTY producer has not filled a terminal row"
                available = w.DWORD()
                assert kernel.PeekNamedPipe(handle,None,0,None,ctypes.byref(available),None), ctypes.WinError(ctypes.get_last_error())
                assert available.value > 0, "client has no unread pipe output before the tested action"
                return available.value
            if script.name == "control-client-exit-stalled.sh":
                # No read at all after attachment: preserve actual OS-pipe
                # backpressure until the owner has completed its exit.
                time.sleep(2)
                unread_bytes = assert_backlogged_client()
                start=time.monotonic();command("kill-server")
                owner.wait(timeout=20)
                elapsed=time.monotonic()-start
                assert elapsed < 20
                report={"returncode":0,"stdout":"","stderr":"","unread_output":True,"owner_exit_seconds":elapsed,
                        "unread_pipe_bytes_before_action":unread_bytes}
            else:
                transcript=bytearray()
                deadline=time.monotonic()+15
                while len(transcript)<8000:
                    transcript.extend(read(256))
                    assert time.monotonic()<deadline, ("client output did not back up",len(transcript))
                    time.sleep(.1)
                slow_bytes=len(transcript)
                unread_bytes = assert_backlogged_client()
                command("detach-client", "-s", "rt")
                for index in range(5):
                    command("rename-window", "-t", "rt:0", f"exitmarker{index}")
                time.sleep(.5)
                deadline=time.monotonic()+3;drained=0
                while time.monotonic()<deadline and not pipe_eof:
                    value=read(min(4096,4096*128-drained))
                    transcript.extend(value);drained+=len(value)
                    if not value:
                        time.sleep(.005)
                assert pipe_eof, ("control stdout did not reach EOF within original three-second/512KiB drain bounds", drained)
                remaining = deadline - time.monotonic()
                assert remaining > 0, "control exit exceeded original three-second drain bound"
                client_status = client.wait(timeout=remaining)
                exit_lines = [line for line in transcript.splitlines() if line.startswith(b"%exit")]
                assert exit_lines == [b"%exit"], ("expected exactly one normal control exit", exit_lines)
                assert transcript.endswith(b"%exit\n"), "normal control exit must terminate the complete stream"
                assert client_status == 0, ("control client did not exit successfully", client_status)
                assert not client.stdin.closed, "fixture stdin must remain held open through control exit"
                exit_drain_seconds = 3 - (deadline - time.monotonic())
                assert not any(line.startswith(b"%window-renamed ") and b"exitmarker" in line for line in transcript.splitlines()), transcript
                assert owner.poll() is None and int(command("display", "-pt", "rt", "#{pane_pid}")) == pane_pid
                report={"returncode":0,"stdout":"","stderr":"","slow_drained_bytes":slow_bytes,
                        "fast_drained_bytes":drained,"control_transcript":base64.b64encode(transcript).decode(),
                        "unread_pipe_bytes_before_action":unread_bytes,
                        "stdout_eof":pipe_eof,"normal_exit_records":len(exit_lines),
                        "client_returncode":client_status,"stdin_held_open":not client.stdin.closed,
                        "exit_drain_seconds":exit_drain_seconds}
            assert hashlib.sha256(executable.read_bytes()).hexdigest()==digest
            assert hashlib.sha256(script.read_bytes()).hexdigest()==source_digest
            report.update(candidate_sha256=digest,script_sha256=source_digest,owner_pid=owner_pid,
                          pane_pid=pane_pid,control_client_pid=client.pid,fixture_transport="native anonymous pipes")
            return report
        finally:
            if client:
                if client.poll() is None:
                    client.kill()
                client.wait(timeout=5)
                for stream in (client.stdin,client.stdout,client.stderr):
                    stream.close()
            client_job.close()
            cleanup=owners.cleanup()
            if report is not None:
                report.update(cleanup)
