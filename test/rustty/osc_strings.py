"""OSC title/PWD bytes, framing, capture bounds and command prefixes."""
import base64


def requests():
    def case(name, operations, **options):
        return ({"id": "protocol/osc/" + name, "host": {"title_report": True},
                 "operations": [op if isinstance(op, dict) else {"op": "write", "data": op.hex()} for op in operations],
                 **options}, ["effects.title", "effects.pwd", "effects.host", "parser.events", "effects.pty"])

    def osc(number, body, end=b"\x07"):
        return b"\x1b]" + str(number).encode() + b";" + body + end

    observe = {"op": "observe"}
    initial = osc(2, b"before") + osc(7, b"file:///before")
    query = b"\x1b[21t"
    values = [b"", b"plain title", "héllö 世界 😀".encode(), b"semi;colon=equals", b"file:///tmp/a%20b",
              b"file://hostname/tmp/path", b"file:///tmp/%FF", b"/relative-or-absolute", b"https://example.org/path",
              b"bad:\x00value", b"raw\xffvalue", b"\xc0\xaf", b"\xed\xa0\x80", b"\xf4\x90\x80\x80",
              b"\xe2\x82", b"\x80", b"title\nline", b"title\rline", b"title\tline", b"\x1b\\"]
    for number in (0, 2, 7):
        for end in (b"\x07", b"\x1b\\"):
            for index, value in enumerate(values):
                yield case(f"bytes/{number}/{end.hex()}/{index}", [initial, observe, osc(number, value, end), observe, query])
            for length in (1023, 1024, 1025, 2046, 2047, 2048, 2049, 4095, 4096, 4097):
                # Different ends expose accidental retention of a trailing slice.
                body = b"a" * (length // 2) + b"b" * (length - length // 2)
                yield case(f"length/{number}/{end.hex()}/{length}", [initial, osc(number, body, end), observe, query])
            for index, body in enumerate((b"a" * 1023 + "é".encode(), b"a" * 1022 + "界".encode(),
                                          b"a" * 1021 + "😀".encode(), b"a" * 1024 + b"\xff",
                                          b"a" * 2046 + b"\xff", b"a" * 2046 + "é".encode())):
                yield case(f"utf8-boundary/{number}/{end.hex()}/{index}", [initial, osc(number, body, end), observe, query])
        for index, end in enumerate((b"\x18", b"\x1a", b"\x1b[0m", b"\x1bX", b"\x1b]2;replacement\x07")):
            yield case(f"termination/{number}/{index}", [initial, osc(number, b"pending", end), observe, query])
        for index, body in enumerate((b"one\x00two", b"one\x05two", b"one\x07two", b"one\x08two", b"one\x09two",
                                      b"one\x0atwo", b"one\x0btwo", b"one\x0ctwo", b"one\x0dtwo", b"one\x1ftwo",
                                      b"one\x7ftwo", b"one\x9ctwo", "one—‐two".encode())):
            yield case(f"controls/{number}/{index}", [initial, osc(number, body), observe, query])

    for target in ("title", "pwd"):
        for index, value in enumerate(values + [b"x" * 2048, b"x" * 4097, b"x" * 8192]):
            setter = {"op": target + "_set", "data": value.hex()}
            yield case(f"setter/{target}/{index}", [setter, observe, query, setter, observe,
                       {"op": "terminal_reset"}, observe, query, setter, b"\x1bc", observe, query])
        for index, value in enumerate((b"", b"raw\xff\x00value", b"a" * 1023 + b"\xc3", b"x" * 4097)):
            yield case(f"snapshot/{target}/{index}", [{"op": target + "_set", "data": value.hex()}],
                       kind="snapshot", after=[{"op": "write", "data": query.hex()}])

    # Valid commands make a malformed numeric prefix observable: a permissive
    # integer parser must not turn an unsupported OSC into a supported action.
    commands = {0: b"changed", 1: b"icon", 2: b"changed", 7: b"file:///changed",
                8: b"id=id;https://example.org", 9: b"notification", 52: b"c;aGVsbG8=",
                133: b"A", 777: b"notify;title;body", 1337: b"CurrentDir=/changed", 5522: b"type=read;"}
    for number, body in commands.items():
        text = str(number).encode()
        prefixes = [text, b"0" + text, b"+" + text, b"-" + text, b" " + text, text + b" ",
                    text + b"_", text[:1] + b"_" + text[1:], b"0x" + format(number, "x").encode()]
        for index, prefix in enumerate(prefixes):
            yield case(f"prefix/{number}/{index}", [initial, b"\x1b]" + prefix + b";" + body + b"\x07",
                       b"X", observe, query])
        yield case(f"prefix/{number}/no-separator", [initial, b"\x1b]" + text + b"\x07", b"X", observe, query])

    for number, prefix in ((9, b""), (777, b"notify;title;"), (8, b"id=id;https://example.org/")):
        for length in (2046, 2047, 2048, 2049, 4096):
            yield case(f"fixed-capture/{number}/{length}", [osc(number, prefix + b"x" * (length - len(prefix))),
                       b"X", observe])
    for length in (1024, 2047, 2048, 2049, 4096):
        yield case(f"allocating-capture/52/{length}", [osc(52, b"c;" + base64.b64encode(b"x" * length))])

    for number, prefix in ((9, b"9;"), (1337, b"CurrentDir=")):
        for end in (b"\x07", b"\x1b\\"):
            for index, value in enumerate(values):
                yield case(f"pwd-alias/{number}/{end.hex()}/{index}", [initial, osc(number, prefix + value, end), observe])
            for length in (2046, 2047, 2048, 2049):
                yield case(f"pwd-alias-limit/{number}/{end.hex()}/{length}", [initial,
                           osc(number, prefix + b"x" * (length - len(prefix)), end), observe])
    for index, body in enumerate((b"CurrentDir", b"CurrentDir=", b"currentdir=/lower", b"CURRENTDIR=/upper",
                                  b" CurrentDir=/space", b"CurrentDir =/space", b"CurrentDir= /space ",
                                  b"CurrentDir=/with=equals;and;semicolons", b"Unknown=/ignored")):
        yield case(f"pwd-alias-key/{index}", [initial, osc(1337, body), observe])
    for index, body in enumerate((b"9", b"9;", b"9;path;extra", b"9x;path", b"09;path", b"+9;path", b" 9;path")):
        yield case(f"pwd-alias-key/9/{index}", [initial, osc(9, body), observe])

    for head in [str(number).encode() for number in range(14)] + [b"120", b"50"]:
        for index, suffix in enumerate((b"", b";", b";0", b";1", b";3", b";4", b";invalid", b"x", b";raw\xff")):
            yield case(f"conemu/{head.decode()}/{index}", [initial, b"output", osc(9, head + suffix), b"X", observe])
    for body in (b"5", b"12", b"4;0", b"10;1", b"9;", b"11;"):
        for length in (2047, 2048, 2049):
            yield case(f"conemu-capture/{body.hex()}/{length}", [initial, b"output",
                       osc(9, body + b"x" * (length - len(body))), b"X", observe])
    for index, setup in enumerate((b"", b"output", b"a\r\nb\r\nc\r\nlast", b"abcdefghijkl", b"\x1b[2;4r\x1b[4;5H")):
        yield case(f"conemu-fresh-prompt/{index}", [setup, osc(9, b"12"), observe, b"X", observe])

    for number in (2, 7):
        for length in (1023, 1024, 2046, 2047, 2048, 2049):
            prefix = b"\x1b]" + str(number).encode() + b";" + b"x" * length
            yield case(f"snapshot/capture/{number}/{length}", [initial, prefix], kind="snapshot",
                       after=[{"op": "write", "data": (b"\x07" + query).hex()}])
