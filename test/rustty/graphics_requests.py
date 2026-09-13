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
