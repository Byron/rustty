"""Independently check the complete linked timing matrix and its output guards."""
import hashlib
import json
from pathlib import Path
from statistics import median

root = Path(__file__).resolve().parent
source = root / "linked-timings.json"
records = json.loads(source.read_text())
names = list(dict.fromkeys(row["name"] for row in records))
assert len(records) == 261 and len(names) == 29, "requires the full final matrix"
results = []
for name in names:
    rows = [row for row in records if row["name"] == name]
    assert len(rows) == 9
    assert {(row["version"], row["round"]) for row in rows} == {
        (version, round_) for version in ("before", "after", "native")
        for round_ in (1, 2, 3)
    }
    assert all(row["status"] == 0 and not row["stderr"] for row in rows)
    for key in ("checksum", "history_rows", "page_bytes", "page_count", "units_per_iteration"):
        assert len({row["result"][key] for row in rows}) == 1, (name, key)
    assert len({row["result"]["iterations"] for row in rows}) == 1, name
    assert len({row["result"]["owned_bytes"] for row in rows
                if row["version"] != "native"}) == 1, (name, "owned_bytes")
    samples = {
        version: {row["round"]: row["result"]["elapsed_ns"]
                  / row["result"]["iterations"] / 1e6
                  for row in rows if row["version"] == version}
        for version in ("before", "after", "native")
    }
    assert all(value > 0 for group in samples.values() for value in group.values())
    medians = {version: median(group.values()) for version, group in samples.items()}
    results.append({
        "name": name,
        "median_ms": medians,
        "time_change_percent": (medians["after"] / medians["before"] - 1) * 100,
        "paired_time_change_percent": [
            (samples["after"][round_] / samples["before"][round_] - 1) * 100
            for round_ in (1, 2, 3)
        ],
        "owned_bytes": {version: sorted({row["result"]["owned_bytes"] for row in rows
                                          if row["version"] == version})
                        for version in ("before", "after")},
    })
result = {
    "timings_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
    "audit_source_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
    "records": len(records),
    "cases": len(names),
    "correctness_and_page_guards": "all equal across runs and engines",
    "owned_memory_changes": [],
    "results": results,
}
(root / "linked-root-measurement-check.json").write_text(json.dumps(result, indent=2) + "\n")
print(json.dumps({row["name"]: round(row["time_change_percent"], 2) for row in results}, indent=2))
