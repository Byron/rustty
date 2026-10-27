# Mouse-hover CPU, 2026-10-27

The supplied `rustty-busy-on-mouse-over.trace` exposes avoidable Stream Deck
dashboard work on ordinary event-loop wakes. Mouse movement already bypasses
egui repainting over terminal panes, but `about_to_wait → tick_deck → sync_deck`
still rebuilds the dashboard and submits a cloned snapshot on every wake.

## Recording

Both runs profile `/Applications/Rustty.app/Contents/MacOS/rustty`, executable
UUID `FA6A1633-28E3-356D-BB86-B8D929D93296`. The recorded executable's source
revision is not established by this trace. The corresponding unconditional
dashboard update is also present in the inspected source at `85c62cce1`.

| Run | Duration | Running-thread samples | Sample weight |
| --- | ---: | ---: | ---: |
| `startup and minor activity` | 147.055 s | 2,730 | 2,730 ms |
| `mouse-over-hover` | 42.940 s | 1,153 | 1,153 ms |

Samples carry 1 ms weights. These are statistical CPU estimates, not direct CPU
accounting or measurements of Activity Monitor's reported 4%.

| Inclusive stack in hover run | Weight | Share of process samples |
| --- | ---: | ---: |
| `about_to_wait` | 190 ms | 16.5% |
| `sync_deck_with_host` | 155 ms | 13.4% |
| `Dashboard::update` | 109 ms | 9.5% |
| `draw_frame` | 20 ms | 1.7% |
| `App::mouse` | 11 ms | 1.0% |

Inclusive rows overlap: dashboard work is inside `about_to_wait`. All 155
dashboard samples in the hover run occur under that callback. The stacks show
pane enumeration, dashboard-position reconciliation, label formatting, hashing,
allocation, and tile/group rebuilding. This repeats across the workspace,
regardless of whether the pane under the mouse runs a shell or a TUI.

The main thread accounts for 658 ms, the native `com.apple.NSEventThread` for
355 ms, and the connected Stream Deck worker for 36 ms. Native event handling
and the device's own loop remain separate costs. The first run also contains
session restoration, JSON deserialization, pane creation, and drawing; its
487 ms of drawing samples and 99 ms of dashboard samples do not establish that
most startup activity was caused by hovering.

## Shells and TUIs

Plain shells have no terminal mouse reports. Mode 1000 reports buttons, mode
1002 adds button-held motion, and mode 1003 reports ordinary motion. Cell-based
formats suppress repeated movement inside one cell; pixel format 1016 retains
pixel motion. Shift capture follows the existing application/user policy.
Requested reports and resulting PTY output remain necessary work. Command-hover
link detection, selection dragging, divider dragging, and UI hover retain their
existing paths.
The samples do not identify the hovered pane's program or active mouse mode,
so this recording cannot directly compare shell and TUI hover costs.

## Change and verification

Dashboard rebuilding and worker submission now depend on changes to workspace,
host state, pane output, configuration, device input, or status expiry. No new
polling deadline is added. Explicit dashboard commands retain their immediate
updates. Expired progress/activity is processed before the dashboard refresh.
The duplicate command-policy scan in `about_to_wait` is removed.

- Native regression: 256 pointer moves across panes and split gaps cause zero
  dashboard rebuilds; a workspace change causes one immediate refresh.
- Restoring unconditional updates makes that regression fail with
  `passive pointer motion rebuilt the Stream Deck dashboard`.
- The complete hidden debug Metal smoke passes, including mouse scrolling, selection,
  links, OSC cursor shapes, PTY input/output, and the Stream Deck fixture.
  USB is disabled; native minimization and quick-window animation are skipped.
- 33 application input tests, three terminal mouse tests, and three dashboard
  host tests pass. Production Clippy, Windows ARM64 compilation, formatting,
  and diff checks pass.
- The optimized macOS bundle builds and passes signature verification. Its
  complete smoke stops before the hover check at
  `quad-in quarter rebuilt unchanged terminal content`; the installed executable
  with the trace's exact UUID fails at the same assertion. The optimized
  dashboard-only fixture passes separately.

Trace XML exports and native smoke logs are under `/tmp/rustty-hover-*`.
No end-to-end before/after CPU reduction or physical-device behavior has been
measured for this change; the native regression verifies the skipped work.
