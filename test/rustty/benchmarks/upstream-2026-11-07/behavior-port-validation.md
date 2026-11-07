# Behavior port validation

## Horizontal controls
- Baseline: two of four new horizontal_controls integration tests failed; root fresh oracle baseline12/12 failed.
- cargo +1.95.0 test -p rustty-vt --locked --offline --test horizontal_controls --test line_edits --test reverse_index:9 passed.
- cargo +1.95.0 test -p rustty-vt --locked --offline --test terminal --test feed_boundaries --test row_shifts:37 passed.
- cargo +1.95.0 build -p rustty-vt --locked --offline --example parity:pass.
- python3 test/rustty/parity_reference.py --no-build --case upstream-2026-11/dec --artifacts /private/tmp/rustty-reintegrate.leadij/horizontal-parity:12 comparisons,0 failures.
- rustfmt +1.95.0 scoped files and git diff --check:pass.

## Right margin after wrap
- Parent baseline: wide pending/direct/grapheme cases fail; narrow control passes.
- cargo +1.95.0 test -p rustty-vt --locked --offline --test grapheme_wrap --test resize_wrap --test terminal --test feed_boundaries:34 passed.
- cargo +1.95.0 build -p rustty-vt --locked --offline --example parity:pass.
- python3 test/rustty/parity_reference.py --no-build --case upstream-2026-11/ --artifacts /private/tmp/rustty-reintegrate.leadij/print-wrap-parity:24 comparisons,0 failures (12 new wrap,12 horizontal).
- Scoped rustfmt and git diff --check:pass.

## Final tmux equivalence regressions
- Added explicit max-u64/overflow pane/window/session notification IDs at every fragmentation size, omitted/empty plain output and exit records, and cross-session active-window routing.
- Extended client exit coverage to omitted/empty reasons; existing linked-window-close and duplicate-link tests retained.
- cargo +1.95.0 test -p tmuxx-protocol -p tmuxx-client --locked --offline --test control --test client:26 passed (13 protocol,13 client), no production changes required.
- Scoped rustfmt +1.95.0 and git diff --check:pass.
- Committed b57fb615033447b4662ed7a603d714555a90cda9 / ouskoty. Final handoff tree/index clean, rustty attached, no paused Tix operation. No push performed.

## Formatter pending wrap
- Parent baseline:6/9 exact margin comparisons fail; physical-edge control passes.
- cargo +1.95.0 test -p rustty-vt --locked --offline --test selection_format:20 passed, including replay+X and omitted modes/margins/standalone controls.
- Rebuilt debug parity oracle; terminal_format_requests generated all11 pending-wrap cases into formatter-pending-generated.json.
- python3 test/rustty/parity_reference.py --no-build --fixtures /private/tmp/rustty-reintegrate.leadij/formatter-pending-generated.json --artifacts /private/tmp/rustty-reintegrate.leadij/formatter-after:33 comparisons,0 failures.
- Scoped rustfmt and git diff --check:pass.
- Frozen archive regeneration intentionally deferred to root final12-group refresh after all native layout/behavior changes; generator/checksum changes make old common headers stale. Historical snapshots retained.

## Virtual placeholder targets and live cursor equivalence
- cargo +1.95.0 test -p rustty-vt --locked --offline --test graphics_history --test graphics_features:6 passed.
- cargo +1.95.0 test -p rustty-render --locked --offline --lib unicode_placeholder_targets_choose_stably_and_position_relative_children:1 passed (pinned placement counted once instead of old twice).
- Rebuilt debug parity. Existing placeholder target21 cases plus exact cursor shrink1 case generated into placeholder-cursor-generated.json.
- python3 test/rustty/parity_reference.py --no-build --fixtures /private/tmp/rustty-reintegrate.leadij/placeholder-cursor-generated.json --artifacts /private/tmp/rustty-reintegrate.leadij/placeholder-cursor-after:66 comparisons,0 failures.
- Exact shrink:12x6,nohistory,bottom x,topcursor,shrink12x3,upload/place; Rust row ID live and native anchor non-garbage. No cursor source fix needed.
- Scoped rustfmt and git diff --check:pass. Frozen archive final refresh remains root-owned.

## Image number transmission order
- cargo +1.95.0 test -p rustty-vt --locked --offline --lib graphics:37 passed.
- cargo +1.95.0 test -p rustty-vt --locked --offline --test snapshot_graphics --test graphics_features:22 passed.
- The first fresh comparison mistakenly started before the background example build completed and used the previous9b51762 binary; all6 new comparisons failed, confirming the old behavior. Those raw failure artifacts are image-number-after; they are NOT final validation.
- After waiting for successful example build, python3 test/rustty/parity_reference.py --no-build --fixtures /private/tmp/rustty-reintegrate.leadij/image-number-generated.json --artifacts /private/tmp/rustty-reintegrate.leadij/image-number-rebuilt:6 comparisons,0 failures.
- Cases exercise older frame switching and real host animation tick after newer upload, numbered placement and frame mutation. Snapshot unit proves transmission order survives reconstruction.
- Scoped rustfmt and git diff --check:pass.

## Oldest transmission eviction
- New eviction unit failed before one-line age change (old id9 retained after animation tick).
- cargo +1.95.0 test -p rustty-vt --locked --offline --lib graphics:38 passed.
- cargo +1.95.0 test -p rustty-vt --locked --offline --test snapshot_graphics --test graphics_policy --test graphics_history:29 passed.
- Rebuilt native vt-oracle and debug Rust example after optional graphics_max_bytes adapter field.
- python3 test/rustty/parity_reference.py --no-build --fixtures /private/tmp/rustty-reintegrate.leadij/image-eviction-generated.json --artifacts /private/tmp/rustty-reintegrate.leadij/image-eviction-after:6 comparisons,0 failures; both only older animated and both images animated same tick. Equally placed images isolate incoming age policy.
- python3 test/rustty/parity_reference.py --no-build --artifacts /private/tmp/rustty-reintegrate.leadij/eviction-smoke:181 comparisons,0 failures.
- Scoped rustfmt,zig fmt and git diff --check:pass.
- tmux attach_quota/shared global usage-order policy intentionally unchanged (exclusive tmux API callers). Native placed/transient priority predates4441 and is an older Rust local-eviction parity gap; incoming change only switches age ordering.

## Line selection over blank rows
- Parent baseline root blank-drag case3/3 failures.
- cargo +1.95.0 test -p rustty-vt --locked --offline --lib selection_gesture:6 passed, including actual triple-click forward/backward blank-line selection.
- cargo +1.95.0 test -p rustty-vt --locked --offline --test selection --test selection_adjust --test render_view:23 passed.
- Rebuilt debug oracle. Generated new blank-drag and24 existing line-gesture cases anchored on nonblank rows. Follow-up correction: the previously claimed separate blank initial press gap was unsupported; both engines use trimmed initial selection, and six fresh initial-press/blank-anchor comparisons match.
- python3 test/rustty/parity_reference.py --no-build --fixtures /private/tmp/rustty-reintegrate.leadij/selection-line-generated.json --artifacts /private/tmp/rustty-reintegrate.leadij/selection-line-after:75 comparisons,0 failures.
- Scoped rustfmt and git diff --check:pass.

## Follow-up checks of older findings
- No builds or source changes. Used explicitly selected existing debug/native binaries; hashes saved in old-gap-binaries.json after coordination with the performance agent outside its timing window.
- old-saved-cursor-cases.json:15/15 pass, including minimized smoke, original generated/2/187, two live grid viewport-retention variants, and snapshot cross-decode. Exact final cursor x5,y1,pending_wrapfalse. Existing d97e6c277 already fixed this before the reintegration starting tree.
- preexisting-gap-cases.json:18 comparisons; six expected origin-export differences; six blank-line initial press/anchor-drag comparisons pass. Two draft graphics requests had no observe operation, so their six passes are not evidence for image behavior; explicit observation was added in the separate follow-up below.
- preexisting-eviction-cases.json:6/6 expected pre-range differences, with actual graphics observation. Native retains images[3,9], Rust[1,3] for placed and transient-priority witnesses.
- old-gap-recheck.md records exact inputs, ancestry and current evidence. Initial blank-press gap retracted; saved-cursor gap removed from current limitations; origin-mode export and local eviction priority remain preexisting differences.
