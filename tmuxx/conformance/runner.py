#!/usr/bin/env python3
"""Exact executable C/C, C/R and named-suite evidence; complete profiles gate separately."""

from __future__ import annotations

import argparse
import base64
import collections
import hashlib
import json
import os
import platform
from pathlib import Path
import re
import secrets
import select
import signal
import subprocess
import sys
import tempfile
import time
import tomllib

import generate_command_metadata
import generate_option_metadata
import generate_format_metadata
import remote_c_evidence
from source_paths import c_source


HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
MANIFEST = HERE / "manifest.toml"
SCOPE = HERE / "scope.json"
UNIX_PLATFORMS = ("darwin", "linux")
WINDOWS_NATIVE_KINDS = ("windows-native-views", "windows-native-input", "windows-native-clipboard", "windows-native-ime", "windows-native-frame", "windows-lifetime", "windows-cli-state", "windows-system-c-views", "windows-ssh")


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def checkout_line_endings(root):
    """Identify Git's text checkouts without changing files or trusting extensions."""
    records = subprocess.check_output(["git", "ls-files", "--eol", "-z"], cwd=root)
    return {os.fsdecode(path): fields.split() for record in records.split(b"\0") if record
            for fields, path in [record.split(b"\t", 1)]}


def source_digest(path, root, endings):
    """Compare the LF source ledger with an explicitly identified CRLF checkout.

    Raw hashes still identify binaries, reports, and the complete implementation.
    Only Git-index LF text with uniform CRLF working-tree lines is canonicalized;
    mixed endings and binary content remain byte-exact review changes.
    """
    fields = endings.get(Path(path).relative_to(root).as_posix(), [])
    if fields[:2] == [b"i/lf", b"w/crlf"] and b"attr/-text" not in fields:
        return hashlib.sha256(Path(path).read_bytes().replace(b"\r\n", b"\n")).hexdigest()
    return digest(path)


def rustty_root():
    return Path(os.environ.get("TMUXX_RUSTTY_ROOT", ROOT)).resolve()


def rustty_sources(root):
    paths = [root / "Cargo.toml", root / "Cargo.lock"]
    paths += [path for path in (root / "crates").rglob("*")
              if path.is_file() and not {"target", ".git"}.intersection(path.relative_to(root).parts)]
    return sorted(set(paths))


def rustty_identity():
    root = rustty_root()
    if not (root / "Cargo.toml").is_file():
        raise ValueError(f"Rustty checkout missing: {root}; set TMUXX_RUSTTY_ROOT to the integrated checkout")
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    sources = {path.relative_to(root).as_posix(): digest(path) for path in rustty_sources(root)}
    return {"root": str(root), "revision": revision, "sources": sources}


def implementation_fingerprint():
    paths = list((ROOT / "tmuxx/crates").rglob("*.rs"))
    paths += [path for path in (ROOT / "tmuxx/test-support").rglob("*") if path.is_file()]
    paths += list((ROOT / "tmuxx/crates").rglob("Cargo.toml"))
    paths += [path for path in (ROOT / "tmuxx/vendor").rglob("*") if path.is_file() and not {"target", ".git"}.intersection(path.parts)]
    paths += [ROOT / "Cargo.toml", ROOT / "Cargo.lock", SCOPE]
    paths += [path for path in (ROOT / "tmuxx/windows").rglob("*") if path.is_file()]
    paths += list(HERE.glob("*.toml")) + list(HERE.glob("*.py")) + list(HERE.glob("*.rs"))
    hasher = hashlib.sha256()
    for path in sorted(paths):
        hasher.update(str(path.relative_to(ROOT)).encode())
        hasher.update(path.read_bytes())
    external = rustty_root()
    hasher.update(b"rustty-sources")
    try:
        hasher.update(subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=external, stderr=subprocess.DEVNULL))
    except (OSError, subprocess.SubprocessError):
        hasher.update(b"missing-checkout-revision")
    for path in rustty_sources(external):
        hasher.update(str(path.relative_to(external)).encode())
        hasher.update(path.read_bytes() if path.is_file() else b"missing")
    return hasher.hexdigest()


def manifest():
    return tomllib.loads(MANIFEST.read_text(encoding="utf-8"))


def suites():
    found = {}
    for path in sorted(HERE.glob("*-evidence.toml")):
        for suite in tomllib.loads(path.read_text(encoding="utf-8"))["suites"]:
            identifier = suite["id"]
            if identifier in found:
                raise ValueError(f"duplicate evidence suite: {identifier}")
            if not suite["assertions"] or any(not assertion["tests"] for assertion in suite["assertions"]):
                raise ValueError(f"suite {identifier} needs exact named tests for every assertion")
            allowed = suite.get("platforms", list(UNIX_PLATFORMS))
            if not allowed or len(allowed) != len(set(allowed)) or set(allowed) - {"darwin", "linux", "win32"}:
                raise ValueError(f"suite {identifier} has invalid execution platforms")
            found[identifier] = {**suite, "source": path.relative_to(ROOT).as_posix()}
    return found


def suite_platforms(suite):
    return suite.get("platforms", list(UNIX_PLATFORMS))


def select_suites(requested, host_platform):
    """Keep unavailable executable obligations visible without running zero tests."""
    available = suites()
    selected = available if "all" in requested else {name: available[name] for name in requested}
    config = manifest()
    domains = {identifier: domain["platforms"] for domain in config.get("applicability", {}).values()
               for identifier in domain.get("assertions", [])}

    def applies(suite, assertion):
        if host_platform not in assertion.get("platforms", domains.get(assertion["id"], [host_platform])):
            return False
        profiles = set(suite.get("profiles", []))
        for profile, definition in config["profiles"].items():
            if profiles.intersection(definition.get("includes", [])):
                profiles.add(profile)
        return any(host_platform in config["profiles"][profile]["platforms"] for profile in profiles)

    counterparts = {assertion["id"] for suite in available.values() if host_platform in suite_platforms(suite)
                    for assertion in suite["assertions"] if applies(suite, assertion)}
    runnable, blocked = {}, {}
    for name, suite in selected.items():
        if host_platform in suite_platforms(suite):
            runnable[name] = suite
        elif any(applies(suite, assertion) and ("all" not in requested or assertion["id"] not in counterparts)
                 for assertion in suite["assertions"]):
            blocked[name] = {"status": "blocked", "platform": host_platform,
                             "execution_platforms": suite_platforms(suite), "completed_tests": {},
                             "reason": "Required client execution is unavailable on this host; supported desktop/client evidence remains required."}
        elif "all" not in requested:
            raise ValueError(f"suite {name} does not apply to {host_platform}")
    return runnable, blocked


def host_identity():
    def version(command):
        try:
            result = subprocess.run(command, capture_output=True, timeout=5)
            return {"argv": command, "status": result.returncode,
                    "output": (result.stdout + result.stderr).decode("utf-8", "replace").strip()}
        except (OSError, subprocess.SubprocessError) as error:
            return {"argv": command, "error": str(error)}
    rust = version(["rustc", "+1.95.0", "-vV"])
    target = re.search(r"^host: (.+)$", rust.get("output", ""), re.MULTILINE)
    distribution = {}
    if sys.platform == "linux":
        try:
            distribution = platform.freedesktop_os_release()
        except OSError:
            pass
    server = str(Path(os.environ.get("SystemRoot", "C:/Windows")) / "System32/OpenSSH/sshd.exe") if sys.platform == "win32" else "/usr/sbin/sshd"
    return {"platform": sys.platform, "system": platform.system(), "version": platform.version(),
            "architecture": platform.machine(), "kernel": platform.release(), "release": platform.release(),
            "distribution": distribution, "libc": list(platform.libc_ver()),
            "rust": rust, "rust_host_target": target.group(1) if target else None,
            "cargo_build_target": os.environ.get("CARGO_BUILD_TARGET"),
            "python": {"executable": sys.executable, "version": sys.version},
            "openssh_client": version(["ssh", "-V"]), "openssh_server": version([server, "-V"])}


def source_revision():
    return subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()


def inventory(config=None, host_platform=None):
    if generate_command_metadata.OUTPUT.read_text(encoding="utf-8") != generate_command_metadata.generate():
        raise ValueError("C command metadata changed; regenerate and review metadata.rs and parity coverage")
    if generate_format_metadata.OUTPUT.read_text(encoding="utf-8") != generate_format_metadata.generate():
        raise ValueError("C format callbacks changed; regenerate and review format metadata and conformance")
    if generate_option_metadata.OUTPUT.read_text(encoding="utf-8") != generate_option_metadata.generate():
        raise ValueError("C option metadata changed; regenerate and review options_metadata.rs and parity coverage")
    config = config or manifest()
    host_platform = host_platform or sys.platform
    entries = []
    for assertion in config["assertions"]:
        entries.append({**assertion, "requirement": "required", "status": "pending"})
    for group in config["requirements"]:
        for identifier in group["ids"]:
            deferred = all(config["profiles"][p].get("execution") == "deferred" for p in group["profiles"])
            entries.append({"id": identifier, "profiles": group["profiles"], "requirement": "required",
                            "status": "deferred" if deferred else "pending"})
    entries.extend(scope_inventory(config))
    by_id = {entry["id"]: entry for entry in entries}
    for suite in suites().values():
        if any(profile not in config["profiles"] for profile in suite["profiles"]):
            raise ValueError(f"unknown profile in suite {suite['id']}")
        for assertion in suite["assertions"]:
            identifier = assertion["id"]
            if identifier.startswith(("scope:", "review:", "outside:")):
                raise ValueError("narrow suite evidence cannot certify a scope group or classification")
            applicability = assertion.get("platforms")
            if applicability is not None and (not applicability or set(applicability) - set(suite_platforms(suite))):
                raise ValueError(f"assertion {identifier} has invalid applicability platforms")
            if identifier in by_id:
                entry = by_id[identifier]
                if entry.get("platforms") != applicability:
                    raise ValueError(f"assertion {identifier} has inconsistent applicability platforms")
                entry["profiles"] = sorted(set(entry["profiles"]) | set(suite["profiles"]))
                entry.setdefault("description", assertion["description"])
                entry.setdefault("suites", []).append(suite["id"])
            else:
                entry = {"id": identifier, "profiles": suite["profiles"], "requirement": "required", "status": "pending",
                         "description": assertion["description"], "suites": [suite["id"]]}
                if applicability is not None:
                    entry["platforms"] = applicability
                by_id[identifier] = entry
                entries.append(entry)
    for exclusion in config["exclusions"]:
        for identifier in exclusion["assertions"]:
            entries.append({"id": identifier, "profiles": ["presentation"], "requirement": "outside-profile",
                            "status": "outside-profile", "reason": exclusion["reason"]})
    identifiers = [entry["id"] for entry in entries]
    if len(identifiers) != len(set(identifiers)):
        raise ValueError("duplicate conformance assertion IDs")
    domains = set()
    for name, domain in config.get("applicability", {}).items():
        platforms = domain.get("platforms", [])
        if (not platforms or not set(platforms) <= config.get("platform_execution", {}).keys()
                or not domain.get("rationale", "").strip()):
            raise ValueError(f"invalid platform applicability domain: {name}")
        for identifier in domain.get("assertions", []):
            if identifier not in by_id or identifier in domains:
                raise ValueError(f"unknown or duplicate platform applicability assertion: {identifier}")
            by_id[identifier].update(platforms=platforms[:], platform_domain=name,
                                     platform_rationale=domain["rationale"])
            domains.add(identifier)
    # An execution profile inherits obligations, never their previous evidence.
    for profile, definition in config["profiles"].items():
        shared = set(definition.get("includes", []))
        if shared - config["profiles"].keys() or profile in shared:
            raise ValueError(f"invalid shared profiles for {profile}")
        for entry in entries:
            if (shared.intersection(entry["profiles"])
                    and (not entry.get("platforms") or set(definition["platforms"]).intersection(entry["platforms"]))):
                entry["profiles"] = sorted(set(entry["profiles"]) | {profile})
    obligations = {identifier for group in config["requirements"] for identifier in group["ids"]}
    obligations.update(identifier for identifier in by_id if identifier.startswith("scope:"))
    covered = set()
    for coverage in config.get("coverage", []):
        identifier = coverage["obligation"]
        proof = coverage.get("assertions", [])
        platform_proofs = coverage.get("platform_assertions", {})
        if identifier not in obligations or identifier in covered:
            raise ValueError(f"unknown or duplicate coverage obligation: {identifier}")
        if by_id[identifier].get("suites") or by_id[identifier].get("scenario"):
            raise ValueError(f"coverage {identifier} already has direct evidence; split exact assertions before adding a proof")
        if not proof or len(proof) != len(set(proof)) or not coverage.get("rationale", "").strip():
            raise ValueError(f"coverage {identifier} needs distinct exact assertions and a review rationale")
        applicable = {target for profile in by_id[identifier]["profiles"] for target in config["profiles"][profile]["platforms"]}
        if platform_proofs and set(platform_proofs) != applicable:
            raise ValueError(f"coverage {identifier} needs an exact proof mapping for every applicable platform")
        for target, additions in platform_proofs.items():
            if not additions or len(proof + additions) != len(set(proof + additions)):
                raise ValueError(f"coverage {identifier} has empty or duplicate {target} proof members")
            for source in additions:
                if target not in by_id.get(source, {}).get("platforms", [target]):
                    raise ValueError(f"coverage {identifier} uses an inapplicable {target} proof: {source}")
        all_sources = proof + [source for additions in platform_proofs.values() for source in additions]
        for source in all_sources:
            entry = by_id.get(source, {})
            if source == identifier or source.startswith(("scope:", "review:", "outside:")) or not (entry.get("suites") or entry.get("scenario")):
                raise ValueError(f"coverage {identifier} has no exact executable proof: {source}")
        by_id[identifier]["proof"] = proof + platform_proofs.get(host_platform, [])
        if platform_proofs:
            by_id[identifier]["platform_proofs"] = platform_proofs
        by_id[identifier]["coverage_review"] = coverage["rationale"]
        covered.add(identifier)
    if any(source in covered for identifier in covered for source in by_id[identifier]["proof"]):
        raise ValueError("coverage proofs must be executable assertions, not other derived obligations")
    for entry in entries:
        if host_platform not in entry.get("platforms", [host_platform]):
            entry["status"] = "not-applicable"
    return entries


def discovered(config):
    root = c_source()
    regressions = {path.relative_to(root).as_posix() for path in root.glob(config["discovery"]["regressions"])}
    commands = {name for path in root.glob(config["discovery"]["commands"])
                for name in re.findall(r'\.name\s*=\s*"([^"]+)"', path.read_text(encoding="utf-8"))}
    options = set(re.findall(r'\.name\s*=\s*"([^"]+)"', (root / config["discovery"]["options"]).read_text(encoding="utf-8")))
    source = (root / config["discovery"]["formats"]).read_text(encoding="utf-8")
    table = source.split("static const struct format_table_entry format_table[] = {", 1)[1].split("\n};", 1)[0]
    formats = set(re.findall(r'\{\s*"([^"]+)"\s*,\s*FORMAT_TABLE_', table))
    return dict(regressions=regressions, commands=commands, options=options, formats=formats)


def scope_inventory(config):
    root = c_source()
    scope = json.loads(SCOPE.read_text(encoding="utf-8"))
    found = discovered(config)
    mapped = {kind: set() for kind in found}
    entries = []
    for group in scope["groups"]:
        for kind in found:
            for name in group[kind]:
                if name in mapped[kind]:
                    raise ValueError(f"duplicate scope mapping: {kind}:{name}")
                mapped[kind].add(name)
        sources = group["regressions"]
        surface = {kind: group[kind] for kind in ("commands", "options", "formats") if group[kind]}
        for number, description in enumerate(group["required"], 1):
            entries.append({"id": f"scope:{group['id']}:{number}", "profiles": group["profiles"],
                            "requirement": "required", "status": "pending", "description": description,
                            "sources": sources, "surface": surface})
        for number, description in enumerate(group["outside"], 1):
            entries.append({"id": f"outside:{group['id']}:{number}", "profiles": ["presentation"],
                            "requirement": "outside-profile", "status": "outside-profile", "reason": description,
                            "sources": sources, "surface": surface})
    for kind in found:
        for name in sorted(found[kind] ^ mapped[kind]):
            entries.append({"id": f"review:{kind}:{name}", "profiles": ["classification"],
                            "requirement": "required", "status": "pending",
                            "reason": "New or removed upstream surface needs explicit assertion disposition."})
    paths = {path.relative_to(root).as_posix() for path in (root / "regress").rglob("*") if path.is_file()}
    paths |= {path.relative_to(root).as_posix() for path in root.glob(config["discovery"]["commands"])
              if re.search(r'\.name\s*=', path.read_text(encoding="utf-8"))}
    paths |= {config["discovery"]["options"], config["discovery"]["formats"]}
    tracked = subprocess.check_output(
        ["git", "ls-files", "-z", "--", "*.c", "*.h", "*.y", "*.l", "*.ac", "*.am"], cwd=root)
    paths |= {os.fsdecode(path) for path in tracked.split(b"\0") if path}
    reviewed = scope["source_sha256"]
    endings = checkout_line_endings(root)
    for path in sorted(paths | reviewed.keys()):
        if path not in paths or path not in reviewed or source_digest(root / path, root, endings) != reviewed[path]:
            entries.append({"id": "review:source:" + path, "profiles": ["classification"],
                            "requirement": "required", "status": "pending",
                            "reason": "Source, fixture or shared helper changed; re-review mapped assertions before updating its hash."})
    if scope["reference_revision"] != config["reference_revision"]:
        entries.append({"id": "review:scope-baseline", "profiles": ["classification"],
                        "requirement": "required", "status": "pending",
                        "reason": "Manifest and reviewed assertion scope name different reference revisions."})
    return entries


def counts(entries):
    return dict(sorted(collections.Counter(entry["status"] for entry in entries).items()))


def apply_observations(entries, observations):
    by_id = {entry["id"]: entry for entry in entries}
    for observation in observations:
        identifier = observation["assertion"]
        if identifier not in by_id:
            raise ValueError(f"undeclared assertion: {identifier}")
        if identifier.startswith(("review:", "scope:")) or "proof" in by_id[identifier]:
            raise ValueError("classification requires scope review, not test evidence")
        if by_id[identifier]["requirement"] != "required" or by_id[identifier]["status"] == "not-applicable":
            raise ValueError(f"cannot turn an exclusion into a passing assertion: {identifier}")
        results = by_id[identifier].setdefault("observation_results", {})
        source = observation["scenario"]
        if results.get(source) != "failure":
            results[source] = observation["status"]
        by_id[identifier].setdefault("evidence", []).append(observation["scenario"])
    derive_statuses(entries)
    return entries


def platform_inventory(entries, available, evidence_platform):
    """Choose executable counterparts without removing any required assertion."""
    for entry in entries:
        entry["applicable"] = not entry.get("platforms") or evidence_platform in entry["platforms"]
        declared = entry.get("suites", [])
        entry["declared_suites"] = declared[:]
        entry["suites"] = [name for name in declared if evidence_platform in suite_platforms(available[name])]
        counterparts = [assertion.get("scenario_counterpart") for name in entry["suites"]
                        for assertion in available[name].get("assertions", []) if assertion["id"] == entry["id"]
                        and assertion.get("scenario_counterpart")]
        if counterparts:
            if not entry.get("scenario") or any(name != entry["scenario"] for name in counterparts):
                raise ValueError("scenario counterpart must name its exact original scenario")
            entry["declared_scenario"] = entry.pop("scenario")
    by_id = {entry["id"]: entry for entry in entries}
    for entry in entries:
        if "proof" in entry:
            entry["declared_proof"] = entry["proof"][:]
            # Only explicitly reviewed transport domains remove a common proof
            # member. Platform owner-death variants were selected in inventory;
            # an inapplicable common member must not silently weaken ALL-of.
            entry["proof"] = [identifier for identifier in entry["proof"]
                              if by_id[identifier]["applicable"] or not by_id[identifier].get("platform_domain")]
    return entries


def derive_statuses(entries):
    """Require every declared executable source, then every reviewed proof member."""
    by_id = {entry["id"]: entry for entry in entries}
    passing = {"pass", "intentional-difference"}
    for entry in entries:
        if entry["requirement"] != "required" or entry["status"] in ("deferred", "not-applicable") or "proof" in entry:
            continue
        results = entry.get("observation_results", {})
        required = {"suite:" + suite for suite in entry.get("suites", [])}
        if entry.get("scenario"):
            required.add(entry["scenario"])
        applicable_results = [status for source, status in results.items()
                              if not source.startswith("suite:") or source in required]
        entry["status"] = "failure" if "failure" in applicable_results else (
            "pass" if required and all(results.get(source) in passing for source in required) else "pending")
    for entry in entries:
        if "proof" not in entry or entry["status"] in ("deferred", "not-applicable"):
            continue
        statuses = [by_id.get(source, {}).get("status") for source in entry["proof"]]
        entry["status"] = "failure" if "failure" in statuses else (
            "pass" if statuses and all(status in passing for status in statuses) else "pending")


def b64(value):
    return base64.b64encode(value).decode("ascii")


class Server:
    def __init__(self, executable, kind):
        self.executable = str(Path(executable).resolve(strict=True))
        self.kind = kind
        # Keep socket names short enough for macOS sockaddr_un.
        self.directory = tempfile.TemporaryDirectory(prefix="txc-", dir="/tmp")
        self.endpoint = Path(self.directory.name) / "s"
        self.trace = []
        self.started = False
        self.base = [self.executable, "-f", "/dev/null"]
        self.base += ["-S" if kind == "c" else "--native-endpoint", str(self.endpoint)]
        self.env = {"PATH": "/usr/bin:/bin", "LC_ALL": "C.UTF-8", "TERM": "screen", "SHELL": "/bin/sh"}

    def command(self, *args, check=True):
        result = subprocess.run(self.base + (["-N"] if self.started else []) + list(args), env=self.env, cwd=ROOT,
                                capture_output=True, timeout=8)
        self.trace.append({"argv_base64": [b64(os.fsencode(arg)) for arg in args],
                           "status": result.returncode, "stdout_base64": b64(result.stdout),
                           "stderr_base64": b64(result.stderr)})
        if check and result.returncode:
            raise AssertionError(f"{args!r}: status {result.returncode}; stdout={result.stdout!r}; stderr={result.stderr!r}")
        return result

    def start(self, child="exec sleep 60"):
        self.command("new-session", "-d", "-s", "parity", "-n", "first", "-x", "80", "-y", "24", child)
        self.started = True
        self.command("has-session", "-t", "parity")
        if not self.endpoint.exists():
            raise AssertionError("chosen implementation did not create its isolated endpoint")

    def wait_capture(self, marker):
        deadline = time.monotonic() + 8
        while True:
            result = self.command("capture-pane", "-p", "-t", "parity:0.0")
            if marker in result.stdout:
                return result.stdout
            if time.monotonic() >= deadline:
                raise AssertionError(f"pane did not emit {marker!r}; last capture={result.stdout!r}")
            time.sleep(0.01)

    def close(self):
        try:
            self.command("kill-server", check=False)
        except (OSError, subprocess.TimeoutExpired):
            pass
        self.directory.cleanup()


def graph(server):
    server.start()
    server.command("split-window", "-h", "-d", "-t", "parity:0", "exec sleep 60")
    server.command("new-window", "-d", "-t", "parity", "-n", "second", "exec sleep 60")
    server.command("rename-window", "-t", "parity:1", "renamed")
    return {
        "sessions": server.command("list-sessions", "-F", "#{session_id}:#{session_name}:#{session_windows}").stdout,
        "windows": server.command("list-windows", "-t", "parity", "-F", "#{window_id}:#{window_index}:#{window_name}:#{window_panes}").stdout,
        "panes": server.command("list-panes", "-s", "-t", "parity", "-F", "#{window_id}:#{pane_id}:#{pane_index}").stdout,
    }


def formats(server):
    server.start()
    server.command("set-option", "-g", "@label", "value with spaces")
    expressions = ["#{session_name}:#{window_name}:#{pane_id}", "#{@label}",
                   "#{?#{==:#{session_name},parity},yes#,please,no}", "#{&&:1,0}",
                   "#S:#D:##:#,", "#{l:#{session_name}}"]
    return {expression: server.command("display-message", "-p", "-t", "parity:0.0", expression).stdout
            for expression in expressions}


def capture(server):
    server.start("printf 'ALPHA\\n\\033[31mRED\\033[0m\\nCAPTURE-READY'; exec sleep 60")
    return {"capture": server.wait_capture(b"CAPTURE-READY")}


def literal_input(server):
    server.start("stty raw -echo; printf INPUT-READY; exec cat")
    server.wait_capture(b"INPUT-READY")
    server.command("send-keys", "-l", "-t", "parity:0.0", "roundtrip-literal-$-semicolon\\;")
    return {"capture": server.wait_capture(b"roundtrip-literal-$-semicolon;")}


def linked_graph(server):
    server.start()
    server.command("new-session", "-d", "-s", "other", "-n", "second", "-x", "80", "-y", "24", "exec sleep 60")
    server.command("link-window", "-d", "-s", "parity:0", "-t", "other:5")
    graph_format = "#{session_name}:#{window_index}:#{window_id}:#{window_name}:#{window_panes}"
    result = {"linked": server.command("list-windows", "-a", "-F", graph_format).stdout}
    server.command("rename-window", "-t", "other:5", "shared")
    result["shared-rename"] = server.command("list-windows", "-a", "-F", graph_format).stdout
    server.command("move-window", "-d", "-s", "other:0", "-t", "parity:3")
    result["moved"] = server.command("list-windows", "-a", "-F", graph_format).stdout
    server.command("unlink-window", "-t", "other:5")
    result["unlinked"] = server.command("list-windows", "-a", "-F", graph_format).stdout
    result["surviving-sessions"] = server.command("list-sessions", "-F", "#{session_name}:#{session_windows}").stdout
    return result


def geometry(server):
    server.start()
    server.command("split-window", "-d", "-h", "-b", "-l", "20", "-t", "parity:0.0", "exec sleep 60")
    server.command("split-window", "-d", "-v", "-l", "5", "-t", "parity:0.0", "exec sleep 60")
    fields = "#{pane_id}:#{pane_index}:#{pane_left}:#{pane_top}:#{pane_width}:#{pane_height}:#{pane_active}"
    result = {"split": server.command("list-panes", "-t", "parity:0", "-F", fields).stdout}
    server.command("resize-window", "-t", "parity:0", "-x", "100", "-y", "30")
    result["resized"] = server.command("list-panes", "-t", "parity:0", "-F", fields).stdout
    return result


def nested_geometry(server):
    server.start()
    fields = "#{pane_id}:#{pane_left}:#{pane_top}:#{pane_width}:#{pane_height}:#{pane_active}"
    snapshot = lambda: server.command("list-panes", "-t", "parity:0", "-F", fields).stdout
    server.command("split-window", "-d", "-h", "-t", "parity:0", "exec sleep 60")
    result = {"default-split-80": snapshot()}
    server.command("resize-window", "-t", "parity:0", "-x", "100", "-y", "30")
    result["default-split-100"] = snapshot()
    server.command("split-window", "-d", "-v", "-t", "%0", "exec sleep 60")
    layout = b"100x30,0,0{30x30,0,0[30x9,0,0,0,30x20,0,10,2],69x30,31,0,1}"
    checksum = 0
    for byte in layout:
        checksum = (((checksum >> 1) | ((checksum & 1) << 15)) + byte) & 0xffff
    server.command("select-layout", "-t", "parity:0", f"{checksum:04x}," + layout.decode())
    result["custom-ratios"] = snapshot()
    server.command("resize-pane", "-t", "%2", "-y", "8")
    server.command("resize-pane", "-t", "%1", "-x", "40")
    result["last-pane-resize"] = snapshot()
    server.command("resize-pane", "-Z", "-t", "%2")
    result["zoom"] = snapshot()
    server.command("resize-pane", "-Z", "-t", "%2")
    result["unzoom"] = snapshot()
    server.command("kill-pane", "-t", "%2")
    result["remove-neighbor"] = snapshot()
    server.command("new-window", "-d", "-t", "parity:1", "-n", "before", "exec sleep 60")
    server.command("resize-window", "-t", "parity:1", "-x", "80", "-y", "24")
    server.command("split-window", "-d", "-b", "-h", "-t", "parity:1", "exec sleep 60")
    result["before-split"] = server.command("list-panes", "-t", "parity:1", "-F", fields).stdout
    return result


def option_validation(server):
    server.start()
    result = {}
    for name, valid, invalid in [("history-limit", "10", "-1"), ("base-index", "2", "invalid"),
                                  ("remain-on-exit", "on", "nonsense")]:
        server.command("set-option", "-g", name, valid)
        before = server.command("show-options", "-gqv", name).stdout
        rejected = server.command("set-option", "-g", name, invalid, check=False)
        after = server.command("show-options", "-gqv", name).stdout
        # Exact diagnostics remain in the trace; here the assertion is rejection
        # with no mutation, independent of source-path prefixes in an error.
        result[name] = str(rejected.returncode).encode() + b":" + before + b":" + after
    server.command("set-option", "-g", "@value", "a")
    server.command("set-option", "-ga", "@value", "b")
    result["user-option-append"] = server.command("show-options", "-gqv", "@value").stdout
    return result


def config_order(server):
    server.start()
    directory = Path(server.directory.name)
    child = directory / "child.conf"
    child.write_text("set-option -g @included included\n")
    source = directory / "main.conf"
    source.write_text("TMUXX_VALUE=early\nset-environment -g TMUXX_VALUE late\n"
                      "set-option -g @flag 1\n%if #{@flag}\nset-option -g @branch wrong\n"
                      "%else\nset-option -g @branch prepared\n%endif\n"
                      "set-option -g @value \"$TMUXX_VALUE\"\n"
                      f"source-file {child}\nset-option -gF @after '#{{@included}}'\n")
    server.command("set-option", "-g", "@flag", "0")
    server.command("source-file", str(source))
    return {name: server.command("show-options", "-gqv", name).stdout
            for name in ("@branch", "@value", "@included", "@after")}


def capture_ranges(server):
    server.start("printf 'FIRST\\nSECOND\\nTHIRD\\nCAPTURE-READY'; exec sleep 60")
    server.wait_capture(b"CAPTURE-READY")
    return {"bounded": server.command("capture-pane", "-p", "-t", "parity:0.0", "-S", "1", "-E", "2").stdout,
            "joined": server.command("capture-pane", "-pJ", "-t", "parity:0.0", "-S", "0", "-E", "2").stdout}


def environment(server):
    server.start()
    server.command("set-environment", "-g", "TMUXX_GLOBAL", "old")
    server.command("set-environment", "-g", "TMUXX_GLOBAL", "updated")
    server.command("set-environment", "-t", "parity", "TMUXX_GONE", "removed")
    server.command("set-environment", "-rt", "parity", "TMUXX_GONE")
    server.command("set-environment", "-ht", "parity", "TMUXX_HIDDEN", "hidden-value")
    server.command("set-environment", "-t", "parity", "TMUXX_QUOTE", 'a"$`\\z')
    result = {
        "removed": server.command("show-environment", "-t", "parity", "TMUXX_GONE").stdout,
        "hidden-filter": server.command("show-environment", "-t", "parity", "TMUXX_HIDDEN").stdout,
        "hidden": server.command("show-environment", "-ht", "parity", "TMUXX_HIDDEN").stdout,
        "quoted": server.command("show-environment", "-st", "parity", "TMUXX_QUOTE").stdout,
        "format-hidden": server.command("display-message", "-p", "-t", "parity:0", "#{TMUXX_HIDDEN}").stdout,
    }
    server.command("new-window", "-d", "-t", "parity", "-n", "environment",
                   "printf 'ENV:%s:%s:%s:END' \"${TMUXX_GLOBAL-unset}\" \"${TMUXX_GONE-unset}\" \"${TMUXX_HIDDEN-unset}\"; exec sleep 60")
    deadline = time.monotonic() + 5
    while True:
        data = server.command("capture-pane", "-pt", "parity:environment").stdout
        if b":END" in data:
            result["child"] = data
            break
        if time.monotonic() >= deadline:
            raise AssertionError(f"environment child did not start: {data!r}")
        time.sleep(0.01)
    return result


def scoped_options(server):
    server.start()
    server.command("set-option", "-t", "parity:0", "remain-on-exit", "on")
    server.command("set-option", "-t", "parity", "history-limit", "3")
    result = {
        "inferred-window-scope": server.command("show-window-options", "-vt", "parity:0", "remain-on-exit").stdout,
        "session-local": server.command("show-options", "-vt", "parity", "history-limit").stdout,
        "global-unchanged": server.command("show-options", "-gv", "history-limit").stdout,
        "absent-local": server.command("show-options", "-vt", "parity", "base-index").stdout,
        "inherited": server.command("show-options", "-Avt", "parity", "base-index").stdout,
    }
    server.command("set-option", "-ut", "parity", "history-limit")
    result["unset-local"] = server.command("show-options", "-vt", "parity", "history-limit").stdout
    result["unset-inherits"] = server.command("show-options", "-Avt", "parity", "history-limit").stdout
    server.command("set-option", "-gt", "parity", "history-limit", "17")
    server.command("set-option", "-gu", "history-limit")
    result["reset-global"] = server.command("show-options", "-gv", "history-limit").stdout
    return result


SCENARIOS = {"graph": graph, "format": formats, "capture": capture, "input": literal_input,
             "linked-graph": linked_graph, "geometry": geometry, "options": option_validation,
             "config-order": config_order, "capture-ranges": capture_ranges, "nested-geometry": nested_geometry,
             "environment": environment, "scoped-options": scoped_options}


def run_side(executable, kind, scenario):
    server = Server(executable, kind)
    try:
        observations = SCENARIOS[scenario](server)
        return {"status": "pass", "observations": {key: b64(value) for key, value in observations.items()},
                "trace": server.trace}
    except (AssertionError, OSError, subprocess.TimeoutExpired) as error:
        return {"status": "failure", "error": str(error), "trace": server.trace}
    finally:
        server.close()


def executable_identity(path):
    path = Path(path).resolve(strict=True)
    result = subprocess.run([str(path), "-V"], capture_output=True, timeout=5)
    if result.returncode:
        raise ValueError(f"cannot identify executable {path}: {result.stderr!r}")
    return {"path": str(path), "sha256": digest(path), "version": result.stdout.decode("utf-8", "replace").strip()}


def suite_reference(suite, references):
    if suite.get("kind") in ("remote-c-cargo", "remote-c-scenarios", "windows-system-c-views"):
        if "$reference" in suite.get("env", {}).values() or not suite.get("reference_version"):
            raise ValueError(f"{suite['id']} needs a pinned remote reference, never a local $reference")
        return None
    needs_reference = ("$reference" in suite.get("env", {}).values()
                       or suite.get("reference_version") or suite.get("kind") == "rustty-system-ssh")
    role = suite.get("reference", "current" if needs_reference else "none")
    if role == "none":
        if needs_reference:
            raise ValueError(f"{suite['id']} declares no reference but uses a C reference")
        return None
    if role not in ("current", "older"):
        raise ValueError(f"unknown reference role in {suite['id']}: {role}")
    identity = references.get(role)
    if not identity:
        raise ValueError(f"{suite['id']} requires --{'older-' if role == 'older' else ''}reference")
    expected = suite.get("reference_version")
    if expected and identity.get("version") != expected:
        raise ValueError(f"{suite['id']} requires {expected}, got {identity.get('version')}")
    if role == "older" and identity.get("sha256") == (references.get("current") or {}).get("sha256"):
        raise ValueError("current and older references must be different C executables")
    return identity


def suite_observations(suite, returncode, output):
    harness = suite.get("harness", "libtest")
    if harness == "windows-external-json":
        value = windows_external_record(suite, output)
        records = [(name, "ok") for name in value["tests"]] if value else []
    elif harness == "rustty-windows-conpty":
        manifest = json.loads((ROOT / "tmuxx/windows/conpty.json").read_text(encoding="utf-8"))
        hashes = {file["name"]: file["sha256"] for file in manifest["files"]}
        expected = {"suite": "windows_conpty", "status": "passed", "completed_redraws": 8,
                    "byte_boundary_cursor_ordering": True, "version": manifest["version"],
                    "package_sha256": manifest["sha256"], "conpty_sha256": hashes["conpty.dll"],
                    "openconsole_sha256": hashes["OpenConsole.exe"]}
        records = []
        for line in output.splitlines():
            if not line.startswith("TMUXX_WINDOWS_CONPTY_RESULT="):
                continue
            try:
                value = json.loads(line.split("=", 1)[1])
                valid = isinstance(value, dict) and all(
                    type(value.get(key)) is type(wanted) and value[key] == wanted for key, wanted in expected.items())
            except ValueError:
                valid = False
            records.append(("windows_conpty::synchronized_redraws", "ok" if valid else "FAILED"))
    elif harness == "libtest":
        records = re.findall(r"^test (\S+) \.\.\. (ok|FAILED|ignored)(?:[ \t].*)?$", output, re.MULTILINE)
    else:
        raise ValueError(f"unknown result harness: {harness}")
    completed = {}
    for name, result in records:
        # Duplicate names are ambiguous; a later success must not erase failure.
        completed[name] = "duplicate" if name in completed else result
    observations = []
    for assertion in suite["assertions"]:
        passed = returncode == 0 and all(completed.get(name) == "ok" for name in assertion["tests"])
        observations.append({"assertion": assertion["id"], "scenario": "suite:" + suite["id"], "status": "pass" if passed else "failure"})
    return observations, completed


def windows_external_record(suite, output):
    """Only one complete, exact native fixture result can establish evidence."""
    try:
        records = [json.loads(line) for line in output.splitlines() if line.startswith("{")]
    except ValueError:
        return None
    if len(records) != 1 or not isinstance(records[0], dict):
        return None
    value = records[0]
    expected = {name for assertion in suite["assertions"] for name in assertion["tests"]}
    tests = value.get("tests")
    if (type(value.get("schema")) is not int or value["schema"] != 1
            or value.get("kind") != suite.get("kind") or value.get("passed") is not True
            or type(value.get("failures")) is not list or value["failures"]
            or type(value.get("cleanup_remaining")) is not list or value["cleanup_remaining"]
            or not isinstance(tests, list) or not all(isinstance(name, str) for name in tests)
            or len(tests) != len(set(tests)) or set(tests) != expected
            or not all(isinstance(value.get(key), str) and re.fullmatch(r"[0-9a-f]{64}", value[key])
                       for key in (("candidate_sha256",) if suite.get("kind") == "remote-c-scenarios"
                                   else ("candidate_sha256", "gui_sha256")))):
        return None
    return value


def same_executable(left, right):
    return (isinstance(left, dict) and isinstance(right, dict)
            and isinstance(left.get("path"), str) and isinstance(right.get("path"), str)
            and Path(left["path"]).resolve() == Path(right["path"]).resolve()
            and left.get("sha256") == right.get("sha256"))


def executable_unchanged(identity):
    try:
        return (isinstance(identity, dict) and isinstance(identity.get("path"), str)
                and Path(identity["path"]).is_file() and digest(identity["path"]) == identity.get("sha256"))
    except OSError:
        return False


def validate_native_evidence(suite, details, candidate, gui):
    """Bind replayed native results to the same files frozen before collection."""
    output = base64.b64decode(details.get("stdout_base64", ""), validate=True).decode("utf-8", "replace")
    value = windows_external_record(suite, output)
    if value is None or value != details.get("native_result"):
        raise ValueError(f"{suite['id']} lacks its exact native completion record")
    report = details.get("native_report")
    if (not isinstance(report, str) or not Path(report).is_file()
            or json.loads(Path(report).read_text(encoding="utf-8")) != value):
        raise ValueError(f"{suite['id']} native result file is missing or changed")
    binaries = details.get("native_binaries")
    expected = {"candidate": candidate} if suite.get("kind") == "remote-c-scenarios" else {"candidate": candidate, "gui": gui}
    if not isinstance(binaries, dict) or set(binaries) != set(expected):
        raise ValueError(f"{suite['id']} lacks its native executable identities")
    for name, frozen in expected.items():
        identity = binaries[name]
        if (not same_executable(identity, frozen) or not executable_unchanged(identity)
                or value[name + "_sha256"] != identity.get("sha256")):
            raise ValueError(f"{suite['id']} native {name} executable differs from the frozen collection identity")
    if suite.get("kind") == "windows-system-c-views":
        files = value.get("system_ssh_files")
        if not isinstance(files, dict) or set(files) != {"config", "key", "known_hosts"}:
            raise ValueError("system-C desktop evidence lacks exact restricted SSH fixture identities")
        if not all(executable_unchanged(identity) for identity in files.values()):
            raise ValueError("system-C desktop SSH configuration or credentials changed since collection")
    return value


SUITE_OWNER_ENV = "TMUXX_CONFORMANCE_OWNER_TOKEN"


def stop_marked_linux_processes(token):
    """Clean detached fixtures without trusting mutable numeric PID identities."""
    if sys.platform != "linux" or not token:
        return []
    marker = (SUITE_OWNER_ENV + "=" + token).encode()
    errors = []

    def pinned_members():
        members = []
        for path in Path("/proc").iterdir():
            if not path.name.isdigit():
                continue
            descriptor = None
            try:
                if marker not in (path / "environ").read_bytes().split(b"\0"):
                    continue
                before = (path / "stat").read_bytes().rsplit(b") ", 1)[1].split()
                if before[0] == b"Z":
                    continue
                descriptor = os.pidfd_open(int(path.name))
                after = (path / "stat").read_bytes().rsplit(b") ", 1)[1].split()
                if after[19] != before[19] or marker not in (path / "environ").read_bytes().split(b"\0"):
                    os.close(descriptor)
                    descriptor = None
                    continue
                members.append((descriptor, int(path.name), int(after[1])))
                descriptor = None
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                pass
            except (OSError, IndexError, ValueError) as error:
                errors.append(f"could not identify owned fixture {path.name}: {error}")
            finally:
                if descriptor is not None:
                    os.close(descriptor)
        return members

    def wait_members(members, timeout):
        deadline = time.monotonic() + timeout
        while any(not select.select([descriptor], [], [], 0)[0] for descriptor, _, _ in members):
            if time.monotonic() >= deadline:
                break
            time.sleep(.02)

    def signal_members(members, signum):
        for descriptor, pid, _ in members:
            try:
                signal.pidfd_send_signal(descriptor, signum)
            except ProcessLookupError:
                pass
            except OSError as error:
                errors.append(f"could not signal owned fixture {pid}: {error}")

    def close_members(members):
        for descriptor, _, _ in members:
            os.close(descriptor)

    # Brokers block TERM. Give owners and their watchdogs a chance to finish
    # before any group-wide cleanup, including owners which ignore TERM.
    members = pinned_members()
    try:
        signal_members(members, signal.SIGTERM)
        wait_members(members, 2)
    finally:
        close_members(members)
    for _ in range(4):
        members = pinned_members()
        if not members:
            break
        try:
            live = {pid for _, pid, _ in members}
            ancestors = [member for member in members if member[2] not in live]
            if not ancestors:
                errors.append("owned fixture ancestry could not be resolved")
                break
            # Kill owners before their marked brokers. Preserve the pidfds and
            # captured parent relation for this stage, then allow watchdog cleanup
            # before considering descendants in a fresh scan.
            signal_members(ancestors, signal.SIGKILL)
            wait_members(members, 2)
        finally:
            close_members(members)
    remaining = pinned_members()
    if remaining:
        errors.append(f"{len(remaining)} marked fixture processes remained after cleanup")
    close_members(remaining)
    return errors


def stop_suite_session(process):
    """Stop marked Linux descendants first, then remaining owned session jobs."""
    errors = stop_marked_linux_processes(getattr(process, "tmuxx_suite_owner_token", None))
    if os.name == "posix":
        for _ in range(3):
            groups = {}
            try:
                rows = subprocess.check_output(["ps", "-axo", "pid=,pgid=,stat="], timeout=2)
                for row in rows.splitlines():
                    pid, group, state = row.split()
                    pid, group = int(pid), int(group)
                    try:
                        if not state.startswith(b"Z") and os.getsid(pid) == process.pid:
                            groups[pid if sys.platform == "linux" else group] = pid
                    except ProcessLookupError:
                        pass
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                errors.append(f"could not enumerate suite session: {error}")
                groups[process.pid] = process.pid
            for group, pid in groups.items():
                try:
                    if sys.platform == "linux":
                        descriptor = os.pidfd_open(pid)
                        try:
                            if os.getsid(pid) == process.pid:
                                signal.pidfd_send_signal(descriptor, signal.SIGKILL)
                        finally:
                            os.close(descriptor)
                    elif os.getsid(pid) == process.pid and os.getpgid(pid) == group:
                        os.killpg(group, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                except OSError as error:
                    errors.append(f"could not stop suite process group {group}: {error}")
            if not groups:
                break
            time.sleep(0.02)
    try:
        process.kill()
    except ProcessLookupError:
        pass
    except OSError as error:
        errors.append(f"could not stop suite child: {error}")
    return errors


class WindowsSuiteJob:
    """Kill-on-close ownership established before the worker starts any tests."""

    def __init__(self, *, allow_breakaway=False):
        import ctypes
        from ctypes import wintypes

        class BasicLimits(ctypes.Structure):
            _fields_ = [("ProcessUserTimeLimit", ctypes.c_int64), ("JobUserTimeLimit", ctypes.c_int64),
                        ("LimitFlags", wintypes.DWORD), ("MinimumWorkingSetSize", ctypes.c_size_t),
                        ("MaximumWorkingSetSize", ctypes.c_size_t), ("ActiveProcessLimit", wintypes.DWORD),
                        ("Affinity", ctypes.c_size_t), ("PriorityClass", wintypes.DWORD),
                        ("SchedulingClass", wintypes.DWORD)]

        class ExtendedLimits(ctypes.Structure):
            _fields_ = [("BasicLimitInformation", BasicLimits), ("IoInfo", ctypes.c_uint64 * 6),
                        ("ProcessMemoryLimit", ctypes.c_size_t), ("JobMemoryLimit", ctypes.c_size_t),
                        ("PeakProcessMemoryUsed", ctypes.c_size_t), ("PeakJobMemoryUsed", ctypes.c_size_t)]

        self.api = ctypes.WinDLL("kernel32", use_last_error=True)
        for name, arguments, result in [
            ("CreateJobObjectW", [ctypes.c_void_p, wintypes.LPCWSTR], wintypes.HANDLE),
            ("SetInformationJobObject", [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD], wintypes.BOOL),
            ("AssignProcessToJobObject", [wintypes.HANDLE, wintypes.HANDLE], wintypes.BOOL),
            ("TerminateJobObject", [wintypes.HANDLE, wintypes.UINT], wintypes.BOOL),
            ("CloseHandle", [wintypes.HANDLE], wintypes.BOOL),
        ]:
            function = getattr(self.api, name)
            function.argtypes, function.restype = arguments, result
        self.handle = self.api.CreateJobObjectW(None, None)
        if not self.handle:
            raise ctypes.WinError(ctypes.get_last_error())
        limits = ExtendedLimits()
        limits.BasicLimitInformation.LimitFlags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if allow_breakaway:
            # Only the IME fixture's independent, bounded restoration watchdog
            # explicitly requests breakaway. Product and ordinary suite jobs
            # retain their normal non-breakaway ownership.
            limits.BasicLimitInformation.LimitFlags |= 0x800
        if not self.api.SetInformationJobObject(self.handle, 9, ctypes.byref(limits), ctypes.sizeof(limits)):
            error = ctypes.WinError(ctypes.get_last_error())
            self.close()
            raise error

    def assign(self, process):
        import ctypes
        if not self.api.AssignProcessToJobObject(self.handle, int(process._handle)):
            raise ctypes.WinError(ctypes.get_last_error())

    def stop(self):
        import ctypes
        if not self.api.TerminateJobObject(self.handle, 1):
            raise ctypes.WinError(ctypes.get_last_error())

    def close(self):
        if self.handle:
            self.api.CloseHandle(self.handle)
            self.handle = None


def run_suite_process(command, *, cwd, env, timeout, allow_breakaway=False):
    job = WindowsSuiteJob(allow_breakaway=allow_breakaway) if os.name == "nt" else None
    owner_token = secrets.token_hex(32)
    process = None
    try:
        # Before assignment the worker can only wait for this byte. If startup
        # or the owner fails, EOF ends it without ever spawning the test command.
        invocation = [sys.executable, str(Path(__file__).resolve()), "--suite-child", *command] if job else command
        process = subprocess.Popen(invocation, cwd=cwd, env={**env, SUITE_OWNER_ENV: owner_token}, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, stdin=subprocess.PIPE if job else None,
                                   start_new_session=os.name == "posix")
        process.tmuxx_suite_owner_token = owner_token
        if job:
            job.assign(process)
            process.stdin.write(b"\x01")
            process.stdin.close()
            process.stdin = None
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired as error:
            if job:
                job.stop()
            cleanup_errors = stop_suite_session(process)
            try:
                stdout, stderr = process.communicate(timeout=2)
            except subprocess.TimeoutExpired as drain:
                # A separately detached Unix process may retain output writers.
                stdout, stderr = drain.output or error.output or b"", drain.stderr or error.stderr or b""
                process.stdout.close()
                process.stderr.close()
                cleanup_errors.append("suite output pipes remained open after session cleanup")
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    cleanup_errors.append("suite child could not be reaped within cleanup timeout")
            error.output, error.stderr = stdout, stderr
            error.cleanup_errors = cleanup_errors
            raise
        return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
    except BaseException:
        if process and process.poll() is None:
            if job:
                job.stop()
            stop_suite_session(process)
            process.wait(timeout=2)
        raise
    finally:
        if job:
            job.close()
        if process:
            for stream in (process.stdin, process.stdout, process.stderr):
                if stream and not stream.closed:
                    stream.close()


def run_suite(suite, reference, candidate=None, candidate_identity=None, gui_identity=None):
    command = ["cargo", *suite.get("args", [])]
    cwd = ROOT
    env = os.environ.copy()
    env.update({key: str(reference) if value == "$reference" else value for key, value in suite.get("env", {}).items()})
    env["CARGO_TERM_COLOR"] = "never"
    if sys.platform == "win32":
        # Native fixture PIDs and cwd belong to the interpreter, not a launcher
        # shim which may create it from a different working directory.
        env.setdefault("TMUXX_REMOTE_PYTHON", sys.executable)
    try:
        if sys.platform not in suite_platforms(suite):
            raise ValueError(f"suite {suite['id']} requires execution on {suite_platforms(suite)}, not {sys.platform}")
        if reference is None and "$reference" in suite.get("env", {}).values():
            raise ValueError(f"{suite['id']} needs an explicit C reference")
        external = None
        remote_reference = None
        native_binaries = None
        native_report = None
        if suite.get("kind") in ("rustty-cargo", "rustty-system-ssh", "rustty-desktop-smoke", *WINDOWS_NATIVE_KINDS):
            external = rustty_identity()
            cwd = Path(external["root"])
            if sys.platform == "win32":
                env.setdefault("TMUXX_CONPTY_RESOURCES", str(cwd / "target/debug/Rustty/resources"))
            if suite["kind"] == "rustty-system-ssh":
                command = [sys.executable, str(HERE / "rustty_system_ssh.py"), "--rustty", str(cwd), "--reference", str(reference)]
            elif suite["kind"] == "rustty-desktop-smoke":
                command = [sys.executable, str(HERE / "rustty_desktop_smoke.py"), "--rustty", str(cwd)]
            elif suite["kind"] in WINDOWS_NATIVE_KINDS:
                gui = Path(env.get("TMUXX_WINDOWS_GUI", cwd / "target/debug/Rustty/rustty.exe")).resolve(strict=True)
                executable = Path(candidate or env.get("TMUXX_WINDOWS_CANDIDATE", ROOT / "target/debug/tmuxx/tmuxx.exe")).resolve(strict=True)
                native_binaries = {"gui": {"path": str(gui), "sha256": digest(gui)},
                                   "candidate": {"path": str(executable), "sha256": digest(executable)}}
                for name, frozen in (("candidate", candidate_identity), ("gui", gui_identity)):
                    if frozen is not None and not same_executable(native_binaries[name], frozen):
                        raise ValueError(f"native {name} executable changed before suite execution")
                native_report = Path(tempfile.mkdtemp(prefix="tmuxx-windows-evidence-")) / "result.json"
                if suite["kind"] in ("windows-native-views", "windows-native-input", "windows-native-clipboard", "windows-native-ime", "windows-native-frame", "windows-lifetime", "windows-cli-state", "windows-system-c-views"):
                    script = suite["kind"].replace("-", "_") + ".py"
                    if suite["kind"] == "windows-native-ime" and env.get("TMUXX_WINDOWS_IME_PROFILE") != "temporary-japanese":
                        raise ValueError("Windows IME desktop prerequisite missing: TMUXX_WINDOWS_IME_PROFILE=temporary-japanese")
                    if suite["kind"] == "windows-system-c-views":
                        for name in ("TMUXX_SYSTEM_C_SSH_CONFIG", "TMUXX_SYSTEM_C_SSH_HOST", "TMUXX_SYSTEM_C_SELECTION"):
                            if not env.get(name):
                                raise ValueError("Windows system-C desktop prerequisite missing: " + name)
                        remote_reference = remote_c_evidence.identity(suite, run_suite_process)
                        env.update(TMUXX_REMOTE_C_CONFIG=remote_reference["config"]["path"],
                                   TMUXX_REMOTE_PYTHON=sys.executable, TZ="UTC", LC_ALL="C.UTF-8")
                    command = [sys.executable, str(HERE / script), "--gui", str(gui),
                               "--candidate", str(executable), "--report", str(native_report), *suite.get("args", [])]
                else:
                    required = {"TMUXX_WINDOWS_SSH_KEY": "--key", "TMUXX_WINDOWS_SSH_KNOWN_HOSTS": "--known-hosts",
                                "TMUXX_WINDOWS_SSH_HOST": "--host", "TMUXX_WINDOWS_SSH_VIEWER_KEY": "--viewer-key",
                                "TMUXX_WINDOWS_SSH_VIEWER_BRIDGE": "--viewer-bridge",
                                "TMUXX_WINDOWS_SSH_RUSTTY_VIEWER_KEY": "--rustty-viewer-key",
                                "TMUXX_WINDOWS_SSH_RUSTTY_VIEWER_BRIDGE": "--rustty-bridge"}
                    for name in required:
                        if not env.get(name):
                            raise ValueError(f"Windows cross-logon desktop prerequisite missing: {name}")
                    observer = env.get("TMUXX_WINDOWS_SSH_OBSERVER", str(native_report.with_name("observer.json")))
                    command = [sys.executable, str(HERE / "windows_ssh.py"), "--tmuxx", str(executable),
                               "--gui", str(gui), "--observer", observer, "--report", str(native_report)]
                    for name, argument in required.items():
                        command += [argument, env[name]]
                    for name, argument in {"TMUXX_WINDOWS_SSH_EXE": "--ssh",
                                           "TMUXX_WINDOWS_SSH_DENIAL_DIRECTORY": "--denial-directory"}.items():
                        if env.get(name):
                            command += [argument, env[name]]
                    command += suite.get("args", [])
        elif suite.get("kind") in ("remote-c-cargo", "remote-c-scenarios"):
            remote_reference = remote_c_evidence.identity(suite, run_suite_process)
            env["TMUXX_REMOTE_C_CONFIG"] = remote_reference["config"]["path"]
            env["TMUXX_REMOTE_PYTHON"] = sys.executable
            env["TZ"] = "UTC"
            env["LC_ALL"] = "C.UTF-8"
            if suite.get("raw_system_ssh"):
                system = remote_reference["system_ssh"]
                env.update(TMUXX_SYSTEM_C_SSH=system["ssh"]["path"],
                           TMUXX_SYSTEM_C_SSH_CONFIG=system["config"]["path"], TMUXX_SYSTEM_C_SSH_HOST=system["host"])
                if not env.get("TMUXX_SYSTEM_C_SELECTION"):
                    raise ValueError("raw C SSH requires private TMUXX_SYSTEM_C_SELECTION")
            if suite["kind"] == "remote-c-scenarios":
                executable = Path(candidate or env.get("TMUXX_WINDOWS_CANDIDATE", ROOT / "target/debug/tmuxx/tmuxx.exe")).resolve(strict=True)
                native_binaries = {"candidate": {"path": str(executable), "sha256": digest(executable)}}
                if candidate_identity is not None and not same_executable(native_binaries["candidate"], candidate_identity):
                    raise ValueError("native scenario candidate changed before execution")
                native_report = Path(tempfile.mkdtemp(prefix="tmuxx-windows-scenarios-")) / "result.json"
                command = [sys.executable, str(HERE / "windows_scenarios.py"), "--candidate", str(executable),
                           "--config", remote_reference["config"]["path"], "--report", str(native_report)]
        elif suite.get("kind", "cargo") != "cargo":
            raise ValueError(f"unknown suite kind: {suite['kind']}")
        result = run_suite_process(command, cwd=cwd, env=env, timeout=suite.get("timeout", 300),
                                   allow_breakaway=suite.get("kind") == "windows-native-ime")
        output = result.stdout.decode("utf-8", "replace")
        observations, completed = suite_observations(suite, result.returncode, output)
        recorded_env = set(suite.get("env", {})) | ({"TMUXX_CONPTY_RESOURCES"} if "TMUXX_CONPTY_RESOURCES" in env else set())
        if suite.get("kind") == "windows-ssh":
            recorded_env |= {name for name in env if name.startswith("TMUXX_WINDOWS_SSH_")}
        if suite.get("kind") == "windows-native-ime":
            recorded_env.add("TMUXX_WINDOWS_IME_PROFILE")
        if suite.get("kind") == "windows-system-c-views" or suite.get("raw_system_ssh"):
            recorded_env |= {name for name in env if name.startswith("TMUXX_SYSTEM_C_")}
        details = {"argv": command, "cwd": str(cwd), "env": {key: env[key] for key in sorted(recorded_env)}, "status": result.returncode,
                   "stdout_base64": b64(result.stdout), "stderr_base64": b64(result.stderr), "completed_tests": completed}
        if native_binaries:
            value = windows_external_record(suite, output)
            if value:
                if not native_report.is_file() or json.loads(native_report.read_text(encoding="utf-8")) != value:
                    raise ValueError("native fixture result file does not match its completion record")
                for name, identity in native_binaries.items():
                    if value[name + "_sha256"] != identity["sha256"] or digest(identity["path"]) != identity["sha256"]:
                        raise ValueError(f"native fixture {name} executable changed or result hash mismatches")
                details["native_result"] = value
            details["native_binaries"] = native_binaries
            details["native_report"] = str(native_report)
        if external:
            details["rustty"] = external
            if external != rustty_identity():
                raise ValueError("Rustty sources changed during suite execution")
        if remote_reference:
            details["remote_reference"] = remote_reference
            details["env"].update({key: env[key] for key in ("TMUXX_REMOTE_C_CONFIG", "TMUXX_REMOTE_PYTHON", "TZ", "LC_ALL")})
            try:
                details["remote_fixtures"] = remote_c_evidence.fixtures(suite, result.stderr.decode("utf-8", "replace"), remote_reference)
                remote_c_evidence.verify(suite, details, run_suite_process)
            except (ValueError, TypeError, OSError, subprocess.SubprocessError) as error:
                observations, _ = suite_observations(suite, -1, "")
                details.update(status=-1, error=str(error))
    except subprocess.TimeoutExpired as error:
        stdout, stderr = error.output or b"", error.stderr or b""
        observations, completed = suite_observations(suite, -1, stdout.decode("utf-8", "replace"))
        details = {"argv": command, "cwd": str(cwd), "status": -1, "error": str(error),
                   "stdout_base64": b64(stdout), "stderr_base64": b64(stderr), "completed_tests": completed,
                   "cleanup_errors": getattr(error, "cleanup_errors", [])}
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        observations, _ = suite_observations(suite, -1, "")
        details = {"argv": command, "status": -1, "error": str(error)}
    details["platform"] = sys.platform
    return observations, details


def smoke(args):
    config = manifest()
    fingerprint = implementation_fingerprint()
    revision = source_revision()
    if not args.no_smoke and not args.reference:
        raise ValueError("C/R smoke scenarios require --reference; use --no-smoke for native suites")
    reference = executable_identity(args.reference) if args.reference else None
    if reference and not reference["version"].startswith("tmux "):
        raise ValueError("reference must identify as C tmux")
    older = executable_identity(args.older_reference) if args.older_reference else None
    if older and not older["version"].startswith("tmux "):
        raise ValueError("older reference must identify as C tmux")
    references = {"current": reference, "older": older}
    candidate = executable_identity(args.candidate)
    if args.candidate_kind == "rust" and not candidate["version"].startswith("tmuxx "):
        raise ValueError("Rust candidate must identify as tmuxx; refusing accidental C/C evidence")
    observations = []
    details = {}
    selected, suite_details = select_suites(args.suite or [], sys.platform)
    blocked_suites = list(suite_details)
    for name in blocked_suites:
        print(f"suite {name}: blocked ({suite_details[name]['reason']})", flush=True)
    if selected and args.candidate_kind != "rust":
        raise ValueError("Rust executable suites cannot be attached to C/C evidence")
    selected_references = {name: suite_reference(suite, references) for name, suite in selected.items()}
    native_gui = None
    if any(suite.get("kind") in WINDOWS_NATIVE_KINDS for suite in selected.values()):
        gui = Path(os.environ.get("TMUXX_WINDOWS_GUI", rustty_root() / "target/debug/Rustty/rustty.exe")).resolve(strict=True)
        native_gui = {"path": str(gui), "sha256": digest(gui)}
    for name, suite in selected.items():
        identity = selected_references[name]
        if suite.get("kind") in WINDOWS_NATIVE_KINDS:
            evidence, suite_details[name] = run_suite(suite, identity["path"] if identity else None,
                candidate=candidate["path"], candidate_identity=candidate, gui_identity=native_gui)
            if all(item["status"] == "pass" for item in evidence):
                try:
                    validate_native_evidence(suite, suite_details[name], candidate, native_gui)
                except (ValueError, TypeError, OSError) as error:
                    evidence, _ = suite_observations(suite, -1, "")
                    suite_details[name].update(status=-1, error=str(error))
        else:
            evidence, suite_details[name] = run_suite(suite, identity["path"] if identity else None)
        suite_details[name]["reference"] = selected_references[name]
        observations.extend(evidence)
        passed = all(item["status"] == "pass" for item in evidence)
        print(f"suite {name}: {'pass' if passed else 'failure'}", flush=True)
        if not passed:
            failed = suite_details[name]
            print(f"  {failed.get('error', 'status ' + str(failed['status']))}", flush=True)
            for key in ("stdout_base64", "stderr_base64"):
                if failed.get(key):
                    print(base64.b64decode(failed[key]).decode("utf-8", "replace")[-8000:], flush=True)
    assertions = {entry["scenario"]: entry["id"] for entry in config["assertions"]}
    for scenario in ([] if args.no_smoke else args.scenario or SCENARIOS):
        left = run_side(args.reference, "c", scenario)
        right = run_side(args.candidate, args.candidate_kind, scenario)
        passed = left["status"] == right["status"] == "pass" and left["observations"] == right["observations"]
        status = "pass" if passed else "failure"
        observations.append({"assertion": assertions[scenario], "scenario": scenario, "status": status})
        details[scenario] = {"reference": left, "candidate": right}
        print(f"{scenario}: {status}")
        if not passed:
            print(f"  reference: {left.get('error', left.get('observations'))}")
            print(f"  candidate: {right.get('error', right.get('observations'))}")
    entries = apply_observations(platform_inventory(inventory(config, sys.platform), suites(), sys.platform), observations)
    changed = fingerprint != implementation_fingerprint()
    revision_changed = revision != source_revision()
    candidate_changed = not executable_unchanged(candidate)
    gui_changed = native_gui is not None and not executable_unchanged(native_gui)
    references_changed = any(identity and (not Path(identity["path"]).is_file() or digest(identity["path"]) != identity["sha256"]) for identity in references.values())
    report = {"schema_version": 2, "kind": "exact-assertion-evidence", "candidate_kind": args.candidate_kind,
              "checkout_revision": revision, "checkout_changed_during_run": revision_changed,
              "reference_revision": config["reference_revision"], "manifest_sha256": digest(MANIFEST),
              "implementation_fingerprint": fingerprint, "source_changed_during_run": changed, "platform": sys.platform,
              "host": host_identity(), "platform_execution": config["platform_execution"],
              "reference": reference, "older_reference": older, "references_changed_during_run": references_changed,
              "candidate": candidate, "candidate_changed_during_run": candidate_changed,
              "native_gui": native_gui, "native_gui_changed_during_run": gui_changed, "summary": counts(entries),
              "entries": entries, "scenarios": details, "suites": suite_details, "blocked_suites": blocked_suites, "profile_release_ready": False}
    destination = Path(args.report)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Exact assertions only. Full inventory: {report['summary']}. No profile certified.")
    print(f"Evidence: {destination}")
    if changed:
        print("Source changed during evidence collection; rerun before using this report.")
    if references_changed:
        print("C reference changed during evidence collection; rerun before using this report.")
    if revision_changed or candidate_changed or gui_changed:
        print("Checkout revision or candidate/GUI executable changed during evidence collection; rerun before using this report.")
    return 0 if (observations and not blocked_suites and not any((changed, revision_changed, references_changed, candidate_changed, gui_changed))
                 and all(item["status"] == "pass" for item in observations)) else 1


def blockers(report, profile, config=None):
    config = config or manifest()
    if profile not in config["profiles"]:
        raise ValueError(f"unknown profile {profile}")
    problems = []
    if report.get("candidate_kind") != "rust":
        problems.append("C/C establishes the reference harness, not Rust conformance")
    if report.get("manifest_sha256") != digest(MANIFEST):
        problems.append("manifest changed since evidence was collected")
    if report.get("checkout_revision") != source_revision():
        problems.append("checkout revision changed since evidence was collected")
    if report.get("reference_revision") != config["reference_revision"]:
        problems.append("reviewed C baseline changed since evidence was collected")
    if report.get("implementation_fingerprint") != implementation_fingerprint():
        problems.append("implementation changed since evidence was collected")
    if report.get("source_changed_during_run"):
        problems.append("source changed during evidence collection")
    if report.get("references_changed_during_run"):
        problems.append("C reference changed during evidence collection")
    for key, description in (("checkout_changed_during_run", "checkout revision"),
                             ("candidate_changed_during_run", "candidate executable"),
                             ("native_gui_changed_during_run", "native GUI executable")):
        if report.get(key):
            problems.append(f"{description} changed during evidence collection")
    baseline = subprocess.run(["git", "merge-base", "--is-ancestor", config["reference_revision"], "HEAD"],
                              cwd=c_source(), capture_output=True)
    if baseline.returncode:
        problems.append("reviewed C baseline is missing or not an ancestor; review the upstream history")
    for role in ("candidate", *(["reference"] if report.get("reference") else []), *(["older_reference"] if report.get("older_reference") else [])):
        recorded = report.get(role) or {}
        path = recorded.get("path")
        if not executable_unchanged(recorded):
            problems.append(f"{role} executable is missing or changed since evidence was collected")
    if config.get("platform_execution", {}).get(report.get("platform"), {}).get("status") == "deferred":
        problems.append(f"{report.get('platform')} execution is user-deferred, not certified")
    if report.get("platform") not in config["profiles"][profile]["platforms"]:
        problems.append(f"report platform does not certify {profile}")
    host = report.get("host", {})
    if host.get("platform") != report.get("platform"):
        problems.append("report lacks matching host-platform identity")
    if report.get("platform") == "linux":
        if not host.get("architecture") or not host.get("kernel") or not host.get("distribution", {}).get("ID"):
            problems.append("GNU/Linux evidence requires distribution, kernel and architecture identity")
        if (host.get("libc") or [None])[0] != "glibc" or not (host.get("rust_host_target") or "").endswith("-linux-gnu"):
            problems.append("this Linux qualification requires an identified GNU target and glibc host")
        if host.get("cargo_build_target") not in (None, host.get("rust_host_target")):
            problems.append("cross-compiled test targets require separate executable qualification")
    available = suites()
    current_entries = platform_inventory(inventory(config, report.get("platform")), available, report.get("platform"))
    evidence = {entry["id"]: entry for entry in report.get("entries", [])}
    for entry in current_entries:
        entry["observation_results"] = dict(evidence.get(entry["id"], {}).get("observation_results", {}))
    required = {entry["id"] for entry in current_entries if entry["status"] != "not-applicable" and
                (profile in entry["profiles"] or "classification" in entry["profiles"])}
    required.update(source for entry in current_entries if entry["id"] in required for source in entry.get("proof", []))
    by_id = {entry["id"]: entry for entry in current_entries}
    if not report.get("reference") and any(
        entry["id"] in required and entry.get("scenario")
        and entry["observation_results"].get(entry["scenario"]) in ("pass", "intentional-difference")
        for entry in current_entries
    ):
        problems.append("C/R scenario evidence requires a genuine C reference executable")
    references = {"current": report.get("reference"), "older": report.get("older_reference")}
    for suite in available.values():
        if not any(assertion["id"] in required for assertion in suite["assertions"]):
            continue
        details = report.get("suites", {}).get(suite["id"], {})
        if report.get("platform") not in suite_platforms(suite):
            if any(assertion["id"] in required and not by_id[assertion["id"]].get("suites")
                   for assertion in suite["assertions"]):
                problems.append(f"{suite['id']}: requires supported client execution on {', '.join(suite_platforms(suite))}; "
                                "a local or historical desktop result does not qualify this server host")
            # In particular, copied macOS observations must not pass Linux gates.
            for assertion in suite["assertions"]:
                by_id[assertion["id"]]["observation_results"].pop("suite:" + suite["id"], None)
            continue
        try:
            if details.get("platform") != report.get("platform"):
                raise ValueError(f"{suite['id']} lacks matching execution-platform evidence")
            expected = suite_reference(suite, references)
            if details.get("reference") != expected:
                raise ValueError(f"{suite['id']} lacks matching executable reference evidence")
            output = base64.b64decode(details.get("stdout_base64", ""), validate=True).decode("utf-8", "replace")
            if suite.get("kind") in (*WINDOWS_NATIVE_KINDS, "remote-c-scenarios") and details.get("status") == 0:
                validate_native_evidence(suite, details, report.get("candidate"), report.get("native_gui"))
            if suite.get("kind") in ("remote-c-cargo", "remote-c-scenarios", "windows-system-c-views") and details.get("status") == 0:
                remote_c_evidence.verify(suite, details, run_suite_process)
            observations, _ = suite_observations(suite, details.get("status", -1), output)
        except (ValueError, TypeError, OSError, subprocess.SubprocessError) as error:
            problems.append(str(error))
            observations, _ = suite_observations(suite, -1, "")
        for observation in observations:
            results = by_id[observation["assertion"]]["observation_results"]
            if results.get(observation["scenario"]) != "failure":
                results[observation["scenario"]] = observation["status"]
    derive_statuses(current_entries)
    for entry in current_entries:
        identifier = entry["id"]
        if profile not in entry["profiles"] and "classification" not in entry["profiles"]:
            continue
        if entry["status"] == "not-applicable":
            continue
        if entry["requirement"] == "required" and entry["status"] not in ("pass", "intentional-difference"):
            problems.append(f"{identifier}: {entry['status'] if identifier in evidence else 'missing evidence'}")
    return problems


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    inv = sub.add_parser("inventory", help="show reviewed scoped assertions and upstream drift without certifying them")
    inv.add_argument("--json", action="store_true")
    smoke_parser = sub.add_parser("smoke", help="run declared narrow C/C or C/R scenarios, not the complete release gate")
    smoke_parser.add_argument("--reference", help="C tmux executable required by smoke scenarios and C-reference suites")
    smoke_parser.add_argument("--older-reference", help="explicit C3.7c executable required by the supported-version suites")
    smoke_parser.add_argument("--candidate", required=True)
    smoke_parser.add_argument("--candidate-kind", choices=("c", "rust"), default="rust")
    smoke_parser.add_argument("--scenario", choices=SCENARIOS, action="append")
    smoke_parser.add_argument("--suite", choices=["all", *suites()], action="append", help="collect exact named Cargo test evidence")
    smoke_parser.add_argument("--no-smoke", action="store_true", help="collect selected suites without C/R smoke scenarios")
    smoke_parser.add_argument("--report", default=str(HERE / "artifacts/smoke.json"))
    gate = sub.add_parser("gate", help="fail on pending/deferred/missing assertions or stale evidence")
    gate.add_argument("--report", required=True)
    gate.add_argument("--profile", required=True)
    args = parser.parse_args(argv)
    try:
        if args.action == "inventory":
            entries = inventory()
            if args.json:
                print(json.dumps({"source_revision": source_revision(), "summary": counts(entries), "entries": entries}, indent=2))
            else:
                print(f"Scoped assertions: {len(entries)}; {counts(entries)}")
                print(f"Reviewed source surface: { {kind: len(names) for kind, names in discovered(manifest()).items()} }")
                print(f"Upstream changes requiring review: {sum(e['id'].startswith('review:') for e in entries)}")
                print(f"Platform execution: { {platform: record['status'] for platform, record in manifest()['platform_execution'].items()} }")
                print("This inventory is independent of Engine::SUPPORTED_COMMANDS. No profile certified.")
            return 0
        if args.action == "smoke":
            return smoke(args)
        report = json.loads(Path(args.report).read_text(encoding="utf-8"))
        problems = blockers(report, args.profile)
        if problems:
            print(f"{args.profile}: BLOCKED ({len(problems)} obligations or evidence problems)")
            print("\n".join(problems[:20]))
            return 1
        print(f"{args.profile}: PASS")
        return 0
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"conformance: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--suite-child":
        # Private Windows Job Object handshake, deliberately outside argparse.
        if sys.stdin.buffer.read(1) != b"\x01":
            raise SystemExit(125)
        raise SystemExit(subprocess.call(sys.argv[2:], stdin=subprocess.DEVNULL))
    raise SystemExit(main())
