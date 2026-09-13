"""Literal search across native pages, disabled scrollback and restored history."""
from pathlib import Path

import snapshots


def requests():
    # Native allocation varies with column count. These dimensions cross page
    # boundaries both within the active area and between active/history pages.
    for cols, rows, count in ((80, 4, 1024), (80, 768, 768),
                              (215, 4, 512), (215, 256, 256),
                              (1024, 4, 256), (1024, 64, 64)):
        patterns = [
            ("hard", b"X\r\n" * (count - 1) + b"X", [b"X", b"X\nX", b"\n"]),
            ("blanks", b"X\r\n" + b"\r\n" * (count - 2) + b"X", [b"X", b"\n", b"\n\n"]),
            ("soft", (b"." * (cols - 1) + b"X") * count, [b"X.", b".X."]),
        ]
        for name, text, needles in patterns:
            for needle in needles:
                request = {
                    "id": f"grid/search/pages/{name}/{cols}/{rows}/{count}/{needle.hex()}",
                    "kind": "input", "cols": cols, "rows": rows,
                    "operations": [{"op": "write", "data": text.hex()}, {"op": "pages"},
                        {"op": "grid", "grid": {"action": "search", "needle": needle.hex()}}],
                }
                yield request, ["terminal.search", "terminal.pages"]

    # Native removes a prefix of forward active results, using the endpoint's
    # strict position after active top-left, before reversing the remaining list.
    for name, cols, rows, text, needles in [
        ("top-left", 8, 4, b"X", [b"X", b"X\n", b"\n"]),
        ("column-one", 8, 4, b" X", [b"X"]),
        ("adjacent", 8, 4, b"XX", [b"X", b"XX"]),
        ("newline-span", 8, 4, b"X\r\nY", [b"X", b"X\n", b"X\nY", b"\n"]),
        ("ed22", 8, 4, b"X\r\nZ\x1b[22J\x1b[HY", [b"X", b"Z", b"Y", b"X\nZ", b"Z\nY", b"\n"]),
        ("ed22-many-pages", 1024, 64, b"X\x1b[64;1HX\x1b[22J\x1b[HX\x1b[64;1HX",
         [b"X", b"X\n", b"\n"]),
    ]:
        for needle in needles:
            yield ({"id": f"grid/search/no-scrollback/{name}/{needle.hex()}",
                    "kind": "input", "cols": cols, "rows": rows,
                    "operations": [{"op": "grid", "grid": {"action": "limits", "bytes": 0}},
                        {"op": "write", "data": text.hex()}, {"op": "pages"},
                        {"op": "grid", "grid": {"action": "search", "needle": needle.hex()}}]},
                   ["terminal.search", "terminal.pages"])

    data = snapshots.fixture(Path(__file__).resolve().parents[2]
                             / "src/terminal/snapshot/testdata/complete-v1.hex").hex()
    for name, restore in [
        ("ready", [{"op": "restore_ready", "data": data}]),
        ("prepended", [{"op": "restore_ready", "data": data}, {"op": "restore_next"}]),
        ("complete", [{"op": "restore", "data": data}]),
    ]:
        for needle in (b"A", b"B", b"C", b"\n"):
            yield ({"id": f"grid/search/restored-pages/{name}/{needle.hex()}", "kind": "input",
                    "operations": [*restore, {"op": "pages"},
                        {"op": "grid", "grid": {"action": "search", "needle": needle.hex()}}]},
                   ["terminal.search", "terminal.pages", "snapshot.streaming"])


if __name__ == "__main__":
    import json
    import sys

    json.dump([{"request": request, "covers": covers} for request, covers in requests()],
              sys.stdout, indent=2)
    sys.stdout.write("\n")
