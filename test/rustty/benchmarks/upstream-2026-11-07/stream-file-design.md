# Shared generated-file terminal benchmark

The SGR port adds a Criterion group and a standalone Rust example using the same implementation, plus a `stream_file` operation in the existing native `vt-primitives` tool. No runtime/public library API or dependency was added. Criterion, serde and serde_json were already workspace dev dependencies.

Config JSON names one input file and the terminal options:

```json
{"name":"styled-4-10000-legacy","data":"target/upstream-corpora/styled-4.vt","cols":120,"rows":80,"scrollback_bytes":10000,"grapheme_width":"legacy"}
```

Defaults: name=file, cols120, rows80, scrollback_bytes10000, grapheme_width=legacy. `unicode` sets mode2027 as a reset default in both engines. Dimensions and iteration counts must be positive, inputs nonempty and at most128MiB. Both adapters read resident input once, split it into64KiB chunks, and reset logical terminal/parser state outside every timed feed. One complete warmup precedes measurements; each replay's visible checksum is validated outside the accumulated timer. Reset cost, file I/O, checks, process startup and destruction are not reported throughput. This differs from inherited `ghostty-bench +terminal-stream`, which reads file chunks during its timed step; their numbers are not interchangeable.

Use after building current tools:

```sh
cargo +1.95.0 run --locked --offline --release -p rustty-vt --example stream-file -- CONFIG.json 3
zig-out/bin/vt-primitives stream_file CONFIG.json 3
RUSTTY_STREAM_CONFIG=CONFIG.json GHOSTTY_PRIMITIVES_BIN=zig-out/bin/vt-primitives cargo +1.95.0 bench --locked --offline -p rustty-vt --bench primitives -- stream_file
```

The Criterion group is opt-in via RUSTTY_STREAM_CONFIG; all existing benchmark groups keep their behavior. Standalone JSON includes exact input bytes per iteration, elapsed feed nanoseconds, viewport checksum and retained memory/row metrics. Before/after measurements use frozen binaries, same compiler/options/input/config, serial repeated rounds and no competing builds/tests. Individual validation invocations are not accepted timing samples.

The checksum covers cursor position/pending wrap/mode2027/cursor style, each active row's wrapping, text scalars, cell widths/spacer heads/protection/semantic content, all visual style attributes (including inline background cells), hyperlink membership/URI/explicit IDs. It omits storage IDs, revisions, dirty/blank hints, numeric implicit link identities and the cursor's open hyperlink. Focused regressions/native parity cover the omitted identity/continuation guarantees; this checksum does not claim exhaustive terminal equivalence.

`page_bytes` is Rust's native logical allocation charge and Ghostty's retained page mapping bytes. `owned_bytes` is Rust's page-storage estimate; native emits null. These exclude allocator bookkeeping and whole-process memory, do not sample peak memory, and are reported separately rather than forced equal while capacity carry differs. `history_rows` and `page_count` are retained counts after the final replay.

Validation: Rust scalar versus64KiB delivery, RIS straddling a64KiB boundary, both width defaults, three byte budgets, style/wide/combining/link/background output and visible mutation checksum checks. Native unit checks independently construct expected logical output and reset incomplete parser state/default modes. The native139-byte style/wide/link/inline-background fixture and a hand-authored24-cell table give checksum8376364026207819422, which matches both Rust and native across reset replays. Strict no-dependency Rust helper Clippy and native filtered unit test pass. App agent independently reviewed timer boundaries, checksum and reset API usage with no blocker. Initial native missing-name/empty-file inconsistencies were corrected before integration.

Shared corpus generation uses inherited ghostty-gen, seed42, exact argv/input hashes and structural validation recorded in upstream-corpora/manifest.json and upstream-workload-plan.json. Large reproducible corpora and binaries stay outside the code/documentation commits. The22 input files cover plain,1–4SGR attributes, ED2/ED22/sparse erasure/40additionalEL clears at three dimensions, short/full implicit hyperlinks, and wide/mixed UTF8 with spaces. Final port measurements identify which configurations were run; this design note is not a claim that every combination was measured.
