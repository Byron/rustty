use rustty_vt::{Color, SemanticContent, Style, Terminal};

#[test]
fn alignment_pattern_retains_colors_and_resets_other_style_attributes() {
    let mut terminal = Terminal::new(8, 4, 20);
    terminal.feed(b"\x1b[1;2;3;4;5;7;8;9;53;31;44;58;5;2m\x1b#8");
    let expected = Style {
        foreground: Color::Indexed(1),
        background: Color::Indexed(4),
        ..Style::default()
    };
    assert_eq!(terminal.screen().cursor.style, expected);
    for row in &terminal.screen().rows {
        for (col, cell) in row.cells.iter().enumerate() {
            assert_eq!(&*terminal.screen().cell_text(row, col), "E");
            assert_eq!(cell.style, expected);
        }
    }
}

#[test]
fn alignment_pattern_resets_margins_and_row_metadata_but_keeps_cursor_state() {
    let mut terminal = Terminal::new(8, 4, 20);
    terminal.feed(b"\x1b]133;P\x07\x1bV\x1b]8;id=current;uri\x07abcdefghijklmnopqr");
    terminal.feed(b"\x1b[?69h\x1b[2;6s\x1b[2;4r\x1b[?6h\x1b#8");
    assert!(!terminal.modes.dec(6));
    assert_eq!((terminal.margins.top, terminal.margins.bottom), (0, 3));
    assert_eq!((terminal.margins.left, terminal.margins.right), (0, 7));
    let cursor = &terminal.screen().cursor;
    assert_eq!((cursor.row, cursor.col), (0, 0));
    assert!(cursor.protected);
    assert!(cursor.hyperlink.is_some());
    assert_eq!(cursor.semantic, SemanticContent::Prompt);
    for row in &terminal.screen().rows {
        assert!(!row.wrapped && !row.wrap_continuation);
        assert_eq!(row.semantic, SemanticContent::Output);
        assert!(row.cells.iter().enumerate().all(|(col, cell)| {
            &*terminal.screen().cell_text(row, col) == "E"
                && cell.width == 1
                && !cell.protected
                && cell.hyperlink.is_none()
        }));
    }
    terminal.feed(b"X");
    assert!(terminal.screen().rows[0].cells[0].protected);
    assert!(terminal.screen().rows[0].cells[0].hyperlink.is_some());
}
