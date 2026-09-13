# Rustty terminal compatibility checks

This directory compares the Rust implementation with the current Ghostty Zig
terminal in separate processes. Rustty's application and libraries never link
the Zig oracle. Both adapters accept one JSON request per line and return one
JSON response per line. Terminal input, outgoing bytes, title and PWD are hexadecimal;
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
python3 test/rustty/parity.py --no-build --unicode
python3 test/rustty/parity.py --no-build --snapshots
python3 test/rustty/parity.py --no-build --snapshot-wire
python3 test/rustty/parity.py --no-build --protocols
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

`--snapshots` exports one snapshot from each implementation, restores each
encoding in both implementations, and resumes terminal input. It compares
the restored state and effects with uninterrupted execution as well as with
the other implementation. Both wire encodings are retained in failure
artifacts; their bytes may differ because PAGE grouping is not prescribed.
Cases include all cuts through representative UTF-8, ESC, CSI, OSC, DCS and
APC sequences, plus styles, hyperlinks, screens, history, saved cursors and
reflow.

`--snapshot-wire` compares the complete version-one wire fixture, every
truncation and selected corruptions. It observes READY, each history PAGE and
FINISH, including source offsets and live writes, resets, screen switches and
resizes between pages. Following transport bytes must remain unread. It also
constructs PAGEs whose physical width differs from the terminal width. These
advanced cases currently expose differences and original Zig assertions;
`snapshot/reference-limit/` cases retain those failures explicitly. Use
`--case snapshot/streaming` or `--case snapshot/invalid` for isolated checks.
An expected rejection must be `InvalidSnapshot`; an unrelated adapter error
or an unexpected successful decode still fails the case.

`--protocols` compares terminal-level DCS replies and host notifications.
Capability names are read
from the original Zig terminfo source; the Rust table is not used to select
the test cases. It checks every advertised capability, the extra Co/RGB/TN
keys, malformed and multiple keys, and DECRQSS style, cursor and margin
queries. The current adapters leave the host terminfo name unset. Notification
callbacks retain the original bytes, including invalid UTF-8. OSC 9/777 and
ConEmu progress cases cover terminators, optional percentages, malformed fields
and notification fallbacks. Enabling the real progress callback also checks
the progress removal effect emitted by RIS.

The protocol suite also compares OSC 52 clipboard callbacks and the resulting
PTY replies. Each request may provide `clipboard_replies`, consumed in callback
order, with `status`, `contents` (hexadecimal `mime`/`data` pairs), `available`
(hexadecimal MIME names), and `remember`. An exhausted reply list succeeds with
empty contents. The Zig adapter calls the original synchronous reply API;
the Rust adapter supplies content through `EffectHandler` and observes the
core-generated reply bytes. Failure statuses and no reply become empty contents
for OSC 52, which has no write acknowledgement or session grants. Cases compare
binary data, text MIME preference, selectors, base64 validation, terminators,
host decisions and ordering with other effects. No system clipboard is accessed.

Kitty OSC 5522 cases additionally compare metadata validation, read MIME filtering,
targets listings, DATA chunk boundaries, streamed write transactions, aliases,
write acknowledgements and remembered grants. Both adapters use the native
status/result APIs. `clipboard_read_enabled`, `clipboard_write_enabled` and
`clipboard_write_limit` configure the host; a `clipboard_options` operation may
change those settings during a transaction. `terminal_reset` directly resets
the terminal state, while `reset` delivers RIS through the parser, so their
different effects on clipboard grants are tested explicitly. One-time passwords
from paste events and exhaustive allocation/resource limits remain uncovered.

Host query fixtures provide a `host` object with optional `color_scheme`,
`device_attributes`, `size`, `enquiry`, `xtversion` and `terminfo_name` values.
The last three are hexadecimal byte strings. Callback invocation is recorded
before its PTY response, including a color scheme of `none` or a size whose
`available` is false. Missing callbacks stay absent. `title_report` and `visible`
control the corresponding host settings. `host_options` replaces this object;
`resize` accepts optional `cell_size: [width, height]` and compares mode 2048
reports separately from size-query callbacks. Tests include unknown attributes,
wide size multiplication, actual reference response limits, visibility and
saved-mode effects, title reporting, raw terminfo names and reset persistence.
These comparisons use `feed_with_handler` with the reference's absent callback
defaults. The asynchronous `feed` API's configurable application defaults are
covered by core tests rather than being substituted for these reference defaults.

Mode fixtures read the available ANSI/DEC entries from the original source.
`observe_modes` selects mode tags (`number`, `private`) whose current, saved,
reset-default and report values are compared. `observe_mode_effects` additionally
compares cursor visibility/blink and active mouse mode/format. The raw
`mode_set`, `mode_save`, `mode_restore`, `mode_raw_default` and `modes_reset`
operations call the mode-state API without handler transitions. `mode_default`
instead applies the original embedder's `defaultConfigurable` policy; its Rust
counterpart calls the guarded terminal API. Acceptance is recorded in
`mode_results`. These distinct cases prevent raw bit state from being mistaken
for semantic configuration. `cursor_defaults` sets the configured shape and
optional blink policy through the terminal APIs. Stream cases cover mode reports,
saved-state reuse, transitions, reset behavior, cursor defaults and malformed
parameters. The pinned reference ignores ANSI DECRQM and DECSTR, and truncates
unknown DECRQM mode tags to 15 bits; these behaviors are retained explicitly.

Color fixtures compare xterm OSC 4/5/10–19 and reset commands, plus Kitty OSC 21.
`observe_colors` exposes each dynamic color and all 256 palette entries with
their current, default and explicit override values. Unset colors stay null;
renderer fallback colors are not substituted for terminal state. A
`color_defaults` operation replaces the configured foreground/background/cursor
and palette defaults while preserving terminal overrides. Missing dynamic
defaults mean unset; a missing palette selects the native builtins. Parser-only
`colors` requests compare hexadecimal `color_inputs` with the original RGB
parser, including every name from the original X11 table. Protocol cases cover
palette indices, query terminators, malformed lists, unsupported targets,
configuration changes, resets and the native fixed capture/request-count limits.
The capture cases currently validate the completed command's effects and state;
intermediate parser storage/continuation and allocation-failure behavior still
need separate coverage.

OSC string fixtures compare byte-preserving title/PWD state, typed callbacks,
ConEmu/iTerm2 PWD aliases, command prefixes, control bytes and capture boundaries.
The native stream validates a title's UTF-8 before truncating it to the first
1024 bytes, which can leave a partial UTF-8 scalar in storage. PWD payloads remain
opaque bytes. The reference's title/PWD parsers reserve a NUL byte in a
2048-byte capture; completed commands longer than 2047 bytes are discarded.
ConEmu cases distinguish recognized extensions, notifications and fresh prompts,
including commands that can use all 2048 bytes without a NUL. The direct `title_set` and `pwd_set` operations
call uncapped terminal setters and emit no callbacks. Raw values and pending
captures are also exercised through both snapshot encodings and decoders.
These cases compare snapshot continuation behavior, not peak parser allocation.

Each failure saves its request, both full responses and the first difference
under `target/parity/failures/`. Minimization removes operations and bytes
while retaining a successful state comparison with the same mismatching field;
it may reduce valid UTF-8 into malformed input. Original failures are retained.
An oracle crash, invalid response, unsupported operation or timeout fails the
run. Requests are limited to 16 MiB and responses to 128 MiB. Graphics file,
temporary-file and shared-memory transports are disabled in the Zig oracle.

`--unicode` compares the display widths of all 1,112,064 valid Unicode scalars
in batches of 4,096 codepoints, plus rejected surrogate and out-of-range inputs.
The cases enumerate the scalar range independently of Rust's generated table.
This verifies scalar width; terminal grapheme composition needs separate cases.

`--thorough` additionally exercises Unicode, input, parser, protocol and both snapshot suites, all split points
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
