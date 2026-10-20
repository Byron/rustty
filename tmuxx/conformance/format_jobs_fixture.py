"""Bounded, byte-exact child programs shared by native and C job tests."""
import os
from pathlib import Path
import sys
import time


def wait_file(name):
    deadline = time.monotonic() + 30
    while not Path(name).exists():
        if time.monotonic() >= deadline:
            raise TimeoutError(name)
        time.sleep(0.005)


mode = sys.argv[1]
if mode == "idle":
    time.sleep(120)
elif mode == "emit":
    os.write(1, sys.argv[2].encode("utf-8"))
elif mode == "expand":
    os.write(1, bytes([35]) + b"{@value}")
elif mode == "stream":
    if not Path("done").exists():
        os.write(1, b"first\n")
        wait_file("next")
        os.write(1, b"second\n")
        wait_file("done")
    os.write(1, b"tail")
elif mode == "shared":
    with Path(os.environ["SIDE"] + ".count").open("ab") as stream:
        stream.write(b"x")
    wait_file("release")
    os.write(1, b"finished")
elif mode == "nested":
    os.write(1, bytes([35]) + b"(python format_jobs_fixture.py forbidden)")
elif mode == "forbidden":
    Path("forbidden").write_bytes(b"unexpected nested format job")
elif mode == "failure":
    os.write(1, b"out\n")
    os.write(2, b"error")
    raise SystemExit(7)
elif mode == "lifetime":
    destination = Path(os.environ["SIDE"] + ".pid")
    temporary = destination.with_name(destination.name + ".tmp")
    temporary.write_text(str(os.getpid()), encoding="ascii")
    temporary.replace(destination)
    time.sleep(120)
else:
    raise ValueError(mode)
