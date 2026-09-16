use rustty_vt::{ScrollbackLimits, Terminal};

#[test]
fn scroll_clear_retains_rows_when_ordinary_scrollback_is_disabled() {
    for alternate in [false, true] {
        let mut terminal = Terminal::with_limits(8, 3, ScrollbackLimits::NONE);
        if alternate {
            terminal.feed(b"\x1b[?1049h");
        }
        terminal.feed(b"A\r\nB\x1b[22J");
        assert_eq!(terminal.screen().history_len(), 2);
        assert_eq!(
            terminal
                .screen()
                .row_text(&terminal.screen().physical_row(0)),
            "A"
        );
        assert_eq!(
            terminal
                .screen()
                .row_text(&terminal.screen().physical_row(1)),
            "B"
        );
        assert_eq!(terminal.screen().cursor.row, 0);
        assert_eq!(terminal.screen().cursor.col, 0);

        terminal.feed(b"C\r\nD\r\nE\r\nF");
        assert_eq!(terminal.screen().history_len(), 2);
        assert_eq!(
            terminal
                .screen()
                .row_text(&terminal.screen().physical_row(0)),
            "A"
        );
        terminal.feed(b"\x1b[3J");
        assert!(terminal.screen().history().next().is_none());
        assert_eq!(terminal.screen().history_bytes(), 0);
    }
}

#[test]
fn explicitly_disabling_scrollback_clears_retained_scroll_clear_rows() {
    let mut terminal = Terminal::with_limits(8, 3, ScrollbackLimits::NONE);
    terminal.feed(b"A\x1b[22J");
    assert_eq!(terminal.screen().history_len(), 1);
    terminal.set_limits(ScrollbackLimits::NONE);
    assert!(terminal.screen().history().next().is_none());
    assert_eq!(terminal.screen().history_bytes(), 0);
}
