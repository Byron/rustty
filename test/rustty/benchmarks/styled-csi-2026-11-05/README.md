# Batch CSI parameters for styled scrolling

Original benchmark revision IDs are preserved; see the [history mapping](../hot-path-2026-11-05/history.md)
for their equivalents after consolidating these reports at the end of the session.

Rustty's styled ASCII scrolling is **4.7% faster** in the final comparison:
12.276 → 11.698 µs per 32-record batch. An earlier qualification run with the
identical optimized executable measured **7.9%**. Both engine orders improved
in both runs. The other 17 final workloads changed by at most 1.4%; these small
changes do not establish a speedup or equivalence.

Ghostty measured 7.654 µs in the final run. Rustty's gap narrows from 60.4% to
52.8%; further work is needed to close the remaining gap.

## Change

`Parser::advance_batched` consumes CSI parameter digits and separators without
re-entering the general state machine for each byte. It retains pending input
once per parameter run. Scalar and batched parsing share the original parameter
accumulation logic, including saturation and extended-parameter limits.
Initial colon rejection, callbacks, cancellations, and other controls retain
their existing behavior. SGR attribute admission and the packed cell layout
are unchanged.

This applies the same general idea as Kitty's `consume_csi_digit_run`
(`kitty/vt-parser.c`) and Ghostty's `consumeCsiParams`
(`src/terminal/stream.zig`). No external production code was modified.
The earlier SIMD control improved plain and styled input by almost the same
absolute amount; this change instead removes parser bookkeeping.

The existing ASCII workload has 6,144 printable bytes in 32 records, each 192
columns, on a 128×32 screen with 1,024 history rows. Styling adds 368 bytes of
SGR commands. Of these, 176 parameter bytes now use 64 capture updates rather
than 176. Input generation, priming and terminal construction are outside timing.

## Final comparison

Times are pooled medians in µs. Negative change means faster Rustty.
`simd` in the saved runner data labels the optimized build; both Rust binaries
use the normal SIMD configuration.

| Workload | Before | After | Change | Ghostty |
| --- | ---: | ---: | ---: | ---: |
| feed/ascii | 0.539 | 0.533 | -1.07% | 0.491 |
| feed/chinese | 2.992 | 2.978 | -0.46% | 408.630 |
| feed/combining | 52.786 | 53.523 | +1.40% | 401.181 |
| feed/emoji | 30.711 | 30.487 | -0.73% | 16.834 |
| stream/ascii | 7.288 | 7.344 | +0.77% | 5.511 |
| stream/chinese | 12.693 | 12.758 | +0.52% | 8.557 |
| stream/combining | 603.992 | 607.892 | +0.65% | 491.527 |
| stream/emoji | 433.360 | 434.191 | +0.19% | 749.639 |
| stream_styled/ascii | 12.276 | 11.698 | -4.71% | 7.654 |
| stream_styled/chinese | 17.694 | 17.752 | +0.33% | 34.818 |
| stream_styled/combining | 665.993 | 663.379 | -0.39% | 506.785 |
| stream_styled/emoji | 511.106 | 508.236 | -0.56% | 763.660 |
| chunked_feed_mixed/whole | 75.356 | 75.475 | +0.16% | — |
| chunked_feed_mixed/7_bytes | 117.467 | 116.605 | -0.73% | — |
| chunked_feed_mixed/4_KiB | 75.738 | 75.347 | -0.52% | — |
| chunked_stream_mixed/whole | 219.886 | 218.743 | -0.52% | — |
| chunked_stream_mixed/7_bytes | 364.862 | 361.553 | -0.91% | — |
| chunked_stream_mixed/4_KiB | 218.308 | 219.541 | +0.56% | — |

[comparison.json](comparison.json) preserves all normalized samples,
per-order 95% median confidence intervals, executable hashes and settings.
[qualification.json](qualification.json) preserves the first paired run.
The final styled ASCII medians improve by 4.86% and 4.71% in the two orders;
their before/after median confidence intervals do not overlap in either order.
The qualification order improvements are 8.68% and 6.13%.

All 18 workloads completed serially in approximately **131 seconds** after
setup. Each engine/order used 20 samples, 0.2 seconds warmup, a 0.5-second
measurement target, and 10,000 bootstrap resamples. Every process had a
30-second timeout; the invocation had a 180-second budget. Builds and tests
were stopped during timing. Ordinary desktop activity remained possible.
Criterion workspace paths in the JSON refer to the temporary run directory;
the normalized samples needed to inspect these comparisons are embedded.

Machine: Apple M4 Max, macOS 27.2. Rust 1.99.1, thin LTO, one codegen unit,
**`-C target-cpu=native`**, inherited from Cargo configuration. Ghostty:
Zig 0.17.0, ReleaseFast/native. [build.json](build.json) identifies source and
flags. The before executable is from `ea1765151`; the parser/VT source is
identical at the optimization's parent `3a37a0527`.

Reproduction after building and freezing the two Rust binaries and native
Ghostty benchmark:

```sh
python3 test/rustty/bench_compare.py \
  --before /path/to/before --simd /path/to/after \
  --ghostty /path/to/vt-primitives \
  --case rustty/feed/ --case rustty/stream/ \
  --case rustty/stream_styled/ --case rustty/chunked_ \
  --max-time 180 --output /tmp/styled-csi-comparison
```

## Rejected experiments and validation

[experiments.json](experiments.json) records the discarded candidates. Reversing
style-owner lookup, inlining color equality, deferring history-row counting,
and adding a capture-helper inline hint produced no useful gain. Direct SGR
dispatch produced only a small, inconsistent improvement. Only CSI parameter
batching is retained. The short workload cycles over 2–3 pages, limiting the
benefit available from shortening page-owner searches.

[Validation](validation.txt): 35 parser tests and 210 terminal tests passed,
including the parser corpus, SGR, batched input, and extended cursor commands.
The existing all-split scalar/batched test now also compares pending input and
covers tmux mode, zero/small/default capture budgets, initial colon rejection,
C0/DEL, cancellation/restarts, intermediates, numeric saturation and parameter
overflow. Strict Clippy, formatting of the changed crate, and whitespace checks
pass. A static review found no correctness issue.

These are headless terminal-core measurements. Application rendering, PTYs,
other CPUs, a fresh native differential run and full workspace QA were not
measured. Workspace-wide formatting reports existing differences in two
untouched files, recorded in the validation log.
