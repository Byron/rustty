"""Explicit private Windows/POSIX child programs for CLI interoperability."""
import os
import sys
import time
from pathlib import Path


def terminate(code):
    import ctypes
    from ctypes import wintypes
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.GetCurrentProcess.restype = wintypes.HANDLE
    kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
    kernel.TerminateProcess.restype = wintypes.BOOL
    if not kernel.TerminateProcess(kernel.GetCurrentProcess(), code):
        raise ctypes.WinError(ctypes.get_last_error())


def raw():
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes
        import msvcrt
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.GetStdHandle.argtypes = [wintypes.DWORD]
        kernel.GetStdHandle.restype = wintypes.HANDLE
        kernel.GetConsoleMode.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
        kernel.SetConsoleMode.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        handle = kernel.GetStdHandle(-10 & 0xffffffff)
        mode = wintypes.DWORD()
        if not kernel.GetConsoleMode(handle, ctypes.byref(mode)) or not kernel.SetConsoleMode(handle, (mode.value & ~7) | 0x200):
            raise ctypes.WinError(ctypes.get_last_error())
        msvcrt.setmode(0, os.O_BINARY)
        msvcrt.setmode(1, os.O_BINARY)
    else:
        import tty
        tty.setraw(0)


def available(deadline):
    if os.name == "nt":
        import msvcrt
        while time.monotonic() < deadline:
            if msvcrt.kbhit():
                return True
            time.sleep(.001)
        return False
    import select
    return bool(select.select([0], [], [], max(0, deadline - time.monotonic()))[0])

mode = sys.argv[1]
if mode == "idle":
    time.sleep(120)
elif mode == "echo":
    raw()
    os.write(1, b"READY")
    while data := os.read(0, 1):
        os.write(1, data)
elif mode == "retained":
    os.write(1, b"retained")
    os._exit(7)
elif mode == "mark":
    os.write(1, sys.argv[2].encode())
    time.sleep(120)
elif mode == "env-mark":
    os.write(1, os.environ[sys.argv[2]].encode())
    time.sleep(120)
elif mode == "exit-now":
    os._exit(int(sys.argv[2]))
elif mode == "terminate-now":
    if os.name == 'nt':
        terminate(int(sys.argv[2]))
    else:
        import signal
        os.kill(os.getpid(), signal.SIGTERM)
elif mode == "write":
    if os.name == 'nt':
        import msvcrt
        msvcrt.setmode(1, os.O_BINARY)
    os.write(1, sys.argv[2].encode())
elif mode == "duplex":
    if os.name == 'nt':
        import msvcrt
        msvcrt.setmode(0, os.O_BINARY)
        msvcrt.setmode(1, os.O_BINARY)
    os.read(0, 1)
    os.write(1, b'R')
    os.read(0, 1)
elif mode == "burst":
    raw()
    os.write(1, b'READY')
    os.read(0, 1)
    data = b'Z' * 262144
    while data:
        # A Windows console write can complete a valid short prefix (32767
        # bytes here). The fixture must actually emit the entire stimulus.
        count = os.write(1, data)
        if count <= 0:
            raise OSError('zero-length console write')
        data = data[count:]
elif mode == "colors":
    raw()
    os.write(1, b'\x1b]10;#112233\x07\x1b]11;#040506\x07READY')
    while os.read(0, 1):
        os.write(1, b'\x1b]10;?\x07\x1b]11;?\x07\x1b[?996n')
        data = bytearray()
        deadline = time.monotonic() + .1
        while available(deadline):
            data.extend(os.read(0, 1))
        with open(sys.argv[2], 'ab') as output:
            output.write(data.hex().encode() + b'\n')
elif mode in ("sink", "slow-sink"):
    if os.name == "nt":
        import msvcrt
        msvcrt.setmode(0, os.O_BINARY)
    with open(sys.argv[2] if len(sys.argv) > 2 else os.devnull, 'wb') as output:
        while data := os.read(0, 4096):
            output.write(data)
            output.flush()
            if mode == 'slow-sink':
                time.sleep(.001)
elif mode == "exit":
    raw()
    os.write(1, b"READY")
    os.read(0, 1)
    if sys.argv[2] == "status":
        os._exit(137)
    elif os.name == "nt":
        terminate(137)
    else:
        import signal
        os.kill(os.getpid(), signal.SIGKILL)
else:
    raise ValueError(mode)
