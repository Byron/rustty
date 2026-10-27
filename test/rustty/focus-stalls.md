# Focus and tab stalls, 2026-10-27

The supplied `russty-gpu-idle-in-background.trace`, run
`mouse-over-and-focus-changes`, records four main-thread hangs in 18.041 seconds.
Both this trace and `rustty-mouse-hover.trace` profile the installed executable
with UUID `A49FCF2E-3111-385C-8516-FC34A5A96D6B`.

| Hang starts | Hang duration | Waiting for next drawable |
| --- | ---: | ---: |
| 3.834643 s | 1,010.654 ms | 1,001.014 ms |
| 6.135222 s | 1,012.177 ms | 1,000.802 ms |
| 10.035780 s | 1,010.214 ms | 1,001.013 ms |
| 14.568395 s | 1,009.995 ms | 1,001.014 ms |

Samples at the ends of those waits resolve through
`App::draw_frame → Painter::paint → egui_wgpu::Painter::paint_and_update_textures
→ get_current_texture → CAMetalLayer::nextDrawable`. The blocked acquisition is
the main window's surface. Each follows two `egui_render` submissions less than
1 ms apart. Rustty's GPU activity during each entire hang totals only
0.89–1.56 ms, so GPU saturation does not explain the delay. No sampled stack in
this run reaches `spawn_pane` or `spawn_restored`.

Retiring the retained cursor patch presents the main frame through a Core
Animation transaction. The low-latency main surface has two drawables; the
transaction can retain one while the other is displayed, leaving none for the
next redraw. This fits the recorded waits, but the trace does not directly
identify which references retain the drawables. The cursor patch itself already
uses three drawables and its six presentations have short acquisitions.

## Drawable configuration

Configure macOS main surfaces with a maximum frame latency of two, which gives
Metal three drawables. This matches egui's existing allowance for transaction
presentation during live resize. Keeping the setting in the surface configuration
preserves it through resizing, reconfiguration, and surface recovery. A native
setter before painting could be overwritten by egui's subsequent reconfiguration.

The tradeoff is one additional full-window drawable and capacity for one more
queued frame. Cursor retirement still uses the same transaction to synchronize
the main frame and cursor patch.

### Validation

The native cursor smoke now checks eight pane switches and eight switches between
already-warmed tabs. Each switch must render three consecutive main frames in
under 500 ms, retain at least three main-layer drawables, and preserve cursor
retirement. Existing capture, pixel, synchronized-output, and cursor-blink checks
remain in place.

- The original configuration fails the capacity check with
  `cursor retirement has only 2 main drawables`.
- The optimized three-drawable build passes all lifecycle checks. The sixteen
  redraw bursts take 0.91–3.51 ms each. During the separate 30-second idle sample,
  50 cursor updates require no main frames, terminal paints, or pane preparation.
- Formatting passes. Strict app Clippy encounters two unchanged warnings in
  `desktop.rs` (`nonminimal_bool`, `type_complexity`); the same check passes with
  those two lints allowed.

Run with `RUSTTY_SMOKE_CURSOR_LAYER=1`, `RUSTTY_SMOKE_CURSOR_PANES=12`, and a fresh
`RUSTTY_SMOKE_DIR` using the native app bundle. Results are saved in
`/tmp/rustty-capacity-baseline` and `/tmp/rustty-focus-final` for this session.
These test windows did not take keyboard focus and reported occlusion. Ordinary
redraw bursts, including an experiment retaining one extra drawable, did not
reproduce the original timeout. The capacity regression validates the mitigation;
a new foreground trace is still needed to establish that the observed stalls are
gone. The burst timings are CPU submission time, not visible activation latency.

## Background GPU activity

The `idel in background` run lasts 12.923 seconds and contains no hangs. Rustty
submits seven rendered frames, all around 10.886–11.891 seconds. Its combined
GPU execution intervals total 8.720 ms, with no Rustty GPU work during the first
roughly 10.8 seconds. Most GPU activity in this system trace belongs to
WindowServer. System-wide GPU activity should not be attributed to Rustty.

## Hover CPU

The separate 15.015-second hover recording contains 297 running-thread samples,
each weighted at 1 ms. Of these, 20 include dashboard synchronization, eight
include pane draining, nine include the mouse handler, and 12 belong to the PTY
writer. These inclusive categories overlap. Mouse stacks reach `send_input`
and `Session::write`, demonstrating actual terminal mouse reports. In the
profiled implementation, a successful local PTY write wakes the UI even when no
output or blocked input needs handling; this re-enters pane draining and marks
the dashboard dirty.

These samples are statistical CPU estimates and do not establish a constant
5% CPU load or a before/after improvement. The first dashboard fix removed
work from pure pointer events; successful terminal writes still take this
separate notification path.

Successful local writes now request a UI wake only when a previously blocked
input producer needs to retry or input just finished restoring saved display
content. The queue-budget mutex orders the backpressure flag against releases,
and every release still notifies blocking protocol-reply producers. PTY output,
resize, host reports, errors, and exit keep their existing notifications. This
does not suppress the mouse reports requested by a terminal application.

The real-PTY regression starts a raw, no-echo child that consumes input silently.
After draining startup notifications, 256 mouse reports must complete without
any UI callback; resizing must still produce a callback. The original writer
fails this assertion, and the updated writer passes. All 22 session unit tests
pass, including restored output, host reports, command ordering, backpressure,
blocking replies, and child cleanup. Strict Clippy for the session crate and
workspace formatting also pass. End-to-end foreground CPU usage has not been
remeasured.

The combined optimized app builds and passes bundle signature verification at
`target/release/Rustty.app`. Its executable UUID is
`AFF1E147-8426-3747-AF61-78B505347DBF` (arm64).

Trace exports and parsed interval summaries are retained under
`/tmp/rustty-background-*`, `/tmp/rustty-hover-new-*`, and
`/tmp/rustty-metal-summary.*`.
