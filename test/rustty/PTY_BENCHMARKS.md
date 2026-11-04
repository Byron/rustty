# Linux PTY and parser measurements

`pty-bench` measures actual Rustty sessions with local, embedded tmuxx, and native
socket tmuxx ownership. It does not create a desktop window, renderer, or GPU.

```sh
cargo +1.95.0 build --release --locked --offline -p rustty \
  --features sessions --example pty-bench
target/release/examples/pty-bench /tmp/pty-before
# Select one path and preserve the unpaced stress case:
target/release/examples/pty-bench /tmp/pty-stress embedded unpaced
```

Output directories must be new. Each completed path writes raw samples and
prints a short summary. Failures write `complete: false` and return a nonzero
status; they are not recorded as successful throughput runs. On Linux, tmuxx's
process-owner syscalls may require execution outside a syscall-restricting
sandbox. All PTYs, children, owners, sockets and configuration are test-owned.

Build both revisions before timing, copy their executables to separate paths,
then run baseline/candidate/candidate/baseline with builds and other benchmarks
stopped. The child uses the same executable as its parent. Keep compiler flags,
harness source, configuration and machine constant. Do not compare a debug run
with a release run or multiply speedups from independent experiments.

## Measurement boundaries

All paths use 128 columns, 32 rows and 1024 history rows. Local sessions keep the
normal compression policy; tmuxx retains its owner configuration and normal view
policy. The benchmark exercises these paths separately:

- `host_echo_*_ns`: time from `Session::write` to observing the child's updated
  OSC title in that Session. This includes input queues, child handling, output
  parsing and view delivery, with 1ms think time outside each measurement.
- `sparse_dsr`: time in the child from writing a cursor-position query to reading
  its reply. For tmuxx this measures the owner and PTY, excluding native client
  view delivery. Sparse reports must be row 1, column 1, and completed bulk runs must end at
  row 32, column 1; other returned coordinates are checked against the grid.
- `bulk_*`: three approximately 16MiB runs of ASCII logs or styled mixed text.
  Default runs acknowledge complete records totaling at most 256KiB after the
  Session view sees an ordered title marker. Markers never interrupt UTF-8 or
  escape sequences. `view_ack_bytes` records the actual batch size; these are
  measurements with bounded outstanding replay, not unrestricted throughput.
  Paced `write_ns` includes those view-acknowledgment waits; unpaced and burst
  `write_ns` only covers writing and flushing the payload.
- `burst_*_then_dsr`: 200 runs of exactly 4096 or 65536 bytes, followed by a
  cursor query. `write_ns` separates writer backpressure from `drained_ns`, which
  includes the final reply. The difference is the post-write queue/parser tail.

Wake callbacks use a capacity-one notification channel, like Rustty's existing
coalesced desktop notification. Callback counts are diagnostic, not rendered
frames. None of these measurements establishes visible or rendered latency.

`unpaced` removes the view acknowledgment from bulk output. At the original
baseline, sustained 16MiB embedded output disconnected its bounded subscriber
queue. The event-driven and output-coalescing prototypes also disconnected.
The independent input-latency improvements do not fix that existing overload
behavior. Preserve it as a separate stress result.

## Linux baseline and experiments, 2026-11-04

Machine: Linux 7.0.0-1019-azure on ARM Neoverse-N2, 16 cores. Rust 1.95.0 with the
repository's release profile (thin LTO, one codegen unit), default AArch64 CPU
flags and no affinity pinning. This host is not x86; x86 SIMD performance work
is deferred until it can be executed and timed on appropriate hardware.

The [artifacts](benchmarks/pty-2026-11-04/) preserve source revisions, hashes,
commands, samples and failed experiments. The permanent-harness baseline uses
`76a5d2ffe`, after the TUI, compression and program-status changes. Earlier
prototypes used `a90e26835`; their measurements are explicitly separate.

### Existing implementation and references

Ghostty's `src/termio/Exec.zig` already separates gathering from parsing using
four 64KiB buffers, bounded refill spinning and bridge polls that stop when the
parser becomes idle. Its comments derive the thresholds from macOS/M4 Max.
Kitty's `kitty/child-monitor.c` only enables repeated PTY gathering on macOS;
its Linux path leaves it disabled. Their production sources remain unchanged.

The original ordinary Rustty session reads up to 32KiB and parses on the same
thread. tmuxx already has separate PTY readers and an owner event queue, followed
by client/view parsing. Embedded client delivery also has an unconditional 2ms
polling sleep. Native socket delivery uses separate request and response workers.
The desktop already coalesces wakeups with an atomic pending flag.

Rustty's parser ASCII/UTF-8 prefix scans and UTF-8 validation, and its VT
printing/destination scans, select explicit SIMD only on AArch64/NEON. Existing
scalar-equivalence tests can support a future x86 implementation; a cross-build
alone would not establish an x86 performance improvement.

### Initial core baseline

The existing primitive benchmark self-checks passed. The timed subset used
20 samples, 0.3s warmup and 1s requested measurement, extended by Criterion when
needed. Ghostty used Zig 0.17.0 `ReleaseFast`. These specific core workloads
exclude PTY, IPC and presentation; they are not general terminal speed rankings.

| Workload | Rustty | Ghostty |
| --- | ---: | ---: |
| ASCII feed | 1.443µs | 1.226µs |
| Emoji feed | 62.35µs | 39.36µs |
| Styled ASCII stream | 28.09µs | 64.98µs |
| Styled emoji stream | 1.169ms | 1.744ms |

```sh
zig build vt-primitives -Demit-lib-vt=true -Demit-macos-app=false \
  -Doptimize=ReleaseFast
GHOSTTY_PRIMITIVES_BIN="$PWD/zig-out/bin/vt-primitives" \
  cargo +1.95.0 bench --locked --offline -p rustty-vt --bench primitives -- \
  '^(rustty|ghostty)/(feed|stream_styled)/(ascii|emoji)$' \
  --sample-size 20 --warm-up-time 0.3 --measurement-time 1
```

### Prototype selection

Independent prototype ABBA results on `a90e26835` justified production trials:

| Prototype | ASCII MiB/s before → after | Mixed MiB/s before → after | Echo p50 before → after |
| --- | ---: | ---: | ---: |
| Ordinary session gather | 156.10 → 234.79 | 50.49 → 54.30 | 51.95 → 60.60µs |
| Embedded event-driven IO | 35.21 → 73.86 | 10.08 → 11.99 | 3067.01 → 100.43µs |

The local gather prototype adds one worker and four reusable 32KiB buffers
instead of the prior single 32KiB buffer: a fixed 128KiB pool and about 9µs of
median sparse-echo handoff cost. The large-burst total median improved from
543 to 410µs, while its post-write tail grew from 143 to 208µs because the child
finished writing earlier. These are different latency boundaries.

The prototype harness used 4224/67648-byte bursts and inserted bulk markers at
arbitrary byte boundaries. Its exact source and samples are preserved under
`experiments`; the corrected permanent harness uses exact sizes and complete
records. Fresh production comparisons must use the permanent harness on both
revisions, rather than treating these prototype results as final measurements.

### Rejected tmuxx output coalescing

A separate prototype combined only already-queued consecutive output from the
same pane/process up to 64KiB. It retained the first other event, did not wait
for future data, and never combined across commands, EOF or another pane.

Native callbacks fell from about 13.6k to 3.5k, but ASCII throughput stayed at
68–70MiB/s and styled mixed throughput regressed from 11.45–12.03 to
10.86–11.08MiB/s. Embedded throughput was unchanged or slower, with unchanged
3ms echo. Unpaced 16MiB output still disconnected. The extra machinery was
rejected; its patch and raw measurements remain in `experiments`.

### Permanent-harness baseline after protocol/status/compression changes

The corrected harness passes all three paths at `76a5d2ffe`, including expected
DSR coordinates and complete view acknowledgments. Raw samples and its exact
compiler/source/executable manifest are in `baseline/`.

| Path | Echo p50 / p99 | ASCII MiB/s (3 runs) | Styled mixed MiB/s (3 runs) |
| --- | ---: | ---: | ---: |
| Ordinary local | 50.3 / 69.1µs | 160.85, 161.50, 161.67 | 50.68, 50.77, 50.88 |
| Embedded tmuxx | 3066.2 / 3099.3µs | 35.22, 35.21, 35.22 | 10.11, 10.11, 10.10 |
| Native socket tmuxx | 136.4 / 174.7µs | 67.81, 68.18, 69.10 | 11.11, 11.01, 11.03 |

The same harness records `complete: false` and exits 1 for unpaced embedded
16MiB output (`baseline/embedded-unpaced.json`), preserving the known overload
failure as explicit evidence.
