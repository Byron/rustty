"""Checks that the differential runner cannot hide missing coverage or state."""
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch
import parity


class HarnessTests(unittest.TestCase):
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
        original = {"id": "delivery", "operations": [
            {"op": "write", "data": data.hex()}, {"op": "observe"},
            {"op": "resize", "cols": 5, "rows": 2},
            {"op": "write", "data": "ff"},
        ]}

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
        self.assertGreater(len(variants), 3)
        for variant in variants:
            self.assertEqual(collapse(original["operations"]), collapse(variant["operations"]))

    def test_generated_cases_are_reproducible(self):
        self.assertEqual(list(parity.generated_requests(42, 3)), list(parity.generated_requests(42, 3)))
        self.assertNotEqual(list(parity.generated_requests(41, 3)), list(parity.generated_requests(42, 3)))

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


if __name__ == "__main__":
    unittest.main()
