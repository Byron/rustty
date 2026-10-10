#!/usr/bin/env python3
"""Compare live POSIX Kitty uploads; recreate each unlinked object for each oracle."""
import base64
import ctypes
import errno
import itertools
import json
import mmap
import os
from pathlib import Path
import subprocess
import sys
import zlib

from graphics_requests import png


LIBC = ctypes.CDLL(None, use_errno=True)
# Darwin declares the mode variadic; its ARM64 ABI passes varargs on the stack.
LIBC.shm_open.argtypes = [ctypes.c_char_p, ctypes.c_int] if sys.platform == "darwin" else [ctypes.c_char_p, ctypes.c_int, ctypes.c_uint]
LIBC.shm_open.restype = ctypes.c_int
LIBC.shm_unlink.argtypes = [ctypes.c_char_p]
LIBC.shm_unlink.restype = ctypes.c_int
IDS = itertools.count()


def cases():
    rgba = b"\x01\x02\x03\xff\x04\x05\x06\x80"
    yield "page-rounded", rgba, "s=2,v=1,f=32", rgba, False
    yield "ignores-chunk-flag", rgba, "s=2,v=1,f=32,m=1", rgba, False
    yield "explicit-size", rgba, "s=2,v=1,f=32,S=8", rgba, False
    yield "offset", b"pad" + rgba, "s=2,v=1,f=32,O=3", rgba, False
    yield "offset-size", b"pad" + rgba + b"tail", "s=2,v=1,f=32,O=3,S=8", rgba, False
    yield "rgb", b"\x01\x02\x03\x04\x05\x06", "s=2,v=1,f=24", b"\x01\x02\x03\xff\x04\x05\x06\xff", False
    compressed = zlib.compress(rgba)
    yield "compressed", compressed, f"s=2,v=1,f=32,o=z,S={len(compressed)}", rgba, False
    encoded = png(2, 1, 8, 6, b"\0" + rgba)
    yield "png", encoded, f"f=100,S={len(encoded)}", rgba, False
    yield "query", rgba, "a=q,s=2,v=1,f=32", None, False
    yield "quiet", rgba, "s=2,v=1,f=32,q=2", rgba, False
    yield "size-too-small", rgba, "s=2,v=1,f=32,S=4", None, True
    yield "offset-outside", rgba, "s=2,v=1,f=32,O=4294967295", None, True
    yield "size-outside", rgba, "s=2,v=1,f=32,S=4294967295", None, True
    yield "oversized-dimensions", rgba, "s=10001,v=1,f=32", None, True


def run(binary, name, data, options, expected, error, native=False):
    shm_name = f"/rustty-{os.getpid()}-{next(IDS)}".encode()
    fd = LIBC.shm_open(shm_name, os.O_CREAT | os.O_EXCL | os.O_RDWR, ctypes.c_uint(0o600))
    if fd < 0:
        raise OSError(ctypes.get_errno(), "shm_open")
    try:
        os.ftruncate(fd, len(data))
        with mmap.mmap(fd, len(data), flags=mmap.MAP_SHARED, prot=mmap.PROT_READ | mmap.PROT_WRITE) as mapped:
            mapped[:] = data
        os.close(fd)
        fd = None
        command = b"\x1b_Ga=t,i=1,t=s," + options.encode() + b";" + base64.b64encode(shm_name) + b"\x1b\\"
        request = {"id": name, "cols": 1, "rows": 1, "observe_graphics": True,
                   "operations": [{"op": "write", "data": command.hex()}]}
        if native:
            request["graphics_shared_memory"] = True
        result = subprocess.run([str(binary)], input=json.dumps(request) + "\n",
                                capture_output=True, text=True, timeout=30, check=True)
        response = json.loads(result.stdout)
        assert response["ok"], (name, binary, response)
        # Both engines own unlinking after opening, including rejected ranges.
        reopened = LIBC.shm_open(shm_name, os.O_RDONLY, 0)
        if reopened >= 0:
            os.close(reopened)
            raise AssertionError((name, binary, "shared memory was not unlinked"))
        assert ctypes.get_errno() == errno.ENOENT, (name, binary, reopened, ctypes.get_errno())
        images = response["observations"][-1]["graphics"]["primary"]
        if expected is None:
            assert images == [], (name, binary, images)
        else:
            assert len(images) == 1 and images[0]["pixels"] == expected.hex(), (name, binary, images)
        writes = b"".join(bytes.fromhex(event["data"]) for event in response["events"] if event["kind"] == "write")
        if error:
            assert b";EINVAL:" in writes, (name, binary, writes)
        elif "q=2" in options:
            assert writes == b"", (name, binary, writes)
        else:
            assert writes == b"\x1b_Gi=1;OK\x1b\\", (name, binary, writes)
        return images
    finally:
        if fd is not None:
            os.close(fd)
        LIBC.shm_unlink(shm_name)


def main():
    if len(sys.argv) != 3:
        raise SystemExit("usage: kitty_shared_memory.py GHOSTTY_ORACLE RUSTTY_ORACLE")
    binaries = [Path(path).resolve() for path in sys.argv[1:]]
    count = 0
    for case in cases():
        results = [run(binary, *case, native=index == 0) for index, binary in enumerate(binaries)]
        assert results[0] == results[1], (case[0], results)
        count += 1
    print(f"{count} shared-memory cases match; uploaded pixels, replies and unlink checked")


if __name__ == "__main__":
    main()
