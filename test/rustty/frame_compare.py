#!/usr/bin/env python3
"""Compare frozen frame probes serially, reversing each workload's order."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess

CASES = ["cached_redraw", "scroll_ascii", "scroll_styled", "mixed_unicode",
         "alternate_repaint", "resize_reflow"]


def stats(values):
    values = sorted(values)
    return {"median": statistics.median(values),
            "p95": values[(len(values) * 95 + 99) // 100 - 1],
            "p99": values[(len(values) * 99 + 99) // 100 - 1]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", required=True, type=Path)
    parser.add_argument("--after", required=True, type=Path)
    parser.add_argument("--kind", required=True, choices=["prepare", "app"])
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--case", action="append", choices=CASES)
    parser.add_argument("--offscreen", action="store_true")
    args = parser.parse_args()
    binaries = {name: getattr(args, name).resolve() for name in ["before", "after"]}
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    manifest = {
        "kind": args.kind, "platform": platform.platform(), "offscreen": args.offscreen,
        "cases": args.case or CASES,
        "binaries": {name: {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
                     for name, path in binaries.items()},
        "workload_sha256": hashlib.sha256(Path(__file__).with_name("frame_workloads.rs").read_bytes()).hexdigest(),
    }
    manifest_path = output / "manifest.json"
    if manifest_path.exists():
        assert json.loads(manifest_path.read_text()) == manifest, "Output belongs to a different run"
    else:
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    results_path = output / "results.json"
    results = json.loads(results_path.read_text()) if results_path.exists() else []
    for case in manifest["cases"]:
        if any(result["case"] == case for result in results):
            continue
        result = {"case": case, "forward": {}, "reverse": {}}
        control = None
        for direction, order in [("forward", ["before", "after"]), ("reverse", ["after", "before"])]:
            for variant in order:
                directory = output / case / direction / variant
                directory.mkdir(parents=True, exist_ok=True)
                env = {key: value for key, value in os.environ.items() if not key.startswith("RUSTTY_SMOKE_")}
                command = [str(binaries[variant])]
                if args.kind == "prepare":
                    command += ["--case", case]
                else:
                    env.update(RUSTTY_SMOKE_DIR=str(directory), RUSTTY_SMOKE_TIMING=case)
                    if args.offscreen:
                        env["RUSTTY_SMOKE_OFFSCREEN"] = "1"
                print(case, direction, variant, flush=True)
                run = subprocess.run(command, env=env, capture_output=True, text=True, timeout=60)
                (directory / "stdout.log").write_text(run.stdout)
                (directory / "stderr.log").write_text(run.stderr)
                run.check_returncode()
                if args.kind == "prepare":
                    reports = json.loads(run.stdout)
                    assert len(reports) == 1
                    report = reports[0]
                else:
                    report = json.loads((directory / "timing.json").read_text())
                    assert report["offscreen_gpu_submission"] == args.offscreen
                    assert not report["gpu_completion_measured"]
                    assert not report["visible_presentation_measured"]
                assert report["case"] == case
                assert report["sample_frames"] == len(report["samples"]) == 50
                assert report["warmup_frames"] == 50
                settings = {key: report.get(key) for key in ["font", "font_size_points", "size_pixels", "terminal_size", "scale_factor", "columns"]}
                if control is None:
                    control = settings
                assert settings == control, (case, direction, variant, settings, control)
                result[direction][variant] = report
        result["pooled"] = {
            variant: {name: stats([sample[index] for direction in ["forward", "reverse"]
                                  for sample in result[direction][variant]["samples"]])
                      for index, name in enumerate(control["columns"])}
            for variant in ["before", "after"]
        }
        results.append(result)
        temporary = output / "results.tmp"
        temporary.write_text(json.dumps(results, indent=2) + "\n")
        temporary.replace(results_path)
        print(result["pooled"], flush=True)


if __name__ == "__main__":
    main()
