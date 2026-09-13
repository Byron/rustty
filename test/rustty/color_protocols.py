"""Color parser, state, defaults and xterm/Kitty OSC replies."""


def requests(root):
    def case(name, operations):
        return ({"id": "protocol/colors/" + name, "observe_colors": True,
                 "operations": [op if isinstance(op, dict) else {"op": "write", "data": op.hex()} for op in operations]},
                ["terminal.colors", "effects.pty"])

    def osc(number, body=b"", end=b"\x07", separator=True):
        return b"\x1b]" + str(number).encode() + (b";" if separator else b"") + body + end

    def defaults(fg=None, bg=None, cursor=None, palette=None):
        return {"op": "color_defaults", "colors": {"foreground": fg, "background": bg,
                "cursor": cursor, "palette": palette}}

    def query(end=b"\x07"):
        return osc(10, b"?;?;?;?;?;?;?;?;?;?", end) + osc(21, b"foreground=?;background=?;cursor=?;selection_foreground=?", end)

    observe = {"op": "observe"}
    colors = []
    for line in (root / "src/terminal/res/rgb.txt").read_bytes().splitlines():
        if line.strip():
            name = line[12:].strip(b" \t")
            colors.extend((name, name.lower(), name.upper(), b" \t" + name + b"\t "))
    if not colors:
        raise RuntimeError("native X11 colors are missing")
    for index in range(0, len(colors), 128):
        yield ({"id": f"protocol/colors/parser/names/{index}", "kind": "colors",
                "color_inputs": [color.hex() for color in colors[index:index + 128]]}, ["terminal.colors"])

    forms = [b"", b"#", b"#1", b"#12", b"#123", b"123", b"#1234", b"#12345", b"#123456",
             b"123456", b"#123456789", b"123456789", b"#123456789abc", b"123456789abc", b"#" + b"f" * 13,
             b"RED", b"green", b"grey50", b"light blue", b"light  blue", b"bad", b"not-a-color", b"\xff", b"red\x00",
             b"rgb:f/0/8", b"rgb:ff/00/88", b"rgb:fff/000/888", b"rgb:ffff/0000/8888",
             b"rgb:f/00/888", b"rgb:/0/0", b"rgb:00000/0/0", b"rgb:0/0", b"rgb:0/0/0/0",
             b"RGB:f/0/8", b"rgb:+f/0/8", b"rgb:f_f/0/8", b"rgb:f__f/0/8", b"rgb:_f/0/8",
             b"rgb:f_/0/8", b"rgb:0xf/0/8", b"#f_f000000", b"#f__f00000000"]
    for value in (b"0", b"1", b".5", b"1.", b"+0.5", b"-0", b"-0.000", b"-0.1", b"1.0000001",
                  b"2", b"", b".", b"+", b"nan", b"inf", b"1e-1", b"0x0.8", b"0..5", b" 0.5", b"0.5 ",
                  b"0.123456789012345", b"0.123456789012345999", b"0." + b"3" * 400,
                  b"1." + b"0" * 15 + b"1", b"-0." + b"0" * 15 + b"1", b"1" * 400):
        forms.append(b"rgbi:" + value + b"/0.5/1")
    for byte in (b" ", b"\t", b"\n", b"\r", b"\v", b"\f", b"\x00", b"\xc2\xa0"):
        forms.extend((byte + b"#123456", b"#123456" + byte, byte + b"red" + byte))
    protocol_forms = tuple(forms)
    for value in range(256):
        forms.extend((f"#{value:02x}{255-value:02x}{value^0x55:02x}".encode(),
                      f"rgb:{value:02x}/{255-value:03x}/{value^0x55:04x}".encode()))
    for index in range(0, len(forms), 128):
        yield ({"id": f"protocol/colors/parser/forms/{index}", "kind": "colors",
                "color_inputs": [color.hex() for color in forms[index:index + 128]]}, ["terminal.colors"])

    palette = [[index, 255 - index, (index * 17) % 256] for index in range(256)]
    for end in (b"\x07", b"\x1b\\"):
        term = end.hex()
        yield case(f"initial/{term}", [query(end)])
        for start in range(0, 256, 16):
            entries = range(start, start + 16)
            queries = b";".join(f"{index};?".encode() for index in entries)
            sets = b";".join(f"{index};#{index:02x}{255-index:02x}{index^0x55:02x}".encode() for index in entries)
            resets = b";".join(str(index).encode() for index in entries)
            yield case(f"palette/{term}/{start}", [osc(4, queries, end), osc(4, sets, end), observe,
                       osc(4, queries, end), osc(104, resets, end), observe, osc(4, queries, end)])

        for number in range(10, 20):
            yield case(f"dynamic/{term}/{number}", [osc(number, b"?", end), osc(number, b"#123456", end),
                       observe, osc(number, b"?", end), query(end), osc(number + 100, end=end), observe, query(end)])
        for number in (4, 5):
            for index in (0, 1, 2, 3, 4, 5, 6, 7, 8, 255, 256, 257, 258, 259, 260, 261, 263, 264, 511, 512):
                body = f"{index};#123456;{index};?;1;#abcdef;1;?".encode()
                yield case(f"special/{term}/{number}/{index}", [osc(number, body, end)])

        keys = [str(index) for index in range(256)] + ["foreground", "background", "cursor", "selection_foreground",
                "selection_background", "cursor_text", "visual_bell", "second_transparent_background"]
        for start in range(0, len(keys), 16):
            batch = keys[start:start + 16]
            queries = ";".join(key + "=?" for key in batch).encode()
            sets = ";".join(key + "=#123456" for key in batch).encode()
            resets = ";".join(key + "=" for key in batch).encode()
            yield case(f"kitty/{term}/{start}", [osc(21, queries, end), osc(21, sets, end), observe,
                       osc(21, queries, end), osc(21, resets, end), observe, osc(21, queries, end)])

    for index, value in enumerate(protocol_forms):
        yield case(f"osc-form/{index}", [osc(10, value), osc(4, b"3;" + value), osc(21, b"4=" + value),
                   query(), osc(4, b"3;?;4;?")])

    for index, body in enumerate((b"", b";", b";;", b"0", b"0;", b"0;;red", b";0;red;;1;blue;",
                                  b"0;#123456;bad;red;1;blue", b"0;#123456;1;invalid;2;blue",
                                  b"0;?;bad;red;1;?", b"0;+red;1;blue", b"+1;red;1;?", b"-0;red;0;?",
                                  b"-1;red;1;?", b"0x1;red;1;?", b"1_0;red;10;?", b"1__0;red;10;?",
                                  b"_1;red;1;?", b"1_;red;1;?", b" 1;red;1;?", b"1 ;red;1;?")):
        yield case(f"palette-list/{index}", [osc(4, body), observe, osc(4, b"0;?;1;?;2;?;10;?")])
    for index, body in enumerate((b"", b";", b";;", b"?;?;?", b"#123456;;#abcdef", b"red;bad;blue",
                                  b"red;?;blue;?", b"?;invalid;?", b";red;blue", b"red;blue;green;invalid;red")):
        yield case(f"dynamic-list/{index}", [osc(10, body), observe, query()])

    for number in (104, 105, 110, 111, 112, 113, 119):
        for index, body in enumerate((b"", b";", b";;", b"bad", b"0", b"1;bad;2", b"256", b"260", b"512",
                                      b"+1", b"-0", b"1_0", b" ", b"\t")):
            setup = osc(4, b"0;red;1;blue;2;green;10;yellow") + osc(10, b"red;green;blue")
            yield case(f"reset-list/{number}/{index}", [setup, observe, osc(number, body), observe, query(), osc(4, b"0;?;1;?;2;?;10;?")])
        yield case(f"reset-no-separator/{number}", [osc(4, b"0;red") + osc(10, b"red;green;blue"),
                   osc(number, separator=False), observe, query(), osc(4, b"0;?")])

    for index, body in enumerate((b"", b";", b";1=red;;2=blue;", b"1", b"1=", b"1= ", b"1=\t", b"1= ? ",
                                  b"1=\t?\t", b"1=red=blue", b"1=invalid;2=red;2=?", b"unknown=red;2=blue;2=?",
                                  b"foreground=#123456;foreground=?;foreground;foreground=?",
                                  b"Foreground=red;foreground=?", b"+1=red;1=?", b"-0=red;0=?", b"0x1=red;1=?",
                                  b"1_0=red;10=?", b"1__0=red;10=?", b"_1=red;1=?", b"1_=red;1=?",
                                  b" 1=red;1=?", b"1 =red;1=?", b"255=red;256=blue;255=?", b"1=red;1=;1=?")):
        yield case(f"kitty-list/{index}", [osc(4, b"1;#abcdef"), osc(21, body), observe, query(), osc(4, b"0;?;1;?;2;?;10;?")])

    for index, configured in enumerate((defaults(), defaults([1, 2, 3]), defaults(bg=[4, 5, 6]),
                                        defaults(cursor=[7, 8, 9]), defaults([1, 2, 3], [4, 5, 6], [7, 8, 9], palette))):
        yield case(f"defaults/{index}", [configured, observe, query(), osc(4, b"0;?;255;?"),
                   osc(10, b"#abcdef;#fedcba;#123456") + osc(4, b"0;#abcdef;255;#123456"), observe,
                   defaults([11, 12, 13], [14, 15, 16], [17, 18, 19]), observe, query(),
                   b"\x1bc", observe, query(), {"op": "terminal_reset"}, observe, query(),
                   osc(110) + osc(111) + osc(112) + osc(104), observe, query(),
                   configured, observe, query(), defaults(), observe, query()])
    yield case("identical-overrides", [defaults([1, 2, 3], [4, 5, 6], [7, 8, 9], palette),
               osc(10, b"#010203;#040506;#070809") + osc(4, b"0;#00ff00;255;#ff00ef"), observe,
               defaults(), observe, query(), osc(4, b"0;?;255;?"), osc(104) + osc(110) + osc(111) + osc(112), observe])

    for number, prefix in ((4, b"0;#ff0000"), (10, b"#ff0000"), (21, b"0=#ff0000")):
        for length in (2047, 2048, 2049, 4096, 4097):
            yield case(f"capture/{number}/{length}", [osc(number, prefix + b" " * (length - len(prefix))), observe,
                       query(), osc(4, b"0;?")])
    for prefix in (b"04;0;red", b"+4;0;red", b" 4;0;red", b"4_;0;red", b"010;red",
                   b"021;foreground=red", b"0110", b"+110", b"110x", b"10", b"21"):
        yield case(f"prefix/{prefix.hex()}", [osc(10, b"#123456"), b"\x1b]" + prefix + b"\x07", observe,
                   query(), osc(4, b"0;?")])
    # Short reset keys reach the native 526-request limit while the complete
    # body still fits in its fixed OSC capture buffer.
    for count in (525, 526, 527, 528, 529):
        for trailing in (b"", b";", b";unknown=?"):
            yield case(f"kitty-limit/{count}/{trailing.hex()}", [osc(4, b"1;#abcdef"),
                       osc(21, b";".join([b"1"] * count) + trailing), observe, osc(4, b"1;?")])
