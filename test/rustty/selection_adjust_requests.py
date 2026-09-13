"""Direct native selection-end adjustments, with unmodified bounds and text."""
import struct

from snapshots import frame, records, write

MOTIONS = ("left", "right", "up", "down", "home", "end", "page_up", "page_down",
           "beginning_of_line", "end_of_line")


def point(x=0, y=0, tag="active"):
    return {"tag": tag, "x": x, "y": y}


def grid(action, **options):
    return {"op": "grid", "grid": {"action": action, **options}}


def adjustments(start, end, rectangle):
    for motion in MOTIONS:
        yield grid("select", start=start, end=end, rectangle=rectangle)
        yield grid("adjust_selection", adjustment=motion)


def case(name, operations, columns=8, rows=4):
    return ({"id": "grid/selection-adjust/" + name, "kind": "input",
             "cols": columns, "rows": rows, "operations": operations},
            ["terminal.selection"])


def requests():
    texts = {
        "empty": b"",
        "sparse": b"a    b\r\n\r\n  c",
        "written-spaces": b"  a  \r\n   \r\n\tz",
        "background": b"a\x1b[44m\x1b[2;1H\x1b[2K\x1b[0m\x1b[4;3Hz",
        "hard-lines": b"first\r\nsecond\r\nthird\r\nlast",
        "soft-wrap": b"abcdefghijklmnopqrstuvwxyz",
        "wide-graphemes": "abc界e\u0301🙂\r\n界  z".encode(),
        "wide-spacer": "abcdefg界Z".encode(),
    }
    bounds = [(point(), point(5)), (point(7, 3), point(5)),
              (point(), point(7, 3)), (point(7, 3), point()),
              (point(3, 1), point(3, 1)), (point(2, 2), point(0, 1))]
    for name, text in texts.items():
        for index, (start, end) in enumerate(bounds):
            for rectangle in (False, True):
                yield case(f"matrix/{name}/{index}/{rectangle}",
                           [write(text), *adjustments(start, end, rectangle)])
        operations = [write(text), grid("select", start=point(3, 1), end=point(3, 1))]
        for motion, count in (("right", 35), ("left", 35), ("down", 6), ("up", 6),
                              ("page_down", 3), ("page_up", 3)):
            operations.extend(grid("adjust_selection", adjustment=motion) for _ in range(count))
        operations += [grid("clear_selection"), grid("adjust_selection", adjustment="right")]
        yield case("sequence/" + name, operations)

    history = b"\r\n".join(f"line {number}".encode() for number in range(12))
    for viewport in (0, -3):
        for tag, end_y in (("active", 2), ("viewport", 1), ("screen", 1), ("history", 2)):
            for rectangle in (False, True):
                yield case(f"history/{viewport}/{tag}/{rectangle}",
                           [write(history), grid("viewport", delta=viewport),
                            *adjustments(point(0, 2), point(6, end_y, tag), rectangle)], rows=3)
    for mode in (b"\x1b[?47h", b"\x1b[?1049h"):
        yield case("alternate/" + mode.hex(),
                   [write(history + mode + b"alt\r\n\r\nend"),
                    *adjustments(point(5, 2), point(4), True)])


def snapshot_requests(reference):
    def snapshot(columns, rows, text):
        response = reference.request({"id": "selection-adjust-source", "cols": columns, "rows": rows,
                                      "operations": [write(text), {"op": "snapshot"}]})
        if not response["ok"] or len(response["snapshots"]) != 1:
            raise RuntimeError("reference could not encode adjustment fixture")
        return records(bytes.fromhex(response["snapshots"][0]))

    for physical, logical in ((4, 8), (8, 4)):
        parts = snapshot(physical, 2, b"ab cd efgh")
        terminal = bytearray(parts[0][1])
        struct.pack_into("<H", terminal, 0, logical)
        struct.pack_into("<H", terminal, 18, logical - 1)
        parts[0] = (1, terminal)
        screen = bytearray(parts[1][1])
        struct.pack_into("<H", screen, 12, 0)
        screen[17] &= ~1
        parts[1] = (2, screen)
        for rectangle in (False, True):
            yield case(f"physical-{physical}-logical-{logical}/{rectangle}",
                       [{"op": "restore", "data": frame(parts).hex()},
                        *adjustments(point(0, 1), point(min(physical, logical) - 1), rectangle)])

    for widths in ((8, 4, 8), (4, 8, 4)):
        # Four active rows make page-down jump across the narrow middle page.
        # Down still traverses that page one row at a time, retaining its clamp.
        parts = snapshot(8, 4, b"")
        pages = [snapshot(width, 2, b"first" if index == 0 else b"last" if index == 2 else b"")[2]
                 for index, width in enumerate(widths)]
        parts[2:3] = pages
        screen = bytearray(parts[1][1])
        struct.pack_into("<H", screen, 2, len(pages))
        struct.pack_into("<Q", screen, 4, 2)
        parts[1] = (2, screen)
        for end_y in range(6):
            for rectangle in (False, True):
                end = point(widths[end_y // 2] - 1, end_y, "screen")
                yield case(f"mixed-{'-'.join(map(str, widths))}/{end_y}/{rectangle}",
                           [{"op": "restore", "data": frame(parts).hex()},
                            grid("viewport", delta=-1),
                            *adjustments(point(0, 5, "screen"), end, rectangle)])
