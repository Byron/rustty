"""Native console and named-pipe counterparts of every buffers.sh vector."""
from __future__ import annotations

import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time


def run(candidate, shell, script):
    from windows_upstream import Owners
    digest = hashlib.sha256(candidate.read_bytes()).hexdigest()
    source_digest = hashlib.sha256(script.read_bytes()).hexdigest()
    with tempfile.TemporaryDirectory(prefix="windows-buffers-") as temporary:
        directory = Path(temporary)
        executable = directory / "tmuxx.exe"
        shutil.copyfile(candidate, executable)
        owners = Owners(executable, shell, directory)
        owners.env.setdefault("TMUXX_CONPTY_RESOURCES", str(candidate.parent / "resources"))
        report, checks = None, []
        try:
            selected = owners.request({"token": owners.token, "label": "buffers", "operation": "select", "start": True})
            def invoke(*args, data=None):
                return subprocess.run([str(executable), "-L", selected["instance"], *map(str, args)],
                                      env=owners.env, input=data, capture_output=True, timeout=15)
            def ok(*args, data=None):
                value = invoke(*args, data=data)
                assert value.returncode == 0 and not value.stderr, (args, value.returncode, value.stderr)
                return value.stdout
            def fail(expected, *args):
                value = invoke(*args)
                assert value.returncode != 0 and not value.stdout, (args, value)
                assert value.stderr == expected.encode() + b"\n", (args, value.stderr, expected)
            def buffers(expected):
                value = ok("list-buffers", "-F", "#{buffer_name}=#{buffer_sample}")
                assert b" ".join(value.split()) == expected.encode(), value
            def show(expected, name=None):
                assert ok("show-buffer", *(["-b", name] if name else [])).rstrip(b"\n") == expected.encode()

            ok("new-session", "-d", "-s", "B", "-x", "80", "-y", "24")
            ok("set-buffer", "one"); ok("set-buffer", "two")
            buffers("buffer1=two buffer0=one"); show("two"); show("one", "buffer0")
            ok("set-buffer", "-a", "!"); buffers("buffer2=! buffer1=two buffer0=one")
            ok("delete-buffer"); ok("set-buffer", ""); buffers("buffer1=two buffer0=one")
            ok("set-buffer", "-b", "named", "abc"); buffers("named=abc buffer1=two buffer0=one")
            ok("set-buffer", "-b", "named", "xyz"); ok("set-buffer", "-a", "-b", "named", "123")
            buffers("named=xyz123 buffer1=two buffer0=one"); show("xyz123", "named")
            ok("set-buffer", "-b", "named", "-n", "other"); buffers("other=xyz123 buffer1=two buffer0=one")
            fail("unknown buffer: nosuch", "set-buffer", "-b", "nosuch", "-n", "foo")
            fail("no data specified", "set-buffer", "-b", "other")
            fail("no buffer nosuch", "show-buffer", "-b", "nosuch")
            assert ok("list-buffers", "-f", "#{==:#{buffer_name},other}", "-F", "#{buffer_name}") == b"other\n"
            ok("delete-buffer", "-b", "buffer1"); buffers("other=xyz123 buffer0=one")
            fail("unknown buffer: buffer1", "delete-buffer", "-b", "buffer1")
            ok("delete-buffer"); buffers("other=xyz123")
            fail("no buffers", "show-buffer"); fail("no buffer", "delete-buffer")
            ok("delete-buffer", "-b", "other"); fail("no buffers", "show-buffer")
            checks.append("automatic-named-append-rename-delete-formats-filter-errors")

            ok("set-option", "-g", "buffer-limit", "3"); ok("set-buffer", "-b", "keepme", "precious")
            for value in ("a1", "a2", "a3", "a4"):
                ok("set-buffer", value)
            assert b" ".join(ok("list-buffers", "-F", "#{buffer_sample}").split()) == b"a4 a3 a2 precious"
            ok("set-option", "-g", "buffer-limit", "50"); ok("delete-buffer", "-b", "keepme")
            for _ in range(3):
                ok("delete-buffer")
            checks.append("automatic-limit-preserves-named-buffer")

            # Unix stty cannot configure a Windows console. This application
            # requests raw VT input and explicitly renders cat -v's CR form.
            app = directory / "raw_cat.py"
            app.write_text(r'''
import ctypes,msvcrt,os,pathlib,sys
k=ctypes.WinDLL('kernel32',use_last_error=True)
k.GetStdHandle.restype=ctypes.c_void_p
k.SetConsoleMode.argtypes=[ctypes.c_void_p,ctypes.c_ulong]
assert k.SetConsoleCP(65001) and k.SetConsoleOutputCP(65001)
assert k.SetConsoleMode(k.GetStdHandle(-10),0x200)
assert k.SetConsoleMode(k.GetStdHandle(-11),0xD)
msvcrt.setmode(0,os.O_BINARY); msvcrt.setmode(1,os.O_BINARY)
ready=pathlib.Path(sys.argv[1]); pending=ready.with_name(ready.name+'.tmp')
pending.write_text(str(os.getpid())); pending.replace(ready)
while True:
 data=os.read(0,1)
 if not data: break
 byte=data[0]
 os.write(1,bytes((94,byte+64)) if byte<32 and byte not in (9,10) else data)
''', encoding="utf-8")
            paste_pids = []
            for index, (data, flags, expected) in enumerate([
                ("one\ntwo", [], b"one^Mtwo"), ("one\ntwo", ["-r"], b"one"),
                ("one\ntwo", ["-s", "|"], b"one|two"),
                ("one\ntwo", ["-s", "XX"], b"oneXXtwo"), ("gone", ["-d"], b"gone"),
            ]):
                if index:
                    ok("kill-window", "-t", "B:9")
                ready = directory / f"raw-cat-{index}.ready"
                ok("new-window", "-d", "-t", "B:9", sys.executable, "-u", app, ready)
                deadline = time.monotonic() + 8
                while not ready.exists():
                    assert time.monotonic() < deadline, "native raw console application did not start"
                    time.sleep(.02)
                pane_pid = int(ok("display-message", "-p", "-t", "B:9.0", "#{pane_pid}"))
                assert pane_pid == int(ready.read_text()), "paste target is not the actual console application"
                paste_pids.append(pane_pid)
                ok("set-buffer", "-b", "paste", data)
                ok("paste-buffer", *flags, "-b", "paste", "-t", "B:9.0")
                while True:
                    line = ok("capture-pane", "-p", "-t", "B:9.0").split(b"\n")[0]
                    if line == expected:
                        break
                    assert time.monotonic() < deadline, (data, flags, line, expected)
                    time.sleep(.02)
            fail("no buffer paste", "show-buffer", "-b", "paste")
            fail("no buffer nosuch", "paste-buffer", "-b", "nosuch", "-t", "B:9.0")
            ok("kill-window", "-t", "B:9")
            checks.append("actual-conpty-translated-raw-separator-delete-paste")

            first, second = directory / "input é.bin", directory / "output Ω.bin"
            payload = b"line1\tx\x1b[31m\x01\x02\xc3\xa9\n"
            first.write_bytes(payload)
            ok("load-buffer", "-b", "file", first); ok("save-buffer", "-b", "file", second)
            assert second.read_bytes() == payload
            ok("save-buffer", "-a", "-b", "file", second)
            assert second.read_bytes() == payload * 2
            ok("delete-buffer", "-b", "file")
            missing = str(first) + ".nosuch"
            fail("No such file or directory: " + missing, "load-buffer", "-b", "x", missing)
            fail("no buffer nosuch", "save-buffer", "-b", "nosuch", second)
            ok("set-buffer", "-b", "sb", "data")
            assert invoke("save-buffer", "-b", "sb", directory / "nonexistent/dir/file").returncode != 0
            checks.append("binary-unicode-file-roundtrip-append-exact-errors")

            # The supported native stream resource is stdout. Its anonymous
            # pipe is open successfully, then its reader consumes exactly eight
            # bytes and closes while the 100000-byte write is still pending.
            # Named-pipe pathnames are explicitly rejected by native file policy.
            first.write_bytes(bytes(100000)); ok("load-buffer", "-b", "write-error", first)
            from runner import WindowsSuiteJob
            writer_job = WindowsSuiteJob()
            writer = subprocess.Popen([str(executable), "-L", selected["instance"],
                                       "save-buffer", "-b", "write-error", "-"],
                                      env=owners.env, stdin=subprocess.DEVNULL,
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0,
                                      creationflags=subprocess.CREATE_NO_WINDOW)
            received = []
            try:
                writer_job.assign(writer)
                import ctypes
                from ctypes import wintypes as w
                import msvcrt
                kernel = ctypes.WinDLL("kernel32", use_last_error=True)
                kernel.PeekNamedPipe.argtypes = [w.HANDLE, w.LPVOID, w.DWORD, w.LPVOID, ctypes.POINTER(w.DWORD), w.LPVOID]
                kernel.PeekNamedPipe.restype = w.BOOL
                available = w.DWORD()
                handle = msvcrt.get_osfhandle(writer.stdout.fileno())
                deadline = time.monotonic() + 8
                while True:
                    assert kernel.PeekNamedPipe(handle, None, 0, None, ctypes.byref(available), None), ctypes.WinError(ctypes.get_last_error())
                    if available.value >= 8:
                        break
                    assert writer.poll() is None and time.monotonic() < deadline, "buffer writer produced no bytes"
                    time.sleep(.005)
                received.append(os.read(writer.stdout.fileno(), 8))
                writer.stdout.close()
                assert writer.wait(timeout=10) != 0, "save-buffer after partial stdout write succeeded"
                assert received == [bytes(8)], received
                assert writer.stderr.read(), "failed write produced no diagnostic"
            finally:
                if writer.poll() is None:
                    writer.kill()
                writer.wait(timeout=5)
                writer.stdout.close(); writer.stderr.close(); writer_job.close()
            ok("delete-buffer", "-b", "write-error")
            assert ok("save-buffer", "-b", "sb", "-") == b"data"
            ok("delete-buffer", "-b", "sb")
            ok("load-buffer", "-b", "stdinbuf", "-", data=b"from stdin"); show("from stdin", "stdinbuf")
            ok("delete-buffer", "-b", "stdinbuf")
            assert ok("display-message", "-p", "alive") == b"alive\n"
            checks.append("partial-stdout-pipe-write-failure-and-stdio-roundtrip")
            assert hashlib.sha256(executable.read_bytes()).hexdigest() == digest
            assert hashlib.sha256(script.read_bytes()).hexdigest() == source_digest
            report={"returncode":0,"stdout":"","stderr":"","checks":checks,
                    "paste_application_pids":paste_pids,"partial_write_bytes":len(received[0]),
                    "candidate_sha256":digest,"script_sha256":source_digest}
            return report
        finally:
            cleanup=owners.cleanup()
            if report is not None:
                report.update(cleanup)
