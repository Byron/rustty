import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import source_paths
import runner
import remote_c_reference


class SourcePathsTests(unittest.TestCase):
    def test_reference_checkout_is_explicit_and_optional_for_local_identity(self):
        with patch.dict(os.environ, {}, clear=True):
            self.assertEqual(runner.rustty_root(), source_paths.ROOT)
            self.assertEqual(remote_c_reference.observer_path("rust/conformance/native_input.py"),
                             source_paths.TMUXX / "conformance/native_input.py")
            with self.assertRaisesRegex(ValueError, "set TMUXX_C_SOURCE"):
                source_paths.c_source()
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {"TMUXX_C_SOURCE": directory}):
            with self.assertRaisesRegex(ValueError, "not a tmux C checkout"):
                source_paths.c_source()
            (Path(directory) / "tmux.h").write_text("fixture")
            self.assertEqual(source_paths.c_source(), Path(directory).resolve())
            self.assertEqual(remote_c_reference.observer_path("regress/input-cursor.sh"),
                             Path(directory).resolve() / "regress/input-cursor.sh")
