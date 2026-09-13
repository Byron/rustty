"""Observe real native page lifetimes across terminal storage operations."""


def requests(reference):
    observe = {"op": "pages"}

    def write(lines):
        return {"op": "write", "data": (b"x\r\n" * lines).hex()}

    def limits(lines=None, bytes=None):
        return {"op": "grid", "grid": {"action": "limits", "lines": lines, "bytes": bytes}}

    def case(name, cols, rows, operations):
        return ({"id": "pages/" + name, "kind": "input", "cols": cols, "rows": rows,
                 "operations": [observe, *operations]}, ["terminal.pages"])

    capacities = {}
    pool_bytes = None
    for cols in (2, 8, 80, 215, 1024):
        response = reference.request({"id": "pages/capacity", "kind": "page_layout",
                                      "page_layout": {"columns": cols}})
        if not response["ok"]:
            raise RuntimeError("reference page capacity query failed")
        capacity = response["page_layout"]["layout"]["capacity"]["rows"]
        capacities[cols] = capacity
        pool_bytes = response["page_layout"]["constants"]["standard_bytes"]
        for rows in sorted({1, 24, min(capacity, 1024), min(capacity + 1, 1024), 1024}):
            yield case(f"initial/{cols}/{rows}", cols, rows, [])
        for rows in sorted({1, 24, min(capacity + 1, 1024)}):
            operations = []
            for count in (max(0, capacity - rows), 1, 1, capacity, capacity):
                operations += [write(count), observe]
            yield case(f"grow/{cols}/{rows}", cols, rows, operations)
        for lines in (0, 1, capacity - 1, capacity, capacity + 1, capacity * 2):
            yield case(f"lines/{cols}/{lines}", cols, 24,
                       [write(capacity * 3), observe, limits(lines=lines), observe,
                        write(capacity), observe])
        for byte_limit in (0, 1, pool_bytes * 2, pool_bytes * 3):
            yield case(f"bytes/{cols}/{byte_limit}", cols, 24,
                       [write(capacity * 3), observe, limits(bytes=byte_limit), observe,
                        write(capacity), observe])
        for sequence in (b"\x1b[3J", b"\x1b[22J", b"\x1bc"):
            yield case(f"clear/{cols}/{sequence.hex()}", cols, 24,
                       [write(capacity + 7), observe, {"op": "write", "data": sequence.hex()},
                        observe, write(capacity), observe])

    for cols, resized in ((8, 4), (8, 12), (80, 40), (80, 120), (215, 80), (1024, 512)):
        yield case(f"reflow/{cols}/{resized}", cols, 24,
                   [write(capacities[cols] + 9), observe,
                    {"op": "resize", "cols": resized, "rows": 24}, observe,
                    write(17), observe, {"op": "resize", "cols": cols, "rows": 12}, observe])
    for cols in (8, 80, 1024):
        operations = [write(capacities[cols] + 7), observe]
        for rows in (1, 24, 1024, 2):
            operations += [{"op": "resize", "cols": cols, "rows": rows}, observe]
        yield case(f"height/{cols}", cols, 24, operations)

    for cols in (8, 80, 215):
        response = reference.request({"id": "pages/snapshot/source", "kind": "input",
                                      "cols": cols, "rows": 24,
                                      "operations": [write(capacities[cols] + 31),
                                                     {"op": "snapshot"}]})
        if not response["ok"] or len(response["snapshots"]) != 1:
            raise RuntimeError("reference page snapshot failed")
        data = response["snapshots"][0]
        yield case(f"snapshot/{cols}", cols, 24,
                   [{"op": "restore", "data": data}, observe, write(1), observe,
                    {"op": "resize", "cols": cols + 1, "rows": 24}, observe])
        for byte_limit in (0, 1, None):
            yield case(f"snapshot-stream/{cols}/{byte_limit}", cols, 24,
                       [{"op": "restore_ready", "data": data}, observe,
                        limits(bytes=byte_limit), {"op": "restore_next"}, observe,
                        {"op": "restore_next"}, observe])
