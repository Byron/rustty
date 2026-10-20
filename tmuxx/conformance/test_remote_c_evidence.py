"""Remote provenance replay tests with mocked SSH; no C or host pass implied."""
import base64
import copy
import json
import re
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import remote_c_evidence as evidence


class RemoteEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="tx-remote-ledger-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        for name in ("key", "known_hosts"):
            (self.root / name).write_text("test marker; no credential")
        self.config = {"host": "user@example.invalid", "key": str(self.root / "key"),
                       "known_hosts": str(self.root / "known_hosts"), "ssh": sys.executable,
                       "binary": "/tmp/tmux reference", "version": "tmux next-3.9", "sha256": "a" * 64}
        self.path = self.root / "config.json"
        self.path.write_text(json.dumps(self.config))
        self.suite = {"id": "remote", "reference_version": "tmux next-3.9", "remote_fixtures": 2}
        self.probe = {"schema": 1, "binary": self.config["binary"], "version": self.config["version"],
                      "sha256": self.config["sha256"], "helper_sha256": evidence.oracle.digest(evidence.HERE / "remote_c_reference.py"),
                      "control_helper_sha256": evidence.oracle.CONTROL_SOURCE_SHA256,
                      "host": ["test-posix", "host", "release", "version", "arch"], "libc": ["test-libc", "1.0"], "pid": 12}

    def execute(self, command, **kwargs):
        self.assertEqual(command[-1], "--probe")
        self.assertEqual(command[-3:-1], ["--config", str(self.path)])
        self.assertEqual(kwargs["timeout"], 45)
        return subprocess.CompletedProcess(command, 0, json.dumps(self.probe).encode(), b"")

    def record(self):
        pinned = evidence.identity(self.suite, self.execute, self.path)
        lines = []
        for index in range(2):
            socket = f"/tmp/private-oracle-{index}/socket"
            identity = pinned["remote"] | {"pid": 100 + index, "server_pid": 200 + index, "socket": socket,
                                           "ownership": "linux-child-subreaper"}
            cleanup = {"schema": 1, "fixture": socket, "cleanup_remaining": [], "cleanup_errors": []}
            lines += ["TMUXX_REMOTE_C_IDENTITY=" + json.dumps(identity), "TMUXX_REMOTE_C_CLEANUP=" + json.dumps(cleanup)]
        stderr = "\n".join(lines)
        return {"remote_reference": pinned, "stderr_base64": base64.b64encode(stderr.encode()).decode(),
                "remote_fixtures": evidence.fixtures(self.suite, stderr, pinned)}

    def test_probe_binds_config_helpers_ssh_credentials_and_exact_remote_identity(self):
        details = self.record()
        evidence.verify(self.suite, details, self.execute)
        self.assertEqual(set(details["remote_reference"]),
                         {"config", "helper", "support", "control_support", "ssh", "key", "known_hosts", "remote", "observer_sources"})
        for field in ("schema", "binary", "version", "sha256", "helper_sha256", "host", "libc"):
            original = self.probe[field]
            self.probe[field] = "wrong"
            with self.subTest(field=field), self.assertRaises(ValueError):
                evidence.identity(self.suite, self.execute, self.path)
            self.probe[field] = original

    def test_raw_ssh_resolves_exact_credentials_and_rechecks_saved_identity(self):
        (self.root / "identity").write_bytes(b"private fixture marker")
        config = self.root / "ssh-config"
        config.write_text("Host fixture\n")
        selected = {"config": {"path": str(config)}, "ssh": {"path": sys.executable}, "host": "fixture"}
        settings = ("batchmode yes\nidentitiesonly yes\nstricthostkeychecking true\nrequesttty false\n"
                    f"identityfile {self.root / 'identity'}\nuserknownhostsfile {self.root / 'known_hosts'}\n")
        def resolve(command, **kwargs):
            self.assertEqual(command[1:], ["-G", "-T", "-F", str(config.resolve()), "--", "fixture"])
            return subprocess.CompletedProcess(command, 0, settings.encode(), b"")
        result = evidence.system_ssh_identity(resolve, selected)
        self.assertEqual(set(result), {"config", "ssh", "host", "key", "known_hosts"})
        self.assertEqual(evidence.system_ssh_identity(resolve, result), result)
        for changed in (settings.replace("requesttty false", "requesttty true"),
                        settings.replace("identitiesonly yes", "identitiesonly no"),
                        settings + "identityfile unrecorded-key\n"):
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                evidence.system_ssh_identity(
                    lambda command, **kwargs: subprocess.CompletedProcess(command, 0, changed.encode(), b""), selected)
        (self.root / "identity").write_bytes(b"changed fixture marker")
        self.assertNotEqual(evidence.system_ssh_identity(resolve, result), result)

    def test_each_fixture_needs_unique_incarnation_and_matching_complete_cleanup(self):
        details = self.record()
        original = details["remote_fixtures"]
        cases = []
        value = copy.deepcopy(original); value["identity"].pop(); cases.append(value)
        value = copy.deepcopy(original); value["identity"][1] = value["identity"][0]; cases.append(value)
        value = copy.deepcopy(original); value["identity"][0]["sha256"] = "b" * 64; cases.append(value)
        value = copy.deepcopy(original); value["cleanup"][0]["cleanup_remaining"] = [42]; cases.append(value)
        value = copy.deepcopy(original); value["cleanup"][0]["cleanup_errors"] = ["timeout"]; cases.append(value)
        value = copy.deepcopy(original); value["cleanup"][0]["fixture"] = "unknown"; cases.append(value)
        value = copy.deepcopy(original); value["cleanup"][1] = value["cleanup"][0]; cases.append(value)
        value = copy.deepcopy(original); value["cleanup"].pop(); cases.append(value)
        for value in cases:
            stderr = "\n".join(prefix + json.dumps(record) for kind, prefix in (
                ("identity", "TMUXX_REMOTE_C_IDENTITY="), ("cleanup", "TMUXX_REMOTE_C_CLEANUP=")) for record in value[kind])
            with self.subTest(value=value), self.assertRaises(ValueError):
                evidence.fixtures(self.suite, stderr, details["remote_reference"])

    def test_gate_reprobe_rejects_changed_config_known_hosts_key_or_saved_records(self):
        for field in ("config", "known_hosts", "key", "remote-host", "records"):
            details = self.record()
            path = self.path if field == "config" else self.root / field
            original = path.read_bytes() if path.is_file() else None
            host = self.probe["host"][:]
            if field == "config": self.path.write_text(json.dumps(self.config, indent=2))
            elif field in ("known_hosts", "key"): path.write_bytes(b"different test marker")
            elif field == "remote-host": self.probe["host"][1] = "replacement-host"
            elif field == "records": details["remote_fixtures"]["cleanup"].clear()
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "changed"):
                evidence.verify(self.suite, details, self.execute)
            if original is not None: path.write_bytes(original)
            self.probe["host"] = host

    def test_windows_format_counterparts_keep_complete_unix_matrices(self):
        tests = evidence.HERE.parent / "crates/tmuxx-config/tests"
        original = (tests / "c_reference.rs").read_text(encoding="utf-8")
        portable = (tests / "remote_c_reference.rs").read_text(encoding="utf-8")
        def function(source, name):
            start = source.index("fn " + name + "()")
            end = source.find("\n#[test]", start)
            return source[start:len(source) if end < 0 else end].strip()
        def without_layout(source):
            # Ignore formatting only outside string/character literals.
            output, quote, escaped = [], None, False
            for index, char in enumerate(source):
                if quote:
                    output.append(char)
                    if escaped: escaped = False
                    elif char == "\\": escaped = True
                    elif char == quote: quote = None
                elif char in ('"', "'"):
                    output.append(char); quote = char
                elif char == "," and source[index + 1:].lstrip().startswith(("]", ")")):
                    pass  # rustfmt may add/remove optional trailing commas.
                elif not char.isspace():
                    output.append(char)
            return "".join(output)
        for name in ("styled_width_and_trimming_match_c_bytes", "text_parser_and_argv_match_c_execution",
                     "regex_and_substitution_observations_match_c"):
            expected = function(original, name)
            expected = re.sub(r'OsStr::new\(("[^"\n]*")\)', r'b\1.as_slice()', expected)
            expected = re.sub(r'OsStr::from_bytes\(([^()]*)\)', r'\1', expected)
            expected = expected.replace("OsString::from_vec(arg.to_vec())", "arg.to_vec()")
            expected = expected.replace("reference.run(argv);", "reference.run(argv.iter().map(|arg| arg.to_vec()));")
            self.assertEqual(without_layout(expected), without_layout(function(portable, name)), name)

    def test_observers_need_exact_names_sources_and_complete_descendant_cleanup(self):
        details = self.record()
        suite = self.suite | {"remote_observers": ["native_input"]}
        stderr = base64.b64decode(details["stderr_base64"]).decode()
        observer = {"schema": 1, "name": "native_input", "fixture": "/tmp/private-oracle-0/socket",
                    "sources": {relative: evidence.oracle.OBSERVER_SOURCES[relative]
                                for relative in evidence.oracle.OBSERVERS["native_input"]},
                    "cleanup_remaining": [], "cleanup_errors": []}
        def collected(value):
            return evidence.fixtures(suite, stderr + "\nTMUXX_REMOTE_C_OBSERVER=" + json.dumps(value), details["remote_reference"])
        self.assertEqual(collected(observer)["observers"], [observer])
        for change in ({"name": "unknown"}, {"sources": {}}, {"fixture": "/unowned"},
                       {"cleanup_remaining": [123]}, {"cleanup_errors": ["timeout"]}, {"schema": True}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                collected(observer | change)
        with self.assertRaises(ValueError):
            evidence.fixtures(suite, stderr, details["remote_reference"])


if __name__ == "__main__":
    unittest.main()
