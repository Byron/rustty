use rustty_vt::Terminal;

#[test]
fn resize_repairs_live_and_saved_pending_wrap() {
    for (text, columns, pending_wrap, expected) in [
        ("ABCD", 6, false, "ABCDX"),
        ("ABCD", 3, false, "ABC\nDX"),
        ("ABCD", 2, true, "AB\nCD\nX"),
        ("ABCD", 4, true, "ABCD\nX"),
        ("ABCDEFGH", 6, false, "ABCDEF\nGHX"),
        ("AB界", 6, false, "AB界X"),
        ("ABC", 6, false, "ABCX"),
    ] {
        for restore in [false, true] {
            let mut terminal = Terminal::new(4, 5, 100);
            terminal.feed(text.as_bytes());
            if restore {
                terminal.feed(b"\x1b7");
            }
            terminal.resize(columns, 6);
            if restore {
                terminal.feed(b"\x1b8");
            }
            assert_eq!(
                terminal.screen().cursor.pending_wrap,
                pending_wrap,
                "text={text:?}, columns={columns}, restore={restore}",
            );
            terminal.feed(b"X");
            let screen = terminal.screen();
            let actual = screen
                .rows()
                .map(|row| screen.row_text(row))
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(
                actual.trim_end_matches('\n'),
                expected,
                "text={text:?}, columns={columns}, restore={restore}",
            );
        }
    }
}
