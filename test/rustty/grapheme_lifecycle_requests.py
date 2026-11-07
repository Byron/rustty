"""Live grapheme allocation, reuse, movement and snapshot continuation."""
import struct

import snapshots


def requests(reference):
    pages = {"op": "pages"}

    def write(text):
        return {"op": "write", "data": text.encode().hex()}

    def cluster(length=1, base="a"):
        return base + "\u0301" * length

    def case(name, cols, rows, operations, **options):
        return ({"id": "pages/graphemes/" + name, "kind": "input", "cols": cols,
                 "rows": rows, "operations": [pages, *operations], **options},
                ["terminal.pages", "terminal.cells"])

    for cols, rows in ((80, 24), (1024, 2), (1024, 46)):
        for length in (1, 4, 5, 16, 64):
            yield case(f"growth/{cols}/{rows}/{length}", cols, rows, [
                write(cluster(length) * 512), pages, write(cluster(length)), pages,
                write("\x1b[H\x1b[2J"), pages, write(cluster(length) * 513), pages,
            ])

    # DCH/ICH move allocations within a row; erasing frees their original runs.
    for command in ("\x1b[100X", "\x1b[100P", "\x1b[100@", "\x1b[K", "\x1b[1K",
                    "\x1b[2K", "\x1b[2J", "\x1b#8"):
        yield case(f"erase/{command.encode().hex()}", 1024, 2, [
            write(cluster() * 512), pages, write("\x1b[1;257H" + command), pages,
            write("\x1b[2;1H" + cluster() * 256), pages,
            write("\x1b[H\x1b[2J" + cluster() * 513), pages,
        ])

    # Alternating holes can have enough free bytes without a two-chunk run.
    fragmented = "".join(f"\x1b[1;{col}Ha" for col in range(1, 513, 2))
    yield case("fragmentation", 1024, 2, [
        write(cluster(4) * 512), pages, write(fragmented), pages,
        write("\x1b[1;3H\u0301"), pages, write(cluster(5) * 256), pages,
    ])

    for mode, setup in (("primary", ""), ("alternate", "\x1b[?1049h")):
        for command in ("\x1b[2S", "\x1b[2T", "\x1b[2L", "\x1b[2M",
                        "\x1b[4;1H\n", "\x1b[2;4r\x1b[4;1H\n",
                        "\x1b[?69h\x1b[2;79s\x1b[2S"):
            yield case(f"rows/{mode}/{command.encode().hex()}", 80, 4, [
                write(setup + (cluster(5) * 79 + "\r\n") * 7), pages,
                write("\x1b[H" + command), pages, {"op": "observe"},
                write("\x1b[r\x1b[?69l\x1b[H" + cluster(5) * 241), pages,
            ])

    for reflow in (False, True):
        for length in (1, 5, 64):
            yield case(f"resize/{reflow}/{length}", 80, 4, [
                write("\x1b[31m" + cluster(length) * 201 + ("" if reflow else "\x1b[?7l")),
                pages, {"op": "resize", "cols": 24, "rows": 6}, pages,
                {"op": "observe"}, {"op": "snapshot"},
                {"op": "resize", "cols": 120, "rows": 3}, pages,
                write("\x1b[H" + cluster(length) * 121), pages,
                {"op": "observe"}, {"op": "snapshot"},
            ])

    # A page rebuild for one resource also copies the other resources in row
    # order. Keep a cursor style alive through grapheme growth and vice versa.
    colors = "".join(f"\x1b[38;2;{i};0;0m{cluster(5)}" for i in range(110))
    yield case("style-growth", 1024, 2, [
        write(cluster(5) * 200), pages, write(colors), pages,
        write(cluster(5) * 201), pages, {"op": "observe"}, {"op": "snapshot"},
    ])

    encoded = reference.request({"id": "pages/graphemes/template", "cols": 80, "rows": 3,
                                 "operations": [{"op": "snapshot"}]})
    template = snapshots.records(bytes.fromhex(encoded["snapshots"][0]))
    for capacity in (0, 1, 16, 17, 48, 512, 1024, 8192):
        parts = []
        for tag, payload in template:
            if tag == 3:
                payload = bytearray(payload)
                struct.pack_into("<I", payload, 12, capacity)
            parts.append((tag, payload))
        for length in (1, 5, 64):
            yield case(f"restored/{capacity}/{length}", 80, 3, [
                {"op": "restore", "data": snapshots.frame(parts).hex()}, pages,
                write(cluster(length) * 70), pages, write("\x1b[H\x1b[20X"), pages,
                write("\x1b[2;1H" + cluster(length) * 70), pages,
                {"op": "observe"}, {"op": "snapshot"},
            ])

    # Snapshot decode reserves complete suffixes, retaining the resulting
    # allocation history for the very next live write.
    for length in (1, 4, 5, 64):
        source = reference.request({"id": "pages/graphemes/continued-source", "cols": 80,
                                    "rows": 3, "operations": [
                                        write(cluster(length) * 70), {"op": "snapshot"}]})
        yield case(f"continued/{length}", 80, 3, [
            {"op": "restore", "data": source["snapshots"][0]}, pages,
            write("\x1b[2;1H" + cluster(length) * 80), pages,
            write("\x1b[1;2H\u0301"), pages, {"op": "snapshot"},
        ])

    # Existing suffixes transfer without allocating when widening wraps within
    # the same page. Across pages the native terminal appends each suffix anew.
    for cols, rows, line in ((3, 3, 1), (1024, 48, 46), (3, 1, 1)):
        for mode, setup in (("primary", ""), ("alternate", "\x1b[?1049h")):
            yield case(f"wrap/{cols}/{rows}/{line}/{mode}", cols, rows, [
                write(setup + f"\x1b[?2027h\x1b[{line};{cols}H☺\u200d"), pages,
                write("❤"), pages, {"op": "observe"}, {"op": "snapshot"},
                write("\x1b[H\x1b[2J" + cluster() * 513), pages,
            ])

    def native_snapshot(cols, rows, text):
        source = reference.request({"id": "pages/graphemes/mixed-source", "cols": cols,
                                    "rows": rows, "operations": [write(text), {"op": "snapshot"}]})
        return snapshots.records(bytes.fromhex(source["snapshots"][0]))

    for widths in ((8, 4, 8), (4, 8, 4)):
        for length in (1, 5):
            for capacity in (1, 16, 17, 64, 128):
                parts = native_snapshot(8, 6, "")
                copied = []
                for index, width in enumerate(widths):
                    page = bytearray(native_snapshot(width, 2,
                        f"\x1b[3{index + 1}m" + cluster(length) * (width * 2) + "\x1b[0m")[2][1])
                    struct.pack_into("<I", page, 12, capacity << index)
                    copied.append((3, page))
                parts[2:3] = copied
                screen = bytearray(parts[1][1])
                struct.pack_into("<H", screen, 2, len(copied))
                parts[1] = (2, screen)
                restored = snapshots.frame(parts).hex()
                edits = [(f"ind/{no_history}/{bottom}", [
                    *([{"op": "grid", "grid": {"action": "limits", "bytes": 0}}] if no_history else []),
                    write(f"\x1b[1;{bottom}r\x1b[{bottom};1H\x1bD"),
                ]) for no_history in (False, True) for bottom in (4, 6)]
                edits += [("scroll-up", [write("\x1b[2S")]),
                          ("scroll-down", [write("\x1b[2T")]),
                          ("scroll-partial", [write("\x1b[?69h\x1b[2;7s\x1b[2S")]),
                          ("resize-narrow", [{"op": "resize", "cols": 3, "rows": 6}]),
                          ("resize-wide", [{"op": "resize", "cols": 12, "rows": 6}])]
                for name, operations in edits:
                    yield case(f"mixed/{widths}/{length}/{capacity}/{name}", 8, 6, [
                        {"op": "restore", "data": restored}, pages, *operations,
                        pages, {"op": "observe"}, {"op": "snapshot"},
                        write("\x1b[?69l\x1b[r\x1b[H" + cluster(5) * 17), pages,
                        {"op": "observe"}, {"op": "snapshot"},
                    ])

    # Mixed-width rows exercise joins, overwrite barriers and page-local pen IDs.
    simple = [
        ("wide-space-edge", 4, "", "中 中"),
        ("one-column", 1, "", "中 a語"),
        ("two-columns", 2, "", "中 a語"),
        ("three-columns", 3, "", "中 a語"),
        ("pending-narrow-wide", 4, "", "abcd中 二"),
        ("pending-wide-narrow", 4, "", "中二a三"),
        ("skin-tone", 5, "x", "a☝🏻b"),
        ("spacing-mark", 5, "x", " 中ःb"),
        ("prepend", 5, "x", "a\u0600中b"),
        ("regional-pair", 5, "x", "a🇦🇧b"),
        ("join-inside-wide", 12, "", "中二👍🏻三"),
        ("join-inside-narrow", 12, "", "aабःb"),
        ("placeholder", 9, "", "中 a\U0010eeee\u0301語 b"),
        ("latin1-control", 9, "", "中é\u00adÿ語\u0085a"),
        ("latin1-grapheme", 9, "", "中 ©\u200d🙂a"),
        ("overwrite-barriers", 9,
         "\x1b[31m界a\u0301\x1b]8;id=old;old\x1b\\x\x1b]8;;\x1b\\"
         "\x1b[0m界Q\x1b[H\x1b[1;44m\x1b[1\"q\x1b]133;P\x07",
         "中 a語 b界 c"),
    ]
    for length in (3, 4, 5, 31, 32, 33):
        simple.append((f"homogeneous-return/{length}", 120, "", "中 " + "語" * length + " a界"))
    for length in (255, 256, 257):
        simple.append((f"decode-boundary/{length}", 73, "", ("中 a" * 86)[:length]))
    for graphemes in (False, True):
        mode = f"\x1b[?2027{'h' if graphemes else 'l'}"
        for name, cols, setup, body in simple:
            yield case(f"mixed-batch/{name}/{int(graphemes)}", cols, 5, [
                write(mode + setup), write(body), {"op": "observe"}, {"op": "pages"},
                write("\x1b[2b"), {"op": "observe"},
            ], observe_semantic=True, kind="snapshot", after=[write("\x1b[2b\x1b[6n")])

    # Exactly the upstream stale-style shape: prior page has three unrelated
    # styles, the pen is bold, and the printable slice itself triggers a wrap.
    layout = reference.request({"id": "mixed-batch/capacity", "kind": "page_layout",
                                "page_layout": {"columns": 200}})
    if not layout["ok"]:
        raise RuntimeError("mixed batching native capacity query failed")
    rows = layout["page_layout"]["layout"]["capacity"]["rows"]
    if rows < 3:
        raise RuntimeError("mixed batching source page is smaller than its active rows")
    styles = "\x1b[3my\x1b[0m\x1b[2my\x1b[0m\x1b[4my\x1b[0m\x1b[1m"
    body = "".join(" " if i % 3 == 2 else chr(0x4E00 + i % 512) for i in range(807))
    for limit in (10_000, 50_000_000):
        for graphemes in (False, True):
            mode = f"\x1b[?2027{'h' if graphemes else 'l'}"
            yield case(f"mixed-batch/style-page-crossing/{limit}/{int(graphemes)}", 200, 3, [
                {"op": "grid", "grid": {"action": "limits", "bytes": limit}},
                write(mode + styles + "\x1b[3;1H" + "\n" * (rows - 3)),
                {"op": "pages"}, write(body), {"op": "pages"}, {"op": "observe"},
                write("\x1b[2b"), {"op": "observe"},
            ], observe_semantic=True, kind="snapshot", after=[write("\x1b[2b\x1b[6n")])
