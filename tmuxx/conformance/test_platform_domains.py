"""Explicit transport domains preserve all shared and macOS obligations."""
import copy
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import runner


class PlatformDomainTests(unittest.TestCase):
    def test_windows_retains_shared_effects_without_importing_local_unix_imsg(self):
        config, suites = runner.manifest(), runner.suites()
        original = runner.inventory(config, "darwin")
        windows = {entry["id"]: entry for entry in runner.platform_inventory(runner.inventory(config, "win32"), suites, "win32")}
        macos = {entry["id"]: entry for entry in runner.platform_inventory(runner.inventory(config, "darwin"), suites, "darwin")}
        domain = set(config["applicability"]["unix-imsg"]["assertions"])
        self.assertEqual(domain, {"ipc.four-command-control-pairings", "ipc.ordinary-tty-rejection-before-mutation",
                                 "ipc.identify-order-version-and-descriptor-rejection", "control.c-3.7c-command-control-pairings"})
        for entry in original:
            identifier = entry["id"]
            if "proof" in entry:
                self.assertEqual(macos[identifier]["proof"], entry["proof"])
                expected = [source for source in entry["proof"] if source not in domain]
                if entry.get("platform_proofs"):
                    expected = [source for source in expected if source not in entry["platform_proofs"]["darwin"]]
                    expected += entry["platform_proofs"]["win32"]
                self.assertEqual(windows[identifier]["proof"], expected)
            if identifier in domain:
                self.assertNotIn("windows", windows[identifier]["profiles"])
                self.assertFalse(windows[identifier]["applicable"])
                self.assertTrue(macos[identifier]["applicable"])
        for identifier in ("control.real-ssh-bootstrap-reconnect-indeterminate-loss", "control.c-3.7c-ssh-discovery-reconnect",
                           "rustty.stock-c-ssh-native-views-and-reconnect", "rustty.stock-c-3.7c-ssh-native-views-and-reconnect"):
            self.assertIn(identifier, windows["control.supported-version-matrix"]["proof"])
        # Missing actual Windows/shared behavior stays pending: selecting a
        # platform is never evidence and cannot certify any broad obligation.
        self.assertEqual(windows["control.supported-version-matrix"]["status"], "pending")
        self.assertIn("windows", windows["cli.byte-preserving-argv"]["profiles"])
        self.assertIn("windows", windows["windows.named-pipe-acl-desktop-ssh"]["profiles"])

    def test_unknown_duplicated_or_unexplained_domain_is_rejected(self):
        for change in ({"assertions": ["nonexistent"]}, {"platforms": []}, {"rationale": ""},
                       {"platforms": ["unreviewed-platform"]}):
            config = runner.manifest()
            config["applicability"]["unix-imsg"].update(change)
            with self.assertRaises(ValueError):
                runner.inventory(config)
        config = runner.manifest()
        config["applicability"]["duplicate"] = config["applicability"]["unix-imsg"].copy()
        with self.assertRaises(ValueError):
            runner.inventory(config)

    def test_counterparts_preserve_exact_all_of_accounting_and_explicit_requests(self):
        unix = {"id": "unix", "platforms": ["darwin", "linux"], "profiles": ["command-control"],
                "assertions": [{"id": "shared", "tests": ["unix_exact"]}]}
        windows = {"id": "windows", "platforms": ["win32"], "profiles": ["windows"],
                   "assertions": [{"id": "shared", "tests": ["windows_exact"]}]}
        available = {"unix": unix, "windows": windows}
        with patch.object(runner, "suites", return_value=available):
            selected, blocked = runner.select_suites(["all"], "win32")
            self.assertEqual(set(selected), {"windows"})
            self.assertEqual(blocked, {})
            selected, blocked = runner.select_suites(["unix"], "win32")
            self.assertEqual(selected, {})
            self.assertEqual(set(blocked), {"unix"})
            unix["assertions"].append({"id": "uncovered", "tests": ["unavailable"]})
            self.assertEqual(set(runner.select_suites(["all"], "win32")[1]), {"unix"})
            unix["assertions"].pop()
        available["second"] = {**windows, "id": "second"}
        entry = {"id": "shared", "requirement": "required", "status": "pending", "suites": list(available)}
        entries = runner.platform_inventory([entry], available, "win32")
        self.assertEqual(entry["declared_suites"], ["unix", "windows", "second"])
        self.assertEqual(entry["suites"], ["windows", "second"])
        runner.apply_observations(entries, [{"assertion": "shared", "scenario": "suite:unix", "status": "failure"},
                                           {"assertion": "shared", "scenario": "suite:windows", "status": "pass"}])
        self.assertEqual(entry["status"], "pending")
        runner.apply_observations(entries, [{"assertion": "shared", "scenario": "suite:second", "status": "pass"}])
        self.assertEqual(entry["status"], "pass")

    def test_counterpart_gate_requires_current_platform_exact_transcript(self):
        suites = {name: {"id": name, "platforms": platforms, "profiles": ["command-control"],
                        "assertions": [{"id": "shared", "tests": [name + "_exact"]}]}
                  for name, platforms in (("unix", ["darwin", "linux"]), ("windows", ["win32"]))}
        entry = {"id": "shared", "profiles": ["windows"], "requirement": "required", "status": "pending", "suites": list(suites)}
        config = {"profiles": {"windows": {"platforms": ["win32"]}}, "reference_revision": "baseline"}
        with tempfile.TemporaryDirectory() as directory:
            candidate = Path(directory) / "candidate"
            candidate.write_bytes(b"exact candidate")
            report = {"platform": "win32", "host": {"platform": "win32"}, "candidate_kind": "rust",
                      "checkout_revision": "head", "reference_revision": "baseline",
                      "manifest_sha256": runner.digest(runner.MANIFEST), "implementation_fingerprint": "sources",
                      "candidate": {"path": str(candidate), "sha256": runner.digest(candidate)},
                      "entries": [{**entry, "observation_results": {"suite:unix": "pass", "suite:windows": "pass"}}],
                      "suites": {"windows": {"platform": "win32", "reference": None, "status": 0,
                                              "stdout_base64": runner.b64(b"test windows_exact ... ok\n")}}}
            with patch.object(runner, "suites", return_value=suites), \
                 patch.object(runner, "inventory", side_effect=lambda *_: [copy.deepcopy(entry)]), \
                 patch.object(runner, "source_revision", return_value="head"), \
                 patch.object(runner, "implementation_fingerprint", return_value="sources"), \
                 patch.object(runner.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)):
                self.assertEqual(runner.blockers(report, "windows", config), [])
                report["suites"]["windows"]["stdout_base64"] = runner.b64(b"running 0 tests\n")
                self.assertIn("shared: failure", runner.blockers(report, "windows", config))
                report["suites"]["windows"]["stdout_base64"] = runner.b64(b"test windows_exact ... ok\n")
                report["suites"]["windows"]["platform"] = "darwin"
                self.assertIn("shared: failure", runner.blockers(report, "windows", config))
                del suites["windows"]
                entry["suites"] = ["unix"]
                self.assertTrue(any("supported client execution" in p for p in runner.blockers(report, "windows", config)))

    def test_scenario_counterpart_preserves_provenance_and_common_proof_members(self):
        suite = {"id": "windows", "platforms": ["win32"],
                 "assertions": [{"id": "scenario", "scenario_counterpart": "original", "tests": ["exact"]}]}
        base = {"id": "scenario", "requirement": "required", "status": "pending", "scenario": "original", "suites": ["windows"]}
        entry = runner.platform_inventory([copy.deepcopy(base)], {"windows": suite}, "win32")[0]
        self.assertNotIn("scenario", entry)
        self.assertEqual(entry["declared_scenario"], "original")
        suite["assertions"][0]["scenario_counterpart"] = "different"
        with self.assertRaisesRegex(ValueError, "exact original scenario"):
            runner.platform_inventory([copy.deepcopy(base)], {"windows": suite}, "win32")
        entries = [{"id": "common", "platforms": ["darwin"], "status": "not-applicable", "requirement": "required"},
                   {"id": "obligation", "proof": ["common"], "status": "pending", "requirement": "required"}]
        runner.platform_inventory(entries, {}, "win32")
        runner.derive_statuses(entries)
        self.assertEqual(entries[1]["proof"], ["common"])
        self.assertEqual(entries[1]["status"], "pending")


if __name__ == "__main__":
    unittest.main()
