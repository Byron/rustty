"""Private native/reference child programs for the shared lifecycle matrix."""
import os
from pathlib import Path
import sys
import time

mode = sys.argv[1]
if mode == "idle":
    time.sleep(120)
elif mode == "interactive":
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.GetStdHandle.argtypes = [wintypes.DWORD]
        kernel.GetStdHandle.restype = wintypes.HANDLE
        kernel.GetConsoleMode.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
        kernel.SetConsoleMode.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        handle = kernel.GetStdHandle(-10 & 0xffffffff)
        state = wintypes.DWORD()
        if not kernel.GetConsoleMode(handle, ctypes.byref(state)):
            raise ctypes.WinError(ctypes.get_last_error())
        if not kernel.SetConsoleMode(handle, state.value & ~7):
            raise ctypes.WinError(ctypes.get_last_error())
        import msvcrt
        msvcrt.setmode(0, os.O_BINARY)
    else:
        import termios
        state = termios.tcgetattr(0)
        state[3] &= ~termios.ECHO
        termios.tcsetattr(0, termios.TCSANOW, state)
    Path("interactive-" + os.environ["TMUX_PANE"] + ".ready").write_bytes(b"")
    if os.name == "nt":
        assert os.read(0, 1) in (b"\r", b"\n")
    else:
        input()
    os.write(1, b"READY")
    time.sleep(120)
elif mode == "job":
    Path("job.pid").write_text(str(os.getpid()), encoding="ascii")
    Path("ready").write_bytes(b"")
    deadline = time.monotonic() + 30
    while not Path("release").exists():
        if time.monotonic() >= deadline:
            raise TimeoutError("release")
        time.sleep(0.005)
else:
    raise ValueError(mode)
