"""Legacy character-set mapping at cell writes, including single shifts."""


def requests():
    def case(name, data, after=None):
        request = {"id": "protocol/charsets/" + name, "cols": 8, "rows": 3,
                   "operations": [{"op": "write", "data": data.hex()}]}
        if after is not None:
            request.update(kind="snapshot", after=[{"op": "write", "data": after.hex()}])
        return request, ["terminal.cells", "terminal.cursor"]

    for charset in (b"B", b"A", b"0"):
        for slot, invoke in ((b"(", b""), (b")", b"\x0e"),
                             (b"*", b"\x1bn"), (b"+", b"\x1bo")):
            setup = b"\x1b" + slot + charset + invoke
            for payload in (b"#q`~", "Ā".encode(), b"\xbb", "a\u0301q".encode()):
                name = f"mapping/{charset.hex()}/{slot.hex()}/{payload.hex()}"
                yield case(name, setup + payload)
                yield case("repeat/" + name, setup + payload + b"\x1b(B\x0f\x1b[2b")

        # Single shifts are consumed by cell writes. Combining scalars and
        # wide-character spacer writes distinguish this from parser dispatch.
        for slot, invoke in ((b"*", b"\x1bN"), (b"+", b"\x1bO")):
            setup = b"\x1b" + slot + charset
            for initial in (b"", b"1234567", "123456#\u0301".encode()):
                for payload in ("界#q".encode(), "\u0301#q".encode()):
                    for grapheme in (False, True):
                        mode = b"\x1b[?2027h" if grapheme else b"\x1b[?2027l"
                        name = f"single/{charset.hex()}/{slot.hex()}/{initial.hex()}/{payload.hex()}/{int(grapheme)}"
                        yield case(name, mode + setup + initial + invoke + payload)
            yield case(f"snapshot/single/{charset.hex()}/{slot.hex()}", setup + invoke,
                       after="\u0301#q".encode())
            yield case(f"snapshot/repeat/{charset.hex()}/{slot.hex()}", setup + invoke + b"q",
                       after=b"\x1b[2b")
