"""Local implementation and optional upstream reference source locations."""
import os
from pathlib import Path

TMUXX = Path(__file__).resolve().parents[1]
ROOT = TMUXX.parent


def c_source():
    selected = os.environ.get("TMUXX_C_SOURCE")
    if not selected:
        raise ValueError("set TMUXX_C_SOURCE to the C reference checkout for upstream comparisons")
    root = Path(selected).resolve()
    if not (root / "tmux.h").is_file():
        raise ValueError(f"TMUXX_C_SOURCE is not a tmux C checkout: {root}")
    return root
