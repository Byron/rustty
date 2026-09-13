"""Native PAGE resource exhaustion, hash collisions, and grapheme boundaries."""
import struct

import snapshots


def _style_value(number, channel=0, flags=0):
    value = bytearray(16)
    value[channel * 4:channel * 4 + 4] = bytes((2, number & 255,
                                               (number >> 8) & 255, (number >> 16) & 255))
    struct.pack_into("<H", value, 12, flags)
    return bytes(value)


def _style_hash(value):
    # terminal/style.zig: PackedStyle tags precede all three 24-bit values;
    # fold its two u64 halves and apply Zig's std.hash.int(u64).
    packed = int.from_bytes(value[12:14], "little") << 96
    for channel in range(3):
        color = value[channel * 4:channel * 4 + 4]
        packed |= color[0] << (channel * 8)
        packed |= int.from_bytes(color[1:], "little") << (24 + channel * 24)
    mask = (1 << 64) - 1
    folded = (packed & mask) ^ (packed >> 64)
    for shift in (32, 29, 32):
        folded = ((folded ^ (folded >> shift)) * 0xbea225f9eb34556d) & mask
    return folded ^ (folded >> 29)


def style_requests(reference):
    encoded = reference.request({"id": "snapshot/resources/styles/source", "cols": 80, "rows": 1,
                                 "operations": [{"op": "snapshot"}]})
    if not encoded["ok"] or len(encoded["snapshots"]) != 1:
        raise RuntimeError("reference could not encode style resource fixture")
    template = snapshots.records(bytes.fromhex(encoded["snapshots"][0]))

    def case(name, capacity, entries, references):
        page = bytearray(struct.pack("<HHHHHHII", 80, 1, len(entries), 0, capacity, 0, 0, 0))
        for wire_id, value in entries:
            page.extend(struct.pack("<H", wire_id) + value)
        page.extend(struct.pack("<BH", 3 << 4, 80))
        for wire_id in references + [0] * (80 - len(references)):
            page.extend(struct.pack("<Q", (ord("A") << 2) | (wire_id << 26)))
        page.extend(struct.pack("<I", 0))
        parts = [(tag, page if tag == 3 else data) for tag, data in template]
        return ({"id": "snapshot/resources/styles/" + name, "operations": [
            {"op": "restore", "data": snapshots.frame(parts).hex()}, {"op": "observe"},
        ]}, ["snapshot.cross-decode", "terminal.cells"])

    for capacity in (0, 1, 2, 3, 4, 7, 8, 9, 16, 31, 32, 33, 64, 128, 256):
        table = 1 << (capacity - 1).bit_length() if capacity else 0
        live = max(0, table * 13 // 16 - 1)
        entries = [(i + 1, _style_value(i + 1)) for i in range(live + 3)]
        yield case(f"capacity/{capacity}", capacity, entries,
                   list(range(1, min(10, live + 3) + 1)) + [max(1, live), live + 1, live + 2, live + 3])

    a, b, c = (_style_value(i) for i in (1, 2, 3))
    yield case("duplicate-values", 4, [(1, a), (2, a), (3, b), (4, c), (5, a), (6, b)],
               list(range(1, 7)))
    yield case("ignored-wire-ids", 4, [(0, c), (1, a), (1, c), (2, b), (3, c)], [0, 1, 2, 3])
    yield case("unused-entries-retain-capacity", 4, [(1, a), (2, b), (3, c), (4, a)], [3, 4])
    yield case("rejected-first-id-wins", 4, [(1, a), (2, b), (3, c), (3, a), (4, a)], [1, 2, 3, 4])

    malformed = []
    for channel in range(3):
        for color in ((0, 1, 0, 0), (1, 1, 1, 0), (1, 1, 0, 1), (3, 0, 0, 0)):
            value = bytearray(16)
            value[channel * 4:channel * 4 + 4] = bytes(color)
            malformed.append(bytes(value))
    for flags in (0x600, 0x700, 0x800, 0x8000):
        value = bytearray(16)
        struct.pack_into("<H", value, 12, flags)
        malformed.append(bytes(value))
    for padding in (14, 15):
        value = bytearray(16)
        value[padding] = 1
        malformed.append(bytes(value))
    entries = []
    for wire_id, value in enumerate([bytes(16)] + malformed, 1):
        entries.extend(((wire_id, value), (wire_id, c)))
    first_valid = len(malformed) + 2
    entries.extend(((first_valid, a), (first_valid + 1, b), (first_valid + 2, c)))
    yield case("invalid-and-default-first-id-wins", 4, entries, list(range(1, first_valid + 3)))

    def colliding(mask, bucket, count, value_for):
        result = []
        number = 0
        while len(result) < count:
            value = value_for(number)
            if _style_hash(value) & mask == bucket:
                result.append(value)
            number += 1
        return result

    # Every family has 32 entries at one home bucket, which reaches PSL 31
    # while the set still has capacity. Another home bucket must fail too;
    # repeated values must still be found, even after the global PSL limit.
    families = [(f"rgb-channel-{channel}",
                 lambda n, channel=channel: _style_value((n * 0x9e3779) & 0xffffff, channel))
                for channel in range(3)]
    families += [(f"flag-{flags:03x}", lambda n, flags=flags: _style_value(n, flags=flags))
                 for flags in [1 << bit for bit in range(8)] + [0x100, 0x200, 0x300, 0x400, 0x500, 0x5ff]]

    def mixed_tags(number):
        value = bytearray(16)
        for channel in range(3):
            tag = (number + channel) % 3
            rgb = (number * (0x9e3779 + channel * 2)) & 0xffffff
            value[channel * 4:channel * 4 + 4] = bytes((tag, rgb & 255 if tag else 0,
                                                       (rgb >> 8) & 255 if tag == 2 else 0,
                                                       (rgb >> 16) & 255 if tag == 2 else 0))
        struct.pack_into("<H", value, 12, number % 0x600)
        return bytes(value)

    families.append(("mixed-tags-flags", mixed_tags))
    for name, value_for in families:
        for bucket in (0, 63):
            values = colliding(63, bucket, 33, value_for)
            unrelated = colliding(63, (bucket + 40) & 63, 1, value_for)[0]
            entries = list(enumerate(values, 1)) + [(34, unrelated)]
            entries += [(35 + i, value) for i, value in enumerate(reversed(values[:32]))]
            yield case(f"collision/{name}/{bucket}", 64, entries, list(range(1, 67)))

    # Refcount ties occur after a busy bucket's entry is displaced. Different
    # wire IDs raise its refcount; repeating one wire ID must not do so.
    home_zero = colliding(15, 0, 2, _style_value)
    home_one = colliding(15, 1, 2, _style_value)
    entries = [(1, home_zero[0])] + [(i, home_one[0]) for i in range(2, 7)]
    entries += [(2, home_zero[1]), (7, home_one[1]), (8, home_zero[1])]
    yield case("reference-count-displacement-ties", 16, entries, list(range(1, 9)))


def requests(reference):
    encoded = reference.request({"id": "snapshot/resources/source", "cols": 80, "rows": 1,
                                 "operations": [{"op": "snapshot"}]})
    if not encoded["ok"] or len(encoded["snapshots"]) != 1:
        raise RuntimeError("reference could not encode resource fixture")
    template = snapshots.records(bytes.fromhex(encoded["snapshots"][0]))

    def case(name, capacity, entries):
        # Eight-bit bare codepoints, one active row, no styles or hyperlinks.
        page = bytearray(struct.pack("<HHHHHHII", 80, 1, 0, 0, 0, 0, capacity, 0))
        page.extend(struct.pack("<BH", 0, 80) + b"A" * 80)
        page.extend(struct.pack("<I", len(entries)))
        for row, col, suffix in entries:
            page.extend(struct.pack("<HHH", row, col, len(suffix)))
            page.extend(struct.pack(f"<{len(suffix)}I", *suffix))
        parts = [(tag, page if tag == 3 else data) for tag, data in template]
        return ({"id": "snapshot/resources/graphemes/" + name, "operations": [
            {"op": "restore", "data": snapshots.frame(parts).hex()},
            {"op": "observe"},
        ]}, ["snapshot.cross-decode", "terminal.cells"])

    for capacity in (0, 1, 16, 17, 32, 48, 64, 128, 256, 512, 1024, 2048, 8192):
        for length in (1, 4, 5, 8, 9, 16, 17, 32, 33, 64, 65, 128):
            entries = [(0, col, [0x300 + i for i in range(length)]) for col in range(4)]
            yield case(f"capacity/{capacity}/{length}", capacity, entries)
        yield case(f"map/{capacity}", capacity, [(0, col, [0x301]) for col in range(80)])
    yield case("mid-cluster", 512, [(0, col, list(range(0x300, 0x300 + count)))
                                    for col, count in enumerate((64, 64, 8, 61))])
    yield case("retry-failed", 512, [(0, col, [0x301] * count)
                                     for col, count in ((0, 64), (1, 64), (2, 8),
                                                        (3, 61), (3, 1), (3, 4))])
    yield case("invalid-and-duplicate", 64, [
        (1, 0, [0x301]), (0, 80, [0x301]),
        (0, 0, [0, 0xD800, 0x110000]), (0, 0, [0x301, 0, 0xD800, 0x302]),
        (0, 0, [0x303]), (0, 1, [0x301] * 65),
    ])
