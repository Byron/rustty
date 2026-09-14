"""Direct full-screen/terminal export bytes and optional native state replay."""
import itertools
from pathlib import Path

import snapshots
import selection_format_requests
from selection_format_requests import grid

SCREEN_FLAGS = ("cursor", "style", "hyperlink", "protection", "kitty_keyboard", "charsets")
TERMINAL_FLAGS = ("palette", "modes", "scrolling_region", "tabstops", "pwd", "keyboard")


def screen_extra(enabled=()):
    return {key: key in enabled for key in SCREEN_FLAGS}


def terminal_extra(enabled=(), screen=()):
    return {**{key: key in enabled for key in TERMINAL_FLAGS}, "screen": screen_extra(screen)}


def export(scope, emit="vt", content="all", **options):
    return grid("format_" + scope, format={"emit": emit}, format_content=content, **options)


def formats(content="all"):
    for scope, emit, unwrap, trim in itertools.product(("screen", "terminal"), ("plain", "vt", "html"), (False, True), (False, True)):
        yield grid("format_" + scope, format={"emit": emit, "unwrap": unwrap, "trim": trim}, format_content=content)


def case(name, operations, cols=8, rows=4):
    return ({"id": "grid/terminal-format/" + name, "kind": "input", "cols": cols, "rows": rows,
             "operations": [op if isinstance(op, dict) else snapshots.write(op) for op in operations]},
            ["terminal.selection"])


def requests():
    yield from option_requests()
    texts = {
        "empty": b"", "spaces": b"   a   \r\n \r\n\tb\r\n",
        "wrap": b"abcdefghijklmnopqrstuvwxyz0123456789",
        "wide": "abcdefg界🙂e\u0301\r\n界<>é".encode(),
        "styles": b"\x1b[1;2;3;5;7;8;9;53;4:3;38;2;1;2;3;48;5;200;58;2;9;8;7mA\x1b[0mB",
        "blank-background": b"\x1b[44m\x1b[2J\x1b[2;3HZ\x1b[3;1H\x1b[0mA",
        "links": b'\x1b]8;id=a;https://x/"<&\xff\x07ab\x1b]8;;\x07 c',
        "palette": b"\x1b]4;1;#aabbcc\x07\x1b[31mRed",
        "history": b"line\r\n" * 25,
    }
    for name, data in texts.items():
        yield case("contents/" + name, [data, *formats(),
                   grid("select", start={"x": 1}, end={"x": 6, "y": 2}), *formats("selection")])
    yield case("defaults", [b"abcdefghijk", grid("format_screen"), grid("format_terminal"),
                export("screen", "plain"), export("terminal", "plain"),
                *formats("none"), *formats("selection")])

    state = (b"\x1b[?1h\x1b[?7l\x1b[4h\x1b[20h\x1b[?69h\x1b[2;7s\x1b[2;3r"
             b"\x1b[3g\x1b[2G\x1bH\x1b[6G\x1bH\x1b[>4;2m\x1b]7;file://host/\xffpath\x07"
             b"\x1b[=21;1u\x1b(0\x1b)A\x1b*B\x1b+0\x1bn\x1b|"
             b'\x1b[1"q\x1b]8;id=cursor;uri\xff\x07\x1b[1;31m\x1b[2;4H')
    for scope in ("screen", "terminal"):
        flags = SCREEN_FLAGS if scope == "screen" else TERMINAL_FLAGS
        choices = [(), flags, *(tuple([flag]) for flag in flags)]
        for enabled in choices:
            extra = screen_extra(enabled) if scope == "screen" else terminal_extra(enabled, SCREEN_FLAGS if enabled == flags else ())
            yield case(f"extras/{scope}/{'-'.join(enabled) or 'none'}", [state, *[
                export(scope, emit, content, **{scope + "_extra": extra})
                for emit, content in itertools.product(("plain", "vt", "html"), ("all", "none"))]])

    for data in (b"abcd", b"abc ", "ab界".encode(), "abe\u0301x".encode(),
                 b"\x1b[31mabcd\x1b[32m", b'\x1b[1"qabcd\x1b[0"q',
                 b"\x1b]8;id=old;old\x07abcd\x1b]8;id=new;new\x07"):
        yield case("pending-wrap/" + data.hex(), [data, *[
            export(scope, content=content, **{scope + "_extra":
                screen_extra(SCREEN_FLAGS) if scope == "screen" else terminal_extra(TERMINAL_FLAGS, SCREEN_FLAGS)})
            for scope, content in itertools.product(("screen", "terminal"), ("all", "none"))]], cols=4)
    for gl, gr in itertools.product((b"", b"\x0e", b"\x1bn", b"\x1bo"), (b"", b"\x1b~", b"\x1b}", b"\x1b|")):
        yield case(f"charsets/{gl.hex()}/{gr.hex()}", [b"\x1b(0\x1b)A\x1b*B\x1b+0" + gl + gr,
                    export("screen", content="none", screen_extra=screen_extra(["charsets"])), b"\x1bc",
                    export("screen", content="none", screen_extra=screen_extra(["charsets"]))])
    for number, private in ((2, False), (12, False), (7, True), (25, True), (1000, True), (1049, True), (5522, True)):
        tag = {"number": number, "private": private}
        output = export("terminal", content="none", terminal_extra=terminal_extra(["modes"]))
        yield case(f"mode-default/{private}/{number}", [{"op": "mode_raw_default", **tag, "value": True}, output,
                    {"op": "mode_set", **tag, "value": False}, output, {"op": "modes_reset"}, output])

    for alternate in (False, True):
        prefix = b"\x1b[?1049h" if alternate else b""
        for name, data in (("hard", b"\x1b[31mword   \r\n" * 90),
                           ("soft", b"\x1b[31m" + b"a" * 1023 + b" " * (1024 * 30) + b"\x1b[32mz"),
                           ("wide", ("a" * 1023 + "界").encode() * 50),
                           ("blank", b"first\r\n" + b"\r\n" * 80 + b"last")):
            yield case(f"pages/{alternate}/{name}", [prefix + data, *formats()], cols=1024, rows=3)
    data = snapshots.fixture(Path(__file__).resolve().parents[2] / "src/terminal/snapshot/testdata/complete-v1.hex").hex()
    yield case("restored", [{"op": "restore", "data": data}, *formats(),
                export("terminal", terminal_extra=terminal_extra(TERMINAL_FLAGS, SCREEN_FLAGS))])


def option_formats(**options):
    for scope, emit, unwrap, trim in itertools.product(
            ("screen", "terminal", "selection"), ("plain", "vt", "html"), (False, True), (False, True)):
        yield grid("format_" + scope, format={"emit": emit, "unwrap": unwrap, "trim": trim, **options})


def option_requests():
    select = grid("select", start={"tag": "screen", "x": 0, "y": 0}, end={"x": 7, "y": 3})
    palette = [[i, 255 - i, (i * 37) % 256] for i in range(256)]
    styled = b"\x1b[31;44;58;5;2mA \x1b[38;2;1;2;3mB\x1b[0m\r\n\x1b[44m\x1b[2K\x1b[5GZ"
    for foreground, background, resolved in itertools.product((None, [1, 23, 255]), (None, [255, 9, 2]), (False, True)):
        options = {"foreground": foreground, "background": background, "palette": palette if resolved else None}
        name = f"options/colors/{foreground}/{background}/{resolved}"
        yield case(name, [styled, select, *option_formats(**options)])
    yield case("options/colors/empty", [*option_formats(foreground=[1, 2, 3], background=[4, 5, 6], palette=palette)])
    yield case("options/colors/pages", [b"\x1b[31;44mword\r\n" * 90,
               *option_formats(foreground=[1, 2, 3], background=[4, 5, 6], palette=palette)], cols=1024, rows=3)

    def rule(first, last, replacement):
        return {"range": [ord(first), ord(last)], "replacement": replacement}

    maps = {
        "empty": [],
        "latin": [rule("a", "z", {"codepoint": ord("X")})],
        "overlap": [rule("a", "z", {"codepoint": ord("X")}), rule("b", "q", {"string": '<&é"\''})],
        "unicode": [rule("\0", "\U0010ffff", {"codepoint": ord("🙂")})],
        "grapheme": [rule("\u0301", "\u0301", {"string": "<combining>"})],
        "empty-string": [rule("a", "z", {"string": ""})],
        "spaces": [rule(" ", " ", {"string": "<space>"})],
        "unwritten": [rule("\0", "\0", {"string": "<empty>"})],
        "inverted": [rule("z", "a", {"string": "unused"})],
        "control": [rule("b", "b", {"string": "\0\t\n\x1b\x07"})],
        "nul": [rule("a", "a", {"codepoint": 0})],
        "long": [rule("b", "b", {"string": "界" * 1025})],
    }
    text = "  abc界 e\u0301q\r\n\x1b[31;44;58;5;2ma b\x1b[0m\r\n\x1b[44m\x1b[2K\x1b[6Gz".encode()
    for name, mapping in maps.items():
        yield case("options/map/" + name, [text, select, *option_formats(codepoint_map=mapping)])
    mapping = [rule("a", "a", {"string": "<&>"})]
    yield case("options/map/pages", [("a" * 1023 + "界").encode() * 35,
               *option_formats(codepoint_map=mapping)], cols=1024, rows=3)
    yield case("options/pending-wrap", [b"\x1b[31mabcx\x1b[32m", *[
        grid("format_" + scope, format={"emit": emit, "foreground": [1, 2, 3], "background": [4, 5, 6],
             "palette": palette, "codepoint_map": [rule("x", "x", {"string": "<&"})]},
             format_content=content, **{scope + "_extra": screen_extra(SCREEN_FLAGS) if scope == "screen"
                                      else terminal_extra(TERMINAL_FLAGS, SCREEN_FLAGS)})
        for scope, emit, content in itertools.product(("screen", "terminal"), ("plain", "vt", "html"), ("all", "none"))]], cols=4)
    for size in (0, 1, 255, 257):
        request, covers = case(f"options/invalid/palette-{size}", [grid("format_screen", format={"palette": [[0, 0, 0]] * size})])
        request["expected_error"] = "InvalidFormatPalette"
        yield request, covers
    for value in (0xd800, 0xdfff, 0x110000, 0xffffffff):
        for field in ("range", "replacement"):
            invalid = rule("a", "z", {"codepoint": ord("X")})
            if field == "range":
                invalid["range"][1] = value
            else:
                invalid["replacement"]["codepoint"] = value
            request, covers = case(f"options/invalid/{field}-{value}", [grid("format_screen", format={"codepoint_map": [invalid]})])
            request["expected_error"] = "InvalidCodepoint"
            yield request, covers


def snapshot_requests(reference):
    for request, covers in selection_format_requests.snapshot_requests(reference):
        setup = [op for op in request["operations"]
                 if op["op"] != "grid" or op["grid"]["action"] in ("select", "viewport")]
        yield ({**request, "id": request["id"].replace("selection-format/restored", "terminal-format/restored"),
                "operations": [*setup, *formats(),
                    export("screen", screen_extra=screen_extra(SCREEN_FLAGS)),
                    export("terminal", terminal_extra=terminal_extra(TERMINAL_FLAGS, SCREEN_FLAGS))]}, covers)


if __name__ == "__main__":
    import json
    import sys
    json.dump([{"request": request, "covers": covers} for request, covers in requests()], sys.stdout, indent=2)
    sys.stdout.write("\n")
