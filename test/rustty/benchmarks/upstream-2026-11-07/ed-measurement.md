# ED2 semantic prompt gate

Before: `52a8151 / uxpryuy`. After: `06ddbdb5a / ztmmomo`. Upstream `ce920296c` and connected snapshot/import behavior are tracked in the final commit inventory.

The terminal now skips the prompt-row scan when the active screen has never acquired semantic prompt/input state. The runtime bit stays true until a fresh screen or RIS. OSC, public cursor row creation, alternate-screen cursor copying, accepted streamed history, JSON/GHOSTSNP restore, and visual imports all maintain or derive it. No protocol field or version changes. Prompt-aware ED2 behavior is unchanged.

## Measurement

Apple M4 Max, Mac16,5, 16 logical cores, 64 GiB; rustc 1.95.0, release, thin LTO, one codegen unit, pinned offline dependencies. Before and after executables were frozen before the quiet window. The same generated input bytes, dimensions, legacy width mode, and 10,000-byte scrollback limit were used. Full reset, setup, and output checks are outside the timer. Three alternating before/after/native rounds; exact commands, compiler and binary hashes are in `ed-measurement-run.json` and `ed-shared-timings.json`.

ED timings use complete-CRLF prefixes of about 1 MiB from the inherited generator’s 8 MiB files. The full files were also checked before/after/native. Prefix derivation, exact lengths, SHA-256, and ED/EL/CRLF counts are in `erase-prefixes/manifest.json`; those identical prefixes will be reused for the blank-row port. Plain control remains the full 8 MiB file.

| Input | Before ms/feed | After ms/feed | Change | Native ms/feed |
|---|---:|---:|---:|---:|
| plain | 14.949 | 15.025 | +0.51% | 5.893 |
| erase-2-120x80 | 645.367 | 615.439 | -4.64% | 5.542 |
| erase-2-120x12 | 113.194 | 113.431 | +0.21% | 5.302 |
| erase-2-300x100 | 1886.991 | 1824.243 | -3.33% | 8.752 |
| erase-22-120x80 | 131.179 | 134.102 | +2.23% | 15.185 |
| erase-rate-120x80 | 283.826 | 269.832 | -4.93% | 6.182 |

These are primitive input-processing measurements, not UI latency or idle CPU measurements. The larger ED2 inputs improve 3.3–4.9%; the 12-row case has no clear gain. Plain changes +0.51%, and ED22 changes +2.23%. The separate 64-redraw fixture gives plain +2.00%, ED2 +1.99%, prompt-seen ED2 +0.13%, and ED22 +0.29%; it does not demonstrate an ED2 gain before the blank-row work. The small control shifts remain below a material regression for this change; no broad speedup claim is made. Raw rounds remain available to assess variation.

Visible output checksums, history rows, logical page charge, page count, and Rust page-storage estimates agree before/after for every shared input. Native visible checksums also agree. Page charge is neither RSS nor a peak; the Rust owned estimate excludes allocator bookkeeping. Native is substantially faster on repeated blank clears; that remaining path is covered by the next blank-row port.

## Correctness and scope

- 205 VT library tests and 58 related integration tests pass (14 semantic, 33 snapshot, 6 redraw, 4 reflow semantics, 1 snapshot continuation). Two library tests directly cover restored/streamed sticky-state invariants; seven public semantic regressions cover cursor mutation, alternate screens, JSON/binary/visual restore and recycled prompt rows.
- 181 native smoke comparisons and 33 fresh semantic comparisons pass, including two snapshot cross-decodes. Full 8 MiB input checks and five prefix checks match before/after/native; 24 controlled workload selfchecks pass.
- Focused formatting and diff checks pass. Package Clippy passes with the already-existing `program_status.rs:148` unnecessary_lazy_evaluations warning excluded; no new lint suppression was added to source.
- The native case generator gains 11 cases. Root will refresh frozen protocol fixtures with all affected groups at the final integration stage, preserving historical payloads. No frozen historical request was removed here.

Artifacts: `ed-before-*`, `ed-after-*`, `ed-shared-selfchecks.json`, `ed-prefix-selfchecks.json`, `ed-shared-timings.json`, `ed-controlled-timings.json`, `ed-measurement-run.json`, `ed2-regressions/validation.json`, and the `ed-*.log` files. Generated measurements remain outside the semantic code commit.
