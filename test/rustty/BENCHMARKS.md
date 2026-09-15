# Portable terminal primitive benchmarks

The Rustty benchmarks use Criterion and call the headless `rustty-vt` APIs
directly. They do not construct a session, PTY, font system, renderer, GPU, or
window. Criterion is a development dependency only. No application build is
needed. Timing excludes terminal construction and input generation; the `feed`
and `stream` workloads include UTF-8 decoding and VT parsing.

```sh
cargo bench --offline -p rustty-vt --bench primitives
# Filter a single primitive and corpus:
cargo bench --offline -p rustty-vt --bench primitives -- rustty/print/chinese
# Exercise every workload once, without collecting timing statistics:
cargo bench --offline -p rustty-vt --bench primitives -- --test
```

## Before and after

Keep the harness, input, compiler flags, and machine the same across revisions.
Build both revisions before measuring, then run them **serially** with other
builds and benchmarks stopped. Criterion supplies warmup, sampling, confidence
intervals, outlier analysis, and saved baseline comparisons.

```sh
cargo bench --offline -p rustty-vt --bench primitives -- --save-baseline before
# After the implementation change:
cargo bench --offline -p rustty-vt --bench primitives -- --baseline before
```

Set `CRITERION_HOME` to the same absolute directory when using separate
worktrees or target directories. Results otherwise live in `target/criterion`.
For an exploratory run, append `--sample-size 20 --warm-up-time 0.3
--measurement-time 1`; use the defaults for longer measurements.

To measure the earlier String-backed cells, copy this same benchmark to that
revision and adapt only `cell_sum` to read `row.cells[col].text`, and `scalar`
to read `cell.text.chars().next()`. All setup and measured operations stay the
same. The benchmark prints the compiled cell size to identify its layout.

## Ghostty comparison

The optional Zig executable imports only `libghostty-vt` and the Zig standard
library. It avoids the application dependencies and macOS instrumentation in
the general `ghostty-bench` runner. This target also cross-compiles to Linux.

```sh
zig build vt-primitives test-vt-primitives \
  -Demit-lib-vt=true -Demit-macos-app=false -Doptimize=ReleaseFast
GHOSTTY_PRIMITIVES_BIN="$PWD/zig-out/bin/vt-primitives" \
  cargo bench --offline -p rustty-vt --bench primitives
```

With that environment variable, Criterion registers matching `ghostty/*`
groups. `iter_custom` returns the duration reported by the **native operation
loop**; process startup, file reads, terminal setup, JSON, IPC, and final
terminal destruction are excluded. Predecoding for the original primitives is
also excluded; `feed` and `stream` decode inside their measured parser calls. Both engines use identical
pre-generated UTF-8 corpora and validate content checksums. Ghostty's own unit
check additionally inspects combining and ZWJ suffixes and cloned text.
The native process primes a fresh terminal for each Criterion batch; Rust
retains its primed terminal across batches. Each batch repeats its operation
many times to measure steady behavior.

For a Rust-only saved baseline, filter the comparison to `rustty`, then run
the `ghostty` groups separately: Criterion's strict `--baseline` requires an
existing baseline for every selected group. Compare the resulting Criterion
estimates using the same units. Native pages, allocators, and retained metadata
differ between engines, so these are comparisons of equivalent terminal
operations, not of Rust versus Zig in isolation.

## Workloads and units

All terminals have 128 columns and 32 rows and DEC 2027 grapheme handling
enabled. The six original primitives and `feed` disable scrollback. Their
corpora repeat a fixed pattern 128 times:

| Corpus | Pattern | Input scalars | Allocating text path |
| --- | --- | ---: | --- |
| ASCII | `abcdefgh` | 1,024 | None with inline cells |
| Chinese | `天地玄黄宇宙洪荒` | 1,024 | None: each ideograph is one inline scalar |
| Combining | `áb̂c̃d̈` | 1,024 | 512 two-scalar graphemes |
| Emoji | `👩‍💻👨‍🚀` | 768 | 256 three-scalar ZWJ graphemes |

| Primitive | Measured work | Throughput unit |
| --- | --- | --- |
| `width` | Unicode width lookup on predecoded scalars | Input scalar |
| `print` | Home cursor and overwrite the same populated viewport via `Terminal::print` | Input scalar |
| `scalar` | Sum the first codepoint of every active cell | Cell slot (4,096 per scan) |
| `read` | Sum every codepoint, including page-owned grapheme suffixes | Cell slot (4,096 per scan) |
| `clone` | Copy the visible screen and destroy the copy | Cell slot (4,096 per copy) |
| `reflow` | Resize 128 → 64 → 128 columns, preserving all text | Complete resize round trip |
| `feed` | Parse CUP (home cursor) followed by the same UTF-8 text as `print` | Input byte |
| `stream` | Parse 32 plain wrapped records, scrolling and evicting bounded history | Input byte |
| `stream_styled` | Same records with alternating SGR foreground colors and bold | Input byte |

`print` measures warmed overwrites, including replacement of previous text;
it bypasses the UTF-8/VT parser. `scalar` isolates inline access, whereas `read`
uses the public text iterator, decoding UTF-8 only for graphemes. Empty cells and
wide-cell continuations are scanned too. `clone` includes destruction in both
engines. Reflow fits within the active screen even for the Chinese corpus.

`feed` exposes UTF-8 decoding, parser dispatch, and any printable-run batching in
addition to cell replacement. It includes a three-byte CUP sequence. Stream
iterations contain 32 records of 192 display columns followed by CRLF: 6,144
printed columns and 64 physical rows. Record lengths are equal in display
columns across corpora, using 24 ASCII, 12 Chinese, or 48 combining/emoji pattern
repetitions. `stream_styled` cycles palette colors 1–4, alternates bold, and
resets SGR before each CRLF.

| Stream corpus | Printable scalars per iteration | Plain input bytes | Styled input bytes |
| --- | ---: | ---: | ---: |
| ASCII | 6,144 | 6,208 | 6,576 |
| Chinese | 3,072 | 9,280 | 9,648 |
| Combining | 12,288 | 18,496 | 18,864 |
| Emoji | 9,216 | 33,856 | 34,224 |

Both stream engines request a 1,024-line history limit with no byte limit and
prime with 2,048 physical rows before measurement, so allocations and history
eviction are already active. Native pages and resource admission can retain
different exact row counts below the same requested limit. Rust keeps the
terminal across Criterion batches; the native helper primes a fresh terminal
for each batch. History eviction is page-granular, so longer measurements
average over allocation/recycling phases better than single iterations.

Outside the timers, the harness checks the analytically expected visible text,
every populated cell's foreground/bold, final cursor, and bounded nonempty
history. An order-sensitive checksum over active cell contents and styles must
match the native helper. These checks are exercised by `--test` as well as
ordinary Criterion runs. All input generation and priming stay outside timing.

Allocator instrumentation is excluded from timing. The separate
`scalar_allocations` test checks that ordinary scalar printing and reads make
no allocations. Chinese text alone does not exercise the grapheme allocator;
the combining and emoji cases do.

### Supplemental chunked input

The six Rust-only `rustty/chunked_feed_mixed` and
`rustty/chunked_stream_mixed` cases deliver identical input whole, in 7-byte
chunks, or in 4-KiB chunks. Chunk boundaries may split UTF-8, CSI, combining
sequences and ZWJ emoji. Each 16-column unit is `abcdefgh天地áb̂👩‍💻`, with
foreground/bold SGR changes between units, exposing short printable runs.

The feed case overwrites 16 populated rows with 128 units (4,935 input bytes),
including CUP and a final SGR reset, without scrollback. The stream case uses
the same 32 wrapped 192-column records, 1,024-line history limit and 2,048-row
priming as the original streams (14,976 input bytes per iteration). Throughput
counts input bytes. Construction, input generation, priming and validation
remain outside timing; each timed iteration includes all chunk deliveries.

Before timing, all three deliveries must produce identical cursor, history,
cell text, widths, styles and wrap flags. Exact expected cells and row layout
are also checked independently before and after timing, including retained
history. These groups leave the original 36 workloads and native helper
unchanged. Save a separate baseline for them:

```sh
cargo bench --offline -p rustty-vt --bench primitives -- \
  chunked_ --save-baseline chunked-before
```

### Supplemental history reflow

The four Rust-only `rustty/reflow_history` cases use the same ASCII, Chinese,
combining and emoji stream records. Setup writes 256 records of 192 display
columns plus CRLF, retaining 481 history rows and 32 visible rows at 128
columns. Each timed iteration resizes 128 → 64 → 128 columns. At 64 columns,
737 history rows remain; the 1,024-line limit and absent byte limit allow all
content to survive. Throughput counts complete resize round trips.

One full resize round trip primes the terminal before timing. No input is
added inside the measured loop, so history neither grows nor evicts records.
Outside timing, exact cell text, widths, styles, wrap flags, cursor position
and history/viewport row counts are checked at both widths before measurement
and at 128 columns after measurement. Construction, parsing, priming and
validation are excluded. These cases supplement the existing 42 Rust workloads
and have no native counterpart.

### Supplemental host-memory policy

The eight Rust-only `rustty/stream_memory_capped` and
`rustty/stream_styled_memory_capped` cases repeat the original stream inputs
with a 50,000,000-byte owned-history cap, matching the app's default host-memory
policy. They retain the original 1,024-line native limit and priming. The host
cap is high enough to preserve this workload's history, but activates its
incremental payload accounting. These cases exercise that accounting cost;
they do not measure host-cap eviction or the complete app.

Outside timing, the harness checks that enabling the cap and feeding another
batch preserve the uncapped reference's history length and expected visible
contents, styles and cursor. The same content and history bounds, plus the
owned-byte cap, are checked after timing. The existing 46 Rust workloads and
36 native comparisons are unchanged; the new eight cases have no native
counterpart because this cap charges Rust-owned storage.

## Optimization measurements, 2026-09-15

The table compares Rustty at `3116bbb` with the four optimizations ending at
`2880a22`. Both versions have 56-byte cells. Ghostty production code is unchanged;
the reference uses the original native benchmark binary. These are medians in
microseconds per workload, with the units defined above, on an Apple M4 Max
running macOS 26.7, Rust 1.98.1, Zig 0.16.0, and Criterion 0.8.2. Rust uses the
workspace release profile (thin LTO, one codegen unit); Ghostty uses ReleaseFast.
Runs were serial, without competing builds or tests, using 20 samples, 0.3 seconds
of warmup, and 1 second of measurement per case. These short samples measure
the primitives, not application CPU or rendering performance.

| Primitive | Corpus | Rust before µs | Rust after µs | Ghostty reference µs |
| --- | --- | ---: | ---: | ---: |
| width | ASCII | 15.615 | 0.478 | 0.345 |
| width | Chinese | 13.735 | 0.478 | 0.342 |
| width | Combining | 15.212 | 0.435 | 0.408 |
| width | Emoji | 11.357 | 0.331 | 0.304 |
| print | ASCII | 41.018 | 41.677 | 4.972 |
| print | Chinese | 109.186 | 55.880 | 11.067 |
| print | Combining | 100.698 | 50.015 | 399.149 |
| print | Emoji | 122.636 | 46.300 | 13.972 |
| scalar | ASCII | 1.538 | 1.532 | 1.336 |
| scalar | Chinese | 1.489 | 1.519 | 1.380 |
| scalar | Combining | 1.561 | 1.541 | 1.329 |
| scalar | Emoji | 1.480 | 1.523 | 1.298 |
| read | ASCII | 10.600 | 1.720 | 1.934 |
| read | Chinese | 12.571 | 1.768 | 1.913 |
| read | Combining | 13.124 | 2.554 | 5.954 |
| read | Emoji | 11.736 | 2.617 | 2.446 |
| clone | ASCII | 10.923 | 10.656 | 5.830 |
| clone | Chinese | 11.746 | 11.842 | 5.834 |
| clone | Combining | 29.078 | 12.835 | 17.411 |
| clone | Emoji | 19.775 | 11.787 | 11.030 |
| reflow | ASCII | 274.793 | 69.065 | 22.501 |
| reflow | Chinese | 270.201 | 74.211 | 26.377 |
| reflow | Combining | 297.831 | 64.418 | 51.870 |
| reflow | Emoji | 275.598 | 56.242 | 32.131 |

Ghostty's indexed Unicode tables and tracked pins informed the Rustty changes:
Unicode properties now use deduplicated blocks instead of binary searches,
and reflow maps only live anchors instead of every cell. Inline text iteration
avoids a scalar-to-UTF-8 round trip. Grapheme payloads use page chunk indices
instead of hashing, and append builds its replacement in bounded stack scratch
before allocating one shared payload.

Full-text reads improve 4.5–7.1×, reflow 3.6–4.9×, Chinese printing 2.0×, and
combining/emoji printing 2.0–2.6×. ASCII printing remains essentially unchanged
and substantially slower than Ghostty. A separate scalar-scan confirmation
using 50 samples, 0.5 seconds of warmup, and 2 seconds of measurement confirmed
small regressions: Chinese 1.485 → 1.535 µs (3.4%) and emoji 1.473 → 1.530 µs
(3.9%) per 4,096-cell scan. No scalar-scan speedup is claimed.

The indexed Unicode table occupies
55,808 shared read-only bytes, replacing 14,994 bytes of production range data;
the old ranges remain a test-only oracle. Grapheme slot metadata is allocated
lazily: scalar-only pages have none, while long or sparse suffix allocations
can leave empty slots in the indexed vector. This change does not shrink cells.

The Ghostty combining-print result hits its existing full grapheme-map cliff
at 512 clusters. It does not represent general combining-text throughput:
the earlier density control measured about 404 µs at 512 clusters and 10 µs
at 513, where priming triggers page growth. Ghostty production code was not
modified to remove this cliff. Native reflow also includes OS page-recycling
costs, so the comparison is specific to this machine and allocator.

Validation passed all 291 `rustty-vt` tests, all 24 benchmark smoke cases, strict
all-target VT Clippy, formatting, the app check, and native benchmark unit
checks. Differential Unicode, snapshot, resize, graphics-anchor, and grapheme
suites passed 7,970 of 7,973 comparisons. The remaining three are chunking
variants of the preexisting `pages/graphemes/wrap/3/1/1/alternate` discrepancy:
Rustty preserves a ZWJ when wrapping a widening grapheme in a one-row alternate
screen; Ghostty drops it. This also reproduces before the inline-cell changes.

## Printing and scrolling follow-up, 2026-09-15

This follow-up starts with the already optimized production code at `e112c2c`.
The identical 36-case harness from `847ad1c` measures that baseline and the
production changes ending at `d2911c9` (`5cc935a` replaces printed cells directly;
`d2911c9` avoids building resource lists for rows without resources). Ghostty
production code remains unchanged; only its benchmark utility was extended.

Measurements use the same machine, compilers, release settings, and short
Criterion sampling configuration described above. All builds and tests finished
before serial timing. Values are medians in microseconds per complete workload.
The `feed` and stream byte/scalar counts are defined in the workload tables;
they are not the same amount of work as the original `print` case.

ASCII printing improves 3.38× and Chinese printing 1.87×. Parsed ASCII overwrites
improve 2.73×, plain ASCII streams 2.66×, and styled ASCII streams 2.18×. This
pass does not reach the proposed 5× printing target. Ghostty's printable-run
batching remains a substantial opportunity for parsed output. Cell size stays
56 bytes. The ASCII/combining direct scalar-scan controls are about 3% slower
in this run, roughly 0.04 microseconds per 4,096-cell scan; those regressions
are retained in the table rather than treated as improvements.

Sampling used optimized Rust binaries with debug information and frame pointers,
at nominal 1 kHz for eight seconds per workload. Timings above use the normal
release binaries without profiling. Shares below describe physical symbols;
inlined work contributes to its enclosing symbol, and shares are independently
normalized for each run.

- Before the change, cursor/style/link helpers account for about 32% of ASCII
  printing samples, and erase-before-replace accounts for another 10%.
- After replacement was consolidated, `sync_resource_row` accounted for 29% of
  ASCII stream samples. After skipping resource-list construction for rows
  without resources, its share falls to 6.4%.
- Final ASCII streaming spends about 28% in cell writing, 16% in the enclosing
  print operation, 11% accounting for row storage, and 8% allocating/initializing
  cell rows. These identify remaining costs beyond printable-run batching.

Two native reference cases need special interpretation. Combining overwrite
still hits the full 512-entry grapheme-map cliff. Parsed Chinese overwrite hits
another existing cliff: the native batch writer scans the remaining eligible
Unicode run, rejects an existing wide destination cell, prints one character,
and rescans the suffix. That is roughly half a million eligibility checks for
1,024 characters. A confirming profile attributes 96.4% of samples to
`Terminal.printSlice`. Fresh-row Chinese streaming batches successfully. The
Chinese `feed` result therefore does not describe general Chinese throughput.
Neither native production behavior was changed.

Final validation: all 293 `rustty-vt` tests, all 72 Rust/native benchmark smoke
cases, strict all-target Clippy, formatting, and the app check pass. Charset,
style, hyperlink, and grapheme differential suites pass 2,013 of 2,016 cases;
the three failures are the same previously documented alternate-screen ZWJ
mismatch in whole/scalar/chunked input variants. Independent review found no
correctness issues in either production change.

| Operation | Corpus | Rustty before µs | Rustty after µs | Ghostty µs | Speedup |
| --- | --- | ---: | ---: | ---: | ---: |
| print | ascii | 43.466 | 12.861 | 6.236 | 3.38× |
| print | chinese | 57.447 | 30.717 | 12.130 | 1.87× |
| print | combining | 50.903 | 34.039 | 404.776 | 1.50× |
| print | emoji | 47.003 | 37.587 | 15.084 | 1.25× |
| feed | ascii | 46.080 | 16.875 | 0.502 | 2.73× |
| feed | chinese | 61.892 | 35.659 | 441.271 | 1.74× |
| feed | combining | 56.481 | 38.841 | 436.534 | 1.45× |
| feed | emoji | 52.625 | 42.016 | 17.382 | 1.25× |
| stream | ascii | 465.277 | 175.223 | 5.749 | 2.66× |
| stream | chinese | 371.331 | 179.941 | 9.311 | 2.06× |
| stream | combining | 906.178 | 563.151 | 502.738 | 1.61× |
| stream | emoji | 864.453 | 633.851 | 743.788 | 1.36× |
| stream_styled | ascii | 545.556 | 249.981 | 7.908 | 2.18× |
| stream_styled | chinese | 420.376 | 220.568 | 35.590 | 1.91× |
| stream_styled | combining | 1032.979 | 739.317 | 541.605 | 1.40× |
| stream_styled | emoji | 1010.541 | 808.619 | 772.004 | 1.25× |
| read | ascii | 1.792 | 1.743 | 1.940 | 1.03× |
| read | chinese | 1.793 | 1.751 | 2.030 | 1.02× |
| read | combining | 2.541 | 2.508 | 5.932 | 1.01× |
| read | emoji | 2.529 | 2.488 | 2.568 | 1.02× |
| clone | ascii | 10.987 | 11.146 | 5.734 | 0.99× |
| clone | chinese | 12.086 | 11.916 | 5.737 | 1.01× |
| clone | combining | 13.198 | 12.888 | 17.418 | 1.02× |
| clone | emoji | 12.174 | 11.802 | 9.545 | 1.03× |
| reflow | ascii | 69.160 | 68.539 | 23.560 | 1.01× |
| reflow | chinese | 71.441 | 70.129 | 24.780 | 1.02× |
| reflow | combining | 64.546 | 64.676 | 51.886 | 1.00× |
| reflow | emoji | 57.025 | 57.145 | 41.325 | 1.00× |
| width | ascii | 0.479 | 0.480 | 0.361 | 1.00× |
| width | chinese | 0.481 | 0.480 | 0.343 | 1.00× |
| width | combining | 0.444 | 0.437 | 0.431 | 1.02× |
| width | emoji | 0.342 | 0.328 | 0.321 | 1.04× |
| scalar | ascii | 1.558 | 1.598 | 1.308 | 0.97× |
| scalar | chinese | 1.542 | 1.553 | 1.310 | 0.99× |
| scalar | combining | 1.569 | 1.609 | 1.304 | 0.97× |
| scalar | emoji | 1.547 | 1.535 | 1.304 | 1.01× |

## Mode lookup and reflow follow-up, 2026-09-15

This pass compares the session baseline `f56b1af` with `1bc18ab`. The benchmark
harness, corpora, release profile and 56-byte cells are unchanged. Both binaries
were built before the final serial measurements, with task builds and tests
stopped. Runs use the same machine described above, macOS 26.7, Rust 1.98.1,
Criterion 0.8.2, thin LTO and one codegen unit: 20 samples, 0.3 seconds of warmup
and 1 second of measurement. Values below are medians in microseconds per
complete workload. Ghostty remains the unchanged correctness reference; its
previous timing results were not remeasured in this pass.

Printing improves 1.15–1.71× and reflow improves 1.13–1.35×. The changes are
separate commits:

- `9f8e5ba` stores current, saved and default modes in their existing snapshot
  bit order. Constant mode queries compile to a single bit extraction instead
  of searching a tree for every printed scalar. Save/reset lifetimes and
  snapshot encoding are preserved. The public serde map shape is retained for
  complete supported mode sets; incomplete or unknown-key maps are rejected.
- `ad838f6` defers cell allocation for independent blank reflow rows until they
  contain copied cells or survive trailing-row trimming. Row identities,
  anchors and metadata remain available throughout the copy. Exact capacities
  are preserved even at widths 1–3, keeping history memory accounting stable.
- `1bc18ab` walks immutable source pages in order and computes each page's
  resized capacity once, avoiding repeated page scans and layout arithmetic.

An eight-second, nominal 1 kHz profile of baseline ASCII reflow attributed about
25% of active samples to cell-vector initialization and 11% to page layout,
metadata and column adjustment. These are physical-symbol shares from an
optimized build with debug information and frame pointers; inlined work is
charged to its enclosing symbol. The timing table uses normal release builds.

Parsed input and streaming medians also improve. Short-run means for some
stream and clone cases have large outliers, so those median changes should not
be read as precise application-throughput predictions. Scalar/read/clone
controls include small regressions in the table; no speedup is claimed for
those paths.

| Operation | Corpus | Before µs | After µs | Speedup |
| --- | --- | ---: | ---: | ---: |
| print | ascii | 13.161 | 11.477 | 1.15× |
| print | chinese | 31.467 | 18.434 | 1.71× |
| print | combining | 33.994 | 27.429 | 1.24× |
| print | emoji | 38.509 | 29.471 | 1.31× |
| reflow | ascii | 69.740 | 54.291 | 1.28× |
| reflow | chinese | 71.336 | 63.227 | 1.13× |
| reflow | combining | 65.486 | 49.447 | 1.32× |
| reflow | emoji | 57.817 | 42.693 | 1.35× |
| feed | ascii | 17.204 | 15.544 | 1.11× |
| feed | chinese | 36.674 | 24.164 | 1.52× |
| feed | combining | 40.151 | 34.358 | 1.17× |
| feed | emoji | 44.339 | 34.744 | 1.28× |
| stream | ascii | 181.088 | 166.024 | 1.09× |
| stream | chinese | 185.136 | 145.148 | 1.28× |
| stream | combining | 584.987 | 492.292 | 1.19× |
| stream | emoji | 647.918 | 532.768 | 1.22× |
| stream_styled | ascii | 254.692 | 241.795 | 1.05× |
| stream_styled | chinese | 225.149 | 187.023 | 1.20× |
| stream_styled | combining | 764.780 | 692.608 | 1.10× |
| stream_styled | emoji | 819.850 | 734.812 | 1.12× |
| scalar | ascii | 1.621 | 1.626 | 1.00× |
| scalar | chinese | 1.572 | 1.585 | 0.99× |
| scalar | combining | 1.618 | 1.643 | 0.98× |
| scalar | emoji | 1.553 | 1.559 | 1.00× |
| read | ascii | 1.799 | 1.793 | 1.00× |
| read | chinese | 1.836 | 1.787 | 1.03× |
| read | combining | 2.530 | 2.606 | 0.97× |
| read | emoji | 2.563 | 2.591 | 0.99× |
| clone | ascii | 11.268 | 11.133 | 1.01× |
| clone | chinese | 11.997 | 11.876 | 1.01× |
| clone | combining | 13.124 | 13.517 | 0.97× |
| clone | emoji | 12.045 | 12.330 | 0.98× |
| width | ascii | 0.488 | 0.491 | 0.99× |
| width | chinese | 0.489 | 0.490 | 1.00× |
| width | combining | 0.453 | 0.445 | 1.02× |
| width | emoji | 0.335 | 0.335 | 1.00× |

A longer control confirmation used 50 samples, 0.5 seconds of warmup and
2 seconds of measurement, again running the same binaries serially. Combining
clone measured 13.204 → 13.186 µs (no significant change), and emoji clone
12.138 → 11.938 µs. The combining scalar scan measured 1.664 → 1.582 µs,
so its short-run regression did not repeat. Combining full-text reads did
confirm a small regression: 2.493 → 2.535 µs, or 1.7% per 4,096-cell scan.

Validation passed all 288 VT tests, strict all-target VT Clippy, formatting,
the app check and all 72 Rust/native benchmark smoke cases. The selected mode,
snapshot, resize and reflow differential suites passed all 2,515 comparisons.
Review checked mode save/reset behavior, snapshot bit order, page alignment,
resource rebuilding and anchor preservation. The new regression checks retained
blank gaps, viewport padding, snapshot round trips, subsequent printing and
exact capacities at widths 1–3.

## Pre-batching Rustty/Ghostty baseline, 2026-09-15

Fresh measurements at `71a0b4f` (Rustty production code through `1bc18ab`),
using the unchanged 36-case harness for both engines. Both binaries were built
and all 72 smoke cases passed before timing; the native benchmark unit checks
also passed. All 72 measured cases completed their content validations.

Runs were serial, with task builds and tests stopped, on the same Apple M4 Max
running macOS 26.7, Rust 1.98.1, Zig 0.16.0 and Criterion 0.8.2. Rust uses thin
LTO and one codegen unit; Ghostty uses ReleaseFast. Each case uses 50 samples,
0.5 seconds of warmup and 2 seconds of measurement. These longer samples
establish a new baseline; differences from earlier tables are not additional
optimization gains.

Values are median microseconds per complete workload, including a complete
resize round trip for reflow. **Rustty/Ghostty** divides the two medians:
values above 1 mean Ghostty is faster; values below 1 mean Rustty is faster.

| Operation | Corpus | Rustty µs | Ghostty µs | Rustty/Ghostty |
| --- | --- | ---: | ---: | ---: |
| print | ascii | 11.421 | 6.373 | 1.79× |
| print | chinese | 17.819 | 12.506 | 1.42× |
| print | combining | 27.075 | 415.678 | 0.07× |
| print | emoji | 29.982 | 15.165 | 1.98× |
| reflow | ascii | 54.219 | 31.227 | 1.74× |
| reflow | chinese | 63.615 | 33.106 | 1.92× |
| reflow | combining | 55.219 | 56.655 | 0.97× |
| reflow | emoji | 42.314 | 38.129 | 1.11× |
| feed | ascii | 15.390 | 0.514 | 29.94× |
| feed | chinese | 23.855 | 537.443 | 0.04× |
| feed | combining | 32.799 | 547.380 | 0.06× |
| feed | emoji | 34.472 | 21.564 | 1.60× |
| stream | ascii | 162.761 | 8.835 | 18.42× |
| stream | chinese | 142.041 | 14.678 | 9.68× |
| stream | combining | 497.660 | 595.353 | 0.84× |
| stream | emoji | 537.748 | 952.646 | 0.56× |
| stream_styled | ascii | 236.498 | 13.001 | 18.19× |
| stream_styled | chinese | 181.230 | 49.334 | 3.67× |
| stream_styled | combining | 656.783 | 587.584 | 1.12× |
| stream_styled | emoji | 694.627 | 836.617 | 0.83× |
| scalar | ascii | 1.596 | 1.327 | 1.20× |
| scalar | chinese | 1.559 | 1.335 | 1.17× |
| scalar | combining | 1.606 | 1.329 | 1.21× |
| scalar | emoji | 1.544 | 1.327 | 1.16× |
| read | ascii | 1.813 | 1.983 | 0.91× |
| read | chinese | 1.784 | 1.999 | 0.89× |
| read | combining | 2.594 | 6.060 | 0.43× |
| read | emoji | 2.558 | 2.508 | 1.02× |
| clone | ascii | 11.020 | 6.470 | 1.70× |
| clone | chinese | 11.683 | 6.480 | 1.80× |
| clone | combining | 13.101 | 18.500 | 0.71× |
| clone | emoji | 11.776 | 10.706 | 1.10× |
| width | ascii | 0.490 | 0.358 | 1.37× |
| width | chinese | 0.488 | 0.351 | 1.39× |
| width | combining | 0.450 | 0.437 | 1.03× |
| width | emoji | 0.336 | 0.326 | 1.03× |

Combining `print`/`feed` still encounter Ghostty's 512-cluster grapheme-map
cliff, and Chinese `feed` still encounters its wide-destination suffix-rescan
cliff. These cases do not describe general native Unicode throughput. Reflow
still excludes scrollback; stream cases use the requested 1,024-line history
limit. Native batches construct a fresh primed terminal while Rust retains its
primed terminal, with setup excluded from both timers.

Some native feed/stream medians remain variable. For example, the 95% median
confidence intervals are 8.387–9.433 µs for ASCII `stream` and
493.845–600.944 µs for combining `feed`. The saved estimates and samples retain
all confidence intervals for subsequent comparisons.

The local Criterion baseline is **`baseline-71a0b4f`**, under
`target/criterion-baseline-71a0b4f/`. That directory also retains the benchmark
binaries, SHA-256 hashes and toolchain metadata in `metadata.json`, the run and
smoke logs, and the extracted `comparison.json`/`comparison.md` table.
After building a candidate, compare it with this baseline using:

```sh
CRITERION_HOME="$PWD/target/criterion-baseline-71a0b4f" \
GHOSTTY_PRIMITIVES_BIN="$PWD/target/criterion-baseline-71a0b4f/vt-primitives" \
  cargo bench --offline -p rustty-vt --bench primitives -- \
  '^(rustty|ghostty)/(print|reflow|feed|stream|stream_styled|scalar|read|clone|width)/' \
  --baseline baseline-71a0b4f \
  --sample-size 50 --warm-up-time 0.5 --measurement-time 2
```

## Parsed input and streaming batches, 2026-09-15

This pass compares production code at `c98fa109c`, built with the 42-case
harness from `329efa7`, against `d43d848` using that same harness. The original
36 workloads are unchanged; the six mixed/chunked workloads described above
are Rust-only. Both Rust binaries were built before measurement. Runs were
serial, without concurrent builds or tests, using 50 samples, 0.5 seconds of
warmup and 2 seconds of measurement on the same machine and release settings
documented above. Values are median microseconds per complete workload.
Speedup is Rustty before/after; Rustty/Ghostty is Rustty after/native, so values
above 1 in the last column mean Ghostty is faster. The Ghostty column is a
fresh measurement of the unchanged native binary. Its feed/stream timings
remain variable; differences from the preceding native table are not code gains.

ASCII `feed` improves 10.01×; plain and styled ASCII streams improve 2.73× and
3.24×. Chinese improves 2.17× for `feed`, 1.61× for plain streams and 1.86× for
styled streams. Mixed/chunked feed improves 1.22–1.36× and mixed streams
1.28–1.45×. Emoji streaming is effectively unchanged. The remaining plain
ASCII/Chinese stream gap is still about 9× versus this native reference.

| Operation | Corpus | Before µs | After µs | Speedup | Ghostty µs | Rustty/Ghostty |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| print | ascii | 10.995 | 11.997 | 0.92× | 6.272 | 1.91× |
| print | chinese | 17.245 | 18.705 | 0.92× | 12.054 | 1.55× |
| print | combining | 25.836 | 29.111 | 0.89× | 403.290 | 0.07× |
| print | emoji | 27.595 | 30.627 | 0.90× | 14.952 | 2.05× |
| reflow | ascii | 51.155 | 46.414 | 1.10× | 29.833 | 1.56× |
| reflow | chinese | 59.949 | 54.820 | 1.09× | 30.655 | 1.79× |
| reflow | combining | 47.148 | 43.399 | 1.09× | 58.479 | 0.74× |
| reflow | emoji | 39.934 | 36.813 | 1.08× | 40.957 | 0.90× |
| feed | ascii | 14.897 | 1.488 | 10.01× | 0.517 | 2.88× |
| feed | chinese | 22.969 | 10.599 | 2.17× | 455.221 | 0.02× |
| feed | combining | 31.005 | 28.260 | 1.10× | 424.773 | 0.07× |
| feed | emoji | 32.570 | 30.399 | 1.07× | 17.923 | 1.70× |
| stream | ascii | 156.583 | 57.349 | 2.73× | 6.306 | 9.09× |
| stream | chinese | 134.491 | 83.300 | 1.61× | 9.455 | 8.81× |
| stream | combining | 476.665 | 407.896 | 1.17× | 501.901 | 0.81× |
| stream | emoji | 509.672 | 506.101 | 1.01× | 744.528 | 0.68× |
| stream_styled | ascii | 225.466 | 69.691 | 3.24× | 8.354 | 8.34× |
| stream_styled | chinese | 169.992 | 91.279 | 1.86× | 37.203 | 2.45× |
| stream_styled | combining | 635.924 | 493.403 | 1.29× | 521.173 | 0.95× |
| stream_styled | emoji | 663.016 | 661.051 | 1.00× | 771.199 | 0.86× |
| scalar | ascii | 1.442 | 1.526 | 0.94× | 1.320 | 1.16× |
| scalar | chinese | 1.434 | 1.486 | 0.96× | 1.318 | 1.13× |
| scalar | combining | 1.461 | 1.539 | 0.95× | 1.292 | 1.19× |
| scalar | emoji | 1.433 | 1.488 | 0.96× | 1.299 | 1.15× |
| read | ascii | 1.896 | 1.969 | 0.96× | 1.957 | 1.01× |
| read | chinese | 2.006 | 1.972 | 1.02× | 1.939 | 1.02× |
| read | combining | 2.589 | 2.734 | 0.95× | 5.889 | 0.46× |
| read | emoji | 2.851 | 2.820 | 1.01× | 2.422 | 1.16× |
| clone | ascii | 10.404 | 10.821 | 0.96× | 6.366 | 1.70× |
| clone | chinese | 11.544 | 11.585 | 1.00× | 6.547 | 1.77× |
| clone | combining | 12.392 | 12.555 | 0.99× | 17.871 | 0.70× |
| clone | emoji | 11.578 | 11.523 | 1.00× | 10.109 | 1.14× |
| width | ascii | 0.473 | 0.480 | 0.99× | 0.343 | 1.40× |
| width | chinese | 0.471 | 0.492 | 0.96× | 0.341 | 1.44× |
| width | combining | 0.432 | 0.449 | 0.96× | 0.427 | 1.05× |
| width | emoji | 0.323 | 0.334 | 0.97× | 0.322 | 1.04× |

| Mixed workload | Delivery | Before µs | After µs | Speedup |
| --- | --- | ---: | ---: | ---: |
| chunked_feed_mixed | whole | 84.936 | 62.873 | 1.35× |
| chunked_feed_mixed | 7_bytes | 95.365 | 78.000 | 1.22× |
| chunked_feed_mixed | 4_KiB | 84.310 | 62.020 | 1.36× |
| chunked_stream_mixed | whole | 348.392 | 240.529 | 1.45× |
| chunked_stream_mixed | 7_bytes | 384.482 | 299.843 | 1.28× |
| chunked_stream_mixed | 4_KiB | 350.149 | 241.958 | 1.45× |

The parser now delivers borrowed ASCII and valid UTF-8 runs to the terminal.
Eligible spans share cursor, page and style work across multiple cells.
Scalar printing still handles wrapping, grapheme joins and complex destination
cells. Insert mode, disabled autowrap, legacy character mappings and hyperlinks
retain scalar handling; UTF-8 batching also falls back for horizontal margins.
Both `feed` and `feed_with_handler` preserve input order and chunk continuation.

Three shared changes reduce scrolling costs: blank rows initialize cells
directly instead of cloning their resource-bearing type; row accounting adds
only present payloads and creates a hyperlink deduplication set only when
needed; resource-page lookup searches from the active end of history. Cell
size remains 56 bytes, and owned-history charges retain their previous
saturating arithmetic and sharing rules. Ghostty production code is unchanged.

The full run records 8–13% slower direct printing and smaller regressions in
several scan/read controls. A second 50-sample run measured the final binary
first, then the before binary, with the same settings. The larger Unicode-print
slowdowns did not repeat; ASCII printing and combining reads remained about 4%
slower. The complete first-run table is retained above, and the reverse-order
controls below show the timing sensitivity. No scan/read speedup is claimed.
Positive changes here mean the final binary is slower.

| Reverse-order control | Corpus | Before µs | After µs | Change |
| --- | --- | ---: | ---: | ---: |
| print | ascii | 11.253 | 11.684 | +3.8% |
| print | chinese | 17.885 | 18.087 | +1.1% |
| print | combining | 26.487 | 26.036 | -1.7% |
| print | emoji | 28.241 | 27.648 | -2.1% |
| scalar | ascii | 1.499 | 1.510 | +0.7% |
| read | combining | 2.638 | 2.735 | +3.7% |
| width | ascii | 0.475 | 0.481 | +1.3% |

Adding the Unicode run writer made LLVM outline the shared grapheme check;
keeping that check inline reduced the resulting Chinese-print slowdown.
Future comparisons should retain the complete harness as well as the
production revision, compiler and release settings: adding benchmarks can
also change generated code in unchanged operations.

Validation passed 294 VT tests, 12 parser tests and all 78 Rust/native benchmark
smoke cases. The scalar-reference table covers 2,088 delivery variants across
Unicode, terminal modes, margins, overwrites and host-handler paths, with
additional checks for ignored scalars after public cursor edits, effect order
and snapshot continuation. Native differential testing passed 6,966 of 6,969
comparisons. The three failures are the previously documented whole/scalar/
chunked variants of `pages/graphemes/wrap/3/1/1/alternate`: Rustty preserves a
ZWJ that Ghostty drops when a widening grapheme wraps in a one-row alternate
screen. Strict parser/VT Clippy, formatting and the app check also passed.

The local results are in `target/criterion-batching/`: `comparison.json` and
`comparison.md` contain the table; `metadata.json` identifies revisions,
toolchains, binary SHA-256 hashes and validation logs. Rust estimates use the
`matched-before` and `final` labels, from `matched-before.log` and
`matched-after.log`. Native `final` estimates come from the earlier `final.log`
run. `reverse-controls.json`/`reverse-controls.md` retain the confirmation
table. The final profiling captures and summaries are under `profiles/`.

To repeat the Rust comparison with the retained executables, keep builds and
other benchmarks stopped during both runs. The fresh directory below avoids
replacing the recorded results. Build any new candidate before either run and
retain this same 42-case harness:

```sh
CRITERION_HOME="$PWD/target/criterion-batching-repeat" \
  target/criterion-batching/primitives-before --bench '^rustty/' \
  --save-baseline matched-before \
  --sample-size 50 --warm-up-time 0.5 --measurement-time 2
CRITERION_HOME="$PWD/target/criterion-batching-repeat" \
  target/criterion-batching/primitives-final --bench '^rustty/' \
  --baseline matched-before \
  --sample-size 50 --warm-up-time 0.5 --measurement-time 2
```

Final eight-second, nominal 1 kHz profiles used an optimized Rust build with
debug information and frame pointers. ASCII streaming spent 20% of active
samples in `sync_resource_row`, 17% in `scroll_up` (including inlined blank-row
initialization), 17% in `Row::storage_bytes`, and 11% in `print_ascii`. Chinese
streaming spent 31% in `print_utf8`, 15% in row synchronization, 12% in scrolling
and 11% in storage accounting. These are physical-symbol shares, including
inlined work. Row synchronization also scans fresh blank cells; its full cost
is not page lookup. Row lifecycle/accounting and layout calculations therefore
remain candidates for the next plain-stream pass.

The native combining overwrite and Chinese `feed` cliffs still limit those
comparisons. Use bounded `stream` and `stream_styled` results to assess ongoing
output, and the mixed/chunked cases to check fallback and delivery overhead.
Combining and ZWJ-heavy input still needs scalar grapheme work. The reflow
workload remains limited to the active screen; history reflow needs separate
workloads before choosing its next optimization.

## Scrolling follow-up and history reflow, 2026-09-15

This pass compares production code at `1c09810` with `00c1116`, using the same
46-case Rust harness from `263f4c4` for both. The original 42 workloads are
unchanged; four new history-reflow cases retain 256 records across each resize
round trip. The native helper and Ghostty production code are unchanged.

Both Rust binaries were built before the final consecutive before/after runs.
Native measurements then used the retained ReleaseFast helper. All timings
were serial, with builds, tests and profiles stopped, on the Apple M4 Max and
toolchain/release settings documented above: 50 samples, 0.5 seconds of warmup
and 2 seconds of measurement. Values are median microseconds per complete
workload. Speedup is Rustty before/after; Rustty/Ghostty is Rustty after/native.
Native timing changes from earlier tables are not production code gains.
Against this reference, Rustty still takes about 8× as long for plain
ASCII/Chinese streams.

Plain ASCII streaming improves 1.30× and styled ASCII streaming 1.26×;
Chinese improves 1.14× and 1.17× respectively. Combining and emoji stream
medians improve only about 1–2%. Mixed streams improve 1.05–1.06×, and the
active-screen reflow round trips improve 1.03–1.18×. History reflow is essentially
unchanged: these new cases establish a baseline for subsequent scrollback work.

| Operation | Corpus | Before µs | After µs | Speedup | Ghostty µs | Rustty/Ghostty |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| print | ascii | 11.201 | 11.512 | 0.97× | 6.191 | 1.86× |
| print | chinese | 17.828 | 17.809 | 1.00× | 12.053 | 1.48× |
| print | combining | 25.750 | 26.399 | 0.98× | 391.584 | 0.07× |
| print | emoji | 27.474 | 28.004 | 0.98× | 14.924 | 1.88× |
| reflow | ascii | 45.917 | 41.888 | 1.10× | 20.400 | 2.05× |
| reflow | chinese | 54.456 | 52.932 | 1.03× | 21.520 | 2.46× |
| reflow | combining | 42.731 | 37.305 | 1.15× | 47.417 | 0.79× |
| reflow | emoji | 36.408 | 30.727 | 1.18× | 29.828 | 1.03× |
| feed | ascii | 1.472 | 1.476 | 1.00× | 0.496 | 2.97× |
| feed | chinese | 10.363 | 10.356 | 1.00× | 435.823 | 0.02× |
| feed | combining | 28.566 | 29.474 | 0.97× | 397.441 | 0.07× |
| feed | emoji | 30.761 | 30.578 | 1.01× | 17.305 | 1.77× |
| stream | ascii | 56.259 | 43.135 | 1.30× | 5.265 | 8.19× |
| stream | chinese | 79.397 | 69.606 | 1.14× | 8.638 | 8.06× |
| stream | combining | 399.392 | 394.062 | 1.01× | 486.927 | 0.81× |
| stream | emoji | 499.913 | 488.874 | 1.02× | 717.038 | 0.68× |
| stream_styled | ascii | 66.294 | 52.754 | 1.26× | 7.742 | 6.81× |
| stream_styled | chinese | 86.721 | 74.350 | 1.17× | 34.704 | 2.14× |
| stream_styled | combining | 493.137 | 482.367 | 1.02× | 501.022 | 0.96× |
| stream_styled | emoji | 656.609 | 644.300 | 1.02× | 748.537 | 0.86× |
| scalar | ascii | 1.467 | 1.516 | 0.97× | 1.276 | 1.19× |
| scalar | chinese | 1.449 | 1.493 | 0.97× | 1.277 | 1.17× |
| scalar | combining | 1.495 | 1.537 | 0.97× | 1.262 | 1.22× |
| scalar | emoji | 1.440 | 1.475 | 0.98× | 1.273 | 1.16× |
| read | ascii | 1.964 | 1.957 | 1.00× | 1.908 | 1.03× |
| read | chinese | 1.928 | 1.960 | 0.98× | 1.900 | 1.03× |
| read | combining | 2.742 | 2.757 | 0.99× | 5.823 | 0.47× |
| read | emoji | 2.754 | 2.801 | 0.98× | 2.405 | 1.17× |
| clone | ascii | 10.585 | 10.821 | 0.98× | 5.310 | 2.04× |
| clone | chinese | 11.357 | 11.581 | 0.98× | 5.302 | 2.18× |
| clone | combining | 12.347 | 12.583 | 0.98× | 16.371 | 0.77× |
| clone | emoji | 11.388 | 11.563 | 0.98× | 9.225 | 1.25× |
| width | ascii | 0.476 | 0.476 | 1.00× | 0.343 | 1.39× |
| width | chinese | 0.476 | 0.476 | 1.00× | 0.341 | 1.40× |
| width | combining | 0.430 | 0.431 | 1.00× | 0.424 | 1.02× |
| width | emoji | 0.323 | 0.325 | 0.99× | 0.318 | 1.02× |

| Mixed workload | Delivery | Before µs | After µs | Speedup |
| --- | --- | ---: | ---: | ---: |
| chunked_feed_mixed | whole | 61.832 | 61.324 | 1.01× |
| chunked_feed_mixed | 7_bytes | 75.860 | 77.365 | 0.98× |
| chunked_feed_mixed | 4_KiB | 61.084 | 60.723 | 1.01× |
| chunked_stream_mixed | whole | 235.469 | 225.040 | 1.05× |
| chunked_stream_mixed | 7_bytes | 295.193 | 281.826 | 1.05× |
| chunked_stream_mixed | 4_KiB | 237.677 | 223.721 | 1.06× |

| History reflow corpus | Before µs | After µs | Speedup |
| --- | ---: | ---: | ---: |
| ascii | 1557.017 | 1543.292 | 1.01× |
| chinese | 1261.355 | 1265.312 | 1.00× |
| combining | 2163.563 | 2154.903 | 1.00× |
| emoji | 1564.801 | 1566.051 | 1.00× |

The implementation changes are separate commits:

- `0c88873` assigns the known fresh blank row's resource page directly after
  growth/eviction. Existing rows retain ordinary resource transfer, including
  externally supplied hyperlink payloads and page-boundary moves.
- `6794e3e` delays minimum-limit/layout calculations while a row fits existing
  page capacity and the raw line limit is not exceeded. Byte-floor work runs
  only when a byte policy exists. Native floors and recycling order remain
  unchanged, without cached policy state.
- `00c1116` checks active physical row widths once on the ordinary index-scroll
  path. Its two independent mutation paths retain their own widening checks.

Intermediate 30-sample measurements supported each change. The initial
column-scan measurement was slower; a repeated paired run with the same
binaries improved plain ASCII/Chinese streams about 3%. The final table uses
fresh consecutive measurements of the complete before and final binaries.

The scan/read/clone/width and direct-print controls range from 0.4% faster to
3.4% slower in this run. Those small regressions remain in the table; no
control-path speedup is claimed.

All 296 VT tests, strict all-target VT Clippy, formatting, the app check and
all 82 Rust/native benchmark smoke cases passed. The new regression checks
cover fresh-row resource ownership and native line/byte floors. Existing
snapshot/row-shift tests verify narrow restored pages, margins and pins.
Native page/layout and snapshot suites passed 6,582 of 6,585 comparisons.
The three failures remain the whole/scalar/chunked variants of
`pages/graphemes/wrap/3/1/1/alternate`: Rustty preserves the same extra ZWJ.

Final eight-second, nominal 1 kHz profiles used an optimized build with debug
information and frame pointers. Page layout/metadata symbols account for less
than 0.1% of active samples. Remaining ASCII streaming costs include
`scroll_up` at 21% (including inlined blank-row initialization),
`Row::storage_bytes` at 21%, and `sync_resource_row` at 16%. Chinese streaming
spends 37% in `print_utf8`, 13% in scrolling, 13% in storage accounting and 11%
in row synchronization. These are independently normalized physical-symbol
shares; inlined work belongs to its enclosing symbol. Row initialization,
accounting and existing-row synchronization remain the main ASCII targets.

The local artifacts are in `target/criterion-scrolling/`: `comparison.json`
and `comparison.md` contain all 46 rows; `metadata.json` records revisions,
toolchains, SHA-256 hashes and logs. Final Rust estimates use `matched-before`
and `final`; native estimates use `final`. Intermediate `before`, `blank`,
`layout`, `columns` and `column-control-*` runs remain available separately.
The final profiles and summaries are in `profiles/`.

To repeat the Rust comparison with the frozen binaries:

```sh
CRITERION_HOME="$PWD/target/criterion-scrolling-repeat" \
  target/criterion-scrolling/primitives-before --bench '^rustty/' \
  --save-baseline matched-before \
  --sample-size 50 --warm-up-time 0.5 --measurement-time 2
CRITERION_HOME="$PWD/target/criterion-scrolling-repeat" \
  target/criterion-scrolling/primitives-final --bench '^rustty/' \
  --baseline matched-before \
  --sample-size 50 --warm-up-time 0.5 --measurement-time 2
```

Ghostty's combining overwrite and Chinese `feed` cliffs still limit those
ratios as measures of general Unicode throughput. The new history-reflow
workload provides a baseline for preserved scrollback, separately from the
existing active-screen round trip. Cell size remains 56 bytes.

## Row bookkeeping follow-up, 2026-09-16

This pass compares production at `dfcfb9f` with `8e1ae0c`, using the same
54-case harness from `75dc73e`. The original 46 Rust workloads are unchanged;
eight new cases enable the app-default 50 MB owned-history cap. Ghostty
production code and its retained ReleaseFast helper are unchanged.

All builds, tests and profiles finished before the consecutive Rust before,
Rust after and native measurements. The machine and toolchains remain the
Apple M4 Max, macOS 26.7, Rust 1.98.1 and Zig 0.16.0, using the release settings
above. Each case uses 50 samples, 0.5 seconds of warmup and 2 seconds of
measurement. Values below are median microseconds per complete workload;
speedup is Rustty before/after, and Rustty/Ghostty is Rustty after/native.
Changes in native timings from earlier tables are measurement variation,
not Ghostty code gains; Rustty's paired before/after is the optimization measure.

Plain ASCII streaming improves 1.55× and styled ASCII 1.41×; Chinese improves
1.27× and 1.24×. With the host cap enabled, ASCII improves 1.20× and 1.16×,
and Chinese 1.10× and 1.08×. The larger uncapped gains do not apply to the
app's default memory policy. Mixed streams improve 1.08–1.13×. Both reflow
families are essentially unchanged. Plain ASCII/Chinese streams still take
5.06×/5.98× Ghostty's time against this fresh reference.

| Operation | Corpus | Before µs | After µs | Speedup | Ghostty µs | Rustty/Ghostty |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| print | ascii | 11.301 | 11.032 | 1.02× | 6.209 | 1.78× |
| print | chinese | 17.525 | 18.050 | 0.97× | 12.200 | 1.48× |
| print | combining | 26.420 | 26.139 | 1.01× | 394.942 | 0.07× |
| print | emoji | 27.508 | 27.792 | 0.99× | 14.867 | 1.87× |
| reflow | ascii | 42.265 | 42.247 | 1.00× | 27.588 | 1.53× |
| reflow | chinese | 53.377 | 53.064 | 1.01× | 29.441 | 1.80× |
| reflow | combining | 37.640 | 37.414 | 1.01× | 54.735 | 0.68× |
| reflow | emoji | 31.143 | 31.135 | 1.00× | 32.954 | 0.94× |
| feed | ascii | 1.483 | 1.482 | 1.00× | 0.522 | 2.84× |
| feed | chinese | 10.362 | 10.584 | 0.98× | 440.006 | 0.02× |
| feed | combining | 28.638 | 29.171 | 0.98× | 406.875 | 0.07× |
| feed | emoji | 30.237 | 30.164 | 1.00× | 17.510 | 1.72× |
| stream | ascii | 44.160 | 28.493 | 1.55× | 5.631 | 5.06× |
| stream | chinese | 68.251 | 53.534 | 1.27× | 8.947 | 5.98× |
| stream | combining | 385.624 | 370.951 | 1.04× | 491.737 | 0.75× |
| stream | emoji | 491.643 | 474.636 | 1.04× | 729.671 | 0.65× |
| stream_styled | ascii | 53.467 | 37.983 | 1.41× | 8.011 | 4.74× |
| stream_styled | chinese | 74.901 | 60.260 | 1.24× | 34.622 | 1.74× |
| stream_styled | combining | 483.362 | 466.102 | 1.04× | 508.918 | 0.92× |
| stream_styled | emoji | 642.866 | 632.449 | 1.02× | 759.897 | 0.83× |
| scalar | ascii | 1.543 | 1.558 | 0.99× | 1.285 | 1.21× |
| scalar | chinese | 1.489 | 1.504 | 0.99× | 1.290 | 1.17× |
| scalar | combining | 1.558 | 1.572 | 0.99× | 1.276 | 1.23× |
| scalar | emoji | 1.479 | 1.491 | 0.99× | 1.281 | 1.16× |
| read | ascii | 2.301 | 2.305 | 1.00× | 1.897 | 1.21× |
| read | chinese | 2.659 | 2.676 | 0.99× | 1.910 | 1.40× |
| read | combining | 2.965 | 2.952 | 1.00× | 5.830 | 0.51× |
| read | emoji | 3.116 | 3.085 | 1.01× | 2.430 | 1.27× |
| clone | ascii | 11.008 | 10.958 | 1.00× | 6.035 | 1.82× |
| clone | chinese | 11.635 | 11.720 | 0.99× | 5.836 | 2.01× |
| clone | combining | 12.783 | 12.732 | 1.00× | 17.847 | 0.71× |
| clone | emoji | 11.713 | 12.155 | 0.96× | 9.952 | 1.22× |
| width | ascii | 0.478 | 0.480 | 1.00× | 0.344 | 1.40× |
| width | chinese | 0.477 | 0.476 | 1.00× | 0.342 | 1.39× |
| width | combining | 0.432 | 0.432 | 1.00× | 0.427 | 1.01× |
| width | emoji | 0.326 | 0.326 | 1.00× | 0.318 | 1.03× |

| Mixed workload | Delivery | Before µs | After µs | Speedup |
| --- | --- | ---: | ---: | ---: |
| chunked_feed_mixed | whole | 61.775 | 60.705 | 1.02× |
| chunked_feed_mixed | 7_bytes | 75.956 | 76.638 | 0.99× |
| chunked_feed_mixed | 4_KiB | 60.681 | 60.622 | 1.00× |
| chunked_stream_mixed | whole | 230.916 | 204.667 | 1.13× |
| chunked_stream_mixed | 7_bytes | 286.973 | 265.795 | 1.08× |
| chunked_stream_mixed | 4_KiB | 223.823 | 204.679 | 1.09× |

| History reflow corpus | Before µs | After µs | Speedup |
| --- | ---: | ---: | ---: |
| ascii | 1561.573 | 1543.514 | 1.01× |
| chinese | 1272.017 | 1263.198 | 1.01× |
| combining | 2164.458 | 2157.904 | 1.00× |
| emoji | 1568.128 | 1564.365 | 1.00× |

| Capped workload | Corpus | Before µs | After µs | Speedup |
| --- | --- | ---: | ---: | ---: |
| stream_memory_capped | ascii | 44.246 | 36.844 | 1.20× |
| stream_memory_capped | chinese | 68.561 | 62.132 | 1.10× |
| stream_memory_capped | combining | 388.913 | 383.839 | 1.01× |
| stream_memory_capped | emoji | 487.891 | 480.249 | 1.02× |
| stream_styled_memory_capped | ascii | 53.632 | 46.322 | 1.16× |
| stream_styled_memory_capped | chinese | 75.107 | 69.487 | 1.08× |
| stream_styled_memory_capped | combining | 485.286 | 477.042 | 1.02× |
| stream_styled_memory_capped | emoji | 647.402 | 637.311 | 1.02× |

The implementation changes are separate commits:

- `48f8103` checks resource ownership across active page ranges before running
  per-row synchronization. Ordinary history insertion preserves those owners;
  changed or unowned rows retain the existing directional transfer path.
  Cursor synchronization still runs, including resource-induced page splits.
- `8e1ae0c` stops scanning incoming and evicted payloads when no host-memory cap
  consumes the total. Uncapped `history_bytes()` and its JSON field now compute
  the total on demand, taking time proportional to retained cells. Capped
  screens keep incremental charges. Enabling a cap already recounts current
  rows, including externally replaced rows. No per-row cache was introduced.

The original feed, print, read, scalar, clone and width controls range from
2.4% faster to 3.8% slower in this paired run. These differences remain in the
table; no broad control-path speedup is claimed. Initial 30-sample measurements
supported both changes. One capped-ASCII sample after the accounting change
was slower; a 50-sample ABBA repeat was effectively unchanged at 36.1–36.8 µs.
The final table uses fresh consecutive measurements of the full pass.

All 298 VT tests, strict all-target VT Clippy, formatting, the app check and
90 Rust/native benchmark smoke cases passed. New regressions cover unowned
public hyperlinks during scrolling, page growth, payload accounting across
cap transitions, eviction and JSON restoration. Native page/layout and both
snapshot suites passed 6,582 of 6,585 comparisons, with no selected-suite
coverage gaps. The same three failures remain the whole/scalar/chunked
variants of `pages/graphemes/wrap/3/1/1/alternate`: Rustty retains an extra ZWJ.

Final eight-second, nominal 1 kHz profiles use optimized code with debug
information and frame pointers. Uncapped ASCII and Chinese streaming recorded
no active samples in `Row::storage_bytes` or `sync_resource_row`. Capped ASCII
still spends 27% in payload accounting. Remaining uncapped ASCII samples
include scrolling/blank-row initialization at 32%, printing at 22%, and
history-prefix disposal at 10%. Chinese spends 49% in `print_utf8` and 16% in
scrolling. These are independently normalized physical-symbol shares;
inlined work belongs to the enclosing symbol. Cell size remains 56 bytes.

The local artifacts are in `target/criterion-row-bookkeeping/`:
`comparison.md` and `comparison.json` contain all 54 Rust rows and 36 native
references; `metadata.json` records revisions, hashes, toolchains and logs.
Final labels are `matched-before` and `final`; intermediate labels and the
balanced capped repeat are retained separately. Frozen executables are
`primitives-before`, `primitives-sync`, `primitives-accounting`,
`primitives-final` and `vt-primitives`; final profiles and summaries are under
`profiles/`. Ghostty's Chinese-feed and combining-overwrite cliffs remain,
so those ratios are not general Unicode-throughput comparisons.

To repeat the complete comparison with the frozen binaries:

```sh
CRITERION_HOME="$PWD/target/criterion-row-bookkeeping-repeat" \
  target/criterion-row-bookkeeping/primitives-before --bench '^rustty/' \
  --save-baseline matched-before \
  --sample-size 50 --warm-up-time 0.5 --measurement-time 2
CRITERION_HOME="$PWD/target/criterion-row-bookkeeping-repeat" \
  target/criterion-row-bookkeeping/primitives-final --bench '^rustty/' \
  --save-baseline final \
  --sample-size 50 --warm-up-time 0.5 --measurement-time 2
GHOSTTY_PRIMITIVES_BIN="$PWD/target/criterion-row-bookkeeping/vt-primitives" \
  CRITERION_HOME="$PWD/target/criterion-row-bookkeeping-repeat" \
  target/criterion-row-bookkeeping/primitives-final --bench '^ghostty/' \
  --save-baseline final \
  --sample-size 50 --warm-up-time 0.5 --measurement-time 2
```

## Ordinary grapheme boundary fast exit, 2026-09-16

`4b7a41d` returns immediately for two ordinary grapheme classes after the
existing state normalization. CJK ideographs take this path; active joining
states still reset normally, and every nonordinary pair retains the original
rules. The change adds four production lines and one regression test.

This table compares `8cba1ec` with `4b7a41d`, using the unchanged 54-case Rust
harness from `75dc73e` and the same 36 native workloads. Ghostty production
code and its ReleaseFast helper are unchanged. Machine, toolchains and release
flags remain those documented above: Apple M4 Max, macOS 26.7, Rust 1.98.1,
Zig 0.16.0, thin LTO/one codegen unit for Rust, and Criterion 0.8.2.

The initial consecutive Rust runs had severe timing variation: several
baseline interquartile ranges exceeded their medians, and plain combining
streaming measured 1,039 µs instead of the repeated roughly 375–390 µs. All
initial data are retained but excluded from the table. Every Rust case was
rerun as an adjacent before/after pair, with the first version alternating
between table rows. This alternates order across workloads, not within each
workload. Each run has 50 samples, 0.5 seconds of warmup and a 2-second target
measurement. The native column retains this pass's earlier serial native run.
Builds and tests were stopped throughout timing. Medians are microseconds per
complete workload; speedup is before/after and Rustty/Ghostty is after/native.

Chinese streaming improves 1.10× plain and 1.08× styled. With the app-default
host-memory cap enabled, those gains are 1.12× and 1.07×. Chinese feed improves
1.16× and direct print 1.04×. The remaining plain Chinese-stream gap is 5.40×
against the native reference. Combining streams are essentially unchanged;
reflow gains are negligible and retained-history reflow is up to about 1.5%
slower. Non-Chinese workloads range from 4.6% faster to 2.9% slower; no broad
speedup outside Chinese workloads is claimed.

| Operation | Corpus | Before µs | After µs | Speedup | Ghostty µs | Rustty/Ghostty |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| print | ascii | 11.670 | 11.129 | 1.05× | 6.224 | 1.79× |
| print | chinese | 17.588 | 16.856 | 1.04× | 12.063 | 1.40× |
| print | combining | 26.425 | 26.353 | 1.00× | 405.483 | 0.06× |
| print | emoji | 28.161 | 28.014 | 1.01× | 15.048 | 1.86× |
| reflow | ascii | 42.283 | 42.088 | 1.00× | 24.087 | 1.75× |
| reflow | chinese | 53.490 | 53.192 | 1.01× | 26.863 | 1.98× |
| reflow | combining | 37.536 | 37.533 | 1.00× | 51.446 | 0.73× |
| reflow | emoji | 31.537 | 31.037 | 1.02× | 32.648 | 0.95× |
| feed | ascii | 1.491 | 1.486 | 1.00× | 0.499 | 2.98× |
| feed | chinese | 10.366 | 8.958 | 1.16× | 437.762 | 0.02× |
| feed | combining | 28.726 | 28.252 | 1.02× | 411.084 | 0.07× |
| feed | emoji | 30.778 | 30.746 | 1.00× | 17.171 | 1.79× |
| stream | ascii | 28.459 | 28.839 | 0.99× | 5.558 | 5.19× |
| stream | chinese | 53.544 | 48.661 | 1.10× | 9.007 | 5.40× |
| stream | combining | 376.423 | 377.295 | 1.00× | 502.206 | 0.75× |
| stream | emoji | 485.012 | 480.382 | 1.01× | 755.548 | 0.64× |
| stream_styled | ascii | 38.617 | 38.362 | 1.01× | 7.960 | 4.82× |
| stream_styled | chinese | 59.638 | 55.459 | 1.08× | 35.888 | 1.55× |
| stream_styled | combining | 473.214 | 473.976 | 1.00× | 517.477 | 0.92× |
| stream_styled | emoji | 634.184 | 636.170 | 1.00× | 774.701 | 0.82× |
| scalar | ascii | 1.564 | 1.562 | 1.00× | 1.312 | 1.19× |
| scalar | chinese | 1.502 | 1.499 | 1.00× | 1.305 | 1.15× |
| scalar | combining | 1.578 | 1.568 | 1.01× | 1.298 | 1.21× |
| scalar | emoji | 1.495 | 1.488 | 1.00× | 1.304 | 1.14× |
| read | ascii | 2.310 | 2.314 | 1.00× | 1.942 | 1.19× |
| read | chinese | 2.667 | 2.702 | 0.99× | 1.948 | 1.39× |
| read | combining | 2.957 | 2.957 | 1.00× | 5.946 | 0.50× |
| read | emoji | 3.112 | 3.203 | 0.97× | 2.442 | 1.31× |
| clone | ascii | 11.162 | 10.779 | 1.04× | 6.074 | 1.77× |
| clone | chinese | 11.796 | 11.670 | 1.01× | 5.913 | 1.97× |
| clone | combining | 12.996 | 12.582 | 1.03× | 17.472 | 0.72× |
| clone | emoji | 11.862 | 11.704 | 1.01× | 9.932 | 1.18× |
| width | ascii | 0.474 | 0.479 | 0.99× | 0.347 | 1.38× |
| width | chinese | 0.478 | 0.477 | 1.00× | 0.344 | 1.39× |
| width | combining | 0.436 | 0.437 | 1.00× | 0.430 | 1.02× |
| width | emoji | 0.325 | 0.327 | 0.99× | 0.322 | 1.02× |

| Mixed workload | Delivery | Before µs | After µs | Speedup |
| --- | --- | ---: | ---: | ---: |
| chunked_feed_mixed | whole | 61.416 | 60.337 | 1.02× |
| chunked_feed_mixed | 7_bytes | 76.682 | 76.483 | 1.00× |
| chunked_feed_mixed | 4_KiB | 61.589 | 60.398 | 1.02× |
| chunked_stream_mixed | whole | 210.707 | 208.985 | 1.01× |
| chunked_stream_mixed | 7_bytes | 270.588 | 272.591 | 0.99× |
| chunked_stream_mixed | 4_KiB | 215.510 | 216.774 | 0.99× |

| History reflow corpus | Before µs | After µs | Speedup |
| --- | ---: | ---: | ---: |
| ascii | 1586.294 | 1599.956 | 0.99× |
| chinese | 1296.504 | 1315.405 | 0.99× |
| combining | 2185.228 | 2198.592 | 0.99× |
| emoji | 1570.318 | 1575.818 | 1.00× |

| Capped workload | Corpus | Before µs | After µs | Speedup |
| --- | --- | ---: | ---: | ---: |
| stream_memory_capped | ascii | 38.047 | 37.814 | 1.01× |
| stream_memory_capped | chinese | 63.852 | 56.869 | 1.12× |
| stream_memory_capped | combining | 383.564 | 381.691 | 1.00× |
| stream_memory_capped | emoji | 493.341 | 485.746 | 1.02× |
| stream_styled_memory_capped | ascii | 48.739 | 47.431 | 1.03× |
| stream_styled_memory_capped | chinese | 68.541 | 63.889 | 1.07× |
| stream_styled_memory_capped | combining | 488.524 | 488.111 | 1.00× |
| stream_styled_memory_capped | emoji | 662.031 | 671.134 | 0.99× |

Across selected Rust runs, the median ratio of interquartile range to
sample median is 1.34%. Plain/styled Chinese streams are around 0.7–1.5%; capped
Chinese's baseline is noisier at 6.75%, so its 1.12× estimate is less precise.
Native reflow dispersion reaches 13.30%. Small control and reflow movements
remain observations rather than attributed optimization gains.

The first candidate checked ordinary classes only while the joining state was
idle, before normalization. A balanced ABBA repeat supported its Chinese gain
but showed a small combining slowdown. Moving the ordinary-pair check after
normalization preserved the gain with less fallback overhead in subsequent
measurements. Both variants remain as frozen executables and separately
labelled estimates; the table selects only the retained implementation.

All 299 VT tests, strict all-target VT Clippy, formatting, the app check and
90 Rust/native benchmark smoke cases pass. The new persistent regression
covers every state byte on ordinary pairs, plus prepend and combining
fallbacks. An independent executable compares the previous and retained
functions for all 17 generated grapheme classes paired with all 17 classes
and all 256 state bytes: all 73,984 results and outgoing states match. Since
the function's only character-derived inputs are those classes and the
property tables are unchanged, this exhaustively checks its behavior.

Artifacts are in `target/criterion-grapheme-fast-exit/`. `comparison.md` and
`comparison.json` contain the selected 54-row comparison. Rust estimates
come from `paired/`, native estimates from the root, with `matched-before`
and `final` labels. `initial-comparison.*` and the original root Rust
estimates retain the noisy consecutive run. `metadata.json` identifies all
binaries, revisions, settings, selected logs and trial variants. The exact
Rust measurement schedule is in `paired-run.py`, and `report.py` selects the
final estimates. Frozen binaries include `primitives-before`,
`primitives-fast-exit` (the first variant), `primitives-normalized`,
`primitives-final` (identical to normalized), and `vt-primitives`. The
independent checker and reference source are retained as `equivalence.rs`
and `unicode-reference.rs`.

The native column still includes the known Chinese-feed and combining-overwrite
performance cliffs. Native timing differences from earlier tables are not
code gains. Cell size remains 56 bytes; no property cache or cell-layout
change was introduced.
