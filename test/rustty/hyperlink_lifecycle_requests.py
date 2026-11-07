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

    yield from _batched_requests(reference)


def _batched_requests(reference):
    def write(text):
        data = text if isinstance(text, bytes) else text.encode()
        return {"op": "write", "data": data.hex()}

    def start(uri="http://example.test", identifier=None):
        return f"\x1b]8;{'id=' + identifier if identifier is not None else ''};{uri}\x1b\\"

    end = "\x1b]8;;\x1b\\"

    def case(name, cols, rows, operations, checkpoint=True):
        request = {"id": "pages/hyperlinks/batched/" + name, "kind": "snapshot",
                   "cols": cols, "rows": rows, "observe_semantic": True,
                   "operations": [{"op": "pages"}, *operations],
                   "after": [write("\x1b[2b\x1b[6n")]}
        if not checkpoint:
            request["kind"] = "input"
            request["operations"].extend(request.pop("after"))
        return request, ["terminal.pages", "terminal.cells", "terminal.cursor"]

    texts = {"narrow": "hello world!abc", "wide": "中二三四漢字あア",
             "mixed": "中 二12三 四a漢字 "}
    # Explicit identity stays stable across capture and continued printing.
    for repaint in (False, True):
        for name, body in (("narrow", "abcdefghijklmnopqrstuvwxyz0123456789"),
                           ("mixed", "中 a語 b☝🏻" * 3)):
            yield case(f"event-boundary/{repaint}/{name}", 7, 3, [
                write("\x1b[?2027h" + start(identifier="event") + "abcdefg"
                      + ("\r" if repaint else "")),
                write(body), {"op": "pages"}, {"op": "observe"},
            ])
    for identifier in (None, "explicit"):
        for name, body in texts.items():
            yield case(f"odd-edge/{identifier}/{name}", 7, 3, [
                write("\x1b[1;31m\x1b[1\"q\x1b]133;P\x07" + start(identifier=identifier)),
                write(body), {"op": "pages"}, {"op": "observe"},
                write(end + "\x1b[2b"), {"op": "observe"},
            ])
        yield case(f"underfull/{identifier}", 80, 3, [
            write(start(identifier=identifier)), write(texts["mixed"]),
            {"op": "pages"}, {"op": "observe"},
        ])
        yield case(f"same-uri-reopen/{identifier}", 80, 3, [
            write(start(identifier=identifier) + "中 a" + end), {"op": "observe"},
            write(start(identifier=identifier) + "語 b" + end),
            {"op": "pages"}, {"op": "observe"},
        ])
        yield case(f"grapheme-mode/{identifier}", 7, 3, [
            write("\x1b[?2027h" + start(identifier=identifier)), write("中 ☝🏻語ःa"),
            {"op": "pages"}, {"op": "observe"},
        ])
    for name, change in (("same", ""), ("other", start("http://other.test")), ("closed", end)):
        yield case("overwrite/" + name, 15, 3, [
            write(start() + "o" * 12 + "中\x1b[1;3H" + change),
            write("a二b三四 c漢defg"), {"op": "pages"}, {"op": "observe"},
            ])
    yield case("raw-uri", 7, 3, [
        write(b"\x1b]8;id=opaque;http://raw/\xff\x1b\\"),
        write("中 a語 b"), {"op": "pages"}, {"op": "observe"},
    ])
    yield case("margin-wrap-control", 10, 3, [
        write("\x1b[?2027h\x1b[?69h\x1b[3;8s\x1b[1;10H" + start()),
        write("abcd中 a語 b"), {"op": "pages"}, {"op": "observe"},
    ])
    yield case("charset-control", 8, 3, [
        write("\x1b(0" + start(identifier="line")), write("lqkxmj#q"),
        {"op": "pages"}, {"op": "observe"}, write("\x1b(B\x1b[2b"),
        {"op": "observe"},
    ])
    colors = "".join(f"\x1b[38;2;{i};0;0ma" for i in range(110))
    for resource, growth in (("styles", colors), ("graphemes", "a\u0301" * 513)):
        yield case("cursor-link-after-growth/" + resource, 1024, 2, [
            write(start("x" * 2016) + "a"), write(growth), {"op": "pages"},
            {"op": "observe"}, write("中 a語 b" * 4),
            {"op": "pages"}, {"op": "observe"},
        ])

    if reference is None:
        return
    layout = reference.request({"id": "linked-batch/capacity", "kind": "page_layout",
                                "page_layout": {"columns": 200}})
    if not layout["ok"]:
        raise RuntimeError("linked batching native capacity query failed")
    capacity = layout["page_layout"]["layout"]["capacity"]
    map_cells = layout["page_layout"]["layout"]["hyperlink_map_layout"]["capacity"] * 80 // 100
    if map_cells < 7 or map_cells > 200:
        raise RuntimeError("linked batching fixture expects a map fitting one filler row")
    for free in (0, 1, 2, 3, 7):
        for name, body in texts.items():
            yield case(f"map-free/{free}/{name}", 200, 3, [
                write("\x1b[2;1H" + start("filler") + "x" * (map_cells - free)
                      + "\x1b[H" + start()),
                {"op": "pages"}, write(body * 8), {"op": "pages"}, {"op": "observe"},
                    ])
    yield case("one-map-slot-wide-spacer", 200, 3, [
        write("\x1b[2;1H" + start("filler") + "x" * (map_cells - 1)
              + "\x1b[1;200H" + start()),
        {"op": "pages"}, write("中 a"), {"op": "pages"}, {"op": "observe"},
    ])
    # As upstream, older links on the first page ensure re-interned IDs differ.
    setup = "".join(start(uri) + "x" for uri in ("http://a", "http://b", "http://c"))
    setup += "\x1b[3;1H" + "\n" * (capacity["rows"] - 3)
    mixed = "".join(" " if i % 3 == 2 else chr(0x4E00 + i % 512) for i in range(807))
    narrow = "".join(chr(ord("a") + i % 26) for i in range(1007))
    # Packing a snapshot prunes tiny-budget history in both native and Rust
    # decoders. Keep those live checks; the larger budget also checks capture.
    for limit in (10_000, 50_000_000):
        for name, body in (("narrow", narrow), ("mixed", mixed)):
            yield case(f"page-crossing/{limit}/{name}", 200, 3, [
                {"op": "grid", "grid": {"action": "limits", "bytes": limit}},
                write("\x1b[1m" + setup), {"op": "pages"}, write(body),
                {"op": "pages"}, {"op": "observe"},
                write(end + "\x1b[2b"), {"op": "observe"},
            ], checkpoint=limit != 10_000)
