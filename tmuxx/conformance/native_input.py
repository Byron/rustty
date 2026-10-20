#!/usr/bin/env python3
"""Observe C child bytes through an isolated attached client, never its rendering."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def run(binary):
    with tempfile.TemporaryDirectory(prefix="tx-native-input-") as temporary:
        root = Path(temporary)
        commands = {name: [binary, "-f", "/dev/null", "-S", str(root / name)] for name in ("inner", "outer")}

        def command(name, *args):
            result = subprocess.run(commands[name] + list(args), capture_output=True, timeout=5)
            if result.returncode:
                raise RuntimeError(f"{name} {args}: {result.stderr!r}")
            return result.stdout

        def wait(predicate):
            deadline = time.monotonic() + 5
            while not predicate():
                if time.monotonic() > deadline:
                    raise RuntimeError("C input oracle did not converge")
                time.sleep(.005)

        try:
            command("inner", "new-session", "-d", "-s", "inner", "-x", "80", "-y", "24", "sleep", "90")
            for name, value in (("status", "off"), ("mouse", "on"), ("focus-events", "on"), ("assume-paste-time", "0")):
                command("inner", "set-option", "-g", name, value)
            for key in ("MouseDown1Pane", "C-M-MouseDown1Pane", "MouseUp1Pane", "MouseDrag1Pane", "WheelUpPane"):
                command("inner", "bind-key", "-n", key, "send-keys", "-M")
            command("outer", "new-session", "-d", "-s", "outer", "-x", "80", "-y", "24", "env", "TERM=xterm-256color", *commands["inner"], "attach-session", "-t", "inner")
            command("outer", "set-option", "-g", "status", "off")
            wait(lambda: command("inner", "list-clients", "-F", "#{client_name}").strip())
            observations = {}
            modes = {"disabled": b"", "normal": b"\x1b[?1000h", "drag": b"\x1b[?1002h", "all": b"\x1b[?1003h", "utf8": b"\x1b[?1000h\x1b[?1005h", "sgr": b"\x1b[?1000h\x1b[?1006h", "sgr-drag": b"\x1b[?1002h\x1b[?1006h", "sgr-all": b"\x1b[?1003h\x1b[?1006h"}
            for index, (name, mode) in enumerate(modes.items()):
                output = root / f"bytes-{index}"
                script = "import os,tty,sys\ntty.setraw(0)\nos.write(1,bytes.fromhex(sys.argv[1])+b'READY')\nf=open(sys.argv[2],'wb',buffering=0)\nwhile True: f.write(os.read(0,4096))"
                command("inner", "respawn-pane", "-k", "python3", "-u", "-c", script, mode.hex(), str(output))
                wait(lambda: output.exists() and b"READY" in command("inner", "capture-pane", "-p"))
                events = [("press", b"\x1b[<0;3;2M"), ("drag", b"\x1b[<32;4;2M"), ("release", b"\x1b[<0;4;2m"), ("move", b"\x1b[<35;5;2M"), ("wheel", b"\x1b[<64;5;2M"), ("modified", b"\x1b[<24;6;2M")]
                for event, wire in events:
                    before = len(output.read_bytes())
                    command("outer", "send-keys", "-l", wire.decode() + "Z")
                    wait(lambda: len(output.read_bytes()) > before and output.read_bytes().endswith(b"Z"))
                    observations[f"{name}:{event}"] = output.read_bytes()[before:-1].hex()
            # Explicitly injected focus is forwarded according to DEC1004;
            # focus-events controls the upstream subscription, not this input.
            for option in ("on", "off"):
                for enabled in (False, True):
                    output = root / f"focus-{option}-{enabled}"
                    command("inner", "set-option", "-g", "focus-events", option)
                    command("inner", "respawn-pane", "-k", "python3", "-u", "-c", script, (b"\x1b[?1004h" if enabled else b"").hex(), str(output))
                    wait(lambda: output.exists() and b"READY" in command("inner", "capture-pane", "-p"))
                    command("outer", "send-keys", "-l", "\x1b[IZ")
                    wait(lambda: output.read_bytes().endswith(b"Z"))
                    before = len(output.read_bytes())
                    for wire in ("\x1b[O", "\x1b[I"):
                        command("outer", "send-keys", "-l", wire)
                    command("outer", "send-keys", "-l", "Z")
                    wait(lambda: len(output.read_bytes()) > before and output.read_bytes().endswith(b"Z"))
                    observations[f"focus:{option}:{int(enabled)}"] = output.read_bytes()[before:-1].hex()
            for enabled in (False, True):
                output = root / f"paste-{enabled}"
                command("inner", "respawn-pane", "-k", "python3", "-u", "-c", script, (b"\x1b[?2004h" if enabled else b"").hex(), str(output))
                wait(lambda: output.exists() and b"READY" in command("inner", "capture-pane", "-p"))
                command("inner", "set-buffer", "-b", "input", "a\nb\rc\t")
                # Native frontend policy: unbracketed LF becomes CR; bracketed
                # text retains LF. Choose C's equivalent explicit paste mode.
                command("inner", "paste-buffer", "-p", *(["-r"] if enabled else []), "-b", "input", "-t", "%0")
                command("inner", "send-keys", "-l", "Z")
                wait(lambda: output.read_bytes().endswith(b"Z"))
                observations[f"paste:{int(enabled)}"] = output.read_bytes()[:-1].hex()
            return observations
        finally:
            for command_line in commands.values():
                subprocess.run(command_line + ["kill-server"], capture_output=True, timeout=5)


if __name__ == "__main__":
    print(json.dumps(run(str(Path(sys.argv[1]).resolve()))))
