use rustty_vt::{ScrollbackLimits, Terminal};

#[test]
fn clearing_at_a_prompt_preserves_output_above_trailing_blank_rows() {
    for (tail, expected_history) in [("", 11), ("\r\n\x1b]133;C\x07more", 0)] {
        let mut terminal = Terminal::new(20, 15, 100);
        for row in 0..10 {
            terminal.feed(format!("output {row}\r\n").as_bytes());
        }
        terminal.feed(format!("\x1b]133;A\x07$ {tail}\x1b[H\x1b[2J").as_bytes());
        let screen = terminal.screen();
        assert_eq!(screen.history_len(), expected_history);
        if expected_history != 0 {
            assert_eq!(screen.row_text(screen.physical_row(0)), "output 0");
            assert_eq!(screen.row_text(screen.physical_row(9)), "output 9");
        }
        assert!(screen.rows().all(|row| screen.row_text(row).is_empty()));
    }
}

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
                .row_text(terminal.screen().physical_row(0)),
            "A"
        );
        assert_eq!(
            terminal
                .screen()
                .row_text(terminal.screen().physical_row(1)),
            "B"
        );
        assert_eq!(terminal.screen().cursor.row, 0);
        assert_eq!(terminal.screen().cursor.col, 0);

        terminal.feed(b"C\r\nD\r\nE\r\nF");
        assert_eq!(terminal.screen().history_len(), 2);
        assert_eq!(
            terminal
                .screen()
                .row_text(terminal.screen().physical_row(0)),
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
