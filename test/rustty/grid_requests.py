"""Selection extraction, literal search and tracked references through live edits."""


def requests():
    def grid(action="observe", **options):
        return {"op": "grid", "grid": {"action": action, **options}}

    def point(x=0, y=0, tag="active"):
        return {"tag": tag, "x": x, "y": y}

    def case(name, operations, covers, **options):
        return ({"id": "grid/" + name, "cols": 8, "rows": 4,
                 "operations": [op if isinstance(op, dict) else {"op": "write", "data": op.hex()}
                                for op in operations], **options}, covers)

    observe = grid()
    texts = [b"", b"one two", b"  a  \r\n b   ", b"abcdefghijklmnopqr",
             "界aé🙂".encode(), "e\u0301 #\ufe0f!".encode()]
    bounds = [(point(), point(3)), (point(3), point()), (point(1), point(6, 1)),
              (point(6, 1), point(1)), (point(6), point(1, 1)),
              (point(1, 1), point(6)), (point(1), point(1)), (point(7), point(7, 3))]
    for text_index, text in enumerate(texts):
        for bound_index, (start, end) in enumerate(bounds):
            for rectangle in (False, True):
                yield case(f"selection/bounds/{text_index}/{bound_index}/{int(rectangle)}",
                           [text, grid("select", start=start, end=end, rectangle=rectangle),
                            grid("clear_selection"), observe], ["terminal.selection"])

    # Derived from native Screen.selectionString tests; CRLF reproduces the
    # testWriteString helper's hard line break through the real stream API.
    native_selections = [
        ("trim-space", b"1AB  \r\n2EFGH\r\n3IJKL", point(), point(2, 1)),
        ("internal-empty-line", b"1AB  \r\n\r\n2EFGH\r\n3IJKL", point(), point(2, 2)),
        ("internal-written-spaces", b"1AB\r\n     \r\n2EFGH", point(), point(2, 2)),
        ("unwritten-tail", b"A", point(), point(4, 3)),
        ("written-space-tail", b"A\r\n     ", point(), point(4, 3)),
        ("only-unwritten", b"", point(), point(4, 2)),
        ("only-written-spaces", b"     \r\n     ", point(), point(4, 2)),
        ("soft-wrap", b"1ABCD2EFGH3IJKL", point(0, 1), point(2, 2)),
        ("wide-end-tail", "1A⚡".encode(), point(), point(3)),
        ("wide-end-lead", "1A⚡".encode(), point(), point(2)),
        ("wide-only-tail", "1A⚡".encode(), point(3), point(3)),
        ("wide-end-head", "1ABC⚡".encode(), point(), point(4)),
        ("wide-start-head", "1ABC⚡Z".encode(), point(4), point(2, 1)),
        ("wide-only-head", "1ABC⚡".encode(), point(4), point(4)),
        ("wide-empty-soft-wrap", "👨      ".encode(), point(1), point(2)),
        ("partial-wrap-spaces", b"ab    cd", point(), point(3)),
        ("final-wrap-spaces", b"ab    cd", point(), point(4)),
        ("zwj", b"\x1b[?2027h" + "👨‍".encode(), point(), point(1)),
    ]
    for name, text, start, end in native_selections:
        for reverse in (False, True):
            for rectangle in (False, True):
                a, b = (end, start) if reverse else (start, end)
                yield case(f"selection/native/{name}/{int(reverse)}/{int(rectangle)}",
                           [text, grid("select", start=a, end=b, rectangle=rectangle)],
                           ["terminal.selection"], cols=5, rows=5)

    changes = {
        "erase-line": b"\x1b[1;1H\x1b[2K", "erase-display": b"\x1b[2J",
        "erase-history": b"\x1b[3J", "delete-line": b"\x1b[1;1H\x1b[M",
        "scroll": b"\r\nE\r\nF\r\nG\r\nH", "wrap": b"01234567890123456789",
        "narrow": {"op": "resize", "cols": 5, "rows": 4},
        "wide": {"op": "resize", "cols": 12, "rows": 4},
        "short": {"op": "resize", "cols": 8, "rows": 2},
        "tall": {"op": "resize", "cols": 8, "rows": 6},
        "reset": {"op": "terminal_reset"}, "ris": b"\x1bc",
        "alternate47": b"\x1b[?47hother\x1b[?47l",
        "alternate1049": b"\x1b[?1049hother\x1b[?1049l",
    }
    for name, change in changes.items():
        initial = b"abcdefghijklmnopqr\r\nlast"
        yield case("selection/mutation/" + name,
                   [initial, grid("select", start=point(1), end=point(4, 1)), change, observe],
                   ["terminal.selection", "terminal.resize", "terminal.tracked"])
        for tag in ("active", "viewport", "screen", "history"):
            yield case(f"tracked/mutation/{name}/{tag}",
                       [initial, grid("track", id=1, point=point(1, 0, tag)),
                        grid("track", id=2, point=point(4, 1, tag)), change, observe],
                       ["terminal.tracked", "terminal.resize"])

    history = b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive"
    for delta in (-100, -2, -1, 0, 1, 100):
        yield case(f"viewport/{delta}", [history, grid("viewport", delta=delta),
                   grid("track", id=1, point=point(1, 0, "viewport")),
                   grid("select", start=point(0, 0, "viewport"), end=point(2, 1, "viewport")),
                   b"\r\nsix", observe], ["terminal.selection", "terminal.tracked"])
    for reset in ({"op": "terminal_reset"}, {"op": "reset"}):
        yield case("tracked/reuse/" + reset["op"], [b"A", grid("track", id=1), reset,
                   b"BC", grid("track", id=2, point=point(1)), observe, grid("untrack", id=2)],
                   ["terminal.tracked", "terminal.reset"])
    yield case("tracked/untrack", [b"AB", grid("track", id=1), grid("untrack", id=1),
               grid("track", id=2, point=point(1)), observe, grid("untrack", id=2)], ["terminal.tracked"])
    yield case("tracked/untrack-inactive", [b"AB", grid("track", id=1), b"\x1b[?47h",
               grid("untrack", id=1)], ["terminal.tracked"])
    for alternate in (False, True):
        prefix = b"\x1b[?47h" if alternate else b""
        switch = b"\x1b[?47l" if alternate else b"\x1b[?47h"
        yield case(f"tracked/lifetime/inactive/{alternate}",
                   [prefix + b"A", grid("track", id=1), switch + b"B",
                    grid("track", id=2), grid("untrack", id=1), observe,
                    grid("untrack", id=2)], ["terminal.tracked"])
        for reset in ({"op": "terminal_reset"}, {"op": "reset"}):
            yield case(f"tracked/lifetime/reset/{alternate}/{reset['op']}",
                       [prefix + b"A", grid("track", id=1), reset, prefix + b"BC",
                        grid("track", id=2, point=point(1)), grid("untrack", id=1),
                        observe, grid("untrack", id=2)], ["terminal.tracked", "terminal.reset"])
    for limit in (0, 1, 4):
        yield case(f"tracked/prune/{limit}", [history, grid("track", id=1, point=point(tag="screen")),
                   grid("limits", lines=limit), b"\r\nmore\r\nrows", observe], ["terminal.tracked"])
    for lines, byte_limit in ((0, None), (1, None), (None, 1), (0, 1), (None, 0)):
        yield case(f"limits/minimum/{lines}/{byte_limit}",
                   [grid("limits", lines=lines, bytes=byte_limit), history, observe,
                    {"op": "resize", "cols": 12, "rows": 6}, b"\r\nmore\r\nrows", observe],
                   ["terminal.tracked", "terminal.resize"])

    for alternate in (False, True):
        for margin, setup in (("full", b""), ("vertical", b"\x1b[2;3r"),
                              ("top", b"\x1b[1;3r"),
                              ("horizontal", b"\x1b[?69h\x1b[3;6s")):
            for command in "LMST":
                for count in (0, 1, 2, 5):
                    prefix = b"\x1b[?47h" if alternate else b""
                    operations = [prefix + b"aaaaaa\r\nbbbbbb\r\ncccccc\r\ndddddd"]
                    operations.extend(grid("track", id=y, point=point(1, y)) for y in range(4))
                    operations.extend([
                        grid("select", start=point(1), end=point(3, 2)),
                        setup + b"\x1b[2;3H" + f"\x1b[{count}{command}".encode(), observe,
                    ])
                    yield case(f"tracked/row-shift/{alternate}/{margin}/{command}/{count}",
                               operations, ["terminal.tracked", "terminal.selection"])

    yield case("index/corpus-wrap", [b"\x1b[5W\x1b[4r\x1b[\t33BhD"],
               ["terminal.cells"], cols=80, rows=24)
    for mode, prefix in (("primary", []), ("no-history", [grid("limits", bytes=0)]),
                         ("alternate", [b"\x1b[?47h"])):
        yield case(f"index/single-row/{mode}", [*prefix, b"abc", grid("track", id=1, point=point(2)),
                   grid("select", start=point(1), end=point(2)), b"\n", observe],
                   ["terminal.cells", "terminal.tracked", "terminal.selection"], rows=1)
    for mode in ("primary", "no-history", "alternate", "no-history-retained"):
        initial = [grid("limits", bytes=0)] if mode.startswith("no-history") else []
        if mode == "alternate":
            initial.append(b"\x1b[?47h")
        elif mode == "no-history-retained":
            initial.append(b"A\x1b[22J")
        for top, bottom, horizontal in ((0, 3, False), (1, 3, False),
                                         (0, 2, False), (1, 3, True)):
            margins = f"\x1b[{top + 1};{bottom + 1}r".encode()
            if horizontal:
                margins += b"\x1b[?69h\x1b[3;6s"
            for name, command in (("lf", b"\n"), ("ind", b"\x1bD"),
                                  ("wrap", b"XY")):
                col = (6 if horizontal else 8) if name == "wrap" else 3
                operations = [*initial, b"abcdefghijklmnopqrstuvwxy", margins,
                              f"\x1b[{bottom + 1};{col}H\x1b[44m".encode()]
                operations.extend(grid("track", id=y, point=point(2, y)) for y in range(4))
                operations.extend([grid("select", start=point(2, top), end=point(3, bottom)),
                                   command, observe, {"op": "pages"}])
                yield case(f"index/{mode}/{top}/{bottom}/{horizontal}/{name}", operations,
                           ["terminal.cells", "terminal.tracked", "terminal.selection", "terminal.pages"])

    # 1024 columns put the native page boundary inside this active screen.
    # Include a region beginning exactly at that boundary on macOS ARM64.
    for mode in ("primary", "no-history", "alternate"):
        initial = [grid("limits", bytes=0)] if mode == "no-history" else []
        if mode == "alternate":
            initial.append(b"\x1b[?47h")
        for top, bottom in ((0, 47), (45, 47), (46, 47)):
            operations = [*initial, b"\r\n".join(f"{row:02}".encode() for row in range(48))]
            operations.extend(grid("track", id=y, point=point(1, y))
                              for y in sorted({0, max(0, top - 1), top, top + 1, 45, 46, 47}))
            operations.extend([
                grid("select", start=point(1, top), end=point(1, bottom)),
                f"\x1b[{top + 1};{bottom + 1}r\x1b[{bottom + 1};3H\x1b[44m\x1bD".encode(),
                observe, {"op": "pages"},
            ])
            yield case(f"index/pages/{mode}/{top}/{bottom}", operations,
                       ["terminal.tracked", "terminal.selection", "terminal.pages"],
                       kind="input", cols=1024, rows=48)

    search_texts = [b"one two one", b"abcabcabc", b"abababa", b"abcdefghijklmnopqr",
                    b"one\r\none\r\none\r\none\r\none", b"a  b", "a界e\u0301🙂界".encode()]
    needles = [b"", b"one", b"abc", b"aba", b"hij", b" ", b"a.b", "界".encode(), "\u0301".encode()]
    for ti, text in enumerate(search_texts):
        for ni, needle in enumerate(needles):
            yield case(f"search/literal/{ti}/{ni}", [text, grid("search", needle=needle.hex())],
                       ["terminal.search"])
    # Native search is byte based and ASCII-case-insensitive. PageFormatter's
    # pending-space maps and hard newline coordinates are compared verbatim.
    native_searches = [
        ("overlap-case", "abABaba", 8, [b"aba", b"AbA", b"bab", b"abABaba"]),
        ("ascii-case", "aAaAa", 8, [b"aa", b"A", b"aAa"]),
        ("unicode-case", "Ééé", 8, ["É".encode(), "é".encode(), b"\xc3", b"\xa9", b"\x89"]),
        ("combining-bytes", "e\u0301e\u0301", 8, [b"e", b"\xcc", b"\x81", b"\x81e"]),
        ("raw-bytes", "a界e\u0301🙂界", 8, [b"\xe7", b"\x95\x8ce", b"\xf0", b"\x99", b"\0", b"\xff"]),
        ("spaces", "a  b", 8, [b" ", b"  ", b"a ", b" b"]),
        ("wrap-spaces", "a    b", 4, [b" ", b"  ", b"    ", b"a    b"]),
        ("wide-spaces", "a  界", 4, [b" ", b"  ", " 界".encode(), b"\xe7"]),
        ("trim", "a   \r\nb   ", 8, [b" ", b"a\nb", b"b\n", b"\n"]),
        ("hard-lines", "a\r\nb", 8, [b"a\nb", b"ab", b"\n", b"b\n"]),
        ("blank-lines", "a\r\n\r\nb", 8, [b"\n", b"\n\n", b"a\n\nb", b"b\n"]),
        ("trailing-blanks", "a\r\n\r\n", 8, [b"\n", b"\n\n", b"a\n"]),
        ("empty", "", 8, [b"\n", b"\n\n", b" "]),
        ("only-spaces", "  \r\n   ", 8, [b" ", b"\n", b"\n\n"]),
        ("space-combining", "a \u0301b", 8, [b" ", "\u0301".encode(), b"a b"]),
        ("written-blank-lines", "a\r\n\r\n   \r\n   \r\nb", 8, [b"\n", b"\n\n", b"b\n"]),
        ("literal-syntax", "a.b ab (a)", 8, [b"a.b", b"(a)", b"a.*b", b""]),
    ]
    for name, text, cols, needles in native_searches:
        for index, needle in enumerate(needles):
            yield case(f"search/native/{name}/{index}",
                       [text.encode(), grid("search", needle=needle.hex())],
                       ["terminal.search"], cols=cols)
    for name, change in changes.items():
        yield case("search/mutation/" + name, [history, grid("search", needle=b"o".hex()), change,
                   grid("search", needle=b"o".hex())], ["terminal.search"])
    yield case("search/raw-needle", [b"abc", grid("search", needle="ff")], ["terminal.search"])
