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
        &*terminal.screen().cell_text(&terminal.screen().row(0), 0),
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
