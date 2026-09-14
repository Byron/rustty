"""Kitty image uploads decoded by the real native and Rust PNG implementations."""
import base64
import struct
import zlib


def png(width, height, depth, color, rows, palette=None, transparency=None, interlaced=False):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    result = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, depth, color, 0, 0, int(interlaced)))
    if palette is not None:
        result += chunk(b"PLTE", palette)
    if transparency is not None:
        result += chunk(b"tRNS", transparency)
    return result + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")


def requests():
    def upload(data, options=b"f=100,a=t,i=1"):
        return b"\x1b_G" + options + b";" + base64.b64encode(data) + b"\x1b\\"

    def case(name, data):
        return ({"id": "protocol/graphics/" + name, "observe_graphics": True,
                 "operations": [{"op": "write", "data": data.hex()}]},
                ["graphics.kitty", "graphics.png", "effects.pty"])

    samples = []
    for depth in (1, 2, 4, 8, 16):
        row = {1: b"\x40", 2: b"\x30", 4: b"\x0f", 8: b"\x13\xcd", 16: b"\x12\x34\xab\xcd"}[depth]
        samples.append((f"gray-{depth}", png(2, 1, depth, 0, b"\0" + row)))
        samples.append((f"gray-trns-{depth}", png(2, 1, depth, 0, b"\0" + row, transparency=b"\0\0")))
    for color, channels in ((2, 3), (4, 2), (6, 4)):
        for depth in (8, 16):
            size = 2 * channels * depth // 8
            raw = bytes((i * 41 + 17) % 256 for i in range(size))
            samples.append((f"color-{color}-{depth}", png(2, 1, depth, color, b"\0" + raw)))
    palette = bytes([1, 2, 3, 40, 50, 60, 128, 129, 130, 253, 254, 255])
    for depth, row in ((1, b"\x40"), (2, b"\x30"), (4, b"\x03"), (8, b"\0\x03")):
        for alpha in (None, b"\0\x01\x7f\xff"):
            samples.append((f"indexed-{depth}-{alpha is not None}", png(2, 1, depth, 3, b"\0" + row,
                palette=palette[:6] if depth == 1 else palette,
                transparency=alpha[:2] if alpha and depth == 1 else alpha)))
    samples.append(("rgba-alpha-extremes", png(3, 1, 8, 6, b"\0\x01\x02\x03\0\x04\x05\x06\x7f\x07\x08\x09\xff")))
    for width, height in ((1, 1), (2, 3), (8, 8), (9, 9)):
        rows = b""
        for x0, y0, dx, dy in ((0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8),
                               (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)):
            if x0 >= width:
                continue
            for y in range(y0, height, dy):
                rows += b"\0" + bytes(value for x in range(x0, width, dx)
                                       for value in (x * 17, y * 19, (x + y) * 13, 255))
        samples.append((f"adam7-{width}-{height}", png(width, height, 8, 6, rows, interlaced=True)))
    for name, data in samples:
        yield case("png/" + name, upload(data))
        yield case("png-zlib/" + name, upload(zlib.compress(data), b"f=100,a=t,i=1,o=z"))
    # Every truncation retains real image/chunk bytes; neither decoder receives
    # invented trailers. Small Adam7 images exercise empty passes too.
    for name, data in (samples[0], samples[-1]):
        for length in range(len(data)):
            yield case(f"png-truncated/{name}/{length}", upload(data[:length]))
        for kind in (b"IHDR", b"IDAT", b"IEND"):
            corrupted = bytearray(data)
            start = data.index(kind)
            length = struct.unpack(">I", data[start - 4:start])[0]
            corrupted[start + 4 + length] ^= 1
            yield case(f"png-crc/{name}/{kind.decode()}", upload(bytes(corrupted)))
    data = samples[-1][1]
    for index, payload in enumerate((b"", b"not png", data[:8], data[:32], data[:-12], data + b"tail", b"prefix" + data)):
        yield case(f"png-invalid/{index}", upload(payload))
    corrupted = bytearray(data)
    corrupted[29] ^= 1
    yield case("png-invalid/ihdr-crc", upload(bytes(corrupted)))
    yield case("png/dimensions-from-image", upload(data, b"f=100,a=t,i=1,s=1,v=1"))
    for size in (10000, 10001, 0xffffffff):
        yield case(f"png-dimension-limit/{size}", upload(png(size, 1, 8, 6, b"")))
    yield case("png-dimension-limit/valid-10001", upload(png(10001, 1, 8, 6, b"\0" * 40005)))
    for format_, channels in ((24, 3), (32, 4)):
        raw = bytes(range(2 * channels))
        yield case(f"raw/{format_}", upload(raw, f"f={format_},a=t,i=1,s=2,v=1".encode()))
    yield case("png/both-screens", upload(data) + b"\x1b[?1049h" + upload(samples[0][1]) + b"\x1b[?1049l")

    def raw(options=b"", data=b"\x01\x02\x03\xff"):
        return upload(data, b"f=32,s=1,v=1" + (b"," + options if options else b""))

    identities = {
        "implicit": raw() * 3,
        "numbered": raw(b"I=7") * 3,
        "collision": raw(b"i=2147483647") + raw(b"i=2147483648") + raw() * 2,
        "number-hole": raw(b"i=1") + raw(b"i=3") + raw(b"I=7")
            + b"\x1b_Ga=d,d=I,i=2\x1b\\" + raw(b"I=8") + raw(),
        "implicit-delete": raw() + b"\x1b_Ga=d,d=I,i=2147483647\x1b\\" + raw(),
        "failed-data": raw(data=b"\x01") + raw(),
        "failed-format": raw(b"f=99") + raw(),
        "failed-dimensions": raw(b"s=0") + raw(),
        "number-failed": raw(b"I=9", b"\x01") + raw(b"I=9") + raw(),
        "chunks": raw(b"m=1", b"\x01\x02") + raw(b"a=q,i=19")
            + b"\x1b_Gm=0;A/8=\x1b\\" + raw(),
        "number-chunks": raw(b"i=1") + raw(b"I=9,m=1", b"\x01\x02")
            + b"\x1b_Ga=d,d=I,i=1\x1b\\" + b"\x1b_Gm=0;A/8=\x1b\\" + raw(b"I=9"),
        "failed-chunks": raw(b"m=1", b"\x01") + b"\x1b_Gm=0;\x1b\\" + raw(),
        "query": raw(b"a=q,i=123") + raw(),
        "implicit-display-error": raw(b"a=T,P=123") + raw(),
    }
    for alternate in (False, True):
        for name, data in identities.items():
            prefix = b"\x1b[?1049h" if alternate else b""
            request, _ = case(f"ids/{alternate}/{name}", prefix + data)
            yield request, ["graphics.kitty", "effects.pty"]

    def animation(name, steps, alternate=False):
        operations = []
        if alternate:
            operations.append({"op": "write", "data": b"\x1b[?1049h".hex()})
        for step in steps:
            operations.append({"op": "write", "data": step.hex()} if isinstance(step, bytes)
                              else {"op": "graphics_tick", "now_ms": step})
            operations.append({"op": "observe"})
        return ({"id": "protocol/graphics/animation/" + name, "kind": "input",
                 "observe_graphics": True, "operations": operations}, ["graphics.kitty", "effects.pty"])

    def control(options):
        return b"\x1b_Ga=a,i=1," + options + b"\x1b\\"

    def frames(gaps, placed=True):
        data = raw(b"i=1,C=1,a=" + (b"T" if placed else b"t"))
        for index, gap in enumerate(gaps[1:], 1):
            data += raw(f"a=f,i=1,z={gap or -1}".encode(), bytes([index * 30, 2, 3, 255]))
        return data + control(f"r=1,z={gaps[0] or -1}".encode())

    for alternate in (False, True):
        for state in (1, 2, 3):
            for gaps in ((50,), (0,), (50, 50), (0, 40), (40, 0), (0, 0), (40, 0, 50), (0, 40, 0), (40, 0, 0)):
                setup = frames(gaps) + control(f"s={state}".encode())
                yield animation(f"clock/{alternate}/{state}/{gaps}",
                                [setup, 0, 1, 39, 40, 49, 50, 90, 100, 10000], alternate)
        for loops in (1, 2, 3):
            for gaps in ((10, 40), (10, 0, 0), (0, 40, 0)):
                yield animation(f"loops/{alternate}/{loops}/{gaps}",
                                [frames(gaps) + control(f"s=3,v={loops}".encode()),
                                 0, 10, 50, 60, 100, 110, 500], alternate)
        for name, times in (("restart", [100, 125, 5, 44, 45]),
                            ("saturation", [2**64 - 20, 2**64 - 1, 2**64 - 1])):
            yield animation(f"{name}/{alternate}", [frames((40, 50)) + control(b"s=3"), *times], alternate)

    yield animation("unplaced", [frames((10, 40), placed=False) + control(b"s=3"), 0, 100,
                    b"\x1b_Ga=p,i=1,C=1\x1b\\", 100, 110,
                    b"\x1b_Ga=d,d=i,i=1\x1b\\", 150, 1000,
                    b"\x1b_Ga=p,i=1,C=1\x1b\\", 1010, 1020, b"\x1bc", 2000])
    yield animation("stop-resume", [frames((40, 50)) + control(b"s=3"), 100, 120,
                    control(b"s=1"), 200, control(b"s=3"), 250, 289, 290])
    yield animation("loading-append", [frames((10, 40)) + control(b"s=2"), 0, 10, 50, 100,
                    raw(b"a=f,i=1,z=25", b"\xff\xff\xff\xff"), 150, 175, 200])
    for options in (b"c=1", b"c=2", b"s=2", b"s=3", b"s=1", b"r=1,z=80", b"r=1,z=-1", b"v=2", b"s=99,c=99"):
        yield animation("control/" + options.decode(), [frames((40, 50)) + control(b"s=3"),
                        100, 120, control(options), 140, 150, 190, 200])
    yield animation("both-screens", [frames((10, 40)) + control(b"s=3"), 100,
                    b"\x1b[?1049h" + frames((20, 50)) + control(b"s=3"), 110, 130,
                    b"\x1b[?1049l", 150, b"\x1b[?1049h", 160])
    yield animation("multiple-images", [frames((10, 40)) + control(b"s=3"),
                    raw(b"i=2,a=T,C=1") + raw(b"a=f,i=2,z=25")
                    + b"\x1b_Ga=a,i=2,r=1,z=20,s=3\x1b\\", 0, 10, 20, 45, 50, 70])

    # A few lifecycle transitions, rather than another parameter cross-product.
    for name, current, removed in (("root", 1, 0), ("current", 2, 2),
                                   ("earlier", 3, 2), ("later", 1, 2),
                                   ("root-before-current", 2, 1), ("past-end", 3, 99)):
        yield animation("lifecycle/delete/" + name,
                        [frames((20, 30, 40)) + control(f"c={current},s=3".encode()), 100,
                         f"\x1b_Ga=d,d=f,i=1,r={removed}\x1b\\".encode(), 110, 150])
    yield animation("lifecycle/delete/plain", [raw(b"i=1,a=T,C=1"),
                    b"\x1b_Ga=d,d=f,i=1\x1b\\", b"\x1b_Ga=d,d=F,i=1\x1b\\", 100])

    base = upload(bytes([255, 0, 0, 255, 0, 0, 255, 255]), b"a=T,i=1,f=32,s=2,v=1,C=1")
    green = raw(b"a=f,i=1,c=1,x=1,z=30", bytes([0, 255, 0, 128]))
    yield animation("lifecycle/edit-and-compose", [base, green, control(b"c=2,s=3"), 100,
                    raw(b"a=f,i=1,r=2,x=1,X=1", bytes([255, 255, 255, 255])), 110,
                    b"\x1b_Ga=c,i=1,r=1,c=2,w=1,h=1,x=1,C=1\x1b\\", 120,
                    b"\x1b_Ga=c,i=1,r=2,c=1,w=1,h=1,X=1,C=0\x1b\\", control(b"c=1")])
    yield animation("lifecycle/compose-errors", [base, green,
                    b"\x1b_Ga=c,i=1,r=9,c=1\x1b\\", b"\x1b_Ga=c,i=1,r=1,c=9\x1b\\",
                    b"\x1b_Ga=c,i=1,r=1,c=2,w=1,x=2\x1b\\",
                    b"\x1b_Ga=c,i=1,r=1,c=2,w=1,X=2\x1b\\",
                    b"\x1b_Ga=c,i=1,r=1,c=1,w=1\x1b\\",
                    b"\x1b_Ga=c,i=1,r=1,c=1,w=1,x=1,C=1\x1b\\"])
    yield animation("lifecycle/frame-formats", [base,
                    upload(png(1, 1, 8, 6, b"\0\x01\x02\x03\xff"), b"a=f,i=1,f=100"),
                    upload(zlib.compress(bytes([4, 5, 6])), b"a=f,i=1,f=24,s=1,v=1,o=z"),
                    raw(b"a=f,i=1,r=2", bytes([7, 8, 9, 255, 99])), control(b"c=2"),
                    raw(b"a=f,i=1,r=2", bytes([10, 11, 12])),
                    raw(b"a=q,i=1", bytes([7, 8, 9, 255, 99]))])
    start = raw(b"a=f,i=1,m=1,z=25", bytes([1, 2]))
    end = b"\x1b_Gm=0;A/8=\x1b\\"
    yield animation("lifecycle/chunks", [base, start, end, control(b"c=2"),
                    start, b"\x1b_Ga=d,d=I,i=1\x1b\\", end, base])
    yield animation("lifecycle/chunks-during-playback", [frames((10, 20)) + control(b"s=3"),
                    100, start, 110, end, 130])
    yield animation("lifecycle/retransmit", [frames((10, 20)) + control(b"c=2,s=3"), 100,
                    raw(b"i=1,a=T,C=1", bytes([77, 88, 99, 255])), 200,
                    b"\x1b[?1049h" + raw(b"i=1,a=T,C=1"), b"\x1b[?1049l", b"\x1bc", 300])
