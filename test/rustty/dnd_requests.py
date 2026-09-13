"""OSC 72 conversations and native host drag actions, with no OS side effects."""


def requests():
    def packet(metadata, payload=None, end=b"\x1b\\"):
        return b"\x1b]72;" + metadata + (b";" + payload if payload is not None else b"") + end

    def host(action="observe", **options):
        return {"op": "dnd", "dnd": {"action": action, **options}}

    def item(mime, data):
        return {"mime": mime.hex(), "data": data.hex()}

    def case(name, operations, **options):
        return ({"id": "protocol/dnd/" + name, "kind": "input",
                 "operations": [value if isinstance(value, dict) else
                                {"op": "write", "data": value.hex()} for value in operations],
                 **options}, ["drag-and-drop", "effects.pty"])

    register = packet(b"t=a:i=7", b"text/plain text/uri-list")
    move = host("move", cell_x=5, cell_y=2, pixel_x=40, pixel_y=32,
                mimes=[b"text/plain".hex(), b"text/uri-list".hex()])
    drop = host("drop", cell_x=5, cell_y=2, pixel_x=40, pixel_y=32,
                items=[item(b"text/plain", b"binary\x00\xff"),
                       item(b"text/uri-list", b"file:///tmp/a\r\n")])

    malformed = [b"", b"t", b"t=", b"t=z", b"t=qq", b"t=q:", b":t=q",
                 b"t=q::", b"unknown=1", b"t=q:a=1", b"i=1", b"t=q:i=",
                 b"t=q:i=-1", b"t=q:i=+1", b"t=q:i=4294967295", b"t=q:i=4294967296",
                 b"t=q:i=00000000000", b"t=a:i=1:i=2", b"t=a:t=q", b"t=q:t=a"]
    for index, metadata in enumerate(malformed):
        yield case(f"metadata/{index}", [packet(metadata, b"text/plain"), host()])
    for key in b"xyXY":
        for value in (b"0", b"-0", b"-1", b"2147483647", b"2147483648",
                      b"4294967295", b"-4294967295", b"4294967296", b"--1", b"+1"):
            yield case(f"coordinates/{chr(key)}/{value.decode()}",
                       [packet(b"t=a:" + bytes([key]) + b"=" + value, b"text/plain"), host()])
    for kind in b"aAmMrRopPeEkq":
        for end in (b"\x1b\\", b"\x07"):
            yield case(f"unregistered/{chr(kind)}/{end.hex()}",
                       [packet(b"t=" + bytes([kind]) + b":i=9", b"payload", end), host()])

    yield case("registration", [host(), host("leave"), move, drop, register, host(),
                                 packet(b"t=a:i=8", b"application/octet-stream"), host(),
                                 packet(b"t=A"), host(), packet(b"t=A"), host()])
    yield case("callback-state-order", [register + packet(b"t=a:i=8", b"new-mime")
                                        + packet(b"t=m:o=2", b"new-mime")
                                        + packet(b"t=m:o=0", b"rejected") + packet(b"t=A"), host()])
    yield case("machine-id", [packet(b"t=a:x=1:i=9", b"machine"), host(), register,
                               packet(b"t=a:x=1:i=8", b"another"), host()])
    yield case("registration-chunks", [packet(b"t=a:i=7:m=1", b"text/"), host(),
                                        packet(b"t=q:i=9:m=1", b"plain "), host(),
                                        packet(b"t=A", b"image/png"), host(), move])
    yield case("chunk-malformed", [packet(b"t=a:i=7:m=1", b"text/"), host(),
                                   packet(b"broken", b"ignored"), host(),
                                   packet(b"m=0", b"plain"), host()])
    yield case("move-leave", [register, move, move, host("leave"), host("leave"), move])
    for operation in range(4):
        yield case(f"move-operations/{operation}", [register,
                   host("move", operations=operation, cell_x=4294967295, cell_y=4294967294,
                        pixel_x=-2147483648, pixel_y=2147483647, mimes=[b"\xff".hex()])])
    for mimes in ([], [b""], [b"text/plain", b"text/plain"], [b"a b", b"\xff\x00"]):
        yield case("move-mimes/" + str(len(mimes)) + "-" + b"".join(mimes).hex(),
                   [register, move, host("move", mimes=[mime.hex() for mime in mimes]), host("leave")])
    for size in (4095, 4096, 4097):
        yield case(f"move-payload-chunks/{size}", [register,
                   host("move", mimes=[(b"x" * (size - 1)).hex()])])
    for operation in (0, 1, 2, 3, 4294967295):
        yield case(f"accept/{operation}", [register, move,
                   packet(f"t=m:o={operation}".encode(), b"text/plain  text/uri-list "), host(), drop,
                   packet(f"t=r:o={operation}".encode()), host(), packet(b"t=r:o=1"), host()])
    yield case("accept-chunks", [register, move, packet(b"t=m:o=2:m=1", b"text/"), host(),
                                 packet(b"t=a:o=1:m=1", b"plain "), host(),
                                 packet(b"t=q", b"text/uri-list"), host()])
    yield case("accept-before-hover", [register, packet(b"t=m:o=1", b"text/plain"), host(),
                                       move, packet(b"t=m:o=0"), host(), host("leave"), host()])
    for end in (b"\x1b\\", b"\x07"):
        yield case("data-roundtrip/" + end.hex(), [register, move, drop, host("leave"),
                   packet(b"t=r:x=1:i=99", end=end), packet(b"t=r:x=2", end=end),
                   packet(b"t=r:o=2", end=end), host(), packet(b"t=r:x=1", end=end)])
    for size in (0, 1, 3071, 3072, 3073, 6144, 6145):
        yield case(f"data-chunks/{size}", [register,
                   host("drop", items=[item(b"data", bytes(i % 256 for i in range(size)))]),
                   packet(b"t=r:x=1"), host()])
    for count in (0, 1, 16, 17):
        yield case(f"item-limit/{count}", [register,
                   host("drop", items=[item(f"data/{i}".encode(), bytes([i])) for i in range(count)]),
                   packet(b"t=r:x=1"), packet(b"t=r:x=16"), packet(b"t=r:x=17"), host()])
    for metadata in (b"t=r:x=-1", b"t=r:x=-2147483648", b"t=r:x=2147483648",
                     b"t=r:x=0", b"t=r:x=3", b"t=r:x=1:y=2",
                     b"t=r:x=0:Y=2", b"t=r:x=-1:y=2:Y=-3", b"t=r:y=-1", b"t=r:Y=-1"):
        yield case("data-errors/" + metadata.hex(), [packet(metadata), register, packet(metadata),
                                                    drop, packet(metadata), host()])
    yield case("held-data-lifecycle", [register, drop, move, packet(b"t=r:x=1"), drop,
                                       packet(b"t=a:i=8", b"new-mime"), host(), packet(b"t=r:x=1"),
                                       packet(b"t=A"), host(), packet(b"t=r:x=1")])
    for reset in ({"op": "reset"}, {"op": "terminal_reset"}):
        yield case("reset/" + reset["op"], [register, drop, packet(b"t=m:o=2:m=1", b"text/"),
                                             host(), reset, host(), packet(b"t=q"), host(),
                                             packet(b"t=r:x=1"), packet(b"t=r:o=1"), host()])
    yield case("no-callback", [register, move, packet(b"t=m:o=1", b"text/plain"), drop,
                               packet(b"t=r:x=1"), packet(b"t=r:o=1"), packet(b"t=A"), host()],
               dnd_events=False)
    for kind in (b"o", b"p", b"P", b"e", b"E", b"k"):
        for x in (-1, 0, 1, 2, 3):
            yield case(f"drag-out/{kind.decode()}/{x}", [register,
                       packet(b"t=" + kind + f":x={x}:i=99".encode(), b"data"), host()])

    # The 1 MiB MIME cap is reached across commands; the native OSC capture
    # limit must not be mistaken for the accumulated registration/status cap.
    for kind in (b"a", b"m"):
        for extra in (0, 1):
            chunk = packet(b"t=" + kind + b":o=1:m=1", b"x" * 4096)
            yield case(f"mime-limit/{kind.decode()}/{extra}", [register, move,
                       chunk * 256, packet(b"m=0", b"x" * extra), host(),
                       packet(b"t=q"), host()])


def snapshot_requests(peers):
    """Restore both encodings; committed state is omitted, parser bytes survive."""
    prefixes = [b"\x1b]72;t=a:i=7;text/plain\x1b\\",
                b"\x1b]72;t=a:i=7:m=1;text/\x1b\\",
                b"\x1b]72;t=a:i=7;text/"]
    for peer in peers:
        for index, prefix in enumerate(prefixes):
            source = peer.request({"id": "dnd/snapshot-source", "kind": "input", "operations": [
                {"op": "write", "data": prefix.hex()}, {"op": "snapshot"}]})
            if not source["ok"] or len(source["snapshots"]) != 1:
                raise RuntimeError("DND snapshot capture failed")
            yield ({"id": f"protocol/dnd/snapshot/{peer.name}/{index}", "kind": "input", "operations": [
                {"op": "write", "data": b"\x1b]72;t=a:i=9;old\x1b\\".hex()},
                {"op": "dnd", "dnd": {"action": "drop", "items": [{"mime": "78", "data": "79"}]}},
                {"op": "restore", "data": source["snapshots"][0]},
                {"op": "dnd", "dnd": {"action": "observe"}},
                {"op": "write", "data": (b"plain\x1b\\" if index == 2 else
                                         b"\x1b]72;t=q;plain\x1b\\").hex()},
                {"op": "write", "data": b"\x1b]72;t=r:x=1\x1b\\".hex()},
                {"op": "dnd", "dnd": {"action": "observe"}},
            ]}, ["drag-and-drop", "snapshot.cross-decode"])
