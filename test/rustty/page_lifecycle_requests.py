"""Observe real native page lifetimes across terminal storage operations."""
import struct

import snapshots


def requests(reference):
    observe = {"op": "pages"}

    def write(lines):
        return {"op": "write", "data": (b"x\r\n" * lines).hex()}

    def limits(lines=None, bytes=None):
        return {"op": "grid", "grid": {"action": "limits", "lines": lines, "bytes": bytes}}

    def case(name, cols, rows, operations):
        return ({"id": "pages/" + name, "kind": "input", "cols": cols, "rows": rows,
                 "operations": [observe, *operations]}, ["terminal.pages"])

    # Shrinking without reflow must retire a wide glyph whose tail is cut
    # off, including its style, hyperlink and grapheme resources. Inserting
    # into the shortened row used to crash the native reference.
    for mode, setup in (("primary", b"\x1b[?7l"), ("alternate", b"\x1b[?1049h")):
        for columns in (1, 2, 3, 4):
            for name, glyph in (("wide", "界".encode()),
                                ("grapheme", "界\u0301".encode()),
                                ("linked-style", b"\x1b[1;31m\x1b]8;id=wide;https://example.org\x1b\\"
                                 + "界\u0301".encode() + b"\x1b]8;;\x1b\\\x1b[0m")):
                for inactive in (False, True):
                    data = setup + f"\x1b[1;{columns}H".encode() + glyph
                    if inactive:
                        data += b"\x1b[?1049l" if mode == "alternate" else b"\x1b[?47h"
                    resume = b""
                    if inactive:
                        resume = b"\x1b[?47h" if mode == "alternate" else b"\x1b[?47l"
                    request, covers = case(f"wide-cut/{mode}/{columns}/{name}/{inactive}", 6, 2, [
                        {"op": "write", "data": data.hex()},
                        {"op": "resize", "cols": columns, "rows": 2, "cell_size": [8, 16]},
                        {"op": "observe"}, {"op": "snapshot"}, observe,
                        {"op": "write", "data": (resume + b"\x1b[H\x1b[@X").hex()},
                        {"op": "resize", "cols": 6, "rows": 2, "cell_size": [8, 16]},
                        {"op": "observe"}, {"op": "snapshot"}, observe,
                    ])
                    yield request, covers + ["terminal.resize", "terminal.cells", "terminal.screens"]

    # Growing may reuse a retained allocation or copy into new pages. Copies
    # clear spacer-head markers while keeping the blank cell's attributes;
    # only copied rows lose their previous wrap metadata.
    for mode, setup in (("primary", b""), ("alternate", b"\x1b[?1049h")):
        for initial_columns in (4, 8):
            for head, content in ((False, b"abcdE"), (True, "abc界".encode())):
                for name, style in (("plain", b""), ("colors", b"\x1b[1;31;44m"),
                                    ("link", b"\x1b[1;31m\x1b]8;id=wide;https://example.org\x1b\\")):
                    operations = [{"op": "write", "data": setup.hex()},
                                  {"op": "resize", "cols": 4, "rows": 3, "cell_size": [8, 16]}]
                    data = style + content + (b"\x1b[?7l" if mode == "primary" else b"")
                    operations += [{"op": "write", "data": data.hex()}, {"op": "observe"},
                                   {"op": "resize", "cols": 6, "rows": 3, "cell_size": [8, 16]},
                                   {"op": "observe"}, {"op": "snapshot"}, observe,
                                   {"op": "write", "data": b"\x1b[H\x1b[@\x1b[2;1HZ".hex()},
                                   {"op": "observe"}, {"op": "snapshot"}, observe]
                    request, covers = case(f"widen/{mode}/{initial_columns}/{head}/{name}",
                                           initial_columns, 3, operations)
                    yield request, covers + ["terminal.resize", "terminal.cells", "terminal.styles"]

    for mode, setup in (("primary", b""), ("alternate", b"\x1b[?1049h")):
        data = setup + b"\x1b[1;31;44m\x1b]8;id=wide;https://example.org\x1b\\" + "abc界".encode()
        source = reference.request({"id": "pages/widen/restored-source", "cols": 4, "rows": 3,
                                    "operations": [{"op": "write", "data": data.hex()},
                                                   {"op": "snapshot"}]})
        parts = snapshots.records(bytes.fromhex(source["snapshots"][0]))
        terminal = bytearray(parts[0][1])
        struct.pack_into("<H", terminal, 0, 6)
        parts[0] = (1, terminal)
        restored = snapshots.frame(parts).hex()
        for command in (b"\x1b[H\x1b[X", b"\x1b[H\x1b[@", b"\x1b[H\x1b[P", b"\x1b[1;6HX"):
            request, covers = case(f"widen/restored/{mode}/{command.hex()}", 6, 3, [
                {"op": "restore", "data": restored},
                {"op": "write", "data": command.hex()},
                {"op": "observe"}, {"op": "snapshot"}, observe,
            ])
            yield request, covers + ["terminal.resize", "terminal.cells", "terminal.styles"]

    capacities = {}
    pool_bytes = None
    for cols in (2, 8, 80, 215, 1024):
        response = reference.request({"id": "pages/capacity", "kind": "page_layout",
                                      "page_layout": {"columns": cols}})
        if not response["ok"]:
            raise RuntimeError("reference page capacity query failed")
        capacity = response["page_layout"]["layout"]["capacity"]["rows"]
        capacities[cols] = capacity
        pool_bytes = response["page_layout"]["constants"]["standard_bytes"]
        for rows in sorted({1, 24, min(capacity, 1024), min(capacity + 1, 1024), 1024}):
            yield case(f"initial/{cols}/{rows}", cols, rows, [])
        for rows in sorted({1, 24, min(capacity + 1, 1024)}):
            operations = []
            for count in (max(0, capacity - rows), 1, 1, capacity, capacity):
                operations += [write(count), observe]
            yield case(f"grow/{cols}/{rows}", cols, rows, operations)
        for lines in (0, 1, capacity - 1, capacity, capacity + 1, capacity * 2):
            yield case(f"lines/{cols}/{lines}", cols, 24,
                       [write(capacity * 3), observe, limits(lines=lines), observe,
                        write(capacity), observe])
        for byte_limit in (0, 1, pool_bytes * 2, pool_bytes * 3):
            yield case(f"bytes/{cols}/{byte_limit}", cols, 24,
                       [write(capacity * 3), observe, limits(bytes=byte_limit), observe,
                        write(capacity), observe])
        for sequence in (b"\x1b[3J", b"\x1b[22J", b"\x1bc"):
            yield case(f"clear/{cols}/{sequence.hex()}", cols, 24,
                       [write(capacity + 7), observe, {"op": "write", "data": sequence.hex()},
                        observe, write(capacity), observe])

    for cols, resized in ((8, 4), (8, 12), (80, 40), (80, 120), (215, 80), (1024, 512)):
        yield case(f"reflow/{cols}/{resized}", cols, 24,
                   [write(capacities[cols] + 9), observe,
                    {"op": "resize", "cols": resized, "rows": 24}, observe,
                    write(17), observe, {"op": "resize", "cols": cols, "rows": 12}, observe])
    for cols in (8, 80, 1024):
        operations = [write(capacities[cols] + 7), observe]
        for rows in (1, 24, 1024, 2):
            operations += [{"op": "resize", "cols": cols, "rows": rows}, observe]
        yield case(f"height/{cols}", cols, 24, operations)

    for cols in (8, 80, 215):
        response = reference.request({"id": "pages/snapshot/source", "kind": "input",
                                      "cols": cols, "rows": 24,
                                      "operations": [write(capacities[cols] + 31),
                                                     {"op": "snapshot"}]})
        if not response["ok"] or len(response["snapshots"]) != 1:
            raise RuntimeError("reference page snapshot failed")
        data = response["snapshots"][0]
        yield case(f"snapshot/{cols}", cols, 24,
                   [{"op": "restore", "data": data}, observe, write(1), observe,
                    {"op": "resize", "cols": cols + 1, "rows": 24}, observe])
        for byte_limit in (0, 1, None):
            yield case(f"snapshot-stream/{cols}/{byte_limit}", cols, 24,
                       [{"op": "restore_ready", "data": data}, observe,
                        limits(bytes=byte_limit), {"op": "restore_next"}, observe,
                        {"op": "restore_next"}, observe])
