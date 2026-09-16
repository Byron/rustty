use rustty_vt::Terminal;

#[test]
fn reverse_index_outside_horizontal_margins_uses_bounded_cursor_up() {
    for column in [1, 8] {
        let mut terminal = Terminal::new(8, 5, 20);
        terminal.feed(b"top\r\ninside\r\nbelow\x1b[?69h\x1b[3;6s\x1b[2;4r");
        terminal.feed(format!("\x1b[2;{column}H").as_bytes());
        let rows = serde_json::to_value(terminal.screen()).unwrap()["rows"].clone();
        terminal.feed(b"\x1bM");
        assert_eq!(
            serde_json::to_value(terminal.screen()).unwrap()["rows"],
            rows
        );
        assert_eq!(terminal.screen().cursor.row, 1);
        assert_eq!(terminal.screen().cursor.col, column - 1);
    }
}

#[test]
fn reverse_index_preserves_pending_wrap_when_it_scrolls() {
    let mut terminal = Terminal::new(4, 3, 20);
    terminal.feed(b"abcd\x1bM");
    assert_eq!(terminal.screen().row_text(&terminal.screen().row(0)), "");
    assert_eq!(
        terminal.screen().row_text(&terminal.screen().row(1)),
        "abcd"
    );
    assert!(terminal.screen().cursor.pending_wrap);
    terminal.feed(b"X");
    assert_eq!(
        terminal.screen().row_text(&terminal.screen().row(1)),
        "Xbcd"
    );
}
