#!/usr/bin/env python3
"""Classify saved parity failures; never changes requests or runner results."""
import argparse
from collections import Counter
import json
from pathlib import Path
import struct


def without_pointer_shapes(text):
    data = bytes.fromhex(text)
    if data[:10] != b"GHOSTSNP\x01\x00":
        raise ValueError("unexpected snapshot envelope")
    out = bytearray(data[:10])
    tags = []
    offset = 10
    while offset < len(data):
        if len(data) - offset < 10:
            raise ValueError("truncated snapshot record header")
        tag, length = struct.unpack_from("<HI", data, offset)
        end = offset + 10 + length
        if end > len(data):
            raise ValueError("truncated snapshot record")
        if tag == 0x8001:
            tags.append(tag)
        else:
            out.extend(data[offset:end])
        offset = end
    return bytes(out), tags


def differences(left, right, path="response"):
    if type(left) is not type(right):
        yield path
    elif isinstance(left, dict):
        for key in sorted(left.keys() | right.keys()):
            # These are exactly the top-level exclusions in the runner.
            if path == "response" and key in ("capabilities", "encodings"):
                continue
            if key not in left or key not in right:
                yield f"{path}.{key}"
            else:
                yield from differences(left[key], right[key], f"{path}.{key}")
    elif isinstance(left, list):
        if len(left) != len(right):
            yield f"{path}.length"
        for i, (a, b) in enumerate(zip(left, right)):
            yield from differences(a, b, f"{path}[{i}]")
    elif left != right:
        yield path


def classify(left, right):
    raw_paths = list(differences(left, right))
    if not isinstance(left, dict) or not isinstance(right, dict):
        return {"classification": "needs-review", "difference_count": len(raw_paths),
                "paths": raw_paths[:100], "remaining_count": len(raw_paths),
                "remaining_paths": raw_paths[:100], "snapshots": []}
    normalized_left = dict(left)
    normalized_right = dict(right)
    snapshots = []
    a, b = left.get("snapshots", []), right.get("snapshots", [])
    if isinstance(a, list) and isinstance(b, list) and len(a) == len(b):
        normalized_left["snapshots"] = list(a)
        normalized_right["snapshots"] = list(b)
        for i, (native, rust) in enumerate(zip(a, b)):
            if native == rust:
                continue
            try:
                native_common, native_tags = without_pointer_shapes(native)
                rust_common, rust_tags = without_pointer_shapes(rust)
                pointer_only = (not native_tags and bool(rust_tags)
                                and native_common == rust_common)
                snapshots.append({"index": i, "pointer_only": pointer_only,
                                  "native_pointer_records": len(native_tags),
                                  "rust_pointer_records": len(rust_tags)})
                if pointer_only:
                    normalized_right["snapshots"][i] = native
            except (ValueError, TypeError, struct.error) as error:
                snapshots.append({"index": i, "pointer_only": False,
                                  "parse_error": str(error)})
    remaining = list(differences(normalized_left, normalized_right))
    pointer_only = bool(raw_paths) and not remaining and bool(snapshots)
    return {"classification": "existing-pointer-shape-record-only" if pointer_only else "needs-review",
            "difference_count": len(raw_paths), "paths": raw_paths[:100],
            "remaining_count": len(remaining), "remaining_paths": remaining[:100],
            "snapshots": snapshots}


def selfcheck():
    def rec(tag, payload):
        # Classification compares stored CRC bytes without validating them;
        # snapshot validity remains the separate cross-decoding suite's job.
        return struct.pack("<HII", tag, len(payload), 123) + payload
    native = b"GHOSTSNP\x01\x00" + rec(2, b"screen0") + rec(2, b"screen1")
    rust = (b"GHOSTSNP\x01\x00" + rec(2, b"screen0") + rec(0x8001, b"p0")
            + rec(2, b"screen1") + rec(0x8001, b"p1"))
    left = {"ok": True, "snapshots": [native.hex()], "state": {"x": 1}}
    right = {"ok": True, "snapshots": [rust.hex()], "state": {"x": 1}}
    assert classify(left, right)["classification"] == "existing-pointer-shape-record-only"
    right["state"] = {"x": 2}
    assert classify(left, right)["remaining_paths"] == ["response.state.x"]
    right["state"] = {"x": 1}
    right["snapshots"] = [(rust + rec(0x8002, b"other")).hex()]
    assert classify(left, right)["classification"] == "needs-review"
    assert without_pointer_shapes(rust.hex())[0] == native
    for invalid in [native[:-1], native + b"\x00", b"bad"]:
        try:
            without_pointer_shapes(invalid.hex())
        except ValueError:
            continue
        raise AssertionError("accepted a truncated snapshot")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifacts", type=Path, nargs="?")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    selfcheck()
    if args.artifacts is None:
        print("classification selfcheck passed")
        return
    rows = []
    for path in sorted((args.artifacts / "failures").glob("*")):
        request = json.loads((path / "request.json").read_text())
        left = json.loads((path / "zig.json").read_text())
        right = json.loads((path / "rust.json").read_text())
        rows.append({"id": request["id"], "artifact": str(path),
                     "runner_reason": (path / "difference.txt").read_text().strip(),
                     **classify(left, right)})
    result = {"runner_summary": json.loads((args.artifacts / "summary.json").read_text()),
              "counts": dict(Counter(row["classification"] for row in rows)),
              "failures": rows}
    assert len(rows) == result["runner_summary"]["failures"]
    output = args.output or args.artifacts / "classification.json"
    output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"output": str(output), "counts": result["counts"]}))


if __name__ == "__main__":
    main()
