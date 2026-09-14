"""Replay direct OSC parser inputs, preserving selector and raw payload bytes."""


def request(name, data):
    return ({"id": name, "cols": 12, "rows": 4,
             "observe_semantic": True, "observe_colors": True,
             "operations": [{"op": "write", "data": b"before".hex()},
                            {"op": "osc", "data": data.hex()},
                            {"op": "observe"},
                            {"op": "write", "data": b"after".hex()}]},
            ["parser.events", "effects.host", "effects.pty", "terminal.cells", "clipboard"])


def corpus_requests(root):
    for directory in ("osc-initial", "osc-cmin"):
        paths = sorted((root / "test/fuzz-libghostty/corpus" / directory).glob("*"))
        if not paths:
            raise RuntimeError(f"required corpus is missing: {directory}")
        for path in paths:
            if path.is_file():
                yield request(f"osc/corpus/{directory}/{path.name}", path.read_bytes())


def requests():
    # Control bytes remain data here, even BEL, CAN, SUB, ESC and C1 ST.
    for selector in (0, 1, 2, 255):
        for number in (2, 7, 9, 52, 133):
            for index, data in enumerate((b"", b"text", b";?", b"c;?",
                                          b"c;SGVsbG8=", b"A", b"D;0",
                                          b"a\x00b\x07c\x18d\x1ae\x1b]2;f\x9cg")):
                payload = bytes([selector]) + str(number).encode() + b";" + data
                yield request(f"osc/direct/{number}/{selector}/{index}", payload)
    for number in (2, 7, 9, 133):
        for length in (2047, 2048, 2049):
            data = b"A;" + b"x" * (length - 2) if number == 133 else b"x" * length
            yield request(f"osc/direct/boundary/{number}/{length}",
                          b"\x00" + str(number).encode() + b";" + data)
    for selector in (0, 1, 2):
        case, covers = request(f"osc/direct/pending-stream/{selector}",
                               bytes([selector]) + b"7;a\x00\x07\x18\x1a\x1b]2;raw\x9cb")
        case["operations"][0]["data"] = b"before\x1b[3".hex()
        case["operations"][-1]["data"] = b"1mafter".hex()
        yield case, covers
    yield request("osc/direct/empty-input", b"")
