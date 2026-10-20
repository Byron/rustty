#!/usr/bin/env python3
"""Actual native Rustty stock-C connection and visible HWND reconciliation.

The native Rust implementation executes on Windows; a pinned leased POSIX C
server is only its interoperability peer. No remote child is a Windows pane.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
import traceback

from windows_native_views import Fixture, assert_rendered_workspaces, sha256, wait_json
from windows_scenarios import RemoteServer

TEST = "tmuxx::system::tests::stock_ssh_views_share_live_panes_and_survive_connection_restart"


def selected(reference, path):
    temporary = path.with_suffix(".pending")
    temporary.write_text(json.dumps(reference.identity), encoding="utf-8")
    temporary.replace(path)


def live_panes(reference):
    lines = reference.command("list-panes", "-a", "-F", "#{pane_id}:#{pane_pid}").stdout.decode("ascii").splitlines()
    return {pane[1:]: int(pid) for pane, pid in (line.split(":") for line in lines)}


def wait_clients_closed(reference):
    deadline = time.monotonic() + 10
    while reference.command("list-clients", "-F", "#{client_pid}").stdout.strip():
        if time.monotonic() > deadline:
            raise AssertionError("native Rustty SSH control clients remained after viewer exit")
        time.sleep(.05)


def assert_pane_observations(state, expected):
    # Stock C discovery preserves pane identity but intentionally has no native
    # process metadata. Bind each rendered terminal's application output to the
    # independently observed C PID, instead of treating null as a Windows PID.
    assert set(state["pane_processes"]) == set(expected), (state["pane_processes"], expected)
    for pane, pid in expected.items():
        text = state["pane_texts"][pane].replace("\r", "").replace("\n", "")
        assert f"C-PANE:{pid}" in text, (pane, pid, text)
        assert state["pane_processes"][pane] in (None, pid)


def connection_contract(reference, rustty, env, directory):
    for name in ("one", "two"):
        reference.command("new-session", "-d", "-s", name, "-x", "80", "-y", "24", "/bin/sh")
    reference.command("split-window", "-d", "-t", "one:0", "/bin/sh")
    reference.command("link-window", "-s", "one:0", "-t", "two:1")
    reference.command("set-buffer", "-b", "user-kept", "sentinel")
    usage = reference.command("list-commands").stdout.decode("utf-8")
    paste = next(line for line in usage.splitlines() if line.startswith("paste-buffer "))
    assert all(flag in paste for flag in "Srd"), paste
    floating = next((line for line in usage.splitlines() if line.startswith("new-pane ")), "")
    capabilities = {
        "RUSTTY_TEST_FLOATING": bool(floating),
        "RUSTTY_TEST_MODAL": "O" in floating.partition("[")[2].partition("]")[0],
        "RUSTTY_TEST_GET_CLIPBOARD": reference.command("show-options", "-s", "get-clipboard", check=False).returncode == 0,
    }
    for key, enabled in capabilities.items():
        if enabled:
            env[key] = "1"
        else:
            env.pop(key, None)
    result = subprocess.run(["cargo", "+1.95.0", "test", "--locked", "--offline", "-p", "rustty", "--features", "sessions",
                             TEST, "--", "--ignored", "--test-threads=1"], cwd=rustty, env=env,
                            capture_output=True, timeout=130, creationflags=subprocess.CREATE_NO_WINDOW)
    (directory / "connection.stdout").write_bytes(result.stdout)
    (directory / "connection.stderr").write_bytes(result.stderr)
    records = re.findall(r"^test (\S+) \.\.\. (ok|FAILED|ignored)\s*$", result.stdout.decode("utf-8"), re.MULTILINE)
    assert result.returncode == 0 and records == [(TEST, "ok")], (result.returncode, records, result.stderr[-4000:])
    assert b"__rustty_input_" not in reference.command("list-buffers", "-F", "#{buffer_name}").stdout
    assert reference.command("show-buffer", "-b", "user-kept").stdout == b"sentinel"
    wait_clients_closed(reference)
    return {"capabilities": capabilities, "completed_tests": records}


def gui_contract(reference, fixture, env, host):
    child = ["python3", "-u", "-c", "import os,time; print('C-PANE:'+str(os.getpid()),flush=True); time.sleep(150)"]
    reference.command("new-session", "-d", "-s", "one", "-x", "80", "-y", "24", *child)
    reference.command("split-window", "-d", "-h", "-t", "one:0", *child)
    original = live_panes(reference)

    def view(name, hold):
        directory = fixture.directory / name
        directory.mkdir()
        settings = env | {"RUSTTY_TMUXX_SMOKE_DIR": str(directory), "RUSTTY_TMUXX_SMOKE_VISIBLE": "1",
                          "RUSTTY_TMUXX_SMOKE_ATTACH_ONLY": "1"}
        if hold:
            (directory / "hold").write_text("hold")
            settings["RUSTTY_TMUXX_SMOKE_HOLD_FILE"] = str(directory / "hold")
        process = fixture.start([str(fixture.gui), "--tmuxx=system-ssh:" + host, "--renderer=software",
                                 "--config-default-files=false", "--stream-deck=false"], name, settings)
        value = wait_json(directory / ("ready.json" if hold else "result.json"), process, 45)
        assert not value["errors"] and not value["hidden"] and value["view_graph_consistent"], value
        assert_rendered_workspaces(value)
        return process, directory, value

    process, directory, before = view("c-shared-graph", True)
    assert_pane_observations(before, original)
    reference.command("new-session", "-d", "-s", "external", *child)
    reference.command("new-window", "-d", "-n", "C Native 雪", "-t", "one", *child)
    reference.command("split-window", "-d", "-h", "-t", "one:0", *child)
    reference.command("link-window", "-s", "one:0", "-t", "external:7")
    reference.command("link-window", "-s", "one:0", "-t", "one:9")
    reference.command("new-session", "-d", "-s", "grouped", "-t", "one")
    (directory / "expected.json").write_text(json.dumps({"sessions": 3, "windows": 3, "panes": 5, "links": 8}))
    (directory / "hold").unlink()
    after = wait_json(directory / "result.json", process, 45)
    assert not after["errors"] and after["view_graph_consistent"] and after["ui_labels_supported"], after
    assert_rendered_workspaces(after)
    assert len(after["rendered_hosts"]) == 3 and len(after["view_windows"]) == 8 and len(after["view_panes"]) == 18
    actual = live_panes(reference)
    assert_pane_observations(after, actual)
    assert all(actual[pane] == pid for pane, pid in original.items())
    for native in after["native_hosts"]:
        assert (directory / f"window-{native}.png").read_bytes().startswith(b"\x89PNG\r\n\x1a\n")
    assert process.wait(timeout=20) == 0
    wait_clients_closed(reference)
    assert live_panes(reference) == actual
    fixture.checks.append("windows_system_c_views::visible_shared_graph")
    process, restart_directory, restarted = view("c-reconnected-graph", False)
    assert process.wait(timeout=20) == 0
    assert_pane_observations(restarted, actual)
    assert len(restarted["rendered_hosts"]) == 3
    wait_clients_closed(reference)
    assert live_panes(reference) == actual
    fixture.checks.append("windows_system_c_views::gui_restart_preserves_remote_panes")
    return {"original_panes": original, "final_panes": actual, "before": before, "after": after, "restarted": restarted}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != "win32":
        parser.error("requires native Windows and an interactive desktop")
    required = ("TMUXX_REMOTE_C_CONFIG", "TMUXX_SYSTEM_C_SSH_CONFIG", "TMUXX_SYSTEM_C_SSH_HOST", "TMUXX_SYSTEM_C_SELECTION")
    for name in required:
        if not os.environ.get(name):
            parser.error("explicit desktop/C-SSH prerequisite missing: " + name)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="windows-system-c-views-", dir=args.report.parent))
    fixture = Fixture(args.gui.resolve(strict=True), args.candidate.resolve(strict=True), directory)
    result = {"schema": 1, "kind": "windows-system-c-views", "artifact_directory": str(directory),
              "gui_sha256": sha256(fixture.gui), "candidate_sha256": sha256(fixture.candidate),
              "tests": [], "failures": [], "cleanup_remaining": []}
    ssh_config = Path(os.environ["TMUXX_SYSTEM_C_SSH_CONFIG"]).resolve(strict=True)
    result["system_ssh_files"] = {name: {"path": str(path), "sha256": sha256(path)} for name, path in
                                  (("config", ssh_config), ("key", ssh_config.parent / "identity"),
                                   ("known_hosts", ssh_config.parent / "known_hosts"))}
    selection = Path(os.environ["TMUXX_SYSTEM_C_SELECTION"])
    host = os.environ["TMUXX_SYSTEM_C_SSH_HOST"]
    env = fixture.env | {"RUSTTY_TMUXX_SSH_CONFIG": os.environ["TMUXX_SYSTEM_C_SSH_CONFIG"], "RUSTTY_TEST_SSH_HOST": host}
    reference = None
    try:
        # Compile before the remote fixture lease starts. This is preparation,
        # never a named-test result or a substitute for executable evidence.
        rustty = fixture.gui.parents[3]
        build = subprocess.run(["cargo", "+1.95.0", "test", "--locked", "--offline", "-p", "rustty", "--features", "sessions",
                                TEST, "--no-run"], cwd=rustty, env=env, capture_output=True, timeout=300,
                               creationflags=subprocess.CREATE_NO_WINDOW)
        (directory / "build.stdout").write_bytes(build.stdout)
        (directory / "build.stderr").write_bytes(build.stderr)
        assert build.returncode == 0, build.stderr[-4000:]
        reference = RemoteServer(os.environ["TMUXX_REMOTE_C_CONFIG"])
        selected(reference, selection)
        result["connection"] = connection_contract(reference, rustty, env, directory)
        fixture.checks.append(TEST)
        result["connection_trace"] = reference.trace
        reference.close()
        reference = None
        reference = RemoteServer(os.environ["TMUXX_REMOTE_C_CONFIG"])
        selected(reference, selection)
        result["gui"] = gui_contract(reference, fixture, env, host)
        result["gui_trace"] = reference.trace
    except (Exception, KeyboardInterrupt) as error:
        result["failures"].append(f"{type(error).__name__}: {error}")
        traceback.print_exc(file=sys.stderr)
    finally:
        result["cleanup_remaining"] = fixture.cleanup()
        if reference is not None:
            try:
                reference.close()
            except (Exception, KeyboardInterrupt) as error:
                result["failures"].append("remote cleanup: " + str(error))
        selection.unlink(missing_ok=True)
        result["tests"] = fixture.checks
        result["passed"] = not result["failures"] and not result["cleanup_remaining"]
        args.report.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(result))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
