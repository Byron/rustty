# macOS rendering experiments

Metal's public presentation API does not accept a dirty rectangle for a
window drawable. Scissoring can reduce an application's drawing, but it does
not tell WindowServer that the rest of a presented drawable is unchanged.
Separate retained Core Animation layers let the application update a smaller
surface; the compositor decides how to process that change. See Apple's
[`MTLDrawable`](https://developer.apple.com/documentation/metal/mtldrawable) and
[`CAMetalLayer`](https://developer.apple.com/documentation/quartzcore/cametallayer)
APIs.

## Opaque Metal backbuffer, 2026-10-27

The opacity experiment is inconclusive for Rustty itself. Pooled median frame
wall time changed from 0.668 to 0.723 ms for cached redraws (slower), and from
1.279 to 1.066 ms for single-cell updates (faster), while process CPU results
were mixed. This does not establish a repeatable application-side benefit.
Rustty keeps its production backbuffer transparency support unchanged.

The frozen candidate changed only the `support_transparent_backbuffer` argument
to `egui_wgpu::winit::Painter::new` from `true` to `false`. Both variants used
opaque default application settings; this did not test transparent windows.
Rustty already marks `NSWindow` opaque at `background-opacity = 1`; the
experiment changed the separate GPU backbuffer flag.

Ghostty also supports transparency: [`background-opacity`](../../src/config/Config.zig)
defaults to `1.0` (opaque), while values such as `0.9` make the background
translucent. Its macOS [window implementation](../../macos/Sources/Features/Terminal/Window%20Styles/TerminalWindow.swift)
marks the native window opaque when transparency and glass blur are inactive.
Supporting transparent windows does not require making every native window
nonopaque. Ghostty production code was inspected only.
[Saved evidence](benchmarks/render-2026-10-27/opacity.json) preserves the
executable hashes, controls, per-run summaries, and every WindowServer sample.

Measurements used an Apple M4 Max on macOS 27.0.1, the workspace release profile,
Menlo 13 pt, a 1200×850 physical-pixel window at scale 2, and one disposable
`/bin/sleep` session. Each process warmed 50 frames and measured 50, with at
least 50 ms between frames. Each workload ran baseline → opaque → opaque →
baseline, serially, with other agent builds and tests stopped. Exact compiler
version and additional compiler flags were not recorded.

All eight included reports identify the largest non-primary display,
`Monitor #41053`, at physical position `[-3376, 996]` (logical `[-1688, 498]`).
The harness verified that every warmup and sampled frame encoded a terminal
render pass. This matters because egui can otherwise skip an occluded surface
while the application's frame counter still advances.

| Workload | Order | WindowServer CPU %, baseline → opaque | App CPU %, baseline → opaque | Median frame wall ms, baseline → opaque |
| --- | --- | ---: | ---: | ---: |
| Cached redraw | Forward | 33.58 → 21.62 | 3.26 → 3.56 | 0.479 → 0.742 |
| Cached redraw | Reverse | 29.56 → 31.42 | 3.48 → 3.43 | 0.778 → 0.664 |
| Single-cell update | Forward | 21.90 → 19.90 | 4.55 → 4.45 | 1.345 → 1.469 |
| Single-cell update | Reverse | 20.52 → 15.96 | 4.16 → 5.20 | 1.136 → 0.519 |

CPU percentages use one core as 100%. App CPU covers the 50 measured frames.
WindowServer CPU is the mean of one-second `top` samples over the entire
process, including startup, after discarding the initial zero sample. It
includes unrelated desktop activity and is not aligned to the app's measured
interval. Each reported WindowServer mean contains only five samples.
The opaque single-cell reverse run also had a worse frame-wall p95
(8.186 ms versus 1.792 ms), so its lower median alone is misleading.

GPU completion, scanout, and power were not measured. An earlier separate
Metal trace confirmed real baseline GPU execution, but it was not a paired
GPU comparison. Earlier attempts with inconsistent placement or unverified
rendering are excluded. The first placement-controlled single-cell set was
also excluded after its final baseline failed the render-pass check; the table
uses a separate, complete four-run confirmation.

### Reproduction

Build and freeze both variants before running the comparison. Use identical
source, toolchain, flags, app resources, and harness controls except for the
one backbuffer argument. Keep the test display uncovered and require
`terminal_render_passes_verified = true` plus matching `monitor`,
`outer_position`, `scale_factor`, and `size_pixels` in every report.

```sh
python3 test/rustty/frame_compare.py --kind app \
  --before /path/to/baseline/Rustty.app/Contents/MacOS/rustty \
  --after /path/to/opaque/Rustty.app/Contents/MacOS/rustty \
  --case cached_redraw --case status_update \
  --output /tmp/rustty-opacity-comparison
```

`frame_compare.py` collects app timings and hashes. For these measurements,
temporary executable wrappers also ran the following sampler beside each
app process, stopped it when that process exited, and saved separate logs:

```sh
top -l 0 -s 1 -pid "$(pgrep -x WindowServer)" -stats pid,command,cpu,time
```

The local artifact root is `/tmp/rustty-render-experiment/controlled/`:
`secondary-display-results/` contains the accepted cached-redraw set;
`status-update-confirmation/` contains the accepted single-cell set;
`wrappers/` contains the sampler and display/render assertions. Full per-frame
reports and logs remain there; the committed JSON is the durable summary.

The measured opacity binaries still activated their test windows. A subsequent
debug preflight verified the nonactivating smoke startup: all 100 frames passed
on the same display, and Zed stayed frontmost before, during all 29 polls
approximately 200 ms apart, and after the run. This is a focus check, not a new
performance measurement. Its local evidence is
`nonactivation-preflight/focus.json` under the same artifact root.

A later repeated-launch check exposed winit's `set_visible(true)` making the
window key despite `with_active(false)`. Visible macOS smoke startup now uses
the existing nonactivating `preview_window` path (`NSWindow.orderFront`).
The [final focus check](benchmarks/render-2026-10-27/focus.json) kept the user's
Rustty instance frontmost throughout a 30-second cursor sample and all external
focus polls. This instrumented check is excluded from performance comparisons.

## Retained cursor layer, 2026-10-27

The cursor experiment removed all main-window rendering during the idle blink
samples. Across two 30-second samples per variant, process CPU averaged
**0.389% → 0.036% of one core**, about **91% lower**. Main terminal render passes
and pane preparations fell from **102 to zero**. Both orders improved, although
baseline CPU varied by about a factor of two. The absolute CPU saving is small;
this is an idle terminal workload.

The cursor layer is enabled by default on macOS. `RUSTTY_CURSOR_LAYER=0`
disables it for comparisons or troubleshooting. Windows' GPU and software
renderers are unchanged.
[Saved evidence](benchmarks/render-2026-10-27/cursor.json) contains every result,
the binary hash, build details, and hashes of the measured source files.

Rustty retains a normal cursor-on frame, then waits for a matching cursor-off
frame. The latter becomes the window's retained base; a separate opaque
`CAMetalLayer` holds the cursor-on patch. Blinks only change that layer's
visibility. No extra terminal preparation is needed to activate it. The patch
includes the cursor row's foreground to preserve ligatures and glyph overhang.
Here it was 1097×31 pixels, 3.3% of the 1200×850 window.

Ordinary redraws retire the patch with a Core Animation transaction tied to
successful main-frame presentation. Synchronized output preserves the visible
cursor phase. Captures use ordinary rendering. The first implementation handled
one visible opaque pane without images or blinking text; overlapping UI, IME
composition, transitions, and unsupported geometry use ordinary rendering.

### Release comparison

One frozen executable ran baseline → layer → layer → baseline, serially, on
the same M4 Max and secondary display as the opacity experiment. Every run
settled for three seconds, then sampled 30 seconds with unchanged terminal
content and no native keyboard focus. Menlo 13, scale 2, position
`[-3376, 996]`, and the Metal adapter matched across all four runs. No builds,
other tests, or computer-use inspection ran during these measurements.

| Run | Variant | App CPU %, one core | Main terminal passes | Pane prepares | Layer visibility changes |
| --- | --- | ---: | ---: | ---: | ---: |
| 01-A | Baseline | 0.521 | 51 | 51 | 0 |
| 02-B | Cursor layer | 0.036 | 0 | 0 | 51 |
| 03-B | Cursor layer | 0.036 | 0 | 0 | 50 |
| 04-A | Baseline | 0.256 | 51 | 51 | 0 |

Each layer sample retained its initial patch without another activation.
CPU excludes startup and the subsequent lifecycle checks. Render-pass counters
verify actual terminal encoding, not merely redraw requests. These measurements
establish reduced Rustty rendering work, which is the scope of this experiment.

Built with Rust 1.99.1 for `aarch64-apple-darwin`, the workspace release profile
(thin LTO, one codegen unit), and no extra Rust flags. Full logs and PNG capture
fixtures remain under `/tmp/rustty-render-experiment/cursor-abba/`.

### Validation and reproduction

Three cursor integration tests and three renderer metadata tests passed,
including pixel-identical patch reconstruction across cursor styles, glyph
overhang, clipping, invalidation, and UI overlap. Two existing pane-cache tests,
macOS Clippy, and a Windows ARM64 compilation check also passed.

Both native layer runs passed content retirement, cursor-inclusive capture,
capture equivalence with ordinary rendering, synchronized-output phase
preservation, popup retirement, and synchronized-output release checks.
Native layer pixel alignment and seamless visual handoff still need direct
visual validation; the capture checks exercise the ordinary fallback path.
The existing eligibility checks and ordinary-rendering fallbacks remain active.
Native smoke checks also verified startup with the flag unset (51 layer toggles,
zero main paints or preparations, all lifecycle checks passing) and with `0`
(51 ordinary paints). Both runs preserved native keyboard focus.

Build once, freeze the executable, leave the chosen display uncovered, and use
an empty output directory:

```sh
cargo build -p rustty-app --bin rustty --release --offline
cp target/release/rustty /tmp/rustty-cursor-benchmark
python3 test/rustty/cursor_compare.py \
  --binary /tmp/rustty-cursor-benchmark \
  --output /tmp/rustty-cursor-comparison
```

The script verifies the binary hash before and after each run, checks matching
display controls and focus, and saves reports, logs, and an aggregate summary.
Baseline runs set `RUSTTY_CURSOR_LAYER=0`; layer runs use the macOS default
with the variable unset. The saved measurements above used the earlier opt-in
build with the same layer implementation.

## Split-pane follow-up, 2026-10-27

The user supplied `russty-somewhat-busy-with-focus.trace` with about twelve
visible splits in one window. Its 8.229-second recording contains 112 running
samples, each weighted at 1 ms: 52 ms on the main thread, 40 ms on generic worker
threads, and 20 ms on the native event thread. Of the worker samples, 22 ms have
no exported backtrace. These are statistical sample weights, not independent
CPU accounting. [Saved trace summary](benchmarks/render-2026-10-27/focus-trace.json).

The main thread shows pointer hit testing, layout traversal, drawing, and
workspace/dashboard updates. `draw_frame` accounts for 10 ms; dashboard
synchronization accounts for 7 ms, including 6 ms in `Dashboard::update` and
4 ms in `reconcile_deck_positions`. The nested weights overlap. There is no
sampled Stream Deck discovery/open retry loop.

All running samples occur between 1.045 and 3.157 seconds. The same process
remains observable in the event trace at 4.982 seconds. Sampled drawing groups
are about 558, 671, and 713 ms apart, which does not establish a regular 600 ms
blink cadence. This recording therefore shows event-related work but does not
measure steady idle blinking. The initial blocked-thread snapshot contains
59 PTY read/write/wait triads; none have running samples. It suggests that
workspace-wide bookkeeping covers more sessions than the visible splits,
without identifying their visibility or establishing a thread leak.

Source inspection found two independent costs:

- A single focused cursor invalidates the combined terminal frame on every
  blink when multiple panes are visible. The other panes reuse their prepared
  content, but Rustty still recombines and uploads the whole window. The initial
  cursor-layer eligibility check required exactly one visible pane.
- Every event-loop turn reaching `about_to_wait` rebuilds dashboard state,
  including pane IDs, labels, groups, and layout positions. This scales with the
  workspace and runs even when no report changed. It remains a separate
  optimization opportunity; the cursor change does not remove input-driven
  dashboard updates.

The cursor layer now selects the focused pane inside a split layout. It keeps
the existing opacity, content, transition, and UI guards, and rejects any other
pane whose visible region intersects the cursor patch. Other panes' output and
image animation continue to request ordinary frames, which retire the patch.

### Split-pane validation

The release comparison used twelve nonoverlapping panes in a 1200×850 physical
pixel window at scale 2, Menlo 13 pt, and one blinking cursor in the last
composed pane. Eleven other cursors remained steady. All twelve terminal
generations remained unchanged throughout each sample. The 269×32-pixel patch
covered about 0.84% of the window. All successful runs used the largest
non-primary monitor, `Monitor #41053`, at `[-3376, 996]`, without native keyboard
focus. Stream Deck was disabled in the fixture.

| Run | Variant | CPU %, one core | Main terminal passes | Pane prepares | Layer visibility changes |
| --- | --- | ---: | ---: | ---: | ---: |
| 01-A | Ordinary rendering | 0.551 | 50 | 50 | 0 |
| 02-B | Cursor layer | 0.067 | 0 | 0 | 51 |
| 03-B | Cursor layer | 0.070 | 0 | 0 | 51 |
| 04-A | Ordinary rendering | 0.599 | 51 | 51 | 0 |

Across matched sixty-second totals, process CPU fell from **0.575% to 0.068% of
one core**, about **88% lower**. Main terminal passes and pane preparations
fell from **101 to zero**. Both orders improved. These measurements isolate
idle blinking; they do not predict the total cost of the supplied trace's
input events and larger workspace. GPU completion, scanout, and WindowServer
were not measured.

[Saved comparison](benchmarks/render-2026-10-27/cursor-splits.json) preserves
the frozen binary hash, reports, and build details. Built with Rust 1.99.1,
`aarch64-apple-darwin`, and the unchanged workspace release profile (thin LTO,
one codegen unit). No other agent builds or tests ran during the comparison.
Full logs and capture fixtures remain under
`/tmp/rustty-render-experiment/cursor-splits-20261027-background-abba/`.

The first attempt was rejected because the locked, sleeping Mac yielded no
terminal render passes. A second attempt failed the native keyboard-focus
check before sampling. Both are excluded. Bundle comparisons now use macOS
background launch (`open -g -n -W`); bare executables retain direct launch.
The harness also rejects a sample before timing if settling produced no
terminal render passes.

Three cursor tests pass, including pixel reconstruction over a middle pane
with dimmed neighbors, overlapping-pane rejection, clipping, cursor styles,
and UI occlusion. Production Clippy, Windows ARM64 compilation, formatting,
and the release bundle build pass. Each layer run passed all six existing
native lifecycle checks plus neighboring-pane output retirement and
focus-switch retirement/reacquisition. Captures exercise the ordinary
fallback; native patch alignment and seamless handoff were not visually
inspected.

To repeat with an unlocked Mac and an awake secondary display:

```sh
nu crates/rustty-app/build.nu --release --offline
python3 test/rustty/cursor_compare.py \
  --binary target/release/Rustty.app/Contents/MacOS/rustty \
  --panes 12 --output /tmp/rustty-cursor-splits-comparison
```
