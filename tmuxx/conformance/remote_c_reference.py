#!/usr/bin/env python3
"""Byte-exact, leased C oracle over authenticated no-PTY SSH.

This supplies reference observations to native Rust tests. It never makes the
remote Unix server evidence of native Windows pane ownership or presentation.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import platform
from pathlib import Path
import queue
import re
import runpy
import shlex
import signal
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
import traceback

# Whole-cell style snapshots in the original shell matrix exceed four MiB.
# This test-only SSH oracle retains an explicit finite bound for those records.
MAX_FRAME = 64 * 1024 * 1024
MAX_ARGUMENTS = 256
COMMAND_TIMEOUT = 10
LEASE_SECONDS = 180
SCHEMA = 1
OBSERVER_TIMEOUT = 120
CONTROL_SOURCE_SHA256 = "920ffdda59a6d6d8e3dad9eb390fa6ba20caa3ce71559c75aeff38985596509f"
OBSERVER_SOURCES = {
    'rust/conformance/upstream_reference.py': '4d959de71ecc8cc0d0638ca588910e4ae44b289ff9f90359e8c43911c5dd7d2e',
    'regress/input-cursor.sh': 'a0d496d75b6e696a4087790941b6b47f6255d1e2f484f7734c3928cd87b33360',
    'regress/input-edit.sh': '70ed33ead16a35f6f0abf8f2b8a41d905780f4cadcc1c4e0973c5c4ae89fcfcc',
    'regress/input-lines-region.sh': '406c9c5452a5b1331815f0ca2c9cb67bafaae7e71e4ea6dd81a8d3e0dcd234ed',
    'regress/input-modes.sh': '9bf0d9a3a4f25e405765774bf712012d4f94ef52b06ced90989db085fddeb693',
    'regress/input-malformed.sh': '611ae74504cbc5afaa432eb1b644f96884ae08842a6c9d24f5f262095aae0afb',
    'regress/input-scroll.sh': '9e424d55a055852e88e895a89fd41b0e41197c9cc63ca821ad154f1235d6f918',
    'regress/input-sgr.sh': '36ade2236d0b0352050f4d5b301f52acfa270b56ec300aea6f4c5bf1dec5a2b6',
    'regress/input-unicode.sh': '2a5845dda24bce3d8e2b2058d7e05b8eb8c025b3e1e49fd31815d27661b4958b',
    'regress/input-osc.sh': '0e7abc64fde163e29a1872821f1d0d216cd295d1bdde64d1a5a4f9c2c86503c8',
    'regress/format-fuzzy.sh': 'a1a9070e0437cd11c0ba6d9675b204a40d671ba139ce2598e53f416e2eeecaf1',
    'regress/format-modifiers.sh': '801482bb3ca29a2fde6d612d39c1a60433e6c12745908219130eb5aa47f4f0fd',
    'regress/format-no-current-window.sh': 'e6f8d85a9269183d6b2099d96a00026bb3464545409b2c2fe530a398d54d1615',
    'regress/format-time-range.sh': '61b1852be13bd652cef0297eb3cc2899bdb9d3bbbe42372051a2ca247e5ad72b',
    'regress/json.sh': 'd3f6929e463a39133352e98a29a737af3ac9d6ca59e7c652e8d0454411b2f7d4',
    'regress/buffers.sh': '498a77913c395006940d32b3b97f4844c4ec33c753c86999932680dc2a447110',
    'regress/set-hook-B.sh': '25e484e525b32ae777e6d2e62f80979195ceefd5434e81e8e55994b89279c721',
    'regress/targets.sh': 'ec3acd53877b645abf848a849fd6dd6b97a7d79142fc599e96e4c56d9a0a1f34',
    'regress/targets-panes.sh': 'be45c059315ae2c52a515944a177ee1ab933d31d76b95573ba902ae08394eaa6',
    'regress/targets-current.sh': '320298b6be52042c02152f248cfb7d7b20819f956080bb2b26e1b5c216331888',
    'regress/layout-nesting-limit.sh': '8910895d29ea65bc285b9de1bdd51b1e98f42853a23263e12c9407d045fc17fd',
    'regress/control-client-exit.sh': '8ac69f4fc56a3ead1e95f6bf90eda39728901801c8dbcdecee6a521ae1c47994',
    'regress/control-client-exit-stalled.sh': '9ed328c6f9122e8553a30611dfeb481e911a59f21a6876acad5eb4add54d066f',
    'regress/control-client-run-shell-exit.sh': 'd487aea5e59ddafc665e2b4e5733b66379182267fd78d53ae4956a6fa6df882c',
    'regress/control-client-wait-exit.sh': '9d7132b9fe3a66598235a5dfb87c8a38911cc9f753dca2628f6ad53b266bb639',
    'regress/control-notify-guard.sh': '63eee66509db800370b87cb51cf908a5486d23b0f6946337b6e805b649bcc9ce',
    'regress/control-subscriptions.sh': '789bd4e7afd259770e5dace3c2266aeeb2978e9051e3167ab49d87ef9854f5c7',
    'regress/format-strings.sh': '1e61dae925626c36a18c9d21e5bab76669ded8c6dee30350275ca3e2fadf803e',
    'regress/input-raw-controls.sh': 'b2191ed6fcefd7562b04e6731dad26d09f5f752464561d4c76fcdc96f5fbeabb',
    'regress/input-nowrap-collect.sh': 'ef332f7f382dbd24b4710797a7103f64a01a1deb53e514f25d76d3c8ad104fb7',
    'regress/input-common.inc': '6e327584c8e2d1bf47ad787ce0148836bdffa466cbaa220b43e0fa06da0cee58',
    "rust/conformance/native_input.py": "6c068593abed9ba89e2b1dcc38a914d629c17e3516e84da04f8aae8677379090",
    "rust/conformance/image_pixels.py": "fe407a4825886e514dc18695ea33af56f95d1fdbfdbe5369e66345e7c9ff2c81",
    "rust/conformance/image_quota.py": "debd0a685b9b78f2c55086dc3373e2265bfd69a81102a4380d90681733ad5368",
    "rust/conformance/shell_output.py": "057c66e2b2fd4f8b80b9e0d262a6817bb65bcc8243b0e549b1758f780671b5b6",
    "regress/image_test.py": "cac4a44fcbc0123acc2d5b95fb491d074aa01f510efa5c17fcc70f0d97fff963",
    "regress/image_terminal.py": "aea7c9fb32be9eac0f59219463456d38d4ebf838d4391c76d728867ae487b5ab",
}
OBSERVERS = {name: [f"rust/conformance/{name}.py"] for name in
             ("native_input", "image_pixels", "image_quota", "shell_output")}
OBSERVERS["image_pixels"] += ["regress/image_test.py", "regress/image_terminal.py"]
OBSERVERS["upstream_reference"] = ['rust/conformance/upstream_reference.py', 'regress/input-cursor.sh', 'regress/input-edit.sh', 'regress/input-lines-region.sh', 'regress/input-modes.sh', 'regress/input-malformed.sh', 'regress/input-scroll.sh', 'regress/input-sgr.sh', 'regress/input-unicode.sh', 'regress/input-osc.sh', 'regress/format-fuzzy.sh', 'regress/format-modifiers.sh', 'regress/format-no-current-window.sh', 'regress/format-time-range.sh', 'regress/json.sh', 'regress/buffers.sh', 'regress/set-hook-B.sh', 'regress/targets.sh', 'regress/targets-panes.sh', 'regress/targets-current.sh', 'regress/layout-nesting-limit.sh', 'regress/control-client-exit.sh', 'regress/control-client-exit-stalled.sh', 'regress/control-client-run-shell-exit.sh', 'regress/control-client-wait-exit.sh', 'regress/control-notify-guard.sh', 'regress/control-subscriptions.sh', 'regress/format-strings.sh', 'regress/input-raw-controls.sh', 'regress/input-nowrap-collect.sh', 'regress/input-common.inc']


def control_source():
    source = Path(__file__).with_name("remote_c_control.py").read_bytes().replace(b"\r\n", b"\n")
    if hashlib.sha256(source).hexdigest() != CONTROL_SOURCE_SHA256:
        raise ValueError("persistent C client support differs from reviewed SHA-256")
    return source


def observer_path(relative):
    from source_paths import TMUXX, c_source
    if relative.startswith("rust/conformance/"):
        return TMUXX / relative.removeprefix("rust/")
    return c_source() / relative


def observer_bundle(name, data):
    if name not in OBSERVERS:
        raise ValueError("unknown approved C observer")
    sources = {}
    for relative in OBSERVERS[name]:
        # Python compiles source with universal newlines on both hosts. Pin that
        # canonical source while provenance separately hashes each checkout file.
        source = observer_path(relative).read_bytes().replace(b"\r\n", b"\n")
        if hashlib.sha256(source).hexdigest() != OBSERVER_SOURCES[relative]:
            raise ValueError("C observer source differs from reviewed SHA-256: " + relative)
        sources[relative] = base64.b64encode(source).decode("ascii")
    return json.dumps({"input": base64.b64encode(data).decode("ascii"), "sources": sources}).encode()


def read_exact(stream, length):
    data = bytearray()
    while len(data) < length:
        part = stream.read(length - len(data))
        if not part:
            raise EOFError("truncated oracle frame")
        data.extend(part)
    return bytes(data)


def read_frame(stream):
    first = stream.read(1)
    if not first:
        return None
    length = struct.unpack("!I", first + read_exact(stream, 3))[0]
    if length > MAX_FRAME:
        raise ValueError("oracle frame exceeds byte bound")
    return read_exact(stream, length)


def write_frame(stream, data):
    if len(data) > MAX_FRAME:
        raise ValueError("oracle frame exceeds byte bound")
    stream.write(struct.pack("!I", len(data)) + data)
    stream.flush()


def request(operation, args=(), data=b""):
    if len(args) > MAX_ARGUMENTS or any(b"\0" in arg for arg in args):
        raise ValueError("invalid oracle argv")
    payload = bytes([operation]) + struct.pack("!I", len(args))
    for arg in args:
        payload += struct.pack("!I", len(arg)) + arg
    payload += struct.pack("!I", len(data)) + data
    if len(payload) > MAX_FRAME:
        raise ValueError("oracle request exceeds byte bound")
    return payload


def unpack_request(data):
    if len(data) < 9 or len(data) > MAX_FRAME:
        raise ValueError("invalid oracle request length")
    operation, count = data[0], struct.unpack("!I", data[1:5])[0]
    if count > MAX_ARGUMENTS:
        raise ValueError("oracle argv exceeds count bound")
    offset = 5
    values = []
    for _ in range(count + 1):
        if len(data) - offset < 4:
            raise ValueError("truncated oracle field")
        length = struct.unpack("!I", data[offset:offset + 4])[0]
        offset += 4
        if length > len(data) - offset:
            raise ValueError("truncated oracle value")
        values.append(data[offset:offset + length])
        offset += length
    if offset != len(data) or any(b"\0" in arg for arg in values[:-1]):
        raise ValueError("invalid oracle argv or trailing data")
    return operation, values[:-1], values[-1]


def response(code, stdout=b"", stderr=b""):
    payload = struct.pack("!iI", code, len(stdout)) + stdout + struct.pack("!I", len(stderr)) + stderr
    if len(payload) > MAX_FRAME:
        raise ValueError("oracle response exceeds byte bound")
    return payload


def unpack_response(payload):
    if len(payload) < 12:
        raise ValueError("truncated oracle response")
    code, out_length = struct.unpack("!iI", payload[:8])
    end = 8 + out_length
    if end + 4 > len(payload):
        raise ValueError("truncated oracle stdout")
    err_length = struct.unpack("!I", payload[end:end + 4])[0]
    if end + 4 + err_length != len(payload):
        raise ValueError("truncated oracle stderr or trailing data")
    return code, payload[8:end], payload[end + 4:]


def unpack_environment(payload):
    if len(payload) < 4:
        raise ValueError("truncated oracle environment")
    count, offset, environment = struct.unpack("!I", payload[:4])[0], 4, {}
    if count > 256:
        raise ValueError("oracle environment exceeds variable bound")
    for _ in range(count):
        values = []
        for _ in range(2):
            if offset + 4 > len(payload):
                raise ValueError("truncated oracle environment field")
            length = struct.unpack("!I", payload[offset:offset + 4])[0]
            offset += 4
            if length == 0xffffffff:
                values.append(None)
            else:
                if length > len(payload) - offset:
                    raise ValueError("truncated oracle environment value")
                values.append(payload[offset:offset + length])
                offset += length
        key, value = values
        if (not key or b"=" in key or b"\0" in key or key in environment
                or value is not None and b"\0" in value):
            raise ValueError("invalid or duplicated oracle environment name/value")
        environment[key] = value
    return environment, payload[offset:]


def environment_override(environment, changes):
    result = {os.fsencode(key): os.fsencode(value) for key, value in environment.items()}
    for key, value in changes.items():
        if value is None:
            result.pop(key, None)
        else:
            result[key] = value
    return result


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def reference_identity(binary, expected_version, expected_hash, helper_hash):
    if os.name != "posix":
        raise ValueError("C reference host must supply a native POSIX C tmux")
    if digest(os.fsdecode(binary)) != expected_hash:
        raise ValueError("remote C executable bytes differ from pinned reference")
    version = subprocess.run([binary, b"-V"], capture_output=True, timeout=COMMAND_TIMEOUT, check=True)
    if version.stdout != expected_version + b"\n" or digest(os.fsdecode(binary)) != expected_hash:
        raise ValueError("remote C executable version or bytes differ from pinned reference")
    return {"schema": SCHEMA, "binary": os.fsdecode(binary), "version": expected_version.decode("utf-8"),
            "sha256": expected_hash, "helper_sha256": helper_hash,
            "host": list(os.uname()), "libc": list(platform.libc_ver()), "pid": os.getpid(),
            "control_helper_sha256": CONTROL_SOURCE_SHA256}


def enable_descendant_owner():
    """A private Linux oracle helper adopts descendants even after pane exits."""
    if not sys.platform.startswith("linux"):
        raise ValueError("remote oracle fixture ownership requires Linux PR_SET_CHILD_SUBREAPER")
    import ctypes
    libc = ctypes.CDLL(None, use_errno=True)
    libc.prctl.argtypes = [ctypes.c_int, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong]
    libc.prctl.restype = ctypes.c_int
    if libc.prctl(36, 1, 0, 0, 0):  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), "cannot establish private oracle descendant ownership")


def owned_children():
    return [int(pid) for pid in Path(f"/proc/self/task/{os.getpid()}/children").read_text().split()]


def reclaim_owned_children():
    deadline = time.monotonic() + 5
    while (children := owned_children()):
        for pid in children:
            try:
                # waitpid confirms this remains our child. A live child which
                # subsequently exits stays a zombie until we reap it, so its
                # PID cannot be reused between this check and the signal.
                waited, _ = os.waitpid(pid, os.WNOHANG)
                if not waited:
                    os.kill(pid, signal.SIGKILL)
            except (ChildProcessError, ProcessLookupError):
                pass
        if time.monotonic() >= deadline:
            return owned_children()
        time.sleep(.01)
    return []


def validate_config(config):
    required = ("host", "key", "known_hosts", "binary", "version", "sha256")
    if not isinstance(config, dict) or any(not isinstance(config.get(key), str) or not config[key] for key in required):
        raise ValueError("remote reference needs explicit host, key, known_hosts, binary, version and sha256")
    if config["host"].startswith("-") or any(char.isspace() or char == "\0" for char in config["host"]):
        raise ValueError("invalid explicit SSH destination")
    if not config["binary"].startswith("/") or "\0" in config["binary"]:
        raise ValueError("remote C binary must be an absolute POSIX path")
    if not config["version"].startswith("tmux ") or not re.fullmatch("[0-9a-f]{64}", config["sha256"]):
        raise ValueError("remote C reference needs exact tmux version and lowercase SHA-256")
    for key in ("key", "known_hosts"):
        if not Path(config[key]).is_file():
            raise ValueError(f"explicit SSH {key} file missing")
    port = config.get("port", 22)
    if type(port) is not int or not 1 <= port <= 65535:
        raise ValueError("invalid SSH port")
    return config


def ssh_command(config, source_length):
    # Only fixed bootstrap code enters the shell. Paths, versions, bytes and
    # mutation commands are subsequent framed stdin, never shell interpolation.
    bootstrap = (f"import sys;source=sys.stdin.buffer.read({source_length});"
                 "exec(compile(source,'<tmuxx-remote-oracle>','exec'),"
                 "{'__name__':'__tmuxx_remote__'})")
    command = "exec python3 -c " + shlex.quote(bootstrap)
    return [config.get("ssh", "ssh"), "-F", "none", "-T", "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes",
            "-o", "StrictHostKeyChecking=yes", "-o", "UserKnownHostsFile=" + str(Path(config["known_hosts"]).resolve()),
            "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=2",
            "-p", str(config.get("port", 22)), "-i", str(Path(config["key"]).resolve()), "--", config["host"], command]


class TimedFrames:
    """One bounded reader thread per stream; timeout kills its owning process."""
    def __init__(self, stream):
        self.values = queue.Queue(maxsize=1)
        def reader():
            try:
                while True:
                    value = read_frame(stream)
                    self.values.put(value)
                    if value is None:
                        return
            except BaseException as error:
                self.values.put(error)
        self.thread = threading.Thread(target=reader, daemon=True)
        self.thread.start()

    def next(self, timeout):
        try:
            value = self.values.get(timeout=timeout)
        except queue.Empty:
            raise TimeoutError("remote reference response exceeded its lease") from None
        if isinstance(value, BaseException):
            raise value
        return value


class OracleFixture:
    """Own a foreground C server and only a fresh private socket/config path."""
    def __init__(self, binary, expected_version, expected_hash, helper_hash, environment=None,
                 configuration=None, support=b""):
        self.binary = binary
        self.expected_hash = expected_hash
        self.server = None
        self.directory = None
        self.controls = None
        self.identity = reference_identity(binary, expected_version, expected_hash, helper_hash)
        enable_descendant_owner()
        self.identity["ownership"] = "linux-child-subreaper"
        try:
            self.directory = tempfile.TemporaryDirectory(prefix="tx-oracle-")
            os.chmod(self.directory.name, 0o700)
            self.socket = os.fsencode(str(Path(self.directory.name) / "socket"))
            self.source = Path(self.directory.name) / "source.conf"
            self.files = os.fsencode(str(Path(self.directory.name) / "files"))
            os.mkdir(self.files, 0o700)
            self.env = os.environ.copy()
            self.env.pop("TMUX", None)
            self.env.pop("EDITOR", None)
            self.env.pop("VISUAL", None)
            self.env.update(LC_ALL="C.UTF-8", TZ="UTC", SHELL="/bin/sh")
            self.env = environment_override(self.env, environment or {})
            if configuration is None:
                self.server = subprocess.Popen([binary, b"-D", b"-f", b"/dev/null", b"-S", self.socket],
                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                    env=self.env, start_new_session=True, close_fds=True)
            else:
                self.source.write_bytes(configuration)
                manager = self.control_manager(support)
                manager.start_configuration(self.source)
                self.server = manager.adopted_server()
            deadline = time.monotonic() + COMMAND_TIMEOUT
            while not Path(os.fsdecode(self.socket)).exists():
                if self.server.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("foreground C reference server did not become ready")
                time.sleep(.02)
            self.ensure_identity()
            self.identity["server_pid"] = self.server.pid
            self.identity["socket"] = os.fsdecode(self.socket)
            self.identity["fixture_root"] = os.fsdecode(self.files)
            self.identity["server_start_ticks"] = Path(f"/proc/{self.server.pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
            self.identity["socket_inode"] = os.stat(self.socket).st_ino
            self.identity["expires_unix"] = int(time.time()) + LEASE_SECONDS - 10
        except BaseException:
            self.close()
            raise

    def ensure_identity(self):
        if digest(os.fsdecode(self.binary)) != self.expected_hash:
            raise ValueError("remote C executable bytes differ from pinned reference")

    def command(self, args, data=b"", environment=None):
        if self.server is None or self.server.poll() is not None:
            raise RuntimeError("reference incarnation ended; mutations are never replayed")
        self.ensure_identity()
        completed = subprocess.run([self.binary, b"-N", b"-f", b"/dev/null", b"-S", self.socket, *args],
            input=data, capture_output=True, timeout=COMMAND_TIMEOUT,
            env=environment_override(self.env, environment or {}), cwd=self.files)
        self.ensure_identity()
        # Cleanup uses the kernel subreaper's actual owned children. Injecting
        # list-panes here would run hooks and change the command under test.
        return response(completed.returncode, completed.stdout, completed.stderr)

    def upload(self, data):
        self.source.write_bytes(data)
        return self.command([b"source-file", os.fsencode(self.source)])

    def control_manager(self, source=b""):
        self.ensure_identity()
        if source and hashlib.sha256(source).hexdigest() != CONTROL_SOURCE_SHA256:
            raise ValueError("persistent C client support differs from reviewed SHA-256")
        if self.controls is None:
            if not source:
                raise ValueError("persistent C clients require their pinned support first")
            module = {"__name__": "__tmuxx_private_controls__"}
            exec(compile(source, "<approved-tmuxx-c-controls>", "exec"), module)
            self.controls = module["Controls"](self.binary, self.socket, self.env, self.files)
        return self.controls

    def file(self, operation, leaf, data=b""):
        # POSIX byte names stay bytes, including invalid UTF-8. Files are in a
        # separate private directory and cannot name sockets/configuration or
        # traverse outside it. Never follow a pane-created symlink.
        if not leaf or leaf in (b".", b"..") or any(value in leaf for value in (b"/", b"\\", b"\0")):
            raise ValueError("oracle fixture file needs a single safe byte-exact leaf")
        if len(leaf) > 255 or (operation != 5 and data):
            raise ValueError("invalid oracle fixture file request")
        self.ensure_identity()
        path = self.files + b"/" + leaf
        if operation == 8:
            return response(0, path)
        if operation == 7:
            try:
                information = os.stat(path, follow_symlinks=False)
            except FileNotFoundError:
                return response(0, b"0")
            if not stat.S_ISREG(information.st_mode):
                raise ValueError("oracle fixture path is not a regular file")
            return response(0, b"1")
        flags = os.O_NOFOLLOW | os.O_NONBLOCK | (os.O_WRONLY | os.O_CREAT if operation == 5 else os.O_RDONLY)
        descriptor = os.open(path, flags, 0o600)
        with os.fdopen(descriptor, "wb" if operation == 5 else "rb") as stream:
            information = os.fstat(stream.fileno())
            if not stat.S_ISREG(information.st_mode) or information.st_nlink != 1:
                raise ValueError("oracle fixture path is not a regular file")
            if operation == 5:
                stream.truncate(0)
                stream.write(data)
                return response(0, path)
            contents = stream.read(MAX_FRAME - 11)
            if len(contents) > MAX_FRAME - 12:
                raise ValueError("oracle fixture file exceeds response bound")
            return response(0, contents)

    def observe(self, name, bundle):
        if name not in OBSERVERS:
            raise ValueError("unknown approved C observer")
        supplied = json.loads(bundle)
        if set(supplied) != {"input", "sources"} or set(supplied["sources"]) != set(OBSERVERS[name]):
            raise ValueError("C observer bundle has unexpected sources")
        self.ensure_identity()
        with tempfile.TemporaryDirectory(prefix="observer-", dir=self.directory.name) as temporary:
            root = Path(temporary)
            for relative in OBSERVERS[name]:
                source = base64.b64decode(supplied["sources"][relative], validate=True)
                if hashlib.sha256(source).hexdigest() != OBSERVER_SOURCES[relative]:
                    raise ValueError("C observer source differs from reviewed SHA-256: " + relative)
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(source)
            fixture = root / "input.json"
            fixture.write_bytes(base64.b64decode(supplied["input"], validate=True))
            result = root / "result.json"
            stdout, stderr, cleanup_path = (root / value for value in ("stdout", "stderr", "cleanup.json"))
            arguments = [str(root / OBSERVERS[name][0]), os.fsdecode(self.binary)]
            if name != "native_input":
                arguments += [str(fixture), str(result)]
            pid = os.fork()
            if pid == 0:
                # A separate subreaper owns every helper-created foreground and
                # background generation, including failed Python fixtures.
                status = 0
                with stdout.open("wb") as out, stderr.open("wb") as err:
                    os.dup2(out.fileno(), 1)
                    os.dup2(err.fileno(), 2)
                    try:
                        enable_descendant_owner()
                        signal.alarm(OBSERVER_TIMEOUT)
                        sys.argv = arguments
                        os.environb.update(self.env)
                        runpy.run_path(arguments[0], run_name="__main__")
                    except BaseException:
                        traceback.print_exc()
                        status = 125
                    finally:
                        signal.alarm(0)
                        try:
                            remaining = reclaim_owned_children()
                            cleanup_path.write_text(json.dumps({"cleanup_remaining": remaining, "cleanup_errors": []}))
                            status = status or int(bool(remaining))
                        except BaseException as error:
                            cleanup_path.write_text(json.dumps({"cleanup_remaining": [], "cleanup_errors": [str(error)]}))
                            status = 125
                        sys.stdout.flush()
                        sys.stderr.flush()
                os._exit(status)
            deadline = time.monotonic() + OBSERVER_TIMEOUT + 10
            while True:
                waited, status = os.waitpid(pid, os.WNOHANG)
                if waited:
                    break
                if time.monotonic() >= deadline:
                    # The unreaped child cannot be replaced through PID reuse.
                    os.kill(pid, signal.SIGKILL)
                    os.waitpid(pid, 0)
                    raise TimeoutError("approved C observer exceeded deadline")
                time.sleep(.02)
            self.ensure_identity()
            cleanup = json.loads(cleanup_path.read_text()) if cleanup_path.exists() else {}
            if cleanup.get("cleanup_remaining") != [] or cleanup.get("cleanup_errors") != []:
                raise ValueError("approved C observer did not prove descendant cleanup")
            metadata = {"schema": SCHEMA, "name": name, "fixture": self.identity["socket"],
                        "sources": {relative: OBSERVER_SOURCES[relative] for relative in OBSERVERS[name]}, **cleanup}
            observed = stdout.read_bytes() if name == "native_input" else result.read_bytes() if result.exists() else b""
            code = os.waitstatus_to_exitcode(status)
            diagnostic = b"TMUXX_REMOTE_C_OBSERVER=" + json.dumps(metadata, sort_keys=True).encode() + b"\n" + stderr.read_bytes()
            return response(code, observed, diagnostic)

    def close(self):
        errors = []
        if getattr(self, "controls", None) is not None:
            errors.extend(self.controls.close_all())
        if self.server is not None:
            try:
                if self.server.poll() is None:
                    self.server.terminate()
                try:
                    self.server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self.server.kill()
                    self.server.wait(timeout=5)
                if hasattr(self.server, "close"):
                    self.server.close()
            except (OSError, subprocess.SubprocessError) as error:
                errors.append(str(error))
        try:
            remaining = reclaim_owned_children()
        except OSError as error:
            remaining = [self.server.pid] if self.server and self.server.poll() is None else []
            errors.append(str(error))
        if self.directory is not None:
            try:
                self.directory.cleanup()
            except OSError as error:
                errors.append(str(error))
        return {"schema": SCHEMA, "fixture": self.identity.get("socket"),
                "cleanup_remaining": sorted(remaining), "cleanup_errors": errors}


def agent_main():
    fixture = None
    cleanup = None
    def expire(*_):
        raise TimeoutError("remote C reference fixture lease expired")
    signal.signal(signal.SIGALRM, expire)
    signal.signal(signal.SIGTERM, expire)
    signal.signal(signal.SIGHUP, expire)
    signal.alarm(LEASE_SECONDS)
    try:
        initial = read_frame(sys.stdin.buffer)
        if initial is None:
            return
        operation, args, data = unpack_request(initial)
        if operation not in (0, 4, 22) or len(args) != (5 if operation == 22 else 4) or (operation == 4 and data):
            raise ValueError("reference handshake must precede any mutation")
        binary, version, sha256, helper_hash = args[:4]
        if not binary.startswith(b"/") or not version.startswith(b"tmux ") or not re.fullmatch(b"[0-9a-f]{64}", sha256):
            raise ValueError("invalid pinned reference identity")
        if operation == 4:
            identity = reference_identity(binary, version, sha256.decode("ascii"), helper_hash.decode("ascii"))
            write_frame(sys.stdout.buffer, response(0, json.dumps(identity).encode()))
            return
        environment, trailing = unpack_environment(data) if data and operation == 0 else ({}, b"")
        if trailing:
            raise ValueError("unexpected startup environment bytes")
        fixture = OracleFixture(binary, version, sha256.decode("ascii"), helper_hash.decode("ascii"), environment,
                                data if operation == 22 else None, args[4] if operation == 22 else b"")
        write_frame(sys.stdout.buffer, response(0, json.dumps(fixture.identity).encode()))
        while (payload := read_frame(sys.stdin.buffer)) is not None:
            operation, args, data = unpack_request(payload)
            if operation == 1 and args:
                result = fixture.command(args, data)
            elif operation == 2 and not args:
                result = fixture.upload(data)
            elif operation in (5, 6, 7, 8) and len(args) == 1:
                result = fixture.file(operation, args[0], data)
            elif operation == 9 and len(args) == 1:
                result = fixture.observe(args[0].decode("ascii"), data)
            elif operation in (10, 25) and len(args) >= 2 and args[0] in (b"control", b"command"):
                if fixture.server.poll() is not None:
                    raise RuntimeError("reference incarnation ended; persistent commands are never replayed")
                environment = None
                if operation == 25:
                    changes, data = unpack_environment(data)
                    environment = environment_override(fixture.env, changes)
                result = response(0, fixture.control_manager(data).open(args[1:], args[0] == b"control", environment))
            elif operation in (11, 12, 13, 14, 15, 16, 19) and len(args) == 1 and args[0].isdigit():
                identifier = int(args[0])
                manager = fixture.control_manager(data if operation == 15 else b"")
                if operation == 11:
                    manager.write(identifier, data)
                    result = response(0)
                elif operation == 15:
                    result = response(0, manager.watch(identifier))
                elif data:
                    raise ValueError("unexpected persistent C client payload")
                elif operation == 12:
                    output, errors = manager.read(identifier)
                    result = response(0, output, errors)
                elif operation == 13:
                    manager.close(identifier)
                    result = response(0)
                elif operation == 14:
                    result = response(0, manager.exited(identifier))
                elif operation == 19:
                    manager.finish_input(identifier)
                    result = response(0)
                else:
                    result = response(0, manager.alive(identifier))
            elif operation == 17 and not args and not data:
                result = response(0, fixture.files)
            elif operation == 18 and not args and not data:
                result = response(0, b"1" if fixture.server.poll() is None else b"0")
            elif operation == 20 and args:
                environment, stdin = unpack_environment(data)
                result = fixture.command(args, stdin, environment)
            elif operation == 23 and not args and not data:
                result = response(0, fixture.control_manager().malformed_startup_clients())
            elif operation == 24 and not args and not data:
                startup = fixture.control_manager().startup
                if startup is None:
                    raise ValueError("this fixture did not use an implicit startup client")
                result = response(0, startup)
            elif operation == 3 and not args and not data:
                fixture.ensure_identity()
                cleanup = fixture.close()
                write_frame(sys.stdout.buffer, response(int(bool(cleanup["cleanup_remaining"] or cleanup["cleanup_errors"])),
                                                        json.dumps(cleanup).encode()))
                return
            else:
                raise ValueError("invalid oracle operation; mutations are never replayed")
            write_frame(sys.stdout.buffer, result)
    except (Exception, KeyboardInterrupt) as error:
        try:
            write_frame(sys.stdout.buffer, response(125, stderr=str(error).encode("utf-8")))
        except (OSError, ValueError):
            pass
        raise
    finally:
        signal.alarm(0)
        if fixture is not None and cleanup is None:
            cleanup = fixture.close()
        if cleanup is not None and (cleanup["cleanup_remaining"] or cleanup["cleanup_errors"]):
            print(json.dumps(cleanup), file=sys.stderr)


def local_stdin():
    # TimedFrames can still be waiting for another caller frame when SSH fails.
    # A daemon holding BufferedReader's lock aborts Python during interpreter
    # shutdown; FileIO preserves the same binary stream without that lock.
    return sys.stdin.buffer.raw


def bridge(config, input_stream=None, output_stream=None, probe=False, environment_stdin=False, configuration_stdin=False):
    config = validate_config(config)
    input_stream = input_stream or local_stdin()
    output_stream = output_stream or sys.stdout.buffer
    source = Path(__file__).read_bytes()
    helper_hash = hashlib.sha256(source).hexdigest()
    environment = b""
    configuration = None
    if configuration_stdin:
        operation, arguments, configuration = unpack_request(read_frame(input_stream))
        if operation != 22 or arguments or environment_stdin or probe:
            raise ValueError("invalid local fixture configuration handshake")
    if environment_stdin:
        operation, arguments, environment = unpack_request(read_frame(input_stream))
        changes, trailing = unpack_environment(environment)
        if operation != 21 or arguments or trailing:
            raise ValueError("invalid local fixture environment handshake")
    child = subprocess.Popen(ssh_command(config, len(source)), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=None, close_fds=True)
    try:
        child.stdin.write(source)
        startup = [config["binary"].encode(), config["version"].encode(), config["sha256"].encode(), helper_hash.encode()]
        if configuration is not None:
            write_frame(child.stdin, request(22, startup + [control_source()], configuration))
        else:
            write_frame(child.stdin, request(4 if probe else 0, startup, environment))
        remote = TimedFrames(child.stdout)
        ready = remote.next(COMMAND_TIMEOUT + 10)
        if ready is None:
            raise EOFError("SSH ended before reference identity")
        code, identity_bytes, diagnostic = unpack_response(ready)
        if code:
            raise RuntimeError("remote identity rejected: " + diagnostic.decode("utf-8", "replace"))
        identity = json.loads(identity_bytes)
        if not isinstance(identity, dict) or type(identity.get("schema")) is not int or any(identity.get(key) != value for key, value in {
                "schema": SCHEMA, "binary": config["binary"], "version": config["version"],
                "sha256": config["sha256"], "helper_sha256": helper_hash}.items()):
            raise ValueError("SSH reference handshake mismatches the pinned identity")
        if probe:
            child.stdin.close()
            if child.wait(timeout=10):
                raise RuntimeError("SSH failed after reference identity probe")
            return identity
        print("TMUXX_REMOTE_C_IDENTITY=" + json.dumps(identity, sort_keys=True), file=sys.stderr, flush=True)
        write_frame(output_stream, ready)
        local = TimedFrames(input_stream)
        deadline = time.monotonic() + LEASE_SECONDS - 10
        while (payload := local.next(max(.01, deadline - time.monotonic()))) is not None:
            operation, args, data = unpack_request(payload)
            if operation not in (1, 2, 3, *range(5, 21), 23, 24, 25):
                raise ValueError("invalid local oracle operation")
            if operation == 9:
                if len(args) != 1:
                    raise ValueError("C observer needs one fixed approved name")
                payload = request(9, args, observer_bundle(args[0].decode("ascii"), data))
            elif operation in (10, 15):
                if data:
                    raise ValueError("persistent C support must come from the pinned local source")
                payload = request(operation, args, control_source())
            elif operation == 25:
                _, extra = unpack_environment(data)
                if extra:
                    raise ValueError("persistent C environment must not include arbitrary support source")
                payload = request(operation, args, data + control_source())
            write_frame(child.stdin, payload)
            result = remote.next(OBSERVER_TIMEOUT + 15 if operation == 9 else COMMAND_TIMEOUT * 2 + 5)
            if result is None:
                raise EOFError("SSH ended with an ambiguous operation; request is not replayed")
            code, observed, diagnostic = unpack_response(result)
            if operation == 9:
                for line in diagnostic.decode("utf-8", "replace").splitlines():
                    if line.startswith("TMUXX_REMOTE_C_OBSERVER="):
                        print(line, file=sys.stderr, flush=True)
            if operation == 3:
                cleanup = json.loads(observed) if not code else None
                if (not isinstance(cleanup, dict) or type(cleanup.get("schema")) is not int
                        or cleanup["schema"] != SCHEMA or cleanup.get("cleanup_remaining") != []
                        or cleanup.get("cleanup_errors") != [] or cleanup.get("fixture") != identity.get("socket")):
                    raise ValueError("remote fixture did not prove complete cleanup")
            write_frame(output_stream, result)
            if operation == 3:
                child.stdin.close()
                if child.wait(timeout=10):
                    raise RuntimeError("SSH failed after fixture cleanup")
                return
    finally:
        if child.stdin and not child.stdin.closed:
            child.stdin.close()  # Remote EOF enters its cleanup, including caller interruption.
        try:
            child.wait(timeout=12)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=5)
        child.stdout.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--probe", action="store_true", help="verify reference identity without starting any server")
    parser.add_argument("--environment-stdin", action="store_true", help="read byte-exact per-fixture environment before startup")
    parser.add_argument("--configuration-stdin", action="store_true", help="start through the actual C client with this private initial configuration")
    args = parser.parse_args()
    try:
        identity = bridge(json.loads(args.config.read_text(encoding="utf-8")), probe=args.probe,
                          environment_stdin=args.environment_stdin, configuration_stdin=args.configuration_stdin)
        if args.probe:
            print(json.dumps(identity, sort_keys=True))
    except (Exception, KeyboardInterrupt) as error:
        print(f"remote C oracle: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__tmuxx_remote__":
    agent_main()
elif __name__ == "__main__":
    raise SystemExit(main())
