"""Exercise live process cwd and the explicit Windows root-image policy."""
import os
from pathlib import Path
import subprocess
import sys
import time

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
    if not kernel.SetConsoleMode(handle, state.value & ~4):
        raise ctypes.WinError(ctypes.get_last_error())
    os.write(1, b"CONPTY\r\n")
else:
    import termios
    state = termios.tcgetattr(0)
    state[3] &= ~termios.ECHO
    termios.tcsetattr(0, termios.TCSANOW, state)
    os.write(1, os.ttyname(0).encode() + b"\n")

root = Path.cwd()
for leaf in ["first", "second"]:
    (root / leaf).mkdir()
os.chdir(root / "first")
os.write(1, b"READY")
input()
os.chdir(root / "second")
os.write(1, b"SECOND")
input()
if os.name == "nt":
    # ConPTY has no foreground POSIX process-group API. Preserve the root
    # process identity while a different foreground executable owns input.
    child = subprocess.Popen([os.environ["COMSPEC"], "/d", "/c", "set /p TMUXX_NAMES_WAIT="], close_fds=True)
    destination = root / "foreground.pid"
    temporary = destination.with_name(destination.name + ".tmp")
    temporary.write_text(str(child.pid), encoding="ascii")
    temporary.replace(destination)
    child.wait(timeout=30)
else:
    os.execv("/bin/sleep", ["/bin/sleep", "30"])
