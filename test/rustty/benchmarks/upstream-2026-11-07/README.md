# Reintegration measurement artifacts

These files preserve the November 7, 2026 reintegration's measured inputs, settings, results and validation scope. The main report explains each stage. `*-measurement-run.json` records compiler and executable hashes; raw timing records retain invocation order and individual rounds. `completed-ports.json` and `sgr-harness-amend.json` distinguish measured source identities from later Tix replays. A current checkout or similarly named executable is not automatically the measured version.

`measure-sgr.py` and `sgr-ed-driver/Cargo.toml` have packaging-only relocation edits. Their exact original bytes are retained in `executed-sources/`, with original/relocated SHA256s and the edit descriptions in `relocation-manifest.json`. The overlay changes the Cargo path, adds a standalone workspace boundary, and supplies explicit run/input paths to the Python runner. The controlled Rust source and lockfile are unchanged. The packaged controlled driver compiled successfully with Rust 1.95.0, locked offline dependencies and an external target directory against source checkpoint `96776a926`. Historical benchmarks were not rerun; their timings belong to the original executed sources.

The manifest's `unchanged_sources` means copied without relocation. Its classifier entry records a later comment clarification: classification compares stored CRC bytes but does not validate snapshot CRCs. `unchanged_source_updates` preserves the previous and current hashes; measured and executed-source hashes remain unchanged.

## Reproduction entry points

The selected corpus generator already accepts a generator executable and output directory. With a compatible native `ghostty-bench` available, regenerate the SGR inputs outside the archive:

```sh
python3 generate-upstream-corpora.py /path/to/ghostty-bench \
  --output /path/to/repro/inputs \
  --names plain styled-1 styled-2 styled-3 styled-4
```

Keep `upstream-workload-plan.json` beside the script. Its seeds, byte counts, trims and structural checks describe the inputs; compare generated hashes with `upstream-corpora/manifest.json`. Generating bytes is not terminal parity validation. Full 8 MiB corpora and 1 MiB erase prefixes are omitted from the archive; manifests preserve their derivation and hashes. `generate-shaper-fixture.py` writes the small fixture beside itself; `shaper-fixture.vt` is also included.

The controlled SGR/ED driver can be built from the repository root without joining its parent workspace:

```sh
cargo +1.95.0 build --locked --offline --release \
  --manifest-path test/rustty/benchmarks/upstream-2026-11-07/sgr-ed-driver/Cargo.toml \
  --target-dir /path/to/repro/controlled-target
```

Its CLI remains `rustty-reintegrate-perf CASE [ITERATIONS=100] [ROWS=80]`; cases include `plain`, `sgr-1` through `sgr-4`, `erase-2`, `erase-2-seen`, `erase-22` and `erase-lines`. It writes its small deterministic fixture under the manifest directory's `corpora/` before timing. A fresh build validates current code, not the old before/after measurements.

To repeat the SGR comparison, prepare the two source versions in isolated checkouts with the recorded Rust release settings. Place these five executables in a fresh run directory: `sgr-before-stream`, `sgr-after-stream`, `sgr-native-primitives`, `sgr-before-controlled`, `sgr-after-controlled`. Their roles and original hashes are recorded in `sgr-measurement-run.json`; the before reference is the canonical-style checkpoint `373586cfd`. Then run:

```sh
python3 measure-sgr.py --scratch /path/to/repro \
  --input-root /path/to/repro/inputs
```

The runner writes five relocated input configs plus new timing/metadata files into that run directory. Config dimensions 120×80, legacy width, 10,000-byte history budget, cases, iteration counts and alternating order match the executed SGR run. Only config data paths change. Use a fresh directory so historical result files remain intact. No build, input generation or reset belongs inside the measured feed interval.

## Other evidence and interpretation

`carry-native-checks/run.py` is archived execution provenance with its original machine-specific repository root; its saved results include exact page ledgers, response hashes and the initial transport-bound scope correction. Other stage measurement, prefix-preparation and capture scripts, plus `stream-configs/`, likewise retain their executed scratch paths and settings. These originals have no relocation edits or rerun claim. Production oracle interfaces and deterministic case construction are available for an adapted local replay.

The `mixed-v1` through `mixed-v8` patch, run, timing and summary files preserve rejected or superseded intermediate candidates. V9's six-case control run is provisional. The accepted mixed-width result is the separate final 11-case / 117-record matrix in `mixed-timings.json`, independently checked in `mixed-root-measurement-check.json`. `mixed-tuning-checkpoints.json` records each status and source identity. These earlier results are not substituted for the final matrix.

The linked-text artifacts likewise preserve rejected or superseded V1–V7 candidates, including V4's rejected full matrix; V2 was superseded without a timing run. V8's earlier control repeat is separate from its accepted final 29-case / 261-record matrix in `linked-timings.json`, checked in `linked-root-measurement-check.json`. `linked-tuning-checkpoints.json` records these distinctions. Final linked native validation used the Rust parity runner for 132 comparisons across 44 cases and four shards. The accepted mixed native stage and earlier linked checkpoint diagnostics used the Python reference runner. The measurement scripts only orchestrate the timed Rust and native consumers.

`completed-ports.stage23-before-final-qa.json` preserves the 23 port identities before the final two ancestor QA amendments. `completed-ports.json` adds the replayed current hashes without replacing measured or earlier pre-Clippy identities. Those files describe the earlier seven-amendment checkpoint. The final report and `observer-identity-qa-amend-results.json` record the later amendments and completed fixture, workspace and native validation separately from the initial rebase range-diff.

`classify-final-parity.py` accepts saved parity artifacts and an output path. It preserves the runner's failure count and only identifies exact common-frame/field agreement after the known Rust-only POINTER_SHAPES 0x8001 records. Its output does not turn partial coverage into a passing full suite. `final-validation-plan.md` records intended checks, not their outcome.

All pre-map full-link-history speed ratios are excluded because the old baseline loses links, even when a final viewport happens to match. The 219/300-line witnesses are retained. Page bytes follow native logical charging, while Rust owned-page estimates exclude allocator metadata; neither is peak memory or RSS. More retained rows can increase owned storage. Results measure terminal/renderer primitives and do not establish UI latency or idle CPU improvements.
