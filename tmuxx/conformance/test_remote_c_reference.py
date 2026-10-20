"""Local transport/ownership tests only; no remote C qualification is claimed."""
import io
import base64
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch

import remote_c_reference as oracle
import remote_c_control as controls


def framed(payload):
    stream = io.BytesIO()
    oracle.write_frame(stream, payload)
    return stream.getvalue()


class KeptBytes(io.BytesIO):
    def close(self):
        self.was_closed = True


class Fragmented(io.BytesIO):
    def read(self, length=-1):
        return super().read(min(length, 1) if length >= 0 else 1)


class RemoteOracleTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="tx-oracle-local-")
        self.addCleanup(self.directory.cleanup)
        root = Path(self.directory.name)
        for name in ("key", "known hosts"):
            (root / name).write_bytes(b"private fixture marker, not a credential")
        self.config = {"host": "oracle@example.invalid", "port": 2222,
                       "key": str(root / "key"), "known_hosts": str(root / "known hosts"),
                       "binary": "/private C 雪/tmux;$(not-shell)", "version": "tmux next-3.9", "sha256": "a" * 64}
        self.identity = {"schema": 1, "binary": self.config["binary"], "version": self.config["version"],
                         "sha256": self.config["sha256"], "helper_sha256": oracle.digest(oracle.__file__),
                         "socket": "/tmp/tx-oracle-fixture/socket", "host": ["test-posix", "host", "release", "version", "arch"],
                         "pid": 101, "server_pid": 102}
        self.ready = oracle.response(0, json.dumps(self.identity).encode())
        self.closed = oracle.response(0, json.dumps({"schema": 1, "fixture": self.identity["socket"],
                                                   "cleanup_remaining": [], "cleanup_errors": []}).encode())

    def child(self, responses):
        child = MagicMock()
        child.stdin = KeptBytes()
        child.stdout = KeptBytes(b"".join(framed(payload) for payload in responses))
        child.wait.return_value = 0
        return child

    def sent(self, child):
        source = Path(oracle.__file__).read_bytes()
        stream = io.BytesIO(child.stdin.getvalue())
        self.assertEqual(stream.read(len(source)), source)
        records = []
        while (frame := oracle.read_frame(stream)) is not None:
            records.append(oracle.unpack_request(frame))
        return records

    def test_byte_fragmentation_and_argument_stdin_identity(self):
        args = [b"display", "雪 spaces & $()".encode(), b"\xff\xfe\\"]
        payload = oracle.request(1, args, bytes(range(256)))
        self.assertEqual(oracle.read_frame(Fragmented(framed(payload))), payload)
        self.assertEqual(oracle.unpack_request(payload), (1, args, bytes(range(256))))
        returned = oracle.response(-9, bytes(range(256)), b"\xff\0diagnostic")
        self.assertEqual(oracle.unpack_response(oracle.read_frame(Fragmented(framed(returned)))),
                         (-9, bytes(range(256)), b"\xff\0diagnostic"))

    def test_error_exit_with_held_stdin_does_not_abort_python(self):
        script = """import sys, threading, time
import remote_c_reference as oracle
source = oracle.local_stdin()
started = threading.Event()
class Marked:
    def read(self, length):
        started.set()
        return source.read(length)
oracle.TimedFrames(Marked())
assert started.wait(1)
time.sleep(.05)
raise SystemExit(7)
"""
        child = subprocess.Popen([sys.executable, "-c", script], cwd=Path(oracle.__file__).parent,
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            # Keep stdin open through process exit, exactly as a Rust caller
            # does when the oracle rejects a failed/ended remote incarnation.
            self.assertEqual(child.wait(timeout=5), 7)
            self.assertNotIn(b"Fatal Python error", child.stderr.read())
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)
            child.stdin.close()
            child.stdout.close()
            child.stderr.close()

    def test_malformed_lengths_nul_trailing_data_and_count_reject(self):
        payload = oracle.request(1, [b"x"], b"y")
        for malformed in (payload[:4], payload[:-1], payload + b"x", b"\1" + struct.pack("!I", 257) + b"0000",
                          b"\1" + struct.pack("!I", 1) + struct.pack("!I", 1) + b"\0" + struct.pack("!I", 0)):
            with self.subTest(malformed=malformed), self.assertRaises(ValueError):
                oracle.unpack_request(malformed)
        with self.assertRaises(ValueError):
            oracle.read_frame(io.BytesIO(struct.pack("!I", oracle.MAX_FRAME + 1)))
        for malformed in (b"\0", b"\0\0\0\4ab"):
            with self.assertRaises(EOFError):
                oracle.read_frame(io.BytesIO(malformed))
        with self.assertRaises(ValueError):
            oracle.request(1, [b"bad\0arg"])
        with self.assertRaises(ValueError):
            oracle.unpack_response(oracle.response(0, b"x") + b"trailing")

    def test_ssh_uses_pinned_credentials_and_fixed_no_pty_bootstrap(self):
        oracle.validate_config(self.config)
        command = oracle.ssh_command(self.config, 123)
        self.assertIn("-T", command)
        for setting in ("BatchMode=yes", "IdentitiesOnly=yes", "StrictHostKeyChecking=yes"):
            self.assertIn(setting, command)
        self.assertEqual(command[-2], self.config["host"])
        self.assertTrue(command[-1].startswith("exec python3 -c "))
        for private in (self.config["binary"], self.config["version"], self.config["sha256"]):
            self.assertNotIn(private, command[-1])
        for changed in ({"host": "-oProxyCommand=bad"}, {"host": "user host"}, {"port": True},
                        {"sha256": "unknown"}, {"binary": "relative/tmux"}, {"version": "tmuxx 1"},
                        {"key": str(Path(self.directory.name) / "missing")}):
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                oracle.validate_config(self.config | changed)

    def test_bridge_preserves_binary_responses_and_proves_cleanup(self):
        mutation = oracle.request(1, [b"display", b"-I"], bytes(range(256)))
        reply = oracle.response(0, b"\0\xffout\r\n", b"\xfeerr")
        child = self.child([self.ready, reply, self.closed])
        output = io.BytesIO()
        with patch.object(oracle.subprocess, "Popen", return_value=child) as spawn:
            oracle.bridge(self.config, io.BytesIO(framed(mutation) + framed(oracle.request(3))), output)
        self.assertEqual(output.getvalue(), framed(self.ready) + framed(reply) + framed(self.closed))
        self.assertEqual([record[0] for record in self.sent(child)], [0, 1, 3])
        self.assertTrue(child.stdin.was_closed)
        child.kill.assert_not_called()
        spawn.assert_called_once()

    def test_initial_configuration_and_pinned_support_precede_actual_startup(self):
        configuration = b"set -g @runs '\xff once'\nrun-shell 'wait-for-release'\n"
        child = self.child([self.ready, self.closed])
        output = io.BytesIO()
        incoming = framed(oracle.request(22, data=configuration)) + framed(oracle.request(3))
        with patch.object(oracle.subprocess, "Popen", return_value=child):
            oracle.bridge(self.config, io.BytesIO(incoming), output, configuration_stdin=True)
        records = self.sent(child)
        self.assertEqual([record[0] for record in records], [22, 3])
        self.assertEqual(records[0][2], configuration)
        self.assertEqual(records[0][1][-1], oracle.control_source())
        self.assertEqual(hashlib.sha256(records[0][1][-1]).hexdigest(), oracle.CONTROL_SOURCE_SHA256)
        self.assertTrue(child.stdin.was_closed)

    def test_invalid_startup_configuration_cannot_open_ssh(self):
        for request, environment, probe in ((oracle.request(1), False, False),
                                            (oracle.request(22, [b"unapproved"]), False, False),
                                            (oracle.request(22), True, False), (oracle.request(22), False, True)):
            with self.subTest(request=request, environment=environment, probe=probe), \
                 patch.object(oracle.subprocess, "Popen") as spawn, self.assertRaises(ValueError):
                oracle.bridge(self.config, io.BytesIO(framed(request)), io.BytesIO(),
                              environment_stdin=environment, configuration_stdin=True, probe=probe)
            spawn.assert_not_called()

    def test_persistent_feature_flags_preserve_endpoint_and_client_environment(self):
        manager = controls.Controls(b"/private/tmux", b"/private/socket", {b"TERM": b"base"}, b"/private/files")
        child = SimpleNamespace(child=SimpleNamespace(pid=101))
        environment = {b"TERM": b"tmuxx-identity-test"}
        with patch.object(controls, "BufferedChild", return_value=child) as spawn:
            manager.open([b"-T", b"RGB,hyperlinks", b"display", b"-p", b"#{client_termname}"],
                         control=False, environment=environment)
        self.assertEqual(spawn.call_args.args,
                         ([b"/private/tmux", b"-N", b"-f", b"/dev/null", b"-S", b"/private/socket",
                           b"-T", b"RGB,hyperlinks", b"display", b"-p", b"#{client_termname}"],
                          environment, b"/private/files"))
        self.assertEqual(manager.env, {b"TERM": b"base"})
        for flags in ([b"-2", b"-TRGB,clipboard", b"-u", b"-q"],
                      [b"-T256,rgb", b"-T256@", b"-2"],
                      [b"-2", b"-T256@,focus", b"-Trgb:unknown:mouse"]):
            with self.subTest(flags=flags), patch.object(controls, "BufferedChild", return_value=child) as spawn:
                manager.open(flags + [b"-C", b"attach"], control=False)
            self.assertEqual(spawn.call_args.args[0], manager.prefix + flags + [b"-C", b"attach"])
        for args in ([b"-S", b"/other", b"display"], [b"-T", b"RGB", b"-S", b"/other", b"display"],
                     [b"-T", b"RGB;bad", b"display"], [b"-T", b"RGB"], [b"-f", b"/other", b"display"]):
            with self.subTest(args=args), patch.object(controls, "BufferedChild") as spawn, self.assertRaises(ValueError):
                manager.open(args)
            spawn.assert_not_called()

    def test_persistent_environment_injects_only_pinned_support(self):
        value = b"tmuxx-identity-test"
        environment = struct.pack("!II", 1, 4) + b"TERM" + struct.pack("!I", len(value)) + value
        opening = oracle.request(25, [b"control", b"-T", b"RGB,hyperlinks", b"attach", b"-t", b"test"], environment)
        child = self.child([self.ready, oracle.response(0, struct.pack("!II", 1, 2)), self.closed])
        with patch.object(oracle.subprocess, "Popen", return_value=child):
            oracle.bridge(self.config, io.BytesIO(framed(opening) + framed(oracle.request(3))), io.BytesIO())
        records = self.sent(child)
        self.assertEqual(records[1][0], 25)
        self.assertEqual(records[1][2], environment + oracle.control_source())

    def test_mismatched_identity_precedes_mutation_and_closes_ssh(self):
        for field in ("schema", "binary", "version", "sha256", "helper_sha256"):
            changed = self.identity | {field: "wrong"}
            child = self.child([oracle.response(0, json.dumps(changed).encode())])
            with self.subTest(field=field), patch.object(oracle.subprocess, "Popen", return_value=child), \
                 self.assertRaisesRegex(ValueError, "handshake mismatches"):
                oracle.bridge(self.config, io.BytesIO(framed(oracle.request(1, [b"set", b"@x", b"y"]))), io.BytesIO())
            self.assertEqual([record[0] for record in self.sent(child)], [0])
            self.assertTrue(child.stdin.was_closed)

    def test_probe_uses_identity_operation_without_starting_a_fixture(self):
        child = self.child([self.ready])
        with patch.object(oracle.subprocess, "Popen", return_value=child):
            self.assertEqual(oracle.bridge(self.config, io.BytesIO(), io.BytesIO(), probe=True), self.identity)
        self.assertEqual([record[0] for record in self.sent(child)], [4])
        self.assertTrue(child.stdin.was_closed)
        signals = SimpleNamespace(SIGALRM=14, SIGTERM=15, SIGHUP=1, signal=MagicMock(), alarm=MagicMock())
        handshake = framed(oracle.request(4, [b"/tmux", b"tmux next-3.9", b"a" * 64, b"b" * 64]))
        with patch.object(oracle, "OracleFixture") as fixture, patch.object(oracle, "reference_identity", return_value=self.identity), \
             patch.object(oracle, "signal", signals), \
             patch.object(oracle.sys, "stdin", SimpleNamespace(buffer=io.BytesIO(handshake))), \
             patch.object(oracle.sys, "stdout", SimpleNamespace(buffer=io.BytesIO())):
            oracle.agent_main()
        fixture.assert_not_called()

    def test_ambiguous_eof_never_retries_and_timeout_reaps_only_owned_ssh(self):
        mutation = oracle.request(1, [b"set", b"@counter", b"1"])
        child = self.child([self.ready])
        with patch.object(oracle.subprocess, "Popen", return_value=child) as spawn, \
             self.assertRaisesRegex(EOFError, "ambiguous operation"):
            oracle.bridge(self.config, io.BytesIO(framed(mutation)), io.BytesIO())
        self.assertEqual([record[0] for record in self.sent(child)], [0, 1])
        spawn.assert_called_once()
        child = self.child([])
        child.wait.side_effect = [subprocess.TimeoutExpired("ssh", 12), 0]
        remote, local = MagicMock(), MagicMock()
        remote.next.side_effect = [self.ready, TimeoutError("fixture deadline")]
        local.next.return_value = mutation
        with patch.object(oracle.subprocess, "Popen", return_value=child), \
             patch.object(oracle, "TimedFrames", side_effect=[remote, local]), \
             self.assertRaisesRegex(TimeoutError, "fixture deadline"):
            oracle.bridge(self.config, io.BytesIO(), io.BytesIO())
        child.kill.assert_called_once()
        self.assertEqual(child.wait.call_count, 2)
        self.assertEqual([record[0] for record in self.sent(child)], [0, 1])

    def test_cleanup_result_cannot_hide_remaining_processes_or_missing_record(self):
        for value in ({}, {"schema": True, "cleanup_remaining": [], "cleanup_errors": []},
                      {"schema": 1, "cleanup_remaining": [123], "cleanup_errors": []},
                      {"schema": 1, "cleanup_remaining": [], "cleanup_errors": ["timeout"]}):
            child = self.child([self.ready, oracle.response(0, json.dumps(value).encode())])
            with self.subTest(value=value), patch.object(oracle.subprocess, "Popen", return_value=child), \
                 self.assertRaisesRegex(ValueError, "prove complete cleanup"):
                oracle.bridge(self.config, io.BytesIO(framed(oracle.request(3))), io.BytesIO())

    def test_agent_eof_malformed_input_and_deadline_all_close_fixture(self):
        handshake = framed(oracle.request(0, [b"/tmux", b"tmux next-3.9", b"a" * 64, b"b" * 64]))
        for tail, error in ((b"", None), (b"\0\0", EOFError), (framed(oracle.request(7)), ValueError)):
            fixture = MagicMock()
            fixture.identity = self.identity
            fixture.close.return_value = {"schema": 1, "cleanup_remaining": [], "cleanup_errors": []}
            signals = SimpleNamespace(SIGALRM=14, SIGTERM=15, SIGHUP=1, signal=MagicMock(), alarm=MagicMock())
            with self.subTest(tail=tail), patch.object(oracle, "OracleFixture", return_value=fixture), \
                 patch.object(oracle, "signal", signals), \
                 patch.object(oracle.sys, "stdin", SimpleNamespace(buffer=io.BytesIO(handshake + tail))), \
                 patch.object(oracle.sys, "stdout", SimpleNamespace(buffer=io.BytesIO())):
                if error:
                    with self.assertRaises(error): oracle.agent_main()
                else:
                    oracle.agent_main()
                fixture.close.assert_called_once()
                signals.alarm.assert_any_call(oracle.LEASE_SECONDS)
                signals.alarm.assert_called_with(0)
                expire = signals.signal.call_args_list[0].args[1]
                with self.assertRaises(TimeoutError): expire()

    def test_foreground_server_cleanup_escalates_only_its_process_handle(self):
        fixture = object.__new__(oracle.OracleFixture)
        fixture.server = MagicMock()
        fixture.server.poll.return_value = None
        fixture.server.wait.side_effect = [subprocess.TimeoutExpired("owned server", 5), 0]
        fixture.directory = MagicMock()
        fixture.identity = self.identity
        with patch.object(oracle.os, "kill") as numeric_kill, patch.object(oracle, "reclaim_owned_children", return_value=[]):
            self.assertEqual(fixture.close(), {"schema": 1, "fixture": self.identity["socket"],
                                              "cleanup_remaining": [], "cleanup_errors": []})
        fixture.server.terminate.assert_called_once()
        fixture.server.kill.assert_called_once()
        fixture.directory.cleanup.assert_called_once()
        numeric_kill.assert_not_called()

    def test_adopted_children_are_killed_only_while_owned_and_all_generations_reaped(self):
        self.enterContext(patch.object(oracle.os, "WNOHANG", 1, create=True))
        self.enterContext(patch.object(oracle.signal, "SIGKILL", 9, create=True))
        # The second orphan generation appears after its parent is killed.
        with patch.object(oracle, "owned_children", side_effect=[[10], [10, 11], [11], []]), \
             patch.object(oracle.os, "waitpid", side_effect=[(0, 0), (10, 9), (0, 0), (11, 9)]) as wait, \
             patch.object(oracle.os, "kill") as kill, patch.object(oracle.time, "sleep"):
            self.assertEqual(oracle.reclaim_owned_children(), [])
        self.assertEqual([call.args[0] for call in kill.call_args_list], [10, 11])
        self.assertEqual([call.args[0] for call in wait.call_args_list], [10, 10, 11, 11])
        with patch.object(oracle, "owned_children", side_effect=[[42], []]), \
             patch.object(oracle.os, "waitpid", side_effect=ChildProcessError), \
             patch.object(oracle.os, "kill") as kill, patch.object(oracle.time, "sleep"):
            self.assertEqual(oracle.reclaim_owned_children(), [])
        kill.assert_not_called()

    def test_remote_lease_interrupt_cleans_an_already_started_fixture(self):
        fixture = MagicMock()
        fixture.identity = self.identity
        fixture.close.return_value = {"schema": 1, "cleanup_remaining": [], "cleanup_errors": []}
        handshake = oracle.request(0, [b"/tmux", b"tmux next-3.9", b"a" * 64, b"b" * 64])
        signals = SimpleNamespace(SIGALRM=14, SIGTERM=15, SIGHUP=1, signal=MagicMock(), alarm=MagicMock())
        with patch.object(oracle, "OracleFixture", return_value=fixture), patch.object(oracle, "signal", signals), \
             patch.object(oracle, "read_frame", side_effect=[handshake, TimeoutError("lease expired")]), \
             patch.object(oracle.sys, "stdout", SimpleNamespace(buffer=io.BytesIO())), \
             self.assertRaisesRegex(TimeoutError, "lease expired"):
            oracle.agent_main()
        fixture.close.assert_called_once()

    def test_dead_server_or_changed_binary_never_launches_replacement(self):
        fixture = object.__new__(oracle.OracleFixture)
        fixture.server = MagicMock()
        fixture.server.poll.return_value = 1
        with patch.object(oracle.subprocess, "run") as execute, self.assertRaisesRegex(RuntimeError, "incarnation ended"):
            fixture.command([b"set", b"@x", b"y"])
        execute.assert_not_called()
        fixture.server.poll.return_value = None
        binary = Path(self.directory.name) / "changed reference"
        binary.write_bytes(b"changed")
        fixture.binary = str(binary).encode()
        fixture.expected_hash = "0" * 64
        with patch.object(oracle.subprocess, "run") as execute, self.assertRaisesRegex(ValueError, "bytes differ"):
            fixture.command([b"set", b"@x", b"y"])
        execute.assert_not_called()

    def test_command_does_not_inject_observation_commands_or_mutate_environment(self):
        fixture = object.__new__(oracle.OracleFixture)
        fixture.server = MagicMock()
        fixture.server.poll.return_value = None
        fixture.binary, fixture.socket = b"/private/tmux", b"/private/socket"
        fixture.files = b"/private/files"
        fixture.env = {b"ORIGINAL": b"kept", b"REMOVED": b"gone"}
        fixture.ensure_identity = MagicMock()
        completed = subprocess.CompletedProcess([], 7, b"\xffout", b"\0err")
        with patch.object(oracle.subprocess, "run", return_value=completed) as execute:
            result = fixture.command([b"set-hook", b"-g", b"after-list-panes", b"display-message hook"],
                                     b"\xfeinput", {b"REMOVED": None, b"REQUEST": "雪 &".encode()})
        self.assertEqual(oracle.unpack_response(result), (7, b"\xffout", b"\0err"))
        execute.assert_called_once_with(
            [b"/private/tmux", b"-N", b"-f", b"/dev/null", b"-S", b"/private/socket",
             b"set-hook", b"-g", b"after-list-panes", b"display-message hook"],
            input=b"\xfeinput", capture_output=True, timeout=oracle.COMMAND_TIMEOUT,
            env={b"ORIGINAL": b"kept", b"REQUEST": "雪 &".encode()}, cwd=b"/private/files")
        self.assertEqual(fixture.env, {b"ORIGINAL": b"kept", b"REMOVED": b"gone"})
        self.assertEqual(fixture.ensure_identity.call_count, 2)

    def test_environment_framing_preserves_bytes_and_rejects_ambiguous_names(self):
        def packet(pairs):
            data = struct.pack("!I", len(pairs))
            for pair in pairs:
                for value in pair:
                    data += struct.pack("!I", 0xffffffff if value is None else len(value))
                    if value is not None:
                        data += value
            return data
        environment = [(b"BYTES", b"\xff\xfe"), ("雪".encode(), "& value".encode()), (b"REMOVE", None)]
        self.assertEqual(oracle.unpack_environment(packet(environment) + b"\0stdin"),
                         (dict(environment), b"\0stdin"))
        for pairs in ([(b"", b"value")], [(None, b"value")], [(b"a=b", b"value")],
                      [(b"a\0b", b"value")], [(b"name", b"\0")], [(b"x", b"1"), (b"x", b"2")]):
            with self.subTest(pairs=pairs), self.assertRaises(ValueError):
                oracle.unpack_environment(packet(pairs))
        encoded = packet(environment)
        for end in range(len(encoded)):
            with self.subTest(end=end), self.assertRaises(ValueError):
                oracle.unpack_environment(encoded[:end])

    def test_private_file_names_are_byte_exact_and_cannot_traverse(self):
        fixture = object.__new__(oracle.OracleFixture)
        fixture.files = b"/private-oracle/files"
        fixture.ensure_identity = lambda: None
        for name in (b"snow \xff & value", "雪.txt".encode()):
            self.assertEqual(oracle.unpack_response(fixture.file(8, name)),
                             (0, fixture.files + b"/" + name, b""))
        for name in (b"", b".", b"..", b"../value", b"a/b", b"a\\b", b"x\0", b"x" * 256):
            with self.subTest(name=name), self.assertRaises(ValueError):
                fixture.file(8, name)

    def test_approved_observer_bundle_pins_every_source_and_preserves_binary_input(self):
        for name, relatives in oracle.OBSERVERS.items():
            bundle = json.loads(oracle.observer_bundle(name, bytes(range(256))))
            self.assertEqual(base64.b64decode(bundle["input"]), bytes(range(256)))
            self.assertEqual(set(bundle["sources"]), set(relatives))
            for relative, encoded in bundle["sources"].items():
                self.assertEqual(hashlib.sha256(base64.b64decode(encoded)).hexdigest(),
                                 oracle.OBSERVER_SOURCES[relative])
        with self.assertRaises(ValueError):
            oracle.observer_bundle("../unapproved", b"")
        with patch.object(oracle.Path, "read_bytes", return_value=b"changed reviewed source"), self.assertRaises(ValueError):
            oracle.observer_bundle("native_input", b"")



if __name__ == "__main__":
    unittest.main()
