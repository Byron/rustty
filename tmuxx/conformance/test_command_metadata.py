import tempfile
import unittest
from pathlib import Path

import generate_command_metadata as metadata


class CommandMetadataTests(unittest.TestCase):
    def test_checked_registry_matches_sources(self):
        self.assertEqual(metadata.OUTPUT.read_text(), metadata.generate())

    def test_unknown_tokens_and_cycles_fail_closed(self):
        self.assertEqual(metadata.strings('A " c"', {"A": '"a" "b"'}), "ab c")
        for expression, macros in [("MISSING", {}), ("A", {"A": "A"})]:
            with self.assertRaises(ValueError):
                metadata.strings(expression, macros)

    def test_selector_roles_come_from_c_descriptors_and_client_flags(self):
        fields = {"source": "{ 's', CMD_FIND_PANE, 0 }",
                  "target": "{ 't', CMD_FIND_WINDOW, CMD_FIND_WINDOW_INDEX }",
                  "flags": "CMD_AFTERHOOK|CMD_CLIENT_CFLAG"}
        self.assertEqual(metadata.selector_flags(fields, "c:s:t:", "example"), "cst")
        self.assertEqual(metadata.selector_flags({"flags": "CMD_CLIENT_TFLAG"}, "t:", "example"), "t")
        for name in metadata.MANUAL_SELECTOR_FLAGS:
            self.assertEqual(metadata.selector_flags({"flags": "0"}, "c:t:", name), "t")
        self.assertEqual(metadata.selector_flags({"flags": "0"}, "c:s:t:", "example"), "")
        with self.assertRaises(ValueError):
            metadata.selector_flags(fields, "s:t:", "example")
        with self.assertRaises(ValueError):
            metadata.selector_flags({"target": "UNKNOWN", "flags": "0"}, "t:", "example")

    def test_source_flag_and_registration_order_drift_changes_output(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "tmux.h").write_text('#define TARGET "[-t target]"\n')
            (root / "cmd.c").write_text('const struct cmd_entry *cmd_table[] = { &second, &first, NULL };')
            entry = '''const struct cmd_entry NAME = {
                .name = "NAME", .args = { "t:", 0, 1, NULL },
                .usage = TARGET, .flags = CMD_STARTSERVER, .exec = example
};'''
            source = entry.replace("NAME", "first") + "\n" + entry.replace("NAME", "second")
            path = root / "cmd-example.c"
            path.write_text(source)
            generated = metadata.generate(root)
            self.assertLess(generated.index('name: "second"'), generated.index('name: "first"'))
            path.write_text(source.replace("CMD_STARTSERVER", "CMD_READONLY"))
            self.assertNotEqual(generated, metadata.generate(root))


if __name__ == "__main__":
    unittest.main()
