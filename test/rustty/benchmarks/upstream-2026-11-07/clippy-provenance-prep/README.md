# Baseline Clippy provenance and minimal-fix assessment

Recommendation: repair all nine listed lint sites at their five introducing visible Tix commits. Each proposed change preserves return values, validation rules, and callback/error ordering. There is no unsafe or broad change that requires a report-only exception. Root decides whether and when to apply; this directory contains scratch patches only. No repository/index/history mutation, builds, or tests occurred.

`provenance.json` records complete current/original hashes, authors, Tix IDs, file digests, and checkpoint context. The current checkpoint has19 completed ports; `sgr-harness-amend.json` moved uxpryuy/ztmmomo/nokoswn to1e1fbf90/ddfc413a/3882d7da while preserving their visible IDs. None of these later amendments changes the older targets below.

| Lint and current location | Introducing visible change / current commit | Minimal fix and assessment |
| --- | --- | --- |
| unnecessary_lazy_evaluations, program_status.rs148 | unwwnxk /7fe765940f08ee9dade746d824bf8ef4f93fba25, `feat(rustty-vt): retain complete program status records` | Replace `.then(|| match kind { ... })` with `.then_some(match kind { ... })`. The match only compares borrowed bytes and constructs a Copy enum/None; eager evaluation has no side effects, allocation, or fallible work. Existing `.flatten()` and blocked-only result remain. The earlier note attributing this to23595b3de/stpumxk was incorrect: blame and `git log -S` identify unwwnxk. |
| manual_inspect, tmuxx-runtime/delivery.rs47 | vmwpzyz /559b5f2f4a01ac1f6b3bc94bd3e6c5b10cffc207, `fix(tmuxx): recover lagging native subscribers` | Replace identity `map_err` with `inspect_err`, retain the same atomic fetch_sub and AcqRel ordering. try_send still runs once, successful sends retain the charge, either Full or Disconnected errors subtract once and return the original message/error untouched. The early byte-limit return remains before fetch_add. |
| double_ended_iterator_last, snapshot.rs979; unscroll.rs81,145,146 | pzqlpkv /98eeb3c062e5094e2a27929fbbf09c52662a87dc, `feat(rustty-vt): restore scrollback with unscroll` | Replace the four fresh `history().last()` calls with `history().next_back()`. These tests inspect the same final historical row; they need not materialize intervening compressed rows. The replacement API is present at this amendment target (details below). |
| cloned_ref_to_slice_refs, modes.rs132 | qkrlxpx /47a3cd345d7cc6df6af4a427689de3bf73fbba36, `fix(rustty-vt): complete terminal mode status replies` | Compare `effects.as_slice()` with `std::slice::from_ref(&expected)`. Both sides stay borrowed for the assertion; expected remains usable on the next byte split, and exact element equality still runs. Removes only the unnecessary Effect/Vec clone. |
| collapsible_if, Windows notifications/tui.rs262 and310 | myywtyl /96997fc3c93cac0655e4d0ab9d20074f02eb94dd, `feat(rustty): support actionable desktop notifications` | Collapse into let-chain conditions. Activation still rejects only button values above entry.buttons before changing/removing the entry. Query still calls Group first, calls Tag only for this group, propagates COM errors in that order, and ignores unparseable hex tags. Mutex, Watch drop, callback dispatch, and query results are unchanged. |

## Iterator and amendment prerequisites

Rows gained public `DoubleEndedIterator::next_back` in13be7aed6b41df3116aaedb16f171e82f3f935c6 /ksrntwr (`perf(rustty): pack terminal pages and vectorize printing`). That commit also exposes concrete `Rows` from `Screen::history`; an opaque `impl Iterator` does not hide the trait. Git ancestry confirms ksrntwr precedes pzqlpkv. Direct inspection of pzqlpkv's screen.rs shows both the trait implementation and `pub fn history(&self) -> Rows<'_>`, so all four replacements are valid at the introducing unscroll commit.

The later13214f05f2 /yxqyqpl navigation change adds optimized nth/nth_back; it is not a prerequisite for next_back and is not the right amendment target for these warnings.

The target workspace manifests already specify edition2024 and rust-version1.95. Result::inspect_err (stable1.76), bool::then_some (1.62), std::slice::from_ref, and edition2024 let chains all fit those targets. This is source/API verification, not a compile result.

## Artifacts and checks

- `{pzqlpkv,qkrlxpx,unwwnxk,myywtyl,vmwpzyz}.patch` are minimal diffs against immutable source at each named introducing commit. Use each only after reaching the corresponding visible Tix target and confirming the tree, following the installed Tix workflow. Preserve original authors/IDs/review state; root controls the replay and validation.
- `targets/<id>/{before,after}/...` are exact target file snapshots for review. Do not copy those whole files onto the final tip: later work may have modified them.
- `current-combined.patch` is the same six-file result based on the current tip, for final diff review or a deliberate combined check; it is not a proposal for an unrelated new commit.
- All five per-target patches passed `git apply --check` against their target snapshots. The combined patch passed against the live checkout. All changed scratch target/current files parsed/formatted through rustfmt edition2024. No broad formatting churn appears in either patch set.

No new tests are needed for these source-equivalent lint repairs. Relevant existing checks if root applies them:

```sh
cargo +1.95.0 test --locked --offline -p rustty-vt --test program_status_records
cargo +1.95.0 test --locked --offline -p rustty-vt --test snapshot --test unscroll --test modes
cargo +1.95.0 test --locked --offline -p tmuxx-runtime --lib delivery::tests
cargo +1.95.0 clippy --locked --offline -p rustty-vt -p tmuxx-runtime --all-targets --no-deps -- -D warnings
```

Program-status coverage already checks known/unknown kinds and nonblocked states; delivery tests cover pending snapshots, queue failures, disconnects, and drained final messages. The existing Windows notification tests cover token/button bounds, pending/timeouts, untracked deduplication, callbacks, and nonce ownership. Root can use the established Windows cross-target check/Clippy command; actual Windows runtime checks remain unavailable in this macOS session. Do not present that cross-check as executed desktop notification validation.

Final workspace strict Clippy may reveal additional independent warnings after these blockers are removed. This assessment covers exactly the nine listed sites and does not predeclare final workspace success.
