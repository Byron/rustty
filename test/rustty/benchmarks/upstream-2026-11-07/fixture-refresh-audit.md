# Final frozen-fixture preservation audit

`audit-fixture-refresh.py` is a standalone, read-only Python script. It uses the existing format-1 gzip JSON-lines headers, manifests and delivery-count rule from `preserve_parity_fixtures.py`. It imports no repository modules and starts no generators, builds or oracles.

After all 12 archives are regenerated and the historical DND source patch is applied, run from the repository root:

```sh
python3 /private/tmp/rustty-reintegrate.leadij/audit-fixture-refresh.py \
  --before /private/tmp/rustty-reintegrate.leadij/fixtures-before-refresh \
  --after test/rustty/fixtures \
  --repo-root "$PWD" \
  --output /private/tmp/rustty-reintegrate.leadij/final-fixture-audit.json
```

The JSON records each group's original/current source commit and oracle hashes, archive hashes/bytes, case/comparison counts, added/removed IDs, changed request hashes, coverage counts and per-case changes, native probe changes, and source CRC/directory provenance. Native probe request hashes are indexed back to header positions and IDs; large embedded responses are not duplicated. It also checks that all current archives have one source/oracle provenance and that every recorded source CRC/directory membership matches the current repository.

The three `protocol/dnd/snapshot/rust/{0,1,2}` requests and coverage lists must equal the originals and the new generator-tracked legacy source exactly, including embedded payload text. Its original commit, oracle hashes and archive metadata must match the saved protocols provenance. The script separately requires the three `rust-current` IDs and three unchanged Zig IDs. Fresh payloads may equal or differ from historical bytes; identical data does not establish historical preservation by itself.

Exit 0 (`preserved`) means preservation and source checks passed with no review findings; it is not a runtime parity result. Exit 1 means `invalid` input/provenance/historical records or `review_required` request/probe/coverage/ID changes. Expected changes still appear for explicit review. No runtime differences, IDs, coverage tags or native fields are silently discarded.

If layout changes rename a capacity-derived case, pass `--renames path/to/reviewed-renames.json`, containing an explicit list:

```json
[
  {
    "group": "pages",
    "before": "old exact case ID",
    "after": "new exact case ID",
    "before_request_sha256": "canonical request hash from audit",
    "after_request_sha256": "canonical request hash from audit",
    "evidence": "Source function/commit and concrete old/new capacity evidence reviewed by root"
  }
]
```

The old and new IDs must actually be removed/added, hashes must match, mappings must be unique and evidence must be nonempty. A recorded rename remains visible in raw ID deltas and does not waive request or coverage review. Nothing is inferred from similar names or counts.

During preparation, only the script's tiny `--selfcheck` ran. It checks detection of lost IDs/coverage, changed probe replies, unsubstantiated renames, historical payload mutation, missing fresh IDs, stale CRCs and incorrect archive hashes. An initial macOS temporary-path alias exposed a selfcheck path-normalization issue, which was corrected.

The actual final refresh at `f2b7e1344` was subsequently audited. `final-fixture-audit.json` retains zero errors and eight review categories with `review_required` status; `final-fixture-review.md/json` separately accepts the explained source/ID/probe changes. Historical DND records remain exact. This preservation review does not claim passing runtime parity.
