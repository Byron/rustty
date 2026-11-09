# Native zgui follow-up, 2026-11-09

The native frontend now releases removed item signals, retains window metadata
across terminal redraws, isolates pane geometry bindings, settles input without
GPU presentation, skips unused whole-window terminal composition, and removes
the fixed 16 ms split-animation timer. The outliner and cross-window resource
ownership are unchanged.

The before bundle was built from `b02db4566`; the after bundle contains
`f0664fdfb344bfeee91bb560d018d9ddb076cff0` (Tix change `ozxmovu`). Both are debug
builds with Rust 1.95.0 on Apple M4 Max / Metal. The after bundle uses zgui fork
revision `1d526ed98cb89c2cc43e063a8445dedeeb556416`.

These are exploratory application replays: one forward-order run per case,
50 samples after warmup, offscreen GPU submission, and at least 50 ms between
samples. Fork validation ran during part of the baseline collection; host CPU
contention was not controlled. The numbers suggest reduced CPU work, but do not
establish production speedups, display cadence, or improved latency tails.
They do not measure GPU completion, visible presentation, or typing latency.

| Median measurement | Before | After |
| --- | ---: | ---: |
| Cached redraw thread CPU | 409.5 µs | 295.1 µs |
| Changing Unicode output thread CPU | 2.879 ms | 2.631 ms |
| Cached redraw wall time | 478.4 µs | 337.0 µs |
| Changing Unicode output wall time | 2.970 ms | 2.764 ms |
| Cached redraw RSS | 225.1 MB | 226.1 MB |
| Changing Unicode output RSS | 227.5 MB | 228.3 MB |

Short-run RSS is essentially unchanged. Signal drop/churn regressions establish
that removed models are released; these short replays do not exercise repeated
UI creation and destruction or quantify long-run memory growth.

Raw samples and machine/window metadata are in [before cached](before-cached.json),
[after cached](after-cached.json), [before output](before-output.json), and
[after output](after-output.json). Reproduce each case with the matching bundle:

```sh
RUSTTY_SMOKE_DIR=/tmp/zgui-replay RUSTTY_SMOKE_OFFSCREEN=1 \
  RUSTTY_SMOKE_TIMING=cached_redraw \
  target/debug/Rustty.app/Contents/MacOS/rustty --framework=zgui
# Use RUSTTY_SMOKE_TIMING=mixed_unicode for changing output.
```

Validation passed: 340 library tests (five ignored), 155 desktop tests, 27 native
smoke checks both offscreen and presented, 54 egui smoke checks, strict Clippy,
formatting, release and Windows all-target compilation, and signed bundle
verification. Native smoke verifies no GPU rendering during click-to-IME
settlement, no metadata rebuilding for terminal-only output, same-frame Find
counts, and unchanged-pane retention. Windows runtime and high-refresh visual
cadence were not tested.
