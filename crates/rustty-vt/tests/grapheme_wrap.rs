use rustty_vt::Terminal;

#[test]
fn printing_beyond_right_margin_uses_margin_after_each_kind_of_wrap() {
    for bytewise in [false, true] {
        for (text, expected) in [
            ("\x1b[1;4H  🇦", "🇦"),
            ("\x1b[1;5H🇦", "🇦"),
            ("\x1b[?2027h\x1b[1;5H☝\u{fe0f}", "☝\u{fe0f}"),
            ("\x1b[1;5HABC", "BC"),
        ] {
            let mut t = Terminal::new(5, 3, 0);
            t.feed(b"\x1b[?69h\x1b[1;2s");
            for part in text
                .as_bytes()
                .chunks(if bytewise { 1 } else { text.len() })
            {
                t.feed(part);
            }
            assert_eq!(
                (t.screen().cursor.row, t.screen().cursor.col),
                (1, 1),
                "{text:?}"
            );
            assert!(t.screen().cursor.pending_wrap, "{text:?}");
            assert_eq!(t.screen().row_text(t.screen().row(1)), expected);
            t.feed(b"X");
            assert_eq!((t.screen().cursor.row, t.screen().cursor.col), (2, 1));
            assert_eq!(t.screen().row_text(t.screen().row(2)), "X");
        }
    }
}

#[test]
fn widening_grapheme_below_scroll_region_keeps_its_suffix() {
    let mut terminal = Terminal::new(3, 3, 100);
    terminal.feed("\x1b[?2027h\x1b[1;2r\x1b[3;3H❤\u{200d}👍".as_bytes());
    let screen = terminal.screen();
    assert_eq!(screen.row(2).text(0).as_str(), "❤\u{200d}👍");
    assert_eq!(screen.row(2).cells[0].width(), 2);
}

#[test]
fn widening_grapheme_on_single_row_preserves_cluster_and_history() {
    for alternate in [false, true] {
        for margins in [false, true] {
            let mut terminal = Terminal::new(5, 1, 100);
            terminal.feed(b"\x1b[?2027h");
            if alternate {
                terminal.feed(b"\x1b[?1049h");
            }
            terminal.feed("\x1b[1;5Ha\u{301}\x1bD".as_bytes());
            if margins {
                terminal.feed(b"\x1b[?69h\x1b[3;5s");
            }
            terminal.feed("\x1b[1;5H❤\u{200d}👍".as_bytes());
            let screen = terminal.screen();
            let column = if margins { 2 } else { 0 };
            assert_eq!(
                screen.row(0).text(column).as_str(),
                "❤\u{200d}👍",
                "alternate={alternate}, margins={margins}",
            );
            if !alternate {
                assert_eq!(
                    screen.physical_row(0).text(4).as_str(),
                    "a\u{301}",
                    "margins={margins}",
                );
            }
        }
    }
}
