# Cold-history navigation

Rustty now skips rows and validates row anchors using page metadata, so reading the viewport, restoring a selected tmuxx view, formatting a short selection and starting search do not decode unrelated cold history. Iterator bounds, row IDs, physical widths, native formatting maps and selection behavior are preserved. Reading an actual historical cell still restores its page; a short historical selection restores exactly its target page.

The paired measurements use frozen revision `247ad8119a670c6c7c7967cb298437f494809fb6` and the navigation-only candidate. The same gaps were rechecked when porting onto `e982fc4c0`; the intervening snapshot limits and cursor compatibility changes remain intact. The source patch used for timing is retained with the artifacts. No full-search algorithm, owner compression policy or renderer behavior changes in this step.

## Results

Linux ARM64 Neoverse-N2, Rust 1.95.0 (`59807616e`), release with thin LTO, one codegen unit and no RUSTFLAGS. Other agents paused builds/benchmarks. Three adjacent pairs per operation/corpus used baseline/candidate, candidate/baseline, baseline/candidate order. Each run prepares a 120×32 terminal, feeds the corpus, compresses history, then clones it outside the timed operation. It measures one cold call and one warm call. There were 108 recorded executions; raw samples and per-pair ratios accompany the medians below. No CPU affinity/frequency settings were changed.

Median cold milliseconds, before → after:

| Rows | Text | Viewport | Short selection | First search feed |
| ---: | --- | ---: | ---: | ---: |
| 20,000 | ascii | 1.7887 → 0.0014 | 1.9434 → 0.0281 | 4.3658 → 2.1168 |
| 20,000 | unicode | 1.6231 → 0.0013 | 1.7181 → 0.0311 | 3.2948 → 0.9965 |
| 100,000 | ascii | 8.6115 → 0.0019 | 10.4243 → 0.1424 | 13.0477 → 1.5993 |
| 100,000 | unicode | 7.9984 → 0.0020 | 9.7421 → 0.1568 | 12.1871 → 0.8274 |
| 250,000 | ascii | 21.9724 → 0.0038 | 24.5431 → 0.3604 | 33.8749 → 3.8043 |
| 250,000 | unicode | 31.3247 → 0.0042 | 22.3997 → 0.4247 | 30.5493 → 2.5187 |

All 54 candidate operation runs and their warm repeats leave owned bytes exactly unchanged. Before the change these calls restore 18,947,520 / 95,852,160 / 239,630,400 bytes of cell storage at 20k / 100k / 250k rows. Recompression returns each terminal exactly to its original owned size. Owned bytes measure terminal allocations; these results make no RSS claim.

First-search setup still includes linear row-ID scans and active-page text formatting, taking up to 3.8 ms at 250k rows in these samples. This is not constant-time navigation. Full-history search, reflow and full snapshots necessarily read history and remain separate workloads. These timings measure terminal-core operation latency, excluding PTY transport, rendering and visible display latency.

## Reproduction

The [artifact directory](benchmarks/navigation-2026-11-05) contains the original harness, frozen workspace/lock manifests, exact source patch, compiler version, binary hashes, corpus formulas/byte counts, ordered executions and summaries. The production harness has the same behavior with normal Rust formatting.

Build the frozen baseline plus the harness/example entry and the candidate in separate checkouts using the same compiler/profile. Preserve both executable files before rebuilding:

```sh
cargo +1.95.0 build --locked --offline --release -p rustty-vt --example history-access
python3 test/rustty/benchmarks/navigation-2026-11-05/run.py \
  /path/to/baseline-history-access /path/to/candidate-history-access 20000
```

Each invocation creates and prints a fresh result directory, preserving previous raw samples. Repeat with 100000 and 250000. An optional final `ascii` or `unicode` argument narrows a timing window. Each executable accepts `<rows> <ascii|unicode> [case]`; the recorded cases are `viewport`, `selection_text_tail` and `search_first_feed`. Inspect `execution.jsonl` for run order, and compare result/again counts and before/after/recompressed owned bytes as well as times.

ASCII rows contain `INFO line={i:06} task={i%1000:04} file=src/terminal.rs result=complete` plus CRLF (65 bytes/row). Unicode rows contain cycling SGR 31–36, `line={i:06} 界面 é 👩‍💻 保存 completed`, SGR reset and CRLF (61 bytes/row). Recorded setup records preserve actual byte totals, page counts and owned memory for every run.

## Validation

New regressions verify exact cold-owned bytes for point lookup, tracked anchors, viewport iteration, selection formatting, viewport snapshots and first search feed; skipped `nth`/`nth_back` rows do not materialize discarded pages, including out-of-range and mixed-direction traversal. A historical short selection restores one page. The tmuxx view handoff regression preserves a valid historical selection and offset without warming history and drops a stale endpoint.

Main-tree validation uses Rust 1.95.0: all 170 VT library tests, 18 selection/formatting tests, six search tests, 33 snapshot tests and the tmuxx cold-selection handoff test passed (228 tests total). The production history-access example compiled and completed all ten cases for a 1,000-row Unicode smoke run; these debug timings are not benchmark results.
