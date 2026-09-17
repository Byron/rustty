#!/usr/bin/env python3
"""Offline fixture generators and the preserved Python reference runner.

Normal suite execution uses parity-runner.rs (or the parity.py launcher).
Keep this reference for regeneration and runner-equivalence checks only.
"""
import argparse
import hashlib
import json
from pathlib import Path
import queue
import random
import re
import subprocess
import sys
import threading
import snapshots
import protocols
import kitty_clipboard
import paste_requests
import graphics_requests
import placement_requests
import placeholder_requests
import relative_placement_requests
import semantic_prompts
import prompt_redraw
import charsets
import grid_requests
import search_pages
import viewport_search
import terminal_search
import page_layout_requests
import page_lifecycle_requests
import host_queries
import mode_defaults
import color_protocols
import osc_strings
import osc_requests
import glyph_requests
import reset_stream
import dnd_requests
import style_lifecycle_requests
import grapheme_lifecycle_requests
import hyperlink_lifecycle_requests
import selection_requests
import selection_adjust_requests
import selection_format_requests
import terminal_format_requests
import selection_gesture_requests

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
ARTIFACTS = ROOT / "target/parity"
MAX_REQUEST = 32 * 1024 * 1024
MAX_RESPONSE = 128 * 1024 * 1024


class Peer:
    def __init__(self, name, command, timeout):
        self.name = name
        self.timeout = timeout
        self.queue = queue.Queue()
        self.log = (ARTIFACTS / f"{name}.log").open("wb")
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=self.log, cwd=ROOT)
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        try:
            while line := self.process.stdout.readline(MAX_RESPONSE + 1):
                if len(line) > MAX_RESPONSE or not line.endswith(b"\n"):
                    raise RuntimeError("response exceeded limit or was truncated")
                self.queue.put(json.loads(line))
            raise RuntimeError(f"oracle exited; inspect {self.name}.log")
        except Exception as error:
            self.queue.put(error)

    def request(self, request):
        data = json.dumps(request, separators=(",", ":")).encode()
        if len(data) > MAX_REQUEST:
            raise RuntimeError(f"{self.name}: request exceeded {MAX_REQUEST} bytes")
        self.process.stdin.write(data + b"\n")
        self.process.stdin.flush()
        try:
            response = self.queue.get(timeout=self.timeout)
        except queue.Empty as error:
            self.close()
            raise RuntimeError(f"{self.name}: timeout after {self.timeout}s") from error
        if isinstance(response, Exception):
            self.close()
            raise RuntimeError(f"{self.name}: {response}") from response
        return response

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.log.close()


def difference(left, right, path="response"):
    if type(left) is not type(right):
        return f"{path}: Zig {left!r}, Rust {right!r}"
    if isinstance(left, dict):
        if left.keys() != right.keys():
            return f"{path}: different fields: {left.keys() ^ right.keys()}"
        for key in left:
            if diff := difference(left[key], right[key], f"{path}.{key}"):
                return diff
    elif isinstance(left, list):
        if len(left) != len(right):
            return f"{path}: Zig length {len(left)}, Rust length {len(right)}"
        for index, (a, b) in enumerate(zip(left, right)):
            if diff := difference(a, b, f"{path}[{index}]"):
                return diff
    elif left != right:
        return f"{path}: Zig {left!r}, Rust {right!r}"
    return None


def variants(request, exhaustive=False):
    """Delivery boundaries change; operation/observation ordering never changes."""
    yield request
    if request.get("expected_error"):
        return
    if request.get("kind", "terminal") not in ("terminal", "input", "parser", "snapshot"):
        return
    for name, chunks in (("scalar", [1]), ("chunks", [2, 7, 1, 13, 4])):
        variant = dict(request, id=f"{request['id']}/{name}", scalar=name == "scalar")
        if name == "scalar":
            # Both adapters feed one byte at a time when scalar is true.
            # Expanding each byte into a JSON operation changes no delivery
            # behavior and can exceed the transport's request-size limit.
            yield variant
            continue
        for field in ("operations", "after"):
            if field not in request:
                continue
            operations = []
            for operation in request[field]:
                if operation["op"] != "write":
                    operations.append(operation)
                    continue
                data = bytes.fromhex(operation["data"])
                pos = 0
                index = 0
                # Large capture-limit cases must fit the bounded JSON transport.
                # Scalar delivery still exercises every individual input byte.
                sizes = [2, 4096, 1, 16384, 7] if len(data) > 4096 else chunks
                while pos < len(data):
                    size = sizes[index % len(sizes)]
                    operations.append({"op": "write", "data": data[pos:pos + size].hex()})
                    pos += size
                    index += 1
            variant[field] = operations
        yield variant
    if exhaustive:
        for field in ("operations", "after"):
            for index, operation in enumerate(request.get(field, [])):
                if operation["op"] != "write":
                    continue
                data = bytes.fromhex(operation["data"])
                if len(data) > 64:
                    continue
                for split in range(1, len(data)):
                    operations = request[field][:index] + [
                        {"op": "write", "data": data[:split].hex()},
                        {"op": "write", "data": data[split:].hex()},
                    ] + request[field][index + 1:]
                    yield dict(request, id=f"{request['id']}/split-{field}-{index}-{split}", **{field: operations})


def corpus_requests():
    # stream.zig fuzz fixtures reserve byte zero for scalar/vector delivery.
    # The terminal bytes begin at offset one, in both reference paths.
    for directory in ("stream-initial", "stream-cmin"):
        paths = sorted((ROOT / "test/fuzz-libghostty/corpus" / directory).glob("*"))
        if not paths:
            raise RuntimeError(f"required corpus is missing: {directory}")
        for path in paths:
            if path.is_file():
                yield {"id": f"corpus/{directory}/{path.name}", "cols": 80, "rows": 24,
                       "operations": [{"op": "write", "data": path.read_bytes()[1:].hex()}]}


def generated_requests(seed, count):
    rng = random.Random(seed)
    writes = ["hello", "é界👩🏽‍💻", "\r", "\n", "\t", "\b", "\x1b[2J", "\x1b[K",
              "\x1b[1;1H", "\x1b[31m", "\x1b[0m", "\x1b[?1049h", "\x1b[?1049l",
              "\x1b[2;3r", "\x1b[r", "\x1b[4h", "\x1b[4l", "\x1b[2P", "\x1b[2@"]
    for case in range(count):
        operations = []
        for _ in range(30):
            if rng.randrange(8) == 0:
                operations.append({"op": "resize", "cols": rng.randint(3, 30), "rows": rng.randint(2, 8)})
            else:
                operations.append({"op": "write", "data": rng.choice(writes).encode().hex()})
            if rng.randrange(4) == 0:
                operations.append({"op": "observe"})
        yield {"id": f"generated/{seed}/{case}", "cols": 12, "rows": 4, "operations": operations}


def unicode_requests():
    """Compare every valid Unicode scalar without deriving cases from Rust's table."""
    for start in range(0, 0x110000, 4096):
        codepoints = [cp for cp in range(start, min(start + 4096, 0x110000))
                      if not 0xD800 <= cp <= 0xDFFF]
        yield ({"id": f"unicode/scalars/{start:06x}", "kind": "unicode",
                "codepoints": codepoints}, ["unicode.width"])
    for cp in (0xD800, 0xDFFF, 0x110000, 0xFFFFFFFF):
        yield ({"id": f"unicode/scalars/reject-{cp:06x}", "kind": "unicode",
                "codepoints": [cp], "expected_error": "InvalidCodepoint"}, ["unicode.width"])


def input_requests():
    def request(name, setup, events, covers):
        return ({"id": "input/" + name, "kind": "input", "cols": 80, "rows": 24,
                 "operations": [{"op": "write", "data": setup.encode().hex()}]
                 + [{"op": "input", "input": event} for event in events]}, covers)

    keys = ["space", "tab", "enter", "escape", "backspace", "backquote", "minus", "equal",
            "bracket_left", "bracket_right", "backslash", "semicolon", "quote", "comma", "period", "slash",
            "arrow_up", "arrow_down", "arrow_left", "arrow_right", "home", "end", "page_up", "page_down",
            "insert", "delete", "numpad_0", "numpad_9", "numpad_enter", "numpad_add", "numpad_decimal",
            "shift_left", "control_left", "alt_left", "meta_left"] + [f"f{i}" for i in range(1, 26)]
    keys += ["key_" + chr(cp) for cp in range(ord('a'), ord('z') + 1)] + [f"digit_{n}" for n in range(10)]
    # Include every named key with native legacy or Kitty encoding, including
    # distinct right-hand modifiers and keypad navigation identities.
    native_keys = set(re.findall(r"\.\{\s*\.(\w+),", (ROOT / "src/input/kitty.zig").read_text()))
    native_keys.update(re.findall(r"result\.set\(\.(\w+),", (ROOT / "src/input/function_keys.zig").read_text()))
    keys += sorted(native_keys.difference(keys))
    keys.append("unidentified")
    punctuation = dict(zip(("space", "backquote", "minus", "equal", "bracket_left", "bracket_right",
                            "backslash", "semicolon", "quote", "comma", "period", "slash"), " `-=[]\\;',./"))
    configurations = [("legacy", ""), ("cursor", "\x1b[?1h"), ("keypad", "\x1b[?66h"),
                      ("backarrow", "\x1b[?67h"), ("alt-prefix-off", "\x1b[?1036l"),
                      ("modify-other", "\x1b[>4;2m"), ("keypad-application", "\x1b[?1035l\x1b[?66h")]
    configurations += [(f"kitty-{flags}", f"\x1b[>{flags}u") for flags in (1, 2, 3, 4, 8, 16, 31)]
    for mode, setup in configurations:
        for key in keys:
            events = []
            for modifiers in (*range(16), 16, 32, 48):
                for action in ("press", "repeat", "release"):
                    text = key[-1] if key.startswith(("key_", "digit_")) else punctuation.get(key, "")
                    base = text
                    if modifiers & 1:
                        text = text.upper()
                    events.append({"kind": "key", "key": key, "modifiers": modifiers, "action": action,
                                   "data": text.encode().hex(), "unshifted": ord(base) if base else 0})
            yield request(f"key/{mode}/{key}", setup, events, ["input.key"])
    for flags in (0, 1, 3, 8, 24, 31):
        for name, event in (
            ("consumed-shift", {"key": "key_a", "data": "41", "modifiers": 1, "consumed_modifiers": 1, "unshifted": 97}),
            ("consumed-alt", {"key": "key_e", "data": "c3a9", "modifiers": 4, "consumed_modifiers": 4, "unshifted": 101}),
            ("empty-text", {"key": "key_a", "modifiers": 4, "unshifted": 97}),
            ("shifted-layout", {"key": "key_a", "data": "d090", "modifiers": 1, "unshifted": 0x430}),
            ("ctrl-layout", {"key": "key_c", "data": "d181", "modifiers": 2, "unshifted": 0x441}),
            ("ime-enter", {"key": "enter", "data": "e697a5e69cac"}),
            ("ime-backspace", {"key": "backspace", "data": "e697a5e69cac"}),
            ("composing", {"key": "key_a", "data": "61", "composing": True, "unshifted": 97}),
            ("unidentified-layout", {"key": "unidentified", "data": "d090", "modifiers": 1, "unshifted": 0x430}),
            ("unidentified-text", {"key": "unidentified", "data": "e697a5e69cac"}),
            ("unidentified-control", {"key": "unidentified", "data": "63", "modifiers": 2, "unshifted": 99}),
            ("keypad-equal-text", {"key": "numpad_equal", "data": "3d", "unshifted": 61}),
            ("keypad-equal-alternate", {"key": "numpad_equal", "data": "2b", "modifiers": 1, "unshifted": 61}),
            ("help-text", {"key": "help", "data": "61", "unshifted": 97}),
            ("menu-text", {"key": "context_menu", "data": "61", "unshifted": 97}),
            ("composing-right-shift", {"key": "shift_right", "modifiers": 1, "composing": True}),
            ("composing-lock", {"key": "caps_lock", "modifiers": 16, "composing": True}),
        ):
            yield request(f"key/text-{flags}/{name}", f"\x1b[>{flags}u" if flags else "",
                          [dict(event, kind="key")], ["input.key"])
    for flags in (0, 1, 3, 8, 24, 31):
        for option in ("false", "true", "left", "right"):
            for sides in range(16):
                events = [
                    {"kind": "key", "key": "key_e", "data": text.encode().hex(),
                     "unshifted": 101, "modifiers": modifiers | sides << 6,
                     "consumed_modifiers": consumed, "action": action,
                     "macos_option_as_alt": option}
                    for modifiers in (4, 5, 6, 12, 20, 36, 0xFC04)
                    for consumed in (0, 4)
                    for text in ("", "e", "é", "€", "èe")
                    for action in ("press", "repeat", "release")
                ]
                yield request(f"option/{flags}/{option}/{sides}", f"\x1b[>{flags}u" if flags else "",
                              events, ["input.key"])
    for mode in (9, 1000, 1002, 1003):
        for encoding in (0, 1005, 1006, 1015, 1016):
            setup = f"\x1b[?{mode}h" + (f"\x1b[?{encoding}h" if encoding else "")
            for button in (None, "left", "middle", "right", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven"):
                events = [{"kind": "mouse", "button": button, "action": action, "modifiers": modifiers, "x": x, "y": y}
                          for action in ("press", "release", "motion") for modifiers in (0, 1, 2, 4, 7)
                          for x, y in ((0, 0), (9, 17), (639, 383), (640, 384), (2000, 4000),
                                       (-1, -1), (-0.25, 0), (1.5, 2.5), (640.25, 384.25))]
                yield request(f"mouse/{mode}/{encoding}/{button}", setup, events, ["input.mouse"])

    # Retain the caller's last reported cell across input operations, including
    # rejected buttons and output formats which cannot encode a coordinate.
    geometries = [
        ("plain", [640, 384], [8, 16], [0, 0, 0, 0]),
        ("padding", [103, 101], [5, 10], [7, 13, 11, 17]),
        ("tiny", [4, 2], [8, 16], [10, 20, 30, 40]),
        ("zero", [0, 0], [8, 16], [0, 0, 0, 0]),
        ("max-grid", [4294967295, 4294967295], [1, 1], [0, 0, 0, 0]),
        ("max-padding", [4294967295, 4294967295], [8, 16], [4294967295] * 4),
    ]
    for mode in (0, 9, 1000, 1002, 1003):
        for encoding in (0, 1005, 1006, 1015, 1016):
            setup = (f"\x1b[?{mode}h" if mode else "") + (f"\x1b[?{encoding}h" if encoding else "")
            for name, screen, cell, padding in geometries:
                positions = [(0, 0), (1, 1), (padding[0] - 0.5, padding[1] - 0.5),
                             (padding[0] + 0.5, padding[1] + 0.5),
                             (padding[0] + cell[0] - 0.25, padding[1]),
                             (padding[0] + cell[0], padding[1] + cell[1]),
                             tuple(screen), (screen[0] + 0.25, screen[1] + 0.25), (-0.25, -0.25)]
                events = [
                    {"kind": "mouse", "action": action, "button": button, "x": x, "y": y,
                     "modifiers": modifiers, "screen_size": screen, "cell_size": cell,
                     "padding": padding, "track_last_cell": True, "any_button_pressed": held}
                    for x, y in positions
                    for action, button, held, modifiers in (
                        ("motion", None, False, 0), ("motion", None, False, 7),
                        ("motion", "left", False, 0), ("motion", "left", True, 0),
                        ("press", "ten", True, 0), ("motion", None, True, 0),
                        ("press", "left", True, 0), ("press", "left", True, 0),
                        ("release", "right", False, 0), ("release", None, False, 0))
                ]
                yield request(f"mouse/retained/{mode}/{encoding}/{name}", setup, events, ["input.mouse"])

            # Renderer geometry is independent of the terminal's cell count.
            # UTF-8 mouse coordinates can require three or four bytes.
            events = [{"kind": "mouse", "action": action, "button": "left", "x": x, "y": y,
                       "screen_size": [65535, 65535], "cell_size": [1, 1], "track_last_cell": True}
                      for x, y in ((94, 95), (222, 223), (2014, 2015), (2047, 4095),
                                   (55262, 57311), (55263, 1), (1, 57310),
                                   (65534, 65535), (-1, -1), (1e30, -1e30))
                      for action in ("press", "motion", "release")]
            yield request(f"mouse/limits/{mode}/{encoding}", setup, events, ["input.mouse"])

    def mouse(**options):
        return {"op": "input", "input": {"kind": "mouse", "action": "motion", "x": 9, "y": 17,
                                         "track_last_cell": True, **options}}

    transitions = [b"\x1b[?47h", b"\x1b[?1003l\x1b[?1003h", b"\x1b[?1016h", b"\x1bc",
                   b"\x1b[?1000h", b"\x1b[?1005h"]
    for transition in transitions:
        setup = {"op": "write", "data": b"\x1b[?1003h\x1b[?1006h".hex()}
        yield ({"id": "input/mouse/transitions/" + transition.hex(), "kind": "input", "cols": 80, "rows": 24,
                "operations": [setup, mouse(), {"op": "write", "data": transition.hex()}, mouse(),
                               setup, mouse(), mouse(track_last_cell=False), mouse(),
                               mouse(reset_mouse=True), mouse(),
                               {"op": "resize", "cols": 1, "rows": 1}, mouse(),
                               {"op": "terminal_reset"}, setup, mouse()]}, ["input.mouse"])

    yield request("mouse/pixel-limits", "\x1b[?1003h\x1b[?1016h", [
        {"kind": "mouse", "action": "release", "button": "left", "x": x, "y": y,
         "screen_size": [2147483648, 2147483648], "cell_size": [65536, 65536], "track_last_cell": True}
        for x, y in ((2147483520, -2147483648), (2147483648, -2147483904),
                     (1e30, -1e30), (-0.5, 0.5), (-1.5, 1.5))
    ], ["input.mouse"])
    for enabled in (False, True):
        yield request(f"focus/{enabled}", "\x1b[?1004h" if enabled else "",
                      [{"kind": "focus", "focused": focused} for focused in (True, False, True)], ["input.focus-paste"])
        yield request(f"paste/{enabled}", "\x1b[?2004h" if enabled else "",
                      [{"kind": "paste", "data": text.encode().hex()} for text in
                       ("", "hello", "é界👩🏽‍💻", "a\nb\r\nc", "\x00\x08\x05\x04\x1b\x7f\x03\x1c\x15\x1a\x11\x13\x17\x16\x12\x0f")],
                      ["input.focus-paste"])
        binary = [bytes([byte]) for byte in range(256)]
        binary += [bytes(range(256)), b"\xc0\xaf\xe0\xa0\xed\xa0\x80\xf4\x90\x80\x80",
                   b"before\x1b[201~after", b"\x9b201~", b"\xff\n\xfe", b"\r\n\r", b""]
        binary += [b"\xff" * offset + b"\x1b[201~" + b"\xfe" * (6 - offset) for offset in range(7)]
        for name, data in enumerate(binary):
            yield request(f"paste-bytes/{enabled}/{name}", "\x1b[?2004h" if enabled else "",
                          [{"kind": "paste", "data": data.hex()},
                           {"kind": "paste_safe", "data": data.hex()},
                           {"kind": "paste_safe", "data": data.hex(), "conservative": True}],
                          ["input.focus-paste"])


def parser_requests():
    cases = {
        "utf8": "aé界👩🏽‍💻\x7f".encode(),
        "malformed": b"\xc0\xaf\xe0\xa0\x1b[0m\xed\xa0\x80\xf4\x90\x80\x80\xf0\x9f",
        "controls": bytes(range(32)) + b"\xc2\x9b\x1b[\x07\x18m",
        "csi": b"\x1b[38:2::1:2:3m\x1b[1:2H\x1b[6553599;0m\x1b[?25$p",
        "csi-overflow": b"\x1b[" + b"1;" * 30 + b"m\x1b[1234567890m",
        "escape": b"\x1b(0\x1b#8\x1b %A\x1b    !x",
        "osc": b"\x1b]2;title\x07\x1b]not-a-command\x1b\\\x1b]52;c;YWJj\x9c",
        "osc-cancel": b"\x1b]2;unfinished\x18tail\x1b]2;other\x1a",
        "dcs": b"\x1bP1;2$qpayload\x1b\\\x1bP>|version\x9c",
        "apc": b"\x1b_Ga=T;dGVzdA==\x1b\\\x1bXignored\x1b\\",
    }
    for name, data in cases.items():
        yield ({"id": "parser/" + name, "kind": "parser", "operations": [
            {"op": "write", "data": data.hex()}, {"op": "observe"},
            {"op": "write", "data": b"\x1b[0m".hex()}, {"op": "reset"},
        ]}, ["parser.raw-events"])
    for directory in ("parser-initial", "parser-cmin"):
        paths = sorted((ROOT / "test/fuzz-libghostty/corpus" / directory).glob("*"))
        if not paths:
            raise RuntimeError(f"required corpus is missing: {directory}")
        for path in paths:
            if path.is_file():
                yield ({"id": f"parser/corpus/{directory}/{path.name}", "kind": "parser",
                        "operations": [{"op": "write", "data": path.read_bytes().hex()}]}, ["parser.raw-events"])


def save_failure(request, left, right, reason):
    digest = hashlib.sha256(json.dumps(request, sort_keys=True).encode()).hexdigest()[:16]
    directory = ARTIFACTS / "failures" / digest
    directory.mkdir(parents=True, exist_ok=True)
    for name, value in (("request", request), ("zig", left), ("rust", right)):
        (directory / f"{name}.json").write_text(json.dumps(value, indent=2) + "\n")
    (directory / "difference.txt").write_text(reason + "\n")
    return directory.relative_to(ROOT) if directory.is_relative_to(ROOT) else directory


def compare(peers, request):
    if request.get("kind") == "snapshot":
        return snapshots.compare(peers, request, difference)
    expected_error = request.get("expected_error")
    sent = {k: v for k, v in request.items() if k != "expected_error"}
    left, right = [peer.request(sent) for peer in peers]
    # Capabilities are checked independently of observed behavior.
    comparable = [{k: v for k, v in response.items() if k != "capabilities"} for response in (left, right)]
    reason = difference(*comparable)
    if expected_error:
        if any(response["ok"] or response["err"] != expected_error for response in (left, right)):
            reason = reason or f"expected decoder rejection: {expected_error}"
    elif not left["ok"] or not right["ok"]:
        reason = reason or f"request failed: {left.get('err')} / {right.get('err')}"
    return left, right, reason


def coverage_gaps(manifest, capabilities, covered):
    missing = []
    for entry in manifest["requirements"]:
        if entry["status"] != "complete":
            missing.append(f"{entry['id']}: {entry['status']} — {entry['remaining']}")
        elif not all(entry["id"] in peer for peer in capabilities):
            missing.append(f"{entry['id']}: an oracle lacks this capability")
        elif entry["id"] not in covered:
            missing.append(f"{entry['id']}: no passing coverage case in this run")
    return missing


def minimize(peers, request):
    """Delta-debug operations, then bytes, retaining a successful state mismatch."""
    request = dict(request, id=request["id"] + "/minimized")
    attempts = 0
    original = compare(peers, request)[2]

    def signature(reason):
        return re.sub(r"\[\d+\]", "[]", reason.split(":", 1)[0]) if reason else None

    def still_fails(candidate):
        nonlocal attempts
        if attempts >= 2000:
            return False
        attempts += 1
        left, right, reason = compare(peers, candidate)
        return left["ok"] and right["ok"] and signature(reason) == signature(original)

    def reduce_sequence(sequence, candidate):
        partitions = 2
        while sequence and attempts < 2000:
            size = max(1, (len(sequence) + partitions - 1) // partitions)
            for start in range(0, len(sequence), size):
                reduced = sequence[:start] + sequence[start + size:]
                if still_fails(candidate(reduced)):
                    sequence = reduced
                    partitions = max(2, partitions - 1)
                    break
            else:
                if partitions >= len(sequence):
                    break
                partitions = min(len(sequence), partitions * 2)
                continue
        return sequence

    request["operations"] = reduce_sequence(request.get("operations", []),
                                             lambda ops: dict(request, operations=ops))
    for index, operation in enumerate(request["operations"]):
        if operation["op"] != "write":
            continue

        def replace(data):
            operations = request["operations"].copy()
            operations[index] = {"op": "write", "data": data.hex()}
            return dict(request, operations=operations)

        data = reduce_sequence(bytes.fromhex(operation["data"]), replace)
        request = replace(data)
    left, right, reason = compare(peers, request)
    return request, left, right, reason, attempts


def collect_requests(args, peers):
    fixtures = json.loads(args.fixtures.read_text())
    requests = []
    if args.replay:
        requests.append((json.loads(args.replay.read_text()), []))
    else:
        for fixture in fixtures:
            if not args.case or args.case in fixture["request"]["id"]:
                requests.append((fixture["request"], fixture["covers"]))
        if args.input or args.thorough:
            requests.extend((request, covers) for request, covers in input_requests()
                            if not args.case or args.case in request["id"])
        if args.parser or args.thorough:
            requests.extend((request, covers) for request, covers in parser_requests()
                            if not args.case or args.case in request["id"])
        if args.grid or args.thorough:
            requests.extend((request, covers) for request, covers in terminal_search.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in viewport_search.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in grid_requests.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in selection_requests.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in selection_requests.snapshot_requests(peers[0])
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in selection_gesture_requests.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in selection_adjust_requests.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in selection_adjust_requests.snapshot_requests(peers[0])
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in selection_format_requests.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in selection_format_requests.snapshot_requests(peers[0])
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in terminal_format_requests.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in terminal_format_requests.snapshot_requests(peers[0])
                            if not args.case or args.case in request["id"])
        if args.page_layout or args.thorough:
            requests.extend((request, covers) for request, covers in page_layout_requests.requests()
                            if not args.case or args.case in request["id"])
        if args.pages or args.thorough:
            requests.extend((request, covers) for request, covers in page_lifecycle_requests.requests(peers[0])
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in style_lifecycle_requests.requests(peers[0])
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in grapheme_lifecycle_requests.requests(peers[0])
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in hyperlink_lifecycle_requests.requests(peers[0])
                            if not args.case or args.case in request["id"])
        if args.thorough:
            requests.extend((request, covers) for request, covers in search_pages.requests()
                            if not args.case or args.case in request["id"])
        if args.unicode or args.thorough:
            requests.extend((request, covers) for request, covers in unicode_requests()
                            if not args.case or args.case in request["id"])
        if args.snapshots or args.thorough:
            requests.extend((request, covers) for request, covers in snapshots.requests()
                            if not args.case or args.case in request["id"])
        if args.snapshot_wire or args.thorough:
            requests.extend((request, covers) for request, covers in snapshots.streaming_requests(ROOT)
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in snapshots.invalid_requests(ROOT)
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in snapshots.wire_requests(ROOT, peers[0])
                            if not args.case or args.case in request["id"])
        if args.protocols or args.thorough:
            requests.extend((request, covers) for request, covers in dnd_requests.requests()
                            if not args.case or args.case in request["id"])
            if not args.case or args.case in "protocol/dnd/snapshot/" or "protocol/dnd/snapshot" in args.case:
                requests.extend((request, covers) for request, covers in dnd_requests.snapshot_requests(peers)
                                if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in protocols.requests(ROOT)
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in protocols.effect_requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in protocols.clipboard_requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in kitty_clipboard.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in paste_requests.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in graphics_requests.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in placement_requests.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in relative_placement_requests.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in placeholder_requests.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in glyph_requests.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in reset_stream.requests()
                            if not args.case or args.case in request["id"])
            if not args.case or args.case in "protocol/glyph/snapshot/committed-registrations-omitted":
                requests.extend(glyph_requests.snapshot_wire_requests(peers[0]))
            requests.extend((request, covers) for request, covers in semantic_prompts.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in prompt_redraw.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in charsets.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in host_queries.requests()
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in mode_defaults.requests(ROOT)
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in color_protocols.requests(ROOT)
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in osc_strings.requests()
                            if not args.case or args.case in request["id"])
            requests.extend(osc_strings.allocating_requests(args.case))
        if args.corpus or args.thorough:
            requests.extend((request, ["terminal.write"]) for request in corpus_requests()
                            if not args.case or args.case in request["id"])
        if args.osc or args.thorough:
            requests.extend((request, covers) for request, covers in osc_requests.corpus_requests(ROOT)
                            if not args.case or args.case in request["id"])
            requests.extend((request, covers) for request, covers in osc_requests.requests()
                            if not args.case or args.case in request["id"])
        requests.extend((request, []) for request in generated_requests(args.seed, args.generated or (100 if args.thorough else 0))
                        if not args.case or args.case in request["id"])
    return requests


def main():
    global ARTIFACTS
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--thorough", action="store_true", help="run extended cases and require complete coverage")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--case", help="only fixture IDs containing this text")
    parser.add_argument("--replay", type=Path, help="replay a saved request")
    parser.add_argument("--minimize", action="store_true", help="reduce a replayed state mismatch")
    parser.add_argument("--timeout", type=float, default=15)
    parser.add_argument("--seed", type=int, default=0)
    parser.add_argument("--generated", type=int, default=0)
    parser.add_argument("--corpus", action="store_true", help="compare inherited terminal-stream corpus cases")
    parser.add_argument("--input", action="store_true", help="compare keyboard, mouse, focus and paste encoding")
    parser.add_argument("--parser", action="store_true", help="compare raw parser events and inherited parser corpus")
    parser.add_argument("--osc", action="store_true", help="compare direct OSC corpus state and effects")
    parser.add_argument("--unicode", action="store_true", help="compare widths of all 1,112,064 Unicode scalars")
    parser.add_argument("--snapshots", action="store_true", help="cross-decode both snapshot encodings and resume terminal input")
    parser.add_argument("--snapshot-wire", action="store_true", help="compare snapshot fixtures, streaming, malformed input and mixed PAGE widths")
    parser.add_argument("--protocols", action="store_true", help="compare terminal protocol queries and host effects")
    parser.add_argument("--grid", action="store_true", help="compare selection, literal search and tracked grid references")
    parser.add_argument("--page-layout", action="store_true", help="compare native page/resource layout arithmetic without backing allocations")
    parser.add_argument("--pages", action="store_true", help="compare native page lifetimes, boundaries and allocation charges")
    parser.add_argument("--max-failures", type=int, default=20)
    parser.add_argument("--artifacts", type=Path, default=ARTIFACTS, help="isolated output directory for concurrent suites")
    parser.add_argument("--zig-bin", type=Path, default=ROOT / "zig-out/bin/vt-oracle")
    parser.add_argument("--rust-bin", type=Path, default=ROOT / "target/debug/examples/parity")
    parser.add_argument("--fixtures", type=Path, default=HERE / "smoke.json")
    args = parser.parse_args()
    if args.minimize and not args.replay:
        parser.error("--minimize requires --replay")
    ARTIFACTS = args.artifacts.resolve()
    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    if not args.no_build:
        zig_build = ["zig", "build", "vt-oracle", "-Demit-lib-vt=true", "-Demit-macos-app=false"]
        if args.pages or args.corpus or args.osc or args.thorough:
            zig_build.append("-Doptimize=ReleaseSafe")
        subprocess.run(zig_build, cwd=ROOT, check=True)
        subprocess.run(["cargo", "build", "--offline", "-p", "rustty-vt", "--example", "parity"], cwd=ROOT, check=True)
    peers = []
    failures = 0
    checked = 0
    aborted = False
    missing = []
    covered = set()
    try:
        for name, binary in (("zig", args.zig_bin.resolve()), ("rust", args.rust_bin.resolve())):
            peers.append(Peer(name, [str(binary)], args.timeout))
        capabilities = [set(peer.request({"kind": "capabilities"})["capabilities"]) for peer in peers]
        manifest = json.loads((HERE / "coverage.json").read_text())
        requests = collect_requests(args, peers)
        for base, covers in requests:
            success = True
            baseline = None
            for request in ([base] if args.replay else variants(base, exhaustive=args.thorough)):
                left = right = None
                try:
                    left, right, reason = compare(peers, request)
                    normalized = [{k: v for k, v in value.items() if k not in ("id", "capabilities", "encodings")}
                                  for value in (left, right)]
                    if baseline is None:
                        baseline = normalized
                    elif not reason:
                        for name, original, variant in zip(("zig", "rust"), baseline, normalized):
                            if diff := difference(original, variant, name + ".delivery"):
                                reason = diff
                                break
                except Exception as error:
                    reason = str(error)
                    aborted = True
                checked += 1
                if checked % 100 == 0:
                    print(f"Checked {checked} comparisons ({request['id']})", flush=True)
                if reason:
                    success = False
                    failures += 1
                    path = save_failure(request, left, right, reason)
                    print(f"FAIL {request['id']}: {reason}\n  {path}", flush=True)
                    if args.minimize and left and right and left["ok"] and right["ok"]:
                        reduced, lval, rval, diff, attempts = minimize(peers, request)
                        reduced_path = save_failure(reduced, lval, rval, diff)
                        print(f"  Minimized in {attempts} comparisons: {reduced_path}", flush=True)
                    if aborted or failures >= args.max_failures:
                        break
            if success:
                covered.update(covers)
            if aborted or failures >= args.max_failures:
                break
        if args.thorough:
            missing = coverage_gaps(manifest, capabilities, covered)
            for gap in missing:
                print(f"COVERAGE GAP {gap}")
    finally:
        for peer in peers:
            peer.close()
    summary = {"mode": "thorough" if args.thorough else "smoke", "checked": checked,
               "failures": failures, "coverage_gaps": missing, "covered": sorted(covered),
               "full_parity": args.thorough and not failures and not missing}
    (ARTIFACTS / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(f"{checked} comparisons, {failures} failures, {len(missing)} coverage gaps; "
          + ("full parity verified" if summary["full_parity"] else "full parity not established"))
    return 1 if failures or missing or not checked else 0


if __name__ == "__main__":
    sys.exit(main())
