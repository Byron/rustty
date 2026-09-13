"""Retained Glyph APC parsing, outlines, eviction and bounded capture cases."""
import base64
import struct


def apc(command):
    return b"\x1b_25a1;" + command + b"\x1b\\"


def write(data):
    return {"op": "write", "data": data.hex()}


def outline(points=0):
    data = struct.pack(">hhhhh", int(points > 0), 0, 0, 0, 0)
    if points:
        data += struct.pack(">HH", points - 1, 0)
        while points:
            count = min(points, 256)
            data += bytes([0x39, count - 1])
            points -= count
    return data


def register(options=b"cp=e0a0", data=None):
    return b"r;" + options + b";" + base64.b64encode(outline() if data is None else data)


def requests():
    def case(name, operations, kind="input"):
        return ({"id": f"protocol/glyph/{name}", "kind": kind, "cols": 8, "rows": 3,
                 "operations": operations + [{"op": "glyph_observe"}]}, ["graphics.glyphs"])

    def commands(name, values):
        operations = []
        for value in values:
            operations += [write(apc(value)), {"op": "glyph_observe"}]
        return case(name, operations)

    for command in (b"", b"s", b"s;", b"s;ignored=1", b"q", b"r", b"r;", b"c",
                    b"unknown", b"support", b";s", b"S", b"s\x00", b"r;cp=e0a0"):
        yield commands(f"framing/{command.hex()}", [command])
    for cp in (b"0", b"-0", b"+0", b"41", b"d800", b"e000", b"e0a0", b"f8ff",
               b"f900", b"f0000", b"ffffd", b"ffffe", b"100000", b"10fffd",
               b"10fffe", b"1fffff", b"200000", b"0xe0a0", b"E0_A0", b"_e0a0",
               b"e0a0_", b"-e0a0", b"+e0a0", b"", b"\xff", b" e0a0"):
        yield commands(f"codepoint/{cp.hex()}", [
            register(b"cp=" + cp), b"q;cp=" + cp, b"c;cp=" + cp, b"q;cp=" + cp])

    options = {
        b"fmt": [b"glyf", b"colrv0", b"colrv1", b"", b"GLYF"],
        b"reply": [b"0", b"1", b"2", b"3", b"01", b"", b"none"],
        b"upm": [b"0", b"1", b"2048", b"+12", b"-0", b"4_096", b"4294967295", b"4294967296", b"1.0", b""],
        b"aw": [b"0", b"4096", b"+1", b"1__2", b"0x1000"],
        b"lh": [b"0", b"4096", b"-1", b" 1000"],
        b"width": [b"1", b"2", b"3", b"01", b"", b"+1"],
        b"size": [b"height", b"advance", b"contain", b"cover", b"stretch", b"fit", b""],
        b"align": [b"start,start", b"end,end", b"center,center", b"end,baseline",
                   b"baseline,center", b"start", b"start,center,extra", b""],
        b"pad": [b"0,0,0,0", b".1,.2,.3,.4", b"0.4,0.2,0.6,0.1",
                 b"-0,+0,.5,0.", b"1e-1,0,0,0", b"0,0,0", b"0,0,0,0,0",
                 b"1.1,0,0,0", b"nan,0,0,0", b"0.123456789012345999,0,0,0"],
    }
    for key, values in options.items():
        for value in values:
            opts = b"cp=e0a0;" + key + b"=" + value
            yield commands(f"option/{key.decode()}/{value.hex()}", [
                register(opts), b"r;" + opts + b";%%%", b"q;cp=e0a0"])
    for options in (b"cp=e0a0;cp=e0a1", b"cp=e0a0;cp=bad!", b"cp=bad!;cp=e0a0",
                    b"cp=e0a0;upm=0;upm=2048", b"cp=e0a0;reply=0;reply=2",
                    b"cp=e0a0;unknown=anything", b"cp=e0a0;;", b"cp=e0a0;CP=e0a1"):
        yield commands(f"duplicates/{options.hex()}", [register(options)])

    for payload in (b"", b"A", b"AA", b"AAAA", b"AAAAAAAAAAAAAA==", b"AAAAAAAAAAAAAB==",
                    b"AAAAAAAAAAAAAA", b"AAAAAAAAAAAAAA= ", b"%%==", b"====",
                    b"AAAAAAAAAAAAAA==extra", b"AAAAAAAAAAAAAA==\n"):
        yield commands(f"base64/{payload.hex()}", [b"r;cp=e0a0;" + payload])

    # Both short and signed-long coordinate deltas, repeats and off-curve points.
    triangle = (struct.pack(">hhhhhHH", 1, -1, -1, 100, 100, 2, 0)
                + bytes([0x37, 0x06, 0x01, 10, 5]) + struct.pack(">h", 40)
                + bytes([20, 3]) + struct.pack(">h", -30))
    yield commands("outline/triangle", [register(data=triangle)])
    for cut in range(len(triangle)):
        yield commands(f"outline/truncated/{cut}", [register(data=triangle[:cut])])
    for name, data in (
        ("composite", struct.pack(">hhhhh", -1, 0, 0, 0, 0)),
        ("zero-extra-byte", outline() + b"\xff"),
        ("zero-instructions", outline() + b"\x00\x01"),
        ("one-instructions", struct.pack(">hhhhhHH", 1, 0, 0, 0, 0, 0, 1)),
        ("unordered-contours", struct.pack(">hhhhhHHH", 2, 0, 0, 0, 0, 0, 0, 0)),
        ("repeat-overrun", struct.pack(">hhhhhHH", 1, 0, 0, 0, 0, 0, 0) + b"\x39\x01"),
        ("reserved-flags", struct.pack(">hhhhhHH", 1, 0, 0, 0, 0, 0, 0) + b"\xf1"),
        ("trailing-data", triangle + b"ignored"),
        ("decoded-64k", outline() + bytes(65536 - 10)),
        ("decoded-over-64k", outline() + bytes(65537 - 10)),
    ):
        yield commands(f"outline/{name}", [register(data=data)])
    for points in (1, 2, 255, 256, 257, 4096, 5461, 5462, 65535, 65536):
        yield commands(f"outline/point-allocation/{points}", [register(data=outline(points))])
    yield commands("errors/namespace-after-decode", [
        register(b"cp=41", b""), register(b"cp=41"), register(b"cp=41;upm=0")])

    yield commands("storage/replace-and-clear", [
        register(), register(b"cp=e0a1"), register(b"cp=e0a0;upm=2048;width=2"),
        b"c;cp=invalid", b"c;cp=e0a1", b"c;cp=e0a1", b"c", b"q;cp=e0a0"])
    fill = b"".join(apc(register(f"cp={cp:x}".encode())) for cp in range(0xe000, 0xe400))
    yield case("storage/fifo", [write(fill), {"op": "glyph_observe"},
        write(apc(register(b"cp=e000;upm=2048"))), write(apc(register(b"cp=e400"))),
        write(apc(b"q;cp=e000") + apc(b"q;cp=e001"))])

    for reset in (write(b"\x1bc"), {"op": "terminal_reset"}):
        yield case(f"reset/{reset['op']}", [write(apc(register())), {"op": "glyph_clean"},
            reset, write(apc(b"q;cp=e0a0"))])
    yield case("storage/screens", [write(apc(register())), write(b"\x1b[?1049h"),
        write(apc(b"q;cp=e0a0") + apc(register(b"cp=e0a1"))), write(b"\x1b[?1049l")])
    yield case("storage/text-width-is-native", [write(apc(register(b"cp=e0a0;width=2"))),
        write("\ue0a0X\ue0a0".encode())], kind="terminal")

    for limit in (0, 1, 2, 3, 10, 30, 31, 32):
        yield case(f"capture/limit/{limit}", [{"op": "glyph_limit", "glyph_max_bytes": limit},
            write(apc(b"s")), write(apc(register())), {"op": "glyph_limit"},
            write(apc(b"q;cp=e0a0"))])
    for offset in (0, 2, 4, 5, 6, 8, 18):
        body = b"25a1;" + register()
        yield case(f"capture/disable/{offset}", [write(b"\x1b_" + body[:offset]),
            {"op": "glyph_enable", "value": False}, write(body[offset:] + b"\x1b\\"),
            {"op": "glyph_observe"}, {"op": "glyph_enable", "value": True},
            write(apc(b"q;cp=e0a0"))])
        yield case(f"capture/change-limit/{offset}", [write(b"\x1b_" + body[:offset]),
            {"op": "glyph_limit", "glyph_max_bytes": 1}, write(body[offset:] + b"\x1b\\")])
    yield case("capture/disabled-persists-reset", [{"op": "glyph_enable", "value": False},
        write(b"\x1bc"), write(apc(b"s") + apc(register()))])
    yield case("capture/default-boundary", [
        write(apc(b"s;" + b"x" * (1024 * 1024 - 2))),
        write(apc(b"s;" + b"x" * (1024 * 1024 - 1)))])
    for framing in (b"\x1b_25A1;s\x1b\\", b"\x1b_25a1s\x1b\\",
                    b"\x1b_25a1;s\x07\x1b\\", b"\x1b_25a1;s\x18",
                    b"\x1b_25a1;s\x1b_25a1;s\x1b\\"):
        yield case(f"capture/framing/{framing.hex()}", [write(framing)])

    data = apc(register())
    # ESC commits APC before the following backslash is consumed. Once that
    # happens the glossary is deliberately absent from snapshot v1; exercise
    # that separate reference limitation in snapshot_wire_requests below.
    for cut in range(1, len(data) - 1):
        yield ({"id": f"protocol/glyph/snapshot/continuation/{cut}", "kind": "snapshot",
                "cols": 8, "rows": 3, "operations": [write(b"A" + data[:cut])],
                "after": [write(data[cut:]), {"op": "glyph_observe"}]},
               ["graphics.glyphs", "snapshot.cross-decode"])


def snapshot_wire_requests(reference):
    """Compare native v1's deliberate loss of already-committed registrations."""
    encoded = reference.request({"id": "glyph/snapshot-source", "kind": "input",
        "cols": 8, "rows": 3, "operations": [
            write(apc(register())[:-1]), {"op": "glyph_observe"}, {"op": "snapshot"}]})
    if not encoded["ok"] or len(encoded["snapshots"]) != 1:
        raise RuntimeError("native glyph snapshot capture failed")
    entries = encoded["glyph_results"][0]["entries"]
    if len(entries) != 1 or entries[0]["codepoint"] != 0xe0a0:
        raise RuntimeError("native APC did not commit at the final ESC")
    yield ({"id": "protocol/glyph/snapshot/committed-registrations-omitted", "kind": "input",
        "cols": 8, "rows": 3, "operations": [write(apc(register())), {"op": "glyph_observe"},
            {"op": "restore", "data": encoded["snapshots"][0]}, {"op": "glyph_observe"},
            write(b"\\" + apc(b"q;cp=e0a0"))]}, ["graphics.glyphs", "snapshot.cross-decode"])


if __name__ == "__main__":
    import json
    import sys
    json.dump([{"request": request, "covers": covers} for request, covers in requests()],
              sys.stdout, indent=2)
    sys.stdout.write("\n")
