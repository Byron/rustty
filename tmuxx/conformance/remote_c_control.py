"""Bounded persistent clients owned by one leased POSIX C oracle fixture."""
from __future__ import annotations

import os
from pathlib import Path
import re
import select
import signal
import socket
import struct
import subprocess
import threading
import time

MAX_CLIENTS = 8
MAX_BACKLOG = 8 * 1024 * 1024
TIMEOUT = 5


class AdoptedServer:
    """The exact daemon child adopted by this fixture's Linux subreaper."""
    def __init__(self, pid):
        self.pid, self.returncode = pid, None
        self.pidfd = os.pidfd_open(pid)

    def poll(self):
        if self.returncode is None:
            waited, status = os.waitpid(self.pid, os.WNOHANG)
            if waited:
                self.returncode = os.waitstatus_to_exitcode(status)
        return self.returncode

    def wait(self, timeout):
        deadline = time.monotonic() + timeout
        while self.poll() is None:
            if time.monotonic() >= deadline:
                raise subprocess.TimeoutExpired("owned C daemon", timeout)
            time.sleep(.01)
        return self.returncode

    def terminate(self):
        signal.pidfd_send_signal(self.pidfd, signal.SIGTERM)

    def kill(self):
        signal.pidfd_send_signal(self.pidfd, signal.SIGKILL)

    def close(self):
        if self.pidfd is not None:
            os.close(self.pidfd)
            self.pidfd = None


class BufferedChild:
    def __init__(self, argv, env, cwd):
        self.child = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      stderr=subprocess.PIPE, env=env, cwd=cwd, close_fds=True)
        self.pidfd = os.pidfd_open(self.child.pid)
        self.lock = threading.Condition()
        self.output, self.errors = bytearray(), bytearray()
        self.eof = False
        self.failure = None
        self.threads = []
        os.set_blocking(self.child.stdin.fileno(), False)
        for stream, destination, primary in ((self.child.stdout, self.output, True),
                                             (self.child.stderr, self.errors, False)):
            def reader(stream=stream, destination=destination, primary=primary):
                try:
                    while data := os.read(stream.fileno(), 65536):
                        with self.lock:
                            if len(self.output) + len(self.errors) + len(data) > MAX_BACKLOG:
                                self.failure = "C client output exceeded its bounded backlog"
                                self.lock.notify_all()
                                return
                            destination.extend(data)
                            self.lock.notify_all()
                except OSError as error:
                    with self.lock:
                        self.failure = str(error)
                finally:
                    with self.lock:
                        if primary:
                            self.eof = True
                        self.lock.notify_all()
            thread = threading.Thread(target=reader, daemon=True)
            thread.start()
            self.threads.append(thread)

    def write(self, data):
        if len(data) > MAX_BACKLOG:
            raise ValueError("C client input exceeds its byte bound")
        deadline = time.monotonic() + TIMEOUT
        offset = 0
        while offset < len(data):
            with self.lock:
                if self.failure:
                    raise RuntimeError(self.failure)
            if self.child.poll() is not None:
                raise RuntimeError("C client exited; partial writes are never replayed")
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([], [self.child.stdin], [], remaining)[1]:
                raise TimeoutError("C client write exceeded deadline; partial writes are never replayed")
            try:
                offset += os.write(self.child.stdin.fileno(), data[offset:])
            except BlockingIOError:
                continue

    def read(self):
        deadline = time.monotonic() + TIMEOUT
        with self.lock:
            while not self.output and not self.eof and not self.failure:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("C client read exceeded deadline; this is not EOF")
                self.lock.wait(remaining)
            if self.failure:
                raise RuntimeError(self.failure)
            data, errors = bytes(self.output[:65536]), bytes(self.errors)
            del self.output[:len(data)]
            self.errors.clear()
            return data, errors

    def close(self):
        self.finish_input()
        if self.child.poll() is None:
            signal.pidfd_send_signal(self.pidfd, signal.SIGTERM)
        try:
            self.child.wait(timeout=TIMEOUT)
        except subprocess.TimeoutExpired:
            signal.pidfd_send_signal(self.pidfd, signal.SIGKILL)
            self.child.wait(timeout=TIMEOUT)
        for thread in self.threads:
            thread.join(timeout=TIMEOUT)
            if thread.is_alive():
                raise TimeoutError("C client reader retained an inherited pipe")
        self.child.stdout.close()
        self.child.stderr.close()
        os.close(self.pidfd)

    def finish_input(self):
        if not self.child.stdin.closed:
            self.child.stdin.close()


class Controls:
    def __init__(self, binary, socket, env, cwd):
        self.prefix = [binary, b"-N", b"-f", b"/dev/null", b"-S", socket]
        self.env, self.cwd = env, cwd
        self.next_id = 0
        self.children, self.closed, self.watches = {}, {}, {}
        self.startup = None

    def start_configuration(self, path):
        if self.children or self.startup is not None:
            raise ValueError("startup configuration requires a fresh private C fixture")
        # Preserve the original cfg-lifetime regression's implicit start:
        # this command client actually forks the daemon and waits for config.
        argv = [self.prefix[0], b"-f", os.fsencode(path), b"-S", self.prefix[-1],
                b"start-server", b";", b"display", b"-p", b"starter"]
        child = BufferedChild(argv, self.env, self.cwd)
        self.next_id += 1
        self.children[self.next_id] = child
        self.startup = struct.pack("!II", self.next_id, child.child.pid)

    def adopted_server(self):
        excluded = {child.child.pid for child in self.children.values()}
        binary = Path(os.fsdecode(self.prefix[0])).resolve()
        deadline = time.monotonic() + TIMEOUT
        while time.monotonic() < deadline:
            candidates = []
            for text in Path(f"/proc/self/task/{os.getpid()}/children").read_text().split():
                pid = int(text)
                try:
                    if (pid not in excluded and Path(f"/proc/{pid}/exe").resolve(strict=True) == binary
                            and Path(f"/proc/{pid}/comm").read_text().strip() == "tmux: server"):
                        candidates.append(pid)
                except FileNotFoundError:
                    pass
            if len(candidates) == 1:
                return AdoptedServer(candidates[0])
            if len(candidates) > 1:
                raise ValueError("ambiguous private C daemon ownership")
            time.sleep(.01)
        raise TimeoutError("configured C daemon was not adopted by its private subreaper")

    def malformed_startup_clients(self):
        if self.startup is None:
            raise ValueError("malformed startup fixture requires actual startup configuration")
        pid = os.getpid()
        packets = b""
        # Byte-for-byte native-endian imsg shapes from the original regression.
        for kind, body in ((108, b"/tmp\0"), (107, struct.pack("=I", pid)), (106, b""), (200, b"")):
            packets += struct.pack("=IIII", kind, 16 + len(body), 8, pid) + body
        started = time.monotonic()
        for _ in range(140):
            if time.monotonic() - started > TIMEOUT:
                raise TimeoutError("malformed startup client matrix exceeded its fixture deadline")
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
                stream.settimeout(TIMEOUT)
                stream.connect(self.prefix[-1])
                stream.sendall(packets)
                try:
                    if stream.recv(1):
                        raise AssertionError("malformed startup client was retained")
                except (ConnectionResetError, BrokenPipeError):
                    pass
        return b"140"

    def open(self, arguments, control=True, environment=None):
        features = []
        while arguments and arguments[0].startswith(b"-"):
            option = arguments[0]
            if option in (b"-2", b"-u", b"-q", b"-C"):
                features.append(option)
                arguments = arguments[1:]
            elif option.startswith(b"-T"):
                separate = option == b"-T"
                if separate and len(arguments) < 2:
                    raise ValueError("missing persistent C terminal feature list")
                value = arguments[1] if separate else option[2:]
                if len(value) > 1024 or not re.fullmatch(rb"[A-Za-z0-9_,:@-]+", value):
                    raise ValueError("invalid persistent C terminal feature list")
                width = 2 if separate else 1
                features.extend(arguments[:width])
                arguments = arguments[width:]
            else:
                break
        if not arguments or arguments[0].startswith(b"-"):
            raise ValueError("persistent C fixture needs command argv, not global endpoint options")
        if len(self.children) >= MAX_CLIENTS:
            raise ValueError("persistent C client count exceeds its bound")
        child = BufferedChild(self.prefix + features + ([b"-C"] if control else []) + arguments,
                              self.env if environment is None else environment, self.cwd)
        self.next_id += 1
        identifier = self.next_id
        self.children[identifier] = child
        name = b""
        if control:
            deadline = time.monotonic() + TIMEOUT
            while not name:
                listing = subprocess.run(self.prefix + [b"list-clients", b"-F", b"#{client_pid}:#{client_name}"],
                                         capture_output=True, env=self.env, timeout=TIMEOUT, close_fds=True)
                for line in listing.stdout.splitlines():
                    pid, separator, value = line.partition(b":")
                    if separator and pid == str(child.child.pid).encode("ascii"):
                        name = value
                        break
                if not name and (child.child.poll() is not None or time.monotonic() >= deadline):
                    raise RuntimeError("persistent C control client failed before attachment")
                if not name:
                    time.sleep(.01)
        return struct.pack("!II", identifier, child.child.pid) + name

    def write(self, identifier, data):
        self.children[identifier].write(data)

    def read(self, identifier):
        return self.children[identifier].read()

    def finish_input(self, identifier):
        self.children[identifier].finish_input()

    def exited(self, identifier):
        code = self.closed[identifier] if identifier in self.closed else self.children[identifier].child.poll()
        return b"running" if code is None else str(code).encode("ascii")

    def close(self, identifier):
        if identifier in self.closed:
            return
        child = self.children[identifier]
        child.close()
        self.closed[identifier] = child.child.returncode
        del self.children[identifier]

    def watch(self, pid):
        if len(self.watches) >= 256:
            raise ValueError("remote process observation count exceeds its bound")
        start = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
        parent = pid
        while parent not in (0, os.getpid()):
            parent = int(Path(f"/proc/{parent}/stat").read_text().rsplit(")", 1)[1].split()[1])
        if parent != os.getpid():
            raise ValueError("observed process does not belong to this private oracle")
        descriptor = os.pidfd_open(pid)
        try:
            if Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19] != start:
                raise ValueError("observed process incarnation changed")
        except BaseException:
            os.close(descriptor)
            raise
        identifier = len(self.watches) + 1
        self.watches[identifier] = descriptor
        return str(identifier).encode("ascii")

    def alive(self, identifier):
        return b"0" if select.select([self.watches[identifier]], [], [], 0)[0] else b"1"

    def close_all(self):
        errors = []
        for identifier in list(self.children):
            try:
                self.close(identifier)
            except (OSError, subprocess.SubprocessError, TimeoutError) as error:
                errors.append(str(error))
        for descriptor in self.watches.values():
            os.close(descriptor)
        self.watches.clear()
        return errors
