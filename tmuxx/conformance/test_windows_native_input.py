"""Clipboard fixture safety checks without touching the system clipboard."""
import unittest
from unittest.mock import MagicMock, call
from windows_native_input import Clipboard


class ClipboardGuards(unittest.TestCase):
    def fixture(self):
        clipboard = Clipboard.__new__(Clipboard)
        clipboard.u = MagicMock()
        clipboard.k = MagicMock()
        clipboard.owner = 15
        clipboard.saved = []
        clipboard.sequence = None
        clipboard.open = MagicMock()
        clipboard.put = MagicMock()
        clipboard.fixture_contents_match = MagicMock(return_value=True)
        return clipboard

    def test_preexisting_formats_are_neither_read_nor_replaced(self):
        clipboard = self.fixture()
        clipboard.u.EnumClipboardFormats.return_value = 13
        with self.assertRaisesRegex(RuntimeError, "prerequisite"):
            clipboard.__enter__()
        clipboard.u.EmptyClipboard.assert_not_called()
        clipboard.u.GetClipboardData.assert_not_called()
        clipboard.u.CloseClipboard.assert_called_once()

    def test_new_user_data_between_entry_and_mutation_is_preserved(self):
        clipboard = self.fixture()
        clipboard.u.EnumClipboardFormats.side_effect = [0, 13]
        clipboard.__enter__()
        with self.assertRaisesRegex(RuntimeError, "changed before"):
            clipboard.text("fixture")
        clipboard.u.EmptyClipboard.assert_not_called()
        clipboard.put.assert_not_called()

    def test_sequence_is_recorded_after_closing_and_same_owner_can_synthesize_formats(self):
        clipboard = self.fixture()
        clipboard.u.EnumClipboardFormats.return_value = 0
        clipboard.u.GetClipboardSequenceNumber.return_value = 42
        clipboard.text("fixture")
        calls = clipboard.u.method_calls
        self.assertLess(calls.index(call.CloseClipboard()), calls.index(call.GetClipboardSequenceNumber()))
        self.assertEqual(clipboard.sequence, 42)
        clipboard.u.reset_mock()
        clipboard.u.GetClipboardSequenceNumber.return_value = 43
        clipboard.u.GetClipboardOwner.return_value = clipboard.owner
        clipboard.__exit__(None, None, None)
        clipboard.u.EmptyClipboard.assert_called_once()

    def test_new_clipboard_owner_is_never_overwritten_during_cleanup(self):
        clipboard = self.fixture()
        clipboard.sequence = 42
        clipboard.u.GetClipboardSequenceNumber.return_value = 43
        clipboard.u.GetClipboardOwner.return_value = 99
        with self.assertRaisesRegex(RuntimeError, "changed concurrently"):
            clipboard.__exit__(None, None, None)
        clipboard.u.EmptyClipboard.assert_not_called()
        clipboard.u.CloseClipboard.assert_called_once()

    def test_same_window_new_user_content_is_never_overwritten(self):
        clipboard = self.fixture()
        clipboard.sequence = 42
        clipboard.u.GetClipboardSequenceNumber.return_value = 43
        clipboard.u.GetClipboardOwner.return_value = clipboard.owner
        clipboard.fixture_contents_match.return_value = False
        with self.assertRaisesRegex(RuntimeError, "changed concurrently"):
            clipboard.__exit__(None, None, None)
        clipboard.u.EmptyClipboard.assert_not_called()

    def test_content_identity_requires_exact_utf16_and_allowed_formats(self):
        import ctypes
        clipboard = self.fixture()
        del clipboard.fixture_contents_match
        clipboard.fixture_bytes = "fixture\0".encode("utf-16-le")
        clipboard.u.GetClipboardData.return_value = 15
        for value, formats, expected in [("fixture", [13, 16, 1, 7, 0], True),
                                          ("changed", [13, 0], False),
                                          ("fixture", [13, 50000, 0], False)]:
            data = (value + "\0").encode("utf-16-le")
            allocation = ctypes.create_string_buffer(data, len(data))
            clipboard.u.EnumClipboardFormats.side_effect = formats
            clipboard.k.GlobalSize.return_value = len(data)
            clipboard.k.GlobalLock.return_value = ctypes.addressof(allocation)
            self.assertEqual(clipboard.fixture_contents_match(), expected)


if __name__ == "__main__":
    unittest.main()
