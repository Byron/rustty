# Rustty terminal compatibility checks

This directory compares the Rust implementation with the current Ghostty Zig
terminal in separate processes. Rustty's application and libraries never link
the Zig oracle. Both adapters accept one JSON request per line and return one
JSON response per line. Terminal input and outgoing bytes are hexadecimal;
cell text is an array of Unicode scalar values. Adjacent PTY write callbacks
are coalesced because callback batching is not part of the terminal protocol.
Colors, styles, both screens, scrollback, cell widths and cursor state are
compared directly; snapshots are not used as a substitute for observable state.

Run the initial suite from the repository root:

```sh
python3 test/rustty/parity.py
```

It builds both adapters once, then compares whole-buffer, scalar and varied
delivery. The scalar variant calls Zig's `stream.next` explicitly. Variants
must also agree with each implementation's whole-buffer result. This suite
currently exercises only the subset in `smoke.json`; passing it does **not**
establish full libghostty-vt compatibility.

When the shell sandbox requires builds to run separately:

```sh
zig build vt-oracle -Demit-lib-vt=true -Demit-macos-app=false
cargo build --offline -p rustty-vt --example parity
python3 test/rustty/parity.py --no-build
```

Use deterministic generated cases and saved failures to diagnose differences:

```sh
python3 test/rustty/parity.py --no-build --generated 100 --seed 0
python3 test/rustty/parity.py --no-build --input
python3 test/rustty/parity.py --no-build --parser
python3 test/rustty/parity.py --no-build --replay target/parity/failures/ID/request.json
python3 test/rustty/parity.py --no-build --replay target/parity/failures/ID/request.json --minimize
python3 -m unittest discover -s test/rustty -p 'test_*.py'
```

`--input` adds matrices for legacy/Kitty keyboard modes, modifiers and key
actions, consumed text modifiers, IME, mouse formats, focus and paste. Use
`--case input/key` to select keyboard cases. Input cases compare each encoded
result even when it is empty; a dropped key cannot disappear from the event
list. Mouse coordinates use 8-by-16-pixel cells. `--artifacts`, `--zig-bin` and
`--rust-bin` select isolated output and adapter paths for concurrent work.

`--parser` compares raw UTF-8/ANSI events, parser state and all inherited
`parser-initial` and `parser-cmin` fixtures. Unlike stream fixtures, parser
fixtures contain no delivery-selector byte. The Zig adapter uses the original
parser and UTF-8 decoder. For OSC it captures bytes at the parser's transition
boundary because Zig exposes validated commands while Rust exposes raw OSC
payloads. This comparison therefore does **not** validate OSC command parsing,
effects or command-specific limits; those need terminal/protocol cases.

Each failure saves its request, both full responses and the first difference
under `target/parity/failures/`. Minimization removes operations and bytes
while retaining a successful state comparison with the same mismatching field;
it may reduce valid UTF-8 into malformed input. Original failures are retained.
An oracle crash, invalid response, unsupported operation or timeout fails the
run. Requests are limited to 16 MiB and responses to 128 MiB. Graphics file,
temporary-file and shared-memory transports are disabled in the Zig oracle.

`--thorough` additionally exercises input and parser cases, all split points
for short writes, the inherited stream corpus and generated operations. The
stream corpus's first byte is its original delivery selector, so it is removed
from the terminal input.
Missing corpus directories are errors. A thorough run also requires every
entry in `coverage.json` to be complete, exposed by both adapters and covered
by a passing case in that run. The coverage manifest intentionally remains
partial while validated OSC, advanced protocol state, input encoding, graphics,
clipboard, drag-and-drop and snapshot interoperability are being implemented.
**A thorough run must currently fail.** Do not change a coverage entry to
complete merely because one happy-path example passes.

Native macOS window behavior, font rendering, IME, accessibility, clipboard
integration and GPU output require separate application checks. Performance
measurements are also separate from these semantic comparisons.
