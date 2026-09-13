# Rustty terminal compatibility checks

This directory compares the Rust implementation with the current Ghostty Zig
terminal in separate processes. Rustty's application and libraries never link
the Zig oracle. Both adapters accept one JSON request per line and return one
JSON response per line. Terminal input, outgoing bytes, title and PWD are hexadecimal;
cell text is an array of Unicode scalar values. Cell and cursor hyperlinks contain
hexadecimal URI and explicit-ID bytes, or a numeric implicit ID. This preserves
invalid UTF-8 without replacing it with display text. Adjacent PTY write callbacks
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
python3 test/rustty/parity.py --no-build --grid
python3 test/rustty/parity.py --no-build --page-layout
python3 test/rustty/parity.py --pages
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

`--protocols --case protocol/paste` exercises the state-aware paste entry point:
text versus clipboard sources, bracketed and Kitty modes, MIME selection and
listing limits, lazy reader failures, deterministic secure-entropy callbacks,
and one-time read grants. Read and entropy calls are observed alongside output
and results; event pastes must not load clipboard data. Production hosts supply
OS randomness. The deterministic entropy source exists only in the test adapters.

`--parser` compares raw UTF-8/ANSI events, parser state and all inherited
`parser-initial` and `parser-cmin` fixtures. Unlike stream fixtures, parser
fixtures contain no delivery-selector byte. The Zig adapter uses the original
parser and UTF-8 decoder. For OSC it captures bytes at the parser's transition
boundary because Zig exposes validated commands while Rust exposes raw OSC
payloads. This comparison therefore does **not** validate OSC command parsing,
effects or command-specific limits; those need terminal/protocol cases.

The OSC hyperlink cases compare opaque URI/ID bytes in cells, the active cursor
and restored snapshots. Duplicate IDs, malformed option traversal, invalid
empty-URI endings and cursor restoration now match the native matrix. Complete
control contexts and storage behavior still need coverage.

`--protocols --case protocol/charsets` retains a bounded matrix of ASCII, UK
and DEC graphics mappings through G0–G3, cell-write single shifts, repeat and
snapshot continuation. Combining characters and wide-cell spacers exercise
shift consumption separately from input-scalar dispatch. Smoke cases also keep
minimized inherited failures with their original corpus source identifiers.

`--grid` compares direct selection, literal search and tracked-reference APIs.
Each `grid` operation appends its result to `grid_results`; actions are `select`,
`clear_selection`, `track`, `untrack`, `viewport`, `limits`, `search` and `observe`.
Points name `active`, `viewport`, `screen` or `history` coordinates. Observations
translate each implementation's own handles to those coordinates and include
cell codepoints, selected text and viewport position. Raw row IDs are not shared.
Selection extraction uses the native plain-text defaults (`unwrap` and `trim`
enabled). Search needles are hexadecimal bytes. Rust's `Screen::search_literal`
accepts arbitrary bytes, folds ASCII case and retains overlapping matches and
native formatter coordinates, including reversed maps for adjacent spaces.
Partial UTF-8 needles map each endpoint to the cell containing that byte. The
regex search and link APIs keep their existing semantics. Match order and
endpoints are compared without sorting, deduplication or normalization.

Literal search formats each retained page separately and follows native active
and history traversal, including repeated soft-wrap matches and trimmed blank
page tails. With scrollback disabled, it preserves native prefix pruning by
match endpoint before reversing results. `search_pages.py` also checks restored
history whose row IDs differ from physical order. The suite retains a native
integer underflow on a short search window (`sliding_window.zig`); `--thorough`
includes that failing case. Run the page suite independently with:

```sh
zig build vt-oracle -Demit-lib-vt=true -Demit-macos-app=false -Doptimize=ReleaseSafe
python3 test/rustty/search_pages.py > target/search-pages.json
python3 test/rustty/parity.py --no-build --fixtures target/search-pages.json --max-failures 10000
```

These direct search cases use `kind=input` to avoid serializing hundreds of
thousands of unrelated cells; their search endpoints and result order remain
unmodified. A reference-process crash is reported as a failure and ends that
run. Incremental search and complete resource-driven page changes remain
uncovered, so the search coverage entry remains partial.

`--page-layout` compares native page/resource offsets, table and bitmap
capacities, column adjustment, and pooled versus exact allocation charge.
`page_layout_requests.py` retains dimension/resource boundaries and deterministic
mixed capacities. These requests compute layouts without allocating their backing
pages, including cases near the native 32-bit page-offset limit. Both adapters
impose nonzero dimensions and 32-bit page-offset limits, and report row-count
overflow in place of native adjustment's checked-cast abort. The Zig
adapter calls the real layout and adjustment functions; initial capacity uses
their private caller's standard-adjustment/fallback policy. The Rust adapter
compiles the private production calculator directly. Constants are observed from
the native ABI and currently validated on macOS ARM64 only. Passing arithmetic
cases does not establish page lifetime, resource retention, splitting, reflow or
search parity; the coverage entry remains partial. `--thorough` includes this
matrix.

Production scrollback limits use these native dimension-dependent minimums,
including a zero line limit. Only zero bytes disables normal scrollback.
Storage charges and pruning use retained page allocations and complete history
pages. Minimum-limit comparisons alone do not establish full storage compatibility.

`--pages` compares each actual storage page's logical columns, used rows,
capacity, pooled ownership and allocation charge, plus each screen's aggregate
row and byte totals. A `pages` operation appends this state to `page_results`.
Large storage cases use `kind=input` to avoid serializing unrelated cells.
Boundaries and allocation metadata are compared without regrouping rows or
normalizing values. The retained cases exercise growth, limits, clearing,
resize and snapshot admission; resource-driven page splitting remains partial.
The harness builds the native core in ReleaseSafe for `--pages` and `--thorough`:
runtime safety remains enabled, while debug-only full-page integrity scans on
every edit would make large boundary cases quadratic. When reusing binaries with
`--no-build`, build the oracle with `-Doptimize=ReleaseSafe` for these cases.

Grid cases cover live writes, erasure, reflow, height changes, screen switches,
resets, handle reuse and scrollback limits, including release of an inactive
screen's tracked handle. Restoring a new terminal while
the adapter owns tracked handles is explicitly unsupported; those external
lifetimes need a separate API comparison. Incremental search, search selection,
word/line/output selection, gestures and styled selection formatting remain
uncovered. The failing grid cases remain part of `--grid` and `--thorough`.

Native libghostty-vt updates OSC 133 semantic state without command lifecycle
callbacks. `Terminal::shell_command_events` explicitly enables Rustty's
`CommandStart`/`CommandEnd` host extension and defaults to false; application
sessions enable it. The oracle uses the native default and still rejects
unexpected effects. VT and session tests verify the opted-in event ordering.

`--protocols --case protocol/semantic` compares all native OSC 133 actions,
prompt kinds, option precedence, capture limits, line transitions and screen
clears. `observe_semantic` adds live redraw policy, click behavior, EOL-clear
state and visible/history row markers to observations. Snapshot cases restore
those values through both encodings, including `redraw=last` and `cl=w`.
These observations read terminal state directly and remain optional for other
protocol cases.

`--protocols --case protocol/prompt-redraw` compares primary-screen resize
cleanup for OSC 133 `redraw=1`, `0` and `last`, including inactive primary
screens, unmarked input, prompt history, styled/protected cells and snapshot
continuation. It also checks that alternate screens and unchanged dimensions
retain their content. Narrowing cases compare prompt metadata on every reflowed
row; inherited streams cover cleanup before DECCOLM's display erasure.

`--protocols --case protocol/graphics` uses the real Wuffs PNG callback with
its original bounded allocator and compares stored image IDs, dimensions and
RGBA pixels on both screens. Native RGB storage is expanded only at the
observation boundary. The matrix includes PNG color depths, palette/transparency,
Adam7, raw/zlib uploads, truncation, checksum corruption and dimension precedence.
Wuffs decoder storage has an alignment correction for arena allocators; it does
not change terminal protocol behavior. Placements, animation, transports and
complete graphics resource-limit coverage remain separate work.

`--protocols --case protocol/glyph` compares native Glyph APC support, uploads,
queries and clearing. Observations retain insertion order, decoded contours and
points, design metrics, declared width and protocol-controlled constraints;
padding uses exact IEEE 754 bits, including signed zero. Cases cover malformed
options/base64/outlines, replacement and 1,024-entry FIFO eviction, the 64 KiB
payload and decoder-allocation limits, dirty state and configured APC capture.
Snapshot cases resume uncommitted commands and explicitly compare version 1's
deliberate omission of committed registrations. The native headless API supports
`glyf` only, reports glossary coverage only and stores the requested width without
changing printed cell width. Font coverage, visible rasterization and injected
allocator failures remain separate work.

`--protocols --case protocol/reset-stream` resets terminal state at every byte
boundary through UTF-8, ESC/CSI, OSC, DCS and APC commands. The pending native
input stream survives a direct host reset, including captured DCS/APC bytes and
their overflow state. This is separate from terminal state cleared by RIS;
already-executed commands are not replayed after either reset.

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

`--snapshot-wire --case snapshot/resources/hyperlinks` compares PAGE hyperlink
table admission, string-pool allocation, duplicate values and wire IDs, invalid
entries, collision limits and the linked-cell map limit. Explicit IDs allocate
before URIs; discarded entries retain their strings until the next successful
allocation reaches set admission. Duplicate values also need temporary string
space. The suite keeps the native bitmap bounds panic for oversized strings in
`snapshot/reference-limit/hyperlinks/`, after the returning wire cases. These
failures remain part of unfiltered `--snapshot-wire` and `--thorough` runs.

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
