import tempfile
import unittest
from pathlib import Path

import generate_option_metadata as metadata


class OptionMetadataTests(unittest.TestCase):
    def test_registry_matches_sources_and_schema(self):
        self.assertEqual(metadata.OUTPUT.read_text(), metadata.generate())
        entries = {item['name']: item for item in metadata.read_entries()}
        self.assertEqual(entries['update-environment']['separator'], ' ,')
        self.assertEqual(entries['command-alias']['separator'], ',')
        self.assertEqual(len(entries['status-format']['defaults']), 3)
        self.assertEqual(entries['image-size-limit']['feature'], 'ENABLE_IMAGES')
        self.assertEqual(entries['utmp']['feature'], 'HAVE_UTEMPTER')
        self.assertTrue(entries['pane-died']['pane'])
        self.assertTrue(entries['pane-died']['hook'])
        self.assertEqual(entries['backspace']['default'], 'C-?')
        self.assertTrue(entries['cursor-colour']['colour'])
        self.assertTrue(entries['dark-theme-black']['colour'])
        self.assertFalse(entries['window-style']['colour'])

    def test_numeric_expressions_are_restricted(self):
        self.assertEqual(metadata.number('(LIMIT << 2) | 1', {'LIMIT': '4 * 2'}), 33)
        for expression, macros in [('MISSING', {}), ('A', {'A': 'A'}), ('f()', {}), ('1 / 2', {})]:
            with self.assertRaises(ValueError):
                metadata.number(expression, macros)

    def test_source_order_defaults_features_and_control_escaping(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'tmux.h').write_text('')
            (root / 'compat.h').write_text('')
            source = '''const struct options_table_entry options_table[] = {
#ifdef ENABLE_IMAGES
\t{ .name = "zeta",
\t  .type = OPTIONS_TABLE_STRING,
\t  .scope = OPTIONS_TABLE_SERVER,
\t  .default_str = "\\001\\b\\033\\\\u001b",
\t},
#endif
\tOPTIONS_TABLE_AFTER_HOOK("attach"),
\t{ .name = "alpha",
\t  .type = OPTIONS_TABLE_NUMBER,
\t  .scope = OPTIONS_TABLE_WINDOW|OPTIONS_TABLE_PANE,
\t  .minimum = 0,
\t  .maximum = INT_MAX,
\t  .default_num = 3,
\t},
};'''
            (root / 'options-table.c').write_text(source)
            entries = metadata.read_entries(root)
            self.assertEqual([item['name'] for item in entries], ['zeta', 'after-attach', 'alpha'])
            self.assertEqual(entries[0]['feature'], 'ENABLE_IMAGES')
            self.assertTrue(entries[2]['pane'])
            output = metadata.generate(root)
            self.assertIn(r'default: "\u{1}\u{8}\u{1b}\\u001b"', output)
            (root / 'options-table.c').write_text(source.replace('.default_num = 3', '.default_num = 4'))
            self.assertNotEqual(output, metadata.generate(root))


if __name__ == '__main__':
    unittest.main()
