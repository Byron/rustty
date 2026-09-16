#!/usr/bin/env python3
"""Measure frozen binaries serially, reversing each workload's order."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import time


def benchmarks(binary, native=None):
    env = os.environ.copy()
    env.pop("GHOSTTY_PRIMITIVES_BIN", None)
    if native:
        env["GHOSTTY_PRIMITIVES_BIN"] = str(native)
    result = subprocess.run(
        [str(binary), "--list", "--format", "terse"],
        env=env, capture_output=True, text=True, check=True,
    )
    return [line.removesuffix(": benchmark") for line in result.stdout.splitlines()
            if line.endswith(": benchmark")]


def measure(binary, native, output, variant, case, direction):
    engine = "ghostty" if variant == "ghostty" else "rustty"
    bench_id = f"{engine}/{case}"
    home = output / "criterion" / variant
    env = os.environ.copy()
    env.pop("GHOSTTY_PRIMITIVES_BIN", None)
    env["CRITERION_HOME"] = str(home)
    if variant == "ghostty":
        env["GHOSTTY_PRIMITIVES_BIN"] = str(native)
    command = [str(binary), "--bench", "--exact", bench_id,
               "--sample-size", "50", "--warm-up-time", "0.3",
               "--measurement-time", "1", "--noplot", "--save-baseline", direction]
    log = output / "logs" / f"{case.replace('/', '-')}-{direction}-{variant}.log"
    started = time.time()
    with log.open("w") as stream:
        subprocess.run(command, env=env, stdout=stream, stderr=subprocess.STDOUT, check=True)
    group, name = bench_id.rsplit("/", 1)
    saved = home / group.replace("/", "_") / name / direction
    sample = json.loads((saved / "sample.json").read_text())
    estimates = json.loads((saved / "estimates.json").read_text())
    assert len(sample["times"]) == len(sample["iters"]) == 50, saved
    normalized = [t / n for t, n in zip(sample["times"], sample["iters"])]
    assert all(value > 0 for value in normalized), saved
    return {
        "median_ns": estimates["median"]["point_estimate"],
        "median_confidence_interval": estimates["median"]["confidence_interval"],
        "samples_ns": normalized,
        "started_unix": started,
        "elapsed_wall_s": time.time() - started,
        "data": str(saved.relative_to(output)),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for variant in ["before", "scalar", "simd", "ghostty"]:
        parser.add_argument(f"--{variant}", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--case", action="append", default=[],
                        help="Only workload names containing any of these texts (repeatable)")
    parser.add_argument("--check", action="store_true", help="Validate and list workloads without timing")
    args = parser.parse_args()
    binaries = {name: getattr(args, name).resolve()
                for name in ["before", "scalar", "simd", "ghostty"] if getattr(args, name)}
    variants = [name for name in binaries if name != "ghostty"]
    if len(variants) < 2:
        parser.error("supply at least two Rust binaries (--before, --scalar, --simd)")
    rust = benchmarks(binaries[variants[0]])
    assert len(rust) == 54, rust
    for variant in variants[1:]:
        assert benchmarks(binaries[variant]) == rust, variant
    native = set()
    if "ghostty" in binaries:
        native = benchmarks(binaries[variants[-1]], binaries["ghostty"])
        native = {name.removeprefix("ghostty/") for name in native if name.startswith("ghostty/")}
        assert len(native) == 36, native
    cases = [name.removeprefix("rustty/") for name in rust
             if not args.case or any(case in name for case in args.case)]
    assert cases, "No matching workloads"
    if args.check:
        print("\n".join(cases))
        return

    output = args.output.resolve()
    (output / "logs").mkdir(parents=True, exist_ok=True)
    manifest = {
        "binaries": {name: {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
                     for name, path in binaries.items()},
        "platform": platform.platform(),
        "samples_per_direction": 50,
        "warmup_seconds": 0.3,
        "measurement_seconds": 1,
        "cases": cases,
    }
    manifest_path = output / "manifest.json"
    if manifest_path.exists():
        assert json.loads(manifest_path.read_text()) == manifest, "Output belongs to a different run"
    else:
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    results_path = output / "results.json"
    results = json.loads(results_path.read_text()) if results_path.exists() else []
    completed = {result["case"] for result in results}
    for index, case in enumerate(cases, 1):
        if case in completed:
            continue
        order = variants + (["ghostty"] if case in native else [])
        result = {"case": case, "forward": {}, "reverse": {}}
        for direction, scheduled in [("forward", order), ("reverse", order[::-1])]:
            for variant in scheduled:
                print(f"{index}/{len(cases)} {case} {direction} {variant}", flush=True)
                binary = binaries[variants[-1] if variant == "ghostty" else variant]
                result[direction][variant] = measure(
                    binary, binaries.get("ghostty"), output, variant, case, direction
                )
        result["median_ns"] = {
            variant: statistics.median(result["forward"][variant]["samples_ns"]
                                       + result["reverse"][variant]["samples_ns"])
            for variant in order
        }
        results.append(result)
        # Resume only complete workloads, preserving adjacency on an interrupted case.
        temporary = output / "results.tmp"
        temporary.write_text(json.dumps(results, indent=2) + "\n")
        temporary.replace(results_path)
        print(f"  medians (ns): {result['median_ns']}", flush=True)


if __name__ == "__main__":
    main()
