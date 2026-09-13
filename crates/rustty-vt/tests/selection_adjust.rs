use rustty_vt::selection::Adjustment;
use rustty_vt::{GridPoint, Selection, Terminal};

#[test]
fn all_motions_preserve_the_logical_anchor_and_rectangle() {
    let mut terminal = Terminal::new(8, 4, 20);
    terminal.feed(b"a\x1b[1;6Hb\x1b[3;3Hc");
    let screen = terminal.screen();
    let anchor = screen.point(3, 1).unwrap();
    let end = screen.point(0, 5).unwrap();
    for rectangle in [false, true] {
        for (motion, row, col) in [
            (Adjustment::Left, 0, 0),
            (Adjustment::Right, 2, 2),
            (Adjustment::Up, 0, 0),
            (Adjustment::Down, 2, 5),
            (Adjustment::Home, 0, 0),
            (Adjustment::End, 2, 7),
            (Adjustment::PageUp, 0, 0),
            (Adjustment::PageDown, 2, 7),
            (Adjustment::BeginningOfLine, 0, 0),
            (Adjustment::EndOfLine, 0, 7),
        ] {
            let mut selection = Selection {
                start: anchor,
                end,
                rectangular: rectangle,
            };
            selection.adjust(screen, motion);
            assert_eq!(selection.start, anchor);
            assert_eq!(selection.rectangular, rectangle);
            assert_eq!(selection.end, screen.point(row, col).unwrap(), "{motion:?}");
        }
    }
    assert_eq!(screen.selection, None);
}

#[test]
fn down_counts_written_spaces_and_skips_background_only_rows() {
    let mut terminal = Terminal::new(8, 5, 20);
    terminal.feed(b"a\r\n   \x1b[44m\x1b[3;1H\x1b[2K\x1b[0m\x1b[5;1Hz");
    let screen = terminal.screen();
    let mut selection = Selection {
        start: screen.point(0, 0).unwrap(),
        end: screen.point(0, 6).unwrap(),
        rectangular: false,
    };
    selection.adjust(screen, Adjustment::Down);
    assert_eq!(selection.end, screen.point(1, 6).unwrap());
    selection.adjust(screen, Adjustment::Down);
    assert_eq!(selection.end, screen.point(4, 6).unwrap());
    selection.adjust(screen, Adjustment::Down);
    assert_eq!(selection.end, screen.point(4, 7).unwrap());
    selection.adjust(screen, Adjustment::Up);
    assert_eq!(selection.end, screen.point(3, 7).unwrap());
}

#[test]
fn horizontal_motions_skip_wide_spacers_but_line_edges_do_not() {
    let mut terminal = Terminal::new(4, 3, 20);
    terminal.feed("abc界e\u{301}".as_bytes());
    let screen = terminal.screen();
    let mut selection = Selection {
        start: screen.point(0, 0).unwrap(),
        end: screen.point(0, 2).unwrap(),
        rectangular: false,
    };
    for (motion, row, col) in [
        (Adjustment::Right, 1, 0),
        (Adjustment::Right, 1, 2),
        (Adjustment::Left, 1, 0),
        (Adjustment::Left, 0, 2),
        (Adjustment::EndOfLine, 0, 3),
    ] {
        selection.adjust(screen, motion);
        assert_eq!(selection.end, screen.point(row, col).unwrap());
    }
}

#[test]
fn page_motions_use_terminal_height_and_ignore_viewport_position() {
    let mut terminal = Terminal::new(4, 2, 20);
    terminal.feed(b"A\r\nB\r\nC\r\nD\r\nE\r\nF");
    terminal.screen_mut().scroll_viewport(3);
    let screen = terminal.screen();
    let mut selection = Selection {
        start: screen.point(5, 0).unwrap(),
        end: screen.point(2, 3).unwrap(),
        rectangular: false,
    };
    selection.adjust(screen, Adjustment::PageUp);
    assert_eq!(selection.end, screen.point(0, 3).unwrap());
    selection.adjust(screen, Adjustment::PageDown);
    assert_eq!(selection.end, screen.point(2, 3).unwrap());
    selection.adjust(screen, Adjustment::Home);
    assert_eq!(selection.end, screen.point(0, 0).unwrap());
    selection.adjust(screen, Adjustment::End);
    assert_eq!(selection.end, screen.point(5, 3).unwrap());
    assert_eq!(screen.viewport_offset, 3);
}

#[test]
fn empty_and_invalid_endpoints_remain_stable() {
    let terminal = Terminal::new(4, 2, 20);
    let screen = terminal.screen();
    let end = screen.point(1, 2).unwrap();
    let mut selection = Selection {
        start: end,
        end,
        rectangular: true,
    };
    for motion in [
        Adjustment::Left,
        Adjustment::Right,
        Adjustment::End,
        Adjustment::PageDown,
    ] {
        selection.adjust(screen, motion);
        assert_eq!(selection.end, end);
    }
    selection.end = GridPoint {
        row: u64::MAX,
        col: 0,
    };
    selection.adjust(screen, Adjustment::Home);
    assert_eq!(selection.end.row, u64::MAX);
}
