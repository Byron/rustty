#!/usr/bin/env python3
"""Generate Ghostty-compatible properties from its pinned uucode UCD directory.

Usage: python3 tools/generate_unicode.py /path/to/uucode/ucd
The checked-in output makes normal Rust builds independent of Zig and the UCD.
Width policy follows src/build/uucode_config.zig and uucode's Wcwidth component.
"""
from pathlib import Path
import struct
import sys

ucd = Path(sys.argv[1])
count = 0x110000
gc = ["Cn"] * count
gb = ["Other"] * count
ea = ["N"] * count
indic = ["None"] * count
ignored = bytearray(count)
modifier = bytearray(count)
modifier_base = bytearray(count)
pictographic = bytearray(count)
variation = bytearray(count)


def entries(name):
    for line in (ucd / name).read_text().splitlines():
        line = line.split("#")[0].strip()
        if not line:
            continue
        parts = [p.strip() for p in line.split(";")]
        span = parts[0].split("..")
        lo, hi = int(span[0], 16), int(span[-1], 16)
        yield lo, hi + 1, parts[1:]


for lo, hi, parts in entries("extracted/DerivedGeneralCategory.txt"):
    gc[lo:hi] = [parts[0]] * (hi - lo)
for lo, hi, parts in entries("auxiliary/GraphemeBreakProperty.txt"):
    gb[lo:hi] = [parts[0]] * (hi - lo)
# UAX11 defaults for reserved CJK ranges precede explicit assignments.
for line in (ucd / "extracted/DerivedEastAsianWidth.txt").read_text().splitlines():
    if "@missing:" in line:
        span, value = line.split("@missing:")[1].strip().split(";")
        lo, hi = (int(x.strip(), 16) for x in span.split(".."))
        ea[lo:hi + 1] = ["W" if value.strip() == "Wide" else "N"] * (hi - lo + 1)
for lo, hi, parts in entries("extracted/DerivedEastAsianWidth.txt"):
    ea[lo:hi] = [parts[0]] * (hi - lo)
for lo, hi, parts in entries("DerivedCoreProperties.txt"):
    if parts[0] == "Default_Ignorable_Code_Point":
        ignored[lo:hi] = bytes([1]) * (hi - lo)
    elif parts[0] == "InCB":
        indic[lo:hi] = [parts[1]] * (hi - lo)
for lo, hi, parts in entries("emoji/emoji-data.txt"):
    target = {"Emoji_Modifier": modifier, "Emoji_Modifier_Base": modifier_base,
              "Extended_Pictographic": pictographic}.get(parts[0])
    if target is not None:
        target[lo:hi] = bytes([1]) * (hi - lo)
for line in (ucd / "emoji/emoji-variation-sequences.txt").read_text().splitlines():
    line = line.split("#")[0].strip()
    if line:
        variation[int(line.split()[0], 16)] = 1

names = ["Other", "Prepend", "Regional_Indicator", "SpacingMark", "L", "V", "T", "LV", "LVT", "ZWJ", "ZWNJ", "Pictographic", "ModifierBase", "Modifier", "IndicExtend", "IndicLinker", "IndicConsonant"]
out = bytearray()
values = bytearray()
previous = None
for cp in range(count):
    prop = gb[cp]
    if modifier[cp]: prop = "Modifier"
    elif modifier_base[cp]: prop = "ModifierBase"
    elif pictographic[cp]: prop = "Pictographic"
    elif indic[cp] == "Extend": prop = "ZWJ" if cp == 0x200D else "IndicExtend"
    elif indic[cp] == "Linker": prop = "IndicLinker"
    elif indic[cp] == "Consonant": prop = "IndicConsonant"
    elif prop == "Extend":
        assert cp == 0x200C, hex(cp)
        prop = "ZWNJ"
    if gc[cp] in ("Cc", "Cs", "Zl", "Zp"): width = 0
    elif cp == 0xAD: width = 1
    elif ignored[cp]: width = 0
    elif cp == 0x2E3A: width = 2
    elif cp == 0x2E3B: width = 3
    elif ea[cp] in ("W", "F") or prop == "Regional_Indicator": width = 2
    else: width = 1
    zero = width == 0 or modifier[cp] or gc[cp] in ("Mn", "Me") or prop in ("V", "T", "Prepend")
    standalone = 2 if cp == 0x20E3 else width
    width = 0 if zero and not modifier[cp] and prop != "Prepend" else min(2, standalone)
    if prop in ("Control", "CR", "LF"): prop = "Other"
    value = width | (int(bool(zero)) << 2) | (names.index(prop) << 3) | (variation[cp] << 8)
    values += struct.pack("<H", value)
    if value != previous:
        out += struct.pack("<IH", cp, value)
        previous = value

# Runtime lookup uses one byte per 128-scalar block, followed by deduplicated
# blocks of little-endian u16 properties. Keep the ranges as a test-only oracle.
block_size = 128
index = bytearray()
blocks = {}
for offset in range(0, len(values), block_size * 2):
    block = bytes(values[offset:offset + block_size * 2])
    block_id = blocks.setdefault(block, len(blocks))
    assert block_id < 256, "Unicode table needs wider block indices"
    index.append(block_id)
table = index + b"".join(blocks)
assert len(index) == count // block_size
for cp in range(count):
    offset = len(index) + (table[cp // block_size] * block_size + cp % block_size) * 2
    assert table[offset:offset + 2] == values[cp * 2:cp * 2 + 2]

dest = Path(__file__).resolve().parents[1] / "src" / "unicode.bin"
dest.write_bytes(out)
print(f"{len(out) // 6} property ranges, {len(out)} bytes: {dest}")
dest = dest.with_name("unicode_table.bin")
dest.write_bytes(table)
print(f"{len(blocks)} property blocks, {len(table)} bytes: {dest}")
