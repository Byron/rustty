# Native large-history restoration

The native client previously applied the portable checkpoint decoder's generic
1,048,576-cell limit to active screens and all history combined. At 128 columns,
8,000 input rows restored successfully, while 8,300 failed with `invalid or
excessive snapshot page dimensions`. The retained checkpoint was only about
60KB. The [earlier pressure report](DELIVERY_RECOVERY.md) preserves the failed
100,000-row baseline and delivery-candidate runs.

Native restoration now counts actual physical row widths in the supplied
metadata. Both the older full-screen `Snapshot` and current metadata-only
`CheckpointSnapshot` use this policy. Mixed-width rows and history retained by
reflow beyond the live line limit remain valid. Neither current columns times
the configured history limit nor encoded checkpoint size predicts the required
cell storage correctly.

The receiver admits at most 8GiB of packed cells (1Gi cells at eight bytes each),
using checked `u64` arithmetic. A complete pane graph checks the aggregate before
restoring its first pane. The portable decoder's defaults remain unchanged:
`ready()` validates the checkpoint header and active pages with the original
limit, then the native receiver explicitly sets the cumulative history budget.
This preserves the active-grid defense even when an untrusted checkpoint header
disagrees with benign outer dimensions. Record, parser-continuation, and graphics
budgets and exact metadata validation remain in effect.

The packed-cell ceiling is not a total RSS bound. Row/resource metadata, images,
existing terminals, immutable snapshots, and attached view copies coexist. This
change adds no wire fields and leaves protocol versions unchanged. Older
receivers retain their existing history limit.

## Validation

Linux AArch64, Rust 1.95.0, locked offline dependencies:

- 33 portable snapshot tests, including forged headers/pages, staged budget
  exhaustion, checksums, record/continuation limits, and streaming history.
- 20 native terminal tests, including both snapshot carriers across the former
  limit, continued fragmented input, styled history/cursor/native metadata
  equality, mixed physical widths, non-reflow resize, reflow beyond the live
  limit, zero history, and exact ceiling/one-cell-over/overflow checks.
- 4 view-metadata tests and 41 existing/native connection tests. The aggregate
  test poisons the first checkpoint to prove the whole graph's over-budget
  rejection occurs before decoding any pane.

Output is in [the artifacts](benchmarks/native-history-2026-11-05/).
No renderer or visible latency is measured. The 32-bit arithmetic path was
reviewed; this host has only the AArch64 target installed.

```sh
cargo +1.95.0 test --locked --offline -p rustty-vt --test snapshot
cargo +1.95.0 test --locked --offline -p rustty-vt --lib view_metadata
cargo +1.95.0 test --locked --offline -p tmuxx-terminal --lib
cargo +1.95.0 test --locked --offline -p rustty --no-default-features \
  --features sessions --lib tmuxx::tests
```

## 100,000-row shared-owner pressure

The unchanged [pressure source](benchmarks/delivery-2026-11-05/pressure-owner/pressure.rs)
uses three approximately 16MiB styled-output bursts, a blocked subscriber, an
independent command observer, and a responsive echo pane. Every view applies
default appearance/compression. Both 100,000-row cases now reconnect through native transport with matching
terminal text. In the original three-burst run,
embedded recovery succeeds after 572ms; the blocked native connection reaches the
existing 10-second socket write timeout before it is released after about 12.6s.
That failed recovery sample is retained and is not counted as a success.

The shorter two-burst variant changes only the loop bound and final-title
expectations, keeping the blocked interval below the existing abandonment
timeout. Both paths recover and reconnect with equal terminal text:

| Blocked transport, two bursts | Unlock to final state | Catch-up command maximum | User CPU | Maximum RSS |
| --- | ---: | ---: | ---: | ---: |
| Embedded | 531ms | 97.2ms | 15.16s | 1,195MiB |
| Native socket | 671ms | 96.6ms | 21.43s | 1,328MiB |

These are single functional pressure runs, not repeated throughput comparisons.
The exact variant source and all raw samples are included with the artifacts.

The original three-burst 100k runs expose substantial synchronous history costs:
maximum sampled command latency during bulk is 747ms embedded and 818ms native;
echo-view latency reaches 2.34s and 2.20s. Embedded catch-up samples reach 101ms.
User CPU is 22.79s/31.23s and reported maximum RSS is 1,229MiB/1,092MiB respectively.
These are `/usr/bin/time -v` job counters; maximum RSS is not summed concurrent
process-tree memory or owned terminal storage. Failed earlier 100k runs provide
no comparable completed throughput baseline. This change fixes restoration;
large-history snapshot/owner responsiveness remains separate performance work.

The subsequent [receiver metadata optimization](SNAPSHOT_RESTORE_PERFORMANCE.md)
measures and reduces the style/tab restoration portion separately from owner
checkpoint construction and PTY delivery.

## Reproduce

Use the portable manifest from the earlier delivery report, then pass `100000`
as the history argument. Each run needs a new empty output path.

```sh
cargo +1.95.0 build --release --locked --offline \
  --manifest-path test/rustty/benchmarks/delivery-2026-11-05/pressure-harness/Cargo.toml
/usr/bin/time -v \
  test/rustty/benchmarks/delivery-2026-11-05/pressure-harness/target/release/rustty-pressure-probe \
  /tmp/new-native-history native 100000
```

For the bounded two-burst run, use the archived variant as that manifest's
binary source. Keep its lockfile, thin-LTO/one-codegen-unit release profile, and
compiler unchanged. The artifact manifest records source, patch, and binary
hashes. Measurements ran without concurrent builds or timed jobs.
