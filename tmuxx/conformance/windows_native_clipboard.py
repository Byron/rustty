#!/usr/bin/env python3
"""Actual native OSC52 host writes/reads, elected GUI delivery and no replay.

Requires an initially empty private desktop clipboard. Both GUI processes enforce
the fixture's foreground/sequence/empty checks inside OpenClipboard; no preexisting
or concurrently changed clipboard contents are read or replaced.
"""
from __future__ import annotations
import argparse
import base64
import ctypes
from ctypes import wintypes as w
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import uuid
from windows_native_input import Clipboard, until
from windows_native_views import Fixture, sha256, wait_json


def atomic_json(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value), encoding='utf-8')
    temporary.replace(path)


class NativeDesktop:
    def __init__(self):
        self.u = ctypes.WinDLL('user32', use_last_error=True)
        self.k = ctypes.WinDLL('kernel32', use_last_error=True)
        self.callback = ctypes.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
        for name, args, result in [
            ('EnumWindows', [self.callback, w.LPARAM], w.BOOL),
            ('GetWindowThreadProcessId', [w.HWND, ctypes.POINTER(w.DWORD)], w.DWORD),
            ('IsWindowVisible', [w.HWND], w.BOOL),
            ('GetWindowRect', [w.HWND, ctypes.POINTER(w.RECT)], w.BOOL),
            ('GetForegroundWindow', [], w.HWND),
            ('SetForegroundWindow', [w.HWND], w.BOOL),
            ('SetWindowPos', [w.HWND, w.HWND, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, w.UINT], w.BOOL),
            ('WindowFromPoint', [w.POINT], w.HWND),
            ('GetAncestor', [w.HWND, w.UINT], w.HWND),
            ('GetCursorPos', [ctypes.POINTER(w.POINT)], w.BOOL),
            ('SetCursorPos', [ctypes.c_int, ctypes.c_int], w.BOOL),
        ]:
            getattr(self.u, name).argtypes, getattr(self.u, name).restype = args, result

    def hwnd(self, process):
        found = []
        @self.callback
        def collect(hwnd, unused):
            pid, rect = w.DWORD(), w.RECT()
            self.u.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
            self.u.GetWindowRect(hwnd, ctypes.byref(rect))
            if pid.value == process.pid and self.u.IsWindowVisible(hwnd) and rect.right - rect.left > 600:
                found.append(hwnd)
            return True
        self.u.EnumWindows(collect, 0)
        assert len(found) == 1, 'expected exactly one owned native terminal HWND'
        return found[0]

    def focus(self, hwnd):
        if not self.u.SetForegroundWindow(hwnd):
            class Mouse(ctypes.Structure):
                _fields_ = [('x', w.LONG), ('y', w.LONG), ('data', w.DWORD), ('flags', w.DWORD),
                            ('time', w.DWORD), ('extra', ctypes.c_size_t)]
            class Input(ctypes.Structure):
                _fields_ = [('type', w.DWORD), ('mouse', Mouse)]
            assert ctypes.sizeof(Input) == 40, 'unexpected x64 native input layout'
            self.u.SendInput.argtypes, self.u.SendInput.restype = [w.UINT, ctypes.POINTER(Input), ctypes.c_int], w.UINT
            pointer, rect = w.POINT(), w.RECT()
            assert self.u.GetCursorPos(ctypes.byref(pointer)) and self.u.GetWindowRect(hwnd, ctypes.byref(rect))
            assert self.u.SetWindowPos(hwnd, -1, 0, 0, 0, 0, 0x13)
            point = w.POINT(rect.left + 120, rect.top + 12)
            try:
                assert self.u.SetCursorPos(point.x, point.y)
                assert self.u.GetAncestor(self.u.WindowFromPoint(point), 2) == hwnd, 'owned titlebar is obscured'
                events = (Input * 2)()
                events[0].mouse.flags, events[1].mouse.flags = 2, 4
                assert self.u.SendInput(2, events, ctypes.sizeof(Input)) == 2
                until(lambda: self.u.GetForegroundWindow() == hwnd, 'owned titlebar did not acquire foreground', 2)
            finally:
                assert self.u.SetWindowPos(hwnd, -2, 0, 0, 0, 0, 0x13)
                current = w.POINT()
                if self.u.GetCursorPos(ctypes.byref(current)) and (current.x, current.y) == (point.x, point.y):
                    assert self.u.SetCursorPos(pointer.x, pointer.y)
        until(lambda: self.u.GetForegroundWindow() == hwnd, 'fixture HWND is not foreground')


class KnownClipboard(Clipboard):
    """Adopt only a successful GUI receipt; reject any later sequence change."""
    def __init__(self, desktop, payload):
        super().__init__(desktop.u, desktop.k, None)
        self.desktop, self.restored = desktop, False
        self.fixture_bytes = (payload + '\0').encode('utf-16-le')

    def arm_sequence(self):
        self.open()
        try:
            assert self.u.GetForegroundWindow() == self.owner, 'focus changed before clipboard arming'
            if self.u.EnumClipboardFormats(0):
                raise RuntimeError('clipboard changed before application emission; data was preserved')
            return self.u.GetClipboardSequenceNumber()
        finally:
            self.u.CloseClipboard()

    def adopt(self, event):
        assert event['kind'] == 'write-complete' and event['allowed'] and event['owner'] == self.owner
        self.sequence = event['sequence']

    def __exit__(self, *unused):
        if self.sequence is None:
            return
        self.open()
        try:
            # Never read unknown bytes, even when newer data has the same HWND.
            if (self.u.GetClipboardSequenceNumber() != self.sequence or self.u.GetClipboardOwner() != self.owner
                    or self.u.GetForegroundWindow() != self.owner):
                raise RuntimeError('clipboard/focus changed concurrently; newer content was preserved')
            handle = self.u.GetClipboardData(13)
            length = self.k.GlobalSize(handle)
            if not handle or not len(self.fixture_bytes) <= length <= len(self.fixture_bytes) + 16:
                raise RuntimeError('clipboard no longer matches the known fixture allocation')
            pointer = self.k.GlobalLock(handle)
            if not pointer:
                raise ctypes.WinError(ctypes.get_last_error())
            try:
                actual = ctypes.string_at(pointer, length)
                if actual[:len(self.fixture_bytes)] != self.fixture_bytes or any(actual[len(self.fixture_bytes):]):
                    raise RuntimeError('clipboard no longer matches the exact known fixture bytes')
            finally:
                self.k.GlobalUnlock(handle)
            assert self.u.EmptyClipboard()
            self.sequence = None
            self.restored = True
        finally:
            self.u.CloseClipboard()


def events(directories):
    result = []
    for directory in directories:
        path = directory / 'clipboard-events.jsonl'
        if path.exists():
            lines = path.read_text(encoding='utf-8').splitlines()
            for line in lines:
                try:
                    result.append(json.loads(line))
                except json.JSONDecodeError:
                    pass  # A later poll must see the complete append.
    return result


def exercise(fixture, report):
    desktop = NativeDesktop()
    payload = 'tmuxx-OSC52-' + uuid.uuid4().hex + ' Ω雪\nsecond line'
    control = fixture.directory / 'clipboard-guard.json'
    atomic_json(control, {'schema': 1, 'process': 0, 'owner': 0, 'sequence': 0, 'phase': 'disarmed', 'payload': payload})
    fixture.env['RUSTTY_TMUXX_CLIPBOARD_FIXTURE'] = str(control)
    instance = 'clipboard-' + uuid.uuid4().hex
    captures, guis = [], []
    with KnownClipboard(desktop, payload) as clipboard:
        owner = fixture.start([str(fixture.candidate), '-D', '-L', instance], 'owner')
        deadline = time.monotonic() + 15
        while True:
            result = subprocess.run([str(fixture.candidate), '-L', instance, 'list-sessions'], capture_output=True,
                                    env=fixture.env, timeout=3)
            if result.returncode == 0:
                break
            assert owner.poll() is None and time.monotonic() < deadline, 'owner did not start'
            time.sleep(.05)
        script = fixture.directory / 'osc52.py'
        script.write_text("""import base64,ctypes,msvcrt,os,pathlib,sys,threading,time
k=ctypes.WinDLL('kernel32',use_last_error=True)
k.GetStdHandle.restype=ctypes.c_void_p
k.SetConsoleMode.argtypes=[ctypes.c_void_p,ctypes.c_ulong]
assert k.SetConsoleCP(65001) and k.SetConsoleOutputCP(65001)
assert k.SetConsoleMode(k.GetStdHandle(-10),0x200)
assert k.SetConsoleMode(k.GetStdHandle(-11),0xD)
msvcrt.setmode(0,os.O_BINARY); msvcrt.setmode(1,os.O_BINARY)
directory=pathlib.Path(sys.argv[1]); tag=sys.argv[2]
def record():
 with open(directory/(tag+'.bin'),'ab',buffering=0) as output:
  while True:
   data=os.read(0,1024)
   if not data:return
   output.write(data)
threading.Thread(target=record,daemon=True).start()
os.write(1,('OSC52 READY PID='+str(os.getpid())+'\\r\\n').encode())
emitted=set(); deadline=time.monotonic()+100
while time.monotonic()<deadline:
 for phase in ('write','read'):
  if tag=='requestor' and phase not in emitted and (directory/(phase+'.go')).exists():
   emitted.add(phase)
   data=sys.argv[3].encode('ascii') if phase=='write' else b'?'
   os.write(1,b'\\x1b]52;c;'+data+b'\\x1b\\\\')
 time.sleep(.01)
""", encoding='utf-8')
        encoded = base64.b64encode(payload.encode()).decode()
        fixture.command(instance, 'new-session', '-d', '-s', 'clipboard', sys.executable, '-u', str(script), str(fixture.directory), 'requestor', encoded)
        fixture.command(instance, 'split-window', '-h', '-t', 'clipboard', sys.executable, '-u', str(script), str(fixture.directory), 'other', encoded)
        fixture.command(instance, 'select-pane', '-t', '%0')
        fixture.command(instance, 'link-window', '-s', '@0', '-t', 'clipboard:7')
        fixture.command(instance, 'set-option', '-s', 'set-clipboard', 'on')
        fixture.command(instance, 'set-option', '-s', 'get-clipboard', 'request')
        for pane in ('%0', '%1'):
            fixture.command(instance, 'tmuxx-retain-pane', '-t', pane, 'on')
        for name in ('first', 'second'):
            gui, capture, state = fixture.view(name, instance, False, True,
                                               ['--clipboard-read=allow', '--clipboard-write=allow'])
            captures.append(capture)
            guis.append((gui, capture, state))
        assert guis[0][2]['pane_processes'] == guis[1][2]['pane_processes']
        elected, _, state = guis[-1]
        clipboard.owner = desktop.hwnd(elected)
        desktop.focus(clipboard.owner)
        time.sleep(.2)  # Let the native focus notification reach the owner before the one-shot emission.
        guard = {'schema': 1, 'process': elected.pid, 'owner': clipboard.owner, 'phase': 'write',
                 'sequence': clipboard.arm_sequence(), 'payload': payload}
        atomic_json(control, guard)
        (fixture.directory / 'write.go').write_text('emit once', encoding='ascii')
        until(lambda: any(item['kind'] == 'write-complete' for item in events(captures)), 'native OSC52 write did not complete')
        observed = events(captures)
        receipt = next(item for item in observed if item['kind'] == 'write-complete')
        clipboard.adopt(receipt)
        assert [item['kind'] for item in observed] == ['write', 'write-complete'], observed
        assert all(item['allowed'] and item['process'] == elected.pid for item in observed), observed
        fixture.checks.append('windows_native_clipboard::osc52_write_elects_one_native_gui')
        guard.update(phase='read', sequence=receipt['sequence'])
        atomic_json(control, guard)
        (fixture.directory / 'read.go').write_text('emit once', encoding='ascii')
        expected = b'\x1b]52;c;' + encoded.encode() + b'\x1b\\'
        received = fixture.directory / 'requestor.bin'
        until(lambda: received.exists() and received.read_bytes() == expected, 'native clipboard query did not reply byte-exactly once')
        observed = events(captures)
        assert [item['kind'] for item in observed] == ['write', 'write-complete', 'read', 'read-complete'], observed
        assert all(item['allowed'] and item['process'] == elected.pid for item in observed), observed
        other = fixture.directory / 'other.bin'
        assert not other.exists() or not other.read_bytes(), 'clipboard query reply reached another pane'
        fixture.checks.append('windows_native_clipboard::native_read_replies_once_to_original_pane')
        report.update(elected_gui_pid=elected.pid, elected_hwnd=clipboard.owner, clipboard_events=observed,
                      payload_sha256=__import__('hashlib').sha256(payload.encode()).hexdigest(),
                      pane_processes=state['pane_processes'], incarnation=state['instance'])
        # Clear the known value before its owning HWND is destroyed. The strict
        # sequence guard refuses to read/replace newer data, including same-HWND data.
        clipboard.__exit__()
        report['clipboard_restored_empty'] = clipboard.restored
        guard.update(phase='disarmed', sequence=desktop.u.GetClipboardSequenceNumber())
        atomic_json(control, guard)
        empty_sequence = desktop.u.GetClipboardSequenceNumber()
        (guis[-1][1] / 'hold').unlink()
        final = wait_json(guis[-1][1] / 'result.json', elected)
        assert final['pane_processes'] == state['pane_processes'] and elected.wait(timeout=10) == 0
        gui, capture, reconnected = fixture.view('reconnected', instance, False, True,
                                                 ['--clipboard-read=allow', '--clipboard-write=allow'])
        captures.append(capture)
        guis.append((gui, capture, reconnected))
        assert reconnected['pane_processes'] == state['pane_processes'] and reconnected['instance'] == state['instance']
        time.sleep(.3)
        assert events(captures) == observed, 'native reconnect replayed an old OS clipboard effect'
        assert received.read_bytes() == expected, 'native reconnect replayed a clipboard query reply'
        assert desktop.u.GetClipboardSequenceNumber() == empty_sequence, 'clipboard changed after guarded cleanup'
        fixture.checks.append('windows_native_clipboard::reconnect_never_replays_host_effect_or_reply')
        for process, capture, original in guis:
            if process.poll() is None:
                (capture / 'hold').unlink(missing_ok=True)
                after = wait_json(capture / 'result.json', process)
                assert after['pane_processes'] == original['pane_processes']
                assert process.wait(timeout=10) == 0
        fixture.command(instance, 'kill-server')
        assert owner.wait(timeout=10) == 0
        fixture.assert_dead(state['pane_processes'].values())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gui', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != 'win32':
        parser.error('requires native Windows')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix='windows-native-clipboard-', dir=args.report.parent))
    fixture = Fixture(args.gui.resolve(strict=True), args.candidate.resolve(strict=True), directory)
    report = {'schema': 1, 'kind': 'windows-native-clipboard', 'artifact_directory': str(directory),
              'gui_sha256': sha256(fixture.gui), 'candidate_sha256': sha256(fixture.candidate),
              'tests': [], 'failures': [], 'cleanup_remaining': []}
    try:
        exercise(fixture, report)
    except (Exception, KeyboardInterrupt) as error:
        report['failures'].append(f'{type(error).__name__}: {error}')
    finally:
        report['tests'] = fixture.checks
        report['cleanup_remaining'] = fixture.cleanup()
        report['passed'] = not report['failures'] and not report['cleanup_remaining']
        args.report.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
        print(json.dumps(report))
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
