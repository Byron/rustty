# Rustty charset batching, 2026-10-15

Porting Ghostty `a4f0d9f4a` to Rustty's packed ASCII writer improves the
measured DEC line-drawing workloads by 9.00–10.25× and British charset
workloads by 10.57–13.08×. Character mapping stays shared with scalar printing;
single shifts, Unicode, hyperlinks and exceptional destination cells retain
their existing handling.

The baseline was built at `0ac7cfd1be02704ccd4d3c9bf023c0894077c000`
(Tix change `zpnsnkm`), after the upstream correctness ports and the benchmark
addition. Later message or ancestor amendments can change this hash. Both
binaries contain identical benchmark workloads and were frozen before timing.

Environment: Apple M4 Max, 64 GB RAM, macOS 27.0.1 (26A434), arm64;
`rustc 1.99.0 (b940084d7 2026-09-28)`, workspace bench profile. Builds and tests
were paused during measurements. Each workload ran serially in before/after
order and then after/before order, with 50 samples per direction, 0.3 seconds
warmup and a one-second measurement target. Pooled results are the median of
all 100 normalized samples, not the mean of the two directional medians.

Times below are microseconds per complete workload iteration. The last column
is pooled baseline time divided by candidate time.

| Workload | Forward before → after | Reverse before → after | Pooled before → after | Speedup |
| --- | ---: | ---: | ---: | ---: |
| Existing ASCII feed | 0.432206 → 0.434278 | 0.422774 → 0.426221 | 0.427805 → 0.429966 | 0.995× |
| Existing ASCII stream | 5.563079 → 5.659194 | 5.579606 → 5.563397 | 5.573474 → 5.627621 | 0.990× |
| Charset repaint / ASCII | 4.674154 → 4.528179 | 4.596913 → 4.510758 | 4.624332 → 4.518595 | 1.023× |
| Charset repaint / DEC | 55.721370 → 5.412413 | 54.989197 → 5.423295 | 55.497498 → 5.413590 | 10.252× |
| Charset repaint / British | 59.790066 → 4.473290 | 58.898894 → 4.566169 | 59.245262 → 4.530722 | 13.076× |
| Charset stream / ASCII | 8.888957 → 8.897137 | 8.960052 → 8.737602 | 8.935130 → 8.778387 | 1.018× |
| Charset stream / DEC | 90.041544 → 10.073473 | 90.325730 → 9.986011 | 90.112087 → 10.017129 | 8.996× |
| Charset stream / British | 94.224942 → 8.934132 | 93.257568 → 8.798770 | 93.911239 → 8.882232 | 10.573× |

The retention criterion was at least 10% improvement in DEC repaint or stream
in both orders, with no ASCII feed/stream regression above 5%. All mapped
workloads improved by more than 88% in both directions. Existing ASCII feed
and stream pooled times increased 0.51% and 0.97%; their worst directional
increase was 1.73%. The candidate passes the criterion.

`charset_bench.rs` repaints a styled 128×32 screen or streams 32 styled
192-column records with history capped at 1,024 rows. Streaming warms through
history eviction before timing. Outside timing, the workloads validate every
visible codepoint, width, style, cursor position, pending wrap, checksum and
REP after selecting ASCII again. Seven charset tests and eight batched-input
tests also pass, including scalar-versus-batched snapshots across all
printable ASCII bytes, styles, narrow windows and page boundaries.

This measures terminal parsing and cell storage, excluding fonts, renderer
preparation, GPU work, PTYs and presentation. The gains apply to these
charset-heavy workloads; they do not predict GUI frame rate or a new Ghostty
speedup measurement.

Frozen executables:

| Variant | SHA-256 |
| --- | --- |
| before | `2f42e874851dabef12d0f68f2e7eaa2caac7a99252b2adb0967004799515e542` |
| after | `bd705a7f6ccdc61500d5aa95f4049c1702ffa0f759aca0ff8a2a532aadc1b4c8` |

At each revision, the commands used to build and validate the executable were:

```sh
cargo bench --locked --offline -p rustty-vt --bench primitives --no-run
cargo bench --locked --offline -p rustty-vt --bench primitives -- charset --test
```

The reported executable, `target/release/deps/primitives-2209d459cc6cadfc`, was
copied to `/private/tmp/rustty-charset-performance-20261015/before` and then
`after` at the respective revisions. The exact timing command was:

```sh
python3 test/rustty/bench_compare.py \
  --before /private/tmp/rustty-charset-performance-20261015/before \
  --simd /private/tmp/rustty-charset-performance-20261015/after \
  --output /private/tmp/rustty-charset-performance-20261015/results \
  --case charset_ --case feed/ascii --case stream/ascii
```

The existing runner's `simd` variant slot names the candidate here; the charset
mapping loop does not introduce SIMD. Executable filenames can differ after
dependency or toolchain changes; copy the path Cargo reports. Use a fresh
output directory when repeating a run with different binaries.

[Raw normalized samples and directional estimates](benchmarks/charset-2026-10/results.json)
and the [hashed execution manifest](benchmarks/charset-2026-10/manifest.json)
are retained with this report. Their original paths point to the local run,
whose Criterion files and logs remain in the temporary results directory.
