"""Explicit host-query decisions and their terminal protocol replies."""


def requests():
    def case(name, operations, host=None):
        return ({"id": "protocol/host/" + name,
                 "operations": [op if isinstance(op, dict) else {"op": "write", "data": op.hex()} for op in operations],
                 "host": host or {}}, ["effects.host", "effects.pty"])

    queries = b"\x1b[c\x1b[>c\x1b[=c\x1b[>q\x05\x1b[?996n\x1b[14t\x1b[16t\x1b[18t"
    populated = {"device_attributes": {}, "color_scheme": "dark", "size": {},
                 "xtversion": b"rustty test".hex(), "enquiry": b"answer".hex()}
    yield case("defaults/absent", [queries])
    yield case("defaults/configured", [queries], populated)
    yield case("defaults/changed", [queries, {"op": "host_options", "host": populated}, queries,
                                     {"op": "host_options", "host": {}}, queries])
    yield case("defaults/ris", [queries, b"\x1bc", queries], populated)
    yield case("defaults/direct-reset", [queries, {"op": "terminal_reset"}, queries], populated)

    for index, attributes in enumerate(({}, {"features": []}, {"conformance_level": 1, "features": [1, 6, 22]},
                                         {"conformance_level": 65535, "features": [0, 65535, 22, 22], "device_type": 65535,
                                          "firmware_version": 65535, "rom_cartridge": 65535, "unit_id": 4294967295},
                                         {"features": [65535] * 256, "unit_id": 0xAABBCCDD})):
        yield case(f"attributes/values/{index}", [b"\x1b[c\x1b[>0c\x1b[=0c"], {"device_attributes": attributes})
    for index, query in enumerate((b"c", b"0c", b"1c", b"0;0c", b">c", b">0c", b">1c", b">0;0c",
                                    b"=c", b"=0c", b"=1c", b"?c", b"?0c", b"0:c", b">65535c")):
        yield case(f"attributes/context/{index}", [b"\x1b[" + query], {"device_attributes": {}})
    yield case("attributes/decid", [b"\x1bZ"], {"device_attributes": {}})
    yield case("attributes/decid-absent", [b"\x1bZ"])

    for name, key, command, lengths in (("version", "xtversion", b"\x1b[>0q", (0, 1, 255, 256, 257, 280, 281, 282, 512)),
                                        ("enquiry", "enquiry", b"\x05", (0, 1, 254, 255, 256, 257))):
        for length in lengths:
            yield case(f"{name}/length/{length}", [command], {key: (b"x" * length).hex()})
        yield case(f"{name}/raw-bytes", [command], {key: b"a\x00\xff\x1b\\".hex()})
    for index, query in enumerate((b">q", b">0q", b">1q", b">0;0q", b"q", b"?q", b"=q")):
        yield case(f"version/context/{index}", [b"\x1b[" + query], {"xtversion": b"version".hex()})
    for index, data in enumerate((b"\x05", b"\x1b[\x05m", b"\x1b]\x05\x07", b"\x1bP\x05q\x1b\\")):
        yield case(f"enquiry/context/{index}", [data], {"enquiry": b"answer".hex()})

    for scheme in (None, "none", "dark", "light"):
        yield case(f"color-scheme/{scheme}", [b"\x1b[?996n\x1b[996n\x1b[?997n\x1b[?996;0n"], {"color_scheme": scheme})
    for private in (b"", b"?"):
        for value in (5, 6, 996, 998, 0, 65535):
            yield case(f"status/{private.hex()}/{value}", [b"\x1b[" + private + str(value).encode() + b"n"], populated)

    sizes = [None, {}, {"available": False}, {"rows": 0, "columns": 0, "cell_width": 0, "cell_height": 0},
             {"rows": 65535, "columns": 65535, "cell_width": 4294967295, "cell_height": 4294967295}]
    for index, size in enumerate(sizes):
        yield case(f"size/values/{index}", [b"\x1b[14t\x1b[16t\x1b[18t\x1b[?2048h\x1b[?2048h\x1b[?2048l"], {"size": size})
    for index, query in enumerate((b"14t", b"14;0t", b"14;1t", b"14;2t", b"16t", b"18t", b"18;1t", b"21t", b"?14t")):
        yield case(f"size/context/{index}", [b"\x1b[" + query], {"size": {}})
    yield case("size/saved-mode", [b"\x1b[?2048h\x1b[?2048s\x1b[?2048l\x1b[?2048r\x1b[?2048$p"], {"size": {}})
    for enabled in (False, True):
        for cell_size in (None, [0, 0], [8, 16], [4294967295, 4294967295]):
            yield case(f"size/resize/{enabled}/{cell_size}", [
                b"\x1b[?2048h" if enabled else b"\x1b[?2048l",
                {"op": "resize", "cols": 12, "rows": 4, "cell_size": cell_size},
                {"op": "resize", "cols": 10, "rows": 5, "cell_size": cell_size},
                b"\x1b[?2048$p",
            ])

    for visible in (False, True):
        yield case(f"visibility/{visible}", [b"\x1b[?998n\x1b[?2033$p\x1b[?2033h\x1b[?2033h\x1b[?2033l",
                                              b"\x1bc\x1b[?998n", {"op": "terminal_reset"}, b"\x1b[?998n"], {"visible": visible})
    yield case("visibility/change", [b"\x1b[?2033h", {"op": "host_options", "host": {"visible": False}}, b"\x1b[?998n"])
    yield case("visibility/saved-mode", [b"\x1b[?2033h\x1b[?2033s\x1b[?2033l\x1b[?2033r\x1b[?2033$p"], {"visible": False})
    for enabled in (False, True):
        yield case(f"title-report/{enabled}", [b"\x1b]2;Title\x07\x1b[21t\x1bc\x1b[21t"], {"title_report": enabled})

    for index, name in enumerate((None, b"", b"rustty", b"xterm-rustty", "héllo".encode(), b"raw\x00\xff",
                                  b"x" * 128, b"x" * 129, b"name with punctuation;=")):
        query = b"\x1bP+q544e\x1b\\"
        yield case(f"terminfo/{index}", [query, b"\x1bc", query, {"op": "terminal_reset"}, query],
                   {"terminfo_name": name.hex() if name is not None else None})
