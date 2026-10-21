"""Bounded tmuxx v2 frames for the independent Python conformance peers."""
import struct

import lz4.block
import msgpack

BLOCK_BYTES = 256 * 1024
RECORD_BYTES = 64 * 1024 * 1024
HEADER = struct.Struct("<4sB3sII")
FINAL, COMPRESSED = 1, 2


def encode(record):
    payload = msgpack.packb(record, use_bin_type=True)
    if len(payload) > RECORD_BYTES:
        raise ValueError("native record exceeds limit")
    frames = bytearray()
    for offset in range(0, len(payload), BLOCK_BYTES):
        block = payload[offset:offset + BLOCK_BYTES]
        compressed = lz4.block.compress(block, store_size=False)
        flags = FINAL if offset + len(block) == len(payload) else 0
        data = block
        if len(compressed) < len(block):
            flags |= COMPRESSED
            data = compressed
        frames.extend(HEADER.pack(b"TMX2", flags, b"\0\0\0", len(data), len(block)))
        frames.extend(data)
    return bytes(frames)


def read_record(read, max_bytes=RECORD_BYTES):
    def exact(size):
        result = bytearray()
        while len(result) < size:
            data = read(size - len(result))
            if not data:
                raise EOFError("native stream ended inside a record")
            result.extend(data)
        return result

    payload = bytearray()
    while True:
        magic, flags, reserved, encoded, decoded = HEADER.unpack(exact(HEADER.size))
        if magic != b"TMX2" or flags & ~(FINAL | COMPRESSED) or reserved != b"\0\0\0":
            raise ValueError("invalid native frame header")
        if not 0 < encoded <= BLOCK_BYTES or not 0 < decoded <= BLOCK_BYTES:
            raise ValueError("native block exceeds limit")
        if (flags & COMPRESSED and encoded >= decoded) or (not flags & COMPRESSED and encoded != decoded):
            raise ValueError("invalid native block lengths")
        if not flags & FINAL and decoded != BLOCK_BYTES:
            raise ValueError("nonfinal native block must be full")
        if decoded > max_bytes - len(payload):
            raise ValueError("native record exceeds limit")
        block = exact(encoded)
        if flags & COMPRESSED:
            block = lz4.block.decompress(block, uncompressed_size=decoded)
            if len(block) != decoded:
                raise ValueError("native decompressed length mismatch")
        payload.extend(block)
        if flags & FINAL:
            return msgpack.unpackb(payload, raw=False, strict_map_key=False)
