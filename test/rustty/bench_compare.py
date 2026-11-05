#!/usr/bin/env python3
"""Measure frozen binaries serially, reversing each workload's order."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import statistics
import subprocess
import time


SAMPLE_SIZE = 20
WARMUP_SECONDS = 0.2
MEASUREMENT_SECONDS = 0.5
RESAMPLES = 10_000


def run(command, *, timeout, **kwargs):
    # Kill the native helper too when Criterion exceeds its wall-clock budget.
    with subprocess.Popen(command, start_new_session=True, **kwargs) as process:
        try:
            stdout, _ = process.communicate(timeout=timeout)
        except (subprocess.TimeoutExpired, KeyboardInterrupt):
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate()
            raise
        if process.returncode:
            raise subprocess.CalledProcessError(process.returncode, command)
        return stdout


def benchmarks(binary, native=None, timeout=30):
    env = os.environ.copy()
    env.pop("GHOSTTY_PRIMITIVES_BIN", None)
    if native:
        env["GHOSTTY_PRIMITIVES_BIN"] = str(native)
    result = run(
        [str(binary), "--list", "--format", "terse"],
        env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=timeout,
    )
    return [line.removesuffix(": benchmark") for line in result.splitlines()
            if line.endswith(": benchmark")]


def measure(binary, native, output, variant, case, direction, timeout=30):
    engine = "ghostty" if variant == "ghostty" else "rustty"
    bench_id = f"{engine}/{case}"
    home = output / "criterion" / variant
    env = os.environ.copy()
    env.pop("GHOSTTY_PRIMITIVES_BIN", None)
    env["CRITERION_HOME"] = str(home)
    if variant == "ghostty":
        env["GHOSTTY_PRIMITIVES_BIN"] = str(native)
    command = [str(binary), "--bench", "--exact", bench_id,
               "--sample-size", str(SAMPLE_SIZE), "--warm-up-time", str(WARMUP_SECONDS),
               "--measurement-time", str(MEASUREMENT_SECONDS), "--nresamples", str(RESAMPLES),
               "--noplot", "--save-baseline", direction]
    log = output / "logs" / f"{case.replace('/', '-')}-{direction}-{variant}.log"
    started = time.time()
    clock_started = time.monotonic()
    with log.open("w") as stream:
        run(command, env=env, stdout=stream, stderr=subprocess.STDOUT, timeout=timeout)
    saved = home / f"{engine}_{case}" / direction
    sample = json.loads((saved / "sample.json").read_text())
    estimates = json.loads((saved / "estimates.json").read_text())
    assert len(sample["times"]) == len(sample["iters"]) == SAMPLE_SIZE, saved
    normalized = [t / n for t, n in zip(sample["times"], sample["iters"])]
    assert all(value > 0 for value in normalized), saved
    return {
        "median_ns": estimates["median"]["point_estimate"],
        "median_confidence_interval": estimates["median"]["confidence_interval"],
        "samples_ns": normalized,
        "started_unix": started,
        "elapsed_wall_s": time.monotonic() - clock_started,
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
    parser.add_argument("--timeout", type=float, default=30,
                        help="Wall-clock limit per benchmark process in seconds (default: 30)")
    parser.add_argument("--max-time", type=float, default=600,
                        help="Wall-clock budget for the comparison in seconds (default: 600)")
    args = parser.parse_args()
    if not (0 < args.timeout < float("inf") and 0 < args.max_time < float("inf")):
        parser.error("--timeout and --max-time must be finite and positive")
    deadline = time.monotonic() + args.max_time

    def remaining():
        seconds = deadline - time.monotonic()
        if seconds <= 0:
            raise SystemExit("Comparison reached --max-time; completed cases are saved for resuming")
        return min(args.timeout, seconds)

    binaries = {name: getattr(args, name).resolve()
                for name in ["before", "scalar", "simd", "ghostty"] if getattr(args, name)}
    variants = [name for name in binaries if name != "ghostty"]
    if not variants or len(binaries) < 2:
        parser.error("supply a Rust binary (--before, --scalar, --simd) and at least one comparison")
    rust = benchmarks(binaries[variants[0]], timeout=remaining())
    assert rust and len(rust) == len(set(rust)) and all(name.startswith("rustty/") for name in rust), rust
    for variant in variants[1:]:
        assert benchmarks(binaries[variant], timeout=remaining()) == rust, variant
    native = set()
    if "ghostty" in binaries:
        native = benchmarks(binaries[variants[-1]], binaries["ghostty"], timeout=remaining())
        native = {name.removeprefix("ghostty/") for name in native if name.startswith("ghostty/")}
        assert native and native <= {name.removeprefix("rustty/") for name in rust}, native
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
        "samples_per_direction": SAMPLE_SIZE,
        "warmup_seconds": WARMUP_SECONDS,
        "measurement_seconds": MEASUREMENT_SECONDS,
        "bootstrap_resamples": RESAMPLES,
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
                    binary, binaries.get("ghostty"), output, variant, case, direction,
                    timeout=remaining(),
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
    try:
        main()
    except subprocess.TimeoutExpired as error:
        raise SystemExit(f"Benchmark exceeded {error.timeout:.1f}s; completed cases are saved. "
                         "See the last workload's log or raise --timeout/--max-time.") from error
