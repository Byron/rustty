"""Live hyperlink/string allocation, cursor ownership and page movement."""
import struct

import snapshots
from hyperlink_requests import native_hash


def requests(reference):
    pages = {"op": "pages"}

    def write(text):
        return {"op": "write", "data": text.encode().hex()}

    def start(uri="https://example.org", identifier="link"):
        return f"\x1b]8;{'id=' + identifier if identifier is not None else ''};{uri}\x1b\\"

    end = "\x1b]8;;\x1b\\"

    def case(name, cols, rows, operations):
        return ({"id": "pages/hyperlinks/" + name, "kind": "input", "cols": cols,
                 "rows": rows, "operations": [pages, *operations]},
                ["terminal.pages", "terminal.cells"])

    for identifier in (None, "link"):
        yield case(f"map/{identifier}", 1024, 2, [
            write(start(identifier=identifier) + "a" * 102), pages,
            write("b"), pages, write("c" * 400 + end), pages,
            write("\x1b[H\x1b[2J" + start(identifier=identifier) + "d" * 513), pages,
        ])
        for length in (1, 31, 33, 992, 2016, 2048, 2049):
            uri = "x" * length
            yield case(f"strings/{identifier}/{length}", 80, 3, [
                write(start(uri, identifier)), pages, write("a" + end), pages,
                write(start(uri, identifier)), pages, write("b" + end), pages,
                write("\x1b[H\x1b[2J" + start(uri, identifier) + "c" + end), pages,
            ])
        for length in (992, 1984, 2016, 2048):
            yield case(f"map-strings/{identifier}/{length}", 1024, 2, [
                write(start("x" * length, identifier) + "a" * 102), pages,
                write("b"), pages, {"op": "observe"}, write("c" * 300), pages,
                {"op": "snapshot"},
            ])

    links = "".join(start(f"https://example.org/{i}", str(i)) + "a\u0301" + end
                    for i in range(160))
    for command in ("\x1b[20X", "\x1b[20P", "\x1b[20@", "\x1b[2K", "\x1b[2J", "\x1b#8"):
        yield case(f"erase/{command.encode().hex()}", 1024, 2, [
            write("\x1b[31m" + links), pages, write("\x1b[H" + command), pages,
            write("\x1b[2;1H" + links), pages, {"op": "snapshot"},
        ])

    # Distinct links force set growth; dead middle IDs and long strings must
    # survive/reclaim with the native set's insertion order.
    yield case("set-reuse", 1024, 2, [
        write(links), pages, write("\x1b[1;21H\x1b[110X"), pages,
        write("\x1b[2;1H" + links), pages, write("\x1b[H\x1b[2J" + links), pages,
    ])
    many = "".join(start(f"https://example.org/{i}", str(i)) + "a" + end for i in range(600))
    yield case("large-set", 1024, 2, [write(many), pages, {"op": "snapshot"}])

    colliding = []
    index = 0
    while len(colliding) < 40:
        identifier = str(index)
        if native_hash(identifier.encode(), b"uri") & 127 == 0:
            colliding.append(identifier)
        index += 1
    crowded = "".join(start("uri", identifier) + "a" + end for identifier in colliding)
    yield case("collisions", 80, 3, [write(crowded), pages,
        write("\x1b[H\x1b[20X\x1b[2;1H" + crowded), pages, {"op": "snapshot"}])

    # A rebuild may drop a cursor URI which cannot be duplicated alongside
    # the surviving cells. Later printing must use the current cursor state.
    colors = "".join(f"\x1b[38;2;{i};0;0ma" for i in range(110))
    for resource, text in (("styles", colors), ("graphemes", "a\u0301" * 513)):
        yield case(f"other-growth/{resource}", 1024, 2, [
            write(start("x" * 2016, None) + "a"), pages, write(text), pages,
            {"op": "observe"}, write("last"), {"op": "snapshot"},
        ])
    for identifier in (None, "link"):
        yield case(f"cursor-pages/{identifier}", 1024, 48, [
            write(start(identifier=identifier) + "A"), pages,
            write("\x1b[48;1HB"), pages, {"op": "observe"},
            write("\x1b[HC"), pages, {"op": "observe"},
            write("\x1b[48;1H\nD" + end), pages, {"op": "snapshot"},
        ])

    for mode, setup in (("primary", ""), ("alternate", "\x1b[?1049h")):
        for command in ("\x1b[2S", "\x1b[2T", "\x1b[2L", "\x1b[2M",
                        "\x1b[2;4r\x1b[4;1H\n", "\x1b[?69h\x1b[2;79s\x1b[2S"):
            yield case(f"rows/{mode}/{command.encode().hex()}", 80, 4, [
                write(setup + start() + ("a\u0301" * 79 + "\r\n") * 7), pages,
                write("\x1b[H" + command), pages, {"op": "observe"},
                write("\x1b[r\x1b[?69l\x1b[H" + links), pages, {"op": "snapshot"},
            ])

    for reflow in (False, True):
        for identifier in (None, "link"):
            yield case(f"resize/{reflow}/{identifier}", 80, 4, [
                write("\x1b[31m" + links + start(identifier=identifier) + "界\u0301" * 40
                      + ("" if reflow else "\x1b[?7l")), pages,
                {"op": "resize", "cols": 24, "rows": 6}, pages, {"op": "observe"},
                {"op": "resize", "cols": 120, "rows": 3}, pages, {"op": "observe"},
                write("\x1b[H" + "a" * 121 + end), pages, {"op": "snapshot"},
            ])

    source = reference.request({"id": "pages/hyperlinks/source", "cols": 80, "rows": 3,
                                "operations": [{"op": "snapshot"}]})
    template = snapshots.records(bytes.fromhex(source["snapshots"][0]))
    for table, strings in ((0, 0), (48, 1), (144, 32), (192, 32), (192, 2048), (768, 64)):
        parts = []
        for tag, payload in template:
            if tag == 3:
                payload = bytearray(payload)
                struct.pack_into("<H", payload, 10, table)
                struct.pack_into("<I", payload, 16, strings)
            parts.append((tag, payload))
        yield case(f"restored/{table}/{strings}", 80, 3, [
            {"op": "restore", "data": snapshots.frame(parts).hex()}, pages,
            write(start() + "a" * 103 + end), pages,
            write("\x1b[H\x1b[2J" + links), pages, {"op": "snapshot"},
        ])

    for identifier in (None, "link"):
        source = reference.request({"id": "pages/hyperlinks/continued-source", "cols": 80,
                                    "rows": 3, "operations": [
                                        write(start(identifier=identifier) + "a" * 80),
                                        {"op": "snapshot"}]})
        yield case(f"continued/{identifier}", 80, 3, [
            {"op": "restore", "data": source["snapshots"][0]}, pages,
            write("b" * 80 + end), pages, write("\x1b[H\x1b[20X" + links), pages,
            {"op": "snapshot"},
        ])

    def native_snapshot(cols, rows, text):
        source = reference.request({"id": "pages/hyperlinks/mixed-source", "cols": cols,
            "rows": rows, "operations": [write(text), {"op": "snapshot"}]})
        return snapshots.records(bytes.fromhex(source["snapshots"][0]))

    for widths in ((8, 4, 8), (4, 8, 4)):
        for table, strings in ((48, 32), (144, 64), (192, 2048), (768, 64)):
            parts = native_snapshot(8, 6, "")
            copied = []
            for index, width in enumerate(widths):
                text = start(f"uri-{index}", str(index)) + f"\x1b[3{index + 1}m"
                page = bytearray(native_snapshot(width, 2, text + "a\u0301" * (2 * width) + end)[2][1])
                struct.pack_into("<H", page, 10, table)
                struct.pack_into("<I", page, 16, strings)
                copied.append((3, page))
            parts[2:3] = copied
            screen = bytearray(parts[1][1])
            struct.pack_into("<H", screen, 2, 3)
            parts[1] = (2, screen)
            for name, command in (("ind", "\x1b[2;6r\x1b[6;1H\n"),
                                  ("scroll", "\x1b[H\x1b[2S"),
                                  ("scroll-partial", "\x1b[?69h\x1b[2;3s\x1b[2S"),
                                  ("grow-columns", "\x1b[1;8HZ")):
                yield case(f"mixed/{widths}/{table}/{strings}/{name}", 8, 6, [
                    {"op": "restore", "data": snapshots.frame(parts).hex()}, pages,
                    write(start(identifier=None) + command), pages, {"op": "observe"},
                    write("\x1b[r\x1b[?69l" + "a\u0301" * 60 + end), pages, {"op": "snapshot"},
                ])

    for name, text in (("many", many), ("collisions", crowded),
                       ("long-cursor", start("x" * 2016, None) + "a" * 110)):
        after = [write("\x1b[H\x1b[20X" + start() + "a\u0301" * 103 + end), pages,
                 {"op": "resize", "cols": 80, "rows": 6}, pages, {"op": "observe"}]
        yield case("reflow-transient/" + name, 1024, 3, [write(text), *after])
        # PAGE stores populated row counts, not unused grid capacity. Resource
        # charges can legitimately change after capture, so compare them in
        # direct lifecycle fixtures and compare terminal state across capture.
        yield ({"id": "pages/hyperlinks/cross-decode/" + name, "kind": "snapshot", "cols": 1024,
                "rows": 3, "operations": [write(text)],
                "after": [op for op in after if op["op"] != "pages"]},
               ["terminal.pages", "snapshot.cross-decode"])
