# Canonical native style hashing and page accounting

Before: `dbfbbc932 / mokoonq`. After: `373586c / wswurtn`. This follows the final native style representation from `41557e2cc` and `de16f08dc`. Public Rust Style fields, tmux metadata, JSON restoration, and the explicit GHOSTSNP style-wire encoding remain unchanged.

Native style entries are 32 bytes aligned to 16, and the page metadata base is aligned to 16. Individual arenas and hyperlink sets retain their own 8-byte alignment. Available grid bytes are aligned downward before estimating rows. On this macOS ARM64 host, a standard 80-column page now fits 592 rows instead of 591, and a 215-column page fits 222 instead of 221. Native queries confirm the one-row boundary at columns 47970/47971.

Both frozen binaries use rustc 1.95.0, the pinned offline dependencies, and the repository thin-LTO/codegen-units=1 release/bench profile. The machine is an Apple M4 Max, Mac16,5, with 16 logical cores and 64 GiB memory. Other agents ran no builds, tests, or timings during measurement. Three rounds alternate before/after order: B/A, A/B, B/A.

The unchanged resource-churn fixture uses a 120x2 terminal, 16 frames of 80 cells and four untimed warm feeds. Input setup and checks are outside timing. Criterion uses 20 samples, one second of warmup and two seconds of measurement. The table reports the median of three Criterion sample medians, not the unrelated saved-baseline percentage printed in each Criterion log.

| Workload | Before sample medians (µs) | After sample medians (µs) | Median change |
|---|---|---|---:|
| plain | 1.0249, 1.0339, 1.0310 | 1.0175, 1.0286, 1.0398 | -0.23% |
| style | 139.6627, 139.8597, 141.6179 | 141.2325, 142.3701, 146.3671 | +1.79% |
| hyperlink | 626.9914, 627.7968, 637.1887 | 621.6701, 623.1895, 639.1275 | -0.73% |

The new hash is retained for native allocation/resource-ID compatibility; this is not a claimed Rust speedup. Styled repaint costs 1.79% more, while plain and hyperlink controls remain within 1%. Round-three drift appears in both versions. Raw samples and estimates are retained. These measurements describe primitive repaint throughput, not UI latency or idle CPU.

Regression-first validation produced the expected failure in the exact native hash test and three layout tests. Afterward, all 199 library tests and 73 related integration tests pass. Fresh native checks pass 181 smoke comparisons, 171 style-resource comparisons, and three style-collision comparisons. All three benchmark self-checks pass.

All 342 page-layout cases agree when the native map capacity is explicitly constrained to the old coupled-map request and only its additive response field is projected in scratch. This is temporary verification of style accounting. Final raw page-layout parity is required after the independent hyperlink-map port; the native adapter was not changed to hide that field.

The broader page-style suite has 381 comparisons: 120 exact passes, 249 differences solely from the existing Rust POINTER_SHAPES extension (0x8001), and 12 transient-SGR allocation differences addressed by the next port. Every native snapshot record in those 249 cases is byte-identical. The extension existed in the starting Rustty tree; this check does not claim full raw parity.

Four fixture groups were regenerated: pages (687 cases and 148 native probes), snapshot-wire (2183 cases and 14 probes), snapshots (112 cases), and page-layout (342 cases). Page coverage is preserved: 22 capacity-derived names shift with the new row capacity, producing 16 removed IDs and 16 new IDs with unchanged ordered coverage and case count. The layout group adds four cases at 47969–47972 and retains the old boundary inputs. Collision inputs were recomputed for the canonical hash, including the fixed 32-color bucket-zero cluster. Full before/after deltas and original archives remain in scratch.

Eight unrelated fixture groups remain untouched for the parent’s final complete refresh. They were already stale from earlier behavior-source changes. JSON restoration reconstructs pages and re-admits logical styles, so old serialized hash-table positions are never reused.

Artifacts include `style-measurement-run.json`, `style-timings.json`, `style-samples-*`, raw logs, fixture deltas, projected layout results, and `style-page-failure-classification.json`. No measurement artifacts entered the code commit, and no Ghostty production code was edited.
