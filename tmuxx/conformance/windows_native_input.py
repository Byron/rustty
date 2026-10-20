#!/usr/bin/env python3
"""Deliver native HWND input into real ConPTY panes; actual IME composition is separate."""
from __future__ import annotations
import argparse
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
import uuid
from windows_native_views import Fixture, sha256, wait_json


def until(predicate, message, timeout=5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(.02)
    raise AssertionError(message)


class Clipboard:
    """Require an empty clipboard and restore only this fixture's own data.

    Preexisting or concurrent user data is never replaced or read. Clipboard
    testing belongs on an explicitly prepared private desktop.
    """
    def __init__(self, user, kernel, owner):
        self.u, self.k, self.owner = user, kernel, owner
        self.saved = []
        self.sequence = None
        for name, args, result in [
            ("OpenClipboard", [wintypes.HWND], wintypes.BOOL),
            ("EmptyClipboard", [], wintypes.BOOL),
            ("CloseClipboard", [], wintypes.BOOL),
            ("EnumClipboardFormats", [wintypes.UINT], wintypes.UINT),
            ("GetClipboardData", [wintypes.UINT], wintypes.HANDLE),
            ("SetClipboardData", [wintypes.UINT, wintypes.HANDLE], wintypes.HANDLE),
            ("GetClipboardSequenceNumber", [], wintypes.DWORD),
            ("GetClipboardOwner", [], wintypes.HWND),
        ]:
            getattr(user, name).argtypes, getattr(user, name).restype = args, result
        for name, args, result in [
            ("GlobalSize", [wintypes.HGLOBAL], ctypes.c_size_t),
            ("GlobalAlloc", [wintypes.UINT, ctypes.c_size_t], wintypes.HGLOBAL),
            ("GlobalLock", [wintypes.HGLOBAL], ctypes.c_void_p),
            ("GlobalUnlock", [wintypes.HGLOBAL], wintypes.BOOL),
            ("GlobalFree", [wintypes.HGLOBAL], wintypes.HGLOBAL),
        ]:
            getattr(kernel, name).argtypes, getattr(kernel, name).restype = args, result

    def open(self):
        until(lambda: self.u.OpenClipboard(self.owner), "clipboard is unavailable")

    def put(self, format_id, data):
        allocation = self.k.GlobalAlloc(2, len(data))
        if not allocation:
            raise ctypes.WinError(ctypes.get_last_error())
        pointer = self.k.GlobalLock(allocation)
        if not pointer:
            self.k.GlobalFree(allocation)
            raise ctypes.WinError(ctypes.get_last_error())
        ctypes.memmove(pointer, data, len(data))
        self.k.GlobalUnlock(allocation)
        if not self.u.SetClipboardData(format_id, allocation):
            self.k.GlobalFree(allocation)
            raise ctypes.WinError(ctypes.get_last_error())

    def __enter__(self):
        self.open()
        try:
            if self.u.EnumClipboardFormats(0):
                raise RuntimeError("clipboard prerequisite: prepare an empty private desktop clipboard; existing data was preserved")
        finally:
            self.u.CloseClipboard()
        return self

    def text(self, value):
        self.open()
        try:
            if self.u.EnumClipboardFormats(0):
                raise RuntimeError("clipboard changed before fixture mutation; existing data was preserved")
            assert self.u.EmptyClipboard()
            self.fixture_bytes = (value + "\0").encode("utf-16-le")
            self.put(13, self.fixture_bytes)
        finally:
            self.u.CloseClipboard()
        self.sequence = self.u.GetClipboardSequenceNumber()

    def fixture_contents_match(self):
        formats, format_id = [], 0
        while True:
            format_id = self.u.EnumClipboardFormats(format_id)
            if not format_id:
                break
            formats.append(format_id)
        if 13 not in formats or not set(formats).issubset({1, 7, 13, 16}):
            return False
        handle = self.u.GetClipboardData(13)
        length = self.k.GlobalSize(handle)
        if not handle or length != len(self.fixture_bytes):
            return False
        pointer = self.k.GlobalLock(handle)
        if not pointer:
            return False
        try:
            return ctypes.string_at(pointer, length) == self.fixture_bytes
        finally:
            self.k.GlobalUnlock(handle)

    def __exit__(self, *unused):
        if self.sequence is None:
            return
        self.open()
        try:
            if ((self.u.GetClipboardSequenceNumber() != self.sequence
                    and self.u.GetClipboardOwner() != self.owner)
                    or not self.fixture_contents_match()):
                raise RuntimeError("clipboard changed concurrently; preserved the new user content instead of restoring over it")
            assert self.u.EmptyClipboard()
            for format_id, data in self.saved:
                self.put(format_id, data)
        finally:
            self.u.CloseClipboard()
            self.saved.clear()


def exercise(fixture):
    directory = fixture.directory
    instance = "native-input-" + uuid.uuid4().hex
    owner = fixture.start([str(fixture.candidate), "-D", "-L", instance], "owner")
    deadline = time.monotonic() + 15
    while True:
        result = subprocess.run([str(fixture.candidate), "-L", instance, "list-sessions"],
                                capture_output=True, env=fixture.env, timeout=3)
        if result.returncode == 0:
            break
        assert owner.poll() is None and time.monotonic() < deadline, "owner did not start"
        time.sleep(.05)
    script = directory / "record.py"
    script.write_text("""import ctypes,msvcrt,os,sys
k=ctypes.WinDLL('kernel32',use_last_error=True)
k.GetStdHandle.restype=ctypes.c_void_p
k.SetConsoleMode.argtypes=[ctypes.c_void_p,ctypes.c_ulong]
assert k.SetConsoleCP(65001) and k.SetConsoleOutputCP(65001)
assert k.SetConsoleMode(k.GetStdHandle(-10),0x200)
assert k.SetConsoleMode(k.GetStdHandle(-11),0xD)
msvcrt.setmode(0,os.O_BINARY)
msvcrt.setmode(1,os.O_BINARY)
os.write(1,b'INPUT READY\\r\\n\\x1b[?1000h\\x1b[?1006h\\x1b[?2004h')
f=open(sys.argv[1],'ab',buffering=0)
while True:
 data=os.read(0,1024)
 if not data: break
 f.write(data)
 os.write(1,b'INPUT_BYTES='+data.hex().encode()+b'\\r\\n')
""", encoding="utf-8")
    first, second = directory / "first.bin", directory / "second.bin"
    fixture.command(instance, "new-session", "-d", "-s", "input", sys.executable, "-u", str(script), str(first))
    fixture.command(instance, "split-window", "-h", "-t", "input", sys.executable, "-u", str(script), str(second))
    gui, capture, state = fixture.view("input", instance, False, True,
                                      ["--keybind=f12=paste_from_clipboard"])
    u = ctypes.WinDLL("user32", use_last_error=True)
    k = ctypes.WinDLL("kernel32", use_last_error=True)
    callback = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    for name, args, result in [
        ("EnumWindows", [callback, wintypes.LPARAM], wintypes.BOOL),
        ("GetWindowThreadProcessId", [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)], wintypes.DWORD),
        ("IsWindowVisible", [wintypes.HWND], wintypes.BOOL),
        ("GetWindowRect", [wintypes.HWND, ctypes.POINTER(wintypes.RECT)], wintypes.BOOL),
        ("PostMessageW", [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM], wintypes.BOOL),
        ("SetWindowPos", [wintypes.HWND, wintypes.HWND, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, wintypes.UINT], wintypes.BOOL),
        ("MapVirtualKeyW", [wintypes.UINT, wintypes.UINT], wintypes.UINT),
    ]:
        getattr(u, name).argtypes, getattr(u, name).restype = args, result
    handles = []
    @callback
    def collect(hwnd, unused):
        pid, rect = wintypes.DWORD(), wintypes.RECT()
        u.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        u.GetWindowRect(hwnd, ctypes.byref(rect))
        if pid.value == gui.pid and u.IsWindowVisible(hwnd) and rect.right - rect.left > 600 and rect.bottom - rect.top > 400:
            handles.append(hwnd)
        return True
    u.EnumWindows(collect, 0)
    assert len(handles) == 1, "expected one native terminal HWND"
    hwnd = handles[0]
    def post(message, wp, lp):
        assert u.PostMessageW(hwnd, message, wp, lp), "native message delivery failed"
    for char in "NativeΩ雪":
        post(0x100, ord("A"), 1)
        post(0x102, ord(char), 1)
        post(0x101, ord("A"), 0xC0000001)
    until(lambda: second.exists() and second.read_bytes() == "NativeΩ雪".encode(), "native Unicode keyboard delivery failed")
    fixture.checks.append("windows_native_input::keyboard_unicode")
    before_second = second.read_bytes()
    for message, button in [(0x200, 0), (0x201, 1), (0x202, 0)]:
        post(message, button, 80 | (80 << 16))
    until(lambda: first.exists() and re.fullmatch(rb"\x1b\[<0;\d+;\d+M\x1b\[<0;\d+;\d+m", first.read_bytes()),
          "native SGR mouse press/release did not reach clicked pane")
    assert second.read_bytes() == before_second, "mouse was broadcast to the other pane"
    fixture.checks.append("windows_native_input::mouse_target_only")
    sizes = fixture.command(instance, "list-panes", "-a", "-F", "#{pane_id}:#{pane_width}x#{pane_height}")
    assert u.SetWindowPos(hwnd, None, 100, 100, 1100, 750, 0x0014)
    until(lambda: fixture.command(instance, "list-panes", "-a", "-F", "#{pane_id}:#{pane_width}x#{pane_height}") != sizes,
          "native resize did not change live pane dimensions")
    fixture.checks.append("windows_native_input::resize_live_panes")
    original = first.read_bytes()
    paste = "clipboard Ω雪\nsecond line"
    with Clipboard(u, k, hwnd) as clipboard:
        clipboard.text(paste)
        scan = u.MapVirtualKeyW(0x7B, 0)
        post(0x100, 0x7B, 1 | (scan << 16))
        post(0x101, 0x7B, 0xC0000001 | (scan << 16))
        expected = original + b"\x1b[200~" + paste.encode() + b"\x1b[201~"
        until(lambda: first.read_bytes() == expected, "OS clipboard paste did not reach native pane with bracket markers")
        assert second.read_bytes() == before_second, "paste reached the other pane"
    fixture.checks.append("windows_native_input::clipboard_paste_restore")
    (capture / "hold").unlink()
    after = wait_json(capture / "result.json", gui)
    assert after["pane_processes"] == state["pane_processes"], "input replaced pane processes"
    assert gui.wait(timeout=10) == 0
    fixture.command(instance, "kill-server")
    assert owner.wait(timeout=10) == 0
    fixture.assert_dead(state["pane_processes"].values())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != "win32":
        parser.error("requires native Windows")
    args.report.parent.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="windows-native-input-", dir=args.report.parent))
    fixture = Fixture(args.gui.resolve(strict=True), args.candidate.resolve(strict=True), directory)
    report = {"schema": 1, "kind": "windows-native-input", "artifact_directory": str(directory),
              "gui_sha256": sha256(fixture.gui), "candidate_sha256": sha256(fixture.candidate),
              "delivery": "targeted native HWND messages through Winit", "ime_composition_executed": False,
              "tests": [], "failures": [], "cleanup_remaining": []}
    try:
        exercise(fixture)
    except (Exception, KeyboardInterrupt) as error:
        report["failures"].append(f"{type(error).__name__}: {error}")
    finally:
        report["tests"] = fixture.checks
        report["cleanup_remaining"] = fixture.cleanup()
        report["passed"] = not report["failures"] and not report["cleanup_remaining"]
        args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(report))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
