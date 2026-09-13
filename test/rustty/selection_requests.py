"""Pure selection queries, observed without replacing the active selection."""


def requests():
    def point(x=0, y=0, tag="active"):
        return {"x": x, "y": y, "tag": tag}

    def grid(action, **options):
        return {"op": "grid", "grid": {"action": action, **options}}

    def case(name, text, operations, cols=8, rows=4):
        return ({"id": "grid/selectors/" + name, "cols": cols, "rows": rows,
                 "operations": [{"op": "write", "data": text.hex()},
                                grid("select", start=point(), end=point()),
                                *operations, grid("observe")]}, ["terminal.selection"])

    texts = [b"", b"        ", b"one:two", b"  a  \r\n b   ",
             b"abcdefgh\r\nijklmnop", b"abcdefghijklmnopq",
             "a界b e\u0301🙂".encode(), "1234567界abc".encode(),
             "a\u00a0b\u3000c".encode(), b"\x1b[44m\x1b[2Jab\x1b[0m"]
    for index, text in enumerate(texts):
        queries = [grid("select_all")]
        for y in range(4):
            for x in range(8):
                queries.extend([grid("select_word", point=point(x, y)),
                                grid("select_line", point=point(x, y))])
        yield case(f"cells/{index}", text, queries)

    for index, boundaries in enumerate(([], [32], [ord(c) for c in ":;"],
                                         [ord(c) for c in "界é"])):
        yield case(f"word-boundaries/{index}", "ab::cd ;界é".encode(),
                   [grid("select_word", point=point(x, y), boundary_codepoints=boundaries)
                    for y in range(3) for x in range(8)])

    for index, text in enumerate((b"ab     \r\n   cd", b"ab:cd   ef", b"", b"   ",
                                 "a界b".encode())):
        pairs = [(point(), point(7, 2)), (point(7, 2), point()),
                 (point(7), point(3, 1)), (point(3, 1), point(7)),
                 (point(1), point(1)), (point(2), point(2)),
                 (point(2), point(7)), (point(7), point(2)),
                 (point(3), point(4)), (point(4), point(3))]
        yield case(f"word-between/{index}", text,
                   [grid("select_word_between", start=start, end=end) for start, end in pairs])

    semantic = [
        b"\x1b]133;A\x07  PP\x1b]133;B\x07  II\x1b]133;C\x07  OO",
        b"\x1b]133;A\x07prompt-prompt\x1b]133;B\x07input-input\x1b]133;C\x07output-output",
        b"\x1b]133;A\x07  \x1b]133;B\x07input\x1b]133;C\x07out",
        b"\x1b]133;A\x07prompt\x1b]133;B\x07   \x1b]133;C\x07out",
    ]
    for index, text in enumerate([b"  aa  \r\n\r\n bb  ", b"abcdefghijklmno", *semantic]):
        for option_index, options in enumerate(({}, {"trim_line": False}, {"whitespace": []},
                                                {"whitespace": [32, 97]},
                                                {"semantic_prompt_boundary": False})):
            yield case(f"line-options/{index}/{option_index}", text,
                       [grid("select_line", point=point(x, y), **options)
                        for y in range(4) for x in range(8)])

    for cols, rows in ((1, 1), (1, 4), (8, 1)):
        for text in (b"", b" ", b"a", b"a\r\nb", b"abcdefghijk"):
            yield case(f"small/{cols}/{rows}/{text.hex()}", text,
                       [grid("select_all"),
                        *[grid(action, point=point(x, y))
                          for action in ("select_word", "select_line")
                          for y in range(rows) for x in range(cols)]], cols, rows)

    history = b"one two\r\nthree\r\nfour five\r\nsix\r\nseven"
    for tag in ("active", "viewport", "screen", "history"):
        yield case("history/" + tag, history,
                   [grid("viewport", delta=-2), grid("select_all"),
                    *[grid(action, point=point(x, y, tag))
                      for action in ("select_word", "select_line")
                      for y in range(3) for x in (0, 4, 7)]])
    for alternate in (b"\x1b[?47h", b"\x1b[?1049h"):
        yield case("alternate/" + alternate.hex(), history + alternate + b"one two\r\nthree",
                   [grid("select_all"), grid("select_word", point=point(4)),
                    grid("select_line", point=point(1))])

    prompt = b"\x1b]133;A\x07"
    continuation = b"\x1b]133;P;k=s\x07"
    input_start = b"\x1b]133;B\x07"
    output = b"\x1b]133;C\x07"
    command = prompt + b"$ " + input_start + b"cmd\r\n" + output
    output_cases = [
        ("unmarked", b"free\r\noutput"), ("blank", b""),
        ("output-only", output + b"free"),
        ("before-prompt", b"pre\r\n\r\n" + prompt + b"P"),
        ("blank-prefix", b"\r\n\r\n" + prompt + b"P"),
        ("space-prefix", b"   \r\n \r\n" + prompt + b"P"),
        ("prompt-only", prompt + b"P"),
        ("input-only", prompt + b"P" + input_start + b"input"),
        ("blank-command", command),
        ("spaces", command + b"  out \r\n\r\n" + prompt + b"P"),
        ("background", command + b"\x1b[44m\x1b[K"),
        ("space-background", command + b" \x1b[44m\x1b[K"),
        ("inline", prompt + b"P" + input_start + b"in" + output + b"out"),
        ("inline-prompts", prompt + b"P" + output + b"a" + prompt + b"Q" + output + b"b"),
        ("wrap", command + b"abcdefghijklmno\r\ntail"),
        ("wide", command + "a界e\u0301🙂".encode()),
        ("wide-wrap", command + "1234567界abc".encode()),
        ("commands", b"pre\r\n" + command + b"one\r\n" + command + b"two"),
        ("continuation", prompt + b"P\r\n" + continuation + b"Q" + input_start
         + b"in\r\n" + output + b"out"),
        ("clipped-continuation", continuation + b"P" + output + b"early\r\n"
         + continuation + b"Q" + output + b"last\r\nend"),
        ("gapped-continuation", b"old\r\n" + continuation + b"P\r\n"
         + continuation + b"Q\r\n" + output + b"out"),
        ("erased-prompt", command + b"out\x1b[1;1H\x1b[2K"),
    ]
    for name, text in output_cases:
        yield case("output/" + name, text,
                   [grid("select_output", point=point(x, y))
                    for y in range(6) for x in range(8)], rows=6)

    for cols, rows in ((1, 1), (1, 4), (4, 1)):
        for index, text in enumerate((prompt + b"P" + output + b"a", b"a\r\n" + prompt + b"P")):
            yield case(f"output/small/{cols}/{rows}/{index}", text,
                       [grid("select_output", point=point(x, y))
                        for y in range(rows) for x in range(cols)], cols, rows)

    history = b"pre\r\n" + (command + b"out\r\n") * 4
    for tag in ("active", "viewport", "screen", "history"):
        yield case("output/history/" + tag, history,
                   [grid("viewport", delta=-3),
                    *[grid("select_output", point=point(x, y, tag))
                      for y in range(10) for x in (0, 4, 7)]], rows=3)
    for alternate in (b"\x1b[?47h", b"\x1b[?1049h"):
        yield case("output/alternate/" + alternate.hex(), history + alternate + command + b"alt",
                   [grid("select_output", point=point(x, y))
                    for y in range(4) for x in range(8)])
    for invalid in (point(8), point(65535), point(y=65535)):
        yield case(f"output/invalid/{invalid['x']}/{invalid['y']}", command + b"out",
                   [grid("select_output", point=invalid)])

    for action in ("select_word", "select_word_between", "select_line"):
        for invalid in (point(8), point(65535), point(y=65535)):
            yield case(f"invalid/{action}/{invalid['x']}/{invalid['y']}", b"one two",
                       [grid(action, point=invalid, start=point(), end=invalid)])


def snapshot_requests(reference):
    """Physical columns stay authoritative after restoring a different logical width."""
    import struct
    from snapshots import frame, records, write

    def snapshot(columns, rows, text):
        source = reference.request({"id": "selection-source", "cols": columns, "rows": rows,
                                    "operations": [write(text), {"op": "snapshot"}]})
        if not source["ok"] or len(source["snapshots"]) != 1:
            raise RuntimeError("reference could not encode selector fixture")
        return records(bytes.fromhex(source["snapshots"][0]))

    for physical, logical in ((4, 8), (8, 4)):
        parts = snapshot(physical, 2, b"ab cd efgh")
        terminal = bytearray(parts[0][1])
        struct.pack_into("<H", terminal, 0, logical)
        struct.pack_into("<H", terminal, 18, logical - 1)
        parts[0] = (1, terminal)
        if physical > logical:
            screen = bytearray(parts[1][1])
            struct.pack_into("<H", screen, 12, min(logical - 1, struct.unpack_from("<H", screen, 12)[0]))
            screen[17] &= ~1
            parts[1] = (2, screen)
        operations = [{"op": "restore", "data": frame(parts).hex()}]
        for action in ("select_word", "select_line"):
            operations.extend({"op": "grid", "grid": {
                "action": action, "point": {"tag": "screen", "x": x, "y": y}}}
                for y in range(2) for x in range(physical))
        operations.append({"op": "grid", "grid": {"action": "select_all"}})
        yield ({"id": f"grid/selectors/physical-{physical}-logical-{logical}",
                "operations": operations}, ["terminal.selection", "snapshot.cross-decode"])

    for widths in ((8, 4, 8), (4, 8, 4)):
        parts = snapshot(8, 6, b"")
        pages = [snapshot(width, 2, bytes([65 + index]) * (2 * width))[2]
                 for index, width in enumerate(widths)]
        parts[2:3] = pages
        screen = bytearray(parts[1][1])
        struct.pack_into("<H", screen, 2, len(pages))
        parts[1] = (2, screen)
        operations = [{"op": "restore", "data": frame(parts).hex()}]
        operations.extend({"op": "grid", "grid": {
            "action": action, "point": {"tag": "screen", "x": x, "y": y}}}
            for action in ("select_word", "select_line") for y in range(6) for x in range(8))
        operations.append({"op": "grid", "grid": {"action": "select_all"}})
        yield ({"id": "grid/selectors/mixed-pages/" + "-".join(map(str, widths)),
                "operations": operations}, ["terminal.selection", "snapshot.cross-decode"])

    output_pages = [
        ("groups", (b" \r\npre",
                    b"\x1b]133;A\x07P\x1b]133;B\x07i\x1b]133;C\x07O\r\nout ",
                    b"tail\r\n\x1b]133;A\x07P")),
        ("continuations", (b"\x1b]133;P;k=s\x07P\x1b]133;C\x07X\r\n\x1b]133;P;k=s\x07P",
                           b"\x1b]133;P;k=s\x07P\x1b]133;C\x07O\r\nout",
                           b"tail\r\n\x1b]133;A\x07P")),
    ]
    for logical in (4, 8):
        for widths in ((8, 4, 8), (4, 8, 4)):
            for name, texts in output_pages:
                parts = snapshot(logical, 6, b"")
                pages = [snapshot(width, 2, text)[2] for width, text in zip(widths, texts)]
                parts[2:3] = pages
                screen = bytearray(parts[1][1])
                struct.pack_into("<H", screen, 2, len(pages))
                parts[1] = (2, screen)
                operations = [{"op": "restore", "data": frame(parts).hex()}]
                operations.extend({"op": "grid", "grid": {
                    "action": "select_output", "point": {"tag": "screen", "x": x, "y": y}}}
                    for y in range(6) for x in range(8))
                yield ({"id": f"grid/selectors/output/mixed-pages/{name}/{logical}/"
                        + "-".join(map(str, widths)), "operations": operations},
                       ["terminal.selection", "snapshot.cross-decode"])
