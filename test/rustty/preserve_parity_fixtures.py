#!/usr/bin/env python3
"""Regenerate the Rust runner's fixture data; never used during suite execution."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import zlib

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
GROUPS = ("input", "parser", "grid", "page-layout", "pages", "thorough",
          "unicode", "snapshots", "snapshot-wire", "protocols", "corpus", "osc")
reads = set()
directories = set()


def audit(event, args):
    # Track source-derived keys, snapshot templates, and corpus membership as
    # well as generator code. A frozen case must not silently become stale.
    if event == "open" and isinstance(args[0], str) and args[1] and not any(c in args[1] for c in "wax+"):
        path = Path(args[0]).resolve()
        if path.is_relative_to(ROOT) and not path.is_relative_to(ROOT / "target") and path.suffix != ".pyc":
            reads.add(path)
    elif event == "pathlib.Path.glob":
        path, pattern = args
        if pattern != "*":
            raise RuntimeError(f"unrecorded corpus glob: {path}/{pattern}")
        directories.add(path.resolve())


sys.addaudithook(audit)
import parity_reference as reference  # noqa: E402


class RecordingPeer:
    def __init__(self, name, binary, timeout):
        self.name = name
        self.peer = reference.Peer(name, [str(binary.resolve())], timeout)
        self.probes = []

    def request(self, request):
        response = self.peer.request(request)
        # Rust-produced snapshots remain historical compatibility fixtures.
        # Only the native assumptions used to construct cases are enforced.
        if self.name == "zig":
            self.probes.append({"request": request, "response": response})
        return response


def main():
    global reads, directories
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--zig-bin", type=Path, default=ROOT / "zig-out/bin/vt-oracle")
    parser.add_argument("--rust-bin", type=Path, default=ROOT / "target/release/examples/parity")
    parser.add_argument("--output", type=Path, default=HERE / "fixtures")
    parser.add_argument("--group", choices=GROUPS, action="append")
    parser.add_argument("--timeout", type=float, default=30)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    reference.ARTIFACTS = ROOT / "target/parity-fixture-export"
    reference.ARTIFACTS.mkdir(parents=True, exist_ok=True)
    empty = reference.ARTIFACTS / "empty.json"
    empty.write_text("[]\n")
    common = reads | {Path(__file__).resolve()}
    common.update(Path(module.__file__).with_suffix(".py").resolve()
                  for module in list(sys.modules.values())
                  if getattr(module, "__file__", None) and Path(module.__file__).resolve().parent == HERE)
    provenance = {
        "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "oracles": {name: hashlib.sha256(path.read_bytes()).hexdigest()
                    for name, path in (("zig", args.zig_bin), ("rust", args.rust_bin))},
    }
    manifest_path = args.output / "manifest.json"
    manifest = json.loads(manifest_path.read_text()) if args.group and manifest_path.exists() else {}
    for group in args.group or GROUPS:
        reads, directories = set(common), set()
        peers = []
        try:
            for name, binary in (("zig", args.zig_bin), ("rust", args.rust_bin)):
                peers.append(RecordingPeer(name, binary, args.timeout))
            options = argparse.Namespace(fixtures=empty, replay=None, case=None,
                                         thorough=False, generated=0, seed=0,
                                         **{flag.replace("-", "_"): flag == group for flag in GROUPS if flag != "thorough"})
            requests = (list(reference.search_pages.requests()) if group == "thorough"
                        else reference.collect_requests(options, peers))
            probes = peers[0].probes
        finally:
            for peer in peers:
                peer.peer.close()
        inputs = {str(path.relative_to(ROOT)): zlib.crc32(path.read_bytes()) for path in sorted(reads)}
        members = {str(path.relative_to(ROOT)): sorted(item.name for item in path.iterdir() if item.is_file())
                   for path in sorted(directories)}
        comparisons = sum(1 if request.get("expected_error") or request.get("kind", "terminal") not in
                          ("terminal", "input", "parser", "snapshot") else 3 for request, _ in requests)
        header = {"format": 1, "cases": len(requests), "inputs": inputs,
                  "directories": members, "probes": probes}
        path = args.output / f"{group}.jsonl.gz"
        with path.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as output:
            for record in [header, *({"request": request, "covers": covers} for request, covers in requests)]:
                output.write(json.dumps(record, separators=(",", ":"), ensure_ascii=True).encode() + b"\n")
        manifest[group] = {**provenance, "cases": len(requests), "comparisons": comparisons,
                           "bytes": path.stat().st_size, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
        print(f"{group}: {len(requests)} fixtures, {comparisons} comparisons, {path.stat().st_size:,} bytes", flush=True)
        manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
