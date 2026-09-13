use rustty_vt::{Selection, Terminal};

#[test]
fn selection_preserves_native_whitespace_wrap_and_wide_cell_boundaries() {
    for (text, start, end, rectangular, expected) in [
        ("", (1, 0), (6, 1), false, ""),
        ("ab\r\n \r\n", (0, 0), (7, 3), false, "ab\n"),
        ("ab\r\n\r\ncd", (0, 0), (7, 3), false, "ab\n\ncd"),
        ("abcd    ef", (0, 0), (1, 1), false, "abcd    ef"),
        ("abcd    ef", (0, 0), (7, 0), false, "abcd"),
        ("abcdefghijklmnopqr", (1, 0), (6, 1), true, "bcdefgjklmno"),
        ("界aé🙂", (1, 0), (1, 0), false, "界"),
        ("1234567界abc", (0, 0), (7, 0), false, "1234567界"),
        ("1234567界abc", (7, 0), (7, 0), false, "界"),
    ] {
        for reverse in [false, true] {
            let mut terminal = Terminal::new(8, 4, 100);
            terminal.feed(text.as_bytes());
            let screen = terminal.screen_mut();
            let mut start = screen.point(start.1, start.0).unwrap();
            let mut end = screen.point(end.1, end.0).unwrap();
            if reverse {
                std::mem::swap(&mut start, &mut end);
            }
            screen.selection = Some(Selection {
                start,
                end,
                rectangular,
            });
            assert_eq!(
                screen.selection_text().as_deref(),
                Some(expected),
                "{text:?}"
            );
        }
    }
}
