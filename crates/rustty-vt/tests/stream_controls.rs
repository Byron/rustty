use rustty_vt::Terminal;

#[test]
fn xtshiftescape_validates_requests_and_preserves_their_lifecycle() {
    let mut terminal = Terminal::new(10, 6, 0);
    assert_eq!(terminal.mouse_shift_capture(), None);
    for (command, capture) in [
        (b"\x1b[>s".as_slice(), false),
        (b"\x1b[>1s", true),
        (b"\x1b[>0s", false),
    ] {
        terminal.feed(command);
        assert_eq!(terminal.mouse_shift_capture(), Some(capture));
        for invalid in [
            b"\x1b[>2s".as_slice(),
            b"\x1b[>1;0s",
            b"\x1b[>1:0s",
            b"\x1b[>1$s",
        ] {
            terminal.feed(invalid);
            assert_eq!(terminal.mouse_shift_capture(), Some(capture), "{invalid:?}");
        }
        terminal.feed(b"\x1b[?1049h\x1b[?1049l");
        assert_eq!(terminal.mouse_shift_capture(), Some(capture));
        let snapshot = rustty_vt::snapshot::encode_to_vec(&terminal).unwrap();
        terminal = rustty_vt::snapshot::decode(snapshot.as_slice(), Default::default()).unwrap();
        assert_eq!(terminal.mouse_shift_capture(), Some(capture));
    }
    terminal.feed(b"\x1bc");
    assert_eq!(terminal.mouse_shift_capture(), None);
}

#[test]
fn raw_c1_controls_execute_cursor_and_protection_actions_inside_sequences() {
    let mut terminal = Terminal::new(10, 6, 0);
    terminal.feed(b"\x1b[3;4H\x1b[3\x8d");
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (1, 3)
    );
    terminal.feed(b"\x1b[3\x84");
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (2, 3)
    );
    terminal.feed(b"\x1b[3\x85");
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (3, 0)
    );
    terminal.feed(b"\x1b[3\x96X\x1b[3\x97Y");
    let screen = terminal.screen();
    assert!(screen.rows[3].cells[0].protected);
    assert!(!screen.rows[3].cells[1].protected);
    assert!(!screen.cursor.protected);
    terminal.feed(b"\x1b[2K");
    assert_eq!(terminal.screen().rows[3].text(), "X");
}

#[test]
fn ground_state_c1_bytes_keep_utf8_decoding_semantics() {
    let mut terminal = Terminal::new(10, 6, 0);
    terminal.feed(b"\x1b[3;4H\xc2\x8d\xc2\x96X\x8d\x96");
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (2, 6)
    );
    assert!(!terminal.screen().cursor.protected);
    assert_eq!(terminal.screen().rows[2].text(), "   X\u{fffd}\u{fffd}");
}

#[test]
fn invalid_csi_parameter_counts_leave_terminal_state_unchanged() {
    let mut terminal = Terminal::new(20, 6, 10);
    terminal.feed(b"first\r\nsecond\x1b[?69h\x1b[2;18s\x1b[2;5r\x1b[3;5H\x1b[1\"q\x1b[>4;2m");
    let before = rustty_vt::snapshot::encode_to_vec(&terminal).unwrap();
    let mut commands: Vec<String> = "@ABCDEFGIJKLMPSTWXZ`abdeg"
        .chars()
        .map(|byte| format!("1;3{byte}"))
        .collect();
    commands.extend(
        [
            "1;2;3H", "1;2;3f", "1;2;3r", "1;2;3s", "?1;3J", "?1;3K", "0;1\"q", ">0;0;0m", "g",
        ]
        .map(String::from),
    );
    for command in commands {
        assert!(
            terminal
                .feed(format!("\x1b[{command}").as_bytes())
                .is_empty()
        );
        assert!(
            rustty_vt::snapshot::encode_to_vec(&terminal).unwrap() == before,
            "CSI {command} changed state despite an invalid parameter count"
        );
    }
}

#[test]
fn csi_parameter_validation_preserves_optional_and_variable_length_commands() {
    let mut terminal = Terminal::new(20, 6, 0);
    terminal.feed(b"\x1b[3;4H\x1b7\x1b[E");
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (3, 0)
    );
    terminal.feed(b"\x1b[1;2;3u\x1b[1;31m\x1b[?1;25l");
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (2, 3)
    );
    assert!(terminal.screen().cursor.style.bold);
    assert_eq!(
        terminal.screen().cursor.style.foreground,
        rustty_vt::Color::Indexed(1)
    );
    assert!(!terminal.modes.dec(1));
    assert!(!terminal.modes.dec(25));
}

#[test]
fn relative_position_commands_distinguish_zero_from_omitted_counts() {
    let mut terminal = Terminal::new(10, 6, 0);
    terminal.feed(b"\x1b[3;4H\x1b[0a\x1b[0e\x1b[;a\x1b[;e");
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (2, 3)
    );
    terminal.feed(b"\x1b[a\x1b[e");
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (3, 4)
    );
    terminal.feed(b"\x1b[10GX");
    assert!(terminal.screen().cursor.pending_wrap);
    terminal.feed(b"\x1b[0a");
    assert!(!terminal.screen().cursor.pending_wrap);
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (3, 9)
    );
}

#[test]
fn relative_position_commands_use_absolute_position_margin_rules() {
    let mut terminal = Terminal::new(10, 8, 0);
    terminal.feed(b"\x1b[?69h\x1b[3;7s\x1b[3;6r\x1b[4;5H\x1b[99a\x1b[99e");
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (7, 9)
    );
    terminal.feed(b"\x1b[?6h\x1b[2;2H\x1b[0a");
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (5, 5)
    );
    terminal.feed(b"\x1b[0e");
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (5, 6)
    );
}

#[test]
fn explicit_zero_scrolling_preserves_the_direct_scroll_path() {
    for alternate in [false, true] {
        let mut terminal = Terminal::new(6, 4, 10);
        if alternate {
            terminal.feed(b"\x1b[?1049h");
        }
        terminal.feed(b"a\r\nb\r\nc\r\nd\x1b[2;6HX");
        assert!(terminal.screen().cursor.pending_wrap);
        let before = rustty_vt::snapshot::encode_to_vec(&terminal).unwrap();
        terminal.feed(b"\x1b[0S\x1b[;S\x1b[0T\x1b[;T");
        assert!(rustty_vt::snapshot::encode_to_vec(&terminal).unwrap() == before);
        terminal.feed(b"\x1b[S");
        assert_eq!(terminal.screen().rows[0].text(), "b    X");
        assert!(terminal.screen().cursor.pending_wrap);
        terminal.feed(b"\x1b[T");
        assert_eq!(terminal.screen().rows[0].text(), "");
        assert_eq!(terminal.screen().rows[1].text(), "b    X");
        assert!(terminal.screen().cursor.pending_wrap);
    }
}

#[test]
fn explicit_zero_scrolling_preserves_partial_regions() {
    for (alternate, margins) in [
        (false, b"\x1b[2;3r".as_slice()),
        (true, b"\x1b[1;3r".as_slice()),
    ] {
        let mut terminal = Terminal::new(6, 4, 10);
        if alternate {
            terminal.feed(b"\x1b[?1049h");
        }
        terminal.feed(b"a\r\nb\r\nc\r\nd");
        terminal.feed(margins);
        terminal.feed(b"\x1b[0S\x1b[0T");
        let rows: Vec<_> = terminal
            .screen()
            .rows
            .iter()
            .map(|row| row.text())
            .collect();
        assert_eq!(rows, ["a", "b", "c", "d"]);
    }
}

#[test]
fn scroll_clear_omits_empty_rows_and_follows_the_cursor_row() {
    let mut terminal = Terminal::new(8, 5, 20);
    terminal.feed(b"\x1b[4;4H\x1b[22J");
    assert!(terminal.screen().history.is_empty());
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (3, 3)
    );
    terminal.feed(b"\x1b[Habc\x1b[4;4H\x1b[22J");
    assert_eq!(terminal.screen().history.len(), 1);
    assert_eq!(terminal.screen().history[0].text(), "abc");
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (2, 3)
    );
    terminal.feed(b"xy\x1b[22J");
    assert_eq!(terminal.screen().history.len(), 4);
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (0, 0)
    );
}

#[test]
fn scroll_clear_counts_background_cells_and_preserves_cursor_attributes() {
    let mut terminal = Terminal::new(8, 5, 20);
    terminal.feed(b"\x1b[?69h\x1b[2;6s\x1b[2;4r\x1b[3;1H\x1b[44m\x1b[2K\x1b[5;4H\x1b[1\"q\x1b]8;id=cursor;https://example.org\x07\x1b[22J");
    assert_eq!(terminal.screen().history.len(), 3);
    let cursor = &terminal.screen().cursor;
    assert_eq!((cursor.row, cursor.col), (1, 3));
    assert_eq!(cursor.style.background, rustty_vt::Color::Indexed(4));
    assert!(cursor.protected);
    assert_eq!(cursor.hyperlink.as_deref(), Some("https://example.org"));
    assert!(
        terminal
            .screen()
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .all(|cell| {
                cell.text.is_empty() && cell.style.background == rustty_vt::Color::Default
            })
    );
    assert_eq!(terminal.margins.top, 1);
    assert_eq!(terminal.margins.left, 1);
}
