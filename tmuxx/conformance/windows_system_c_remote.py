#!/usr/bin/env python3
"""Restricted stock C control attachment for the Windows desktop SSH fixture.

The Windows forced key permits fixed command forms only. This second check
selects one already-running leased private C incarnation and never starts it.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys
import time


def arguments(selection, command):
    match = re.fullmatch(r"exec tmux -C attach-session -f new-layouts(?: -t '(\$[0-9]+)')?", command)
    listing = command == "exec tmux list-buffers -F '#{buffer_name}'"
    if not match and not listing:
        raise ValueError("unsupported fixed C fixture command")
    if (type(selection.get("schema")) is not int or selection["schema"] != 1
            or type(selection.get("server_pid")) is not int or selection["server_pid"] <= 0):
        raise ValueError("missing selected C server identity")
    now = time.time()
    expiry = selection.get("expires_unix", 0)
    if not now < expiry <= now + 180:
        raise ValueError("selected C fixture lease expired or invalid")
    binary = Path(selection["binary"]).resolve(strict=True)
    if hashlib.sha256(binary.read_bytes()).hexdigest() != selection["sha256"]:
        raise ValueError("selected C binary differs from pinned SHA-256")
    pid = selection["server_pid"]
    process = Path(f"/proc/{pid}")
    if process.stat().st_uid != os.getuid() or (process / "exe").resolve(strict=True) != binary:
        raise ValueError("selected C server owner or executable differs")
    start = (process / "stat").read_text().rsplit(")", 1)[1].split()[19]
    if start != selection["server_start_ticks"]:
        raise ValueError("selected C server incarnation changed")
    socket = Path(selection["socket"])
    directory = socket.parent.stat()
    information = socket.lstat()
    if (directory.st_uid != os.getuid() or stat.S_IMODE(directory.st_mode) != 0o700
            or information.st_uid != os.getuid() or not stat.S_ISSOCK(information.st_mode)
            or information.st_ino != selection["socket_inode"]):
        raise ValueError("selected C socket identity or private ownership differs")
    result = [str(binary), "-N", "-f", "/dev/null", "-S", str(socket)]
    if listing:
        return result + ["list-buffers", "-F", "#{buffer_name}"]
    result += ["-C", "attach-session", "-f", "new-layouts"]
    if match.group(1):
        result += ["-t", match.group(1)]
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--selection", type=Path, required=True)
    parser.add_argument("--command-base64", required=True)
    args = parser.parse_args()
    try:
        if sys.platform != "linux":
            raise ValueError("this is the POSIX C oracle entry, not Windows process evidence")
        command = base64.b64decode(args.command_base64, validate=True).decode("ascii")
        argv = arguments(json.loads(args.selection.read_text()), command)
        os.environ.pop("TMUX", None)
        os.execv(argv[0], argv)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print("private C control fixture: " + str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
