# Terminal hot-path follow-up, 2026-11-05

Original benchmark revision IDs are preserved; see the [history mapping](history.md)
for their equivalents after consolidating these reports at the end of the session.

Six retained improvements reduce styled ASCII scrolling from **11.666 to
9.780 µs (16.2%)** against the previous Rustty build. Ghostty measures **7.931 µs**:
Rustty's overhead drops from 47.1% to 23.3%, removing 50.5% of the excess time.
All 18 Rustty controls improve, by 6.2–17.4%.

[Final paired results](final.json) compare b87938096 with 1d78160 and the frozen
Ghostty reference. There are 12 shared engine workloads and six Rustty chunking
controls. The timed workload phase took 130.4 seconds under a 180-second total
cap, with a 30-second cap on each process. Each variant ran serially in both
orders, 20 samples per order, 0.2-second warmup, 0.5-second target measurement and
10,000 bootstrap resamples. No builds, tests or profiling ran during timing.

| Workload | Previous Rustty | Final Rustty | Ghostty | Rustty improvement |
| --- | ---: | ---: | ---: | ---: |
| ASCII feed | 0.537 µs | 0.454 µs | 0.493 µs | 15.4% |
| Plain ASCII scrolling | 7.388 µs | 6.859 µs | 5.597 µs | 7.2% |
| Styled ASCII scrolling | 11.666 µs | 9.780 µs | 7.931 µs | 16.2% |
| Styled Chinese scrolling | 17.925 µs | 15.182 µs | 35.855 µs | 15.3% |

The styled ASCII result agrees in both orders: final Rustty 9.780/9.778 µs and
Ghostty 7.931/7.931 µs. These are terminal-core measurements, excluding PTY,
fonts, rendering, GPU and application integration.

## What assembly established

[Assembly findings](assembly.md) distinguish retained improvements from rejected
experiments. The ASCII stores were already vectorized across 16 cells without
per-cell bounds checks. The useful changes remove redundant row/cursor lookup,
CSI transitions/capture work, scalar flag comparisons and disabled helper calls.
Removing style-table checks through a cache did not improve whole-workload time;
cache installation added copying and spills. Fully inlining color comparison
also lost despite a smaller stack frame. Neither unchecked indexing nor new
SIMD intrinsics were required for the retained gains.

Of the remaining 1.850 µs styled gap, 1.262 µs (68%) already exists in plain
scrolling. The extra cost introduced by styling is now 2.921 µs in Rustty versus
2.333 µs in Ghostty. This subtraction compares workloads, not individual function
timings. A separate [four-second profile](profile.txt) still distributes samples
across printing, complete CSI parsing, row growth/pruning and style/resource
management; it does not identify a single remaining bounds-check bottleneck.

Validation included 35 parser tests and, at the final production tree, 233 focused
VT tests. The parser's strict Clippy check passed. VT Clippy passed with three
pre-existing lint categories allowed; details are below. A read-only review found
no substantive correctness issues in the retained changes. Application QA,
other architectures and enabled-capture performance were not measured.

## Retained stages

The first retained change reuses the page/row ID already resolved for ASCII,
Unicode and scalar writes. It avoids constructing a full active RowView merely
to notify SIXEL overwrite bookkeeping. ARM64 disassembly confirms that the
extra Screen::row call disappears from Terminal::print_ascii.

[Row-ID measurements](row-id.json) retain all normalized samples, confidence
intervals and frozen executable hashes. In the 12-workload comparison, parsed
feed improved 2.8–6.7%, plain scrolling 3.0–5.0%, and styled scrolling 2.3–4.9%.
Styled ASCII changed from 11.611 to 11.344 µs; plain ASCII from 7.201 to 6.953 µs.
A separate two-case qualification measured 1.8% and 4.0% improvements respectively.

Comparisons ran serially in both orders, 20 samples per order, 0.2 s warmup,
0.5 s measurement target, 10,000 resamples, 30 s per-process timeout and 120 s total
budget. Builds and tests were stopped during timing. Both binaries use Rust
1.99.1 with native CPU flags, thin LTO and one codegen unit on Apple M4 Max.
These stages compare against the retained parser optimization at b87938096;
there is no fresh Ghostty measurement in this stage.

The fixed-field style-hash experiment removed the loop and variable 128-bit
shifts in assembly but changed styled timing by only 1% and slightly worsened
plain timing. It was discarded.

Validation: 187 VT unit tests, 8 batched-input tests and 4 SGR tests passed.
Formatting of the changed file and whitespace checks passed. This stage did
not run application rendering, other architectures or full workspace QA.

## Complete CSI commands

The next change recognizes complete ordinary CSI commands in batched input and
validated UTF-8 runs. It shares parameter parsing and dispatch with the scalar
parser, and keeps the scalar path for incomplete, private, intermediate and
malformed sequences. ARM64 disassembly shows the complete path avoids per-byte
state-table transitions and pending-capture calls; it does not remove bounds
checks or alter continuation semantics.

[CSI measurements](csi.json) compare against the row-ID change. Across 18 controls,
styled ASCII improved 8.0% (11.375 to 10.465 µs), styled Chinese 8.4% (17.268 to
15.821 µs), and mixed whole/4-KiB input 4.2–6.8%. Seven-byte input changed by less
than 0.4%, as expected when commands are split across chunks. Emoji feed and
styled combining had noisy initial regressions; a focused repeat reduced these
to 0.65% and 0.07%. Other controls ranged from 1.7% faster to 1.4% slower.
The broad run used a 150-second total deadline; the repeat used 40 seconds.
Other timing settings match the previous stage.

Validation: all 35 parser tests and 210 focused VT tests passed, including scalar
versus batched equivalence at every input split, continuation serialization,
batched VT input, native snapshots and SGR. Parser Clippy with warnings denied,
formatting and whitespace checks passed. No full application QA was run.

## Active-row lookup

Active row views and mutable headers now locate rows from the end of the page
list, using the existing reverse lookup. This avoids counting and traversing
scrollback pages before reaching the active screen. In ARM64 assembly,
row_header_mut shrinks from 984 to 488 bytes and its stack frame from 80 to 32
bytes. Public row bounds and physical page-width handling remain unchanged.

[Active-row measurements](active-row.json) compare against complete-CSI parsing.
Styled ASCII improved 1.6% (10.650 to 10.482 µs), consistent with the separate
qualification. Styled Chinese improved 3.3%; the other controls were 0.1% slower
to 3.2% faster. The 12-workload run used a 110-second total deadline.
Validation: 232 VT tests, including reverse lookup after page changes, narrow
restored pages, batched input, snapshots and SGR; formatting and whitespace checks.

The admitted-style cache experiment was discarded: it removed bounds,
reference-count and presence checks from cursor validation, but added style
copying and spills during SGR installation. Its paired timings were unchanged.

## Grouped style attributes

A private comparison groups the eight boolean attributes and the three metadata
fields before ordinary color equality. It reads initialized fields explicitly;
there is no layout assumption or unsafe access. Public derived equality and hash,
serialization, and default-style comparisons are unchanged.

ARM64 code groups the cursor flags into one 64-bit load and reduces color-equality
calls in cursor validation from three to one. The admitted side still partly
gathers flag bytes because its first byte also serves as the Option niche. A
more aggressive color-packing variant removed every call and reduced the stack
frame but performed worse; it was discarded.

[Style-comparison measurements](style-equality.json) show styled ASCII 3.4% faster
(10.665 to 10.305 µs), following a separate 2.5% qualification gain. Styled Chinese
improved 2.2%; all 18 controls ranged from 3.7% faster to 1.3% slower. The broad
run used a 150-second deadline; qualification used 55 seconds for three variants.

Validation: 211 VT tests passed, including an oracle against derived equality
across all flag and metadata byte values, representative color combinations and
all underline styles. Formatting and whitespace checks passed. Strict VT Clippy
reported four pre-existing warnings in graphics/features.rs, program_status.rs
and terminal.rs. It passed with only their three lint categories allowed
(byte_char_slices, unnecessary_lazy_evaluations, chunks_exact_to_as_chunks).

## Disabled capture bookkeeping

Scalar, ASCII and Unicode writers check whether tmux capture is enabled before
calling its capture/rendition helpers. The disabled path still invalidates known
capture bounds exactly as before. It also avoids scanning Unicode runs solely
for disabled capture metadata. Enabled capture keeps the existing helper calls.

[Capture-hook measurements](capture-hooks.json) show styled ASCII 1.0% faster
(10.066 to 9.962 µs), following a 1.7% qualification gain. All eight plain/styled
scrolling controls improved, from 0.7% to 5.9%. The control run used a 90-second
deadline. These timings use default capture-disabled terminals; enabled-capture
performance was not separately measured.

Validation: 200 VT tests passed. The existing capture-toggle test now covers
parsed and direct scalar ASCII/Unicode writes, verifying bounds stay invalid
after capture is re-enabled. Formatting and whitespace checks passed.

## Prepared cursor location

ASCII writes now reuse the page/row returned by row preparation, as scalar writes
already do. Wrapping and slow resource synchronization still refresh the location.
Public cursor/resource validation remains intact. ARM64 assembly removes the
unconditional synchronization call, its repeated ordinary-path lookup and nested
48-byte frame. The caller frame stays 224 bytes and vector stores are unchanged;
inlined validation increases print_ascii code size by 908 bytes.

[Cursor-location measurements](cursor-location.json) show styled ASCII 1.5% faster
(10.058 to 9.909 µs), following a 2.1% qualification gain. Plain ASCII improved
0.9–1.7%; the six feed/scrolling/chunking controls all improved by 0.2–1.5%.
The control run used a 65-second deadline. Validation: 233 VT tests passed,
including batched input, public cursor edits, snapshots, narrow restored pages
and SGR; formatting and whitespace checks also passed.
