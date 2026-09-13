"""Compare both snapshot encodings in both decoders and with uninterrupted state."""
import struct


def fixture(path):
    return bytes.fromhex(" ".join(line.split("#", 1)[0] for line in path.read_text().splitlines()))


def records(data):
    if data[:10] != b"GHOSTSNP\x01\0":
        raise ValueError("unsupported snapshot fixture envelope")
    result = []
    offset = 10
    while offset < len(data):
        tag, length = struct.unpack_from("<HI", data, offset)
        end = offset + 10 + length
        if end > len(data):
            raise ValueError("truncated snapshot fixture")
        result.append((tag, data[offset + 10:end]))
        offset = end
    return result


def frame(records):
    result = bytearray(b"GHOSTSNP\x01\0")
    for tag, payload in records:
        prefix = struct.pack("<HI", tag, len(payload))
        crc = 0xffffffff
        for byte in prefix + payload:
            crc ^= byte
            for _ in range(8):
                crc = (crc >> 1) ^ (0x82f63b78 if crc & 1 else 0)
        result.extend(prefix + struct.pack("<I", crc ^ 0xffffffff) + payload)
    return bytes(result)


def wire_requests(root, reference):
    import hyperlink_requests
    import snapshot_resources
    yield from snapshot_resources.requests(reference)
    yield from snapshot_resources.style_requests(reference)
    reference_limits = []
    for request, covers in hyperlink_requests.requests(reference):
        if request["id"].startswith("snapshot/reference-limit/"):
            reference_limits.append((request, covers))
        else:
            yield request, covers

    data = fixture(root / "src/terminal/snapshot/testdata/complete-v1.hex")
    if frame(records(data)) != data:
        raise RuntimeError("snapshot fixture framing or checksum differs")
    yield ({"id": "snapshot/wire/complete-v1", "operations": [
        {"op": "restore", "data": data.hex()}, {"op": "observe"},
        write(b"\x1b[?1049l\x1b[0mZ"),
    ]}, ["snapshot.fixtures"])

    for physical, logical in ((4, 8), (8, 4)):
        encoded = reference.request({"id": "snapshot/mixed-source", "cols": physical, "rows": 1,
            "operations": [write(b"abcdefgh"[:physical]), {"op": "snapshot"}]})
        if not encoded["ok"] or len(encoded["snapshots"]) != 1:
            raise RuntimeError("reference could not encode mixed-PAGE fixture")
        parts = records(bytes.fromhex(encoded["snapshots"][0]))
        terminal = bytearray(parts[0][1])
        struct.pack_into("<H", terminal, 0, logical)
        struct.pack_into("<H", terminal, 18, logical - 1)
        parts[0] = (1, terminal)
        if physical > logical:
            reference_limits.append(({"id": "snapshot/reference-limit/wide-cursor", "operations": [
                {"op": "restore", "data": frame(parts).hex()}, {"op": "observe"},
            ]}, ["snapshot.cross-decode"]))
            screen = bytearray(parts[1][1])
            struct.pack_into("<H", screen, 12, logical - 1)
            screen[17] &= ~1  # The cursor no longer reaches the physical edge.
            parts[1] = (2, screen)
        data = frame(parts)
        for name, after in (
            ("observe", []), ("write", [write(b"a")]), ("up", [write(b"\x1b[A")]),
            ("right", [write(b"\x1b[C")]), ("left", [write(b"\x1b[D")]),
            ("erase", [write(b"\x1b[2J")]), ("resize", [{"op": "resize", "cols": logical + 1, "rows": 2}]),
        ):
            request = {"id": f"snapshot/wire/page-{physical}-terminal-{logical}/{name}",
                "operations": [{"op": "restore", "data": data.hex()}, {"op": "observe"}] + after}
            if physical < logical and name == "right":
                request["id"] = "snapshot/reference-limit/narrow-cursor-right"
                reference_limits.append((request, ["snapshot.cross-decode"]))
            else:
                yield request, ["snapshot.cross-decode"]
    # Keep reference assertions visible and in thorough runs. They are last so
    # an infrastructure abort does not hide the preceding decoder comparisons.
    yield from reference_limits


def streaming_requests(root):
    data = fixture(root / "src/terminal/snapshot/testdata/complete-v1.hex") + b"transport tail"
    mutations = {
        "unchanged": [], "write": [write(b"X\r\n")], "erase-history": [write(b"\x1b[3J")],
        "switch-screen": [write(b"\x1b[?1047l")], "reset": [{"op": "reset"}],
        "resize-width": [{"op": "resize", "cols": 3, "rows": 3}],
        "resize-height": [{"op": "resize", "cols": 2, "rows": 5}],
    }
    for name, mutation in mutations.items():
        for after_pages in (0, 1):
            operations = [{"op": "restore_ready", "data": data.hex()}, {"op": "observe"}]
            for step in range(4):
                if step == after_pages:
                    operations.extend(mutation)
                operations.extend([{"op": "restore_next"}, {"op": "observe"}])
            yield ({"id": f"snapshot/streaming/{name}/{after_pages}", "operations": operations},
                   ["snapshot.streaming", "snapshot.fixtures"])


def invalid_requests(root):
    data = fixture(root / "src/terminal/snapshot/testdata/complete-v1.hex")
    for length in range(len(data)):
        yield ({"id": f"snapshot/invalid/truncated/{length}", "expected_error": "InvalidSnapshot",
                "operations": [{"op": "restore", "data": data[:length].hex()}]}, ["snapshot.fixtures"])
    for offset in (0, 8, 16, 50, 1024, len(data) - 1):
        corrupt = bytearray(data)
        corrupt[offset] ^= 1
        yield ({"id": f"snapshot/invalid/corrupt/{offset}", "expected_error": "InvalidSnapshot",
                "operations": [{"op": "restore", "data": corrupt.hex()}]}, ["snapshot.fixtures"])


def write(data):
    return {"op": "write", "data": data.hex()}


def requests():
    def case(name, prefix, suffix=b"\x1b[0mZ", cols=12, rows=4):
        operations = [write(prefix)] if isinstance(prefix, bytes) else prefix
        return ({"id": "snapshot/" + name, "kind": "snapshot", "cols": cols, "rows": rows,
                 "operations": operations, "after": [write(suffix)]}, ["snapshot.cross-decode"])

    yield case("empty", b"")
    yield case("unicode", "é界👩🏽‍💻".encode())
    yield case("styles-links", b"\x1b[1;3;38:2::10:20:30mA\x1b]8;id=x;https://example.org\x1b\\B")
    for count in (103, 104, 207, 208, 256):
        yield case(f"encoding-resources/styles/{count}",
                   b"".join(f"\x1b[38;5;{index}mX".encode() for index in range(count)),
                   cols=64, rows=4)
    import snapshot_resources
    colliding = []
    number = 0
    while len(colliding) < 33:
        if snapshot_resources._style_hash(snapshot_resources._style_value(number)) & 127 == 127:
            colliding.append(number)
        number += 1
    yield case("encoding-resources/style-collision",
               b"".join(f"\x1b[38;2;{value & 255};{value >> 8 & 255};{value >> 16 & 255}mX".encode()
                        for value in colliding), cols=16, rows=3)
    for count in (2, 3, 32, 40):
        yield case(f"encoding-resources/links/{count}",
                   b"".join(f"\x1b]8;id={index};https://example.org/{index}/{'x' * 128}\x07XXXXX".encode()
                            for index in range(count)), cols=80, rows=4)
    for count in (102, 103, 200):
        yield case(f"encoding-resources/linked-cells/{count}",
                   b"\x1b]8;id=one;https://example.org\x07" + b"x" * count,
                   cols=80, rows=3)
    for count in (30, 31, 32, 64):
        yield case(f"encoding-resources/graphemes/{count}",
                   ("\x1b[?2027h" + ("A" + "\u0301" * 64) * count).encode(), cols=16, rows=4)
    yield case("saved-cursor", b"A\x1b[31m\x1b7\x1b[4;8HB", b"\x1b8C")
    yield case("alt-active", b"primary\x1b[?1049halternate", b"\x1b[?1049lZ")
    yield case("alt-inactive", b"primary\x1b[?47halternate\x1b[?47l", b"\x1b[?47hZ")
    yield case("pending-wrap", b"abcdefghijkl", b"M")
    yield case("history", b"".join(f"line {i}\r\n".encode() for i in range(40)))
    yield case("margins-tabs", b"\x1b[2;4r\x1b[?69h\x1b[3;10s\x1b[3g\x1b[1;5H\x1bH", b"\r\tX\nY")
    yield case("modes", b"\x1b[?7l\x1b[4h\x1b[?25l\x1b[2 q\x1b[?1h", b"abcdefghijklm\x1b[6n")
    yield case("kitty-ring", b"".join(f"\x1b[>{i}u".encode() for i in range(1, 11)),
               b"\x1b[<2u\x1b[?u\x1b[<1u\x1b[?u\x1b[=16;2u\x1b[?u\x1b[=3;3u\x1b[?u\x1b[<8u\x1b[?u")
    yield case("reflow", [write(b"abcdefghijklmnopqrstuv\r\nlast"), {"op": "resize", "cols": 7, "rows": 3}])

    # Every possible cut includes both fresh and partly populated parser state.
    continuations = {
        "utf8": "😄".encode(),
        "escape": b"\x1b(0q",
        "csi": b"\x1b[38:2::10:20:30mX",
        "osc": b"\x1b]2;restored title\x1b\\",
        "dcs": b"\x1bP$qm\x1b\\",
        "apc": b"\x1b_Ga=q,i=7,s=1,v=1,f=24;AAAA\x1b\\",
        "committed-control": b"\x1b[3\x071mX",
    }
    for name, data in continuations.items():
        for cut in range(1, len(data)):
            yield case(f"continuation/{name}/{cut}", b"A" + data[:cut], data[cut:] + b"Z")


def compare(peers, request, difference):
    """Wire page grouping may differ; preserve both encodings in artifacts."""
    def semantic(response):
        return {k: v for k, v in response.items() if k not in ("id", "capabilities", "snapshots")}

    args = {k: v for k, v in request.items() if k not in ("kind", "after")}
    args["kind"] = "terminal"
    prefix = request.get("operations", [])
    suffix = [{"op": "observe"}] + request.get("after", [])
    encoded = [peer.request(dict(args, operations=prefix + [{"op": "snapshot"}])) for peer in peers]
    results = [{"id": request["id"], "ok": response["ok"], "err": response["err"],
                "source": semantic(response)} for response in encoded]
    if not all(response["ok"] for response in encoded):
        return *results, difference(*results) or "snapshot encoding failed"
    if any(len(response["snapshots"]) != 1 for response in encoded):
        raise RuntimeError("snapshot encoder did not return exactly one payload")
    payloads = dict(zip(("zig", "rust"), (response["snapshots"][0] for response in encoded)))
    own_difference = None
    for peer, result in zip(peers, results):
        result["encodings"] = payloads
        live = peer.request(dict(args, operations=prefix + [{"op": "checkpoint"}] + suffix))
        result["live"] = semantic(live)
        result["restored"] = {}
        for source, data in payloads.items():
            restored = peer.request(dict(args, operations=[{"op": "restore", "data": data}] + suffix))
            value = semantic(restored)
            result["restored"][source] = value
            result["ok"] &= live["ok"] and restored["ok"]
            if own_difference is None:
                own_difference = difference(result["live"], value, f"{peer.name}.restore-{source}")
    compared = [{k: v for k, v in result.items() if k != "encodings"} for result in results]
    failure = None if all(result["ok"] for result in results) else "snapshot phase failed"
    return *results, difference(*compared) or own_difference or failure
