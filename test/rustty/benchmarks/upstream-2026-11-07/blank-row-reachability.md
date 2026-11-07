# Blank-row port reachability checklist

Read-only review against the current Rustty tree during the style port. No builds, tests, repository edits or measurements. Function names are stable references; line numbers may shift as the earlier ports land.

## Invariant and header bookkeeping

- `packed.rs::RowHeader` currently occupies one `u64`; **bit 41 is already TEXT_SPANS**, so BLANK needs another unused bit. `MANAGED` includes Rust's text spans as well as styles, links and graphemes.
- BLANK means all physical row cell words are zero and no row-managed memory exists. A visually empty cell, a space, a colored background, a protected/semantic cell, a spacer head/tail or a tmux rendition/tab style does not prove that invariant. False negatives are safe; false positives leave content unerased.
- `RowHeader::new` is used for freshly zeroed Page cells and recycle; `RowHeader::reset` is also used after specialized nonzero fills (`fill_alignment_row`). Keep generic metadata reset distinct from proof that a row was zeroed. Native `Row.reset` clears its blank hint; `Page.resetRow` explicitly sets it after zeroing.
- Separate visual-write invalidation from dirty/revision bookkeeping. `Page::mark_row` currently marks DIRTY and bumps the physical row revision. It is called by both writes and metadata-only paths. If it becomes mark-written, `Page::reset_row` currently calls it *after* resetting the header, which would erase a newly established blank hint. An already blank EL/ED should preserve dirty/revision state when cells do not change, while required metadata cleanup still occurs.
- **`Page::mark_cell` must invalidate BLANK unconditionally.** ASCII/codepoint batch stores pass a default/zero template here even though the actual cells now contain text. Testing only the argument's bits would be incorrect. It also remains responsible for MANAGED/PLACEHOLDER hints.

## Cell writers and copies

| Entry point | Reachability / required handling |
| --- | --- |
| `Page::row_cells_mut` and `Screen::cell_mut` | Generic mutable access must invalidate before returning a slice/reference; callers can write arbitrary nonzero bits. |
| `Page::set_span_part`, `restore_span` | A Rust explicit text span is managed memory even if underlying words are blank. Existing `mark_row` call must clear BLANK. |
| `Page::erase_spans_for_write`, `repair_wide` | Can write ordinary spaces or background-colored cells and change spacer bits outside the immediate write range; invalidate when doing these writes. |
| `Page::clear_cell` | Takes a physical slot rather than logical row, releases style/link/grapheme/span payload, and writes `Cell::blank(background)`. All production callers are row-aware and must ensure invalidation/clear proof; a colored partial clear must invalidate a previously blank row. Do not find the row by dividing a slot and assume logical ordering after header rotation. |
| `Page::shift_cells`, `swap_cells`, `move_cells` | Move/swap both words and payload maps. Shift ends in `repair_wide`. Same-page `Screen::copy_row_cells` marks destination cells and repairs/marks both source and destination. Preserve or invalidate both affected rows as appropriate. |
| `Screen::set_cell_text`, `set_absolute_cell_style`, `set_cell_cursor_hyperlink` | All ultimately call `cell_mut` or `mark_cell`; include styles applied to codepoint-less cells and tmux background metadata. |
| `Screen::install_cell` | Initial raw assignment always calls `mark_cell`; later grapheme/link/style assignment calls it again after possible resource growth. Used by imports, cross-page copies, scalar reflow, JSON restoration and explicit span installation. |
| `Screen::append_*` grapheme paths | Existing raw writes end in `mark_cell`; `clear_grapheme` uses `mark_row`. Removing content can conservatively keep BLANK false. |
| `Screen::fill_alignment_row` | Fills E cells, resets the header, then calls `mark_cell`; the final call must clear any zero-row hint restored by generic reset. |
| `Screen::write_cursor_cell` | Fast one-cell replacement and wide/managed replacement loops call `mark_cell`. Previous-row spacer cleanup goes through `cell_mut`; keep both rows covered. |
| `Screen::write_cursor_ascii`, `write_cursor_codepoints` | Raw SIMD/scalar slice stores bypass `row_cells_mut` but call `mark_cell` once after a nonempty batch. The template may be zero. Future mixed-width/link batching must keep this same boundary. |
| `Screen::clear_previous_wrap_gap`, `prepare_row_shift`, `cursor_reset_wrap` | Direct spacer cleanup uses `mark_row`/`cell_mut`; header-only wrap changes need not establish or invalidate zero-cell proof. |
| `Screen::install_row` / `RowCopy` | Cross-page row rotation installs cells through `install_cell`, then restores wrap/semantic metadata and repairs wide cells. `RowCopy` currently has no blank hint. If preserving source hints, a partial copy may retain BLANK only when both copied source and untouched destination are known blank. A full copy can inherit its source proof; otherwise false is safe. |
| `Page::rebuild` | Transactional resource rebuild changes live style IDs only, for rows already marked MANAGED; it need not invalidate otherwise blank rows. Header proof must survive clone/rebuild without manufacturing true from empty-looking content. |

## Reflow, reset, rotation and serialization

- **Direct bypass:** `screen/resize.rs` around the `if x + width > cols` branch writes `page.cells[slot].set_spacer_head(true)` for the remaining wrap gap, then calls `apply_reflow_row`, which only edits header metadata/DIRTY. Explicitly invalidate BLANK in this gap-only branch. `copy_reflow_runs` and the unmanaged scalar copy branch do call `mark_cell` after their raw assignments.
- `apply_reflow_row` restores identity/wrap/semantic fields and should preserve the cell proof already determined by writers. Its metadata alone cannot prove cells are blank.
- `Page::new`, `recycle`, `reset_row`, `expose`, `truncate`, `remove_prefix` cover all page creation/reuse. Fresh/default-background exposure may preserve a known zero row; colored exposure must clear the hint. `reset_row` must release managed payload while old hints remain accurate, then set BLANK only for zero-background full clear.
- `Page::rotate_rows` rotates complete headers and row IDs without moving cell storage, using header offsets. BLANK must move with its header/offset. Its DIRTY update should preserve the hint. `PageList::{grow,truncate,remove_prefix,clone_range}` delegates to these Page operations; no separate raw cell writer exists there. Whole-page clone naturally copies the header proof.
- `snapshot.rs::Page decode`: constructs fresh Page, assigns cell words then calls `mark_cell`; normalization obtains `row_cells_mut` and can alter width/spacer bits; accepted graphemes call `mark_cell` again. Zero-count rows still pass through generic mutable normalization. If retaining/restoring blank proof for empty rows, establish it only after normalization and resource reconstruction, or keep a conservative false value. BLANK is an internal hint and is not a new GHOSTSNP field/version.
- `screen/serde.rs`: JSON restoration uses `install_cell`, then restores wrap/semantic/DIRTY fields and explicit spans. Preserving serialized dirty=false must not restore an invalid blank hint. No raw header integer is deserialized.
- Test bypass: `screen/tests.rs::native_metadata_sparse_tabs_validate_forward_across_pages` directly changes `page.cells[slot].set_codepoint(Some('x'))` without marking the row. Once the invariant is checked this setup should explicitly mark written. Other manually constructed page fixtures generally already call `mark_cell`; audit them with the same invariant rather than exempting tests.

## Consumers and side effects

- `Page::erase` is the narrow place to skip already blank, zero-background cell erases; check before span/cell scanning. Full unprotected zero clear can establish BLANK after cleanup, colored clear clears it, partial zero clear preserves the previous proof, and protected erases cannot generally establish full-row blankness.
- `Screen::erase_row_cells` also overwrites sixel regions, removes or preserves tmux row/shell metadata, tracks capture extents and calls `tmux_capture_background`. Preserve these side effects even if Page cell work is skipped. The latter may add a nondefault tmux style to apparently empty cells and must invalidate again through its existing style writer.
- `Terminal::erase_display` unprotected full-row cleanup resets wrap/continuation and semantic row metadata even when cells were already blank. Protected erases preserve row metadata. Avoid blindly returning before these operations; row-level semantic prompt state can coexist with zero cells.
- `Terminal::scroll_clear` scans rows for text, width, spacer or background content; a true BLANK hint bypasses only that expensive cell scan. The existing image/scrollback/pin policy remains unchanged.
- `screen/resize.rs::resize_height` scans trailing rows for text while protecting cursor, saved cursor, viewport and external tracked pins. BLANK can replace only the cell predicate, never the pin checks. This is the Rust counterpart of native `PageList.trimTrailingBlankRows`.
- `RowView::used` does a reverse text/background/span scan during reflow. A true hint permits zero without scanning; preserve existing semantics when false and all existing pin/continuation extension logic outside this predicate.
- ED2 prompt detection must still inspect semantic row metadata before skipping any cell scan; a prompt-marked empty row can legitimately carry BLANK. The independent OSC133-seen port remains separate.

## Focused validation matrix

Extend the existing `screen/tests.rs::assert_references` traversal to check `BLANK => every cell.bits()==0 && !MANAGED` (including TEXT_SPANS). Exercise it after fresh/reset/recycle/rotate/clone/grow, scalar and batch ASCII/Unicode/wide writes, hyperlink/grapheme/span writes, style-only/colored/protected cells, partial and complete zero/colored/protected erases, same/cross-page copies, shrink/reflow spacer gaps, snapshot and JSON restore. Check repeated zero EL/ECH/ED preserves dirty/revision state, unprotected ED still clears semantic/wrap metadata, EL preserves its required OSC133 metadata, protected ED preserves it, and tmux capture still adds the expected style. Existing reference assertions already run over many page/growth/reflow/snapshot stress cases, so use that harness instead of inventing a second integrity framework. Measure sparse erase and print controls only after invariant and fresh parity checks pass.
