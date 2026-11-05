# Capture only the requested rows

`capture-pane` previously collected every retained row before applying `-S` and
`-E`. A visible capture could therefore restore all compressed history. Capture
now clips its primary/alternate source segments before constructing any rows,
then preallocates a vector for exactly the selected range. The formatting loop
and owner reclamation behavior remain unchanged.

Clipping precedes mapping/chaining because those iterator adapters can visit
skipped rows. Saved primary capture (`-a`), normal alternate capture with primary
history, signed line numbers, clamped/reversed ranges, viewport independence,
wrapped joins, styles, tabs, links, and capture errors retain their behavior.

## Paired measurements

Linux AArch64, Rust 1.95.0 release with thin LTO and one codegen unit, 120×32
terminal, styled Unicode/tab/link history. Each entry is the median of three
process medians, with five checked captures per case per process. Adjacent pairs
ran before/after, after/before, before/after with other CPU-heavy work paused.

| History | Source state | Capture | Before | After |
| --- | --- | --- | ---: | ---: |
| 20,000 rows | Resident | Visible | 263.5 µs | 3.39 µs |
| 20,000 rows | Cold | Visible | 1.346 ms | 3.23 µs |
| 20,000 rows | Cold | Four historical rows | 1.352 ms | 17.94 µs |
| 100,000 rows | Resident | Visible | 1.298 ms | 4.54 µs |
| 100,000 rows | Cold | Visible | 6.961 ms | 4.58 µs |
| 100,000 rows | Cold | Four historical rows | 6.941 ms | 16.54 µs |
| 20,000 rows | Resident | Full history | 15.978 ms | 15.712 ms |
| 20,000 rows | Cold | Full history | 21.058 ms | 21.323 ms |
| 100,000 rows | Resident | Full history | 80.568 ms | 80.259 ms |
| 100,000 rows | Cold | Full history | 106.855 ms | 107.401 ms |

The full cold controls increased 1.25% and 0.51%; full resident controls decreased
1.66% and 0.38%. A rejected trial formatted directly from lazy iterators and
consistently slowed full captures about 7–10%. Keeping the original contiguous
formatting path removed that regression without a threshold or tuning knob.

With 100,000 cold rows, visible capture previously restored 95,852,160 source
page bytes and now restores zero. The four-row historical capture restores only
its selected page, 371,520 bytes. The five-row history/active-boundary case also
retains cold older pages; full captures still restore all selected history.
These are source-page allocation deltas, excluding output/transient vectors,
screen maps, graphics and process RSS. Timings cover synchronous terminal
capture, not PTY throughput, runtime admission, rendering or visible latency.

## Validation and reference limits

The 26 terminal unit tests and eight nonignored capture-contract tests pass with
Rust 1.95.0. Three new regressions check exact cold-page ownership, primary and
alternate selection, signed indices, reversed/extreme bounds and clipped wrapped
lines. The two ownership regressions fail on the unchanged baseline.

An explicitly archived probe of existing `capture_reference.rs` fixtures records
317 capture results across rendition/ranges, alternate screens, hyperlink lists,
tab edits and tab reflow. Every final candidate result equals the baseline byte
for byte, including modern capture flags.

The installed C reference is tmux 3.6. Its unmodified alternate/history/missing
screen test passes. The explicitly filtered probe passes 30 rendition/range and
eight alternate comparisons. tmux 3.6 lacks `L`, `F`, `H`, and `I`; those cases
are excluded from C comparisons and retained in the Rust differential matrix.
Two additional older-reference differences reproduce on unchanged Rustty:
styled-tab overwrite retains background SGR that 3.6 omits, and tab reflow with
`-N` retains one allocated trailing blank that 3.6 omits. Their exact logs are
retained. These results do not claim a full modern C-reference suite pass.

## Reproduction

The measured baseline is `1d9a516238c7e526ce29627e658408537032beac`; the main port
follows the independent full-search commit. Use the same
[harness](capture-range.rs) for both revisions:

```sh
cargo +1.95.0 build --locked --offline --release -p tmuxx-terminal --example capture-range-bench
/usr/bin/time -v -o RUN.time target/release/examples/capture-range-bench 20000 5 > RUN.json
cargo +1.95.0 test --locked --offline -p tmuxx-terminal --lib --test capture_reference
cargo +1.95.0 check --locked --offline -p tmuxx-terminal --example capture-range-bench
```

Repeat with 100000 rows. The harness runs resident cases first, then rearms and
finishes compression outside each cold timer. Capture population, expected-byte
construction, validation and reclamation are excluded from the capture interval.
Every sample checks output, history length and terminal generation.

The [manifest](benchmarks/capture-range-2026-11-05/manifest.json) records compiler,
source/binary hashes, isolated workspace manifests and validation scope. Raw
samples, `time -v` outputs and exact process order are under
[timing](benchmarks/capture-range-2026-11-05/timing/summary.json). The rejected
trial and explicit reference-probe source, scripts and logs are retained beside
it; the scripts record the exact `/tmp` paths used in this run.
