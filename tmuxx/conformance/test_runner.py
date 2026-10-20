"""Release-gate checks; these do not start a tmux process."""

import tempfile
import unittest
import hashlib
import json
import copy
import base64
import os
import signal
import sys
import time
from types import SimpleNamespace
from pathlib import Path
import subprocess
from unittest.mock import MagicMock, patch

import runner


class InventoryTests(unittest.TestCase):
    def version_suites(self):
        return {name: {"id": name, "reference": role, "reference_version": version, "profiles": ["system-client"],
                       "platforms": ["darwin", "linux", "win32"],
                       "args": ["test"], "assertions": [{"id": name + ".exact", "tests": ["pairings"]}]}
                for name, role, version in [("current", "current", "tmux next-3.9"), ("older", "older", "tmux 3.7c")]}

    def test_version_suites_require_explicit_distinct_matching_references(self):
        suite = self.version_suites()["older"]
        current = {"version": "tmux next-3.9", "sha256": "current"}
        older = {"version": "tmux 3.7c", "sha256": "older"}
        self.assertEqual(runner.suite_reference(suite, {"current": current, "older": older}), older)
        for references, error in [
            ({"current": current}, "requires --older-reference"),
            ({"current": current, "older": current}, "requires tmux 3.7c"),
            ({"current": older, "older": older}, "must be different"),
        ]:
            with self.subTest(references=references), self.assertRaisesRegex(ValueError, error):
                runner.suite_reference(suite, references)

    def test_full_collection_dispatches_both_references_and_detects_changed_binary(self):
        for changed in (False, True):
            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                identities = {}
                for role, version in [("current", "tmux next-3.9"), ("older", "tmux 3.7c"), ("candidate", "tmuxx 0.1.0")]:
                    path = root / role
                    path.write_text(role)
                    identities[str(path)] = {"path": str(path), "version": version, "sha256": runner.digest(path)}
                selected = self.version_suites()
                entries = [{"id": name + ".exact", "requirement": "required", "status": "pending", "suites": [name]} for name in selected]
                calls = []
                def run(suite, reference):
                    calls.append((suite["id"], reference))
                    if changed and suite["id"] == "older":
                        (root / "older").write_text("replaced binary")
                    observations, _ = runner.suite_observations(suite, 0, "test pairings ... ok\n")
                    return observations, {"status": 0, "stdout_base64": runner.b64(b"test pairings ... ok\n")}
                args = SimpleNamespace(reference=str(root / "current"), older_reference=str(root / "older"),
                    candidate=str(root / "candidate"), candidate_kind="rust", suite=["all"], no_smoke=True,
                    scenario=None, report=str(root / "report.json"))
                with patch.object(runner, "suites", return_value=selected), patch.object(runner, "inventory", return_value=entries), \
                     patch.object(runner, "implementation_fingerprint", return_value="sources"), \
                     patch.object(runner, "executable_identity", side_effect=lambda path: identities[path]), \
                     patch.object(runner, "run_suite", side_effect=run):
                    self.assertEqual(runner.smoke(args), int(changed))
                report = json.loads((root / "report.json").read_text())
                self.assertEqual(calls, [("current", str(root / "current")), ("older", str(root / "older"))])
                self.assertEqual(report["suites"]["older"]["reference"], identities[str(root / "older")])
                self.assertEqual(report["references_changed_during_run"], changed)

    def test_gate_requires_both_exact_logs_and_unchanged_reference_identities(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            identities = {}
            for role, version in [("reference", "tmux next-3.9"), ("older_reference", "tmux 3.7c"), ("candidate", "tmuxx 0.1.0")]:
                path = root / role
                path.write_text(role)
                identities[role] = {"path": str(path), "version": version, "sha256": runner.digest(path)}
            selected = self.version_suites()
            entries = [{"id": name + ".exact", "profiles": ["system-client"], "requirement": "required", "status": "pending", "suites": [name]} for name in selected]
            entries.append({"id": "control.supported-version-matrix", "profiles": ["system-client"], "requirement": "required", "status": "pending", "proof": [name + ".exact" for name in selected]})
            config = {"profiles": {"system-client": {"platforms": ["darwin"]}}, "reference_revision": "baseline"}
            report = {"candidate_kind": "rust", "checkout_revision": "checkout", "reference_revision": "baseline",
                "manifest_sha256": runner.digest(runner.MANIFEST), "implementation_fingerprint": "sources", "platform": "darwin", "host": {"platform": "darwin"},
                **identities, "entries": [{**entry, "observation_results": {"suite:" + entry["suites"][0]: "pass"}} for entry in entries if "suites" in entry] + [{**entries[-1], "status": "pass"}],
                "suites": {name: {"reference": identities["reference" if name == "current" else "older_reference"],
                    "status": 0, "platform": "darwin", "stdout_base64": runner.b64(b"test pairings ... ok\n")} for name in selected}}
            with patch.object(runner, "suites", return_value=selected), patch.object(runner, "inventory", side_effect=lambda *_: copy.deepcopy(entries)), \
                 patch.object(runner, "source_revision", return_value="checkout"), patch.object(runner, "implementation_fingerprint", return_value="sources"), \
                 patch.object(runner.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)) as run:
                self.assertEqual(runner.blockers(report, "system-client", config), [])
                self.assertEqual(run.call_args.kwargs["cwd"], runner.c_source())
                for change in [
                    lambda value: value.pop("older_reference"),
                    lambda value: value["suites"].pop("older"),
                    lambda value: value["suites"]["older"].update(reference=value["reference"]),
                    lambda value: value["older_reference"].update(version="tmux 3.6a"),
                    lambda value: value["suites"]["older"].update(stdout_base64=runner.b64(b"running 0 tests\n")),
                    lambda value: value["suites"]["older"].update(stdout_base64=runner.b64(b"test pairings ... ignored\n")),
                    lambda value: value["suites"]["older"].update(status=1),
                    lambda value: value.update(older_reference=value["reference"]),
                ]:
                    forged = copy.deepcopy(report)
                    change(forged)
                    problems = runner.blockers(forged, "system-client", config)
                    self.assertIn("control.supported-version-matrix: failure", problems)
                (root / "older_reference").write_text("replaced")
                self.assertIn("older_reference executable is missing or changed since evidence was collected", runner.blockers(report, "system-client", config))

    def test_external_source_and_revision_changes_invalidate_fingerprint(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "crates/example").mkdir(parents=True)
            (root / "Cargo.toml").write_text("[workspace]\n")
            (root / "Cargo.lock").write_text("lock")
            with patch.object(runner, "rustty_root", return_value=root), patch.object(runner.subprocess, "check_output", return_value=b"base\n"):
                initial = runner.implementation_fingerprint()
                (root / "crates/example/unlisted.rs").write_text("changed source")
                updated = runner.implementation_fingerprint()
                self.assertNotEqual(initial, updated)
                with patch.object(runner.subprocess, "check_output", return_value=b"new-base\n"):
                    self.assertNotEqual(updated, runner.implementation_fingerprint())

    def test_shared_test_support_additions_changes_and_removal_invalidate_fingerprint(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            here = root / "tmuxx/conformance"
            support = root / "tmuxx/test-support"
            external = root / "external"
            for path in (here, support, external):
                path.mkdir(parents=True)
            for path in (root / "Cargo.toml", root / "Cargo.lock", here / "scope.json",
                         external / "Cargo.toml", external / "Cargo.lock"):
                path.write_text("fixture")
            with patch.object(runner, "ROOT", root), patch.object(runner, "HERE", here), \
                 patch.object(runner, "SCOPE", here / "scope.json"), \
                 patch.object(runner, "rustty_root", return_value=external), \
                 patch.object(runner.subprocess, "check_output", return_value=b"base\n"):
                initial = runner.implementation_fingerprint()
                helper = support / "socket_owner.rs"
                helper.write_text("fn verify_owner() {}\n")
                added = runner.implementation_fingerprint()
                self.assertNotEqual(initial, added)
                helper.write_text("fn verify_owner() { panic!(); }\n")
                self.assertNotEqual(added, runner.implementation_fingerprint())
                helper.unlink()
                self.assertEqual(initial, runner.implementation_fingerprint())

    def test_rustty_identity_tracks_workspace_revision_and_sources(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "crates/example").mkdir(parents=True)
            (root / "Cargo.toml").write_text("[workspace]\n")
            (root / "Cargo.lock").write_text("lock")
            source = root / "crates/example/lib.rs"
            source.write_text("original")
            with patch.object(runner, "rustty_root", return_value=root), patch.object(runner.subprocess, "check_output", return_value="base\n"):
                original = runner.rustty_identity()
                self.assertEqual(original["sources"]["crates/example/lib.rs"], runner.digest(source))
                with patch.object(runner.subprocess, "check_output", return_value="changed\n"):
                    self.assertNotEqual(original, runner.rustty_identity())
                source.write_text("changed")
                self.assertNotEqual(original, runner.rustty_identity())
                source.unlink()
                self.assertNotIn("crates/example/lib.rs", runner.rustty_identity()["sources"])
                (root / "Cargo.toml").unlink()
                with self.assertRaisesRegex(ValueError, "checkout missing"):
                    runner.rustty_identity()

    def test_rustty_suite_runs_exact_tests_and_rejects_source_change(self):
        suite = {"id": "external", "kind": "rustty-cargo", "args": ["test"], "platforms": [sys.platform],
                 "assertions": [{"id": "external.exact", "tests": ["module::one"]}]}
        identity = {"root": "/actual/rustty", "sources": {"lib.rs": "before"}}
        result = subprocess.CompletedProcess([], 0, b"test module::one ... ok\n", b"")
        with patch.object(runner, "rustty_identity", return_value=identity), patch.object(runner, "run_suite_process", return_value=result) as run:
            observations, details = runner.run_suite(suite, "/reference")
            self.assertEqual(observations[0]["status"], "pass")
            self.assertEqual(run.call_args.kwargs["cwd"], Path("/actual/rustty"))
            self.assertEqual(details["rustty"], identity)
        with patch.object(runner, "rustty_identity", side_effect=[identity, {**identity, "sources": {"lib.rs": "after"}}]), patch.object(runner, "run_suite_process", return_value=result):
            observations, details = runner.run_suite(suite, "/reference")
            self.assertEqual(observations[0]["status"], "failure")
            self.assertIn("changed during", details["error"])
        with patch.object(runner, "rustty_identity", side_effect=ValueError("checkout missing")), patch.object(runner, "run_suite_process") as run:
            observations, _ = runner.run_suite(suite, "/reference")
            self.assertEqual(observations[0]["status"], "failure")
            run.assert_not_called()

    def proof_config(self):
        config = runner.manifest()
        config["coverage"] = [{"obligation": "scope:sessions-windows-targets:1",
                               "assertions": ["graph.create-split-rename", "graph.split-and-resize-geometry"],
                               "rationale": "Test fixture only: checks proof mechanics, not an actual scope audit."}]
        return config

    def test_reviewed_proofs_require_every_fine_assertion(self):
        config = self.proof_config()
        entries = runner.platform_inventory(runner.inventory(config), runner.suites(), "darwin")
        target = next(entry for entry in entries if entry["id"] == config["coverage"][0]["obligation"])
        self.assertEqual(target["status"], "pending")
        runner.apply_observations(entries, [{"assertion": "graph.create-split-rename", "scenario": "graph", "status": "pass"}])
        self.assertEqual(target["status"], "pending")
        runner.apply_observations(entries, [{"assertion": "graph.split-and-resize-geometry", "scenario": "geometry", "status": "pass"}])
        self.assertEqual(target["status"], "pass")
        runner.apply_observations(entries, [{"assertion": "graph.create-split-rename", "scenario": "graph", "status": "failure"}])
        runner.apply_observations(entries, [{"assertion": "graph.create-split-rename", "scenario": "graph", "status": "pass"}])
        self.assertEqual(target["status"], "failure")
        with self.assertRaises(ValueError):
            runner.apply_observations(entries, [{"assertion": target["id"], "scenario": "graph", "status": "pass"}])

    def test_empty_missing_duplicate_and_derived_proofs_are_rejected(self):
        for proof in [[], ["missing"], ["graph.create-split-rename"] * 2, ["scope:terminal-state:1"]]:
            config = self.proof_config()
            config["coverage"][0]["assertions"] = proof
            with self.subTest(proof=proof), self.assertRaises(ValueError):
                runner.inventory(config)
        config = self.proof_config()
        config["coverage"][0]["rationale"] = ""
        with self.assertRaises(ValueError):
            runner.inventory(config)
        self.assertTrue(all(entry["status"] == "pending" for entry in runner.inventory()
                            if entry["id"].startswith("scope:")))

    def test_every_suite_assigned_to_a_fine_assertion_must_pass(self):
        entries = [{"id": "fine", "requirement": "required", "status": "pending", "suites": ["one", "two"]}]
        runner.apply_observations(entries, [{"assertion": "fine", "scenario": "suite:one", "status": "pass"}])
        self.assertEqual(entries[0]["status"], "pending")
        runner.apply_observations(entries, [{"assertion": "fine", "scenario": "suite:two", "status": "pass"}])
        self.assertEqual(entries[0]["status"], "pass")
        runner.apply_observations(entries, [{"assertion": "fine", "scenario": "suite:one", "status": "failure"},
                                             {"assertion": "fine", "scenario": "suite:two", "status": "pass"}])
        self.assertEqual(entries[0]["status"], "failure")

    def test_platform_counterparts_keep_missing_work_pending_and_all_macos_proofs_required(self):
        available = {name: {"platforms": platforms} for name, platforms in (
            ("unix-one", ["darwin", "linux"]), ("unix-two", ["darwin", "linux"]),
            ("windows", ["win32"]), ("unported", ["darwin", "linux"]))}
        original = [{"id": "exact", "requirement": "required", "status": "pending",
                     "suites": ["unix-one", "unix-two", "windows"]},
                    {"id": "missing", "requirement": "required", "status": "pending", "suites": ["unported"]}]
        windows = runner.platform_inventory(copy.deepcopy(original), available, "win32")
        self.assertEqual(windows[0]["suites"], ["windows"])
        self.assertEqual(windows[1]["suites"], [])
        runner.apply_observations(windows, [{"assertion": "exact", "scenario": "suite:unix-one", "status": "pass"},
                                           {"assertion": "missing", "scenario": "suite:unported", "status": "pass"}])
        self.assertEqual([entry["status"] for entry in windows], ["pending", "pending"])
        runner.apply_observations(windows, [{"assertion": "exact", "scenario": "suite:windows", "status": "pass"}])
        self.assertEqual([entry["status"] for entry in windows], ["pass", "pending"])
        macos = runner.platform_inventory(copy.deepcopy(original), available, "darwin")
        self.assertEqual(macos[0]["suites"], ["unix-one", "unix-two"])
        runner.apply_observations(macos, [{"assertion": "exact", "scenario": "suite:unix-one", "status": "pass"}])
        self.assertEqual(macos[0]["status"], "pending")
        runner.apply_observations(macos, [{"assertion": "exact", "scenario": "suite:unix-two", "status": "pass"}])
        self.assertEqual(macos[0]["status"], "pass")

    def test_remote_counterparts_preserve_exact_named_test_sets(self):
        available = runner.suites()
        pairs = [("windows-remote-c-formats", "config-c-reference"),
                 ("windows-remote-c-unicode", "unicode-cells-c-reference")]
        for windows_name, unix_name in pairs:
            windows, unix = available[windows_name], available[unix_name]
            self.assertEqual(windows["platforms"], ["win32"])
            self.assertEqual(set(unix["platforms"]), {"darwin", "linux"})
            original = {assertion["id"]: assertion["tests"] for assertion in unix["assertions"]}
            for assertion in windows["assertions"]:
                self.assertEqual(assertion["tests"], original[assertion["id"]])
            output = "\n".join("test " + name + " ... ok" for assertion in windows["assertions"] for name in assertion["tests"])
            self.assertTrue(all(value["status"] == "pass" for value in runner.suite_observations(windows, 0, output)[0]))
            self.assertTrue(all(value["status"] == "failure" for value in runner.suite_observations(windows, 0, "running 0 tests\n")[0]))
        self.assertIsNone(runner.suite_reference(available["windows-remote-c-formats"], {}))

    def test_gate_rederives_proofs_and_rejects_stale_reports(self):
        config = self.proof_config()
        entries = runner.apply_observations(runner.inventory(config), [
            {"assertion": "graph.create-split-rename", "scenario": "graph", "status": "pass"}])
        target = config["coverage"][0]["obligation"]
        next(entry for entry in entries if entry["id"] == target)["status"] = "pass"
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "binary"
            executable.write_bytes(b"test executable")
            identity = {"path": str(executable), "sha256": runner.digest(executable)}
            report = {"candidate_kind": "rust", "checkout_revision": runner.source_revision(),
                      "reference_revision": config["reference_revision"], "manifest_sha256": runner.digest(runner.MANIFEST),
                      "implementation_fingerprint": "stale", "platform": "darwin", "reference": identity,
                      "candidate": identity, "entries": entries}
            problems = runner.blockers(report, "command-control", config)
            self.assertIn(f"{target}: pending", problems)
            self.assertIn("implementation changed since evidence was collected", problems)
            report["reference"] = None
            self.assertIn("C/R scenario evidence requires a genuine C reference executable",
                          runner.blockers(report, "command-control", config))

    def test_generated_metadata_drift_blocks_inventory(self):
        for metadata in [runner.generate_command_metadata, runner.generate_option_metadata]:
            with self.subTest(metadata=metadata.__name__):
                with patch.object(metadata, "generate", return_value="changed"):
                    with self.assertRaisesRegex(ValueError, "metadata changed"):
                        runner.inventory()

    def test_reviewed_scope_preserves_real_requirements(self):
        entries = runner.inventory()
        by_id = {entry["id"]: entry for entry in entries}
        self.assertFalse(any(identifier.startswith("review:") for identifier in by_id))
        self.assertIn("wait-for", by_id["scope:hooks-jobs-waits:1"]["surface"]["commands"])
        self.assertIn("respawn-pane", by_id["scope:process-lifecycle:1"]["surface"]["commands"])
        self.assertIn("history-limit", by_id["scope:terminal-state:1"]["surface"]["options"])
        self.assertIn("choose-tree", by_id["scope:native-ui-policy:1"]["surface"]["commands"])
        self.assertIn("regress/targets-panes.sh", by_id["scope:sessions-windows-targets:1"]["sources"])
        self.assertEqual(by_id["outside:tty-scene-presentation:1"]["status"], "outside-profile")
        self.assertFalse(any(entry["status"] == "pass" for entry in entries))

    def test_new_commands_and_changed_sources_reopen_review(self):
        discovered = runner.discovered(runner.manifest())
        discovered["commands"].add("new-upstream-command")
        with patch.object(runner, "discovered", return_value=discovered):
            ids = {entry["id"] for entry in runner.inventory()}
        self.assertIn("review:commands:new-upstream-command", ids)
        source_digest = runner.source_digest
        changed = runner.c_source() / "regress/targets-panes.sh"
        with patch.object(runner, "source_digest", side_effect=lambda path, root, endings: "changed" if path == changed else source_digest(path, root, endings)):
            entries = runner.inventory()
        identifier = "review:source:regress/targets-panes.sh"
        self.assertIn(identifier, {entry["id"] for entry in entries})
        with self.assertRaises(ValueError):
            runner.apply_observations(entries, [{"assertion": identifier, "scenario": "graph", "status": "pass"}])

    def test_smoke_evidence_covers_only_its_exact_assertion(self):
        entries = runner.platform_inventory(runner.inventory(), runner.suites(), "darwin")
        runner.apply_observations(entries, [{"assertion": "graph.create-split-rename", "scenario": "graph", "status": "pass"}])
        by_id = {entry["id"]: entry for entry in entries}
        self.assertEqual(by_id["graph.create-split-rename"]["status"], "pass")
        self.assertEqual(by_id["scope:sessions-windows-targets:1"]["status"], "pending")
        self.assertEqual(by_id["scope:pane-layout:1"]["status"], "pending")
        self.assertTrue(all(entry["status"] == "pending" for entry in entries if entry["id"].startswith("windows.")))
        with self.assertRaises(ValueError):
            runner.apply_observations(entries, [{"assertion": "undeclared", "scenario": "graph", "status": "pass"}])

    def test_pending_and_reference_only_evidence_block_gate(self):
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "binary"
            executable.write_bytes(b"test binary identity")
            identity = {"path": str(executable), "sha256": runner.digest(executable)}
            report = {"candidate_kind": "rust", "checkout_revision": runner.source_revision(), "reference_revision": runner.manifest()["reference_revision"],
                      "manifest_sha256": runner.digest(runner.MANIFEST), "implementation_fingerprint": runner.implementation_fingerprint(),
                      "platform": "darwin", "reference": identity, "candidate": identity, "entries": runner.inventory()}
            self.assertTrue(any("pending" in message for message in runner.blockers(report, "command-control")))
            report["platform"] = "linux"
            self.assertFalse(any("user-deferred" in message for message in runner.blockers(report, "command-control")))
            report["platform"] = "darwin"
            report["candidate_kind"] = "c"
            self.assertTrue(any("C/C" in message for message in runner.blockers(report, "command-control")))
            report["manifest_sha256"] = "stale"
            self.assertTrue(any("manifest changed" in message for message in runner.blockers(report, "command-control")))

    def test_suite_needs_each_exact_test_to_run_and_pass(self):
        suite = {"id": "example", "assertions": [{"id": "exact", "tests": ["module::one", "module::two"]}]}
        for returncode, output in [(0, "running 0 tests\n"), (0, "test module::one ... ok\ntest module::two ... ignored\n"),
                                   (1, "test module::one ... ok\ntest module::two ... ok\n")]:
            observations, _ = runner.suite_observations(suite, returncode, output)
            self.assertEqual(observations[0]["status"], "failure")
        observations, _ = runner.suite_observations(suite, 0, "test module::one ... ok\ntest module::two ... ok\n")
        self.assertEqual(observations[0]["status"], "pass")

    def test_windows_profile_retains_shared_contracts_and_every_windows_obligation(self):
        config = runner.manifest()
        self.assertEqual(config["platform_execution"]["win32"]["status"], "active")
        self.assertEqual(config["profiles"]["windows"]["execution"], "active")
        required = {entry["id"]: entry for entry in runner.inventory() if "windows" in entry["profiles"]}
        for name in ["windows.named-pipe-acl-desktop-ssh", "windows.bridge-no-duplicate-server",
                     "windows.conpty-runtime-packaging", "windows.input-resize-descendant-lifetime",
                     "parser.byte-grammar-and-quoting", "control.every-byte-fragmentation",
                     "terminal.main-alternate-history", "lifetime.persist-ack-gui-restart",
                     "scope:configuration:1", "scope:terminal-state:1"]:
            self.assertEqual(required[name]["requirement"], "required", name)
            self.assertEqual(required[name]["status"], "pending", name)

    def test_native_suites_need_no_c_reference_but_reference_suites_still_do(self):
        self.assertIsNone(runner.suite_reference({"id": "native", "env": {}}, {}))
        for suite in [{"id": "c", "env": {"TMUXX_C_TMUX": "$reference"}},
                      {"id": "c", "reference": "current"},
                      {"id": "c", "kind": "rustty-system-ssh"}]:
            with self.subTest(suite=suite), self.assertRaisesRegex(ValueError, "requires --reference"):
                runner.suite_reference(suite, {})
        with self.assertRaisesRegex(ValueError, "declares no reference"):
            runner.suite_reference({"id": "c", "reference": "none", "env": {"C": "$reference"}}, {})

    def test_native_collection_without_a_fictitious_c_binary(self):
        suite = {"id": "native", "profiles": ["windows"], "args": ["test"], "platforms": [sys.platform],
                 "assertions": [{"id": "native.exact", "tests": ["one"]}]}
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "candidate.exe"
            binary.write_bytes(b"candidate fixture")
            identity = {"path": str(binary), "version": "tmuxx 0.1.0", "sha256": runner.digest(binary)}
            entries = [{"id": "native.exact", "requirement": "required", "status": "pending", "suites": ["native"]}]
            args = SimpleNamespace(reference=None, older_reference=None, candidate=str(binary), candidate_kind="rust",
                                   suite=["native"], no_smoke=True, scenario=None, report=str(Path(directory) / "report.json"))
            result = ([{"assertion": "native.exact", "scenario": "suite:native", "status": "pass"}], {"status": 0})
            with patch.object(runner, "suites", return_value={"native": suite}), patch.object(runner, "inventory", return_value=entries), \
                 patch.object(runner, "implementation_fingerprint", return_value="source"), \
                 patch.object(runner, "executable_identity", return_value=identity) as identify, \
                 patch.object(runner, "run_suite", return_value=result) as execute:
                self.assertEqual(runner.smoke(args), 0)
                identify.assert_called_once_with(str(binary))
                execute.assert_called_once_with(suite, None)
            report = json.loads(Path(args.report).read_text())
            self.assertIsNone(report["reference"])
            self.assertIsNone(report["suites"]["native"]["reference"])
            self.assertEqual(report["host"]["system"], runner.platform.system())
            self.assertEqual(report["host"]["platform"], sys.platform)
            self.assertEqual(report["host"]["python"]["executable"], sys.executable)
            args.no_smoke = False
            with self.assertRaisesRegex(ValueError, "smoke scenarios require --reference"):
                runner.smoke(args)

    def test_exact_custom_conpty_result_rejects_skip_zero_duplicate_and_nonzero_exit(self):
        suite = {"id": "conpty", "harness": "rustty-windows-conpty", "assertions": [
            {"id": "cursor", "tests": ["windows_conpty::synchronized_redraws"]}]}
        manifest = json.loads((runner.ROOT / "tmuxx/windows/conpty.json").read_text())
        hashes = {file["name"]: file["sha256"] for file in manifest["files"]}
        result = {"suite": "windows_conpty", "status": "passed", "completed_redraws": 8,
                  "byte_boundary_cursor_ordering": True, "version": manifest["version"],
                  "package_sha256": manifest["sha256"], "conpty_sha256": hashes["conpty.dll"],
                  "openconsole_sha256": hashes["OpenConsole.exe"]}
        marker = "TMUXX_WINDOWS_CONPTY_RESULT=" + json.dumps(result) + "\n"
        for code, output in [(0, ""), (0, "running 0 tests\n"), (0, "windows_conpty: ignored\n"),
                             (1, marker), (0, marker * 2), (0, "test windows_conpty::synchronized_redraws ... ok\n")]:
            with self.subTest(code=code, output=output):
                self.assertEqual(runner.suite_observations(suite, code, output)[0][0]["status"], "failure")
        self.assertEqual(runner.suite_observations(suite, 0, marker)[0][0]["status"], "pass")
        for key, invalid in [("package_sha256", "wrong"), ("conpty_sha256", "wrong"), ("openconsole_sha256", "wrong"),
                             ("version", "wrong"), ("completed_redraws", 7), ("byte_boundary_cursor_ordering", 1)]:
            changed = "TMUXX_WINDOWS_CONPTY_RESULT=" + json.dumps({**result, key: invalid})
            with self.subTest(key=key):
                self.assertEqual(runner.suite_observations(suite, 0, changed)[0][0]["status"], "failure")
        self.assertEqual(runner.suite_observations(suite, 0, "TMUXX_WINDOWS_CONPTY_RESULT=invalid")[0][0]["status"], "failure")
        suite.pop("harness")
        output = "test windows_conpty::synchronized_redraws ... FAILED\ntest windows_conpty::synchronized_redraws ... ok\n"
        self.assertEqual(runner.suite_observations(suite, 0, output)[0][0]["status"], "failure")

    def test_external_windows_json_requires_exact_names_cleanup_and_one_record(self):
        suite = {"id": "native", "kind": "windows-native-views", "harness": "windows-external-json", "platforms": [sys.platform],
                 "assertions": [{"id": "native", "tests": ["views::one", "views::two"]}]}
        record = {"schema": 1, "kind": "windows-native-views", "passed": True, "tests": ["views::one", "views::two"],
                  "failures": [], "cleanup_remaining": [], "gui_sha256": "a" * 64, "candidate_sha256": "b" * 64}
        self.assertEqual(runner.suite_observations(suite, 0, json.dumps(record))[0][0]["status"], "pass")
        self.assertEqual(runner.suite_observations(suite, 1, json.dumps(record))[0][0]["status"], "failure")
        for change in [{"schema": True}, {"passed": 1}, {"kind": "windows-ssh"}, {"tests": []},
                       {"tests": ["views::one"]}, {"tests": ["views::one", "views::two", "unrequested"]},
                       {"tests": ["views::one", "views::two", "views::two"]}, {"tests": [1, "views::two"]},
                       {"failures": ["failed"]}, {"cleanup_remaining": [123]}, {"cleanup_remaining": None},
                       {"gui_sha256": "wrong"}, {"candidate_sha256": None}]:
            with self.subTest(change=change):
                self.assertEqual(runner.suite_observations(suite, 0, json.dumps(record | change))[0][0]["status"], "failure")
        for output in ["", "test views::one ... ok\ntest views::two ... ok", "{broken}",
                       json.dumps(record) + "\n" + json.dumps(record)]:
            with self.subTest(output=output):
                self.assertEqual(runner.suite_observations(suite, 0, output)[0][0]["status"], "failure")

    def test_external_windows_suite_binds_report_file_and_executable_bytes(self):
        suite = {"id": "native", "kind": "windows-native-views", "harness": "windows-external-json", "platforms": [sys.platform],
                 "assertions": [{"id": "native", "tests": ["views::one"]}]}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            gui, binary = root / "gui.exe", root / "tmuxx.exe"
            gui.write_bytes(b"gui")
            binary.write_bytes(b"candidate")
            identity = {"root": str(root), "sources": {"lib.rs": "source"}}
            for failure in [None, "reported-hash", "changed-binary", "result-file"]:
                binary.write_bytes(b"candidate")
                def execute(command, **kwargs):
                    record = {"schema": 1, "kind": "windows-native-views", "passed": True, "tests": ["views::one"],
                              "failures": [], "cleanup_remaining": [], "gui_sha256": runner.digest(gui),
                              "candidate_sha256": runner.digest(binary)}
                    if failure == "reported-hash":
                        record["candidate_sha256"] = "c" * 64
                    if failure == "changed-binary":
                        binary.write_bytes(b"replacement")
                    report = Path(command[command.index("--report") + 1])
                    report.write_text(json.dumps(record if failure != "result-file" else {}), encoding="utf-8")
                    return subprocess.CompletedProcess(command, 0, json.dumps(record).encode(), b"")
                with self.subTest(failure=failure), patch.dict(runner.os.environ, {"TMUXX_WINDOWS_GUI": str(gui)}), \
                     patch.object(runner, "rustty_identity", return_value=identity), \
                     patch.object(runner, "run_suite_process", side_effect=execute):
                    observations, details = runner.run_suite(suite, None, candidate=str(binary))
                    self.assertEqual(observations[0]["status"], "pass" if failure is None else "failure")
                    if failure is None:
                        self.assertEqual(details["native_binaries"]["candidate"]["sha256"], runner.digest(binary))
                    else:
                        self.assertIn("error", details)

    def test_collection_freezes_candidate_and_revision_before_running_suites(self):
        suite = {"id": "unit", "args": ["test"], "profiles": ["windows"], "platforms": [sys.platform],
                 "assertions": [{"id": "unit.exact", "tests": ["one"]}]}
        for change in (None, "candidate", "missing-candidate", "revision"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                binary = Path(directory) / "tmuxx.exe"
                binary.write_bytes(b"original")
                identity = {"path": str(binary), "sha256": runner.digest(binary), "version": "tmuxx 0.1.0"}
                entries = [{"id": "unit.exact", "requirement": "required", "status": "pending", "suites": ["unit"]}]
                events = []
                def identify(path):
                    events.append("identify")
                    return identity
                def execute(*args, **kwargs):
                    self.assertEqual(events, ["identify"])
                    events.append("execute")
                    if change == "candidate":
                        binary.write_bytes(b"replaced")
                    elif change == "missing-candidate":
                        binary.unlink()
                    observations, _ = runner.suite_observations(suite, 0, "test one ... ok\n")
                    return observations, {"status": 0, "stdout_base64": runner.b64(b"test one ... ok\n")}
                args = SimpleNamespace(reference=None, older_reference=None, candidate=str(binary), candidate_kind="rust",
                    suite=["unit"], no_smoke=True, scenario=None, report=str(Path(directory) / "report.json"))
                with patch.object(runner, "suites", return_value={"unit": suite}), \
                     patch.object(runner, "inventory", return_value=entries), \
                     patch.object(runner, "implementation_fingerprint", return_value="sources"), \
                     patch.object(runner, "source_revision", side_effect=["before", "after" if change == "revision" else "before"]), \
                     patch.object(runner, "executable_identity", side_effect=identify), \
                     patch.object(runner, "run_suite", side_effect=execute):
                    self.assertEqual(runner.smoke(args), int(change is not None))
                report = json.loads(Path(args.report).read_text())
                self.assertEqual(report["candidate"], identity)
                self.assertEqual(report["checkout_revision"], "before")
                self.assertEqual(report["candidate_changed_during_run"], change in ("candidate", "missing-candidate"))
                self.assertEqual(report["checkout_changed_during_run"], change == "revision")

    def test_windows_ssh_requires_and_preserves_the_trusted_command_role_paths(self):
        suite = {"id": "ssh", "kind": "windows-ssh", "harness": "windows-external-json", "platforms": [sys.platform],
                 "assertions": [{"id": "ssh.exact", "tests": ["ssh::exact"]}]}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            gui, candidate = root / "gui.exe", root / "candidate.exe"
            gui.write_bytes(b"gui")
            candidate.write_bytes(b"candidate")
            env = {"TMUXX_WINDOWS_GUI": str(gui), "TMUXX_WINDOWS_SSH_KEY": "desktop-key",
                   "TMUXX_WINDOWS_SSH_KNOWN_HOSTS": "known-hosts", "TMUXX_WINDOWS_SSH_HOST": "same-account@host",
                   "TMUXX_WINDOWS_SSH_VIEWER_KEY": "default-key", "TMUXX_WINDOWS_SSH_VIEWER_BRIDGE": "default-bridge",
                   "TMUXX_WINDOWS_SSH_RUSTTY_VIEWER_KEY": str(root / "key 雪 &^ % !;"),
                   "TMUXX_WINDOWS_SSH_RUSTTY_VIEWER_BRIDGE": str(root / "Rustty 雪 &^ % !;" / "rustty.exe")}
            def execute(command, **kwargs):
                self.assertEqual(command[command.index("--rustty-viewer-key") + 1], env["TMUXX_WINDOWS_SSH_RUSTTY_VIEWER_KEY"])
                self.assertEqual(command[command.index("--rustty-bridge") + 1], env["TMUXX_WINDOWS_SSH_RUSTTY_VIEWER_BRIDGE"])
                record = {"schema": 1, "kind": "windows-ssh", "passed": True, "tests": ["ssh::exact"],
                          "failures": [], "cleanup_remaining": [], "gui_sha256": runner.digest(gui),
                          "candidate_sha256": runner.digest(candidate)}
                Path(command[command.index("--report") + 1]).write_text(json.dumps(record), encoding="utf-8")
                return subprocess.CompletedProcess(command, 0, json.dumps(record).encode(), b"")
            with patch.dict(runner.os.environ, env), patch.object(runner, "rustty_identity", return_value={"root": str(root)}):
                for field in ("TMUXX_WINDOWS_SSH_RUSTTY_VIEWER_KEY", "TMUXX_WINDOWS_SSH_RUSTTY_VIEWER_BRIDGE"):
                    with self.subTest(missing=field), patch.dict(runner.os.environ, {field: ""}), \
                         patch.object(runner, "run_suite_process") as process:
                        observations, details = runner.run_suite(suite, None, candidate=str(candidate))
                        process.assert_not_called()
                        self.assertEqual(observations[0]["status"], "failure")
                        self.assertIn(field, details["error"])
                with patch.object(runner, "run_suite_process", side_effect=execute):
                    observations, _ = runner.run_suite(suite, None, candidate=str(candidate))
                    self.assertEqual(observations[0]["status"], "pass")

    @unittest.skipUnless(sys.platform == "win32", "requires the native Windows SSH fixture parser and identity APIs")
    def test_windows_ssh_generated_command_is_accepted_by_the_actual_fixture_parser(self):
        suite = {"id": "ssh-parser", "kind": "windows-ssh", "harness": "windows-external-json", "platforms": ["win32"],
                 "args": ["--identity"], "assertions": [{"id": "ssh.parser", "tests": ["ssh::parser"]}]}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            gui, candidate = root / "gui.exe", root / "candidate.exe"
            gui.write_bytes(b"not an executable: parser-only check")
            candidate.write_bytes(b"not an executable: parser-only check")
            env = {"TMUXX_WINDOWS_GUI": str(gui), "TMUXX_WINDOWS_SSH_KEY": "desktop-key",
                   "TMUXX_WINDOWS_SSH_KNOWN_HOSTS": "known-hosts", "TMUXX_WINDOWS_SSH_HOST": "same-account@host",
                   "TMUXX_WINDOWS_SSH_VIEWER_KEY": "default-key", "TMUXX_WINDOWS_SSH_VIEWER_BRIDGE": "default-bridge",
                   "TMUXX_WINDOWS_SSH_RUSTTY_VIEWER_KEY": str(root / "key 雪 &^ % !;"),
                   "TMUXX_WINDOWS_SSH_RUSTTY_VIEWER_BRIDGE": str(root / "Rustty 雪 &^ % !;" / "rustty.exe"),
                   "TMUXX_WINDOWS_SSH_EXE": str(root / "not-an-ssh-executable.exe"),
                   "TMUXX_WINDOWS_SSH_DENIAL_DIRECTORY": str(root / "denial 雪 &^ % !;")}
            parsed = []

            def execute(command, **kwargs):
                # The real parser must accept the runner's complete argv before
                # --identity returns. No SSH, GUI or product process can start.
                result = subprocess.run(command, stdin=subprocess.DEVNULL, capture_output=True,
                                        timeout=10, creationflags=subprocess.CREATE_NO_WINDOW)
                parsed.append(result)
                return result

            with patch.dict(runner.os.environ, env), patch.object(runner, "rustty_identity", return_value={"root": str(root)}), \
                 patch.object(runner, "run_suite_process", side_effect=execute):
                observations, _ = runner.run_suite(suite, None, candidate=str(candidate))
            self.assertEqual(len(parsed), 1)
            self.assertEqual(parsed[0].returncode, 0, parsed[0].stderr.decode("utf-8", "replace"))
            self.assertEqual(parsed[0].stderr, b"")
            identity = json.loads(parsed[0].stdout)
            self.assertRegex(identity["account_sid"], r"^S-1-")
            self.assertGreater(identity["pid"], 0)
            # Identity output is deliberately not a conformance success record.
            self.assertEqual(observations[0]["status"], "failure")
            self.assertFalse(Path(parsed[0].args[parsed[0].args.index("--report") + 1]).exists())

    def test_native_suite_rejects_replacement_before_executing_any_fixture(self):
        suite = {"id": "native", "kind": "windows-native-views", "harness": "windows-external-json", "platforms": [sys.platform],
                 "assertions": [{"id": "native", "tests": ["views::one"]}]}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            gui, candidate = root / "gui.exe", root / "candidate.exe"
            gui.write_bytes(b"gui")
            candidate.write_bytes(b"candidate")
            identities = {name: {"path": str(path), "sha256": runner.digest(path)}
                          for name, path in (("gui", gui), ("candidate", candidate))}
            for name in ("candidate", "gui"):
                frozen = copy.deepcopy(identities)
                frozen[name]["sha256"] = "0" * 64
                with self.subTest(name=name), patch.dict(runner.os.environ, {"TMUXX_WINDOWS_GUI": str(gui)}), \
                     patch.object(runner, "rustty_identity", return_value={"root": str(root)}), \
                     patch.object(runner, "run_suite_process") as execute:
                    observations, details = runner.run_suite(suite, None, candidate=str(candidate),
                        candidate_identity=frozen["candidate"], gui_identity=frozen["gui"])
                    self.assertEqual(observations[0]["status"], "failure")
                    self.assertIn(f"native {name} executable changed before", details["error"])
                    execute.assert_not_called()

    def test_native_gate_rechecks_gui_candidate_and_each_exact_result_file(self):
        suites = {name: {"id": name, "kind": "windows-native-views", "harness": "windows-external-json", "platforms": ["win32"],
                        "profiles": ["windows"], "assertions": [{"id": name + ".exact", "tests": ["views::one"]}]}
                  for name in ("first", "second")}
        entries = [{"id": name + ".exact", "profiles": ["windows"], "requirement": "required",
                    "status": "pending", "suites": [name]} for name in suites]
        config = {"profiles": {"windows": {"platforms": ["win32"]}}, "reference_revision": "baseline"}
        changes = [None, "gui-bytes", "gui-missing", "candidate-bytes", "result-file", "result-missing",
                   "recorded-result", "stdout-hash", "other-suite-candidate", "other-suite-gui",
                   "missing-identities", "missing-frozen-gui", "candidate-changed-flag", "gui-changed-flag"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths = {name: root / (name + ".exe") for name in ("gui", "candidate", "other")}
            for change in changes:
                for name, path in paths.items():
                    path.write_bytes(name.encode())
                identities = {name: {"path": str(path), "sha256": runner.digest(path)} for name, path in paths.items()}
                record = {"schema": 1, "kind": "windows-native-views", "passed": True, "tests": ["views::one"],
                          "failures": [], "cleanup_remaining": [], "gui_sha256": identities["gui"]["sha256"],
                          "candidate_sha256": identities["candidate"]["sha256"]}
                details = {}
                for name in suites:
                    result_file = root / (name + ".json")
                    result_file.write_text(json.dumps(record), encoding="utf-8")
                    details[name] = {"platform": "win32", "reference": None, "status": 0, "stdout_base64": runner.b64(json.dumps(record).encode()),
                                     "native_result": copy.deepcopy(record), "native_report": str(result_file),
                                     "native_binaries": {key: copy.deepcopy(identities[key]) for key in ("gui", "candidate")}}
                report = {"candidate_kind": "rust", "checkout_revision": "checkout", "reference_revision": "baseline",
                    "manifest_sha256": runner.digest(runner.MANIFEST), "implementation_fingerprint": "sources", "platform": "win32", "host": {"platform": "win32"},
                    "candidate": identities["candidate"], "native_gui": identities["gui"], "suites": details,
                    "entries": [{**entry, "observation_results": {"suite:" + entry["suites"][0]: "pass"}} for entry in entries]}
                if change == "gui-bytes": paths["gui"].write_bytes(b"replacement")
                elif change == "gui-missing": paths["gui"].unlink()
                elif change == "candidate-bytes": paths["candidate"].write_bytes(b"replacement")
                elif change == "result-file": (root / "second.json").write_text("{}")
                elif change == "result-missing": (root / "second.json").unlink()
                elif change == "recorded-result": details["second"]["native_result"]["candidate_sha256"] = "0" * 64
                elif change == "stdout-hash": details["second"]["stdout_base64"] = runner.b64(json.dumps(record | {"gui_sha256": "0" * 64}).encode())
                elif change in ("other-suite-candidate", "other-suite-gui"):
                    role = change.removeprefix("other-suite-")
                    details["second"]["native_binaries"][role] = identities["other"]
                    altered = record | {role + "_sha256": identities["other"]["sha256"]}
                    details["second"]["native_result"] = altered
                    details["second"]["stdout_base64"] = runner.b64(json.dumps(altered).encode())
                    (root / "second.json").write_text(json.dumps(altered), encoding="utf-8")
                elif change == "missing-identities": details["second"].pop("native_binaries")
                elif change == "missing-frozen-gui": report.pop("native_gui")
                elif change == "candidate-changed-flag": report["candidate_changed_during_run"] = True
                elif change == "gui-changed-flag": report["native_gui_changed_during_run"] = True
                with self.subTest(change=change), patch.object(runner, "suites", return_value=suites), \
                     patch.object(runner, "inventory", side_effect=lambda *_: copy.deepcopy(entries)), \
                     patch.object(runner, "source_revision", return_value="checkout"), \
                     patch.object(runner, "implementation_fingerprint", return_value="sources"), \
                     patch.object(runner.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)):
                    problems = runner.blockers(report, "windows", config)
                    if change is None:
                        self.assertEqual(problems, [])
                    else:
                        self.assertTrue(problems)
                        if not change.endswith("-flag"):
                            self.assertIn("second.exact: failure", problems)

    def test_source_ledger_handles_git_crlf_checkout_without_hiding_edits_or_binary_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.c"
            source.write_bytes(b"one\r\ntwo\r\n")
            expected = hashlib.sha256(b"one\ntwo\n").hexdigest()
            endings = {"source.c": [b"i/lf", b"w/crlf", b"attr/"]}
            self.assertEqual(runner.source_digest(source, root, endings), expected)
            self.assertEqual(source.read_bytes(), b"one\r\ntwo\r\n")
            self.assertNotEqual(runner.source_digest(source, root, {}), expected)
            self.assertNotEqual(runner.source_digest(source, root, {"source.c": [b"i/lf", b"w/crlf", b"attr/-text"]}), expected)
            source.write_bytes(b"one\r\nchanged\r\n")
            self.assertNotEqual(runner.source_digest(source, root, endings), expected)

    def test_failure_cannot_be_overwritten_by_another_passing_observation(self):
        observations = [{"assertion": "graph.create-split-rename", "scenario": "a", "status": "failure"},
                        {"assertion": "graph.create-split-rename", "scenario": "b", "status": "pass"}]
        entries = runner.apply_observations(runner.inventory(), observations)
        self.assertEqual(next(entry for entry in entries if entry["id"] == "graph.create-split-rename")["status"], "failure")

    def test_named_suites_enrich_existing_assertions_without_certifying_scope(self):
        entries = runner.inventory()
        matches = [entry for entry in entries if entry["id"] == "config.prepare-before-execute"]
        self.assertEqual(len(matches), 1)
        self.assertIn("config-c-reference", matches[0]["suites"])
        self.assertTrue(all(entry["status"] == "pending" for entry in entries if entry["id"].startswith("scope:")))


class PlatformEvidenceTests(unittest.TestCase):
    def test_linux_selection_keeps_desktop_obligations_blocked(self):
        runnable, blocked = runner.select_suites(["all"], "linux")
        self.assertIn("cli-linux-owner-watchdog", runnable)
        self.assertIn("unix-ipc-linux-abi", runnable)
        self.assertNotIn("cli-macos-owner-watchdog", runnable)
        self.assertNotIn("cli-macos-owner-watchdog", blocked)
        self.assertIn("rustty-hidden-native-frame", blocked)
        self.assertIn("rustty-desktop-pane-cache", blocked)
        self.assertIn("rustty-sixel-renderer", blocked)
        self.assertIn("rustty-render-viewport-selection", blocked)
        self.assertTrue(all(item["status"] == "blocked" and not item["completed_tests"] for item in blocked.values()))
        entries = {entry["id"]: entry for entry in runner.inventory(host_platform="linux")}
        self.assertEqual(entries["rustty.hidden-native-metal-frame"]["status"], "pending")
        self.assertEqual(entries["lifetime.macos-acknowledged-pty-owner-death"]["status"], "not-applicable")
        self.assertIn("rustty.hidden-native-metal-frame", entries["native.rustty-render-search-adapter"]["proof"])
        with self.assertRaisesRegex(ValueError, "does not apply"):
            runner.select_suites(["cli-macos-owner-watchdog"], "linux")

    def test_platform_watchdogs_preserve_all_common_proof_members(self):
        by_platform = {target: {entry["id"]: entry for entry in runner.inventory(host_platform=target)}
                       for target in ("darwin", "linux", "win32")}
        obligations = ["runtime.child-descendant-cleanup", "lifetime.owner-process-exit", "scope:process-lifecycle:1"]
        for obligation in obligations:
            with self.subTest(obligation=obligation):
                mac = set(by_platform["darwin"][obligation]["proof"])
                linux = set(by_platform["linux"][obligation]["proof"])
                windows = set(by_platform["win32"][obligation]["proof"])
                self.assertEqual(mac - linux, {"lifetime.macos-acknowledged-pty-owner-death"})
                self.assertEqual(linux - mac, {"lifetime.linux-acknowledged-pty-owner-death"})
                self.assertGreater(len(mac & linux), 3)
                self.assertEqual(mac - windows, {"lifetime.macos-acknowledged-pty-owner-death"})
                self.assertEqual(windows - mac, {"lifetime.windows-job-owner-death"})
                self.assertEqual(mac & linux, linux & windows)
        self.assertEqual(by_platform["darwin"]["ipc.linux-ancillary-queues-eof-and-cloexec"]["status"], "not-applicable")

    def test_platform_proof_missing_empty_duplicate_or_wrong_target_fails_review(self):
        config = runner.manifest()
        original = next(item for item in config["coverage"] if item["obligation"] == "runtime.child-descendant-cleanup")
        for replacement in [
            {"darwin": ["lifetime.macos-acknowledged-pty-owner-death"]},
            {"darwin": [], "linux": ["lifetime.linux-acknowledged-pty-owner-death"]},
            {"darwin": ["runtime.graceful-owner-drop"], "linux": ["lifetime.linux-acknowledged-pty-owner-death"]},
            {"darwin": ["lifetime.linux-acknowledged-pty-owner-death"], "linux": ["lifetime.linux-acknowledged-pty-owner-death"]},
            {"darwin": ["missing"], "linux": ["lifetime.linux-acknowledged-pty-owner-death"]},
        ]:
            replacement["win32"] = ["lifetime.windows-job-owner-death"]
            with self.subTest(proof=replacement):
                modified = copy.deepcopy(config)
                next(item for item in modified["coverage"] if item["obligation"] == original["obligation"])["platform_assertions"] = replacement
                with self.assertRaises(ValueError):
                    runner.inventory(modified, "linux")
        for target in ("darwin", "linux", "win32"):
            modified = copy.deepcopy(config)
            mapping = next(item for item in modified["coverage"] if item["obligation"] == original["obligation"])["platform_assertions"]
            del mapping[target]
            with self.subTest(missing=target), self.assertRaises(ValueError):
                runner.inventory(modified, target)

    def test_macos_test_transcript_cannot_certify_a_linux_desktop_requirement(self):
        suite = {"id": "desktop", "platforms": ["darwin"], "profiles": ["native-views"], "args": [],
                 "assertions": [{"id": "desktop.exact", "tests": ["frame"]}]}
        entry = {"id": "desktop.exact", "profiles": ["native-views"], "requirement": "required", "status": "pending", "suites": ["desktop"]}
        config = {"profiles": {"native-views": {"platforms": ["linux", "darwin"]}}, "reference_revision": "baseline"}
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "candidate"
            executable.write_bytes(b"test identity")
            identity = {"path": str(executable), "sha256": runner.digest(executable)}
            report = {"platform": "linux", "candidate_kind": "rust", "checkout_revision": "head", "reference_revision": "baseline",
                      "manifest_sha256": runner.digest(runner.MANIFEST), "implementation_fingerprint": "sources",
                      "reference": identity, "candidate": identity,
                      "entries": [{**entry, "status": "pass", "observation_results": {"suite:desktop": "pass"}}],
                      "suites": {"desktop": {"platform": "darwin", "status": 0, "reference": identity,
                                             "stdout_base64": runner.b64(b"test frame ... ok\n")}}}
            with patch.object(runner, "suites", return_value={"desktop": suite}), \
                 patch.object(runner, "inventory", side_effect=lambda *_: [copy.deepcopy(entry)]), \
                 patch.object(runner, "source_revision", return_value="head"), \
                 patch.object(runner, "implementation_fingerprint", return_value="sources"), \
                 patch.object(runner.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)):
                for forged_platform in ("darwin", "linux"):
                    report["suites"]["desktop"]["platform"] = forged_platform
                    problems = runner.blockers(report, "native-views", config)
                    self.assertIn("desktop.exact: pending", problems)
                    self.assertTrue(any("supported client execution" in item for item in problems))

    def test_unsupported_suite_cannot_run_directly(self):
        suite = {"id": "desktop", "platforms": ["darwin"], "args": [],
                 "assertions": [{"id": "desktop.exact", "tests": ["frame"]}]}
        with patch.object(runner.sys, "platform", "linux"), patch.object(runner, "run_suite_process") as run:
            observations, details = runner.run_suite(suite, "/reference")
        run.assert_not_called()
        self.assertEqual(observations[0]["status"], "failure")
        self.assertIn("requires execution", details["error"])

    def test_linux_gate_requires_specific_gnu_host_identity(self):
        config = {"profiles": {"unix-ipc": {"platforms": ["linux"]}}, "reference_revision": "baseline"}
        host = {"platform": "linux", "architecture": "aarch64", "kernel": "test-kernel",
                "distribution": {"ID": "ubuntu"}, "libc": ["glibc", "2.43"],
                "rust_host_target": "aarch64-unknown-linux-gnu", "cargo_build_target": None}
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "candidate"
            executable.write_bytes(b"test identity")
            identity = {"path": str(executable), "sha256": runner.digest(executable)}
            report = {"platform": "linux", "host": host, "candidate_kind": "rust", "checkout_revision": "head",
                      "reference_revision": "baseline", "manifest_sha256": runner.digest(runner.MANIFEST),
                      "implementation_fingerprint": "sources", "reference": identity, "candidate": identity, "entries": []}
            with patch.object(runner, "suites", return_value={}), patch.object(runner, "inventory", return_value=[]), \
                 patch.object(runner, "source_revision", return_value="head"), \
                 patch.object(runner, "implementation_fingerprint", return_value="sources"), \
                 patch.object(runner.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)):
                self.assertEqual(runner.blockers(report, "unix-ipc", config), [])
                for changes, error in [
                    ({"platform": "darwin"}, "host-platform identity"),
                    ({"architecture": ""}, "distribution, kernel and architecture"),
                    ({"distribution": {}}, "distribution, kernel and architecture"),
                    ({"libc": ["musl", "1.2"]}, "GNU target and glibc"),
                    ({"rust_host_target": None}, "GNU target and glibc"),
                    ({"cargo_build_target": "x86_64-unknown-linux-gnu"}, "cross-compiled"),
                ]:
                    with self.subTest(changes=changes):
                        modified = {**report, "host": {**host, **changes}}
                        self.assertTrue(any(error in problem for problem in runner.blockers(modified, "unix-ipc", config)))


class SuiteCleanupTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "nt", "native Windows Job Object qualification")
    def test_windows_job_cleans_descendants_on_success_failure_timeout_and_interruption(self):
        import ctypes
        from ctypes import wintypes
        api = ctypes.WinDLL("kernel32", use_last_error=True)
        api.OpenProcess.argtypes, api.OpenProcess.restype = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD], wintypes.HANDLE
        api.WaitForSingleObject.argtypes, api.WaitForSingleObject.restype = [wintypes.HANDLE, wintypes.DWORD], wintypes.DWORD
        api.CloseHandle.argtypes = [wintypes.HANDLE]
        unrelated = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"],
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            for mode in ("success", "failure", "timeout", "interrupt", "owner-death", "startup-failure"):
                with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    log = root / "pids"
                    fixture = root / "fixture.py"
                    fixture.write_text("""import os, subprocess, sys, time
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'],
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
with open(os.environ['SUITE_PID_LOG'], 'w') as log:
    print(os.getpid(), child.pid, file=log, flush=True)
os.write(1, b'test owned ... ok\\n\\xffstdout\\n')
os.write(2, b'\\xfestderr\\n')
mode = os.environ['SUITE_MODE']
if mode in ('success', 'failure'):
    sys.exit(0 if mode == 'success' else 3)
time.sleep(60)
""")
                    env = {**os.environ, "SUITE_PID_LOG": str(log), "SUITE_MODE": mode}
                    command = [sys.executable, str(fixture)]
                    def wait_for_fixture():
                        deadline = time.monotonic() + 5
                        while not log.exists() and time.monotonic() < deadline:
                            time.sleep(0.02)
                        self.assertTrue(log.exists(), "owned fixture must start before interruption")
                    if mode == "interrupt":
                        def interrupted(*args, **kwargs):
                            wait_for_fixture()
                            raise KeyboardInterrupt()
                        with patch.object(subprocess.Popen, "communicate", interrupted), self.assertRaises(KeyboardInterrupt):
                            runner.run_suite_process(command, cwd=root, env=env, timeout=3)
                    elif mode == "startup-failure":
                        with patch.object(runner.WindowsSuiteJob, "assign", side_effect=OSError("assignment fixture")), \
                             self.assertRaisesRegex(OSError, "assignment fixture"):
                            runner.run_suite_process(command, cwd=root, env=env, timeout=3)
                        self.assertFalse(log.exists(), "tests cannot start before Job Object assignment")
                    elif mode == "owner-death":
                        script = ("import sys; sys.path.insert(0, sys.argv[1]); import runner; "
                                  "runner.run_suite_process(sys.argv[2:], cwd='.', env=__import__('os').environ.copy(), timeout=60)")
                        owner = subprocess.Popen([sys.executable, "-c", script, str(runner.HERE), *command], cwd=root, env=env,
                                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                        try:
                            wait_for_fixture()
                        finally:
                            owner.kill()
                            owner.wait(timeout=3)
                    elif mode == "timeout":
                        with self.assertRaises(subprocess.TimeoutExpired) as caught:
                            runner.run_suite_process(command, cwd=root, env=env, timeout=2)
                        self.assertIn(b"\xffstdout", caught.exception.output)
                        self.assertIn(b"\xfestderr", caught.exception.stderr)
                        self.assertEqual(caught.exception.cleanup_errors, [])
                    else:
                        result = runner.run_suite_process(command, cwd=root, env=env, timeout=3)
                        self.assertEqual(result.returncode, 0 if mode == "success" else 3)
                        self.assertIn(b"\xffstdout", result.stdout)
                        self.assertIn(b"\xfestderr", result.stderr)
                    self.assertIsNone(unrelated.poll(), "cleanup must leave independent processes alive")
                    if log.exists():
                        for pid in map(int, log.read_text().split()):
                            handle = api.OpenProcess(0x00100000, False, pid)  # SYNCHRONIZE
                            if handle:
                                try:
                                    self.assertEqual(api.WaitForSingleObject(handle, 3000), 0, f"test-owned PID {pid} survived")
                                finally:
                                    api.CloseHandle(handle)
        finally:
            unrelated.kill()
            unrelated.wait(timeout=3)

    @unittest.skipUnless(os.name == "posix", "suite-session cleanup is Unix-only")
    def test_timeout_stops_owned_nested_and_orphan_groups_and_preserves_evidence(self):
        for mode in ("running", "exited"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                log = root / "pids"
                (root / "orphan.py").write_text("""import os, time
if os.fork():
    os._exit(0)
with open(os.environ['SUITE_PID_LOG'], 'a') as log:
    print('orphan', os.getpid(), os.getpgrp(), os.getsid(0), file=log, flush=True)
while True:
    time.sleep(1)
""")
                executable = root / "cargo"
                executable.write_text(f"#!{sys.executable}\n" + """import os, subprocess, sys, time
with open(os.environ['SUITE_PID_LOG'], 'a') as log:
    print('suite', os.getpid(), os.getpgrp(), os.getsid(0), file=log, flush=True)
subprocess.Popen([sys.executable, os.environ['SUITE_ORPHAN']], process_group=0).wait()
while 'orphan' not in open(os.environ['SUITE_PID_LOG']).read():
    time.sleep(0.01)
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
cleared = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'], env={})
private = subprocess.Popen([sys.executable, '-c',
    'import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); time.sleep(60)'], start_new_session=sys.platform == 'linux')
with open(os.environ['SUITE_PID_LOG'], 'a') as log:
    for name, process in [('child', child), ('cleared', cleared), ('private', private)]:
        print(name, process.pid, os.getpgid(process.pid), os.getsid(process.pid), file=log, flush=True)
print('test timeout_case ... ok', flush=True)
os.write(1, bytes([255]) + b'stdout' + bytes([10]))
os.write(2, bytes([254]) + b'stderr' + bytes([10]))
if os.environ['SUITE_MODE'] == 'exited':
    sys.exit(3)
time.sleep(60)
""")
                executable.chmod(0o700)
                unrelated = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"],
                    start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                suite = {"id": "timeout", "args": [], "timeout": 2,
                    "env": {"PATH": str(root) + os.pathsep + os.environ.get("PATH", ""),
                            "SUITE_PID_LOG": str(log), "SUITE_ORPHAN": str(root / "orphan.py"), "SUITE_MODE": mode},
                    "assertions": [{"id": "timeout.exact", "tests": ["timeout_case"]}]}
                try:
                    observations, details = runner.run_suite(suite, "/unused")
                    records = {name: (int(pid), int(group), int(session))
                               for name, pid, group, session in (line.split() for line in log.read_text().splitlines())}
                    self.assertEqual(set(records), {"suite", "orphan", "child", "cleared", "private"})
                    session = records["suite"][0]
                    self.assertTrue(all(record[2] == session for name, record in records.items() if name != "private"))
                    self.assertEqual(records["private"][2], records["private"][0] if sys.platform == "linux" else session)
                    self.assertNotEqual(records["orphan"][1], session)
                    self.assertEqual(observations[0]["status"], "failure")
                    self.assertEqual(details["status"], -1)
                    self.assertEqual(details["completed_tests"], {"timeout_case": "ok"})
                    self.assertIn(b"\xffstdout\n", base64.b64decode(details["stdout_base64"]))
                    self.assertIn(b"\xfestderr\n", base64.b64decode(details["stderr_base64"]))
                    self.assertEqual(details["cleanup_errors"], [])
                    self.assertIsNone(unrelated.poll())
                    with self.assertRaises(ChildProcessError):
                        os.waitpid(session, os.WNOHANG)
                    deadline = time.monotonic() + 3
                    while True:
                        running = []
                        for pid, _, _ in records.values():
                            result = subprocess.run(["ps", "-p", str(pid), "-o", "stat="], capture_output=True, timeout=2)
                            if result.stdout.strip() and not result.stdout.strip().startswith(b"Z"):
                                running.append(pid)
                        if not running or time.monotonic() >= deadline:
                            break
                        time.sleep(0.02)
                    self.assertEqual(running, [])
                finally:
                    unrelated.kill()
                    unrelated.wait(timeout=2)
                    if log.exists():
                        records = [line.split() for line in log.read_text().splitlines()]
                        session = int(records[0][1]) if records else -1
                        for _, raw_pid, raw_group, raw_session in records:
                            pid, group, expected_session = int(raw_pid), int(raw_group), int(raw_session)
                            try:
                                if os.getsid(pid) == expected_session and os.getpgid(pid) == group:
                                    os.killpg(group, signal.SIGKILL)
                            except ProcessLookupError:
                                pass

    def test_timeout_pipe_drain_is_bounded_and_keeps_latest_output(self):
        process = MagicMock()
        process.communicate.side_effect = [
            subprocess.TimeoutExpired(["suite"], 1, output=b"old-out", stderr=b"old-err"),
            subprocess.TimeoutExpired(["suite"], 2, output=b"latest-out", stderr=b"latest-err"),
        ]
        with patch.object(runner.subprocess, "Popen", return_value=process), \
             patch.object(runner, "WindowsSuiteJob", return_value=MagicMock()), \
             patch.object(runner, "stop_suite_session", return_value=[]):
            with self.assertRaises(subprocess.TimeoutExpired) as caught:
                runner.run_suite_process(["suite"], cwd="/unused", env={}, timeout=1)
        self.assertEqual(caught.exception.output, b"latest-out")
        self.assertEqual(caught.exception.stderr, b"latest-err")
        process.stdout.close.assert_called_once()
        process.stderr.close.assert_called_once()
        process.wait.assert_called_once_with(timeout=2)
        self.assertIn("pipes remained open", caught.exception.cleanup_errors[0])


if __name__ == "__main__":
    unittest.main()
