#!/usr/bin/env python3
"""Compare cursor blinking with one frozen binary, serially in A/B/B/A order."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--panes", type=int, choices=[1, 12], default=1)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    assert not any(output.iterdir()), "Use an empty output directory"
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    manifest = {
        "case": "cursor_blink" if args.panes == 1 else "cursor_blink_12_panes",
        "pane_count": args.panes, "platform": platform.platform(),
        "binary": {"path": str(binary), "sha256": digest, "build_profile": "unknown"},
        "order": ["A", "B", "B", "A"],
        "variants": {"A": "baseline", "B": "native_cursor_layer"},
        "cpu_units": "Percent of one CPU core during the app's thirty-second sample",
        "windowserver_measured": False,
    }
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    results = []
    control = None
    for index, variant in enumerate(manifest["order"], 1):
        assert hashlib.sha256(binary.read_bytes()).hexdigest() == digest, "Binary changed"
        directory = output / f"{index:02d}-{variant}"
        directory.mkdir()
        env = {key: value for key, value in os.environ.items()
               if not key.startswith("RUSTTY_SMOKE_") and key != "RUSTTY_CURSOR_LAYER"}
        env.update(RUSTTY_SMOKE_CURSOR_LAYER="1", RUSTTY_SMOKE_DIR=str(directory),
                   RUSTTY_SMOKE_CURSOR_PANES=str(args.panes))
        if variant == "A":
            env["RUSTTY_CURSOR_LAYER"] = "0"
        command = [str(binary)]
        bundle = binary.parent.parent.parent
        if platform.system() == "Darwin" and bundle.suffix == ".app":
            command = ["open", "-g", "-n", "-W", "--stdout", str(directory / "stdout.log"),
                       "--stderr", str(directory / "stderr.log")]
            for key in ["RUSTTY_SMOKE_CURSOR_LAYER", "RUSTTY_SMOKE_DIR",
                        "RUSTTY_SMOKE_CURSOR_PANES", "RUSTTY_CURSOR_LAYER"]:
                if key in env:
                    command += ["--env", f"{key}={env[key]}"]
            command.append(str(bundle))
        print(directory.name, flush=True)
        with (directory / "stdout.log").open("w") as stdout, \
                (directory / "stderr.log").open("w") as stderr:
            run = subprocess.run(command, env=env, stdout=stdout, stderr=stderr, timeout=60)
        run.check_returncode()
        assert hashlib.sha256(binary.read_bytes()).hexdigest() == digest, "Binary changed"
        report = json.loads((directory / "result.json").read_text())
        assert report["passed"] is True, report
        assert report["case"] == manifest["case"], report
        assert report["pane_count"] == report["prepared_panes"] == args.panes, report
        assert report["terminal_unchanged"] is True, report
        assert 0 <= report["focused_composed_index"] < args.panes, report
        if args.panes > 1:
            assert report["focused_composed_index"] > 0, report
        assert report["layer_enabled"] == (variant == "B"), report
        assert report["native_keyboard_focus"] is False, report
        assert report["internal_focus"] is True, report
        assert report["gpu_completion_measured"] is False, report
        assert report["visible_presentation_measured"] is False, report
        settings = {key: report[key] for key in
                    ["size_pixels", "scale_factor", "monitor", "outer_position", "renderer",
                     "pane_count", "prepared_panes", "focused_composed_index"]}
        assert all(value is not None for value in settings.values()), settings
        if control is None:
            control = settings
        assert settings == control, (directory.name, settings, control)
        results.append({"run": directory.name, "variant": variant, "report": report})
        (output / "results.json").write_text(json.dumps(results, indent=2) + "\n")
        print({key: report[key] for key in [
            "process_cpu_seconds", "elapsed_seconds", "process_cpu_percent_of_one_core",
            "frames", "main_terminal_paints", "pane_prepares", "layer_updates",
            "layer_activations",
        ]}, flush=True)

    summary = {"controls": control, "variants": {}}
    for variant in ["A", "B"]:
        reports = [result["report"] for result in results if result["variant"] == variant]
        cpu = sum(report["process_cpu_seconds"] for report in reports)
        elapsed = sum(report["elapsed_seconds"] for report in reports)
        assert len(reports) == 2 and elapsed > 0 and cpu >= 0, reports
        summary["variants"][variant] = {
            "process_cpu_seconds": cpu, "elapsed_seconds": elapsed,
            "process_cpu_percent_of_one_core": 100 * cpu / elapsed,
            **{key: sum(report[key] for report in reports) for key in [
                "frames", "main_terminal_paints", "pane_prepares", "layer_updates",
                "layer_activations",
            ]},
        }
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    main()
