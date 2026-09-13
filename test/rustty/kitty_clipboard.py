"""Kitty clipboard requests evaluated against the original stream handler."""
import base64


def requests():
    def b64(data):
        return base64.b64encode(data)

    def content(mime, data):
        return {"mime": mime.hex(), "data": data.hex()}

    def packet(metadata, payload=None, end=b"\x1b\\"):
        return b"\x1b]5522;" + metadata + (b";" + payload if payload is not None else b"") + end

    def case(name, packets, replies=(), **options):
        return ({"id": "protocol/kitty-clipboard/" + name,
                 "operations": [p if isinstance(p, dict) else {"op": "write", "data": p.hex()} for p in packets],
                 "clipboard_replies": list(replies), **options}, ["clipboard", "effects.pty"])

    def data(mime, payload, end=b"\x1b\\"):
        return packet(b"type=wdata:mime=" + b64(mime), payload, end)

    def alias(mime, aliases):
        return packet(b"type=walias:mime=" + b64(mime), b64(aliases))

    begin = packet(b"type=write:id=opening")
    commit = packet(b"type=wdata:id=closing")
    text_reply = {"contents": [content(b"text/plain", b"text\x00\xff"), content(b"image/png", b"\x89PNG")],
                  "available": [b"text/plain".hex(), b"image/png".hex()]}

    metadata = [b"", b"type", b"type=", b"type=unknown", b"type=read", b"type=READ",
                b"type=read:", b":type=read", b"type=read:broken", b"type=read:=value",
                b"type=unknown:type=read", b"type=read:type=unknown", b"type=read:unknown=x",
                b"type=read:id=a!b c_+-./?:id=second", b"type=read:id=" + b"a!" * 520,
                b"type=read:loc=primary", b"type=read:loc=selection", b"type=read:loc=PRIMARY",
                b"type=read:loc=primary:loc=unknown", b"type=read:password=ignored"]
    for index, meta in enumerate(metadata):
        yield case(f"metadata/structure/{index}", [packet(meta, b64(b"text/plain"))], [text_reply])

    for field, bound in ((b"mime", 256), (b"name", 256), (b"pw", 128)):
        values = [b"", b"***", b"Zg=", b"Zg", b"Zg==", b"Zh==", b"Z g==", b64(b"\xff"),
                  b64(b"x" * bound), b64(b"x" * (bound + 1)), b64(b"\x00")]
        for index, value in enumerate(values):
            extra = b":" + field + b"=" + value
            yield case(f"metadata/{field.decode()}/{index}/read",
                       [packet(b"type=read" + extra, b64(b"text/plain"))], [text_reply])
            for operation in (b"wdata", b"walias"):
                yield case(f"metadata/{field.decode()}/{index}/{operation.decode()}",
                           [begin, packet(b"type=" + operation + extra, b64(b"x")), commit])

    mime_lists = [b"", b".", b"text/plain", b"image/png text/plain", b". text/plain . image/png",
                  b"text/plain text/plain", b"\t text/plain\nimage/png\r\v\f",
                  b"a b c d text/plain .", b"\xff", b"x" * 257]
    for index, mimes in enumerate(mime_lists):
        for ending, end in (("bel", b"\x07"), ("st", b"\x1b\\")):
            yield case(f"read/mimes/{index}/{ending}",
                       [packet(b"type=read:loc=primary:id=read", b64(mimes), end)], [text_reply])
    for index, payload in enumerate((None, b"", b"***", b"Zg=", b"Zg", b"Zh==", b"Z g==")):
        yield case(f"read/payload/{index}", [packet(b"type=read", payload)], [text_reply])
    for status in ("success", "denied", "unsupported", "busy", "io_error", "none"):
        yield case("read/status/" + status, [packet(b"type=read:loc=primary:id=status", b64(b"text/plain"))],
                   [{**text_reply, "status": status}])
    for size in (0, 1, 4095, 4096, 4097, 8192, 8193):
        yield case(f"read/chunk/{size}", [packet(b"type=read:id=chunk", b64(b"image/png"))],
                   [{"contents": [content(b"image/png", bytes(i % 256 for i in range(size)))]}])
    for index, available in enumerate(([], [b"x" * 4095], [b"x" * 4096, b"text/plain"],
                                       [b"x" * 4093, b"y"], [f"mime/{i}".encode() for i in range(20)])):
        yield case(f"read/listing/{index}", [packet(b"type=read", b64(b"."))],
                   [{"available": [mime.hex() for mime in available]}])
    yield case("read/reply-order", [packet(b"type=read", b64(b"b a b missing"))], [{"contents": [
        content(b"a", b"one"), content(b"b", b"first"), content(b"b", b"second"), content(b"extra", b"no"),
    ]}])

    for status in ("success", "denied", "unsupported", "busy", "invalid_data", "io_error", "none"):
        yield case("write/status/" + status, [begin, data(b"text/plain", b64(b"hello\x00\xff")), commit],
                   [{"status": status}])
    yield case("write/empty", [begin, commit])
    yield case("write/without-begin", [data(b"text/plain", b64(b"ignored")), commit])
    yield case("write/replace", [begin, data(b"a", b64(b"ignored")), packet(b"type=write:id=replaced"), commit])
    yield case("write/overwrite-mime", [begin, data(b"a", b64(b"one")), data(b"b", b64(b"two")),
                                        data(b"a", b64(b"three")), commit])
    yield case("write/metadata-at-begin", [packet(b"type=write:loc=primary:id=first:name=" + b64(b"writer")),
                                          packet(b"type=wdata:loc=standard:name=" + b64(b"later") + b":mime=" + b64(b"a"), b64(b"data")),
                                          packet(b"type=wdata:loc=standard:id=last", end=b"\x07")])
    encoded = b64(b"arbitrary \x00\xff clipboard")
    for cut in range(len(encoded) + 1):
        yield case(f"write/base64-split/{cut}", [begin, data(b"a", encoded[:cut]), data(b"a", encoded[cut:]), commit])
    for index, payload in enumerate((b"Zg", b"Zg=", b"Zg==", b"Zh==", b"Zg==Zg==", b"***", b"Z g==", b"Zg===", b"Z")):
        for suffix, ending in (("commit", [commit]), ("mime-switch", [data(b"b", b"Yg=="), commit])):
            yield case(f"write/base64-invalid/{index}/{suffix}", [begin, data(b"a", payload), *ending])
    yield case("write/independent-padded-chunks", [begin, data(b"a", b"Zg=="), data(b"a", b"Zw=="), commit])
    yield case("write/reset-during-transfer", [begin, data(b"a", b"Zm"), b"\x1bc", data(b"a", b"8="), commit])
    yield case("write/terminal-reset-during-transfer", [begin, data(b"a", b"Zm"), {"op": "terminal_reset"}, data(b"a", b"8="), commit])

    aliases = [
        [alias(b"a", b"b c")], [alias(b"a", b"b"), alias(b"b", b"c")],
        [alias(b"b", b"c"), alias(b"a", b"b")], [alias(b"missing", b"b")],
        [alias(b"a", b"b"), alias(b"other", b"b")], [alias(b"other", b"a")],
        [alias(b"a", b"a")], [alias(b"a", b"\xff")], [alias(b"a", b"x" * 257)],
        [packet(b"type=walias", b64(b"b"))], [packet(b"type=walias:mime=" + b64(b"a"), b"***")],
    ]
    for index, packets in enumerate(aliases):
        yield case(f"write/alias/{index}", [begin, data(b"a", b64(b"first")), data(b"other", b64(b"second")), *packets, commit])

    for count in (63, 64, 65):
        yield case(f"write/mime-count/{count}", [begin, *[data(f"mime/{i}".encode(), b"eA==") for i in range(count)], commit])
        yield case(f"write/alias-count/{count}", [begin, data(b"a", b"eA=="),
                                                 alias(b"a", b" ".join(f"mime/{i}".encode() for i in range(count))), commit])
    for limit in (0, 1, 2, 3):
        yield case(f"write/limit/{limit}", [begin, data(b"a", b"YWJj"), commit], clipboard_write_limit=limit)
    yield case("write/limit-captured", [begin, {"op": "clipboard_options", "clipboard_write_limit": 0},
                                        data(b"a", b"YWJj"), commit], clipboard_write_limit=3)
    yield case("write/limit-overwrite-spool", [begin, data(b"a", b"YQ=="), data(b"b", b"Yg=="),
                                              data(b"a", b"Yw=="), commit], clipboard_write_limit=2)
    yield case("write/no-callback", [begin, data(b"a", b"YQ=="), commit], clipboard_write_enabled=False)
    yield case("read/no-callback", [packet(b"type=read", b64(b"text/plain"))], clipboard_read_enabled=False)
    yield case("read/no-callback-invalid-payload", [packet(b"type=read", b"***")], clipboard_read_enabled=False)
    yield case("read/no-callback-after-reset", [b"\x1bc" + packet(b"type=read", b64(b"text/plain"))], clipboard_read_enabled=False)
    yield case("read/osc52-no-callback", [b"\x1b]52;c;?\x07"], clipboard_read_enabled=False)
    yield case("write/osc52-no-callback", [b"\x1b]52;c;YQ==\x07"], clipboard_write_enabled=False)
    yield case("write/no-callback-after-reset", [b"\x1bc" + begin + data(b"a", b"YQ==") + commit], clipboard_write_enabled=False)
    yield case("write/callback-removed", [begin, data(b"a", b"YQ=="),
                                          {"op": "clipboard_options", "clipboard_write_enabled": False}, commit])
    yield case("write/callback-added", [begin, {"op": "clipboard_options", "clipboard_write_enabled": True},
                                        data(b"a", b"YQ=="), commit], clipboard_write_enabled=False)
    yield case("write/limit-after-reset", [b"\x1bc" + begin + data(b"a", b"YWJj") + commit], clipboard_write_limit=2)

    credentials = b":name=" + b64(b"program") + b":pw=" + b64(b"session-password")
    read = packet(b"type=read" + credentials, b64(b"text/plain"))
    write = [packet(b"type=write" + credentials), data(b"a", b"eA=="), commit]
    for direction, first, second in (("read", [read], write), ("write", write, [read])):
        yield case("grants/direction/" + direction, [*first, *first, *second, *first, b"\x1bc", *first],
                   [{"remember": True}, {}, {}, {}, {}])
    yield case("grants/listing-exempt", [read, packet(b"type=read" + credentials, b64(b".")), read],
               [{"remember": True}, {}, {}])
    yield case("grants/direct-reset-preserves", [read, {"op": "terminal_reset"}, read], [{"remember": True}, {}])
    yield case("grants/without-name", [read, packet(b"type=read:pw=" + b64(b"session-password"), b64(b"text/plain")), read],
               [{"remember": True}, {}, {}])
    yield case("grants/different-name", [read, packet(b"type=read:name=" + b64(b"other") + b":pw=" + b64(b"session-password"), b64(b"text/plain"))],
               [{"remember": True}, {}])
    for count in (32, 33):
        packets = [packet(b"type=read:name=" + b64(b"program") + b":pw=" + b64(str(i).encode()), b64(b"text/plain")) for i in range(count)]
        yield case(f"grants/capacity/{count}", [*packets, packets[0], packets[-1]],
                   [{"remember": True}] * count + [{}, {}])
