"""Forced-dispatcher evidence checks; these never connect to SSH or open a GUI."""
import base64
from contextlib import contextmanager
import ctypes
import hashlib
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

if sys.platform == "win32":
    import windows_ssh


@unittest.skipUnless(sys.platform == "win32", "Windows fixture imports native identity APIs")
class TrustedDispatcherEvidence(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.bridge = self.directory / "Rustty 雪 &^ % !;' bridge.exe"
        self.bridge.write_bytes(b"exact fixture image")
        self.command = windows_ssh.rustty_bridge_command(self.bridge)
        self.digest = hashlib.sha256(self.command.encode("utf-8")).hexdigest()
        self.receipt = {"expected_command_sha256": self.digest,
                        "original_command_sha256": self.digest, "child_pid": 123,
                        "bridge_path": str(self.bridge), "accepted": True}

    def write_receipt(self, value=None, name="fresh.json"):
        path = self.directory / name
        path.write_text(json.dumps(value or self.receipt), encoding="utf-8")
        return path

    def read(self, *, previous=frozenset(), accepted=True):
        return windows_ssh.dispatch_receipts(self.directory, previous, self.command,
                                             self.bridge, accepted=accepted)

    def test_complete_command_preserves_path_and_has_no_instance_interpolation(self):
        prefix, encoded = self.command.rsplit(" ", 1)
        self.assertEqual(prefix, "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand")
        self.assertEqual(base64.b64decode(encoded).decode("utf-16-le"),
                         "& '" + str(self.bridge).replace("'", "''") + "' tmux --native-stdio; exit $LASTEXITCODE")

    def test_receipts_must_be_fresh_and_bound_to_command_image_and_child(self):
        path = self.write_receipt()
        self.assertEqual(self.read()[0]["child_pid"], 123)
        with self.assertRaisesRegex(AssertionError, "no fresh"):
            self.read(previous={path})
        other_image = self.directory / "unrelated.exe"
        other_image.write_bytes(b"same format is not same identity")
        for mutation in ({"expected_command_sha256": "0" * 64},
                         {"original_command_sha256": "0" * 64},
                         {"bridge_path": str(other_image)}, {"child_pid": None},
                         {"child_pid": 0}, {"child_pid": True}, {"accepted": False}):
            with self.subTest(mutation=mutation):
                self.write_receipt(self.receipt | mutation)
                with self.assertRaises(AssertionError):
                    self.read()

    def test_denial_requires_no_process_and_a_different_original_command(self):
        rejected = self.receipt | {"accepted": False, "child_pid": None,
                                   "original_command_sha256": "0" * 64}
        self.write_receipt(rejected)
        self.assertEqual(self.read(accepted=False)[0]["child_pid"], None)
        for mutation in ({"child_pid": 123}, {"original_command_sha256": self.digest}):
            with self.subTest(mutation=mutation):
                self.write_receipt(rejected | mutation)
                with self.assertRaises(AssertionError):
                    self.read(accepted=False)

    def test_live_bridge_must_descend_from_a_fresh_dispatcher_command(self):
        args = SimpleNamespace(rustty_viewer_key=self.directory / "key", rustty_bridge=self.bridge)
        desktop = {"account_sid": "desktop", "logon_id": 1, "session_id": 1}
        bridges = [{"pid": 20, "account_sid": "desktop", "logon_id": 2, "session_id": 0},
                   {"pid": 21, "account_sid": "desktop", "logon_id": 3, "session_id": 0}]
        with patch.object(windows_ssh, "dispatch_receipts", return_value=[self.receipt]), \
             patch.object(windows_ssh, "remote_images", return_value=bridges), \
             patch.object(windows_ssh, "run_ssh_json", return_value=[{"pid": 20}, {"pid": 21}]) as remote:
            evidence = windows_ssh.trusted_bridge_evidence(args, set(), self.command, desktop)
            self.assertEqual(evidence["bridge_identities"], bridges)
            remote.assert_called_once_with(args, "--descendants 123")
            remote.return_value = [{"pid": 20}]
            with self.assertRaises(AssertionError):
                windows_ssh.trusted_bridge_evidence(args, set(), self.command, desktop)
            remote.return_value = [{"pid": 20}, {"pid": 21}]
            bridges[0]["account_sid"] = "unrelated"
            with self.assertRaises(AssertionError):
                windows_ssh.trusted_bridge_evidence(args, set(), self.command, desktop)


    @contextmanager
    def image_probe(self, waits):
        def enumerate_processes(pids, capacity, used):
            pids[0] = 17
            used._obj.value = ctypes.sizeof(windows_ssh.w.DWORD)
            return 1

        def image_name(handle, flags, value, size):
            self.assertEqual(handle, 42)
            value.value = str(self.bridge)
            return 1

        with patch.object(windows_ssh, "K") as kernel, \
             patch.object(windows_ssh, "identity_from_handle") as identity:
            kernel.K32EnumProcesses.side_effect = enumerate_processes
            kernel.OpenProcess.return_value = 42
            kernel.QueryFullProcessImageNameW.side_effect = image_name
            kernel.WaitForSingleObject.side_effect = waits
            yield kernel, identity
            kernel.OpenProcess.assert_called_once_with(0x00101000, False, 17)
            kernel.CloseHandle.assert_called_once_with(42)

    def test_image_identity_retains_the_matched_handle_without_reopening_pid(self):
        with self.image_probe([258, 258]) as (kernel, identity):
            def inspect(handle, pid):
                kernel.CloseHandle.assert_not_called()
                return {"pid": pid, "account_sid": "matched-process"}
            identity.side_effect = inspect
            self.assertEqual(windows_ssh.identities_for_image(self.bridge),
                             [{"pid": 17, "account_sid": "matched-process"}])
            identity.assert_called_once_with(42, 17)

    def test_exit_during_identity_read_is_omitted_only_after_handle_is_signaled(self):
        with self.image_probe([258, 0]) as (_, identity):
            identity.side_effect = PermissionError("primary token disposed during exit")
            self.assertEqual(windows_ssh.identities_for_image(self.bridge), [])
            identity.assert_called_once_with(42, 17)
        with self.image_probe([0]) as (_, identity):
            self.assertEqual(windows_ssh.identities_for_image(self.bridge), [])
            identity.assert_not_called()
        with self.image_probe([258, 0]) as (_, identity):
            identity.return_value = {"pid": 17}
            self.assertEqual(windows_ssh.identities_for_image(self.bridge), [])

    def test_live_matched_process_identity_denial_still_fails_closed(self):
        with self.image_probe([258, 258]) as (_, identity):
            identity.side_effect = PermissionError("live matched process token denied")
            with self.assertRaisesRegex(PermissionError, "live matched"):
                windows_ssh.identities_for_image(self.bridge)


if __name__ == "__main__":
    unittest.main()
