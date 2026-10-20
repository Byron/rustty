"""Real keyboard-to-Japanese-IME-to-pane fixture with one-profile restoration.

Run only on an explicitly prepared interactive desktop. This never installs a
language, changes the language list, or reads/writes clipboard content.
"""
import argparse
import ctypes
from ctypes import wintypes as w
import json
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import uuid
import winreg

from windows_native_views import Fixture, wait_json

SERVICE = "{03B5835F-F03C-411B-9CE2-AA23E1171E36}"
PROFILE = "{A76C93D9-5523-4E90-AAFA-4DB112F9AC76}"
REGISTRY = "Software\\Microsoft\\CTF\\TIP\\" + SERVICE + "\\LanguageProfile\\0x00000411\\" + PROFILE

class GUID(ctypes.Structure):
    _fields_ = [("a", w.DWORD), ("b", w.WORD), ("c", w.WORD), ("d", w.BYTE * 8)]
def guid(text):
    return GUID.from_buffer_copy(uuid.UUID(text).bytes_le)

def profile_api(enable=None):
    ole = ctypes.WinDLL("ole32")
    ole.CoInitializeEx.argtypes, ole.CoInitializeEx.restype = [w.LPVOID, w.DWORD], w.LONG
    ole.CoCreateInstance.argtypes = [ctypes.POINTER(GUID), w.LPVOID, w.DWORD, ctypes.POINTER(GUID), ctypes.POINTER(w.LPVOID)]
    ole.CoCreateInstance.restype = w.LONG
    assert ole.CoInitializeEx(None, 2) >= 0
    pointer = w.LPVOID()
    try:
        clsid, iid = guid("33c53a50-f456-4884-b049-85fd643ecfed"), guid("1F02B6C5-7842-4EE6-8A0B-9A24183A95CA")
        assert ole.CoCreateInstance(ctypes.byref(clsid), None, 1, ctypes.byref(iid), ctypes.byref(pointer)) >= 0
        table = ctypes.cast(pointer, ctypes.POINTER(ctypes.POINTER(w.LPVOID))).contents
        service, profile = guid(SERVICE), guid(PROFILE)
        if enable is not None:
            setter = ctypes.WINFUNCTYPE(w.LONG, w.LPVOID, ctypes.POINTER(GUID), w.WORD, ctypes.POINTER(GUID), w.BOOL)(table[17])
            status = setter(pointer, ctypes.byref(service), 0x0411, ctypes.byref(profile), bool(enable))
            assert status >= 0, f"EnableLanguageProfile: {status & 0xffffffff:#x}"
        output = w.BOOL()
        getter = ctypes.WINFUNCTYPE(w.LONG, w.LPVOID, ctypes.POINTER(GUID), w.WORD, ctypes.POINTER(GUID), ctypes.POINTER(w.BOOL))(table[18])
        status = getter(pointer, ctypes.byref(service), 0x0411, ctypes.byref(profile), ctypes.byref(output))
        assert status == 0
        return bool(output.value)
    finally:
        if pointer:
            table = ctypes.cast(pointer, ctypes.POINTER(ctypes.POINTER(w.LPVOID))).contents
            ctypes.WINFUNCTYPE(w.ULONG, w.LPVOID)(table[2])(pointer)
        ole.CoUninitialize()

def registry_state(path=REGISTRY):
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, path) as key:
            values = {}
            index = 0
            while True:
                try:
                    name, value, kind = winreg.EnumValue(key, index)
                    assert isinstance(value, (str, int, list)), "unexpected profile registry type"
                    values[name] = [value, kind]
                    index += 1
                except OSError as error:
                    if error.winerror != 259:
                        raise
                    break
            return {"exists": True, "values": values}
    except FileNotFoundError:
        return {"exists": False, "values": {}}

def ancestor_state():
    parts = REGISTRY.split("\\")
    return {"\\".join(parts[:length]): registry_state("\\".join(parts[:length]))["exists"]
            for length in range(1, len(parts))}

def remove_new_empty_ancestors(saved):
    for path, existed in reversed(list(saved["ancestors"].items())):
        if existed:
            continue
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, path) as key:
                children, values, _ = winreg.QueryInfoKey(key)
            if not children and not values:
                winreg.DeleteKey(winreg.HKEY_CURRENT_USER, path)
        except FileNotFoundError:
            pass

def atomic(path, value):
    temporary = path.with_suffix(".tmp")
    with temporary.open("w", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2)
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)

def restore(journal):
    saved = json.loads(journal.read_text(encoding="utf-8"))
    before = saved["registry"]
    current = registry_state()
    if current == before and profile_api() == saved["enabled"]:
        remove_new_empty_ancestors(saved)
        atomic(journal.with_name("restored.json"), {"restored": True, "registry": current, "enabled": saved["enabled"]})
        return
    # Only this profile's Enable value may be changed by this fixture. Leave
    # unrelated or concurrently added values intact and report them explicitly.
    other_before = {k: v for k, v in before["values"].items() if k != "Enable"}
    other_current = {k: v for k, v in current["values"].items() if k != "Enable"}
    if profile_api() != saved["enabled"]:
        profile_api(saved["enabled"])
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, REGISTRY, 0, winreg.KEY_SET_VALUE) as key:
        if "Enable" in before["values"]:
            value, kind = before["values"]["Enable"]
            winreg.SetValueEx(key, "Enable", 0, kind, value)
        else:
            try:
                winreg.DeleteValue(key, "Enable")
            except FileNotFoundError:
                pass
    if not before["exists"]:
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, REGISTRY) as key:
                children, values, _ = winreg.QueryInfoKey(key)
            if not children and not values:
                winreg.DeleteKey(winreg.HKEY_CURRENT_USER, REGISTRY)
        except FileNotFoundError:
            pass
    remove_new_empty_ancestors(saved)
    if other_before != other_current:
        raise RuntimeError("unrelated profile values changed concurrently; Enable restored, journal retained")
    assert profile_api() == saved["enabled"]
    assert registry_state() == before
    atomic(journal.with_name("restored.json"), {"restored": True, "registry": registry_state(), "enabled": profile_api()})

def watchdog(journal, owner):
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.OpenProcess.argtypes, kernel.OpenProcess.restype = [w.DWORD, w.BOOL, w.DWORD], w.HANDLE
    kernel.WaitForSingleObject.argtypes = [w.HANDLE, w.DWORD]
    kernel.CloseHandle.argtypes = [w.HANDLE]
    kernel.GetCurrentProcess.restype = w.HANDLE
    kernel.IsProcessInJob.argtypes, kernel.IsProcessInJob.restype = [w.HANDLE, w.HANDLE, ctypes.POINTER(w.BOOL)], w.BOOL
    in_job = w.BOOL()
    assert kernel.IsProcessInJob(kernel.GetCurrentProcess(), None, ctypes.byref(in_job))
    assert not in_job.value, "restoration watchdog did not escape its launch job"
    handle = kernel.OpenProcess(0x00100000, False, owner)
    assert handle
    atomic(journal.with_name("watchdog-ready.json"), {"pid": os.getpid(), "owner": owner, "in_job": False})
    try:
        deadline = time.monotonic() + 150
        while not journal.with_name("done").exists():
            if (kernel.WaitForSingleObject(handle, 100) == 0 or time.monotonic() > deadline
                    or journal.with_name("restore-now").exists()):
                for attempt in range(3):
                    try:
                        restore(journal)
                        return
                    except Exception as error:
                        atomic(journal.with_name("watchdog-error.json"), {"attempt": attempt + 1, "error": repr(error)})
                        time.sleep(.25)
                raise RuntimeError("watchdog could not restore profile; journal retained")
    finally:
        kernel.CloseHandle(handle)

def wait(predicate, text, seconds=8):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(.03)
    raise TimeoutError(text)

def enable_after_watchdog(guard, journal, report):
    ready = wait_json(journal.with_name("watchdog-ready.json"), guard, seconds=8)
    if (ready.get("pid") != guard.pid or ready.get("owner") != os.getpid()
            or ready.get("in_job") is not False or guard.poll() is not None):
        raise RuntimeError("independent restoration watchdog identity is not established")
    report["watchdog_pid"] = guard.pid
    assert profile_api(True), "Existing Japanese profile could not be enabled without servicing installation"
    report["enabled_registry"] = registry_state()

def exercise(fixture, report):
    instance = "ime-" + uuid.uuid4().hex
    owner = fixture.start([str(fixture.candidate), "-D", "-L", instance], "owner")
    deadline = time.monotonic() + 15
    while True:
        reply = subprocess.run([str(fixture.candidate), "-L", instance, "list-sessions"], capture_output=True, timeout=3)
        if reply.returncode == 0:
            break
        assert owner.poll() is None and time.monotonic() < deadline
        time.sleep(.05)
    recorder = fixture.directory / "record.py"
    recorder.write_text("""import ctypes,msvcrt,os,sys
k=ctypes.WinDLL('kernel32'); k.GetStdHandle.restype=ctypes.c_void_p
k.SetConsoleMode.argtypes=[ctypes.c_void_p,ctypes.c_ulong]
assert k.SetConsoleCP(65001) and k.SetConsoleOutputCP(65001)
assert k.SetConsoleMode(k.GetStdHandle(-10),0x200)
msvcrt.setmode(0,os.O_BINARY); msvcrt.setmode(1,os.O_BINARY)
os.write(1,b'IME READY\\r\\n')
with open(sys.argv[1],'ab',buffering=0) as f:
 while True:
  data=os.read(0,1024)
  if not data: break
  f.write(data); os.write(1,b'IME_BYTES='+data.hex().encode()+b'\\r\\n')
""", encoding="utf-8")
    received = fixture.directory / "received.bin"
    fixture.command(instance, "new-session", "-d", "-s", "ime", sys.executable, "-u", str(recorder), str(received))
    original = fixture.command(instance, "list-panes", "-a", "-F", "#{pane_id}").strip()
    fixture.command(instance, "split-window", "-d", "-h", "-t", original,
                    sys.executable, "-u", "-c", "import time; print('IME observer',flush=True); time.sleep(180)")
    fixture.command(instance, "select-pane", "-t", original)
    fixture.remember({"pane_processes": dict(enumerate(int(pid) for pid in
        fixture.command(instance, "list-panes", "-a", "-F", "#{pane_pid}").splitlines()))})
    report["original_pane"] = original
    gui, capture, state = fixture.view("ime", instance, False, True)
    report.update(gui_pid=gui.pid, pane_processes=state["pane_processes"], capture=str(capture))
    u = ctypes.WinDLL("user32", use_last_error=True)
    cbtype = ctypes.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
    for name, args, result in [
        ("EnumWindows", [cbtype,w.LPARAM],w.BOOL),
        ("GetWindowThreadProcessId",[w.HWND,ctypes.POINTER(w.DWORD)],w.DWORD),
        ("IsWindowVisible",[w.HWND],w.BOOL),
        ("GetWindowRect",[w.HWND,ctypes.POINTER(w.RECT)],w.BOOL),
        ("GetForegroundWindow",[],w.HWND),
        ("SetForegroundWindow",[w.HWND],w.BOOL),
        ("SetWindowPos",[w.HWND,w.HWND,ctypes.c_int,ctypes.c_int,ctypes.c_int,ctypes.c_int,w.UINT],w.BOOL),
        ("WindowFromPoint",[w.POINT],w.HWND),
        ("GetAncestor",[w.HWND,w.UINT],w.HWND),
        ("GetCursorPos",[ctypes.POINTER(w.POINT)],w.BOOL),
        ("SetCursorPos",[ctypes.c_int,ctypes.c_int],w.BOOL),
        ("LoadKeyboardLayoutW",[w.LPCWSTR,w.UINT],w.HANDLE),
        ("UnloadKeyboardLayout",[w.HANDLE],w.BOOL),
        ("GetKeyboardLayout",[w.DWORD],w.HANDLE),
        ("PostMessageW",[w.HWND,w.UINT,w.WPARAM,w.LPARAM],w.BOOL),
    ]:
        getattr(u,name).argtypes,getattr(u,name).restype=args,result
    handles=[]
    @cbtype
    def find(hwnd,unused):
        pid,rect=w.DWORD(),w.RECT()
        u.GetWindowThreadProcessId(hwnd,ctypes.byref(pid))
        u.GetWindowRect(hwnd,ctypes.byref(rect))
        if (pid.value==gui.pid and u.IsWindowVisible(hwnd)
                and rect.right-rect.left>600 and rect.bottom-rect.top>400): handles.append(hwnd)
        return True
    u.EnumWindows(find,0)
    assert len(handles)==1, handles
    hwnd=handles[0]
    report["hwnd"]=hwnd
    class KEYBDINPUT(ctypes.Structure):
        _fields_=[("vk",w.WORD),("scan",w.WORD),("flags",w.DWORD),("time",w.DWORD),("extra",ctypes.c_size_t)]
    class MOUSEINPUT(ctypes.Structure):
        _fields_=[("dx",w.LONG),("dy",w.LONG),("data",w.DWORD),("flags",w.DWORD),("time",w.DWORD),("extra",ctypes.c_size_t)]
    class UNION(ctypes.Union):
        _fields_=[("ki",KEYBDINPUT),("mi",MOUSEINPUT)]
    class INPUT(ctypes.Structure):
        _fields_=[("type",w.DWORD),("data",UNION)]
    assert ctypes.sizeof(INPUT) == 40, "unexpected native x64 INPUT layout"
    u.SendInput.argtypes,u.SendInput.restype=[w.UINT,ctypes.POINTER(INPUT),ctypes.c_int],w.UINT
    report["foreground_before"]=u.GetForegroundWindow()
    if not u.SetForegroundWindow(hwnd):
        pointer,rect=w.POINT(),w.RECT()
        assert u.GetCursorPos(ctypes.byref(pointer)) and u.GetWindowRect(hwnd,ctypes.byref(rect))
        assert u.SetWindowPos(hwnd,-1,0,0,0,0,0x13)
        try:
            point=w.POINT(rect.left+120,rect.top+12)
            assert u.SetCursorPos(point.x,point.y)
            assert u.GetAncestor(u.WindowFromPoint(point),2)==hwnd, "owned titlebar is obscured; refusing mouse input"
            clicks=(INPUT*2)()
            clicks[0].data.mi.flags,clicks[1].data.mi.flags=2,4
            assert u.SendInput(2,clicks,ctypes.sizeof(INPUT))==2
            wait(lambda:u.GetForegroundWindow()==hwnd,"owned titlebar click did not acquire foreground",2)
        finally:
            assert u.SetWindowPos(hwnd,-2,0,0,0,0,0x13)
            assert u.SetCursorPos(pointer.x,pointer.y)
    wait(lambda:u.GetForegroundWindow()==hwnd,"owned HWND did not become foreground")
    thread=u.GetWindowThreadProcessId(hwnd,None)
    previous=u.GetKeyboardLayout(thread)
    layout=u.LoadKeyboardLayoutW("00000411",0x80)  # KLF_NOTELLSHELL, no activation flags.
    assert layout, "Japanese keyboard layout unavailable"
    report.update(previous_hkl=previous, japanese_hkl=layout)
    def key(vk):
        assert u.GetForegroundWindow()==hwnd,"focus changed; refusing keyboard input"
        events=(INPUT*2)()
        for index,flags in [(0,0),(1,2)]:
            events[index].type=1
            events[index].data.ki=KEYBDINPUT(vk,0,flags,0,0)
        assert u.SendInput(2,events,ctypes.sizeof(INPUT))==2
        assert u.GetForegroundWindow()==hwnd,"focus changed during keyboard input"
        time.sleep(.12)
    try:
        assert u.PostMessageW(hwnd,0x50,0,layout)
        wait(lambda:u.GetKeyboardLayout(thread)==layout,"owned GUI did not accept Japanese input layout")
        (capture/"ime-activate").touch()
        activation=wait_json(capture/"ime-activated.json",gui,seconds=8)
        report["activation"]=activation
        assert activation["active"], f"Native thread profile activation failed: {activation}"
        key(0x16)  # VK_IME_ON: keyboard event, never fabricated composition text.
        for letter in "NIHON": key(ord(letter))
        event_file=capture/"ime-events.json"
        def events():
            try:return json.loads(event_file.read_text(encoding="utf-8"))
            except (FileNotFoundError,json.JSONDecodeError):return []
        preedit=wait(lambda:next((x for x in events() if x.get("kind")=="preedit" and x.get("text")),None),
                     "No genuine Winit IME preedit after keyboard input")
        report["preedit"]=preedit
        assert not received.exists() or not received.read_bytes(), "preedit leaked to the application before commit"
        key(0x20)  # Space chooses a conversion candidate.
        key(0x0d)  # Enter commits that candidate through the IME.
        commit=wait(lambda:next((x for x in events() if x.get("kind")=="commit" and x.get("text")),None),
                    "No genuine Winit IME commit after keyboard conversion")
        report["commit"]=commit
        assert any(ord(c)>127 for c in commit["text"])
        expected=commit["text"].encode("utf-8")
        wait(lambda:received.exists() and received.read_bytes()==expected,"IME commit did not arrive exactly once in the original pane")
        time.sleep(.3)
        assert received.read_bytes()==expected,"IME commit was duplicated after initial delivery"
        assert len([event for event in events() if event.get("kind")=="commit" and event.get("text")])==1
        report["received_hex"]=received.read_bytes().hex()
        report["ime_events"]=events()
        report["passed"]=True
    finally:
        u.PostMessageW(hwnd,0x50,0,previous)
        u.UnloadKeyboardLayout(layout)
        (capture/"hold").unlink(missing_ok=True)

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument("--watchdog",type=Path)
    parser.add_argument("--owner",type=int)
    parser.add_argument("--gui",type=Path)
    parser.add_argument("--candidate",type=Path)
    parser.add_argument("--report",type=Path)
    parser.add_argument("--enable-existing-japanese-profile",action="store_true")
    parser.add_argument("--qualify-safety",action="store_true")
    args=parser.parse_args()
    if args.watchdog:
        watchdog(args.watchdog,args.owner)
        return 0
    if not args.enable_existing_japanese_profile:
        parser.error("requires --enable-existing-japanese-profile for this temporary desktop fixture")
    if not args.gui or not args.candidate or not args.report:
        parser.error("requires --gui, --candidate and --report")
    args.gui=args.gui.resolve(strict=True)
    args.candidate=args.candidate.resolve(strict=True)
    args.report.parent.mkdir(parents=True,exist_ok=True)
    directory=Path(tempfile.mkdtemp(prefix="windows-native-ime-",dir=args.report.parent))
    journal=directory/"profile-journal.json"
    baseline={"registry":registry_state(),"ancestors":ancestor_state(),"enabled":profile_api(),"registry_path":REGISTRY}
    assert not baseline["enabled"],"Fixture expects the reviewed disabled Japanese profile baseline"
    atomic(journal,baseline)
    report={"schema":1,"kind":"windows-native-ime", "artifact_directory":str(directory),
            "passed":False,"tests":[],"failures":[],"cleanup_remaining":[],
            "gui_sha256":hashlib.sha256(args.gui.read_bytes()).hexdigest(),
            "candidate_sha256":hashlib.sha256(args.candidate.read_bytes()).hexdigest()}
    guard=None
    guard_log=None
    fixture=Fixture(args.gui,args.candidate,directory)
    try:
        if args.qualify_safety:
            import io
            import unittest
            from test_windows_native_ime import ImeFixtureSafetyTests
            suite=unittest.defaultTestLoader.loadTestsFromTestCase(ImeFixtureSafetyTests)
            names=[case.id().rsplit(".",1)[1] for case in suite]
            log=io.StringIO()
            result=unittest.TextTestRunner(stream=log,verbosity=2).run(suite)
            (directory/"safety-tests.txt").write_text(log.getvalue(),encoding="utf-8")
            assert result.wasSuccessful() and result.testsRun==len(names) and not result.skipped, log.getvalue()
            report["tests"].extend("windows_native_ime::"+name for name in names)
        guard_log=(directory/"watchdog.log").open("wb")
        guard=subprocess.Popen([sys.executable,str(Path(__file__).resolve()),"--watchdog",str(journal),"--owner",str(os.getpid())],
                               stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=guard_log,
                               creationflags=subprocess.CREATE_NO_WINDOW|0x01000000) # CREATE_BREAKAWAY_FROM_JOB
        enable_after_watchdog(guard,journal,report)
        exercise(fixture,report)
        report["tests"].append("windows_native_ime::japanese_preedit_commit_original_pane")
    except (Exception,KeyboardInterrupt) as error:
        report["failures"].append(f"{type(error).__name__}: {error}")
    finally:
        report["cleanup_remaining"]=fixture.cleanup()
        try:
            restore(journal)
            report["profile_restored"]=True
        except Exception as error:
            report["failures"].append("RESTORATION: "+repr(error))
        if report.get("profile_restored"):
            journal.with_name("done").touch()
        else:
            journal.with_name("restore-now").touch()
        if guard:
            try:
                guard.wait(timeout=8)
                if guard.returncode:
                    report["failures"].append(f"restoration watchdog failed with {guard.returncode}")
            except subprocess.TimeoutExpired:
                if report.get("profile_restored"):
                    guard.kill();guard.wait(timeout=5)
                report["failures"].append(f"restoration watchdog did not exit: {guard.pid}")
        if guard_log:
            guard_log.close()
        if report.get("profile_restored") and not report["cleanup_remaining"] and (not guard or guard.poll() == 0):
            report["tests"].append("windows_native_ime::profile_restored_and_owned_processes_clean")
        report["passed"]=report["passed"] and not report["failures"] and not report["cleanup_remaining"]
        atomic(directory/"result.json",report)
        atomic(args.report,report)
        print(json.dumps(report))
    return 0 if report["passed"] else 1

if __name__=="__main__":raise SystemExit(main())
