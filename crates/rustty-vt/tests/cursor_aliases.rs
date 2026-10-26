use rustty_vt::Terminal;

#[test]
fn backward_aliases_move_left_and_up_with_default_counts() {
    let mut terminal = Terminal::new(8, 5, 0);
    terminal.feed(b"\x1b[4;6H\x1b[2j\x1b[0k");
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (2, 3)
    );
    terminal.feed(b"\x1b[;k\x1b[j");
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (1, 2)
    );

    let mut terminal = Terminal::new(16, 3, 0);
    terminal.feed(b"o\x1b[j\\\x1b[I");
    assert_eq!(
        &*terminal.screen().cell_text(terminal.screen().row(0), 0),
        "\\"
    );
    assert_eq!(terminal.screen().cursor.col, 8);
}

#[test]
fn backward_aliases_reject_extra_parameters() {
    let mut terminal = Terminal::new(8, 5, 0);
    terminal.feed(b"\x1b[4;6H");
    let cursor = terminal.screen().cursor.clone();
    terminal.feed(b"\x1b[1;2j\x1b[1;2k\x1b[1:2j\x1b[1:2k");
    assert_eq!(terminal.screen().cursor, cursor);
}

#[test]
fn backward_moves_stop_at_left_margin_unless_already_left_of_it() {
    for sequence in [b"\x1b[99D".as_slice(), b"\x1b[99j", b"\x08"] {
        for (column, expected) in [(3, 2), (2, 0)] {
            let mut terminal = Terminal::new(8, 3, 0);
            terminal.feed(b"\x1b[?69h\x1b[3;7s");
            terminal.cursor_position(1, column);
            terminal.feed(sequence);
            assert_eq!(terminal.screen().cursor.col, expected, "{sequence:?}");
            assert!(!terminal.screen().cursor.pending_wrap);
        }
    }
    let mut terminal = Terminal::new(8, 3, 0);
    terminal.feed(b"\x1b[?69h\x1b[3;7s\x1b[1;6H\x1b[99D");
    assert_eq!(terminal.screen().cursor.col, 2);
}

#[test]
fn single_axis_cursor_positions_apply_origin_offsets_once() {
    for (sequence, expected) in [
        (b"\x1b[1G".as_slice(), (3, 3)),
        (b"\x1b[1`", (3, 3)),
        (b"\x1b[1d", (2, 4)),
        (b"\x1b[2a", (3, 6)),
        (b"\x1b[2e", (5, 4)),
        (b"\x1b[99a", (3, 8)),
        (b"\x1b[99e", (7, 4)),
    ] {
        let mut terminal = Terminal::new(10, 10, 0);
        terminal.feed(b"\x1b[?69h\x1b[4;9s\x1b[3;8r\x1b[?6h\x1b[2;2H");
        terminal.feed(sequence);
        assert_eq!(
            (terminal.screen().cursor.row, terminal.screen().cursor.col),
            expected,
            "{sequence:?}",
        );
    }
}
