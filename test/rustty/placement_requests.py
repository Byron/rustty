"""Ordinary Kitty placements: storage keys, effective geometry and cursor effects."""
import base64


def command(options, data=b""):
    return b"\x1b_G" + options + b";" + base64.b64encode(data) + b"\x1b\\"


def upload(width=17, height=19, image=1, extra=b""):
    pixels = bytes([32, 64, 128, 255]) * (width * height)
    options = f"a=t,f=32,s={width},v={height},i={image}".encode()
    return command(options + (b"," + extra if extra else b""), pixels)


def case(name, operations, cell=(8, 16)):
    setup = [] if cell is None else [{"op": "resize", "cols": 12, "rows": 8, "cell_size": list(cell)}]
    return ({"id": "protocol/graphics/placements/" + name, "cols": 12, "rows": 8,
             "observe_graphics": True, "observe_graphics_placements": True,
             "operations": [*setup, *[op if isinstance(op, dict) else {"op": "write", "data": op.hex()}
                                       for op in operations]]}, ["graphics.kitty", "effects.pty"])


def requests():
    observe = {"op": "observe"}
    sizes = [b"", b"c=2", b"r=2", b"c=3,r=2", b"c=4294967295", b"r=4294967295",
             b"c=4294967295,r=4294967295"]
    for width, height in ((1, 1), (4, 3), (17, 19)):
        for size in sizes:
            for offset in (b"", b"X=3,Y=7", b"X=4294967295,Y=4294967295"):
                options = b",".join(part for part in (b"a=p,i=1,p=2,C=1", size, offset) if part)
                yield case(f"size/{width}/{height}/{size.decode()}/{offset.decode()}",
                           [upload(width, height), b"\x1b[2;3H", command(options)])
    for crop in (b"x=8,y=7", b"x=8,y=7,w=8,h=8", b"x=10,y=10", b"x=100,y=100,w=1,h=1",
                 b"w=4294967295,h=4294967295", b"x=4294967295,y=4294967295"):
        for size in (b"", b",c=3", b",r=3", b",c=2,r=2"):
            yield case("source/" + crop.decode() + size.decode(),
                       [upload(10, 10), command(b"a=p,i=1,p=2,C=1," + crop + size)])

    for before, after in ((None, (8, 16)), ((0, 0), (8, 16)), ((8, 16), (4, 9)),
                          ((8, 16), (0, 0)), ((0, 16), (8, 0))):
        for size in (b"", b",c=3", b",r=3", b",c=3,r=2"):
            yield case(f"resize/{before}/{after}/{size.decode()}",
                       [upload(), command(b"a=p,i=1,p=1,C=1,X=99,Y=99" + size), observe,
                        {"op": "resize", "cols": 12, "rows": 8, "cell_size": list(after)}], cell=before)

    for size in (b"", b",c=2", b",r=2", b",c=3,r=2"):
        yield case("cursor/" + size.decode(),
                   [upload(), b"\x1b[2;3H", command(b"a=p,i=1,p=1,X=3,Y=7" + size)])
    yield case("keys", [upload(), command(b"a=p,i=1,C=1"), observe,
                command(b"a=p,i=1,C=1"), observe,
                command(b"a=p,i=1,p=1,C=1"), observe,
                b"\x1b[2;3H", command(b"a=p,i=1,p=1,C=1,z=-7"), observe,
                command(b"a=d,d=i,i=1,p=1"), observe,
                command(b"a=p,i=1,C=1"), observe,
                command(b"a=d,d=i,i=1"), observe])
    yield case("retransmit", [upload(), command(b"a=p,i=1,p=1,C=1"),
                command(b"a=p,i=1,C=1"), observe, upload(4, 3), observe,
                command(b"a=p,i=1,C=1"), observe])
    yield case("retransmit/chunks", [upload(), command(b"a=p,i=1,p=1,C=1"), observe,
                command(b"a=T,i=1,p=2,f=32,s=2,v=1,C=1,m=1", b"\x01\x02\x03\xff"), observe,
                command(b"m=0", b"\x04\x05\x06\xff"), observe])
    for options in (b"a=t,i=1,f=32,s=1,v=1", b"a=t,i=1,f=99,s=1,v=1"):
        yield case("retransmit/failure/" + options.decode(), [upload(),
                    command(b"a=p,i=1,p=1,C=1"), observe, command(options, b"x"), observe])
    yield case("retransmit/query", [upload(), command(b"a=p,i=1,p=1,C=1"), observe,
                command(b"a=q,i=1,f=32,s=1,v=1", b"\x01\x02\x03\xff"), observe])
    for delete in (b"d=i,i=1,p=1", b"d=I,i=1,p=1", b"d=i,i=1", b"d=I,i=1", b"d=a", b"d=A",
                   b"d=c", b"d=p,x=3,y=2", b"d=q,x=3,y=2,z=-7", b"d=x,x=3", b"d=y,y=2", b"d=z,z=-7"):
        yield case("delete/" + delete.decode(), [upload(), b"\x1b[2;3H",
                    command(b"a=p,i=1,p=1,C=1,z=-7"), observe,
                    b"\x1b[6;8H", command(b"a=p,i=1,C=1"), observe,
                    b"\x1b[2;3H", command(b"a=d," + delete), observe])

    for name, change in (
        ("history", b"\r\nL" * 12), ("erase", b"\x1b[2J"),
        ("reset", {"op": "terminal_reset"}),
        ("reflow", {"op": "resize", "cols": 6, "rows": 8}),
    ):
        yield case("lifecycle/" + name, [upload(), b"\x1b[2;3H",
                    command(b"a=p,i=1,p=1,C=1"), observe, change, observe])
    for mode in (47, 1049):
        yield case(f"screens/{mode}", [upload(), command(b"a=p,i=1,p=1,C=1"), observe,
                    f"\x1b[?{mode}h".encode(), upload(4, 3),
                    command(b"a=p,i=1,p=1,C=1"), observe,
                    f"\x1b[?{mode}l".encode(), observe])

    for name, change in (("delete", command(b"a=d,d=I,i=1,p=1")),
                          ("replace", upload(4, 3)), ("clear", b"\x1b[2J")):
        yield case("parent-lifetime/" + name, [upload(), upload(image=2),
                    command(b"a=p,i=1,p=1,C=1"),
                    command(b"a=p,i=2,p=2,C=1,P=1,Q=1,H=-2,V=1"), observe,
                    change, observe])
    yield case("virtual-clear", [upload(), command(b"a=p,i=1,p=1,U=1,C=1"),
                upload(image=2), observe, b"\x1b[2J", observe])


if __name__ == "__main__":
    import json
    import sys

    json.dump([{"request": request, "covers": covers} for request, covers in requests()],
              sys.stdout, indent=2)
    sys.stdout.write("\n")
