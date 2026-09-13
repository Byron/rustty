use rustty_vt::Terminal;

#[test]
fn full_width_line_shifts_detach_wrapped_rows() {
    for command in ["\x1b[L", "\x1b[M", "\x1b[T", "\x1b[2;4r\x1b[S"] {
        let mut terminal = Terminal::new(8, 4, 10);
        terminal.feed(b"abcdefghijklmnopqr\r\nlast\x1b[H");
        terminal.feed(command.as_bytes());
        let start = usize::from(command.contains('r'));
        for row in &terminal.screen().rows[start..] {
            assert!(!row.wrapped && !row.wrap_continuation, "{command:?}");
        }
    }
    let mut terminal = Terminal::new(8, 4, 10);
    terminal.feed(b"abcdefghijklmnopqr\r\nlast\x1b[S");
    assert!(terminal.screen().history[0].wrapped);
    assert!(terminal.screen().rows[0].wrapped);
}

#[test]
fn partial_width_line_shifts_preserve_row_wrap_metadata() {
    for command in b"LMST" {
        let mut terminal = Terminal::new(8, 4, 10);
        terminal.feed(b"abcdefghijklmnopqr\r\nlast\x1b[?69h\x1b[3;6s\x1b[1;3H");
        let wraps: Vec<_> = terminal
            .screen()
            .rows
            .iter()
            .map(|row| (row.wrapped, row.wrap_continuation))
            .collect();
        terminal.feed(&[0x1b, b'[', *command]);
        assert_eq!(
            terminal
                .screen()
                .rows
                .iter()
                .map(|row| (row.wrapped, row.wrap_continuation))
                .collect::<Vec<_>>(),
            wraps
        );
    }
}

#[test]
fn moving_rows_removes_orphaned_wide_wrap_padding() {
    let mut terminal = Terminal::new(8, 4, 10);
    terminal.feed("\x1b[8G界\x1b[T".as_bytes());
    assert!(!terminal.screen().rows[1].cells[7].spacer_head);
    assert_eq!(terminal.screen().rows[2].cells[0].text, "界");
}

#[test]
fn margin_splits_clear_wide_text_and_preserve_surviving_attributes() {
    for command in b"LMST" {
        let mut terminal = Terminal::new(8, 4, 10);
        terminal.feed(b"\x1b[31;44m\x1b]8;id=wide;uri\x1b\\");
        for row in 1..=4 {
            terminal.feed(format!("\x1b[{row};1Ha界b界cd").as_bytes());
        }
        let before = terminal.screen().rows[0].cells.clone();
        terminal.feed(b"\x1b[?69h\x1b[3;5s\x1b[1;3H");
        terminal.feed(&[0x1b, b'[', *command]);
        for row in &terminal.screen().rows {
            for col in [1, 5] {
                let cell = &row.cells[col];
                assert!(cell.text.is_empty());
                assert_eq!(cell.width, 1);
                assert_eq!(cell.style, before[col].style);
                assert_eq!(cell.hyperlink_id, before[col].hyperlink_id);
            }
        }
    }
}
