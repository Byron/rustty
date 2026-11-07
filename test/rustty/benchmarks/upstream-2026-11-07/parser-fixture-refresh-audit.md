Run this only after the two accepted QA amendments have replayed to `rustty`, the native observer has been rebuilt, and all 12 archives have been regenerated.

The baseline is already preserved in `fixtures-first-final/`, extracted from immutable `03be726e243bdc235bd641420f511fc2b7792f8e` Git objects. `first-final-fixture-provenance.json` pins all 14 saved files, including the manifest and historical DND source. These are the first final refresh, distinct from the original pre-integration `fixtures-before-refresh/` copies.

```sh
python3 /private/tmp/rustty-reintegrate.leadij/audit-parser-fixture-refresh.py \
  --before /private/tmp/rustty-reintegrate.leadij/fixtures-first-final \
  --before-provenance /private/tmp/rustty-reintegrate.leadij/first-final-fixture-provenance.json \
  --after test/rustty/fixtures \
  --expected-case /private/tmp/rustty-reintegrate.leadij/osc-observer-prep/regression.json \
  --repo-root . \
  --after-commit ACTUAL_REGENERATION_SOURCE_COMMIT \
  --rust-bin target/release/examples/parity \
  --zig-bin zig-out/bin/vt-oracle \
  --output /private/tmp/rustty-reintegrate.leadij/final-parser-fixture-audit.json
```

The expected commit must be the actual source used for regeneration, not a later fixture/documentation commit. The two executable paths are read for SHA-256 checks only; this command never launches them. The auditor imports the selected `audit-fixture-refresh.py` helper from its own directory.

It requires exactly the reviewed old/new `parser/osc-cancel` records, with all case IDs and order, counts, coverage, directory membership and native probes preserved. Every group's only allowed input CRC change is `test/rustty/parity_reference.py`; all current source CRCs are checked. All nine DND snapshot records and the historical Rust DND source must remain exact. Archive hashes and metadata must match their manifests, and all new manifests must record the explicitly supplied source and executable hashes.

Only the synthetic selfcheck has run. It covers missing IDs, altered coverage, changed probes or directory membership, unexpected source CRCs, changed DND records, no requested regression and a change in the wrong group. No refreshed archive or oracle execution is claimed. Preserve the first audit and its separate accepted review unchanged; this second audit documents only the observer regression expansion and regenerated provenance.
