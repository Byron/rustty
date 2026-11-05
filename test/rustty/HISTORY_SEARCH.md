# Full-history search traversal

`TerminalSearch` now formats a history window directly from its known page,
without resolving every row again from the beginning of the page list. A
synchronous `run` refreshes active text on entry, then feeds subsequent history
windows without repeating active/viewport matching: its exclusive terminal
borrow prevents intervening content changes. Incremental callers still control
active-dirty refresh explicitly. Native page order, cross-page overlap,
byte-to-cell coordinates, selection and screen lifecycle semantics are retained.

This is separate from the earlier metadata-only navigation step. Full search
still reads historical cells; the change removes repeated traversal and matching
work without adding an index, architecture-specific path or tuning option.

## Measurements

Linux ARM64 Neoverse-N2, Rust 1.95.0 (`59807616e`), release with thin LTO, one
codegen unit and no RUSTFLAGS. Three adjacent pairs per size/corpus use
baseline/candidate, candidate/baseline, baseline/candidate order. Other agents
paused builds and benchmarks. No affinity or CPU frequency settings changed.
The [artifacts](benchmarks/search-2026-11-05) retain all 36 raw executions,
per-pair ratios, exact compiler/source/binary hashes, patch and harness.

Both measured variants start at `247ad8119` plus the same navigation prerequisite
patch, already preserved in the navigation artifacts. The candidate adds only
this search patch. The same search gaps were rechecked when applying it at
`1d9a51623`; intervening owner/snapshot/protocol changes remain intact. These
numbers are not paired against the earlier pristine-baseline diagnostic or
against the later owner-policy workloads.

Each fresh process prepares a 120×32 terminal, feeds the corpus, compresses
history and clones it outside the measured operation. It times one synchronous
search for `line=000123` cold, then repeats it warm. Each timed call creates a
fresh `TerminalSearch` and recomputes matches; warm means the terminal retains
decoded page cells. All searches reach Complete
and return the expected single match. Milliseconds below are medians, before →
after:

| Rows | Text | Cold search ms | Warm search ms |
| ---: | --- | ---: | ---: |
| 20,000 | ASCII | 78.79 → 38.94 | 72.41 → 35.72 |
| 20,000 | Unicode | 47.07 → 23.19 | 44.71 → 21.25 |
| 100,000 | ASCII | 356.89 → 198.89 | 357.96 → 182.64 |
| 100,000 | Unicode | 226.04 → 124.86 | 216.48 → 116.44 |
| 250,000 | ASCII | 1576.12 → 548.47 | 1528.10 → 518.78 |
| 250,000 | Unicode | 1117.76 → 365.37 | 1083.93 → 343.07 |

Cold elapsed time falls 44–67% across these corpora, but 250k-row search still
holds the synchronous caller for 365–548 ms. Full search restores exactly the
same historical cell storage in both variants: 18,947,520 / 95,852,160 /
239,630,400 additional owned bytes at 20k / 100k / 250k rows. Recompression returns
every run exactly to its initial cold owned size. These are terminal-core API
elapsed times and page-storage measurements, excluding PTY delivery, renderer,
visible latency, process RSS and a separate CPU-utilization measurement.

ASCII rows are `INFO line={i:06} task={i%1000:04} file=src/terminal.rs result=complete`
plus CRLF, 65 bytes/row. Unicode rows cycle SGR 31–36 around
`line={i:06} 界面 é 👩‍💻 保存 completed`, then reset and CRLF, 61 bytes/row.
Raw setup records preserve actual bytes, page counts and initial storage.

## Validation and reproduction

All 94 focused tests passed with Rust 1.95.0: 16 search-filtered library tests,
six search tests, six selection tests, 18 selection-format tests, 33 snapshots,
one snapshot continuation and 14 graphics snapshot tests. The new regression
compares synchronous results with the independent whole-screen search across
cold history, active edits between runs, resize and an alternate-screen roundtrip.
Existing tests cover incremental feed/tick ownership, page overlap, Unicode and
whitespace coordinates, cached links, selected matches and tracked references
through reflow. The artifact validation manifest records exact commands/counts.

Use the existing `history-access` example in separate baseline/candidate
checkouts with identical toolchain/profile, preserving both binaries before
rebuilding. To reproduce the originally timed baseline exactly, start at the
recorded revision and apply the linked navigation patch, manifests and harness;
the candidate additionally applies `search.patch`.

```sh
cargo +1.95.0 build --locked --offline --release -p rustty-vt --example history-access
python3 test/rustty/benchmarks/search-2026-11-05/run.py \
  /path/to/before-history-access /path/to/after-history-access 20000
```

The runner creates and prints a fresh result directory. Repeat with `100000`
and `250000`; an optional final `ascii` or `unicode` limits a timing window.
The original exact harness is retained as `history-probe.rs`; the production
`history-access` example differs only by normal formatting. Preserve both cold
and warm values, match counts and recompressed owned bytes when comparing.
Hosts remain responsible for requesting idle reclamation after shared history
reads, as before; this step does not change maintenance scheduling or pager
retention.
