"""OSC 133 redraw policy applied by primary-screen resize."""


def requests():
    def prompt(body):
        return b"\x1b]133;" + body + b"\x07"

    patterns = {
        "single": b"p> " + prompt(b"B") + b"input",
        "styled": b"\x1b[31;44m\x1bV\x1b]8;;uri\x07p> " + prompt(b"B") + b"input",
        "multiline": b"first\r\n" + prompt(b"P;k=s") + b"second\r\n" + prompt(b"B") + b"input",
        "unmarked": prompt(b"C") + b"\x1b[2J" + prompt(b"B") + b"input",
        "output": b"p> " + prompt(b"C") + b"output",
        "history": b"first" + (b"\r\n" + prompt(b"P;k=s") + b"line") * 6 + prompt(b"B") + b"input",
        "orphan": b"\x1b[2J" + prompt(b"P;k=s") + b"first\r\n" + prompt(b"P;k=s") + b"second",
        "gap": b"p>\r\n" + prompt(b"C") + b"output\r\n" + prompt(b"B") + b"input",
        "below-cursor": b"p> " + prompt(b"B") + b"input\x1b[4;1Hbelow\x1b[1;3H",
    }
    covers = ["terminal.cells", "terminal.cursor", "terminal.resize"]
    for policy in (b"1", b"0", b"last"):
        for name, body in patterns.items():
            for screen in ("primary", "inactive-primary", "alternate"):
                setup = prompt(b"A;redraw=" + policy) + body
                if screen == "alternate":
                    setup = b"\x1b[?1049h" + setup
                elif screen == "inactive-primary":
                    setup += b"\x1b[?1049hALT"
                for cols, rows in ((12, 7), (18, 5), (6, 5)):
                    request = {
                        "id": f"protocol/prompt-redraw/{policy.decode()}/{name}/{screen}/{cols}-{rows}",
                        "cols": 12, "rows": 5, "observe_semantic": True,
                        "operations": [
                            {"op": "write", "data": setup.hex()}, {"op": "observe"},
                            {"op": "resize", "cols": cols, "rows": rows}, {"op": "observe"},
                        ],
                    }
                    yield request, covers
                    if name == "styled" and screen == "primary":
                        yield dict(request, id=request["id"] + "/snapshot", kind="snapshot",
                                   after=[{"op": "write", "data": b"Z".hex()}]), covers

    for policy in (b"1", b"0", b"last"):
        yield {
            "id": f"protocol/prompt-redraw/same-size/{policy.decode()}", "cols": 12, "rows": 5,
            "observe_semantic": True,
            "operations": [{"op": "write", "data": (prompt(b"A;redraw=" + policy) + b"prompt").hex()},
                           {"op": "resize", "cols": 12, "rows": 5}],
        }, covers

    # Minimized inherited streams exercise inactive primary cleanup and its
    # interaction with DECCOLM's subsequent display erasure.
    for name, data in (
        ("inactive", b"\x1b]133;P\x1a\\\x1b[?40h\x1b[?1049h\x1b[?3h"),
        ("history", b"\n]\n\x1b]133;A\x1bDe\x1b[?40h\x1b[?" + b"\x0b" * 19 + b"3h"),
    ):
        yield {"id": "protocol/prompt-redraw/corpus/" + name, "cols": 80, "rows": 24,
               "observe_semantic": True, "operations": [{"op": "write", "data": data.hex()}]}, covers
