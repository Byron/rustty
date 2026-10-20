"""Exact provenance and fixture accounting for native tests with remote C oracles."""
import base64
import json
import os
from pathlib import Path
import shutil
import sys

import remote_c_reference as oracle

HERE = Path(__file__).resolve().parent


def file_identity(path):
    path = Path(path).resolve(strict=True)
    return {"path": str(path), "sha256": oracle.digest(path)}


def config_path(suite):
    name = suite.get("remote_config_env", "TMUXX_REMOTE_C_CONFIG")
    value = os.environ.get(name)
    if not value:
        raise ValueError(f"{suite['id']} needs explicit {name} with pinned remote C identity and SSH credentials")
    return Path(value).resolve(strict=True)


def system_ssh_identity(run_process, selected=None):
    selected = selected or {}
    config = selected.get("config", {}).get("path") or os.environ.get("TMUXX_SYSTEM_C_SSH_CONFIG")
    host = selected.get("host") or os.environ.get("TMUXX_SYSTEM_C_SSH_HOST")
    ssh = selected.get("ssh", {}).get("path") or os.environ.get("TMUXX_SYSTEM_C_SSH")
    if (not config or not ssh or not isinstance(host, str) or not host
            or host.startswith("-") or any(char.isspace() or char == "\0" for char in host)):
        raise ValueError("raw C SSH requires explicit native SSH executable, restricted config and host alias")
    result = {"config": file_identity(config), "ssh": file_identity(ssh)}
    command = [result["ssh"]["path"], "-G", "-T", "-F", result["config"]["path"], "--", host]
    resolved = run_process(command, cwd=HERE, env=os.environ.copy(), timeout=15)
    if resolved.returncode:
        raise ValueError("cannot inspect restricted raw C SSH configuration")
    fields = {}
    for line in resolved.stdout.decode("utf-8").splitlines():
        key, separator, value = line.partition(" ")
        if separator:
            fields.setdefault(key, []).append(value)
    for name, key in (("key", "identityfile"), ("known_hosts", "userknownhostsfile")):
        values = fields.get(key, [])
        if len(values) != 1:
            raise ValueError("raw C SSH fixture requires exactly one " + key)
        result[name] = file_identity(values[0])
    if any(fields.get(key) != [value] for key, value in
           (("batchmode", "yes"), ("identitiesonly", "yes"), ("stricthostkeychecking", "true"), ("requesttty", "false"))):
        raise ValueError("raw C SSH configuration must pin keys, known hosts and no-PTY batch operation")
    for file in result.values():
        if file_identity(file["path"]) != file:
            raise ValueError("raw C SSH credentials changed during inspection")
    return result | {"host": host}


def identity(suite, run_process, selected=None, system_selected=None):
    path = Path(selected) if selected is not None else config_path(suite)
    config_identity = file_identity(path)
    config = oracle.validate_config(json.loads(path.read_text(encoding="utf-8")))
    if config.get("version") != suite.get("reference_version"):
        raise ValueError(f"{suite['id']} requires exact remote {suite.get('reference_version')}")
    ssh = shutil.which(config.get("ssh", "ssh"))
    if not ssh:
        raise ValueError("configured Windows SSH executable is unavailable")
    expected = {"config": config_identity, "helper": file_identity(HERE / "remote_c_reference.py"),
                "support": file_identity(HERE / "remote_reference.rs"), "ssh": file_identity(ssh),
                "control_support": file_identity(HERE / "remote_c_control.py"),
                "known_hosts": file_identity(config["known_hosts"]), "key": file_identity(config["key"])}
    command = [sys.executable, str(HERE / "remote_c_reference.py"), "--config", str(path), "--probe"]
    result = run_process(command, cwd=HERE, env=os.environ.copy(), timeout=45)
    if result.returncode:
        raise ValueError("remote C identity probe failed: " + result.stderr.decode("utf-8", "replace")[-2000:])
    remote = json.loads(result.stdout)
    fields = {"schema": 1, "binary": config["binary"], "version": config["version"],
              "sha256": config["sha256"], "helper_sha256": expected["helper"]["sha256"],
              "control_helper_sha256": oracle.CONTROL_SOURCE_SHA256}
    if (not isinstance(remote, dict) or type(remote.get("schema")) is not int
            or any(remote.get(key) != value for key, value in fields.items())
            or not isinstance(remote.get("host"), list) or len(remote["host"]) != 5
            or not all(isinstance(item, str) for item in remote["host"])
            or not isinstance(remote.get("libc"), list) or len(remote["libc"]) != 2
            or not all(isinstance(item, str) and item for item in remote["libc"])):
        raise ValueError("remote C identity probe returned inconsistent provenance")
    for file in expected.values():
        if file_identity(file["path"]) != file:
            raise ValueError("remote oracle configuration, credentials or source changed during probe")
    names = suite.get("remote_observers", [])
    if not isinstance(names, list) or any(name not in oracle.OBSERVERS for name in names):
        raise ValueError("remote suite names an unapproved C observer")
    expected["observer_sources"] = {relative: file_identity(oracle.observer_path(relative))
                                   for name in names for relative in oracle.OBSERVERS[name]}
    expected["remote"] = fields | {"host": remote["host"], "libc": remote["libc"]}
    if suite.get("raw_system_ssh"):
        expected["system_ssh"] = system_ssh_identity(run_process, system_selected)
    return expected


def fixtures(suite, stderr, pinned):
    records = {"identity": [], "cleanup": []}
    observers = []
    for line in stderr.splitlines():
        if line.startswith("TMUXX_REMOTE_C_OBSERVER="):
            observers.append(json.loads(line[len("TMUXX_REMOTE_C_OBSERVER="):]))
        for kind, prefix in (("identity", "TMUXX_REMOTE_C_IDENTITY="), ("cleanup", "TMUXX_REMOTE_C_CLEANUP=")):
            if line.startswith(prefix):
                records[kind].append(json.loads(line[len(prefix):]))
    count = suite.get("remote_fixtures")
    if type(count) is not int or count < 1 or any(len(values) != count for values in records.values()):
        raise ValueError(f"{suite['id']} lacks its exact {count} remote identities and cleanup completions")
    sockets = set()
    for value in records["identity"]:
        if (not isinstance(value, dict) or type(value.get("schema")) is not int
                or any(value.get(key) != expected for key, expected in pinned["remote"].items())
                or type(value.get("pid")) is not int or value["pid"] <= 0
                or type(value.get("server_pid")) is not int or value["server_pid"] <= 0
                or value.get("ownership") != "linux-child-subreaper"
                or not isinstance(value.get("socket"), str) or not value["socket"].startswith("/")
                or value["socket"] in sockets):
            raise ValueError("remote C fixture identity is duplicated or differs from the pinned probe")
        sockets.add(value["socket"])
    closed = set()
    for value in records["cleanup"]:
        if (not isinstance(value, dict) or type(value.get("schema")) is not int or value["schema"] != 1
                or value.get("cleanup_remaining") != [] or value.get("cleanup_errors") != []
                or value.get("fixture") not in sockets or value["fixture"] in closed):
            raise ValueError("remote C fixture has missing, duplicated or failed cleanup")
        closed.add(value["fixture"])
    expected_observers = suite.get("remote_observers", [])
    if sorted(value.get("name", "") for value in observers) != sorted(expected_observers):
        raise ValueError("remote C observer completions do not match the exact named matrix")
    for value in observers:
        name = value["name"]
        if (type(value.get("schema")) is not int or value["schema"] != 1 or value.get("fixture") not in sockets
                or value.get("cleanup_remaining") != [] or value.get("cleanup_errors") != []
                or value.get("sources") != {relative: oracle.OBSERVER_SOURCES[relative] for relative in oracle.OBSERVERS[name]}):
            raise ValueError("remote C observer source or cleanup identity differs")
    records["observers"] = observers
    return records


def verify(suite, details, run_process):
    recorded = details.get("remote_reference")
    if not isinstance(recorded, dict) or not isinstance(recorded.get("config"), dict):
        raise ValueError(f"{suite['id']} lacks exact remote reference provenance")
    current = identity(suite, run_process, recorded["config"].get("path"), recorded.get("system_ssh"))
    if current != recorded:
        raise ValueError("remote C source, credentials, host or binary changed since collection")
    stderr = base64.b64decode(details.get("stderr_base64", ""), validate=True).decode("utf-8", "replace")
    if fixtures(suite, stderr, recorded) != details.get("remote_fixtures"):
        raise ValueError("remote C fixture records changed since collection")
