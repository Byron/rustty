use rustty_vt::Terminal;

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
