use rustty_vt::{Terminal, snapshot};

#[test]
fn inserting_or_deleting_zero_lines_keeps_all_terminal_state() {
    for setup in [
        b"".as_slice(),
        b"\x1b[?1049h",
        b"\x1b[?69h\x1b[2;6s\x1b[2;4r",
    ] {
        let mut terminal = Terminal::new(8, 5, 10);
        terminal.feed(setup);
        terminal.feed(b"one\r\ntwo\x1b[3;6HX");
        for command in [b"\x1b[0L".as_slice(), b"\x1b[;L", b"\x1b[0M", b"\x1b[;M"] {
            let before = snapshot::encode_to_vec(&terminal).unwrap();
            terminal.feed(command);
            assert!(
                snapshot::encode_to_vec(&terminal).unwrap() == before,
                "{command:?}"
            );
        }
    }
}

#[test]
fn omitted_line_edit_counts_still_move_one_line() {
    let mut terminal = Terminal::new(8, 4, 0);
    terminal.feed(b"one\r\ntwo\r\nthree\x1b[2;4H\x1b[L");
    assert_eq!(terminal.screen().cursor.col, 0);
    assert_eq!(terminal.screen().rows[1].text(), "");
    assert_eq!(terminal.screen().rows[2].text(), "two");
    terminal.feed(b"\x1b[M");
    assert_eq!(terminal.screen().rows[1].text(), "two");
    assert_eq!(terminal.screen().rows[2].text(), "three");
}
