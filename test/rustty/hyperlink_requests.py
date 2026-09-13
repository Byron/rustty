"""Native PAGE LINK admission, string allocation, deduplication and cell limits."""
import struct
import snapshots


def link(wire, uri=b"uri", identifier=1):
    return wire, identifier, uri


def encode_link(entry):
    wire, identifier, uri = entry
    data = struct.pack("<H", wire)
    if isinstance(identifier, int):
        data += struct.pack("<BI", 1, identifier)
    else:
        data += struct.pack("<BI", 2, len(identifier)) + identifier
    return data + struct.pack("<I", len(uri)) + uri


def native_hash(identifier, uri):
    """Pinned PageEntry hash; collision cases verify it through native admission."""
    if isinstance(identifier, int):
        data = b"\x01" + struct.pack("<I", identifier)
    else:
        data = b"\x00" + identifier + struct.pack("<Q", len(identifier))
    data += uri + struct.pack("<Q", len(uri))
    secret = (0xa0761d6478bd642f, 0xe7037ed1a0b428db, 0x8ebc6af09c88c6e3, 0x589965cc75374cc3)
    mask = (1 << 64) - 1

    def mix(a, b):
        product = a * b
        return (product & mask) ^ (product >> 64)

    def read(start, size):
        return int.from_bytes(data[start:start + size], "little")

    state = [mix(secret[0], secret[1])] * 3
    length = len(data)
    if length <= 16:
        if length >= 4:
            end, quarter = length - 4, (length >> 3) << 2
            a = (read(0, 4) << 32) | read(quarter, 4)
            b = (read(end, 4) << 32) | read(end - quarter, 4)
        elif length:
            a, b = (data[0] << 16) | (data[length >> 1] << 8) | data[-1], 0
        else:
            a = b = 0
    else:
        offset = 0
        if length >= 48:
            while offset + 48 < length:
                for i in range(3):
                    state[i] = mix(read(offset + 16 * i, 8) ^ secret[i + 1],
                                   read(offset + 16 * i + 8, 8) ^ state[i])
                offset += 48
            state[0] ^= state[1] ^ state[2]
        while offset + 16 < length:
            state[0] = mix(read(offset, 8) ^ secret[1], read(offset + 8, 8) ^ state[0])
            offset += 16
        a, b = read(length - 16, 8), read(length - 8, 8)
    product = (a ^ secret[1]) * (b ^ state[0])
    return mix((product & mask) ^ secret[0] ^ length, (product >> 64) ^ secret[1])


def requests(reference):
    templates = {}
    reference_limits = []

    def case(name, entries, references, table=192, strings=2048, columns=80, raw=None):
        if columns not in templates:
            encoded = reference.request({"id": f"link/source/{columns}", "cols": columns, "rows": 1,
                                         "operations": [{"op": "snapshot"}]})
            if not encoded["ok"] or len(encoded["snapshots"]) != 1:
                raise RuntimeError("native hyperlink resource source failed")
            templates[columns] = snapshots.records(bytes.fromhex(encoded["snapshots"][0]))
        page = bytearray(struct.pack("<HHHHHHII", columns, 1, 0, len(entries), 0, table, 0, strings))
        page += b"".join(encode_link(entry) for entry in entries) if raw is None else raw
        page += struct.pack("<BH", 3 << 4, columns)
        for index in range(columns):
            wire = references[index] if index < len(references) else 0
            page += struct.pack("<Q", (ord("A") << 2) | (wire << 48))
        page += struct.pack("<I", 0)
        parts = [(tag, page if tag == 3 else data) for tag, data in templates[columns]]
        return ({"id": "snapshot/resources/hyperlinks/" + name,
                 "operations": [{"op": "restore", "data": snapshots.frame(parts).hex()}, {"op": "observe"}]},
                ["snapshot.cross-decode", "terminal.cells"])

    for table in (0, 47, 48, 49, 95, 96, 143, 144, 191, 192, 193, 240, 384, 768, 1536, 3072):
        count = table // 48
        slots = 1 << (count - 1).bit_length() if count else 0
        live = max(0, slots * 13 // 16 - 1)
        entries = [link(i + 1, b"uri", i) for i in range(live + 3)]
        yield case(f"table/{table}", entries, list(range(1, live + 4)), table=table, strings=8192)
    for strings in (0, 1, 32, 2048, 2049, 4096):
        for length in (1, 31, 32, 33, 63, 64, 65, 992, 1024, 1984, 2016, 2048, 2049, 2080):
            for identifier in (1, b"id"):
                name = f"strings/{strings}/{length}/{type(identifier).__name__}"
                request, covers = case(name, [link(1, b"x" * length, identifier), link(2, b"last", 2)],
                                       [1, 2], strings=strings)
                if 0 < strings <= 2048 and length > 2048:
                    # The native large-span scan reads a second bitmap word
                    # when this page has only one. Keep the abort visible,
                    # after the returning cases so they can still be checked.
                    request["id"] = "snapshot/reference-limit/hyperlinks/" + name
                    reference_limits.append((request, covers))
                else:
                    yield request, covers
    for table in (144, 192, 240, 384, 768):
        yield case(f"cell-map/{table}", [link(1)], [1] * 512, table=table, columns=512)
        yield case(f"cell-map-sparse/{table}", [link(1)], [0, 1, 999, 1] * 128,
                   table=table, columns=512)

    for name, entries, references in (
        ("duplicate-value", [link(1), link(2), link(3)], [1, 2, 3]),
        ("identity", [link(1), link(2, identifier=2), link(3, identifier=b"1"), link(4, identifier=b"1")], [1, 2, 3, 4]),
        ("same-id-different-uri", [link(1), link(2, b"other"), link(3)], [1, 2, 3]),
        ("duplicate-wire", [link(1), link(1, b"ignored", 2), link(2), link(3, b"third", 3)], [1, 2, 3]),
        ("zero-wire", [link(0), link(1), link(0, b"other", 2), link(2, b"third", 3)], [0, 1, 2]),
        ("empty-uri-first-wins", [link(1, b""), link(1), link(2)], [1, 2]),
        ("empty-explicit-first-wins", [link(1, identifier=b""), link(1), link(2)], [1, 2]),
        ("raw-bytes", [link(1, b"a\x00\xffb", b"id\x00\xff"), link(2, b"a\x00\xffb", b"id\x00\xff")], [1, 2]),
        ("implicit-u32", [link(1, identifier=0), link(2, identifier=0xffffffff)], [1, 2]),
        ("discarded-tail-starves-pool", [link(0, b"x" * 2048), link(1, b"a"), link(2, b"b")], [1, 2]),
        ("discarded-tail-can-be-reclaimed", [link(0, b"x" * 1984), link(1, b"a", b"id"), link(2, b"b")], [1, 2]),
        ("duplicate-needs-temporary-space", [link(1, b"x" * 2016), link(2, b"x" * 2016), link(3, b"b", 2)], [1, 2, 3]),
        ("duplicate-frees-temporary-space", [link(1, b"x" * 992), link(2, b"x" * 992), link(3, b"b" * 1024, 2)], [1, 2, 3]),
        ("failed-uri-frees-explicit-id", [link(1, b"x" * 1984), link(2, b"x" * 65, b"id"), link(3, b"b" * 64, 2)], [1, 2, 3]),
        ("rejected-table-frees-strings", [link(1), link(2, b"b", 2), link(3, b"x" * 1984, 3), link(4)], [1, 2, 3, 4]),
        ("empty-uri-releases-explicit", [link(1, b"", b"x" * 2048), link(2, b"b" * 2048, 2)], [1, 2]),
        ("unused-entry-retains-strings", [link(1, b"x" * 2048), link(2, b"b")], [2]),
    ):
        yield case(name, entries, references)

    def collisions(bucket, explicit=False):
        values = []
        number = 0
        while len(values) < 33:
            identifier = str(number).encode() if explicit else number
            uri = b"collision"
            if native_hash(identifier, uri) & 63 == bucket:
                values.append((identifier, uri))
            number += 1
        return values

    for bucket in (0, 63):
        for explicit in (False, True):
            values = collisions(bucket, explicit)
            entries = [link(i + 1, uri, identifier) for i, (identifier, uri) in enumerate(values)]
            entries += [link(34 + i, uri, identifier) for i, (identifier, uri) in enumerate(values[:32])]
            yield case(f"collision/{bucket}/{explicit}", entries, list(range(1, 66)), table=64 * 48, strings=8192)
            discarded = [link(i + 1, uri, identifier) for i, (identifier, uri) in enumerate(values[:31])]
            discarded += [link(0, values[31][1], values[31][0]), link(32, values[32][1], values[32][0])]
            yield case(f"reclaim-collision-limit/{bucket}/{explicit}", discarded, list(range(1, 33)),
                       table=64 * 48, strings=8192)
    for kind in (0, 3, 255):
        request, covers = case(f"invalid-kind/{kind}", [link(0)], [], raw=struct.pack("<HB", 0, kind))
        request["expected_error"] = "InvalidSnapshot"
        yield request, covers
    yield from reference_limits
