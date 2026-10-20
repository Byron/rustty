"""File-publication regressions; no GUI or native Windows qualification."""
from pathlib import Path
from types import SimpleNamespace
import tempfile
import threading
import unittest
from unittest.mock import patch

from windows_native_views import wait_json


class NativeViewJsonTests(unittest.TestCase):
    def test_waits_for_partial_json_and_split_utf8_to_become_complete(self):
        with tempfile.TemporaryDirectory(prefix="tmuxx-json-publication-") as temporary:
            path = Path(temporary) / "ready.json"
            partial, split = b'{"text":"', b'{"text":"\xe9\x9b'
            complete = '{"text":"雪","errors":[]}'
            path.write_bytes(partial)
            json_read, utf8_read = threading.Event(), threading.Event()
            failures = []
            original_read = Path.read_text

            def observed_read(file, *args, **kwargs):
                try:
                    text = original_read(file, *args, **kwargs)
                except UnicodeDecodeError:
                    if file == path:
                        utf8_read.set()
                    raise
                if file == path and text == partial.decode():
                    json_read.set()
                return text

            def publish():
                try:
                    if not json_read.wait(2):
                        raise AssertionError("reader never consumed the incomplete JSON")
                    path.write_bytes(split)
                    if not utf8_read.wait(2):
                        raise AssertionError("reader never consumed the split UTF-8 character")
                    path.write_text(complete, encoding="utf-8")
                except BaseException as error:
                    failures.append(error)

            writer = threading.Thread(target=publish)
            writer.start()
            try:
                with patch.object(Path, "read_text", observed_read):
                    result = wait_json(path, SimpleNamespace(poll=lambda: None), seconds=5)
                self.assertEqual(result, {"text": "雪", "errors": []})
            finally:
                writer.join(timeout=3)
                self.assertFalse(writer.is_alive(), "bounded publication writer leaked")
            self.assertEqual(failures, [])
            self.assertTrue(json_read.is_set() and utf8_read.is_set())

    def test_persistent_malformed_json_and_invalid_utf8_fail_at_deadline(self):
        with tempfile.TemporaryDirectory(prefix="tmuxx-json-invalid-") as temporary:
            path = Path(temporary) / "ready.json"
            for data, error in [(b'{"unfinished":', "JSONDecodeError"), (b"\xff", "UnicodeDecodeError")]:
                with self.subTest(error=error):
                    path.write_bytes(data)
                    with self.assertRaisesRegex(TimeoutError, error):
                        wait_json(path, SimpleNamespace(poll=lambda: None), seconds=.02)

    def test_owner_exit_with_incomplete_json_fails_without_waiting_for_deadline(self):
        with tempfile.TemporaryDirectory(prefix="tmuxx-json-exit-") as temporary:
            path = Path(temporary) / "ready.json"
            path.write_bytes(b'{"unfinished":')
            process = SimpleNamespace(returncode=23, poll=lambda: 23)
            with self.assertRaisesRegex(AssertionError, "owner exited 23.*JSONDecodeError"):
                wait_json(path, process, seconds=5)

    def test_complete_but_bad_content_is_returned_for_strict_caller_assertions(self):
        with tempfile.TemporaryDirectory(prefix="tmuxx-json-content-") as temporary:
            path = Path(temporary) / "ready.json"
            path.write_text('{"hidden":true,"errors":["product failure"]}', encoding="utf-8")

            def unexpected_poll():
                self.fail("complete JSON was retried based on its contents")

            self.assertEqual(wait_json(path, SimpleNamespace(poll=unexpected_poll)),
                             {"hidden": True, "errors": ["product failure"]})


if __name__ == "__main__":
    unittest.main()
