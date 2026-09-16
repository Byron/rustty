use rustty_vt::{Terminal, snapshot};

#[test]
fn erasing_a_wrapped_row_disconnects_the_following_row() {
    for setup in ["abcdX", "abc界"] {
        for command in [b"\x1b[X".as_slice(), b"\x1b[K", b"\x1b[2K", b"\x1b[P"] {
            let mut terminal = Terminal::new(4, 3, 10);
            terminal.feed(setup.as_bytes());
            assert!(terminal.screen().row(0).wrapped);
            assert!(terminal.screen().row(1).wrap_continuation);
            terminal.feed(b"\x1b[H");
            terminal.feed(command);
            assert!(!terminal.screen().row(0).wrapped);
            assert!(!terminal.screen().row(1).wrap_continuation);
            assert!(
                terminal
                    .screen()
                    .row(0)
                    .cells
                    .iter()
                    .all(|cell| !cell.spacer_head())
            );
        }
    }
}

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
    assert_eq!(terminal.screen().row_text(&terminal.screen().row(1)), "");
    assert_eq!(terminal.screen().row_text(&terminal.screen().row(2)), "two");
    terminal.feed(b"\x1b[M");
    assert_eq!(terminal.screen().row_text(&terminal.screen().row(1)), "two");
    assert_eq!(
        terminal.screen().row_text(&terminal.screen().row(2)),
        "three"
    );
}
