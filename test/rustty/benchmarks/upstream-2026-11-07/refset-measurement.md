# Dead page-resource ID reuse

Before production tree: 1efa03d (native adapters); after production change: 66db9e1 / Tix ttmoqly. Benchmark source added unchanged in behavior on both versions: test/rustty/resource-churn-bench.rs, registered in crates/rustty-vt/benches/primitives.rs. Before regression log demonstrates five failing tests before code change; all new tests pass after. No resource-ID fixture failures appeared in the selected library/integration checks.

Both binaries use cargo +1.95.0 bench -p rustty-vt --locked --offline --bench primitives --no-run, the repository release profile (thin LTO, codegen-units=1), same macOS ARM64 machine/session. Three successive before runs followed by three successive after runs; no other agents built or measured concurrently. CPU model query unavailable in the sandbox; compiler/host/profile details retained in refset-environment.txt.

Each binary invoked with --bench rustty/resource_churn --noplot --sample-size 20 --warm-up-time 1 --measurement-time 2. 120x2 terminal, no scrollback; 16 frames overwrite the same 80 live cells using 1280 colors or explicit links. Four warm-up batches create bounded resources before timed feeds. Plain control repeats identical text. Setup and output checks are outside timing. Checks verify final cells, colors, links, cursor/default style and empty history. Workload is primitive repaint throughput, not UI latency or idle CPU.

| Workload | Before run estimates (µs) | After run estimates (µs) | Median time change |
|---|---|---|---|
| Plain | 1.0664,1.0442,1.0493 | 1.0545,1.0461,1.0503 | +0.10% (noise) |
| Styles | 214.15,212.72,214.42 | 147.35,146.26,146.93 | -31.39% (1.46× throughput) |
| Explicit hyperlinks | 1041.9,1037.5,1076.9 | 673.21,666.20,682.64 | -35.39% (1.55× throughput) |

Estimates are Criterion's central time estimate from each run; final comparison uses the median of three estimates. Between-run variation is much smaller than intended-workload improvement. All raw run output is in refset-before-{1,2,3}.log and refset-after-{1,2,3}.log; final Criterion sample/estimate JSON is retained in refset-before-samples/ and refset-after-samples/. refset-timings.json contains the parsed values and arithmetic. Saved before/after executables remain in this temporary directory for reproducibility, not for commit.

Validation: cargo +1.95.0 test -p rustty-vt --locked --offline --lib (195 passed); targeted tests page_lifecycle, hyperlinks, shared_hyperlinks, snapshot (49 passed); newly rebuilt Rust oracle against current native oracle through parity-runner --no-build (157/157 smoke comparisons). Formatting and diff whitespace checks pass.

Individual upstream commit diffs fully inspected: d03983f02 (bounded reuse),636c1d780 (PSL safety),738d8f7f5 (membership; Rust Option already safe, regression ported),38f4a710e (positive release count),31a52992f (passed-over cursor),aef8b7b38 (small capacities/reference uniqueness),5d3f65332 (docs refinement). Merge b5d9c93da has no resolution delta per root's complete remerge audit. Source copy of all seven individual diffs is refset-individual-commits.diff.
