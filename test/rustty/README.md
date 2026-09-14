# Rustty terminal compatibility checks

The [CSI/OSC implementation inventory](SEQUENCES.md) traces recognized commands
through the VT, PTY, renderer, and macOS app, including intentional no-ops and
features that currently exist only in the library.

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
python3 test/rustty/parity.py --corpus --case corpus/stream-initial/
python3 test/rustty/parity.py --no-build --input
python3 test/rustty/parity.py --no-build --parser
python3 test/rustty/parity.py --no-build --osc
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
python3 test/rustty/transport_limits.py zig-out/bin/vt-oracle target/debug/examples/parity
```

`--input` adds matrices for legacy/Kitty keyboard modes, modifiers and key
actions, consumed text modifiers, IME, mouse formats, focus and paste. Use
`--case input/key` to select keyboard cases. Input cases compare each encoded
result even when it is empty; a dropped key cannot disappear from the event
list. Input requests initialize both terminals with 8-by-16-pixel cells, matching
the mouse encoder geometry. Terminal requests retain the native zero pixel-size
default. `--protocols --case protocol/host/size/resize/` compares initial and
resized snapshot geometry for both request kinds, including missing/zero cell
sizes, saturated dimensions and mode-2048 reports. `--artifacts`, `--zig-bin` and
`--rust-bin` select isolated output and adapter paths for concurrent work.

`--input --case input/mouse` also compares caller-owned motion tracking,
padding, zero/tiny surfaces and large UTF-8 coordinates. Mouse events accept
`screen_size`, `cell_size`, left/top/right/bottom `padding`, and an independent
`any_button_pressed` value. Positions are surface pixels, including padding;
renderer geometry may differ from terminal dimensions. `track_last_cell`
retains the last cell across operations; `reset_mouse` explicitly clears it.
Raw native tracking survives terminal resets, mode changes and screen switches.

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

`--osc` compares direct OSC dispatch and includes all 40 inherited `osc-initial`
and `osc-cmin` files unchanged. The first byte selects BEL, C1 ST or a missing
terminator exactly as in `fuzz_osc.zig`. Remaining bytes go directly to the OSC
parser, including embedded control bytes; wrapping these records in ANSI escapes
would change their meaning. The native adapter dispatches validated commands
through the existing stream handler. Rust's `Terminal::feed_osc_with_handler`
uses the same command parsing and host-effect dispatch as streamed input while
preserving any unfinished stream sequence.

The corpus and 176 focused fixtures pass 648 comparisons of terminal state and
host effects, including reply terminators, capture boundaries, control bytes and
pending CSI continuation. Use `--osc --case osc/corpus/` for just the inherited
records. Scalar delivery exercises native `next` rather than `nextSlice`;
delivery variants keep each direct OSC record intact. This verifies observable
command behavior, not the typed metadata of commands that the terminal ignores.
Allocator-failure injection and complete command/resource coverage remain.

The OSC hyperlink cases compare opaque URI/ID bytes in cells, the active cursor
and restored snapshots. Duplicate IDs, malformed option traversal, invalid
empty-URI endings and cursor restoration now match the native matrix. Complete
control contexts and storage behavior still need coverage.

`--protocols --case protocol/graphics/placements/` observes stored Kitty
placements with the optional `observe_graphics_placements` request flag. The
existing `graphics` image JSON stays separate. The placement observation includes
internal/external ID namespaces, requested source/size/offset values, anchors,
and the native source rectangle, pixel size, grid size and clipped grid bounds.
Both implementations sort their unordered placement storage by its complete key.

`placement_requests.py` checks ordinary display, cursor movement, source clipping,
aspect-ratio rounding, saturating dimensions, unavailable pixel geometry, resize,
replacement/deletion, history, resets and screen switches. Retransmitting an
explicit image deletes its old placements as soon as transmission starts, even
if the replacement fails. Terminal clears reclaim unplaced image data while
preserving placements outside the active area. Basic parent-orphan and virtual
clear cases are included; full placeholder rendering, margin
clipping, pruning and transports remain incomplete. Rust's
renderer and terminal now share integer placement sizing from `rustty-vt`.

`--protocols --case protocol/graphics/animation/` compares host-clock animation
ticks, their next wakeup delays and whether a tick changed image content. Image
observations include every frame's RGBA pixels and gap, displayed pixels, current
frame, playback state, loop budget/count and shown-at timestamp. A plain image
has an implicit stopped root frame. Rust keeps its public absolute-deadline API;
the adapter reports the corresponding delay, matching the native API.

The 90 timing fixtures cover both screens, multiple images, stopped/loading/running
states, finite loops, gapless frames, late ticks, clock restarts and saturation,
unplaced/replaced placements, terminal reset, frame arrival while loading, and
client-driven controls. Unplaced images schedule no redraws; loading and finite
loops park on the last displayed frame. Controls selecting the current frame
or continuing playback preserve the deadline and do not invalidate its pixels.

An additional 13 lifecycle scenarios (39 comparisons) are selected with
`--protocols --case protocol/graphics/animation/lifecycle/`. They cover deleting
the root/current/neighboring frames while retaining surviving pixels and
deadlines; frame edits, composition and replies; PNG/zlib/raw frame uploads;
excess/short raw data; chunk completion during playback or after deletion; and
same-ID replacement through screen switches and reset. These checks found and
fixed native frame-deletion indexing and chunk identity bugs, alongside Rust
deletion, composition and raw-frame decoding differences. A renderer regression
checks uploaded pixels and texture reuse through the same basic lifecycle.
These are selected transitions, not an exhaustive editing/deletion matrix;
snapshot restoration, resource limits and complete lifecycle coverage remain.


`--protocols --case protocol/graphics/placements/placeholder/` compares native
Unicode placeholder runs, resolved target keys and rounded pixel rectangles.
All reference diacritics,
missing/invalid indices, inherited IDs, row/column discontinuities, palette/RGB
IDs, stable zero-ID selection, explicit ordinary/relative targets, replacement,
erasure, reflow and screen switches are covered alongside aspect fitting,
letterboxing, partial/out-of-image runs, tiny source rectangles and oversized
grids by 484 fixtures (1,452 delivery comparisons). The renderer and adapter
share the VT decoder, target lookup and `Placement::geometry` calculation.
The optional observation reports `geometry` (offset, source and destination
pixel sizes) or `geometry_error`, using native `renderPlacement` as the reference.
Renderer tests check that relative children use independent minimum x/y origins
from the selected virtual parent's visible placeholders, and that ordinary
placements alone do not enable placeholder rendering. Rounded zero-size source
rectangles and overhanging fragments retain their full destination area by
sampling clamped edge texels in the image atlas. Zero cell sizes, overflowing
pixel products and full viewport-edge behavior remain incomplete.

`--protocols --case protocol/graphics/placements/relative/` covers ordinary
parent references and chains with 64 fixtures (192 delivery comparisons).
Cases compare explicit/fallback parent choice, missing images versus placements,
self-parent and cycle errors, the eight-link limit, rejected replacements,
cursor invariance and transitive deletion. The optional placement observation
also exposes the resolved chain root, anchor and accumulated cell offset using
native `resolveChain` and the Rust resolver shared with the renderer. Offsets
saturate as i32 at each link. Replacing an ancestor may deepen existing children
beyond eight links; those children remain stored but cannot resolve or render.
Virtual-plus-parent validation is checked; virtual-root positioning remains
part of the separate placeholder work.

`--protocols --case protocol/charsets` retains a bounded matrix of ASCII, UK
and DEC graphics mappings through G0–G3, cell-write single shifts, repeat and
snapshot continuation. Combining characters and wide-cell spacers exercise
shift consumption separately from input-scalar dispatch. Smoke cases also keep
minimized inherited failures with their original corpus source identifiers.
`--case protocol/charsets/defaults` additionally compares snapshot bytes through
initialization, reset, saved and unsaved cursor restoration, and alternate-screen
creation. The default right-hand slot is G2 in all of these paths.

`--grid` compares direct selection, literal search and tracked-reference APIs.
Each `grid` operation appends its result to `grid_results`; actions are `select`,
`clear_selection`, `select_word`, `select_word_between`, `select_line`,
`select_all`, `select_output`, `adjust_selection`, `format_selection`, `format_screen`,
`format_terminal`, `track`, `untrack`, `viewport`, `limits`, `search`,
`search_needle`, `search_feed`, `search_viewport`, `search_status`, `search_tick`,
`search_run`, `search_matches`, `search_match`, `search_selected`, `search_next`,
`search_prev`, `observe` and the `gesture_*`
actions described below.
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

`--grid --case grid/viewport-retention/` retains 400 fixtures (1,200 delivery
comparisons) for the internal viewport anchor. Native keeps this pin registered
even when the view returns to the active area or follows the top of history.
It can preserve blank cells and affect the saved cursor's reflow position.
Cases cover empty/written/history rows, viewport movement, line insertion and
deletion, partial-region indexing, history clearing, narrowing and widening,
with and without external pins. The minimized `generated/2/187` cursor failure
also remains in the smoke suite.

`--grid --case grid/resize/empty-continuation/` covers blank wrap continuations
after narrowing and widening. Dropping an empty continuation does not add a hard
line break; cursor/selection pins, prompt metadata and backgrounds can retain
the row instead. The 216 fixtures pass 648 delivery comparisons, and the smoke
suite retains the minimized `generated/2/291` case.

`--grid --case grid/selectors/` compares `Screen::select_word`,
`select_word_between`, `select_line`, `select_all` and `select_output` against
the native queries.
Their inclusive bounds appear in `selection_result`, independently of the active
`selection`; querying never replaces it. Word queries accept custom
`boundary_codepoints`. Line queries accept `whitespace`, `trim_line` and
`semantic_prompt_boundary`; defaults trim NUL/space/tab and stop at semantic
transitions. Disabling trimming includes unwritten cells, while an empty
whitespace set trims only unwritten cells. `selection_requests.py` retains 128
fixtures that passed 384 whole-buffer, scalar and varied-delivery comparisons.
They cover hard and soft wraps, wide spacers, custom boundaries, semantic
transitions within rows, history and alternate screens, and restored pages whose
physical widths differ from the terminal width. Bounds are compared directly,
including native hard-edge and row-inclusive trimming behavior.

`--grid --case grid/selectors/output/` isolates 45 command-output fixtures
(135 comparisons). Output selection uses shell integration's prompt groups
and per-cell content kinds. It preserves explicit spaces, rejects prompt/input
clicks, and retains the native screen-origin fallback before the first prompt.
Cases include absent and clipped prompts, continuation groups, unwritten and
background-only cells, history, alternate screens and mixed-width pages.

`--grid --case grid/selection-adjust/` compares all ten native adjustment motions.
The `adjust_selection` action takes an `adjustment` and changes the installed
selection's logical end, preserving its anchor and rectangular mode. It does
not scroll the viewport. Horizontal motion skips unwritten cells but includes
printed spaces; down skips unwritten rows. Direct page motion clamps only at
its destination, while down retains clamps from intervening narrow pages.
The 150 fixtures passed 450 delivery comparisons, including reversed and
collapsed endpoints, wide spacers, hard/soft wraps, history and restored pages.

`--grid --case grid/gesture/` compares the native `SelectionGesture` API directly:
`gesture_press`, `gesture_drag`, `gesture_release`, `gesture_reset`,
`gesture_deep_press` and `gesture_autoscroll`. The optional `gesture` settings
supply time in nanoseconds, pixel coordinates, repeat interval/distance, three
click behaviors and cell/surface geometry. Returned bounds appear in
`selection_result`; observations also expose click count, behavior, drag and
autoscroll state, and retained versus valid anchors. These calls never install
a selection; autoscroll moves the viewport before resolving its target cell.
The 407 fixtures pass 1,221 delivery comparisons covering cell/rectangle
thresholds, word/line/output drags, repeated clicks, pressure, autoscroll and
anchors across output, reflow, pruning, reset and screen changes. Native garbage anchors after reset/pruning are rejected
by the shared validator, including repeated presses. This core API does not
change the app's focus-only clicks or requirement to drag before selecting.

`--grid --case grid/selection-format/` compares selection exports byte-for-byte
using `format_selection`. Its `format` options select `plain`, `vt` or `html`
in `emit`, with independent `unwrap` and `trim` flags. The `formatted` result
contains hexadecimal bytes. `Screen::format_selection` exports content;
`Terminal::format_selection` also emits the palette for styled output and the
current cursor style/hyperlink for VT, matching native formatter defaults.
HTML retains native page wrappers, escaping and hyperlink identity boundaries;
VT retains native SGR ordering. The 121 fixtures exercise all 12 option
combinations, including rectangles, wide/grapheme cells, colors and attributes,
opaque hyperlink bytes, empty rows, cross-page wrapping and restored mixed-width
pages.

`--grid --case grid/terminal-format/` compares `Screen::formatter` and
`Terminal::formatter` through the `format_screen` and `format_terminal` actions.
`format_content` selects `all`, `none`, or the current `selection`. Full exporters
preserve soft-wrapped rows by default; the selection convenience method keeps
its existing unwrapping default. `screen_extra` controls cursor, style, link,
protection, Kitty keyboard and charset state; `terminal_extra` also controls
palette, modes, margins, tabstops, PWD and ModifyOtherKeys. Rust callers can use
the `NONE`, `STYLES` and `ALL` constants to select these extras.

The 93 fixtures pass 279 delivery comparisons across all formats and trim/unwrap
combinations, history, alternate screens, restored mixed-width pages and extras
individually or together. Exports preserve native ordering, including replaying
the pending-wrap edge cell before restoring cursor attributes. Direct Rust tests
also replay full and state-only exports into another terminal and check that
exporting leaves the source unchanged. The native PWD formatter now omits its
internal NUL terminator; all export bytes are compared directly.

`--grid --case grid/terminal-format/options` checks explicit foreground/background
colors, resolution of palette indices to RGB, and codepoint replacements for all
three export entry points. `Options` borrows a 256-entry palette and a slice of
`CodepointMap` rules; each rule has an inclusive character range and a character
or UTF-8 string replacement. The last matching rule wins. HTML escapes replaced
text, while blank cells keep their original padding behavior. The source terminal
is unchanged. Cases include empty strings, NUL and combining characters, overlapping
and inverted ranges, long strings, trim/unwrap combinations, page boundaries and
pending-wrap cursor replay. The adapters reject invalid Unicode scalars and
incorrect palette sizes before constructing the native options.

All formatter fixtures also request `format_map: true`. The result of the same
name contains a `[page_index, x, y]` for every output byte, compared directly
without resolving or normalizing the native coordinates. Rust's
`format_with_map()` returns the bytes and a borrowed `ByteMap`: `get()` retains
raw page coordinates, while `point()` resolves valid coordinates to a `GridPoint`.
It returns `None` when native carried blank-line coordinates extend beyond a
physical page. The borrow prevents mutation while the map is live. Maps include
UTF-8, escaped/replaced text, style/link boundaries, palette and state extras,
reversed blank-cell runs and page-local newline inheritance. Normal `format()`
exports allocate no coordinate map and share the same output path.

Literal search formats each retained page separately and follows native active
and history traversal, including repeated soft-wrap matches and trimmed blank
page tails. With scrollback disabled, it preserves native prefix pruning by
match endpoint before reversing results. `search_pages.py` also checks restored
history whose row IDs differ from physical order. The suite retains the short
search-window case that exposed a native integer underflow in
`sliding_window.zig`: subtracting the needle length before adding one failed
when only the overlap remained. The reference now subtracts the overlap length
directly, with forward/reverse tests proving the retained byte still joins the
next page's match. All 237 page-search comparisons pass without normalization.
Run the page suite independently with:

```sh
zig build vt-oracle -Demit-lib-vt=true -Demit-macos-app=false -Doptimize=ReleaseSafe
python3 test/rustty/search_pages.py > target/search-pages.json
python3 test/rustty/parity.py --no-build --fixtures target/search-pages.json --max-failures 10000
```

These direct search cases use `kind=input` to avoid serializing hundreds of
thousands of unrelated cells; their search endpoints and result order remain
unmodified. A reference-process crash is reported as a failure and ends that
run. Complete resource-driven page changes remain uncovered, so the search
coverage entry remains partial.

`--grid --case grid/search/terminal/` compares persistent `TerminalSearch`
progress and selected matches. `search_feed` copies active contents and a
bounded history window; `search_tick` searches owned data without terminal
access. `search_status` reports running, feed-required or complete, the active
screen as of the last feed, total matches and the selected index/bounds.
`search_run` first feeds current contents and finishes all available history.
`search_matches` preserves newest-first ordering and native duplicates;
`search_match` reads the index in `id`. `search_next`/`search_prev` wrap through
currently available results, with `scroll=false` disabling viewport movement.
They leave terminal text selection independent of the selected search result.

The 204 fixtures in `terminal_search.py` cover feed/tick transitions, cached
reads, ASCII-equivalent needle replacement, both screens, selected-match
retention/fallback, pruning, resize, reset, partial snapshot history restore,
restored-width edits and needles crossing several pages. All 612 delivery
comparisons pass without changing native results. Clean feeds retain physical
cached match coordinates after line edits; search scrolling also retains the
native viewport pin's column, mapping that anchor through reflow and clearing
the column when ordinary scrolling resumes from the active viewport. Cached
history bounds are compared after same-width line movement, including matches
crossing a page boundary and incomplete history searches.
Internal owned tracked points expire with their
search owner and are reclaimed before later tracking, movement or reflow.
The Rust tests check that tick still works after terminal destruction and that
dropping search leaves no pins influencing a later resize. Broader resource
mutation lifetimes and regex extensions remain separate coverage work.

`--grid --case grid/search/viewport/` checks the persistent `ViewportSearch`
cache against native `TerminalSearch.feed` and `viewportMatches`. Set a byte
needle with `search_needle`, refresh with `search_feed` (`active_dirty` defaults
to true), and read with `search_viewport`. Reads before the first feed are empty;
repeated reads preserve results; setting an ASCII-case-equivalent needle keeps
the original needle bytes and cache. Empty needles clear the search.
`viewport_search.py` covers viewport movement within and across pages, matches
outside visible rows on covering pages, multi-page soft-wrap overlap, Unicode
byte endpoints, resets, both screens, resized/restored pages, and full versus
partial-width row movement with dirty tracking disabled. Layout changes require
a feed before resolving cached endpoints against a live screen. A live search
must be cleared before replacing the oracle terminal with a snapshot, as with
tracked handles. Full-history tick orchestration and selected-match navigation
are handled by `TerminalSearch` above.

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

`--pages --case pages/wide-cut/` retains 48 cases (144 comparisons) for
non-reflow shrinking through a wide character on either screen, active or
inactive. Cells, graphemes, links, styles, snapshots and later insertion agree.
The native reference now clears both halves of a truncated wide character;
previously it left an orphan at the right edge and a later insert could assert
while clearing beyond the shortened row. The native regression also verifies
that grapheme storage is reclaimed and the tail stays cleared after widening.

`--pages --case pages/widen/` covers 32 cases (96 comparisons) for growing
without reflow and editing restored narrow pages. A spacer head becomes an
ordinary blank cell while retaining its style, hyperlink and semantic content.
Copied rows discard both wrap flags; rows that reuse their existing allocation
retain them. Cases compare cells, snapshots and page ownership before continued
printing, insertion, deletion and erasure. The shared row repair handles ordinary
resize and lazy physical-page growth with the same attribute-preserving behavior.

`--pages --case pages/styles/` exercises live STYLE ownership: each SGR
attribute, cursor movement, printed and erased cells, page migration, resize,
restored sparse IDs and mixed-width IND copies. Rustty retains the native set's
dead entries, reference counts and ID reuse history instead of reconstructing
STYLE resources when a snapshot is requested. Growth and rehashing clone live
cells in row order. Row copies retain native STYLE ownership, including padding
introduced when narrow pages grow. Failed rehash/growth leaves source cell IDs
and references unchanged. Rendering viewport copies keep resolved styles and
page boundaries without allocating live STYLE tables. The first-IND matrix can
be run separately with `--case pages/styles/mixed-ind/` (48 passing comparisons).

The retained `pages/styles/max-capacity-split/2` case exposed a native cursor
cache defect. A restored two-row page with 32 colliding STYLE entries at capacity
65535 splits during SGR. The original `Screen.splitForCapacity` moved the cursor
pin without refreshing cached `page_row`/`page_cell`; subsequent printing wrote
to the retired row and dropped `X`. This was independently reproduced with
native binary SHA-256
`760b71c042988ece0aee08e1202b3e3b255372e983f4fb451cb38cc3b9d84313`.
The reference now refreshes both caches after migrating the cursor. The existing
native split test checks the pointers and the following styled write, and all six
split comparisons pass. The fixture remains in broad and thorough runs without
normalization; compatibility is measured against this repaired reference.

`pages/styles/mixed-ind-resume/` also retains continued-printing probes after
the first IND. Restored widths 4/8/4 with logical width 8 exposed a native
out-of-bounds cursor followed by an assertion in `printSliceFill`'s STYLE release.
The reference now widens physical pages before editing logical columns. It
prepares replacement pages before moving tracked pins, preserving the source on
allocation failure. Read-only access and untouched pages retain their widths.
Rust now follows the corrected growth, padding, cursor and copied-row metadata.
All 96 first-IND and continued-printing comparisons pass. The original scalar
and batched failures remain recorded; these checks use the repaired reference.
Complete resource-exhaustion combinations, especially splitting during reflow,
remain unverified.

`--pages --case pages/graphemes/` compares live grapheme allocation and reuse
across 259 fixtures (777 delivery comparisons). Cells retain their native
bitmap allocation through moves; copies reserve a new run. Append replaces
four-codepoint chunks before freeing the previous run. Page growth uses native
utilization and row-density projection and rebuilds styles and graphemes
together. Restored snapshots retain their allocation history for continuation.
The matrix covers capacity and fragmentation boundaries, erasure, row shifts,
horizontal margins, both screens, widening across page boundaries, reflow,
mixed-width restored pages, tiny capacities and continued printing. Viewport
copies remain detached from live resource storage. Rust invariants check that
each suffix has one owner, allocations do not overlap and erased storage is
reclaimed. Complete resource limits and reflow splitting remain separate
requirements.

`--pages --case pages/hyperlinks/` retains 99 live hyperlink fixtures (297
delivery comparisons). They cover string and cell-map growth, dead set entries,
duplicate strings, colliding hashes, cursor page crossings, erasure, line shifts,
reflow, mixed restored widths and continued writes after snapshot restoration.
Cursor insertion, ordinary page copies, reflow and PAGE decoding follow their
different native allocation orders. SU/SD retain temporary cursor crossings,
including renewed implicit link IDs. Rebuilding any resource also remaps live
hyperlinks, and detached viewport copies own no string allocations.

Snapshots preserve the live LINK table IDs and cursor-only references, including
tables with more than 511 entries. Three cross-decode fixtures compare continued
terminal state with uninterrupted input. Page allocation charges are compared
separately: PAGE captures populated row counts, so a native snapshot roundtrip
can legitimately discard unused grid capacity. Complete resource exhaustion,
reflow splitting, compression and mutation combinations remain incomplete.

`--snapshot-wire --case snapshot/resources/graphemes/` includes 178 fixtures
(534 delivery comparisons) for bounded suffix admission, malformed and
duplicate entries, map and arena exhaustion, and packed-page roundtrips.
Both decoders reserve a complete suffix once, using at most 64 valid scalars.
The native decoder previously grew suffixes incrementally, requiring temporary
replacement space: a valid packed page with 32 five-codepoint suffixes restored
only 31. A native encode/decode regression and three reflow cross-decode cases
retain that repair. Insufficient capacity still drops a whole suffix, and a
later entry may retry a target whose earlier suffix could not fit.

`pages/styles/mixed-edit/` adds 114 comparisons for cursor motion, direct row
and cell edits, margins and linked/wide printing on restored pages.
`pages/styles/wrap-reset/` adds 24 comparisons for ordinary wraps and wide-cell
spacer heads: ECH, EL and DCH clear the following row's continuation as well as
the current row's wrap state. All 381 retained STYLE comparisons pass.

Grid cases cover live writes, erasure, reflow, height changes, screen switches,
resets, handle reuse and scrollback limits, including release of an inactive
screen's tracked handle. Restoring a new terminal while
the adapter owns tracked handles is explicitly unsupported; those external
lifetimes need a separate API comparison. Complete selection mutation lifetimes
and resource-driven search invalidation remain uncovered. These grid cases remain part of `--grid`
and `--thorough`.

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
constructs PAGEs whose physical width differs from the terminal width. Restored
cursors are bounded by both widths, including pending wrap and subsequent
printing; all 45 mixed-width cursor comparisons pass against the repaired native
reference. Physical PAGE widths remain intact until mutation requires growth. Use
`--case snapshot/streaming` or `--case snapshot/invalid` for isolated checks.
Streaming history checks the current width when each PAGE arrives: resizing
away and back before delivery preserves admission, but skipping an incompatible
PAGE permanently discards the rest of that screen's history sequence. Reset,
alternate-screen recreation, height changes and limits are also compared.
An expected rejection must be `InvalidSnapshot`; an unrelated adapter error
or an unexpected successful decode still fails the case.
`--case snapshot/exact` checks `snapshot::decode_exact`, which requires EOF
after FINISH for bounded buffered sources. It rejects transport tails and
concatenated snapshots without consuming the trailing bytes. Streaming users
keep using `decode`, which stops at FINISH without checking EOF.

`--case snapshot/metadata` compares terminal and screen flags, enum fallbacks,
margins, colors, mode bits, keyboard flags and scrollback limits. Unknown
cursor-default flags retain host preferences, and a restored NUL repeat writes
native empty cells. `--case snapshot/boundary` checks malformed record shapes,
ordering, duplicates, required dimensions and continuation budgets through
full, exact and incremental decoding. Rust tests inject reader and writer errors
at record headers, payload middles and boundaries, checking the returned error
and exact written prefix. Allocation-failure injection remains uncovered.
`--case snapshot/policy-live` checks that an import budget does not change
capture of later terminal input. A raised import budget also leaves the normal
8 MiB parser capture limit intact: an oversized unfinished sequence may import,
but cannot be exported again until capture recovers. Rust tests retain both
the tiny-budget regression and this independently verified native boundary.

`--snapshot-wire --case snapshot/resources/hyperlinks` compares PAGE hyperlink
table admission, string-pool allocation, duplicate values and wire IDs, invalid
entries, collision limits and the linked-cell map limit. Explicit IDs allocate
before URIs; discarded entries retain their strings until the next successful
allocation reaches set admission. Duplicate values also need temporary string
space. Oversized strings exposed a native bitmap bounds panic: the large-span
allocator checked intermediate words but not the final word. The reference now
rejects that allocation without changing the bitmap, allowing snapshot decoding
to discard the oversized hyperlink and continue. All 222 fixtures (660 delivery
comparisons) pass, including those formerly under
`snapshot/reference-limit/hyperlinks/`. The original failure remains recorded;
passing cases measure the repaired native reference.

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

OSC 1337 `Copy=:` uses the same clipboard write callback and policy. Its cases
also compare the fixed capture limit, case-insensitive key, rejected empty/query
payloads, callback absence, ordering and direct reset during capture.

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

`--protocols --case protocol/dnd` compares Kitty OSC 72 commands and host drag
actions without starting an OS drag session. The `dnd` operation observes state
or sends `move`, `drop` and `leave`; MIME names and dropped bytes use hex. The
suite compares registration, acceptance and held data, first-chunk metadata,
binary/plain response boundaries, MIME/item limits, local-only errors, RIS
retention and both snapshot encodings. Callback records include state at each
event, so multiple registrations in one input read cannot hide lost metadata.
`EffectHandler::drag_and_drop` borrows that state synchronously; deferred `feed`
returns event tags. The 8 MiB capture boundary is compared for queries and pending
registration/status chunks; allocation failures and direct utility APIs remain
unverified.

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

`osc_strings.allocating_requests` retains 14 allocating-capture boundary cases
(42 comparisons) for OSC 52, 72 and 5522. Native counts the bytes after the
numeric-prefix semicolon; OSC 52 also reserves its parser-added NUL. Cases
exercise exact and exceeded limits, longest-prefix capture, continued DND
chunks and direct reset during capture. The raw parser's default and snapshot
continuation budgets remain independently bounded. Case filtering precedes
large-payload construction. Allocator failures and peak allocation are still
unverified.

OSC 66/99 have 183 cases (549 comparisons), including six allocating-capture
boundaries and eight snapshot continuations. The native parser recognizes text
sizing and Kitty notification commands, but `TerminalStream` explicitly leaves
both callbacks unimplemented. Rust matches that no-op behavior: text sizing does
not print or alter cells, and Kitty notifications do not notify or reply, even
for queries, multipart messages and closing requests. OSC 9 notifications in
the same cases confirm that the notification callback is active. Valid and
malformed parameters, safe/unsafe UTF-8, both terminators, cancellation, direct
reset and continuation are covered without claiming broader parser completion.
The cases live in `osc_strings.unsupported_allocating_requests`; large boundaries
remain in `allocating_requests` so filtering precedes payload construction.

The six large cases fill metadata and keep the payload short. Native OSC 66
reserves a trailing NUL, accepting at most 8,388,607 captured input bytes; OSC 99
accepts 8,388,608 without a NUL. Their independent payload limits are 4096 bytes
for OSC 66, and 2048 plain or 4096 encoded bytes for OSC 99. Accepted and rejected
commands both produce no terminal effect, so these differential terminal cases
do not by themselves verify typed parser admission, metadata utility APIs or
peak capture allocations.

Each failure saves its request, both full responses and the first difference
under `target/parity/failures/`. Minimization removes operations and bytes
while retaining a successful state comparison with the same mismatching field;
it may reduce valid UTF-8 into malformed input. Original failures are retained.
An oracle crash, invalid response, unsupported operation or timeout fails the
run. Requests are limited to 32 MiB and responses to 128 MiB. Large writes use
varied chunk sizes with bounded JSON overhead; scalar delivery still exercises
each byte. This accommodates hexadecimal input around the 8 MiB OSC limit.
Graphics file, temporary-file and shared-memory transports are disabled in the
Zig oracle.

`--unicode` compares the display widths of all 1,112,064 valid Unicode scalars
in batches of 4,096 codepoints, plus rejected surrogate and out-of-range inputs.
The cases enumerate the scalar range independently of Rust's generated table.
This verifies scalar width; terminal grapheme composition needs separate cases.

`--corpus` runs the inherited `stream-initial` and `stream-cmin` terminal bytes
without requiring the full compatibility gate. The 3,295 fixtures passed 9,885
whole-buffer, scalar and varied-delivery comparisons. Their first byte is the
original delivery selector, so it is removed from terminal input. `--case`
filters corpus and generated cases as well as the other suites. Corpus builds
use ReleaseSafe, retaining runtime checks without per-edit debug integrity scans.

`--thorough` additionally exercises Unicode, input, parser, protocol and both snapshot suites, all split points
for short writes, the inherited stream corpus and generated operations.
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
