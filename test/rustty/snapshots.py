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
    yield from metadata_requests(reference)
    yield from boundary_requests(reference)
    yield from hyperlink_requests.requests(reference)

    data = fixture(root / "src/terminal/snapshot/testdata/complete-v1.hex")
    if frame(records(data)) != data:
        raise RuntimeError("snapshot fixture framing or checksum differs")
    yield ({"id": "snapshot/wire/complete-v1", "operations": [
        {"op": "restore", "data": data.hex()}, {"op": "observe"},
        write(b"\x1b[?1049l\x1b[0mZ"),
    ]}, ["snapshot.fixtures"])

    for name, payload in (("complete", data), ("trailing", data + b"transport tail"),
                          ("concatenated", data + data), ("empty", b""), ("truncated", data[:-1])):
        request = {"id": "snapshot/exact/" + name,
                   "operations": [{"op": "restore_exact", "data": payload.hex()}, {"op": "observe"}]}
        if name != "complete":
            request["expected_error"] = "InvalidSnapshot"
        yield request, ["snapshot.fixtures", "snapshot.cross-decode"]

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
            yield ({"id": "snapshot/wire/page-8-terminal-4/clamped-cursor", "operations": [
                {"op": "restore", "data": frame(parts).hex()}, {"op": "observe"},
                {"op": "pages"}, {"op": "snapshot"}, write(b"Z"),
                {"op": "observe"}, {"op": "pages"}, {"op": "snapshot"},
            ]}, ["snapshot.cross-decode", "terminal.cursor", "terminal.pages"])
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
            yield request, ["snapshot.cross-decode"]


def metadata_requests(reference):
    encoded = reference.request({"id": "snapshot/metadata-source", "cols": 8, "rows": 3,
        "operations": [write(b"abc"), {"op": "snapshot"}]})
    if not encoded["ok"] or len(encoded["snapshots"]) != 1:
        raise RuntimeError("reference could not encode metadata fixture")
    base = records(bytes.fromhex(encoded["snapshots"][0]))
    for value in (0, 1, 2, 255):
        parts = [(tag, bytearray(payload)) for tag, payload in base]
        parts[0][1][29] = value
        yield ({"id": f"snapshot/metadata/cursor-default/{value}", "operations": [
            {"op": "restore", "data": frame(parts).hex()}, {"op": "snapshot"},
            {"op": "cursor_defaults", "cursor_shape": "bar", "cursor_blink": True},
            {"op": "observe"}, {"op": "snapshot"},
        ]}, ["snapshot.fixtures", "snapshot.cross-decode"])
    for value in (0, 0x41, 0xD800, 0xDFFF, 0x10FFFF, 0x110000, 0xFFFFFFFF):
        parts = [(tag, bytearray(payload)) for tag, payload in base]
        struct.pack_into("<I", parts[0][1], 25, value)
        yield ({"id": f"snapshot/metadata/previous-char/{value}", "kind": "snapshot",
            "operations": [{"op": "restore", "data": frame(parts).hex()}], "after": [
            write(b"\x1b[31m\x1b]8;;https://example.org\x07\x1b[3b\x1b[6n"),
        ]}, ["snapshot.fixtures", "snapshot.cross-decode"])
    fields = [
        (0, 20, "B", "status-display", (0, 1, 2, 255)),
        (0, 30, "B", "cursor-shape", (0, 1, 2, 3, 4, 255)),
        (0, 31, "B", "cursor-blink", (0, 1, 2, 3, 255)),
        (0, 32, "B", "shell-redraw", (0, 1, 2, 3, 255)),
        (0, 33, "B", "modify-other-keys", (0, 1, 2, 255)),
        (0, 34, "B", "mouse-event", (0, 1, 2, 3, 4, 5, 255)),
        (0, 35, "B", "mouse-format", (0, 1, 2, 3, 4, 5, 255)),
        (0, 36, "B", "mouse-shift", (0, 1, 2, 3, 255)),
        (0, 37, "B", "mouse-shape", (0, 8, 33, 34, 255)),
        (0, 38, "B", "password-input", (0, 1, 2, 255)),
        (0, 21, "H", "active-screen", (0, 1, 2, 65535)),
        (1, 12, "H", "screen-cursor-x", (0, 7, 8, 65535)),
        (1, 14, "H", "screen-cursor-y", (0, 2, 3, 65535)),
        (1, 16, "B", "screen-cursor-shape", (0, 1, 2, 3, 4, 255)),
        (1, 17, "B", "screen-cursor-flags", (0, 1, 2, 4, 8, 12, 16, 255)),
        (1, 38, "H", "screen-charset", (0, 0x4000, 0x5000, 0xFFFF)),
        (1, 40, "B", "screen-protection", (0, 1, 2, 3, 255)),
        (1, 41, "B", "keyboard-index", (0, 1, 7, 8, 255)),
        (1, 50, "B", "semantic-click-kind", (0, 1, 2, 3, 255)),
        (1, 51, "B", "semantic-click-value", (0, 1, 3, 4, 255)),
    ]
    for offset in (12, 14, 16, 18):
        fields.append((0, offset, "H", f"margin-{offset}", (0, 1, 2, 3, 7, 8, 65535)))
    for offset in (63, 67, 71, 75, 79, 83):
        fields.append((0, offset, "B", f"dynamic-color-{offset}", (0, 1, 2, 255)))
    for offset in (87, 95):
        fields.append((0, offset, "Q", f"limit-{offset}", (0, 1, 1024, 65536, 2**64 - 2, 2**64 - 1)))
    for offset in (39, 47, 55):
        fields.append((0, offset, "Q", f"mode-{offset}", (0, 2**63, 2**64 - 1)))
    for offset in range(42, 50):
        fields.append((1, offset, "B", f"keyboard-flags-{offset}", (0, 31, 32, 255)))
    for index, offset, fmt, name, values in fields:
        for value in values:
            parts = [(tag, bytearray(payload)) for tag, payload in base]
            struct.pack_into("<" + fmt, parts[index][1], offset, value)
            yield ({"id": f"snapshot/metadata/{name}/{value}", "observe_colors": True,
                "observe_semantic": True, "operations": [
                    {"op": "restore", "data": frame(parts).hex()}, {"op": "observe"},
                    {"op": "snapshot"}, write(b"\x1b[3b\x1b[6n"), {"op": "observe"},
                ]}, ["snapshot.fixtures", "snapshot.cross-decode"])


def boundary_requests(reference):
    encoded = reference.request({"id": "snapshot/boundary-source", "cols": 8, "rows": 3,
        "operations": [write(b"abc"), {"op": "snapshot"}]})
    if not encoded["ok"] or len(encoded["snapshots"]) != 1:
        raise RuntimeError("reference could not encode boundary fixture")
    base = records(bytes.fromhex(encoded["snapshots"][0]))

    def invalid(name, parts):
        return ({"id": "snapshot/boundary/" + name, "expected_error": "InvalidSnapshot",
            "operations": [{"op": "restore", "data": frame(parts).hex()}]}, ["snapshot.fixtures"])

    for index, (tag, payload) in enumerate(base):
        for length in sorted({0, 1, max(0, len(payload) - 1)} - {len(payload)}):
            if length < len(payload):
                yield invalid(f"record-{tag}/short-{length}",
                              base[:index] + [(tag, payload[:length])] + base[index + 1:])
        yield invalid(f"record-{tag}/trailing", base[:index] + [(tag, payload + b"\0")] + base[index + 1:])
        yield invalid(f"record-{tag}/unknown-tag", base[:index] + [(65535, payload)] + base[index + 1:])
        yield invalid(f"record-{tag}/missing", base[:index] + base[index + 1:])
        # FINISH ends the stream, so another FINISH is an accepted transport tail.
        if tag != 6:
            yield invalid(f"record-{tag}/duplicate", base[:index] + [base[index]] + base[index:])
    for index, offset, name, values in ((0, 0, "columns", (0,)), (0, 2, "rows", (0,)),
            (0, 23, "screen-count", (0, 2, 3, 65535)), (1, 0, "screen-key", (1, 2, 65535)),
            (1, 2, "page-count", (0, 2, 65535))):
        for value in values:
            parts = [(tag, bytearray(payload)) for tag, payload in base]
            struct.pack_into("<H", parts[index][1], offset, value)
            yield invalid(f"{name}/{value}", parts)
    for limit in (0, 1, 2, 5, 8 * 1024 * 1024):
        for operation in ("restore", "restore_exact", "restore_ready"):
            yield ({"id": f"snapshot/policy-live/{limit}/{operation}", "operations": [
                {"op": operation, "data": frame(base).hex(), "snapshot_max_continuation_bytes": limit},
                write(b"\x1b]2;longer title"), {"op": "snapshot"}, write(b"\x07"), {"op": "observe"},
            ]}, ["snapshot.fixtures", "snapshot.cross-decode"])
    continuation_index = next(i for i, (tag, _) in enumerate(base) if tag == 7)
    for payload in (b"", b"\x1b", b"\x1b[1;", b"\x1b]2;title", b"\xf0\x9f\x98"):
        parts = list(base)
        parts[continuation_index] = (7, payload)
        for limit in sorted({0, max(0, len(payload) - 1), len(payload), len(payload) + 1}):
            for operation in ("restore", "restore_exact", "restore_ready"):
                request = {"id": f"snapshot/boundary/continuation/{payload.hex()}/{limit}/{operation}",
                    "operations": [{"op": operation, "data": frame(parts).hex(),
                                    "snapshot_max_continuation_bytes": limit}, {"op": "observe"}]}
                if limit < len(payload):
                    request["expected_error"] = "InvalidSnapshot"
                yield request, ["snapshot.fixtures", "snapshot.cross-decode"]


def streaming_requests(root):
    data = fixture(root / "src/terminal/snapshot/testdata/complete-v1.hex") + b"transport tail"
    mutations = {
        "unchanged": [], "write": [write(b"X\r\n")], "erase-history": [write(b"\x1b[3J")],
        "switch-screen": [write(b"\x1b[?1047l")], "reset": [{"op": "reset"}],
        "resize-width": [{"op": "resize", "cols": 3, "rows": 3}],
        "resize-height": [{"op": "resize", "cols": 2, "rows": 5}],
        "resize-width-back": [{"op": "resize", "cols": 3, "rows": 3},
                              {"op": "resize", "cols": 2, "rows": 3}],
        "resize-width-back-after-drop": [{"op": "resize", "cols": 3, "rows": 3},
                                         {"op": "restore_next"},
                                         {"op": "resize", "cols": 2, "rows": 3}],
        "reset-resize-width-back": [{"op": "reset"},
                                    {"op": "resize", "cols": 3, "rows": 3},
                                    {"op": "resize", "cols": 2, "rows": 3}],
        "resize-height-back": [{"op": "resize", "cols": 2, "rows": 5},
                               {"op": "resize", "cols": 2, "rows": 3}],
        "recreate-alternate": [write(b"\x1b[?1047l\x1b[?1047h")],
        "shrink-limits": [{"op": "grid", "grid": {"action": "limits", "bytes": 0}}],
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
