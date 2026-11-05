import json
import signal
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest
from unittest.mock import MagicMock, patch

from bench_compare import SAMPLE_SIZE, main, measure, run


class ResultsTest(unittest.TestCase):
    def test_simple_and_parameterized_criterion_directories(self):
        for case in ["print/ascii", "reflow_history_content/ascii-styled/1024"]:
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                output = Path(directory)
                (output / "logs").mkdir()
                saved = output / "criterion" / "before" / f"rustty_{case}" / "forward"
                saved.mkdir(parents=True)
                (saved / "sample.json").write_text(json.dumps({
                    "times": [100] * SAMPLE_SIZE, "iters": [10] * SAMPLE_SIZE,
                }))
                (saved / "estimates.json").write_text(json.dumps({
                    "median": {"point_estimate": 10, "confidence_interval": {}},
                }))
                with patch("bench_compare.run") as execute:
                    result = measure(Path("unused"), None, output, "before", case, "forward")
                self.assertEqual(execute.call_args.kwargs["timeout"], 30)
                self.assertEqual(result["median_ns"], 10)
                self.assertEqual(result["samples_ns"], [10] * SAMPLE_SIZE)
                self.assertEqual(result["data"], str(saved.relative_to(output)))

    def test_timeout_kills_native_process_group(self):
        process = MagicMock(pid=123)
        process.communicate.side_effect = [subprocess.TimeoutExpired("bench", 1), (None, None)]
        with patch("bench_compare.subprocess.Popen") as popen, patch("bench_compare.os.killpg") as kill:
            popen.return_value.__enter__.return_value = process
            with self.assertRaises(subprocess.TimeoutExpired):
                run(["bench"], timeout=1)
        self.assertTrue(popen.call_args.kwargs["start_new_session"])
        kill.assert_called_once_with(123, signal.SIGKILL)
        self.assertEqual(process.communicate.call_count, 2)

    def test_real_timeout_and_exit_status(self):
        with self.assertRaises(subprocess.TimeoutExpired):
            run([sys.executable, "-c", "import time; time.sleep(30)"], timeout=0.1)
        with self.assertRaises(subprocess.CalledProcessError):
            run([sys.executable, "-c", "raise SystemExit(7)"], timeout=2)

    def test_suite_budget_stops_before_launch(self):
        arguments = ["bench_compare.py", "--before", "before", "--simd", "simd",
                     "--output", "unused", "--max-time", "1", "--check"]
        with patch("sys.argv", arguments), patch("bench_compare.time.monotonic", side_effect=[0, 2]):
            with patch("bench_compare.benchmarks") as listing:
                with self.assertRaisesRegex(SystemExit, "reached --max-time"):
                    main()
        listing.assert_not_called()


if __name__ == "__main__":
    unittest.main()
