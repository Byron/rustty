# Admit the final SGR style once per sequence

Before: `373586cfd / wswurtn`. After: `52a8151 / uxpryuy`. This ports `54d162da5` and uses the pure style transformation corresponding to `73a2a1d06`. `apply_sgr` returns the fully transformed Style; its caller admits that value once through the existing rollback-safe `set_cursor_style`. Region attributes use the same transformation. Public parser and protocol fields do not change.

The new shared-file consumers reuse the existing Rust Criterion and native primitive harnesses. The Rust standalone `stream-file` example calls the same measured function. Both engines load the file before timing, feed resident 64-KiB chunks, reset terminal/parser state before every feed, and check visible output after each feed. Setup, reset, and checks are outside the reported elapsed duration. The original native `ghostty-bench +terminal-stream` includes file I/O and therefore has a different measurement boundary.

Both Rust binaries use rustc 1.95.0, the repository thin-LTO/codegen-units=1 release profile, and the pinned offline lockfile. The native helper uses the current Zig compiler with ReleaseFast. Measurements ran on the same Apple M4 Max, Mac16,5, 16 logical cores and 64 GiB memory. Other agents performed no builds, tests, or timings during the window.

The shared corpus has five files: plain text and styled output with one through four attributes per SGR. Each requests 8 MiB from the inherited generator at seed 42, then trims to the last complete CRLF. Exact generator arguments, original/final SHA256 values and trimming counts are in the corpus manifest. All measurements use 120x80 cells, legacy grapheme width and a 10,000-byte history budget. There are three rounds, reversing version order in round two. Each plain round feeds the full file 100 times; each styled round feeds it 30 times.

| Input | Before ms/feed | After ms/feed | Time change | Current native ms/feed |
|---|---:|---:|---:|---:|
| plain | 15.0883 | 15.0717 | -0.11% | 5.9446 |
| styled-1 | 50.1764 | 50.6106 | +0.87% | 25.4446 |
| styled-2 | 56.3567 | 44.2849 | -21.42% | 22.8455 |
| styled-3 | 60.6633 | 41.3428 | -31.85% | 21.9381 |
| styled-4 | 62.8324 | 39.2507 | -37.53% | 21.9090 |

Values are medians of three elapsed-duration/iteration estimates. The intended two-to-four-attribute inputs improve 21–38%; plain and single-attribute controls stay within 1%. Native remains faster on these complete feed workloads. Different retained-history lengths and page charges are reported separately; the pending capacity port changes those policies. These results measure terminal primitive throughput, not UI latency or idle CPU.

The separate controlled fixture provides a second check: 64 bounded redraws, 1,280 SGRs, RGB channels quantized in steps of 51, and four characters per styled run. It verifies every final cell/style and the cursor outside timing. Each executable reports three internal rounds, with 2,000 feeds per styled round or 20,000 plain feeds. Its zero-line history request permits the native page floor; this controlled case is not the native byte-budget comparison.

| Controlled input | Before µs/feed | After µs/feed | Time change |
|---|---:|---:|---:|
| plain | 4.4705 | 4.5002 | +0.66% |
| sgr-1 | 111.8415 | 113.9061 | +1.85% |
| sgr-2 | 152.8717 | 119.7422 | -21.67% |
| sgr-3 | 202.0259 | 138.5708 | -31.41% |
| sgr-4 | 239.3654 | 144.4942 | -39.63% |

Two focused regressions fail before the change: a style discarded by a later reset grows style capacity from 128 to 4096, and failed final admission restores an intermediate italic style instead of the pre-sequence bold style. Four new regressions pass afterward. Validation also passes 203 library tests, 75 related integration tests, the shared-file self-check, 181 fresh native smoke comparisons, and all 12 transient-style comparisons with only the pending hyperlink-capacity response field projected in scratch.

All five full shared inputs have identical before/after/native visible checksums and repeat correctly after reset. Ten controlled before/after self-checks pass. The native primitive build/test target passes, and an independently calculated 8x3 witness matches both engines at checksum 8376364026207819422. Width defaults, parser reset, omitted config defaults, invalid dimensions/iterations, and empty input are checked.

Strict Clippy reports the pre-existing `unnecessary_lazy_evaluations` at `program_status.rs:148`. A focused repeat allowing only that lint passes with all other warnings denied. Focused Rust/Zig formatting and whitespace checks pass.

The visible checksum covers text, style, widths, protection, cell semantics, row wrapping, link URI/explicit ID, cursor position/style/pending wrap, and mode 2027. It excludes retained history, storage IDs, numeric implicit link identities, and the cursor link; the parity suite covers those separately. `page_bytes` is current native page charge. `owned_bytes` estimates Rust page storage and excludes allocator metadata. Neither is peak allocation or RSS.

Raw artifacts: `sgr-measurement-run.json`, `sgr-shared-timings.json`, `sgr-shared-summary.json`, `sgr-controlled-timings.json`, `sgr-controlled-summary.json`, input self-checks, regression logs, and frozen executables in task scratch. The code commit contains no measurement artifacts.
