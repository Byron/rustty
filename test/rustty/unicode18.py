#!/usr/bin/env python3
"""Compare all tailored property triples and longer Unicode 18 joins with Ghostty."""
import itertools
import json
import subprocess
import sys


def requests():
    # One scalar for each pinned uucode GraphemeBreakNoControl property, in order.
    representatives = [
        0x61, 0x600, 0x1F1E8, 0x903, 0x1100, 0x1160, 0x11A8, 0xAC00, 0xAC01,
        0x200D, 0x200C, 0x1F600, 0x1F44D, 0x1F3FB, 0x300, 0x94D, 0x1CF5, 0x915,
    ]
    sequences = itertools.chain(
        [()],
        itertools.product(representatives, repeat=3),
        [
            (0x94D, 0x300, 0x200D, 0x915),
            (0x94D, 0x915, 0x300, 0x915),
            (0x94D, 0x200C, 0x300, 0x915),
            (0x61, 0x1CF5, 0x300, 0x915),
            (0x11A3A, 0x11A0B),
            (0x1F600, 0x94D, 0x300, 0x200D, 0x1F600),
            (0x1F600, 0x94D, 0x300, 0x915),
            (0x1F600, 0x94D, 0x200C, 0x200D, 0x1F600),
            (0x1F600, 0x94D, 0x200C, 0x915),
            (0x1F44D, 0x1F3FB, 0x94D, 0x915),
            (0x1FAF9, 0x1F3FB),
            (0x1FAFA, 0x1F3FF),
            (0x1F1E8, 0x1F1ED, 0x1F1FA, 0x1F1F8),
        ],
    )
    for index, cps in enumerate(sequences):
        yield {"id": f"unicode18/grapheme/{index}", "kind": "unicode-grapheme",
               "codepoints": cps}
    for cp in [0xD800, 0xDFFF, 0x110000, 0xFFFFFFFF]:
        yield {"id": f"unicode18/invalid/{cp:x}", "kind": "unicode-grapheme",
               "codepoints": [cp]}


def main(zig, rust):
    cases = list(requests())
    payload = "".join(json.dumps(case) + "\n" for case in cases).encode()
    outputs = []
    for binary in [zig, rust]:
        result = subprocess.run([binary], input=payload, capture_output=True, timeout=60, check=True)
        outputs.append([json.loads(line) for line in result.stdout.splitlines()])
        assert len(outputs[-1]) == len(cases), binary
    for case, native, port in zip(cases, *outputs):
        if case["id"].startswith("unicode18/invalid/"):
            assert not native["ok"] and not port["ok"], (case, native, port)
            assert native["err"] == port["err"] == "InvalidCodepoint"
            continue
        assert native["ok"] and port["ok"], (case, native, port)
        assert native["grapheme"] == port["grapheme"], (case, native["grapheme"], port["grapheme"])
    print(f"{len(cases)} Unicode 18 grapheme and invalid-input comparisons match Ghostty")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("usage: unicode18.py ZIG_ORACLE RUST_ORACLE")
    main(*sys.argv[1:])
