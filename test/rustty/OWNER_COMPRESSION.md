# Idle compression in tmuxx owners

Detached and remote tmuxx owners now compress cold primary-screen pages using the existing VT codec. The owner services one pending pane at a time in round-robin order and checks queued events before every step. Scheduled commands take priority. A completed pass disarms; metadata queries such as `#{history_bytes}` and `#{C:pattern}` leave it disarmed. There is no new worker, queue, user option or protocol version.

New and respawned owner terminals enable this policy, including retained exited panes. Local sessions and attached views retain their existing configured policy; restoring an owner snapshot does not silently enable compression in a view. Output, actual resize, host reset and history changes request maintenance. Full captures and native snapshots request reclamation after shared reads, including failed captures, rejected snapshots and the final-output snapshot barrier. Unchanged dimensions/options and pending-parser-only capture do not restart a completed pass.

The codec and logical history limits are unchanged: active and viewport pages remain resident, resources and row identities retain their native representation, and cells restore lazily. Each step scans at most eight candidate pages and compresses at most one cell buffer, capped at 1 MiB. Finding total history length and pending panes still scans metadata; this bounds codec work, not total elapsed time independently of history/pane count. Selection, styles, hyperlinks, graphemes, historical graphics anchors, snapshot metadata and capture flags survive reclamation. This is interactive scrollback; it does not add a pager-history tier.

## Real PTY measurements

The baseline is the immediate preceding commit `2b508ce4d088c83f25d663138a576a58c85e5e6a`. Both variants use Rust 1.95.0 (`59807616e`), release with thin LTO and one codegen unit, no RUSTFLAGS, on Linux ARM64 Neoverse-N2. Three adjacent pairs per case run in baseline/candidate, candidate/baseline, baseline/candidate order with other builds and benchmarks stopped. The [artifacts](benchmarks/owner-compression-2026-11-05) retain source/patch hashes, manifests, lockfile, execution order, every raw sample and `/usr/bin/time -v` results. No CPU affinity or frequency setting was changed.

Each process starts a real owner and 128×32 PTYs with a history limit of input rows + 100. The detached case uses a non-subscribing local command observer. The remote case adds normal native connections and two attached views with default configuration, plus a separate native command observer. The child writes the bulk corpus without view acknowledgments. Input is 90 bytes per styled Unicode line; actual ASCII/Unicode byte totals are recorded per run. All 36 paired executions preserved old history through full capture and a fresh native attach.

`#{history_bytes}` measures **owned primary-page storage**: `primary_screen().owned_bytes()`, the sum of `Page::storage_bytes`, including active primary pages and their cell/row/style/link/grapheme allocations. It excludes screen-level tmux capture/time/flag maps, graphics, server/controller state and other views. Sampling ends when this count has stayed unchanged for 100 ms; that does not prove all process workers or attached-view maintenance are idle. Tests separately establish finite pending passes.

Median owned primary-page storage is identical for detached and remote sources:

| Corpus | Before bytes | After bytes | Reduction |
| --- | ---: | ---: | ---: |
| 20,000 styled Unicode lines | 21,372,736 | 1,151,478 | 94.6% |
| 100,000 styled Unicode lines | 105,337,056 | 4,231,280 | 96.0% |
| 100,000 ASCII lines | 105,284,064 | 4,816,845 | 95.4% |

The source returns exactly to its cold owned count after capture and native attach in every run. Same-process RSS at initial owner-owned stability changes much less: detached 100k Unicode is 146.2 → 142.8 MB and ASCII 137.4 → 138.6 MB; remote Unicode is 578.5 → 572.6 MB and ASCII 376.3 → 379.3 MB. RSS includes the embedded owner, controller, in-process model/views and allocator retention, excluding PTY children. The remote sample observes owner-owned stability, not total-process steady state; attached views have their own maintenance cadence. These samples establish page-storage savings, not a 95–96% process-RSS reduction.

Median milliseconds, before → after:

| Owner | Lines/text | Feed to owner completion | Cold full capture | Cold native attach |
| --- | --- | ---: | ---: | ---: |
| detached | 20k Unicode | 80.20 → 80.21 | 32.19 → 43.41 | 119.61 → 129.19 |
| detached | 100k Unicode | 843.41 → 846.23 | 157.26 → 213.34 | 566.98 → 603.16 |
| detached | 100k ASCII | 159.63 → 158.33 | 97.63 → 129.62 | 366.33 → 400.58 |
| remote | 20k Unicode | 100.02 → 97.21 | 31.76 → 43.44 | 121.43 → 127.51 |
| remote | 100k Unicode | 924.56 → 945.80 | 154.73 → 211.50 | 549.30 → 583.17 |
| remote | 100k ASCII | 170.35 → 169.81 | 97.70 → 129.83 | 365.48 → 394.77 |

Feed time changes by at most +2.3% in these medians. Cold full capture costs 33–37% more (11–56 ms), and native attach costs 5–9% more (6–36 ms). They restore historical cells synchronously. This is a deliberate cold-access cost, kept separate from feed throughput and the storage benefit. No extra idle-delay tuning was introduced from these results.

The child separately times a cursor-position query/reply on an echo PTY. Forty samples follow bulk owner completion; compression may already have progressed between PTY bursts. Below, p99 is the median of each run's p99 and max is the largest observed sample across all three runs, in milliseconds:

| Owner / 100k text | Child DSR p99 before → after | Child DSR max before → after |
| --- | ---: | ---: |
| detached Unicode | 0.180 → 0.181 | 0.190 → 0.203 |
| detached ASCII | 0.169 → 0.201 | 0.218 → 0.300 |
| remote Unicode | 0.100 → 0.114 | 0.106 → 110.941 |
| remote ASCII | 0.179 → 0.178 | 0.181 → 0.196 |

The remote Unicode outlier remains in the report. Its view already has output backlog: post-feed echo-to-parsed-view maximum is 1027.7 → 695.9 ms, whereas detached echo is observed through command polling. These boundaries are different; small DSR or command medians do not establish prompt view delivery.

A second sampling loop overlaps a full cold capture and subsequent reclamation. A synchronous full capture blocks owner work: 100k Unicode echo maxima are 158.6 → 215.5 ms detached and 156.1 → 211.9 ms remote; ASCII maxima are 98.2 → 131.0 ms detached and 98.1 → 130.8 ms remote. Depending on admission ordering, that delay appears before the child issues its DSR, inside its DSR interval, or in view delivery. Raw command/DSR/echo times and timestamps are retained, including maxima because one long capture can fall outside p99. These are PTY/parser/owner/view measurements, excluding rendering and visible display latency.

Whole-job medians from `/usr/bin/time -v` for the 100k Unicode runs:

| Owner | User CPU seconds before → after | System CPU seconds before → after | Peak RSS MiB before → after |
| --- | ---: | ---: | ---: |
| detached | 1.50 → 1.61 | 0.27 → 0.27 | 573.9 → 569.5 |
| remote | 3.20 → 3.35 | 0.43 → 0.41 | 962.8 → 962.4 |

These CPU totals include fixture startup, feed, polling, full capture, native
attach, text verification and shutdown. They are not parser-only CPU or idle
maintenance CPU. Peak RSS includes transient capture/snapshot/view allocations
and is distinct from the earlier RSS samples at owner-owned stability.

## Persistent owners with four history panes

A second [paired workload](benchmarks/owner-compression-2026-11-05/multi-pane)
creates four detached 128×32 panes sequentially in one owner, each with 100,000
styled Unicode lines (9,000,000 input bytes). It uses the same baseline,
compiler/profile and AB/BA/AB order. A non-subscribing observer records all
panes' page storage and process RSS after each new pane's owned storage stays
unchanged for 100 ms. No capture or snapshot occurs before these samples. Only
after stage four does it verify both old and final text in every pane; all 24
captures passed with identical byte counts across each pair.

Median decimal MB and creation milliseconds, before → after:

| History panes | Sum of owned primary pages MB | Process RSS MB | Create this pane ms |
| ---: | ---: | ---: | ---: |
| 1 | 105.34 → 4.23 | 135.0 → 142.3 | 2.05 → 2.12 |
| 2 | 210.67 → 8.46 | 258.6 → 166.6 | 46.56 → 7.72 |
| 3 | 316.01 → 12.69 | 504.7 → 186.4 | 85.63 → 14.06 |
| 4 | 421.35 → 16.93 | 744.7 → 209.0 | 94.53 → 20.12 |

Creation times measure the command completing, separately from child-ready and
feed time. They include the real engine rollback clone when creating another
window. After four panes, same-process RSS is 71.9% lower; all three paired
ratios are retained. This demonstrates reusable storage in a persistent owner,
where later panes can reuse allocator memory and rollback copies retain cold
storage. It is not an isolated allocator benchmark or an owner-only RSS value.
Per-pane feed completion medians remain 812–860 ms and are 1.3–2.0% lower in the
candidate; no combined speedup is inferred from independent improvements.

Whole-job user CPU is 4.11 → 4.28 seconds, system CPU 0.40 → 0.27 seconds and peak
RSS 715.7 → 219.6 MiB. These include all four feeds and the later full captures,
verification and shutdown. Later cold access still has the cost shown above.

## Reproduction

Build the same harness at both revisions, copying each executable before the next build. The baseline harness can be copied from this commit into the baseline checkout together with its example entry; no production source changes are required. Original standalone path manifests and lockfile are also retained with the artifacts.

```sh
cargo +1.95.0 build --locked --offline --release -p rustty \
  --features sessions --example owner-history
python3 test/rustty/benchmarks/owner-compression-2026-11-05/run.py \
  /path/to/before /path/to/after detached 100000 unicode
```

The driver prints a new result directory and refuses reuse through the executable's new-directory requirement. Repeat for `remote`, `20000 unicode` and `100000 ascii`. Use
`detached-multi 100000 unicode` for the sequential four-pane workload. Each raw JSON records the exact corpus byte count, title/capture/snapshot checks, per-phase owned bytes and RSS, timing boundaries and individual latency samples. Do not combine these with the earlier Rust 1.99 local compression measurements as if they were one experiment.

The full-capture observer uses the existing blocking admission helper on the bounded local channel; command latency includes this wait. An initial diagnostic used the nonblocking helper and correctly received `WouldBlock` under load, so it is excluded from successful timings. A controller allocation used only to count corpus bytes was also removed before both final binaries were built, avoiding needless influence on allocator RSS. Neither correction changed production code.

## Reflow and recompression diagnostic

The [retained diagnostic](benchmarks/owner-compression-2026-11-05/reflow-diagnostic)
was recorded before the continuation at `247ad8119`, with the same Rust 1.95.0
compiler/profile and no concurrent compiler work. It is one cold/warm sample
per corpus/size, not a paired owner-policy performance claim. Its exact source,
workspace/lockfile, source/binary hashes and all ten access cases are preserved.
The ordinary VT fixture starts at 120×32, compresses history, clones it outside
timing, then resizes 120→60→120 once cold and once warm. Recompression follows
both timed roundtrips. This core workload has no PTY, tmuxx owner or renderer.

| Rows / text | Cold reflow ms | Warm reflow ms | Recompress ms | Largest recompress step µs |
| --- | ---: | ---: | ---: | ---: |
| 20k ASCII | 9.05 | 7.37 | 1.96 | 60.9 |
| 20k Unicode | 27.65 | 15.53 | 1.98 | 51.4 |
| 100k ASCII | 46.58 | 37.68 | 10.80 | 61.9 |
| 100k Unicode | 141.14 | 81.02 | 12.06 | 113.4 |
| 250k ASCII | 118.02 | 100.11 | 30.07 | 61.6 |
| 250k Unicode | 354.43 | 206.93 | 33.09 | 69.3 |

Reflow reads and rebuilds history; it remains a material synchronous operation.
Row counts survive both roundtrips, but page layout/allocation and compressed
size may change. These diagnostics do not establish an optimization or a worst
case bound. To repeat against the current core, build the existing
`history-access` example and invoke `history-access 250000 unicode reflow_roundtrip`
(and `ascii`), preserving a fresh output file and recording the exact revision.

## Validation and remaining work

The separate [shared-owner pressure report](OWNER_PRESSURE.md) checks blocked-view recovery and reconnect against every retained row ID and text. It preserves the existing native write-timeout failure separately from successful recovery.

All 194 targeted tests passed with Rust 1.95.0: 73 runtime library tests, 23 terminal wrapper tests, 48 core library tests, nine VT compression tests and 41 Rustty Connection tests using real PTYs/sockets. New tests cover event/command priority before each page, fair finite maintenance across exited/removed/respawned panes, inert metadata polling, capture success/failure, snapshot rejection and final-barrier failure, immutable queued snapshots, configured view independence, real versus unchanged resize, reset, trimming and a cold historical image anchor. Exact native snapshot/capture/selection invariants survive decompression and reclamation. The validation manifest records commands/counts; the Connection log is retained. The production `owner-history` example also passes `cargo check` with its Unix guard; the exact measured standalone source is archived unchanged.

Full-history search, capture, snapshots and reflow still read historical cells. The earlier metadata-only navigation and snapshot-style traversal commits remove avoidable work but do not make these operations constant time. Further full-search/capture traversal candidates need their own measured semantic steps. A separate compact pager tier has no demonstrated added benefit and would require a distinct retention policy. This owner policy adds no native desktop or renderer features; x86 SIMD remains unmeasured on this ARM64 host.
