"""Scenario counterpart and exact native/C evidence guard regressions."""
import base64
import copy
import json
from pathlib import Path
import tempfile
import unittest

import runner
import windows_scenarios


class ScenarioEvidenceTests(unittest.TestCase):
    def test_windows_counterpart_retains_unix_scenario_and_missing_evidence(self):
        suite = runner.suites()["windows-command-scenarios"]
        available = {suite["id"]: suite}
        entries = [{"id": item["id"], "scenario": item["scenario_counterpart"],
                    "suites": [suite["id"]], "status": "pending", "requirement": "required"}
                   for item in suite["assertions"]]
        windows = runner.platform_inventory(copy.deepcopy(entries), available, "win32")
        unix = runner.platform_inventory(copy.deepcopy(entries), available, "darwin")
        self.assertEqual({item["declared_scenario"] for item in windows}, set(runner.SCENARIOS))
        self.assertTrue(all("scenario" not in item for item in windows))
        self.assertEqual([item["scenario"] for item in unix], [item["scenario"] for item in entries])
        runner.derive_statuses(windows)
        self.assertTrue(all(item["status"] == "pending" for item in windows))
        entries[0]["scenario"] = "unreviewed"
        with self.assertRaisesRegex(ValueError, "exact original"):
            runner.platform_inventory(entries, available, "win32")

    def test_native_remote_completion_requires_every_exact_scenario_and_hash(self):
        suite = runner.suites()["windows-command-scenarios"]
        names = [name for item in suite["assertions"] for name in item["tests"]]
        value = {"schema": 1, "kind": "remote-c-scenarios", "passed": True,
                 "candidate_sha256": "a" * 64, "tests": names,
                 "failures": [], "cleanup_remaining": []}
        observations, _ = runner.suite_observations(suite, 0, json.dumps(value))
        self.assertTrue(all(item["status"] == "pass" for item in observations))
        mutations = [lambda v: v["tests"].pop(), lambda v: v["tests"].append(v["tests"][0]),
                     lambda v: v["cleanup_remaining"].append(123),
                     lambda v: v.update(candidate_sha256=""), lambda v: v.update(passed=False)]
        for mutation in mutations:
            invalid = copy.deepcopy(value)
            mutation(invalid)
            observations, _ = runner.suite_observations(suite, 0, json.dumps(invalid))
            self.assertTrue(all(item["status"] == "failure" for item in observations))
        for output in ("", "running 0 tests", json.dumps(value) + "\n" + json.dumps(value)):
            observations, _ = runner.suite_observations(suite, 0, output)
            self.assertTrue(all(item["status"] == "failure" for item in observations))

    def test_saved_scenario_report_and_candidate_replacement_invalidate_gate(self):
        suite = runner.suites()["windows-command-scenarios"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            candidate, report = root / "tmuxx.exe", root / "report.json"
            candidate.write_bytes(b"native candidate")
            identity = {"path": str(candidate), "sha256": runner.digest(candidate)}
            value = {"schema": 1, "kind": "remote-c-scenarios", "passed": True,
                "candidate_sha256": identity["sha256"], "failures": [], "cleanup_remaining": [],
                "tests": [name for item in suite["assertions"] for name in item["tests"]]}
            report.write_text(json.dumps(value), encoding="utf-8")
            details = {"stdout_base64": base64.b64encode(json.dumps(value).encode()).decode(),
                "native_result": value, "native_report": str(report), "native_binaries": {"candidate": identity}}
            runner.validate_native_evidence(suite, details, identity, None)
            candidate.write_bytes(b"replaced")
            with self.assertRaisesRegex(ValueError, "differs"):
                runner.validate_native_evidence(suite, details, identity, None)
            candidate.write_bytes(b"native candidate")
            report.write_text("{}", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "missing or changed"):
                runner.validate_native_evidence(suite, details, identity, None)


if __name__ == "__main__":
    unittest.main()
