# Conservative blank-row work skipping

Before: `db4827ff87215ff3be8fa892eac3f6ac4c4b484d / movrxsk`. The after commit is recorded in `blank-commit.json`. Frozen `blank-before-*` and `blank-after-*` binaries, hashes and exact commands remain available for the final mixed-width comparison.

A private zero-cell/no-managed-resource row hint skips repeated erases and destination scans. Mutable cell access, scalar/batch writes, copies, text spans and reflow spacer heads invalidate it; row reset and decoding establish it only when zero contents are proven. Row identity, semantic/wrap changes, sixel overwrites, tmux capture and image erasure retain their existing side effects. The hint is not serialized and the public Cell::is_empty contract is unchanged.

Same Apple M4 Max / Mac16,5 machine,16 logical cores,64 GiB; rustc1.95.0 release thinLTO/codegen-units1, locked/offline. Three alternating before/after/native rounds ran with no competing build/test/oracle work. Inputs, resets, warmup and output checks are outside the shared-file timer. All12 Erase workload/dimension variants have full approximately8MiB correctness checks; timings use complete-redraw <=1MiB prefixes. The five original ED prefixes are byte-identical to the ED port measurement. `blank-inputs/manifest.json` records exact lengths, hashes and ED2/ED22/EL2/EL0/CRLF counts; `upstream-corpora/manifest.json` records seed42 and native generator argv.

| Input | Before ms | After ms | Change | Native ms |
|---|---:|---:|---:|---:|
| plain | 15.397 | 13.979 | -9.21% | 6.012 |
| plain-styled-protected | 18.144 | 18.204 | +0.33% | 63.974 |
| styled-1 | 47.113 | 47.660 | +1.16% | 25.517 |
| styled-4 | 37.026 | 37.339 | +0.85% | 22.173 |
| wide | 11.836 | 11.859 | +0.19% | 5.473 |
| wide-spaces | 49.349 | 50.893 | +3.13% | 10.115 |
| mixed-wide | 67.248 | 69.324 | +3.09% | 10.978 |
| erase-background | 658.461 | 669.400 | +1.66% | 48.353 |
| blank-prefix-erase-2-120x80 | 631.575 | 62.555 | -90.10% | 5.794 |
| blank-prefix-erase-2-120x12 | 114.626 | 33.082 | -71.14% | 5.486 |
| blank-prefix-erase-2-300x100 | 1802.510 | 109.875 | -93.90% | 8.955 |
| blank-prefix-erase-22-120x80 | 133.438 | 51.602 | -61.33% | 15.327 |
| blank-prefix-erase-22-120x12 | 32.746 | 20.088 | -38.66% | 5.572 |
| blank-prefix-erase-22-300x100 | 328.687 | 83.359 | -74.64% | 38.766 |
| blank-prefix-erase-rate-120x80 | 266.168 | 57.815 | -78.28% | 6.241 |
| blank-prefix-erase-rate-120x12 | 53.141 | 35.276 | -33.62% | 5.791 |
| blank-prefix-erase-rate-300x100 | 777.884 | 121.903 | -84.33% | 10.710 |
| blank-prefix-erase-lines-120x80 | 108.270 | 12.369 | -88.58% | 2.741 |
| blank-prefix-erase-lines-120x12 | 48.428 | 9.033 | -81.35% | 2.891 |
| blank-prefix-erase-lines-300x100 | 285.896 | 17.143 | -94.00% | 3.107 |

Times are median per complete feed. Shared erase workloads improve34–94% and plain text improves9.2%. Wide-spaces and mixed-wide controls cost3.13% and3.09%; these are consistent measured costs, not dismissed as noise. Other print/background controls remain within1.7%. The final mixed-width port must compare against these preserved pre-blank binaries as well as its immediate baseline; no larger combined regression should be accepted.

Independent controlled driver,64 redraws/feed,120x80 (three internal rounds, line-based zero history limit):

| Case | Before ms | After ms | Change |
|---|---:|---:|---:|
| plain | 0.004 | 0.005 | +1.99% |
| erase-2 | 2.202 | 0.233 | -89.42% |
| erase-2-seen | 2.937 | 0.479 | -83.70% |
| erase-22 | 0.648 | 0.337 | -47.99% |
| erase-lines | 3.173 | 0.372 | -88.27% |

The controlled driver checks all expected cells, styles and cursor state outside timing, including a sticky prompt-seen screen. Its line-based retention limit is distinct from the native shared-file byte budget; do not compare its time directly to native. These throughput results do not measure UI latency or idle CPU.

All20 full shared inputs retain identical before/after history rows, page count, logical page charge and Rust page-storage estimate. Neither page_bytes nor owned_bytes is peak/RSS; owned_bytes omits allocator metadata. All visible checksums agree with native. Separate native full/scalar/chunked cases cover protection, black backgrounds, resources, semantic/wrap metadata, reflow and Kitty image erasure; the native tool has no sixel/tmux mode, so those paths have source/integration coverage, not claimed native parity.

Validation: the fresh pre-port unchanged-row regression fails (revisions0→12/14/16/18), while all241 library and139 affected integration tests across16 suites pass after.181 smoke and21 fresh native comparisons pass. All20 full inputs agree across before/after/native and all10 controlled selfchecks pass. Focused no-deps Clippy passes with only the already known program_status unnecessary_lazy_evaluations allowance; formatting/diff checks pass. Root handles the unrelated stale OSC3008 fixture and original Clippy warnings in the final integration pass.

Raw commands and results: `blank-validation.json`, `blank-shared-timings.json`, `blank-controlled-timings.json`, `blank-measurement-run.json`, `blank-input-selfchecks.json`, `blank-selfcheck-memory.json`, `blank-row-prep/native-review-validation.json`, and successful raw pairs in `blank-row-prep/native-review-raw/responses.jsonl`.
