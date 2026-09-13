"""Native page/resource layout arithmetic, without allocating backing pages."""
import random


def requests():
    def case(name, *, expected_error=None, **layout):
        request = {"id": f"page-layout/{name}", "kind": "page_layout", "page_layout": layout}
        if expected_error:
            request["expected_error"] = expected_error
        return request, ["terminal.page-layout"]

    for columns in (1, 2, 3, 7, 8, 15, 16, 17, 79, 80, 81, 127, 128, 129,
                    214, 215, 216, 255, 256, 257, 511, 512, 513, 1023, 1024,
                    1025, 4095, 4096, 16383, 16384, 32767, 32768,
                    47917, 47918, 47919, 47920, 65534, 65535):
        # 47918/47919 straddle the macOS ARM standard page's one-row limit.
        # Other hosts still compare their own native boundary behavior.
        yield case(f"initial/{columns}", columns=columns)

    empty = {"cols": 1, "rows": 1, "styles": 0, "hyperlink_bytes": 0,
             "grapheme_bytes": 0, "string_bytes": 0}
    for cols, rows in ((1, 1), (1, 7), (1, 8), (1, 15), (1, 16), (1, 17),
                       (80, 24), (80, 591), (215, 215), (215, 221),
                       (1024, 1), (1024, 512), (65535, 1), (1, 65535)):
        for resources, capacity in (("empty", empty), ("default", {})):
            for exact in (False, True):
                yield case(f"dimensions/{resources}/{cols}/{rows}/{exact}",
                           action="layout", capacity=dict(capacity, cols=cols, rows=rows),
                           exact=exact)

    resource_cases = {
        "styles": (0, 1, 2, 3, 4, 7, 8, 9, 15, 16, 17, 31, 32, 33,
                   63, 64, 65, 127, 128, 129, 255, 256, 257, 32767, 32768, 32769, 65535),
        "hyperlink_bytes": (0, 1, 47, 48, 49, 95, 96, 97, 143, 144, 145,
                            191, 192, 193, 383, 384, 385, 767, 768, 769,
                            12287, 12288, 12289, 24575, 24576, 24577, 65535),
        "grapheme_bytes": (0, 1, 15, 16, 17, 31, 32, 33, 511, 512, 513,
                           1023, 1024, 1025, 2047, 2048, 2049, 8191, 8192, 8193,
                           65535, 65536, 65537, (1 << 29) - 1, 1 << 29,
                           (1 << 29) + 1, (1 << 30) - 1, 1 << 30),
        "string_bytes": (0, 1, 31, 32, 33, 63, 64, 65, 1023, 1024, 1025,
                         2047, 2048, 2049, 4095, 4096, 4097, 65535, 65536, 65537,
                         (1 << 30) - 1, 1 << 30, (1 << 31) - 1, 1 << 31),
    }
    for resource, values in resource_cases.items():
        for value in values:
            yield case(f"resource/{resource}/{value}", action="layout",
                       capacity=dict(empty, cols=5, rows=3, **{resource: value}))

    capacities = {
        "standard": None,
        "small-default": {"cols": 80, "rows": 24},
        "small-empty": dict(empty, cols=80, rows=24),
        "styles": dict(empty, cols=80, rows=24, styles=129),
        "graphemes": dict(empty, cols=80, rows=24, grapheme_bytes=8193),
        "strings": dict(empty, cols=80, rows=24, string_bytes=4097),
        "links": dict(empty, cols=80, rows=24, hyperlink_bytes=769),
    }
    for name, capacity in capacities.items():
        for columns in (1, 3, 16, 79, 80, 81, 215, 512, 1024):
            yield case(f"adjust/{name}/{columns}", action="adjust",
                       capacity=capacity, columns=columns)

    # Combine resource padding with row/cell alignment rather than only
    # testing each field in isolation. Bounds keep every layout below 4 GiB.
    rng = random.Random(0x50414745)
    for index in range(64):
        capacity = {"cols": rng.randrange(1, 4097), "rows": rng.randrange(1, 4097),
                    "styles": rng.randrange(65536), "hyperlink_bytes": rng.randrange(65536),
                    "grapheme_bytes": rng.randrange(1 << 20), "string_bytes": rng.randrange(1 << 20)}
        yield case(f"mixed/{index}", action="layout", capacity=capacity,
                   exact=bool(index % 2))

    for name, layout, error in (
        ("initial-zero", {"columns": 0}, "InvalidDimensions"),
        ("adjust-zero", {"action": "adjust", "columns": 0}, "InvalidDimensions"),
        ("zero-columns", {"action": "layout", "capacity": {"cols": 0, "rows": 1}}, "InvalidDimensions"),
        ("zero-rows", {"action": "layout", "capacity": {"cols": 1, "rows": 0}}, "InvalidDimensions"),
        ("unknown-action", {"action": "unknown"}, "InvalidLayoutAction"),
        ("no-row-fits", {"action": "adjust", "columns": 65535, "capacity": empty}, "OutOfMemory"),
        ("row-overflow", {"action": "adjust", "columns": 1,
                          "capacity": {"cols": 1024, "rows": 512}}, "RowCountOverflow"),
        ("page-too-large", {"action": "layout", "capacity": {"cols": 65535, "rows": 65535}}, "PageTooLarge"),
        ("graphemes-too-large", {"action": "layout", "capacity": dict(empty, grapheme_bytes=(1 << 32) - 1)}, "PageTooLarge"),
        ("strings-too-large", {"action": "layout", "capacity": dict(empty, string_bytes=(1 << 32) - 1)}, "PageTooLarge"),
    ):
        yield case(f"rejected/{name}", expected_error=error, **layout)


if __name__ == "__main__":
    import json
    import sys

    json.dump([{"request": request, "covers": covers} for request, covers in requests()],
              sys.stdout, indent=2)
    sys.stdout.write("\n")
