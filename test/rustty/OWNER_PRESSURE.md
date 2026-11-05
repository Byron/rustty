# Owner compression under subscriber pressure

The owner-compression candidate preserves viewport and full retained-history
recovery in the two-burst native and three-burst embedded cases. Both revisions
reach the existing native socket write timeout when a three-burst workload keeps
the reader blocked for about 12 seconds. Reconnecting then reproduces every
retained row ID and its text. The policy does not change that timeout.

These are single paired stress cases, not evidence of a policy speedup. The
separate [owner-compression measurements](OWNER_COMPRESSION.md) measure retained
storage and repeated workloads with multiple panes. This report checks pressure,
recovery, latency, and compatibility with the existing abandonment boundary.

## Workload and source

Baseline: `2b508ce4d`. Candidate: that frozen source plus the owner-policy patch
with SHA256 `4813c1e85802ef46095c8d73965204ac194a094d45402c27c3303535a2d1e788`.
Both use the same full-history harness, locked dependencies, Rust 1.95.0 on Linux
AArch64, thin LTO, one codegen unit, and the default generic target. Runs use
fresh processes/directories without concurrent builds or timed jobs.

The styled record and approximately 16MiB burst size, record-aligned 32KiB writes,
4ms child pacing, separate command observer, echo pane, and default view
appearance/compression match the earlier pressure workload. The harness now
accepts `BURSTS=2|3`, defaulting to 3, and records it in JSON. The loop bound and
final `done-(bursts-1)` title use that value. This is a benchmark workload
parameter; production exposes no new setting. The original archival three-burst
source and the earlier fixed-three-burst functional binary/hash are preserved.

Viewport text equality is checked separately from all retained row IDs/text.
Full-history verification begins after catch-up command/latency sampling ends,
and is repeated after a fresh native reconnect. Actual retained counts are below
the requested 100,000-row ceiling because existing tmux history collection drops
10% when it fills. Both revisions retain the same rows in each matched case.

## Recovery results

| Blocked transport/workload | Before | Candidate | Full retained rows | Reconnect |
| --- | --- | --- | ---: | --- |
| Native, 2 bursts | Recovers in 657ms | Recovers in 692ms | 90,185, equal on both | Viewport and full history equal on both |
| Embedded, 3 bursts | Recovers in 465ms | Recovers in 503ms | 95,277, equal on both | Viewport and full history equal on both |
| Native, 3 bursts | Disconnects | Disconnects | Recovery unavailable | All 95,277 rows and viewport equal on both |

For the native three-burst cases, the reader remains blocked for 11.951s before
and 11.946s after. The existing 10-second write timeout ends the connection before
the guard is released. These recovery failures are preserved in raw JSON and
are not counted as successful catch-up. All six reconnect checks pass, including
full row identities and text. The new two-burst argument is exercised explicitly;
both three-burst cases exercise the default.

## Latency, CPU, and RSS

The successful recovery samples include checkpoint work and view delivery,
not rendering. Median sampled owner-command latency during bulk is about
1.1–1.4ms, but long synchronous snapshot stalls remain: maximum samples range
from 356ms to 1.20s; maximum echo-view samples range from 1.66s to 2.73s. These sparse
tail samples vary between runs and do not establish a speedup or a latency bound.

| Scenario | Catch-up command maximum, before→after | User CPU, before→after | Whole-job maximum RSS, before→after |
| --- | ---: | ---: | ---: |
| Native, 2 bursts | 98.3→127.6ms | 21.01→21.30s | 1,391→1,388MiB |
| Embedded, 3 bursts | 100.7→134.1ms | 21.99→22.42s | 1,281→1,294MiB |
| Native, 3 bursts | No successful recovery | 31.06→31.09s | 1,194→1,155MiB |

Catch-up command medians remain about 0.14ms in the successful cases. All raw
samples, counts, nearest-rank p99 values, user/system CPU, and RSS are preserved.
`/usr/bin/time -v` counters include fixture setup, recovery, reconnect, and the
post-latency full-history verification. That verification reads retained cells
and can restore compressed caches. Maximum RSS is neither summed concurrent
process-tree memory nor owner-only storage, and should not be substituted for
the owner storage/settled-RSS measurements.

## Reproduce

The [artifacts](benchmarks/owner-pressure-2026-11-05/) include exact source,
absolute and portable manifests, lockfile, candidate patch, source/binary hashes,
the earlier functional smoke result, and all successful and failed raw cases.
The portable manifest resolves with locked offline dependencies.

```sh
cargo +1.95.0 build --release --locked --offline \
  --manifest-path test/rustty/benchmarks/owner-pressure-2026-11-05/harness/Cargo.toml
/usr/bin/time -v \
  test/rustty/benchmarks/owner-pressure-2026-11-05/harness/target/release/rustty-pressure-probe \
  /tmp/new-pressure-native-two native 100000 2
# A new output path is required for each run. Omit BURSTS for the original 3.
# Select embedded for the local subscriber path.
python3 test/rustty/benchmarks/owner-pressure-2026-11-05/summarize.py
```

For the recorded comparison, build the same harness against the frozen baseline
and candidate patch, retain separate executables, and use the recorded order:
native 2 before/after, embedded 3 after/before, native 3 before/after. The unrelated
original three-burst archival source remains unchanged.
