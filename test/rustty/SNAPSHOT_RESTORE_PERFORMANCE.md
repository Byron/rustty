# Native snapshot metadata restoration

Restoring native ANSI/tab metadata previously located the physical row twice
for every styled cell. Both lookups scanned the page list from its beginning.
The five ANSI cells per line in the shared-owner pressure corpus made this work
grow with both history length and page count.

The receiver now walks validated, sorted metadata runs forward through the
pages. It retains the existing style-admission helper, including inline blank
cells, style reference counts, resource growth, and page splitting. Admission
updates the known row location when a split moves it; the forward scan adjusts
its page start accordingly. Tab validation uses the same monotonic traversal
without changing validation order or mutating unvalidated state. Existing
DECCARA and legacy snapshot callers retain their ordinary locating wrapper.

## Isolated measurements

Linux AArch64 Neoverse-N2, Rust 1.95.0, release thin LTO, one codegen unit, default
generic target, no concurrent builds/timed jobs. Each process restores the same
128×32 terminal three times, checking native row metadata and full capture text
outside the timed phases. The 100k plain/styled cases also run in reverse order,
providing six samples per revision; other cases have three. Times below are
medians, not rendered or visible latency.

| Corpus | Input rows | Full native restore before | After | Metadata before | After |
| --- | ---: | ---: | ---: | ---: | ---: |
| Styled pressure record | 20,000 | 45.5ms | 41.1ms | 14.5ms | 9.8ms |
| Styled pressure record | 100,000 | 304.8ms | 208.1ms | 149.1ms | 51.7ms |
| Styled pressure record | 250,000 | 1112.6ms | 522.3ms | 725.3ms | 132.3ms |
| Styled text with tabs | 100,000 | 449.0ms | 190.7ms | 346.8ms | 88.9ms |
| Plain control | 20,000 | 22.7ms | 22.2ms | 3.54ms | 3.57ms |
| Plain control | 100,000 | 114.7ms | 113.7ms | 20.26ms | 20.48ms |
| Plain control | 250,000 | 290.9ms | 287.7ms | 54.10ms | 54.01ms |

Styled full restores improve by 10–53%, and the tab case by 58%. Plain controls
remain within about 2%; these samples establish no plain-text speedup. Portable
history decoding stays unchanged: the 100k styled case is 156.6ms before and
156.8ms after. Owner checkpoint construction also stays near 83ms in that case.
This change reduces receiver restoration work; it does not measure PTY delivery,
native frame serialization, view replacement, or rendering, and does not resolve
the synchronous owner snapshot cost documented in
[the 100k pressure report](NATIVE_SNAPSHOT_LIMITS.md).

The comparison freezes `e982fc4c0` and applies only the archived candidate patch.
The later navigation and C-format changes are excluded from this comparison;
the production port follows them at `a7b84bb16`. Exact source/patch/binary hashes,
raw samples, whole-job CPU/RSS counters, and the independent phase harness are
under [the measurement artifacts](benchmarks/snapshot-style-2026-11-05/).
Whole-job CPU/RSS counters also include fixture construction and correctness
checks; maximum RSS is not summed concurrent process-tree memory or an owned
terminal-storage measurement.

## Validation and reproduction

The new resource regression deliberately fills a native style table with
colliding entries, proves rebuild failure, and forces a page split while
applying metadata. It checks later runs on that row and following rows, exact
resource reference counts, checkpoint round trips, and continued input. A
second regression covers sparse tab runs across pages and rejects invalid tab
cells before applying metadata. Existing metadata, DECCARA/resource ownership,
legacy/current native snapshots, parser continuation, and image restore tests
exercise the shared helper and unchanged decode limits.

The production port passes 172 VT library tests, 33 portable snapshot tests,
21 native terminal tests, and 8 DECCARA integration tests (234 total). The
standalone benchmark manifest resolves with locked offline dependencies.

```sh
cargo +1.95.0 test --locked --offline -p rustty-vt --lib
cargo +1.95.0 test --locked --offline -p rustty-vt --test snapshot
cargo +1.95.0 test --locked --offline -p rustty-vt --test region_attributes
cargo +1.95.0 test --locked --offline -p tmuxx-terminal --lib

cargo +1.95.0 build --release --locked --offline \
  --manifest-path test/rustty/benchmarks/snapshot-style-2026-11-05/harness/Cargo.toml
test/rustty/benchmarks/snapshot-style-2026-11-05/harness/target/release/native-style-probe \
  100000 styled 3
```

The arguments are input row count, `plain`/`styled`/`tabs`, and repetitions.
For the recorded A/B comparison, use the frozen base revision and archived
candidate patch with the same harness, lockfile, compiler, and release profile.
Keep binaries separate and run without competing CPU work. The harness reports
checkpoint construction, initial ready decode (including cell admission),
history decode, native metadata validation/application, and complete native
restore separately. Decoded cells are resident; this is not a compressed-history
access benchmark.
