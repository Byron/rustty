"""Terminal-level protocol requests; source names come from the Zig reference."""
import re
import base64


def requests(root):
    def write(data):
        return {"op": "write", "data": data.hex()}

    def case(name, data):
        return ({"id": "protocol/dcs/" + name, "cols": 12, "rows": 4,
                 "operations": [write(data)]}, ["protocol.dcs", "effects.pty"])

    source = (root / "src/terminfo/ghostty.zig").read_text()
    names = re.findall(r'\.name\s*=\s*"([^"\\]+)"', source)
    if len(names) < 250 or len(names) != len(set(names)):
        raise RuntimeError("reference terminfo capability extraction is incomplete")
    for name in names + ["Co", "RGB", "TN", "rustty-unknown"]:
        key = name.encode().hex().encode()
        yield case("capability/" + name,
                   b"\x1bP+q" + key.upper() + b"\x1b\\\x1bP+q" + key.lower() + b"\x1b\\")

    for index, keys in enumerate((
        b"", b"WHO;5;GG", b";616d;;436f;", b"616d;436f;524742;544e;dead",
        b"616D;436F;ff;00;0x42", b"616d\x18tail", b"616d\x1atail",
    )):
        yield case(f"capability/invalid-or-multiple/{index}", b"\x1bP+q" + keys + b"\x1b\\")

    styles = [b"0", b"1", b"2", b"3", b"4", b"4:2", b"4:3", b"4:4", b"4:5", b"5",
              b"7", b"8", b"9", b"53", b"31;42;58;5;123", b"90;107",
              b"38;5;250;48;5;123", b"38:2::1:2:3;48:2::4:5:6;58:2::7:8:9",
              b"1;2;3;4:3;5;7;8;9;53;38;2;11;22;33;48;5;88;58;5;123"]
    for index, style in enumerate(styles):
        yield case(f"decrqss/style/{index}", b"\x1b[" + style + b"m\x1bP$qm\x1b\\")
    for shape in range(7):
        yield case(f"decrqss/cursor/{shape}", f"\x1b[{shape} q\x1bP$q q\x1b\\".encode())
    for enabled in (False, True):
        prefix = b"\x1b[2;4r" + (b"\x1b[?69h\x1b[3;10s" if enabled else b"")
        yield case(f"decrqss/margins/{enabled}", prefix + b"\x1bP$qr\x1b\\\x1bP$qs\x1b\\")
    for index, query in enumerate((b"", b"z", b'"p', b'"q', b"q", b"foo", b"   ", b"x" * 1000)):
        yield case(f"decrqss/unknown/{index}", b"\x1bP$q" + query + b"\x1b\\\x1bP$qm\x1b\\")
    for index, data in enumerate((
        b"\x1bP1$qm\x1b\\", b"\x1bP$pignored\x1b\\", b"\x1bP$qm\x9c",
        b"\x1bP$qm\x18tail", b"\x1bP$qm\x1atail", b"\x1bP$qm\x07\x1b\\",
    )):
        yield case(f"decrqss/context/{index}", data)


def effect_requests():
    def case(name, payload, terminator=b"\x07"):
        return ({"id": "protocol/effects/" + name, "operations": [
            {"op": "write", "data": (b"\x1b]" + payload + terminator).hex()},
        ]}, ["effects.host"])

    for index, payload in enumerate((
        b"9;message", "9;héllo 界".encode(), b"9;", b"9;4", b"9;4;",
        b"777;notify;title;body", b"777;notify;title", b"777;notify;;body",
        b"777;notify;title;body;tail", b"777;other;title;body", b"9;raw\xff\xfe",
        b"777;notify;;", b"777;notify;raw\xff;raw\xfe", b"777;Notify;title;body",
    )):
        for ending, terminator in (("bel", b"\x07"), ("st", b"\x1b\\"), ("c1", b"\x9c")):
            yield case(f"notification/{index}/{ending}", payload, terminator)

    for state in (b"0", b"1", b"2", b"3", b"4", b"5", b"-1", b"256", b"abc", b"", b"01", b"+1", b" 1"):
        for index, value in enumerate((b"", b";0", b";5", b";100", b";101", b";255", b";256", b";-1", b";abc", b";50;extra",
                                       b";01", b";+1", b"; 1", b";18446744073709551616", b";1_0",
                                       b";_10", b";10_", b";1__0", b";_", b";0x10", b";-0", b";18446744073709551615")):
            yield case(f"progress/{state.decode()}/{index}", b"9;4;" + state + value)


def clipboard_requests():
    def content(mime, data):
        return {"mime": mime.hex(), "data": data.hex()}

    def case(name, data, replies=()):
        return ({"id": "protocol/clipboard/" + name,
                 "operations": [{"op": "write", "data": data.hex()}],
                 "clipboard_replies": list(replies)}, ["clipboard", "effects.pty"])

    endings = (("bel", b"\x07"), ("st", b"\x1b\\"), ("c1", b"\x9c"))
    selectors = (b"", b"c", b"p", b"s", b"x", b"0", b"cp", b"pc", b"\xff")
    payloads = [b"", b"Zg==", b"Zg", b"Zm8=", b"Zm8", b"Zm9v", b"AAH/",
                b"Zg===", b"Z g==", b"Zh==", b"Z", b"Zg=", b"=g==", b"Zh",
                b"Zg==\n", b";", base64.b64encode(bytes(range(256))),
                base64.b64encode(b"a" * 16384)]
    for selector in selectors:
        for index, payload in enumerate(payloads):
            for ending, terminator in endings:
                yield case(f"write/{selector.hex()}/{index}/{ending}",
                           b"\x1b]52;" + selector + b";" + payload + terminator)

    for selector in selectors:
        for status in ("success", "denied", "unsupported", "busy", "io_error", "none"):
            for ending, terminator in endings:
                yield case(f"read/{selector.hex()}/{status}/{ending}",
                           b"\x1b]52;" + selector + b";?" + terminator,
                           [{"status": status, "contents": [content(b"text/plain", b"a\x00b\xff")]}])

    for status in ("success", "denied", "unsupported", "busy", "invalid_data", "io_error", "none"):
        yield case("write-status/" + status, b"\x1b]52;c;dGV4dA==\x07", [{"status": status}])

    representations = [
        [], [content(b"image/png", b"\x89PNG")], [content(b"text/plain", b"")],
        [content(b"text/plain", b"first"), content(b"text/plain", b"second")],
        [content(b"image/png", b"\x89PNG"), content(b"UTF8_STRING", b"hi")],
        [content(b"TEXT/PLAIN", b"wrong case")],
        [content(b"text/plain", bytes(range(256)) * 64)],
    ] + [[content(mime, "héllo 界".encode())] for mime in
         (b"text/plain", b"text/plain;charset=utf-8", b"UTF8_STRING", b"TEXT", b"STRING")]
    for index, contents in enumerate(representations):
        yield case(f"read-mimes/{index}", b"\x1b]52;c;?\x07", [{
            "contents": contents, "available": [b"image/png".hex()], "remember": True,
        }])

    for index, payload in enumerate((b"52;", b"52;c", b"52;c?", b"52;cp;?",
                                     b"52;;??", b"52;;;?", b"52;c;?tail")):
        yield case(f"invalid/{index}", b"\x1b]" + payload + b"\x07")

    yield case("ordering", b"\x1b]52;c;?\x07\x1b[6n\x1b]52;p;dGV4dA==\x07"
               b"\x1b]52;s;?\x1b\\\x1b]9;notice\x07\x1bc", [
                   {"contents": [content(b"text/plain", b"first")]},
                   {"status": "denied"},
                   {"contents": [content(b"text/plain", b"last")]},
               ])
