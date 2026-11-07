# Package the finalized evidence selection

`package-upstream-artifacts.py` copies only a finalized, hash-verified selection plus the reviewed relocation overlays. It does not generate a report, copy the report/index, build code, run benchmarks, stage files or change draft status.

After root finishes reviewing the selection, save the final selection JSON with `"finalized": true`. This explicit input flag prevents accidentally packaging the current draft. Keep the complete `files` / `file_sha256` mapping and clear any unresolved unmatched/rejected selection candidates. Do not finalize the selection merely to test this helper; its `--selfcheck` uses temporary synthetic files.

The final command, run from the repository root, is:

```sh
python3 /private/tmp/rustty-reintegrate.leadij/package-upstream-artifacts.py \
  --selection /private/tmp/rustty-reintegrate.leadij/artifact-selection.final.json \
  --source-root /private/tmp/rustty-reintegrate.leadij \
  --overlay-root /private/tmp/rustty-reintegrate.leadij/artifact-packaging-prep \
  --destination test/rustty/benchmarks/upstream-2026-11-07
```

The destination must not exist, including as a symlink. The helper verifies every selected source hash, all reviewed original/relocated source hashes, and the unchanged-source hashes in `relocation-manifest.json` before creating the destination. Paths must remain inside their source roots. It writes the already verified bytes, preserves file modes, applies only the declared overlays, and retains `executed-sources/`, `relocation-manifest.json` and `relocation.patch`.

The generated `artifact-manifest.json` records hashes and byte counts read back from every packaged file, its selected/overlay source, the selected-source hashes and selection/relocation provenance. It explicitly excludes its own hash and size. Thus an overlaid source has its actual packaged hash, while the exact executed original remains separately verifiable. If an I/O error interrupts writing, leave that partial directory for inspection; an automatic retry will refuse it instead of overwriting it.

Root will separately compile-check the relocated driver with locked/offline dependencies and an external or reused target directory, for example:

```sh
cargo +1.95.0 build --locked --offline --release \
  --manifest-path test/rustty/benchmarks/upstream-2026-11-07/sgr-ed-driver/Cargo.toml \
  --target-dir /private/tmp/rustty-reintegrate.leadij/packaging-check-target
```

This checks relocation against current code; it does not repeat historical timings. Keep build output outside the artifact directory. Copy the finalized report and update its index separately. Before the final Tix documentation commit, inspect the exact destination and explicitly stage intended ignored logs by path where necessary; the helper never stages them automatically.

Preparation validation ran only `--selfcheck` in a temporary directory. It verifies draft refusal, selected-source and overlay hash mismatch refusal before destination creation, exact original preservation, overlay installation, destination-hash coverage excluding the manifest itself, existing-destination refusal and parent-path rejection. No real evidence package or repository destination has been created by this task, and no final QA result is implied.
