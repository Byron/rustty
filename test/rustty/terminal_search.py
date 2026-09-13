"""Incremental history search and selected-match lifecycle."""
from pathlib import Path

import snapshots
from viewport_search import grid, needle


def case(name, operations, cols=8, rows=4):
    return ({"id": "grid/search/terminal/" + name, "kind": "input",
             "cols": cols, "rows": rows,
             "operations": [op if isinstance(op, dict)
                            else {"op": "write", "data": op.hex()}
                            for op in operations]}, ["terminal.search"])


def requests():
    state = grid("search_status")
    found = grid("search_matches")
    feed = grid("search_feed")
    clean = grid("search_feed", active_dirty=False)
    tick = grid("search_tick")
    run = grid("search_run")
    next_match = grid("search_next")
    previous = grid("search_prev")
    yield case("idle", [state, tick, found, next_match, previous,
                        needle(b"A"), state, tick, found, clean, state,
                        tick, tick, feed, state, tick, found,
                        needle(b""), state, next_match, tick, found])
    yield case("needle", [b"cat CAT cat\r\nA", needle(b"cat"), run, next_match,
                          state, needle(b"CAT"), state, previous, needle(b"A"),
                          state, run, next_match, needle(b""), state, found])
    yield case("text-selection", [b"A B A", grid("select", start={"x": 2}, end={"x": 2}),
                                  needle(b"A"), next_match, previous, found,
                                  grid("clear_selection"), state])
    for index, text in enumerate([b"ABBA AB", b"A  B\r\n\r\nAB",
                                 b"abcdefghijklmno", "界aé🙂e\u0301AB".encode()]):
        for value in (b"A", b"B", b" ", b"\n", b"ab", b"\xa9", b"\xcc"):
            yield case(f"bytes/{index}/{value.hex()}", [text, needle(value), state,
                       feed, state, tick, state, feed, state, found,
                       next_match, next_match, previous, previous,
                       grid("search_match", id=0), grid("search_match", id=9999)])

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
        for chosen in (next_match, previous):
            for dirty in (False, True):
                yield case(f"mutation/{name}/{chosen['grid']['action']}/{dirty}",
                           [b"A B\r\nC A\r\nA D\r\nE A", needle(b"A"), run,
                            chosen, mutation, grid("search_feed", active_dirty=dirty),
                            state, found, feed, state, found, next_match, previous])
    for mode in (47, 1049):
        enter, leave = f"\x1b[?{mode}h".encode(), f"\x1b[?{mode}l".encode()
        yield case(f"screens/{mode}", [b"A B A", needle(b"A"), run, next_match,
                    enter + b"C A", state, clean, found, previous, tick,
                    leave, state, clean, found, next_match,
                    enter, clean, found, b"\x1bc", clean, found, run, state])
    for cols, count in ((80, 1024), (215, 512), (1024, 160)):
        texts = [("hard", b"A\r\n" * (count - 1) + b"A", [b"A", b"A\nA"]),
                 ("blank", b"A\r\n" + b"\r\n" * (count - 2) + b"A", [b"A", b"\n"]),
                 ("soft", (b"." * (cols - 1) + b"A") * count, [b"A.", b".A."])]
        for name, text, values in texts:
            for value in values:
                yield case(f"pages/{cols}/{name}/{value.hex()}",
                           [text, needle(value), clean, state, tick, tick,
                            clean, state, tick, next_match, clean, tick, found,
                            run, state, found, previous, previous, next_match,
                            grid("search_prev", scroll=False), state,
                            grid("search_match", id=0), grid("search_match", id=9999)], cols=cols)
        for name, mutation in [
            ("append", b"\r\nA\r\n" * count),
            ("prune", grid("limits", lines=5)),
            ("erase-history", b"\x1b[3J"), ("reset", {"op": "terminal_reset"}),
            ("narrow", {"op": "resize", "cols": 5, "rows": 4}),
            ("height", {"op": "resize", "cols": cols, "rows": 9}),
        ]:
            for selected in (next_match, previous):
                yield case(f"history-change/{cols}/{name}/{selected['grid']['action']}",
                           [b"A\r\n" * count, needle(b"A"), run, selected,
                            mutation, feed, state, found, tick, feed, run, found,
                            next_match, previous], cols=cols)
        yield case(f"owned-window/{cols}", [b"A\r\n" * count, needle(b"A"), feed,
                    tick, clean, b"\x1b[HBBBB", tick, feed, found, run, found], cols=cols)
    for value in (b"A", b"AB", b"\n"):
        yield case(f"no-scrollback/{value.hex()}", [grid("limits", bytes=0),
                    b"AB\r\nA\r\nB\r\nAB", needle(value), clean, state, tick,
                    found, next_match, b"\r\nA", feed, found, previous, run, state])

    text = b"".join(f"{row:04}".encode() + b"." * 1020 for row in range(160))
    for start, end in ((30 * 1024 - 3, 30 * 1024 + 5),
                       (15 * 1024, 100 * 1024), (0, len(text))):
        value = text[start:end]
        yield case(f"long-overlap/{start}/{end}", [text, needle(value), feed,
                    state, tick, clean, state, tick, clean, tick, run, found,
                    next_match, previous], cols=1024)
    for dirty in (False, True):
        for count in (1, 40, 180):
            yield case(f"interrupt/{dirty}/{count}", [b"A\r\n" * 180,
                        needle(b"A"), feed, tick, clean, next_match,
                        b"A\r\n" * count, grid("search_feed", active_dirty=dirty),
                        state, tick, found, run, found, previous, next_match], cols=1024)
    for delta in (-1, 0, 1, -10000, 10000):
        yield case(f"scroll-column/{delta}", [b"....A\r\n" * 180, needle(b"A"),
                    run, previous, previous, grid("viewport", delta=delta), state,
                    next_match, clean, grid("search_viewport")], cols=1024)
    for value in (b"A", b"A\nA"):
        yield case(f"no-scrollback/ed22/{value.hex()}", [grid("limits", bytes=0),
                    b"A\r\nA\r\nA\r\nA\x1b[22J", needle(value), run, found,
                    previous, previous, next_match, state])
    for cols in (3, 6, 120):
        yield case(f"review/viewport-resize/{cols}", [b"....A\r\n" * 300,
                    needle(b"A"), run, *[next_match] * 7,
                    {"op": "resize", "cols": cols, "rows": 4}, feed, state,
                    grid("viewport", delta=-1), state], cols=80)
    for complete in (False, True):
        for value in (b"A", b"A\nA"):
            for mutation in (b"\x1b[1;1H\x1b[L", b"\x1b[1;1H\x1b[M", b"\x1b[3J"):
                yield case(f"review/history-move/{complete}/{value.hex()}/{mutation.hex()}",
                           [b"A\r\n" * 300, needle(value), run if complete else feed,
                            tick, clean, tick, mutation, feed, found], cols=1024)

    data = snapshots.fixture(Path(__file__).resolve().parents[2]
                             / "src/terminal/snapshot/testdata/complete-v1.hex").hex()
    for value in (b"A", b"B", b"C", b"\n"):
        yield case(f"prepend/{value.hex()}", [{"op": "restore_ready", "data": data},
                    needle(value), run, found, next_match, {"op": "restore_next"},
                    clean, state, tick, clean, run, found, previous, next_match])
        for mutation in (b"\x1b[1;8HZ", b"\x1b[2J", b"\x1b[2;1H\x1b[L",
                         b"\x1b[2;1H\x1b[M", b"\r\nA\r\nB"):
            yield case(f"restored-edit/{value.hex()}/{mutation.hex()}",
                       [{"op": "restore", "data": data}, needle(value), run,
                        previous, mutation, feed, found, next_match, run, found])


if __name__ == "__main__":
    import json
    import sys

    json.dump([{"request": request, "covers": covers} for request, covers in requests()],
              sys.stdout, indent=2)
    sys.stdout.write("\n")
