"""Native mode bits, host reset policy and stream transition effects."""
import re


def requests(root):
    source = (root / "src/terminal/modes.zig").read_text()
    source = source.split("const entries: []const ModeEntry = &.{", 1)[1].split("\n};", 1)[0]
    entries = []
    for body in re.findall(r"\.\{(.*?)\}", source, re.S):
        name = re.search(r'\.name = "([^"]+)"', body)
        number = re.search(r"\.value = (\d+)", body)
        if name and number:
            entries.append((name[1], {"number": int(number[1]), "private": ".ansi = true" not in body}))
    if not entries:
        raise RuntimeError("no native terminal modes found")
    tags = [tag for _, tag in entries]

    def case(name, operations, effects=False, **options):
        return ({"id": "protocol/modes/" + name, "observe_modes": tags,
                 "observe_mode_effects": effects,
                 "operations": [op if isinstance(op, dict) else {"op": "write", "data": op.hex()} for op in operations],
                 **options}, ["terminal.modes", "terminal.reset", "terminal.cursor", "effects.pty"])

    def api(action, tag, value=False):
        return {"op": "mode_" + action, **tag, "value": value}

    def sequence(tag, suffix):
        return f"\x1b[{'?' if tag['private'] else ''}{tag['number']}{suffix}".encode()

    observe = {"op": "observe"}
    yield case("initial", [observe], True)
    for name, tag in entries:
        yield case("raw/" + name, [
            observe, api("restore", tag), observe,
            api("set", tag, True), api("save", tag), api("set", tag, False), observe,
            api("restore", tag), observe, api("restore", tag), observe,
            api("raw_default", tag, False), observe, api("restore", tag), observe,
            {"op": "modes_reset"}, observe, api("restore", tag), observe,
            api("raw_default", tag, True), api("set", tag, False), observe,
            {"op": "modes_reset"}, observe, api("restore", tag), observe,
            api("save", tag), api("set", tag, True), api("save", tag),
            api("set", tag, False), api("restore", tag), observe,
        ])
        for value in (False, True):
            yield case(f"host-default/{name}/{value}", [
                api("default", tag, value), observe,
                sequence(tag, "l" if value else "h"), observe,
                {"op": "terminal_reset"}, observe,
                sequence(tag, "l" if value else "h"), observe,
                b"\x1bc", observe,
            ], True)
        operations = [observe, sequence(tag, "$p"), sequence(tag, "h"), observe,
                      sequence(tag, "$p"), sequence(tag, "l"), observe, sequence(tag, "$p")]
        if tag["private"]:
            operations += [sequence(tag, "r"), observe, sequence(tag, "h"), sequence(tag, "s"),
                           sequence(tag, "l"), observe, sequence(tag, "r"), observe,
                           sequence(tag, "r"), observe, sequence(tag, "$p")]
        yield case("stream/" + name, operations, True)

    for private in (False, True):
        for number in (0, 14, 117, 32767, 32768, 32775, 32885, 65535):
            tag = {"number": number, "private": private}
            yield case(f"unknown/{private}/{number}", [observe] + [
                operation for action in ("set", "raw_default", "default", "save", "restore")
                for operation in (api(action, tag, True), observe)
            ] + [sequence(tag, "$p"), sequence(tag, "h"), sequence(tag, "$p")],
                observe_modes=[tag])

    for index, data in enumerate((
        b"\x1b[?1;7$p", b"\x1b[?1:7$p", b"\x1b[?$p", b"\x1b[?0$p", b"\x1b[?65536$p",
        b"\x1b[?32775$p\x1b[?32885$p", b"\x1b[?117h\x1b[?117$p\x1b[?117l\x1b[?117$p",
        b"\x1b[?1;7h\x1b[?1;7s\x1b[?1;7l\x1b[?1;7r", b"\x1b[?1:7h",
        b"\x1b[?7:25l", b"\x1b[?1h\x1b[?1:7s\x1b[?1l\x1b[?1r",
        b"\x1b[?1h\x1b[?1s\x1b[?1l\x1b[?1:7r", b"\x1b[?1;7:25h",
        b"\x1b[1h\x1b[?4h\x1b[4h", b"\x1b[>1h\x1b[=1h\x1b[<1h",
    )):
        yield case(f"context/{index}", [data], True)
    for enabled in (False, True):
        yield case(f"clipboard-host/{enabled}", [b"\x1b[?5522$p\x1b[?5522h\x1b[?5522$p\x1b[?5522l\x1b[?5522$p"],
                   True, clipboard_read_enabled=enabled)

    for family in ((9, 1000, 1002, 1003), (1005, 1006, 1015, 1016)):
        for first in family:
            for second in family:
                a, b = ({"number": number, "private": True} for number in (first, second))
                yield case(f"mouse/{first}/{second}", [sequence(a, "h"), observe,
                           sequence(b, "h"), observe, sequence(a, "l"), observe,
                           sequence(b, "h"), sequence(b, "s"), sequence(b, "l"),
                           sequence(b, "r"), observe, b"\x1bc", observe], True)

    for index, commands in enumerate((
        [b"\x1b[?3h", b"\x1b[?3l"],
        [b"\x1b[?40h\x1b[?3h", b"\x1b[?3l"],
        [b"\x1b[?40h\x1b[?3h", b"\x1b[?40l\x1b[?3h"],
        [b"\x1b[?40h\x1b[?3h\x1b[?3s", b"\x1b[?40l\x1b[?3l\x1b[?3r"],
    )):
        yield case(f"column-mode/{index}", [b"abc\x1b[2;4H\x1b[31m"] +
                   [op for data in commands for op in (data, observe)], True)

    for index, reset in enumerate((b"\x1b[!p", b"\x1b[0!p", b"\x1b[1!p", b"\x1bc", {"op": "terminal_reset"})):
        yield case(f"reset/{index}", [b"\x1b[?25l\x1b[?12h\x1b[4h\x1b[?1h\x1b[?1s\x1b[?1l"
                   b"\x1b[2;4r\x1b[?69h\x1b[2;9s\x1b[?6h\x1b[31mAB\x1b7\x1b[5 q", observe,
                   reset, observe, b"\x1b[?1r\x1b8", observe], True)

    for screen_mode in (47, 1047, 1049):
        tag = {"number": screen_mode, "private": True}
        yield case(f"screen-cursor/{screen_mode}", [
            b"\x1b[?25l\x1b[?12h\x1b[5 q", observe, sequence(tag, "h"), observe,
            b"\x1b[?25h\x1b[2 q", observe, sequence(tag, "l"), observe,
            sequence(tag, "h"), observe, b"\x1b[?25l", sequence(tag, "l"), observe,
            {"op": "terminal_reset"}, observe,
        ], True)
    yield case("origin-margins", [
        b"\x1b[2;4r\x1b[?69h\x1b[3;10s\x1b[3;6H", observe,
        b"\x1b[?6h", observe, b"\x1b[2;2H\x1b[?6h", observe,
        b"\x1b[?6s\x1b[?6l", observe, b"\x1b[?6r", observe,
        b"\x1b[?69l", observe, b"\x1b[?6l", observe,
    ], True)
    for dimensions in ((12, 4), (8, 3)):
        yield case(f"synchronized-resize/{dimensions}", [
            b"\x1b[?2026h", observe,
            {"op": "resize", "cols": dimensions[0], "rows": dimensions[1]}, observe,
            b"\x1b[?2026s\x1b[?2026h\x1b[?2026r", observe,
        ], True)

    for shape in ("block", "bar", "underline", "block_hollow"):
        for blink in (None, False, True):
            defaults = {"op": "cursor_defaults", "cursor_shape": shape, "cursor_blink": blink}
            for code in range(7):
                query = b"\x1bP$q q\x1b\\"
                yield case(f"cursor-default/{shape}/{blink}/{code}", [
                    defaults, observe, query, f"\x1b[{code} q".encode(), observe,
                    {"op": "cursor_defaults", "cursor_shape": "underline", "cursor_blink": False},
                    observe, query, b"\x1b[0 q", observe, query,
                    defaults, b"\x1b[?12l", observe, defaults, observe,
                    {"op": "terminal_reset"}, observe, query,
                    b"\x1b[?25l\x1b[6 q\x1bc", observe, query,
                ], True)
    for index, data in enumerate((b"\x1b[ q", b"\x1b[7 q", b"\x1b[65535 q", b"\x1b[1;2 q",
                                  b"\x1b[0;0 q", b"\x1b[1:2 q", b"\x1b[?1 q", b"\x1b[1q")):
        yield case(f"cursor-context/{index}", [b"\x1b[5 q", data, b"\x1bP$q q\x1b\\"], True)
