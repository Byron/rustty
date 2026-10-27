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
