# Terminal behavior audit

Range: `6ba144158..5acdbf154`; Rustty comparison tree: `c06197283`.
Audited every source diff in the assigned groups and all 11 associated merge
remerge diffs. The remerge diffs are empty, including the intermediate merge
`0a5a6a6e1`. Git scratch objects were redirected outside the read-only `.git`.
The initial audit edited no code and ran no checks during the shared rebase.
The implementation and validation follow-up below was completed after root
granted exclusive edit ownership. `behavior-port-validation.md` records commands
and raw artifact locations; `behavior-port-commits.json` maps these results.

## Completed implementation

| Behavior | Rustty commit / Tix ID | Focused validation |
| --- | --- | --- |
| VT420 horizontal controls | `b57ffe1f1` / `ouskkly` | 46 Rust tests; 12 fresh native comparisons; root independent 600-comparison matrix |
| Right margin after scalar/grapheme wrap | `fed6e4c57` / `klmtlvn` | 34 Rust tests; 24 native comparisons including horizontal regressions |
| Formatter margin pending wrap | `ab32a792d` / `powxpsq` | 20 formatter tests; 33 native comparisons |
| Virtual placeholder targets, live cursor shrink equivalence | `9b51762eb` / `qouystx` | 7 VT/renderer tests; 66 native comparisons |
| Image number transmission order | `c43af2aec` / `nvwpkxp` | 59 Rust tests; 6 native comparisons after confirmed rebuild |
| Local image eviction transmission age | `725beff76` / `sxuolkk` | 67 Rust tests; 6 targeted native comparisons and 181 smoke comparisons |
| Line drag over blank rows | `13991acb5` / `ywqqypn` | 29 Rust tests; 75 native comparisons |
| Existing tmux guarantees | `b57fb6150` / `ouskoty` | 26 protocol/client tests; only tests changed |

Each semantic change was validated and committed separately with Codex authorship
and normal signing. The final handoff was clean on `rustty`, with no paused Tix
operation. Root retains responsibility for the final full archive refresh and
cumulative checks after the remaining application/performance work.

## Initial required ports (completed above)

1. **VT420 horizontal operations** — `44a4911e9`, merge `86d97211a`.
   Rust `terminal.rs` has no `ESC 6`/`ESC 9` or `CSI Ps ' }`/`CSI Ps ' ~`
   dispatch. Reuse `insert_blanks` and `delete_chars` over all vertical margin
   rows, gated on cursor inside all four margins; preserve row/column and clear
   pending wrap after accepted operations. DECBI/FI always clear pending wrap,
   move one column outside margin boundaries, and scroll at exact left/right
   margins only if also within vertical margins. DECFI deletes at the left
   margin, preserving the original right-margin cursor. Reject multiple CSI
   parameters/intermediates; omitted/zero count means one. Existing shared
   parser feeds Terminal for local and tmuxx terminals, so no extra host ABI is
   needed. Add tests mirroring upstream full/margin/outside cases, wide cells,
   styles/backgrounds, count defaults and rejected malformed sequences. Native
   comparisons should go through existing `vt-oracle` input/terminal surface.

2. **Pending wrap at a restored right margin** — `f761046d6`, merge `9781c0afc`.
   `formatter.rs:screen_extra` currently uses only `screen.columns - 1`.
   TerminalFormatter restores modes and margins before screen extras and can
   pass the effective wrap column privately; standalone ScreenFormatter and
   omitted mode/margin extras retain screen-edge behavior. Avoid a new public
   config field merely for this internal distinction. Scope: both screen_extra
   callers, existing formatter tests and oracle comparison cases; terminal
   exports and tmux capture consumers inherit this common fix. Upstream cases:
   `ESC[?69h ESC[;2s 00`, wide cell at margin `3;6`, and pending wrap outside
   margins at physical column 20; replay, feed `X`, compare complete content and
   cursor. Also inspect origin-mode CUP behavior while writing tests: Rust
   emits absolute row/col while mode 6 is already restored (an older parity gap,
   not introduced by this range).

3. **Recompute print right edge after wrapping** — `faa346b06`, merge `77f85cd67`.
   `Terminal::print_with_properties` computes immutable `right` from the cursor
   before wrapping. Both pending-wrap and wide-does-not-fit paths then wrap to
   the left margin but leave the limit at the physical screen edge.
   `append_grapheme` has the same stale `right` after widening at the last
   screen column. Reset it to `self.margins.right` after every print_wrap.
   `print_ascii` even has an explicit scalar fallback preserving the obsolete
   behavior; remove that guard once scalar and batch agree. Scope includes
   `print`, `print_utf8`, `print_ascii`, REP via scalar printing, grapheme widening
   and stream tests. Port all four upstream cases (wide/narrow pending wrap,
   direct wide wrap, VS16 widening), with chunked/scalar/batched comparisons.

4. **Only virtual placeholder targets** — `44385f07c`, merge `c412a3199`.
   `Graphics::placeholder_target` filters virtual placements only for zero-ID
   lookup. Its explicit-ID branch returns any placement. Add `.filter` to that
   shared branch; `rustty-render/src/graphics.rs` consumes it at line ~545 and
   both renderer backends inherit the correction. Test explicit nonvirtual and
   virtual targets, zero-ID deterministic fallback, plus renderer output to
   ensure the pinned image remains drawn once and placeholders do not duplicate
   it. Existing tests: graphics_features and
   `unicode_placeholder_targets_choose_stably_and_position_relative_children`.

5. **Stable transmission order for image numbers** — `383a6f6ae`, merge `97f422fa2`.
   `graphics/images.rs:Images::resolve_number` duplicate-number scan ranks by
   `generation`. Existing `Image.identity` is already stable across animation
   and assigned on transmission. Use that rank, keeping unique-number cache
   unchanged. Update existing tests that deliberately assert animation takeover:
   `graphics.rs:numbered_lookup_follows_animation_changes_and_eviction` and
   `graphics/images.rs` public-mutation duplicate-number tests. Calls through
   Graphics::resolve_id cover placement, delete, frame, animation, and snapshot
   loading. Retransmission must become newest; frame advance/deletion must not.
   `graphics/snapshot.rs` serializes images sorted by `(identity,id)` and rebuilds
   identities in this order, so no wire version is needed. Fresh native compare
   must include animated duplicate numbers and a restored snapshot.

6. **Stable transmission order for local image eviction** — `4441aabca`, merge
   `1fa19fa3e`. `Graphics::reserve` currently evicts `.min_by_key(generation)`.
   Use `(identity,id)` for stable age and deterministic ties; check image
   placement cleanup. The subsequent caller trace confirmed `attach_quota` is
   exclusive to tmux shared quota APIs. Its generation-ordered import and global
   quota use-order behavior (`Quota::touch`) retain their existing compatibility
   contract. Existing snapshot setup creates fresh identities in
   original identity order. Keep number-lookup semantic change and eviction in
   separate commits if root considers them independent; avoid collapsing into
   unrelated graphics cleanup. Native equivalent input: old image with 2 frames,
   newer static image, advance old animation, admit third image past quota;
   oldest image and its placements should disappear.

7. **Triple-click drag across blank lines** — `de202be94`, merge `53e55147b`.
   `selection_gesture.rs:drag` Behavior::Line does trimmed select_line on drag
   point with `?`, identical to old upstream. Add untrimmed fallback matching
   the anchor's existing fallback. Shared gesture reaches desktop input and
   autoscroll. Tests should cover forward/backward blank line extension and
   continued drag into text. Correction after follow-up source review and fresh
   replay: direct triple-click of a blank row has no untrimmed fallback in either
   engine; both return no initial selection. The earlier claimed blank-press
   parity gap was unsupported and is retracted.

## Already equivalent, with evidence

- **Live cursor after shrink** — `5ce2325ec`, merge `76329e5ad`.
  Rust Cursor stores row/column, not a tracked pin with a garbage bit.
  `screen/resize.rs` resolves mapped row identity or clamps to active top-left,
  then clamps physical row width; `graphics_place` reads a fresh live row ID
  from `screen.row(cursor.row).id`. Add focused regression reproducing no-history
  bottom-content/top-cursor shrink then place image and check it remains visible
  and attached to a live row. Existing cursor pin clamp regression is relevant
  but does not explicitly prove the Kitty consequence.

- **Tmux protocol changes** — nonmerge commits `7e7968826`, `c2d93620e`,
  `dc6ab39fb`, `9a9208188`, `7e4cc4fae`, `2ddcda8e8`, `cd00d09c6`, `51e7fe51d`;
  merges `0a5a6a6e1`, `eec45a52d`, `ae0dc1b71`.
  Existing `tmuxx-protocol::Decoder` parses byte slices without regex, preserves
  invalid UTF-8 output, uses checked u64 arithmetic for IDs, accepts empty output
  and `%exit` with optional reason. It deliberately unescapes output for
  consumers that receive decoded bytes; Ghostty's intermediate addition then
  removal of unescape is not a behavior to duplicate. Rustty's native tmuxx
  delivery is binary framed and avoids the text parsing/decoding path entirely.
  `tmuxx-client::State::notify` records active window on the named session only,
  separately records active pane, handles linked closes without deleting other
  links, requests discovery for unlinked close, and disconnects on Exit. Layout
  discovery uses Rust owned allocations rather than manually transferred Zig
  arenas, so a failed later line cannot leak earlier owned layouts.
  `dc6ab39fb` and `cd00d09c6` only cover new Zig switch arms; Rust matches already
  cover these events. Existing tests:
  `all_bytes_round_trip_at_every_fragment_size`,
  `extended_output_ignores_future_header_fields`,
  `known_notifications_preserve_names_and_unknown_records`,
  `malformed_records_fail_without_panicking`,
  `closing_one_link_does_not_delete_shared_window_or_panes`,
  `discovery_checks_ranges_references_duplicates_and_limits`,
  `same_session_duplicate_winlinks_keep_indices_and_request_discovery_when_ambiguous`.
  Add small focused cases for oversized output/window/session IDs, plain empty
  output, exit forms and cross-session active-window updates to make the exact
  incoming guarantees explicit. Fail-closed DecodeError is intentional vs
  Ghostty's drop-and-log policy; no change to that Rust API contract is needed.
  These focused tests are now committed as `b57fb6150` / `ouskoty`, alongside
  passing existing linked-close and duplicate-link coverage. The new upper-bound
  tests accept u64::MAX and reject u64::MAX+1 at every record fragmentation size;
  required session/window IDs are covered without changing the legacy optional
  session-renamed name fallback.

## Older differences outside the incoming changes

- **Local image eviction priority:** native `graphics_storage.zig:Candidate`
  ranks transient/unused, nontransient/unused, transient/placed, then
  nontransient/placed before age. The same priority code exists at the old
  `6ba144158` baseline (lines 1833–1858). Incoming `4441aabca` changes only its
  age key from generation to transmission. Rust `Graphics::reserve` now uses
  `(identity,id)` but retains its older absence of this priority tier. The fresh
  eviction regressions intentionally compare equally placed, nontransient images
  to isolate the incoming age correction. This is a known pre-range parity gap,
  not a claim of complete eviction-policy equivalence. Concrete 8-byte-budget
  witnesses transmit three 1x1 RGBA images in order 9,1,3: placing only 9, or
  marking only 1 transient with `N=1`, leaves native IDs [3,9] and Rust [1,3].
  Both witnesses reproduce in all three delivery variants; see
  `preexisting-eviction-cases.json` and `preexisting-eviction-after/`.
- **Formatter origin mode:** restoring mode 6 before emitting an absolute CUP
  remains a separate pre-range formatter replay difference. The new margin-wrap
  regression is independent of origin mode. Native fix `23c7a3e5a` (October 7)
  precedes the old base. On 20x10, input `ESC[3;8r ESC[?6h ESC[2;5H` exports final
  native CUP `2;5H` versus Rust `4;5H`; with horizontal margins 4..15 the second
  native test exports `2;3H` versus Rust `4;6H`. Both exact streams reproduce in
  all three delivery variants; see `old-gap-recheck.md` for artifact paths.

## Older findings verified resolved or equivalent

- The saved-cursor column 4-versus-5 mismatch in the October 26/28 reports was
  already fixed by November 5 commit `c170d0149`, an ancestor of the initial
  Rustty `c06197283`, now rebased as `d97e6c277` / `osvyuyn`. Exact smoke reproducer
  `terminal/resize/retained-viewport-pin-before-saved-cursor` and its original
  `generated/2/187`, two live viewport-retention variants, and snapshot
  cross-decode now pass 15/15 fresh comparisons. Both engines finish the minimal
  case at zero-based x=5,y=1,pending_wrap=false. It is not a current limitation.
- Initial line press on a blank row, and dragging from that anchor into text,
  match in 6/6 fresh comparisons. Native and Rust press paths both use trimmed
  selectLine; the untrimmed fallback belongs to dragging. The previously claimed
  separate initial-press difference is retracted.

## Suggested validation ownership

Root should rebuild the changed native oracle first and run fresh terminal,
formatter, selection-gesture and graphics comparisons after the semantic ports.
Any frozen groups touched by upstream terminal source checksums need refresh,
retaining the historical cases. Keep detailed checks and local change IDs with
the final report rather than treating this read-only audit as validation.
