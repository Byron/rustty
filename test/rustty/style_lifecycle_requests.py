"""Observe native STYLE page growth and cursor/cell reference lifetimes."""
import struct

import snapshots
from snapshot_resources import _style_hash, _style_value


def requests(reference):
    observe = {"op": "pages"}

    def write(data):
        return {"op": "write", "data": data.hex()}

    def styles(count, printed=True, start=0):
        return b"".join(f"\x1b[38;2;{value & 255};{value >> 8 & 255};{value >> 16 & 255}m".encode()
                        + (b"X" if printed else b"") for value in range(start, start + count))

    def case(name, columns, rows, operations):
        return ({"id": "pages/styles/" + name, "kind": "input", "cols": columns, "rows": rows,
                 "operations": [observe, *operations]}, ["terminal.pages"])

    for columns, rows in ((8, 2), (80, 24), (215, 2), (1024, 2)):
        yield case(f"cursor-only/{columns}/{rows}", columns, rows,
                   [write(styles(1024, False)), observe])
        operations = []
        previous = 0
        for count in (102, 103, 104, 208, 512, 1024):
            operations += [write(styles(count - previous, start=previous)), observe]
            previous = count
        yield case(f"printed/{columns}/{rows}", columns, rows, operations)
        yield case(f"transient-sgr/{columns}/{rows}", columns, rows,
                   [write(styles(103)), observe,
                    write(b"\x1b[38;2;103;0;0;0m"), observe])
        yield case(f"erase-reuse/{columns}/{rows}", columns, rows,
                   [write(styles(103)), observe, write(b"\x1b[H\x1b[2J"), observe,
                    write(styles(103, start=1000)), observe, write(styles(1, start=2000)), observe])
        yield case(f"overwrite/{columns}/{rows}", columns, rows,
                   [write(styles(103)), observe,
                    write(b"".join(b"\x1b[H" + styles(1, start=value) for value in range(1000, 1300))),
                    observe, write(styles(104, start=2000)), observe])

    for cols in (80, 1024):
        layout = reference.request({"id": "pages/styles/capacity", "kind": "page_layout",
                                    "page_layout": {"columns": cols}})
        capacity = layout["page_layout"]["layout"]["capacity"]["rows"]
        rows = capacity + 1
        yield case(f"cursor-migration/{cols}", cols, rows,
                   [write(styles(103)), observe,
                    write(f"\x1b[{rows};1H".encode()), observe,
                    write(styles(103, start=1000)), observe,
                    write(b"\x1b[1;1H"), observe, write(b"\x1b7"),
                    write(f"\x1b[{rows};1H".encode() + styles(1, start=3000)), observe,
                    write(b"\x1b8"), observe])
        for action in (b"\x1b[3J", b"\x1b[22J", b"\x1b[2S", b"\x1b[2T"):
            yield case(f"row-lifetime/{cols}/{action.hex()}", cols, rows,
                       [write(styles(103)), write(f"\x1b[{rows};1H".encode()), observe,
                        write(action), observe, write(styles(104, start=1000)), observe])

    source = reference.request({"id": "pages/styles/blank-source", "cols": 16, "rows": 4,
                                "operations": [{"op": "snapshot"}]})
    template = snapshots.records(bytes.fromhex(source["snapshots"][0]))
    for capacity in (0, 1, 2, 3, 4, 8, 16, 128):
        parts = []
        for tag, payload in template:
            if tag == 3:
                payload = bytearray(payload)
                struct.pack_into("<H", payload, 8, capacity)
            parts.append((tag, payload))
        yield case(f"restored-capacity/{capacity}", 16, 4,
                   [{"op": "restore", "data": snapshots.frame(parts).hex()}, observe,
                    write(styles(1)), observe, write(styles(1, start=1)), observe,
                    write(styles(1, start=2)), observe, write(styles(100, start=3)), observe])

    for columns in (8, 80, 1024):
        for reflow in (False, True):
            operations = [write(styles(103)), observe]
            if not reflow:
                operations.append(write(b"\x1b[?7l"))
            for width, height in ((16, 4), (3, 6), (min(columns * 2, 1024), 2), (columns, 4)):
                operations += [{"op": "resize", "cols": width, "rows": height}, observe,
                               write(styles(104, start=1000 + width)), observe]
            yield case(f"resize/{columns}/{reflow}", columns, 4, operations)

    def restored(name, columns, rows, capacity, entries, references, after):
        encoded = reference.request({"id": "pages/styles/restore-template", "cols": columns, "rows": rows,
                                     "operations": [{"op": "snapshot"}]})
        parts = snapshots.records(bytes.fromhex(encoded["snapshots"][0]))
        page = bytearray(struct.pack("<HHHHHHII", columns, rows, len(entries), 0, capacity, 0, 0, 0))
        for wire_id, value in entries:
            page.extend(struct.pack("<H", wire_id) + value)
        for row in references:
            page.extend(struct.pack("<BH", 3 << 4, columns))
            for wire_id in row + [0] * (columns - len(row)):
                page.extend(struct.pack("<Q", (ord("A") << 2) | (wire_id << 26)))
        page.extend(struct.pack("<I", 0))
        data = snapshots.frame([(tag, page if tag == 3 else payload) for tag, payload in parts])
        request, covers = case(name, columns, rows,
                               [{"op": "restore", "data": data.hex()}, observe, *after,
                                {"op": "observe"}, {"op": "snapshot"}])
        return request, covers + ["terminal.cells", "terminal.cursor"]

    entries = [(50, _style_value(1)), (4, _style_value(2)), (3, _style_value(3)),
               (7, _style_value(1)), (6, _style_value(4)), (8, _style_value(5))]
    for capacity in (4, 8, 16):
        yield restored(f"decoded-id-history/{capacity}", 16, 2, capacity, entries,
                       [[50, 7, 3], [8, 50]],
                       [write(styles(1, start=1000)), observe, write(b"\x1b[H\x1b[2J"), observe,
                        write(styles(30, start=2000)), observe])

    def native_snapshot(columns, rows, data):
        encoded = reference.request({"id": "pages/styles/mixed-source", "cols": columns, "rows": rows,
                                     "operations": [write(data), {"op": "snapshot"}]})
        return snapshots.records(bytes.fromhex(encoded["snapshots"][0]))

    continuation_requests = []
    for widths in ((8, 4, 8), (4, 8, 4)):
        parts = native_snapshot(8, 6, b"")
        pages = [native_snapshot(width, 2, styles(width * 2, start=index * 256) + b"\x1b[0m")[2]
                 for index, width in enumerate(widths, 1)]
        parts[2:3] = pages
        screen = bytearray(parts[1][1])
        struct.pack_into("<H", screen, 2, len(pages))
        parts[1] = (2, screen)
        data = snapshots.frame(parts).hex()
        for action, sequence in (
            ("observe", b""),
            ("cursor-down", b"\x1b[1;7H\x1b[B"),
            ("cursor-down-across-page", b"\x1b[2;7H\x1b[B"),
            ("cursor-up-across-page", b"\x1b[5;7H\x1b[A"),
            ("cursor-right", b"\x1b[1;1H\x1b[7C"),
            ("erase-chars", b"\x1b[1;1H\x1b[8X"),
            ("erase-line", b"\x1b[1;1H\x1b[2K"),
            ("erase-display", b"\x1b[1;1H\x1b[2J"),
            ("erase-scrollback", b"\x1b[3J"),
            ("insert-lines", b"\x1b[1;1H\x1b[L"),
            ("delete-lines", b"\x1b[1;1H\x1b[M"),
            ("insert-blanks", b"\x1b[1;1H\x1b[@"),
            ("delete-chars", b"\x1b[1;1H\x1b[P"),
            ("alignment", b"\x1b#8"),
            ("scroll-up", b"\x1b[S"),
            ("scroll-down", b"\x1b[T"),
            ("print-wrap-margin", b"\x1b[?69h\x1b[1;3s\x1b[2;1H1234"),
            ("print-wide", b"\x1b[2;1H1234567" + "⚠️A".encode()),
            ("linked-wrap", b"\x1b[2;1H\x1b]8;id=test;https://example.com\x1b\\12345678Z"),
        ):
            request, covers = case(f"mixed-edit/{'-'.join(map(str, widths))}/{action}", 8, 6,
                [{"op": "restore", "data": data}, observe, {"op": "observe"}, {"op": "snapshot"},
                 write(sequence), observe, {"op": "observe"}, {"op": "snapshot"}])
            yield request, covers + ["terminal.cells", "terminal.cursor", "terminal.styles"]

        for no_history in (False, True):
            for top, bottom in ((0, 4), (0, 5), (1, 4), (1, 5)):
                operations = [{"op": "restore", "data": data}]
                if no_history:
                    operations.append({"op": "grid", "grid": {"action": "limits", "bytes": 0}})
                operations += [write(f"\x1b[{top + 1};{bottom + 1}r\x1b[{bottom + 1};1H\x1bD".encode()),
                               observe, {"op": "observe"}, {"op": "snapshot"}]
                request, covers = case(f"mixed-ind/{'-'.join(map(str, widths))}/{no_history}/{top}/{bottom}",
                                       8, 6, operations)
                yield request, covers + ["terminal.cells", "terminal.styles"]
                request, covers = case(f"mixed-ind-resume/{'-'.join(map(str, widths))}/{no_history}/{top}/{bottom}",
                                       8, 6, operations + [write(styles(10, start=2000) + b"\x1bD\x1bD"),
                                       observe, {"op": "observe"}, {"op": "snapshot"}])
                continuation_requests.append((request, covers + ["terminal.cells", "terminal.styles"]))

    # Ordinary wrapped text and wide-character spacer heads must both lose
    # their continuation when ECH, EL, or DCH disconnects the current row.
    for prefix in (b"abcdX", "abc界".encode()):
        data = snapshots.frame(native_snapshot(4, 3, prefix)).hex()
        for command in (b"\x1b[X", b"\x1b[K", b"\x1b[2K", b"\x1b[P"):
            request, covers = case(f"wrap-reset/{prefix.hex()}/{command.hex()}", 4, 3,
                [{"op": "restore", "data": data}, write(b"\x1b[H" + command),
                 {"op": "observe"}, {"op": "snapshot"}])
            yield request, covers + ["terminal.cells"]

    # These RGB values share native bucket zero at the largest STYLE table.
    # Thirty-two entries reach PSL 31; another style must split the page
    # because its requested u16 capacity can no longer grow.
    collision_numbers = [40521, 60313, 137613, 204934, 392876, 566350, 651702, 662230,
                         690678, 752396, 771872, 831708, 836697, 919978, 931563, 1051952,
                         1124614, 1148777, 1218954, 1238356, 1265694, 1332573, 1366442, 1418328,
                         1470358, 1541153, 1608217, 1621308, 1873474, 2032694, 2045509, 2128999]
    collision_values = [_style_value(number) for number in collision_numbers]
    assert all(_style_hash(value) & 65535 == 0 for value in collision_values)
    entries = list(enumerate(collision_values, 1))
    for rows, references in ((1, [list(range(1, 33))]),
                             (2, [list(range(1, 17)), list(range(17, 33))])):
        yield restored(f"max-capacity-split/{rows}", 80, rows, 65535, entries, references,
                       [write(f"\x1b[{rows};1H".encode()), write(styles(1, start=1000)), observe])

    # Continued printing into mixed physical/logical widths can currently
    # abort the native reference. Retain those requests after the matrices
    # above, so a reference abort cannot hide their preceding comparisons.
    yield from continuation_requests
