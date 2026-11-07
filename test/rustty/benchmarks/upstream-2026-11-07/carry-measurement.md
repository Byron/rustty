# Page resource capacity carry

Before: `3882d7da60158d3042ae33efeb5bca72fcc4b085 / nokoswn` (independent hyperlink maps). After: the capacity-carry commit recorded in `carry-commit.json`. The separate pre-map control uses frozen `06ddbdb5a / ztmmomo`, whose current source-equivalent commit is `ddfc413a6` after the SGR test-import amendment.

This ports resource projection, the four-page demand window, per-resource burst decay, aligned row fitting, and post-allocation budget pruning. A new page reuses observed resource demand instead of repeatedly growing from default capacities. Two successive empty pages drop stale demand. Clones start fresh runtime hints, matching native clone behavior. No snapshot or protocol fields change.

Apple M4 Max, Mac16,5, 16 logical cores, 64 GiB; rustc 1.95.0, release thin LTO and one codegen unit, locked/offline. Three alternating rounds used the same frozen binaries and shared bytes, with no competing builds or checks. Warmup, reset, input loading and result checks are outside the timer. Raw samples, commands, binary hashes and timestamps are in `carry-shared-timings.json` and `carry-measurement-run.json`; the generator manifest retains source, seed 42, argv and exact input SHA256.

| Shared input / byte budget | Before ms | After ms | Change | Native ms | Map + carry vs pre-map |
|---|---:|---:|---:|---:|---:|
| plain | 15.249 | 15.375 | +0.83% | 6.003 | +0.37% |
| styled-1 | 51.221 | 46.849 | -8.54% | 25.532 | -8.53% |
| styled-4 | 39.901 | 36.823 | -7.71% | 22.315 | -7.20% |
| links-full-120x80-0 | 369.227 | 370.555 | +0.36% | 49.032 | +0.49% |
| links-full-215x215-0 | 446.721 | 446.263 | -0.10% | 404.250 | +0.92% |
| links-short-120x80-10000 | 379.163 | 120.597 | -68.19% | 36.521 | -62.88% |
| links-short-120x80-50000000 | 393.150 | 156.406 | -60.22% | 38.100 | -54.42% |
| links-short-215x215-10000 | 541.499 | 123.905 | -77.12% | 38.602 | -73.33% |
| links-full-120x80-10000 | 1070.346 | 316.450 | -70.43% | 40.051 | excluded |
| links-full-120x80-50000000 | 1075.118 | 340.938 | -68.29% | 39.919 | excluded |
| links-full-215x215-10000 | 1302.375 | 311.810 | -76.06% | 36.552 | excluded |
| links-full-215x215-50000000 | 1312.968 | 327.875 | -75.03% | 36.796 | excluded |

Times are medians per complete approximately 8 MiB feed. Carry improves valid retained-link workloads by 60–77% and the style controls by 7.7–8.5%. Plain and full-link zero-history controls remain within 0.9%. Contemporaneous combined map + carry measurements resolve the intermediate map-only short-link regressions: 54–73% faster than pre-map. Old pre-map full-link retained history was incorrect, so those ratios remain excluded; the post-map carry baseline preserves the content and is valid. Both zero-history controls explicitly assert zero retained rows. These primitive results do not establish UI latency or idle CPU behavior.

Retention now follows native byte accounting. This can change retained row count under a byte limit. For example, full links at 120×80 and 50,000,000 bytes change from 14,303 rows / 50,888,704 logical page bytes to 20,639 rows / 49,496,064 bytes, exactly native. Rust page-storage estimates rise from 40,366,202 to 58,347,746 bytes because more linked contents are retained; this is not a heap-saving claim. Short links at the same size and budget change from 41,369 rows / 49,889,280 bytes to 37,740 rows / 49,971,200 bytes, also exactly native. `owned_bytes` excludes allocator metadata and is neither RSS nor peak memory. Full values remain in the summary JSON.

Correctness and validation:

- The pre-port regression failed with new-page style capacity 128 instead of 512. All 15 capacity tests now pass, including 120 independent-map layout boundary combinations, burst decay, clone reset, bounded growth and tall active-area budget floors.
- All 233 library and 450 VT integration tests pass, as do the stream-file selfcheck and both dense 8192-record reflow benchmark checks. The compression comparison now starts both policies with fresh clone hint state; a dedicated carry test verifies the clone reset separately.
- 386 exact native layout and 181 smoke comparisons pass. All 12 link and five plain/style shared-file correctness cases agree on checksums; every candidate link history count and logical page charge equals native.
- Independent fresh native comparisons pass for all 8 responses: 28 exact page-ledger observations and six full terminal observations across sustained styles/links, bursts to plain, the tall zero-byte floor and bounded recycling. Only capability advertisement order differs and remains recorded separately. Full retained states complement the timing harness viewport-only checksum.
- Release build, focused no-deps Clippy and formatting pass. The complete package run has one unrelated parity-runner failure: preserved OSC 3008 fixtures still contain the old selector payload after upstream changed the fuzz corpus. Root is refreshing all fixture groups after the ports. Six initial sandbox shared-memory test failures disappear under the authorized native OS run; no carry failure remains.

Artifacts: `carry-before-regression.log`, `carry-focused-tests.log`, `carry-full-tests.log`, `carry-clippy.log`, `carry-layout-parity.log`, `carry-dense-reflow.log`, `carry-shared-selfchecks.json`, `carry-style-selfchecks.json`, and `carry-native-checks/`.
