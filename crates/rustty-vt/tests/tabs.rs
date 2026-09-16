use rustty_vt::Terminal;

#[test]
fn tab_commands_keep_explicit_zero_counts() {
    let mut terminal = Terminal::new(25, 3, 0);
    terminal.feed(b"\x1b[1;4H\x1b[0I\x1b[0Z\x1b[;I\x1b[;Z");
    assert_eq!(terminal.screen().cursor.col, 3);
    terminal.feed(b"\x1b[I");
    assert_eq!(terminal.screen().cursor.col, 8);
    terminal.feed(b"\x1b[Z");
    assert_eq!(terminal.screen().cursor.col, 0);
}

#[test]
fn tabs_use_direction_and_origin_specific_margin_bounds() {
    let mut terminal = Terminal::new(25, 3, 0);
    terminal.feed(b"\x1b[?69h\x1b[6;20s\x1b[1;17H\t");
    assert_eq!(terminal.screen().cursor.col, 19);
    terminal.feed(b"\x1b[1;25H\x1b[I");
    assert_eq!(terminal.screen().cursor.col, 24);
    terminal.feed(b"\x1b[1;8H\x1b[Z");
    assert_eq!(terminal.screen().cursor.col, 0);
    terminal.feed(b"\x1b[?6h\x1b[1;2H\x1b[Z");
    assert_eq!(terminal.screen().cursor.col, 5);
    terminal.feed(b"\x1b[100D\x1b[Z");
    assert_eq!(terminal.screen().cursor.col, 0);
}

#[test]
fn tab_motion_preserves_pending_wrap() {
    let mut terminal = Terminal::new(25, 3, 0);
    terminal.feed(b"\x1b[25GX\x1b[0I");
    assert!(terminal.screen().cursor.pending_wrap);
    terminal.feed(b"\x1b[Z");
    assert_eq!(terminal.screen().cursor.col, 16);
    assert!(terminal.screen().cursor.pending_wrap);
    terminal.feed(b"\t");
    assert_eq!(terminal.screen().cursor.col, 24);
    assert!(terminal.screen().cursor.pending_wrap);
    terminal.feed(b"\x1b[I");
    assert!(terminal.screen().cursor.pending_wrap);
    terminal.feed(b"Y");
    assert_eq!(
        &*terminal.screen().cell_text(&terminal.screen().row(1), 0),
        "Y"
    );
}
