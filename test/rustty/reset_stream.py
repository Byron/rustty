"""Direct terminal reset retains independently owned native stream state."""
from glyph_requests import apc, register, write


def requests():
    contexts = {
        "utf8": "界".encode(),
        "escape-charset": b"\x1b(0q",
        "escape-save": b"\x1b7",
        "csi-sgr": b"\x1b[38:2::11:22:33mX",
        "csi-modes": b"\x1b[?7;25l",
        "osc-title": b"\x1b]2;retained title\x1b\\",
        "osc-color": b"\x1b]10;#112233\x07",
        "dcs-status": b"\x1bP$qm\x1b\\",
        "dcs-capability": b"\x1bP+q436f;544e\x1b\\",
        "dcs-overflow": b"\x1bP$qmore-than-two\x1b\\",
        "apc-kitty": b"\x1b_Ga=q,i=1,s=1,v=1,f=24;AAAA\x1b\\",
        "apc-glyph": apc(register()),
        "apc-unknown": b"\x1b_hello\x1b\\",
    }
    for name, data in contexts.items():
        for cut in range(1, len(data)):
            yield ({"id": f"protocol/reset-stream/{name}/{cut}", "cols": 8, "rows": 3,
                    "operations": [write(data[:cut]), {"op": "terminal_reset"},
                        write(data[cut:] + b"Z"), {"op": "glyph_observe"}]}, ["terminal.reset"])
    data = apc(b"s;ignored")
    for cut in range(2, 12):
        yield ({"id": f"protocol/reset-stream/limited-glyph/{cut}", "cols": 8, "rows": 3,
                "operations": [{"op": "glyph_limit", "glyph_max_bytes": 1}, write(data[:cut]),
                    {"op": "terminal_reset"}, write(data[cut:] + b"Z"),
                    {"op": "glyph_observe"}]}, ["terminal.reset", "graphics.glyphs"])
