# Visible content format lookup

`#{C:...}` searches only the selected screen's active rows, but previously took a
full terminal snapshot first. Each lookup copied and encoded the owner's history,
and could restore compressed history unnecessarily. It now borrows the selected
screen. The existing row loop, Unicode text, trailing-space trimming, regex/glob
matching, error handling, and first matching line number are unchanged. Viewport
scrolling still does not change the searched rows.

On Linux AArch64, three adjacent before/after pairs per history size gave:

| Retained rows | Before lookup | After lookup | Before peak RSS | After peak RSS |
| --- | ---: | ---: | ---: | ---: |
| 20,000 | 12.318 ms | 11.456 µs | 54.9 MiB | 26.3 MiB |
| 100,000 | 66.493 ms | 11.696 µs | 250.1 MiB | 116.5 MiB |

Lookup numbers are medians of the three process medians. Each process measured
nine rounds of three checked lookups: a Unicode regex match, a glob match on the
second row, and a missing string. Peak RSS is the median whole-process maximum,
including creation and population of the terminal; it is not lookup allocation.
The terminal is 120 × 32 with styled Unicode history. Timing used resident owner
history, Rust 1.95.0 release builds, thin LTO, and one codegen unit. No other agents
built or tested during the measurements. These are synchronous format-expansion
measurements, not PTY throughput or rendered/visible latency.

Two focused tests passed with the repository default Rust 1.99.0:

```sh
cargo test --locked --offline -p tmuxx-terminal --lib active_screen_text
cargo test --locked --offline -p tmuxx-core --lib pane_content_formats
```

The new terminal regression separately compresses older history and verifies that
borrowing primary and alternate active rows preserves its owned byte count. It
covers a scrolled viewport, wide text, and a combining character. The existing
format regression covers visible Unicode rows and alternate-screen matching.
Every timed query checks its result; terminal history ownership and engine
sequence remain unchanged. End-to-end compressed owner scheduling is separate
from this lookup change.

The baseline is `12919ae9f2287c46f9b601963ec27dc766ee8bda`. Build both revisions
with the same [harness](content-format.rs), then run each binary for 20,000 and
100,000 rows in before/after, after/before, before/after order:

```sh
cargo +1.95.0 build --locked --offline --release -p tmuxx-core --example content-format-bench
/usr/bin/time -v -o RUN.time target/release/examples/content-format-bench 20000 9 > RUN.json
```

The retained [manifest](benchmarks/content-format-2026-11-05/manifest.json) records
source and binary hashes, toolchain, and the isolated build setup. The
[summary](benchmarks/content-format-2026-11-05/summary.json),
[order](benchmarks/content-format-2026-11-05/order.json), raw JSON samples, and
`time -v` outputs are beside it.
