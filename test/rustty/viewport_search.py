"""Persistent viewport search, dirty feeds and native whole-page matches."""
from pathlib import Path

import snapshots


def grid(action, **options):
    return {"op": "grid", "grid": {"action": action, **options}}


def needle(value):
    return grid("search_needle", needle=value.hex())


def case(name, operations, cols=8, rows=4):
    return ({"id": "grid/search/viewport/" + name, "kind": "input",
             "cols": cols, "rows": rows,
             "operations": [op if isinstance(op, dict)
                            else {"op": "write", "data": op.hex()}
                            for op in operations]}, ["terminal.search"])


def requests():
    read = grid("search_viewport")
    feed = grid("search_feed")
    clean = grid("search_feed", active_dirty=False)
    yield case("idle", [read, feed, read, needle(b""), read,
                       needle(b"A"), read, clean, read, needle(b""), read])
    yield case("lifecycle", [b"cat CAT cat\r\ndog", needle(b"cat"), read,
                            feed, read, read, needle(b"CAT"), read,
                            b"\x1b[Hdog", read, needle(b"CaT"), read,
                            clean, read, feed, read, needle(b"dog"), read,
                            clean, read, needle(b""), read, feed, read])

    texts = [b"ABBA AB", b"A  B\r\n\r\nAB", b"abcdefghijklmno",
             "界aé🙂e\u0301AB".encode(), "1234567界AB".encode()]
    for index, text in enumerate(texts):
        for value in (b"A", b"B", b" ", b"\n", b"ab", b"\xa9", b"\xcc"):
            yield case(f"bytes/{index}/{value.hex()}",
                       [text, needle(value), feed, read, clean, read])

    mutations = {
        "overwrite": b"\x1b[HA", "erase-line": b"\x1b[1;1H\x1b[2K",
        "erase-display": b"\x1b[2J", "erase-history": b"\x1b[3J",
        "delete-line": b"\x1b[2;1H\x1b[M", "insert-line": b"\x1b[2;1H\x1b[L",
        "scroll": b"\r\nA\r\nF\r\nG\r\nH", "wrap": b"0123456789A123456789",
        "narrow": {"op": "resize", "cols": 5, "rows": 4},
        "wide": {"op": "resize", "cols": 12, "rows": 4},
        "short": {"op": "resize", "cols": 8, "rows": 2},
        "tall": {"op": "resize", "cols": 8, "rows": 6},
        "reset": {"op": "terminal_reset"}, "ris": b"\x1bc",
    }
    for name, mutation in mutations.items():
        for dirty in (False, True):
            yield case(f"mutation/{name}/{dirty}",
                       [b"A B\r\nC A\r\nA D\r\nE A", needle(b"A"), feed, read,
                        mutation, grid("search_feed", active_dirty=dirty), read,
                        feed, read, needle(b""), read])
    for mode in (47, 1049):
        enter = f"\x1b[?{mode}h".encode()
        leave = f"\x1b[?{mode}l".encode()
        yield case(f"screens/{mode}", [b"A B", needle(b"A"), feed, read,
                    enter + b"C A", clean, read, leave, clean, read,
                    enter, clean, read, b"\x1bc", clean, read, needle(b"")])

    for name, prefix, change in (
        ("ind", b"\x1b[2;3r\x1b[3;1H", b"\n"),
        ("history-suffix", b"\x1b[1;3r\x1b[3;1H", b"\n"),
        ("reverse-index", b"\x1b[2;3r\x1b[2;1H", b"\x1bM"),
        ("scroll-up", b"\x1b[2;3r", b"\x1b[S"),
        ("scroll-down", b"\x1b[2;3r", b"\x1b[T"),
        ("partial-width", b"\x1b[?69h\x1b[2;6s\x1b[2;2H", b"\x1b[M"),
    ):
        for disabled in (False, True):
            setup = [grid("limits", bytes=0)] if disabled else []
            yield case(f"layout/{name}/{disabled}",
                       [*setup, b"A B\r\nC A\r\nA D\r\nE A", prefix,
                        needle(b"A"), feed, read, change, clean, read, feed, read])
    yield case("layout/single-row", [grid("limits", bytes=0), b"A",
                needle(b"A"), feed, read, b"\n", clean, read], rows=1)

    for cols, count in ((80, 1024), (215, 512), (1024, 160)):
        texts = [("hard", b"A\r\n" * (count - 1) + b"A", [b"A", b"A\nA"]),
                 ("blank", b"A\r\n" + b"\r\n" * (count - 2) + b"A", [b"A", b"\n"]),
                 ("soft", (b"." * (cols - 1) + b"A") * count, [b"A.", b".A."])]
        for name, text, values in texts:
            for value in values:
                yield case(f"pages/{cols}/{name}/{value.hex()}",
                           [text, needle(value), feed, read,
                            grid("viewport", delta=-100000), clean, read,
                            grid("viewport", delta=1), clean, read,
                            grid("viewport", delta=count // 2), clean, read,
                            grid("viewport", delta=1), clean, read,
                            b"\x1b[HA", feed, read,
                            grid("viewport", delta=100000), clean, read], cols=cols)

    # A needle longer than one page requires multiple overlap pages. Distinct
    # row prefixes expose the native reverse order for preceding pages.
    text = b"".join(f"{row:04}".encode() + b"." * 1020 for row in range(160))
    long_needle = text[15 * 1024:100 * 1024]
    yield case("pages/multiple-overlap", [text, needle(long_needle), feed, read,
                grid("viewport", delta=-100000), clean, read,
                grid("viewport", delta=55), clean, read,
                grid("viewport", delta=50), clean, read], cols=1024)

    data = snapshots.fixture(Path(__file__).resolve().parents[2]
                             / "src/terminal/snapshot/testdata/complete-v1.hex").hex()
    for phase, restore in [
        ("ready", [{"op": "restore_ready", "data": data}]),
        ("prepended", [{"op": "restore_ready", "data": data}, {"op": "restore_next"}]),
        ("complete", [{"op": "restore", "data": data}]),
    ]:
        for value in (b"A", b"B", b"C", b"\n"):
            yield case(f"restored/{phase}/{value.hex()}",
                       [*restore, needle(value), feed, read,
                        grid("viewport", delta=-1000), clean, read,
                        grid("viewport", delta=1000), clean, read])


if __name__ == "__main__":
    import json
    import sys

    json.dump([{"request": request, "covers": covers} for request, covers in requests()],
              sys.stdout, indent=2)
    sys.stdout.write("\n")
