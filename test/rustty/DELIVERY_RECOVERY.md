# Bounded tmuxx subscriber recovery on Linux

Embedded and native socket views now catch up after sustained output without
being disconnected merely because their delivery queue fills. The owner keeps
parsing PTY output and serving other clients. A lagging view receives a current
checkpoint before incremental output resumes; replies and host actions remain
ordered and are not reconstructed by replaying terminal effects.

The original `247ad8119` unpaced embedded benchmark disconnected in 0.85s.
With recovery, both embedded and native paths complete all six approximately
16MiB corpora, cursor-query checks, final-title delivery and subsequent input.
This establishes owner parsing and eventual current-view delivery. Recovery may
skip intermediate output, so these unpaced rates do not measure delivery of
every PTY byte to every view.

## Delivery and lifecycle bounds

- The existing transport channel holds 256 messages, with one additional frame
  being consumed or written. Its byte accounting excludes that in-flight frame.
- Waiting critical messages have a 256-record cap. Their retained bytes and the
  ordinary queued transport bytes share the existing 64MiB budget.
- A missing output, structure or selection update marks the view for a fresh
  checkpoint. Further deltas are suppressed until that snapshot is admitted.
  Construction waits for the older backlog to drain; at most one ready client
  gets a new checkpoint per owner turn, rotating between clients.
- The existing `SnapshotSent` gate permits one admitted snapshot. Embedded
  clients acknowledge receipt; native socket writers acknowledge completed
  writes. The existing snapshot serialization limit is 8GiB. A consumer can
  retain its in-flight frame while the next snapshot is queued.
- Before natural pane removal, lagging views retain a final checkpoint barrier
  so closed Session objects keep their final text. Waiting barriers are charged
  their actual encoded size against the same 64MiB critical-message budget and
  obey the same snapshot ACK gate. Oversized barriers or unconsumed critical
  floods can still disconnect a client.
- Closing transports immediately release command admission, focus, host-action
  election, pane leases and size authority. Their final FIFO drains before
  disconnection, with the existing control-client policy of a 10s maximum wait.

The existing owner idle tick provides eventual recovery with no new output;
there is no per-output acknowledgment or new configuration setting. Its idle
wait is at most 50ms before checkpoint work starts. Snapshot construction is
still synchronous, and retained history affects its cost. No wire version or
message variants change.

## Focused validation

All 68 runtime library tests and 40 Linux Rustty Connection tests pass. Coverage
includes byte-budget recovery with an independently draining healthy client,
exact recovered checkpoints, ordered replies/host actions, full snapshot queues,
ACK overlap, two natural exits, rejected checkpoints, bounded critical queues,
closing deadlines, late-request rejection and immediate owner-role release.

Live tests deliberately block one Session's terminal while more than 16MiB
arrives. They verify admitted input exactly once, responsive-peer command
completion, full retained-history capture equality with the owner, reconnect
agreement, and final text after both pane removal and last-session closure.

```sh
cargo +1.95.0 test --locked --offline -p tmuxx-runtime --lib
cargo +1.95.0 test --locked --offline -p rustty --features sessions --lib tmuxx::tests
```

## PTY throughput and sparse delivery latency

The same permanent harness and Rust 1.95.0 release profile were used on both
revisions. Measurements ran in ABBA order on Linux ARM Neoverse-N2 with 16 cores,
without affinity pinning, builds or competing benchmark jobs. The ordinary local
PTY pipeline is unchanged. These results are not rendered or visible latency.

| Path | Paced ASCII MiB/s before → after | Paced mixed MiB/s before → after | Echo p50 / p99 before → after |
| --- | ---: | ---: | ---: |
| Embedded | 72.46 → 72.47 | 11.27 → 11.16 | 105.08 / 198.79 → 99.31 / 127.92µs |
| Native socket | 70.53 → 68.75 | 11.42 → 11.10 | 131.51 / 168.05 → 136.54 / 165.86µs |

The retained benefit is recovery correctness. Native paced throughput was
2.5–2.8% lower in these pairs, and embedded mixed throughput was 1% lower.
Aggregate user+system CPU was 8.50–8.63s before versus 8.60–8.70s after for
embedded, and 8.42–8.79s versus 8.64–9.13s for native. Peak RSS in those paced
runs was 24–31MiB. The new unpaced runs reach approximately 120–123MiB/s ASCII
and 19.6–19.8MiB/s mixed through owner parsing and snapshot catch-up; the original
failure has no successful throughput number for comparison.

[Raw samples and manifests](benchmarks/delivery-2026-11-05/) include failed
baseline output, all paced samples, unpaced completion, CPU/RSS counters and
source/executable hashes. See [PTY_BENCHMARKS.md](PTY_BENCHMARKS.md) for commands,
corpora and precise measurement boundaries.

## Shared-owner pressure

The `pressure-snapshot-heavy` artifacts preserve an initial interaction workload
whose subscribing command connection requests a full snapshot on each command.
It also uses the original probe's default view appearance. These samples measure
snapshot-heavy interaction, not an isolated owner-command round trip. Every
blocked baseline peer disconnected; all four candidate peers recovered exact
terminal text, and all reconnects matched. Their 150–205ms unlock-to-final times
include queued replay and checkpoint delivery, not just the 50ms idle tick.

The final `pressure-owner` workload uses a separate command-only native
connection, normal application appearance and an independent echo pane. Three
approximately 16MiB styled bursts arrive in complete records of at most 32KiB,
with 4ms between batches (about 8MiB/s). This lets the responsive view keep up
while the blocked view exceeds its delivery capacity. Command and echo samples
run throughout each burst, and command sampling continues through catch-up.

Across embedded and native ABBA pairs, all four baseline blocked peers
disconnect; all four candidate peers recover matching terminal text. All eight
reconnects match. Embedded unlock-to-final recovery takes 81–89ms; native takes
182–191ms. During bulk, owner-command p99 is 1.53–1.93ms after versus
1.71–1.93ms before; echo p99 is at most 3.30ms versus 3.43ms. During candidate
catch-up, maximum sampled owner-command latency is 0.53–1.72ms. Phase variation
precludes a latency-speedup claim; the gain is bounded recovery while other
clients continue working.

User CPU across each three-burst job is 6.55–7.00s after versus 6.98–7.05s before
for blocked embedded views, and 10.35–10.65s versus 9.75–10.34s for native views.
Reported maximum RSS grows from 60–62MiB to 67–68MiB for embedded, and 66–67MiB
to 74–75MiB for native. These are `/usr/bin/time -v` job counters: maximum RSS
is neither summed concurrent process-tree memory nor owned terminal storage.

The 100,000-row native probe fails on both revisions. The existing checkpoint
decoder limits cumulative restored cells to 1,048,576, about 8192 rows at 128
columns including the screen. This is a separate pre-existing large-history
restoration gap; these results do not establish successful 100,000-row recovery
or its owner latency. Failed baseline/candidate samples are retained. Snapshot
construction remains synchronous and must be measured again after that gap is
resolved.

The frozen active-workload measurements precede only the final correction to
latest-size fallback when a closing transport awaits its final FIFO. That
lifecycle transition was not exercised during the timed active workload. The
exact measured production patch/hash and final source hashes are retained;
final regression results include the fallback correction.

## Reproduce the shared-owner workload

The portable manifest changes only recorded dependency paths; the measured
source and lockfile are preserved exactly. Use the same source, lockfile and
release profile for both revisions, copy executables before switching trees,
and run ABBA without other builds or timed jobs.

```sh
cargo +1.95.0 build --release --locked --offline \
  --manifest-path test/rustty/benchmarks/delivery-2026-11-05/pressure-harness/Cargo.toml
/usr/bin/time -v \
  test/rustty/benchmarks/delivery-2026-11-05/pressure-harness/target/release/rustty-pressure-probe \
  /tmp/new-pressure-output embedded
# Select native for a blocked socket peer, or none for a control run.
# Optional third argument selects history rows (default 1024).
```
