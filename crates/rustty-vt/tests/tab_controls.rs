use rustty_vt::Terminal;

#[test]
fn cursor_tabulation_controls_set_and_clear_stops() {
    for set in [b"\x1b[W".as_slice(), b"\x1b[0W"] {
        let mut terminal = Terminal::new(20, 2, 0);
        terminal.feed(b"\x1b[3g\x1b[4G");
        terminal.feed(set);
        terminal.feed(b"\r\t");
        assert_eq!(terminal.screen().cursor.col, 3);
        terminal.feed(b"\x1b[2W\r\t");
        assert_eq!(terminal.screen().cursor.col, 19);
    }
    let mut terminal = Terminal::new(20, 2, 0);
    terminal.feed(b"\x1b[5W\t");
    assert_eq!(terminal.screen().cursor.col, 19);
}

#[test]
fn private_tab_reset_restores_defaults_after_snapshot() {
    let mut terminal = Terminal::new(20, 2, 0);
    terminal.feed(b"\x1b[3g\x1b[4G\x1bH\x1b[?5W");
    let snapshot = rustty_vt::snapshot::encode_to_vec(&terminal).unwrap();
    let mut terminal =
        rustty_vt::snapshot::decode(snapshot.as_slice(), Default::default()).unwrap();
    terminal.feed(b"\r\t");
    assert_eq!(terminal.screen().cursor.col, 8);
    terminal.feed(b"\t");
    assert_eq!(terminal.screen().cursor.col, 16);
}

#[test]
fn invalid_tabulation_controls_preserve_custom_stops() {
    for command in [
        "1W", "3W", "4W", "6W", "65535W", ";W", "0;2W", "2;5W", "?W", "?0W", "?2W", "?;W", "?5;0W",
        ">5W", "5:0W",
    ] {
        let mut terminal = Terminal::new(20, 2, 0);
        terminal.feed(b"\x1b[3g\x1b[4G\x1bH");
        terminal.feed(format!("\x1b[{command}\r\t").as_bytes());
        assert_eq!(terminal.screen().cursor.col, 3, "{command}");
        terminal.feed(b"\t");
        assert_eq!(terminal.screen().cursor.col, 19, "{command}");
    }
}
