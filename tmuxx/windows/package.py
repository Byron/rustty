#!/usr/bin/env python3
"""Stage the pinned x64 ConPTY resources; never accept unchecked archive members."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import tempfile
import urllib.request
import zipfile

MANIFEST = Path(__file__).with_name("conpty.json")


def verify(data: bytes, expected: str, description: str) -> None:
    if hashlib.sha256(data).hexdigest() != expected:
        raise ValueError(f"{description}: SHA-256 mismatch")


def stage(package: Path, destination: Path, *, offline: bool) -> dict:
    spec = json.loads(MANIFEST.read_text(encoding="utf-8"))
    name = f"{spec['package']}.{spec['version']}.nupkg"
    if not package.is_file():
        if offline:
            raise FileNotFoundError(f"Pinned ConPTY package is not cached: {package}")
        package.parent.mkdir(parents=True, exist_ok=True)
        url = f"https://api.nuget.org/v3-flatcontainer/{spec['package']}/{spec['version']}/{name}"
        with urllib.request.urlopen(url, timeout=60) as response:
            data = response.read(16 * 1024 * 1024 + 1)
        verify(data, spec["sha256"], "Downloaded ConPTY package")
        with tempfile.NamedTemporaryFile(dir=package.parent, delete=False) as download:
            temporary = Path(download.name)
            download.write(data)
        try:
            temporary.replace(package)
        finally:
            temporary.unlink(missing_ok=True)
    archive_bytes = package.read_bytes()
    verify(archive_bytes, spec["sha256"], "Cached ConPTY package")
    # Validate everything before creating or changing the destination.
    import io
    members = {}
    with zipfile.ZipFile(io.BytesIO(archive_bytes)) as archive:
        for member in spec["files"]:
            data = archive.read(member["member"])
            if len(data) != member["length"]:
                raise ValueError(f"{member['name']}: unexpected length")
            verify(data, member["sha256"], member["name"])
            members[member["name"]] = data
    destination.mkdir(parents=True, exist_ok=False)
    for name, data in members.items():
        (destination / name).write_bytes(data)
    (destination / "package.json").write_bytes(MANIFEST.read_bytes())
    return spec


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    result = stage(args.package, args.destination, offline=args.offline)
    print(json.dumps({"version": result["version"], "package_sha256": result["sha256"], "directory": str(args.destination)}, sort_keys=True))


if __name__ == "__main__":
    main()
