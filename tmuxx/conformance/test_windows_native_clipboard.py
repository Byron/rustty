"""Known-value host clipboard guard checks; no system clipboard access."""
import ctypes
import unittest
from unittest.mock import MagicMock
from windows_native_clipboard import KnownClipboard


class HostClipboardGuards(unittest.TestCase):
    def fixture(self):
        value = KnownClipboard.__new__(KnownClipboard)
        value.u, value.k = MagicMock(), MagicMock()
        value.owner, value.sequence, value.restored = 15, 42, False
        value.fixture_bytes = 'known Ω雪\0'.encode('utf-16-le')
        value.open = MagicMock()
        value.u.GetClipboardSequenceNumber.return_value = 42
        value.u.GetClipboardOwner.return_value = 15
        value.u.GetForegroundWindow.return_value = 15
        return value

    def test_preexisting_clipboard_is_never_read_or_replaced(self):
        value = self.fixture()
        value.u.EnumClipboardFormats.return_value = 13
        with self.assertRaisesRegex(RuntimeError, 'prerequisite'):
            value.__enter__()
        value.u.GetClipboardData.assert_not_called()
        value.u.EmptyClipboard.assert_not_called()

    def test_data_appearing_before_emission_is_preserved(self):
        value = self.fixture()
        value.u.EnumClipboardFormats.return_value = 13
        with self.assertRaisesRegex(RuntimeError, 'changed before'):
            value.arm_sequence()
        value.u.GetClipboardData.assert_not_called()
        value.u.EmptyClipboard.assert_not_called()

    def test_any_sequence_change_including_same_hwnd_prevents_read_and_clear(self):
        value = self.fixture()
        value.u.GetClipboardSequenceNumber.return_value = 43
        with self.assertRaisesRegex(RuntimeError, 'changed concurrently'):
            value.__exit__()
        value.u.GetClipboardData.assert_not_called()
        value.u.EmptyClipboard.assert_not_called()

    def test_owner_or_foreground_changes_prevent_read_and_clear(self):
        for method in ('GetClipboardOwner', 'GetForegroundWindow'):
            value = self.fixture()
            getattr(value.u, method).return_value = 99
            with self.assertRaisesRegex(RuntimeError, 'changed concurrently'):
                value.__exit__()
            value.u.GetClipboardData.assert_not_called()
            value.u.EmptyClipboard.assert_not_called()

    def test_cleanup_requires_exact_known_bytes_and_clears_only_once(self):
        for matching in (True, False):
            value = self.fixture()
            data = value.fixture_bytes if matching else b'X' + value.fixture_bytes[1:]
            allocation = ctypes.create_string_buffer(data, len(data))
            value.u.GetClipboardData.return_value = 88
            value.k.GlobalSize.return_value = len(data)
            value.k.GlobalLock.return_value = ctypes.addressof(allocation)
            if matching:
                value.__exit__()
                value.__exit__()
                value.u.EmptyClipboard.assert_called_once()
                self.assertTrue(value.restored)
            else:
                with self.assertRaisesRegex(RuntimeError, 'exact known'):
                    value.__exit__()
                value.u.EmptyClipboard.assert_not_called()
                self.assertFalse(value.restored)

    def test_only_successful_receipt_from_expected_hwnd_is_adopted(self):
        value = self.fixture()
        value.sequence = None
        with self.assertRaises(AssertionError):
            value.adopt({'kind': 'write-complete', 'allowed': True, 'owner': 99, 'sequence': 50})
        self.assertIsNone(value.sequence)
        value.adopt({'kind': 'write-complete', 'allowed': True, 'owner': 15, 'sequence': 50})
        self.assertEqual(value.sequence, 50)


if __name__ == '__main__':
    unittest.main()
