"""Native terminal selection export: plain/VT/HTML, bytes and page boundaries."""
import itertools
import selection_adjust_requests
from snapshots import write


def grid(action, **options):
    if action.startswith("format_"):
        options.setdefault("format_map", True)
    return {"op": "grid", "grid": {"action": action, **options}}


def formats():
    for emit, unwrap, trim in itertools.product(("plain", "vt", "html"), (False, True), (False, True)):
        yield grid("format_selection", format={"emit": emit, "unwrap": unwrap, "trim": trim})


def requests():
    texts = {
        "empty": b"",
        "spaces": b"   a   \r\n \r\n\tb\r\n",
        "wrap": b"abcdefghijklmnopqrstuvwxyz0123456789",
        "wide": "abcdefg界🙂e\u0301\r\n界<>é".encode(),
        "background": b"\x1b[44m\x1b[2J\x1b[2;3HZ\x1b[3;1H\x1b[0mA",
        "flags": b"\x1b[1;2;3;5;7;8;9;53;4mA \x1b[0mB\r\nC",
        "underlines": b"".join(f"\x1b[4:{v}m{v}".encode() for v in range(6)),
        "colors": b"\x1b[38;2;1;2;3;48;5;250;58;2;4;5;6mA\x1b[0mB",
        "palette": b"\x1b]4;1;#aabbcc\x07\x1b[31mRed",
        "links": b'\x1b]8;id=a;https://x/"<&\xff\x07ab\x1b]8;;\x07 c',
        "link-styles": b"\x1b]8;id=a;uri\x07\x1b[31mA\x1b[32mB\r\nC\x1b]8;id=b;uri\x07D",
        "link-ids": b"\x1b]8;;uri\x07A\x1b]8;;uri\x07B\x1b]8;id=x;uri\x07C\x1b]8;id=x;uri\x07D",
        "protected": b'\x1b[1"q\x1b[1;44mX\r\n\x1b[2K\x1b[2;3HY',
        "semantic": b"\x1b]133;A\x07$ \x1b]133;B\x07hi\x1b]133;C\x07\r\noutput",
    }
    for name, data in texts.items():
        for index, ((x, y), (end_x, end_y)) in enumerate((((0, 0), (7, 3)), ((7, 3), (2, 0)), ((3, 0), (7, 0)))):
            for rectangle in (False, True):
                yield ({"id": f"grid/selection-format/{name}/{index}/{rectangle}", "cols": 8, "rows": 4,
                        "operations": [write(data), grid("select", start={"x": x, "y": y}, end={"x": end_x, "y": end_y}, rectangle=rectangle), *formats()]}, ["terminal.selection"])
    for alternate in (False, True):
        prefix = b"\x1b[?1049h" if alternate else b""
        yield ({"id": f"grid/selection-format/pages/{alternate}", "cols": 1024, "rows": 3,
                "operations": [write(prefix + b"\x1b[31m" + b"word   \r\n" * 30), grid("select", start={"tag": "screen", "x": 2, "y": 0}, end={"tag": "active", "x": 6, "y": 2}), *formats()]}, ["terminal.selection"])
    yield ({"id": "grid/selection-format/no-selection", "cols": 4, "rows": 2, "operations": [*formats()]}, ["terminal.selection"])

    for name, data in (
        ("wrapped", b"\x1b[31m" + b"a" * 1023 + b" " * (1024 * 30) + b"\x1b[32mz"),
        ("wide-spacers", ("a" * 1023 + "界").encode() * 50),
        ("blank-pages", b"first\r\n" + b"\r\n" * 80 + b"last"),
    ):
        for rectangle in (False, True):
            yield ({"id": f"grid/selection-format/page-boundaries/{name}/{rectangle}",
                    "kind": "input", "cols": 1024, "rows": 3,
                    "operations": [write(data), grid("select", start={"tag": "screen", "x": 0, "y": 0},
                                   end={"x": 1023, "y": 2}, rectangle=rectangle), *formats()]},
                   ["terminal.selection"])


def snapshot_requests(reference):
    # Reuse the native snapshots and mixed physical-width bounds already used
    # by adjustment checks; export each original selection without adjusting it.
    for request, covers in selection_adjust_requests.snapshot_requests(reference):
        setup = [op for op in request["operations"]
                 if op["op"] != "grid" or op["grid"]["action"] == "viewport"]
        select = next(op for op in request["operations"]
                      if op["op"] == "grid" and op["grid"]["action"] == "select")
        yield ({**request, "id": request["id"].replace("selection-adjust", "selection-format/restored"),
                "operations": [*setup, select, *formats()]}, covers)
