"""Forced-dispatcher evidence checks; these never connect to SSH or open a GUI."""
import base64
from contextlib import contextmanager
import ctypes
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

native_wire = None
if importlib.util.find_spec("msgpack") and importlib.util.find_spec("lz4"):
    import native_wire

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
                         "$p = New-Object System.Diagnostics.ProcessStartInfo; $p.FileName = '"
                         + str(self.bridge).replace("'", "''")
                         + "'; $p.Arguments = 'tmux --native-stdio'; $p.UseShellExecute = $false; $p.CreateNoWindow = $true; "
                         "$c = [System.Diagnostics.Process]::Start($p); $c.WaitForExit(); exit $c.ExitCode")

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


@unittest.skipUnless(native_wire, "install conformance/requirements-native.txt for native wire tests")
class NativeFrames(unittest.TestCase):
    def test_binary_compression_fragmentation_and_record_boundaries(self):
        first = {"type": "pane_output", "data": bytes(range(256)) * 2048}
        second = {"version": 2, "instance": "雪"}
        wire = native_wire.encode(first)
        self.assertTrue(wire[4] & native_wire.COMPRESSED)
        self.assertFalse(wire[4] & native_wire.FINAL)
        source = io.BytesIO(wire + native_wire.encode(second))
        read = lambda size: source.read(min(size, 3))
        self.assertEqual(native_wire.read_record(read), first)
        self.assertEqual(native_wire.read_record(read), second)
        self.assertEqual(source.read(), b"")
        with self.assertRaises(EOFError):
            native_wire.read_record(io.BytesIO(wire[:-1]).read)

    def test_raw_record_and_declared_limits_before_payload_read(self):
        raw = {"version": 2}
        wire = native_wire.encode(raw)
        self.assertEqual(wire[4], native_wire.FINAL)
        self.assertEqual(native_wire.read_record(io.BytesIO(wire).read), raw)
        header = native_wire.HEADER
        cases = [
            (b"nope", 1, b"\0\0\0", 1, 1),
            (b"TMX2", 4, b"\0\0\0", 1, 1),
            (b"TMX2", 1, b"\0\0x", 1, 1),
            (b"TMX2", 1, b"\0\0\0", 0, 0),
            (b"TMX2", 0, b"\0\0\0", 1, 1),
            (b"TMX2", 1, b"\0\0\0", 1, 2),
            (b"TMX2", 3, b"\0\0\0", 2, 2),
            (b"TMX2", 3, b"\0\0\0", 1, native_wire.BLOCK_BYTES + 1),
        ]
        for values in cases:
            with self.subTest(header=values), self.assertRaises(ValueError):
                native_wire.read_record(io.BytesIO(header.pack(*values)).read)
        with self.assertRaisesRegex(ValueError, "record exceeds"):
            native_wire.read_record(io.BytesIO(wire[:header.size]).read, max_bytes=1)

    def test_corrupt_compression_and_trailing_messagepack_are_rejected(self):
        wire = native_wire.HEADER.pack(b"TMX2", 3, b"\0\0\0", 1, 64) + b"\xff"
        with self.assertRaises(native_wire.lz4.block.LZ4BlockError):
            native_wire.read_record(io.BytesIO(wire).read)
        payload = native_wire.msgpack.packb({}) * 2
        wire = native_wire.HEADER.pack(b"TMX2", 1, b"\0\0\0", len(payload), len(payload)) + payload
        with self.assertRaises(native_wire.msgpack.ExtraData):
            native_wire.read_record(io.BytesIO(wire).read)


if __name__ == "__main__":
    unittest.main()
