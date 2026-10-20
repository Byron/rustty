import unittest
from unittest.mock import patch
import generate_format_metadata as metadata

class FormatMetadataTests(unittest.TestCase):
    def test_source_registry_and_order(self):
        self.assertEqual(metadata.generate(), metadata.OUTPUT.read_text())
        source = 'static const struct format_table_entry format_table[] = {\n { "z", FORMAT_TABLE_STRING },\n { "a", FORMAT_TABLE_TIME },\n};'
        with patch('pathlib.Path.read_text', return_value=source):
            with self.assertRaises(ValueError): metadata.generate()
