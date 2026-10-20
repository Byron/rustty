"""Private raw-input witness; writes received bytes before publishing completion."""
import os
from pathlib import Path
import sys
import time

if os.name == "nt":
    import ctypes
    import msvcrt
    k = ctypes.WinDLL("kernel32", use_last_error=True)
    k.GetStdHandle.restype = ctypes.c_void_p
    k.SetConsoleMode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
    assert k.SetConsoleCP(65001) and k.SetConsoleOutputCP(65001)
    assert k.SetConsoleMode(k.GetStdHandle(-10), 0x200)
    assert k.SetConsoleMode(k.GetStdHandle(-11), 0xD)
    msvcrt.setmode(0, os.O_BINARY)
    msvcrt.setmode(1, os.O_BINARY)
else:
    import tty
    tty.setraw(0)

result = Path(sys.argv[1])
os.write(1, (b"\x1b[?2004h" if sys.argv[2] == "1" else b"\x1b[?2004l") + b"\x1b[c")
reply = b""
while not reply.endswith(b"c"):
    reply += os.read(0, 1)
result.with_suffix(".ready").touch()
data = b""
marker = b"tmuxx-buffer-end-marker"
while not data.endswith(marker):
    data += os.read(0, 4096)
temporary = result.with_name(result.name + ".tmp")
temporary.write_bytes(data[:-len(marker)])
temporary.replace(result)
time.sleep(60)
