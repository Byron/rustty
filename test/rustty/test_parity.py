"""Checks that the differential runner cannot hide missing coverage or state."""
import json
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch
import parity_reference as parity
import osc_requests


class HarnessTests(unittest.TestCase):
    def test_failure_artifacts_can_live_outside_the_checkout(self):
        request = {"id": "regression", "operations": []}
        with TemporaryDirectory() as temporary:
            root = Path(temporary) / "checkout"
            for artifacts in (root / "evidence", Path(temporary) / "external"):
                with patch.object(parity, "ROOT", root), patch.object(parity, "ARTIFACTS", artifacts):
                    reported = parity.save_failure(request, {"ok": True}, None, "oracle exited")
                actual = root / reported
                self.assertEqual(request, json.loads((actual / "request.json").read_text()))
                self.assertEqual("oracle exited\n", (actual / "difference.txt").read_text())
                self.assertEqual(artifacts.parent != root, reported.is_absolute())

    def test_color_and_effect_differences_are_observed(self):
        left = {"cells": [{"text": [65], "color": [1, 2, 3]}], "events": ["bell"]}
        right = {"cells": [{"text": [65], "color": [1, 9, 3]}], "events": ["bell"]}
        self.assertIn("color[1]", parity.difference(left, right))
        right["cells"] = left["cells"]
        right["events"] = []
        self.assertIn("events", parity.difference(left, right))
        self.assertIsNone(parity.difference(left, left))

    def test_partial_or_missing_coverage_cannot_pass(self):
        item = {"id": "terminal.cells", "status": "partial", "remaining": "all controls"}
        manifest = {"requirements": [item]}
        capabilities = [{"terminal.cells"}, {"terminal.cells"}]
        covered = {"terminal.cells"}
        self.assertTrue(parity.coverage_gaps(manifest, capabilities, covered))
        item["status"] = "complete"
        self.assertTrue(parity.coverage_gaps(manifest, capabilities, set()))
        self.assertTrue(parity.coverage_gaps(manifest, [set(), capabilities[1]], covered))
        self.assertEqual([], parity.coverage_gaps(manifest, capabilities, covered))

    def test_delivery_variants_preserve_bytes_and_barriers(self):
        data = "aé界\x1b[31m".encode()
        original = {"id": "delivery", "kind": "snapshot", "operations": [
            {"op": "write", "data": data.hex()}, {"op": "observe"},
            {"op": "resize", "cols": 5, "rows": 2},
            {"op": "write", "data": "ff"},
        ], "after": [{"op": "write", "data": data.hex()}, {"op": "reset"}]}

        def collapse(operations):
            result = []
            pending = b""
            for operation in operations:
                if operation["op"] == "write":
                    pending += bytes.fromhex(operation["data"])
                else:
                    result.extend([pending, operation])
                    pending = b""
            return result + [pending]

        variants = list(parity.variants(original, exhaustive=True))
        self.assertTrue(variants[1]["scalar"])
        self.assertEqual(original["operations"], variants[1]["operations"])
        self.assertEqual(original["after"], variants[1]["after"])
        self.assertGreater(len(variants), 3)
        for variant in variants:
            for field in ("operations", "after"):
                self.assertEqual(collapse(original[field]), collapse(variant[field]))

    def test_generated_cases_are_reproducible(self):
        self.assertEqual(list(parity.generated_requests(42, 3)), list(parity.generated_requests(42, 3)))
        self.assertNotEqual(list(parity.generated_requests(41, 3)), list(parity.generated_requests(42, 3)))

    def test_large_delivery_keeps_bytes_and_bounded_transport_overhead(self):
        data = bytes(range(256)) * 512
        request = {"id": "large", "operations": [{"op": "write", "data": data.hex()}]}
        for variant in parity.variants(request):
            self.assertEqual(data, b"".join(bytes.fromhex(op["data"]) for op in variant["operations"]))
            self.assertLess(len(json.dumps(variant)), len(data) * 2 + 65536)

    def test_parser_corpus_keeps_its_first_byte(self):
        data = b"\x1b[31m"
        with TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("parser-initial", "parser-cmin"):
                corpus = root / "test/fuzz-libghostty/corpus" / name
                corpus.mkdir(parents=True)
                (corpus / "escape").write_bytes(data)
            with patch.object(parity, "ROOT", root):
                requests = [r for r, _ in parity.parser_requests() if "/corpus/" in r["id"]]
        self.assertEqual(2, len(requests))
        for request in requests:
            for variant in parity.variants(request, exhaustive=True):
                self.assertEqual(data, b"".join(bytes.fromhex(op["data"]) for op in variant["operations"]))

    def test_direct_osc_corpus_preserves_selector_payload_and_operation(self):
        records = [b"", b"\x00", b"\x00" + b"52;c;?", b"\x01" + b"7;a\x07b\x9cc",
                   b"\x02" + b"133;A\x1b]2;raw\x18\x1a", b"\xff" + b"52;c;?"]
        with TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("osc-initial", "osc-cmin"):
                corpus = root / "test/fuzz-libghostty/corpus" / name
                corpus.mkdir(parents=True)
                for index, data in enumerate(records):
                    (corpus / str(index)).write_bytes(data)
            requests = list(osc_requests.corpus_requests(root))
        self.assertEqual(2 * len(records), len(requests))
        for request, _ in requests:
            data = records[int(request["id"].rsplit("/", 1)[1])]
            for variant in parity.variants(request, exhaustive=True):
                direct = [op for op in variant["operations"] if op["op"] == "osc"]
                self.assertEqual([{"op": "osc", "data": data.hex()}], direct)

    def test_snapshot_checks_uninterrupted_state_and_decoder_errors(self):
        class Peer:
            def __init__(self, name, loses_state=False, fails=False):
                self.name = name
                self.loses_state = loses_state
                self.fails = fails

            def request(self, request):
                operations = {op["op"] for op in request["operations"]}
                encoding = "snapshot" in operations
                failed = self.fails and not encoding
                state = ["A"] if encoding else ["A", "AB"]
                if self.loses_state and "restore" in operations:
                    state = ["", "B"]
                return {"id": request["id"], "ok": not failed,
                        "err": "InvalidSnapshot" if failed else None,
                        "observations": [] if failed else state,
                        "snapshots": [self.name] if encoding else []}

        request = {"id": "roundtrip", "kind": "snapshot", "operations": [], "after": []}
        # Different wire encodings are acceptable when every restored state agrees.
        self.assertIsNone(parity.compare([Peer("zig"), Peer("rust")], request)[2])
        # Both decoders losing the same state must not hide behind cross-agreement.
        reason = parity.compare([Peer("zig", True), Peer("rust", True)], request)[2]
        self.assertIn("restore-zig", reason)
        self.assertIsNotNone(parity.compare([Peer("zig", fails=True), Peer("rust", fails=True)], request)[2])

    def test_expected_snapshot_rejections_cannot_hide_other_failures(self):
        class Peer:
            def __init__(self, error):
                self.error = error

            def request(self, request):
                self_request = {"id": request["id"], "ok": self.error is None,
                                "err": self.error, "capabilities": []}
                if "expected_error" in request:
                    raise AssertionError("harness-only expectation was sent to native adapter")
                return self_request

        request = {"id": "bad-wire", "expected_error": "InvalidSnapshot", "operations": []}
        self.assertIsNone(parity.compare([Peer("InvalidSnapshot")] * 2, request)[2])
        self.assertIsNotNone(parity.compare([Peer("UnsupportedOperation")] * 2, request)[2])
        self.assertIsNotNone(parity.compare([Peer(None)] * 2, request)[2])
        self.assertEqual([request], list(parity.variants(request, exhaustive=True)))


if __name__ == "__main__":
    unittest.main()
