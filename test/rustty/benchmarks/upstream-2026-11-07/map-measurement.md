# Independent hyperlink cell-map capacity

Before: `06ddbdb5a / ztmmomo`. After: `cbfb73cef / nokoswn`. This ports the map/capacity portion of `d20d208c5` (merge `298f8ecb9`); its connected carry/projection changes are the next semantic port.

A page now requests linked-cell capacity separately from distinct hyperlink values. Map exhaustion grows only the map, preserving the distinct-link and string capacities. This repairs the old 26,214-cell map ceiling and restores the dense mixed-history benchmark to ordinary coverage.

## Compatibility and correctness

- Typed PageCapacity gains hyperlink_cells with default 64, including explicit-zero support. Ordinary map layouts are independent of page dimensions. Resource growth, copying, reflow, exact range accounting, and snapshot admission use the new dimension.
- GHOSTSNP keeps version 1 and its existing u16 hint: max(set bytes, ceil(cells/16)×48), saturated to 65535. Hints 65520..65535 derive enough cells for the complete page, capped at 65535. The six-field full/compact Screen JSON capacity shape is preserved; its hint restores older coupled capacities, and explicit independent fields are accepted. Existing bounded JSON resource growth remains unchanged.
- Old readers can parse the preserved wire/JSON shape, but cannot retain newly supported dense content above their old map ceiling. No universal bidirectional content-compatibility claim is made.
- Regression-first: printing 200 cells with one URL grew old distinct-link bytes 192→384; the candidate keeps 192. New tests exercise map-only growth, explicit zero, legacy JSON, reflow and 30,000-cell snapshot roundtrips.
- All 218 VT library tests and 83 affected VT integration tests pass. The old multipage reflow test now uses 1024 instead of 256 records because the independent map lets the smaller workload fit in one destination page; all retained text/styles/links remain checked. 25 tmux snapshot tests pass; native interactive suites remain ignored.
- 386 exact raw native page-layout checks and 181 smoke comparisons pass. The temporary canonical-style layout projection is no longer needed. 669 fresh hyperlink snapshot comparisons pass, including 65519/65520/65535 saturation cases on 200×150 pages.
- Both Rust and native pass the 8192-record fully linked mixed-history check after input and 128→64→128 reflow. The former opt-in expected-failing case is now included normally.
- A broad raw snapshot run reached 2685 comparisons and stopped at 20 differences. Each is independently classified as only the pre-existing optional POINTER_SHAPES record 0x8001, with all native records and all other comparable fields identical. This is partial coverage, not a blanket full-suite pass.
- Four dependent fixture groups were regenerated: page-layout 386, pages 687, snapshots 112, snapshot-wire 2186. Existing requests/probes are preserved; layout gains 44 independent-map/boundary cases and snapshot-wire gains 3 dense-link cases plus one native template. Root retains the original groups and will perform the final all-group refresh.
- Focused formatting/diff checks and lib/oracle Clippy pass with the already-existing program_status lazy-evaluation lint excluded. A pre-existing SGR test-module import warning in the harness=false Criterion build has a separate patch for amendment into the SGR commit.

## Measurement

Apple M4 Max, 16 logical cores, 64 GiB; rustc 1.95.0, release, thin LTO, codegen-units 1, locked offline dependencies. Frozen before/after executables, identical inherited 8 MiB generator inputs and explicit byte budgets. Three alternating before/after/native rounds in a quiet window. Setup, reset, warmup and output checks stay outside timing. Exact argv, hashes and raw samples are in map-shared-timings.json and map-measurement-run.json.

| Valid workload | Before ms/feed | After ms/feed | Change | Native ms/feed |
|---|---:|---:|---:|---:|
| plain | 15.200 | 15.161 | -0.26% | 5.946 |
| styled-4 | 38.786 | 38.753 | -0.09% | 21.652 |
| links-full-120x80-0 | 365.179 | 367.105 | +0.53% | 48.909 |
| links-full-215x215-0 | 440.047 | 439.899 | -0.03% | 397.674 |
| links-short-120x80-10000 | 323.220 | 373.270 | +15.48% | 36.418 |
| links-short-120x80-50000000 | 342.046 | 397.781 | +16.29% | 37.993 |
| links-short-215x215-10000 | 460.297 | 541.330 | +17.60% | 38.325 |

Plain, styled and zero-history full-link controls stay within 0.6%. Every zero-history result explicitly has history_rows=0; the label was not inferred from the budget. Short-link history inputs regress 15.5–17.6% at this intermediate stage. The map change is retained for its correctness and independent capacity support; the pending carry/projection port must settle these controls before final performance acceptance. No isolated map speedup is claimed.

The short-link 120×80, 50M case retains 34,241→41,369 history rows (+20.8%) at the same 49,889,280-byte native logical charge. Rust owned-page estimates rise 45,923,616→55,430,352 bytes because more content is retained. Logical page charge is the native accounting policy; it is not RSS or actual Rust heap savings. owned_bytes excludes allocator bookkeeping and neither field is a peak. Full-link history density still needs the upcoming carry/projection policy.

## Invalid old baselines

The candidate matches native visible checksums on all 12 full 8 MiB link combinations (short/full ×120×80/215×215 ×0/10k/50M byte limits). Before fails two final 215×215 full-link checks. More subtly, before 120×80 full-link history also loses links even though the final 8 MiB viewport happens to match: complete 219-line and 300-line prefixes expose the missing links, and candidate equals native. These witnesses are saved in map-full-prefix-checks.json with exact hashes. Therefore **all full-link history before/after speed ratios are excluded**, including the apparently faster 120×80 full run. Uncoordinated selfcheck elapsed fields are diagnostic only and are not presented as performance measurements.

These are primitive throughput measurements, not UI latency or idle CPU measurements. Future carry and batching measurements must keep the same validity distinction.

Artifacts: map-before-regression.log, map-related-tests.log, map-tmux-snapshot-tests.log, map-layout-parity.log, map-link-parity.log, map-dense-reflow.log, map-fixtures-refresh.log, map-optional-record-classification.json, map-shared-selfchecks*.json, map-full-prefix-checks.json, map-shared-timings.json, map-measurement-run.json, and frozen map-before/after-stream executables. Generated measurements stay outside the code commit.
