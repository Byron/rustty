"""Native safety regressions for the explicitly enabled desktop IME fixture."""
import concurrent.futures
import ctypes
from ctypes import wintypes as w
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import MagicMock, patch

if os.name != "nt":
    raise unittest.SkipTest("native Windows IME fixture safety")

import runner
import windows_native_ime as ime


class ImeFixtureSafetyTests(unittest.TestCase):
    def test_watchdog_ack_requires_live_escaped_process_and_exact_identity(self):
        guard = MagicMock(pid=42)
        guard.poll.return_value = None
        good = {"pid": 42, "owner": os.getpid(), "in_job": False}
        for changed in ({"pid": 43}, {"owner": os.getpid() + 1}, {"in_job": True}, {"in_job": 0}):
            with self.subTest(changed=changed), patch.object(ime, "wait_json", return_value=good | changed), \
                    patch.object(ime, "profile_api") as profile, self.assertRaisesRegex(RuntimeError, "identity"):
                ime.enable_after_watchdog(guard, Path("unused"), {})
            profile.assert_not_called()
        guard.poll.return_value = 1
        with patch.object(ime, "wait_json", return_value=good), patch.object(ime, "profile_api") as profile, \
                self.assertRaisesRegex(RuntimeError, "identity"):
            ime.enable_after_watchdog(guard, Path("unused"), {})
        profile.assert_not_called()

    def test_unchanged_profile_restore_does_not_write_enable_value(self):
        with tempfile.TemporaryDirectory(prefix="ime-noop-") as directory:
            journal = Path(directory) / "profile-journal.json"
            saved = {"registry": {"exists": False, "values": {}}, "ancestors": {}, "enabled": False}
            ime.atomic(journal, saved)
            with patch.object(ime, "registry_state", return_value=saved["registry"]), \
                    patch.object(ime, "profile_api", return_value=False) as profile:
                ime.restore(journal)
            self.assertTrue(all(not call.args for call in profile.call_args_list))
            self.assertTrue(json.loads(journal.with_name("restored.json").read_text())["restored"])

    @unittest.skipUnless(os.environ.get("TMUXX_WINDOWS_IME_PROFILE") == "temporary-japanese",
                         "requires the explicitly prepared desktop Japanese profile")
    def test_missing_breakaway_aborts_before_any_profile_change(self):
        before = (ime.registry_state(), ime.ancestor_state(), ime.profile_api())
        self.assertFalse(before[2], "requires the reviewed disabled Japanese profile baseline")
        with tempfile.TemporaryDirectory(prefix="ime-no-breakaway-") as directory:
            result = Path(directory) / "result.json"
            child = runner.run_suite_process([
                sys.executable, str(Path(ime.__file__).resolve()), "--gui", sys.executable,
                "--candidate", sys.executable, "--report", str(result), "--enable-existing-japanese-profile",
            ], cwd=Path(ime.__file__).parent, env=os.environ.copy(), timeout=20)
            self.assertNotEqual(child.returncode, 0)
            report = json.loads(result.read_text(encoding="utf-8"))
            self.assertTrue(any("PermissionError" in failure and "5" in failure for failure in report["failures"]), report)
            self.assertNotIn("enabled_registry", report)
            self.assertNotIn("gui_pid", report)
            self.assertEqual(report["cleanup_remaining"], [])
            self.assertTrue(report["profile_restored"])
        self.assertEqual((ime.registry_state(), ime.ancestor_state(), ime.profile_api()), before)

    @unittest.skipUnless(os.environ.get("TMUXX_WINDOWS_IME_PROFILE") == "temporary-japanese",
                         "requires the explicitly prepared desktop Japanese profile")
    def test_watchdog_restores_after_abrupt_fixture_exit(self):
        before = (ime.registry_state(), ime.ancestor_state(), ime.profile_api())
        self.assertFalse(before[2], "requires the reviewed disabled Japanese profile baseline")
        with tempfile.TemporaryDirectory(prefix="ime-abrupt-watchdog-") as directory:
            root = Path(directory)
            journal = root / "profile-journal.json"
            worker = root / "worker.py"
            worker.write_text("""import os, pathlib, subprocess, sys, time
sys.path.insert(0, sys.argv[1])
import windows_native_ime as ime
root=pathlib.Path(sys.argv[2]); journal=root/'profile-journal.json'
ime.atomic(journal,{'registry':ime.registry_state(),'ancestors':ime.ancestor_state(),'enabled':ime.profile_api()})
with (root/'watchdog.log').open('wb') as log:
 guard=subprocess.Popen([sys.executable,ime.__file__,'--watchdog',str(journal),'--owner',str(os.getpid())],
   stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=log,
   creationflags=subprocess.CREATE_NO_WINDOW|0x01000000)
 report={}; ime.enable_after_watchdog(guard,journal,report)
 ime.atomic(root/'enabled.json',report)
 deadline=time.monotonic()+8
 while not (root/'exit-now').exists() and time.monotonic()<deadline: time.sleep(.01)
 os._exit(17)
""", encoding="utf-8")
            k = ctypes.WinDLL("kernel32", use_last_error=True)
            k.OpenProcess.argtypes, k.OpenProcess.restype = [w.DWORD, w.BOOL, w.DWORD], w.HANDLE
            k.WaitForSingleObject.argtypes, k.WaitForSingleObject.restype = [w.HANDLE, w.DWORD], w.DWORD
            k.CloseHandle.argtypes = [w.HANDLE]
            handle = None
            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
                future = executor.submit(runner.run_suite_process,
                    [sys.executable, str(worker), str(Path(ime.__file__).parent), str(root)],
                    cwd=root, env=os.environ.copy(), timeout=20, allow_breakaway=True)
                try:
                    ime.wait(lambda: (root / "enabled.json").is_file(), "guarded worker did not enable profile", 10)
                    data = json.loads((root / "enabled.json").read_text())
                    handle = k.OpenProcess(0x00100000, False, data["watchdog_pid"])
                    self.assertTrue(handle)
                    self.assertTrue(ime.profile_api())
                    (root / "exit-now").touch()
                    self.assertEqual(future.result(timeout=15).returncode, 17)
                    ime.wait(lambda: journal.with_name("restored.json").is_file(), "watchdog did not restore after owner death", 8)
                    self.assertEqual(k.WaitForSingleObject(handle, 5000), 0, "restoration watchdog remained alive")
                finally:
                    (root / "exit-now").touch()
                    future.result(timeout=25)
                    if journal.exists():
                        ime.restore(journal)
                    if handle:
                        k.WaitForSingleObject(handle, 5000)
                        k.CloseHandle(handle)
        self.assertEqual((ime.registry_state(), ime.ancestor_state(), ime.profile_api()), before)


if __name__ == "__main__":
    unittest.main()
