import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from bench_compare import measure


class ResultsTest(unittest.TestCase):
    def test_simple_and_parameterized_criterion_directories(self):
        for case in ["print/ascii", "reflow_history_content/ascii-styled/1024"]:
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                output = Path(directory)
                (output / "logs").mkdir()
                saved = output / "criterion" / "before" / f"rustty_{case}" / "forward"
                saved.mkdir(parents=True)
                (saved / "sample.json").write_text(json.dumps({
                    "times": [100] * 50, "iters": [10] * 50,
                }))
                (saved / "estimates.json").write_text(json.dumps({
                    "median": {"point_estimate": 10, "confidence_interval": {}},
                }))
                with patch("bench_compare.subprocess.run"):
                    result = measure(Path("unused"), None, output, "before", case, "forward")
                self.assertEqual(result["median_ns"], 10)
                self.assertEqual(result["samples_ns"], [10] * 50)
                self.assertEqual(result["data"], str(saved.relative_to(output)))


if __name__ == "__main__":
    unittest.main()
