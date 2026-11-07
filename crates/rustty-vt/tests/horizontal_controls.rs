use rustty_vt::{Terminal, snapshot};

fn terminal() -> Terminal {
    let mut t = Terminal::new(8, 4, 0);
    t.feed(b"abcdefg\r\nABCDEFG\r\nzyxwvut\r\nZYXWVUT\x1b[?69h\x1b[2;5s\x1b[2;3r");
    t
}

fn lines(t: &Terminal) -> Vec<String> {
    t.screen()
        .rows()
        .map(|row| t.screen().row_text(row))
        .collect()
}

#[test]
fn column_edits_preserve_cursor_and_cells_outside_margins() {
    for (command, expected) in [
        (
            b"\x1b['}".as_slice(),
            ["abcdefg", "AB CDFG", "zy xwut", "ZYXWVUT"],
        ),
        (b"\x1b[99'~", ["abcdefg", "AB   FG", "zy   ut", "ZYXWVUT"]),
    ] {
        let mut t = terminal();
        t.feed(b"\x1b[2;3H");
        t.feed(command);
        assert_eq!(lines(&t), expected);
        assert_eq!((t.screen().cursor.col, t.screen().cursor.row), (2, 1));
    }
}

#[test]
fn column_counts_default_to_one_and_invalid_commands_do_nothing() {
    for final_byte in ['}', '~'] {
        let mut expected = terminal();
        expected.feed(format!("\x1b[2;3H\x1b[1'{final_byte}").as_bytes());
        for count in ["", "0"] {
            let mut t = terminal();
            t.feed(format!("\x1b[2;3H\x1b[{count}'{final_byte}").as_bytes());
            assert_eq!(lines(&t), lines(&expected));
        }
        for command in [
            format!("\x1b[2{final_byte}"),
            format!("\x1b[2;3'{final_byte}"),
            format!("\x1b[2:3'{final_byte}"),
            format!("\x1b[2' {final_byte}"),
        ] {
            let mut t = terminal();
            let before = snapshot::encode_to_vec(&t).unwrap();
            t.feed(command.as_bytes());
            assert_eq!(snapshot::encode_to_vec(&t).unwrap(), before);
        }
    }
}

#[test]
fn column_edits_outside_either_margin_preserve_pending_wrap() {
    for position in [b"\x1b[1;3H".as_slice(), b"\x1b[2;1H", b"\x1b[2;8H"] {
        for command in [b"\x1b['}".as_slice(), b"\x1b['~"] {
            let mut t = terminal();
            t.feed(position);
            t.screen_mut().cursor.pending_wrap = true;
            let before = snapshot::encode_to_vec(&t).unwrap();
            t.feed(command);
            assert_eq!(snapshot::encode_to_vec(&t).unwrap(), before);
        }
    }
}

#[test]
fn horizontal_index_scrolls_at_margin_and_ignores_reverse_wrap() {
    for (command, col, expected) in [
        (
            b"\x1b6".as_slice(),
            2,
            ["abcdefg", "A BCDFG", "z yxwut", "ZYXWVUT"],
        ),
        (b"\x1b9", 5, ["abcdefg", "ACDE FG", "zxwv ut", "ZYXWVUT"]),
    ] {
        let mut t = terminal();
        t.feed(format!("\x1b[2;{col}H\x1b[?45h\x1b[?1045h").as_bytes());
        t.screen_mut().cursor.pending_wrap = true;
        t.feed(command);
        assert_eq!(lines(&t), expected);
        assert_eq!((t.screen().cursor.col, t.screen().cursor.row), (col - 1, 1));
        assert!(!t.screen().cursor.pending_wrap);
    }
    for (command, start, end) in [(b"\x1b6".as_slice(), 1, 0), (b"\x1b9", 6, 7)] {
        let mut t = terminal();
        t.feed(b"\x1b[3;5s");
        t.feed(format!("\x1b[1;{}H", start + 1).as_bytes());
        let before = lines(&t);
        t.feed(command);
        t.feed(command);
        assert_eq!(t.screen().cursor.col, end);
        assert_eq!(lines(&t), before);
    }
}
