use super::*;

#[test]
fn grouped_predicates_match_scalar_text_and_native_display_emptiness() {
    let mut values = vec![Cell::default()];
    for color in [
        Color::Indexed(0),
        Color::Indexed(255),
        Color::Rgb(0, 0, 0),
        Color::Rgb(255, 127, 1),
    ] {
        values.push(Cell::blank(color));
    }
    for cp in [None, Some(' '), Some('a'), Some('界'), Some('\u{10ffff}')] {
        for wide in [Wide::Narrow, Wide::Wide, Wide::SpacerTail, Wide::SpacerHead] {
            let mut cell = Cell::default();
            cell.set_codepoint(cp);
            cell.set_wide(wide);
            values.push(cell);
            cell.set_style_id(32767);
            cell.set_protected(true);
            cell.set_semantic(SemanticContent::Prompt);
            cell.set_hyperlink(true);
            cell.set_grapheme(true);
            values.push(cell);
        }
    }
    let scalar_empty = |cell: &Cell| {
        cell.codepoint().is_none() && cell.wide() == Wide::Narrow && cell.background().is_none()
    };
    for &left in &values {
        for &right in &values {
            let mut cells = [Cell::default(); 34];
            for (i, cell) in cells.iter_mut().enumerate() {
                *cell = if i % 2 == 0 { left } else { right };
            }
            for start in 0..2 {
                for end in start..cells.len() {
                    let slice = &cells[start..end];
                    assert_eq!(
                        Cell::has_text_any(slice),
                        slice.iter().any(|c| c.codepoint().is_some())
                    );
                    assert_eq!(Cell::is_empty_all(slice), slice.iter().all(scalar_empty));
                }
            }
        }
    }
}

#[test]
fn marking_a_row_written_clears_only_the_blank_hint_and_sets_dirty() {
    let mut row = RowHeader::new(16);
    assert!(row.has(RowHeader::BLANK));
    row.set(RowHeader::TEXT_SPANS | RowHeader::CONTINUATION, true);
    row.set_semantic(SemanticContent::Input);
    row.set(RowHeader::DIRTY, false);
    row.mark_written();
    assert!(!row.has(RowHeader::BLANK));
    assert!(row.has(RowHeader::DIRTY));
    assert!(row.has(RowHeader::TEXT_SPANS) && row.has(RowHeader::CONTINUATION));
    assert_eq!(row.semantic(), SemanticContent::Input);
    assert_eq!(row.offset(), 16);
    row.reset();
    assert!(!row.has(RowHeader::BLANK | RowHeader::MANAGED));
}
