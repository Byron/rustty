use rustty_vt::{Color, Style, Terminal, Underline};

#[test]
fn color_values_follow_native_u8_truncation() {
    let mut terminal = Terminal::new(10, 2, 0);
    terminal.feed(b"\x1b[38;5;1196;48;2;256;511;65535;58:2::257:258:259m");
    let style = terminal.screen().cursor.style;
    assert_eq!(style.foreground, Color::Indexed(172));
    assert_eq!(style.background, Color::Rgb(0, 255, 255));
    assert_eq!(style.underline_color, Color::Rgb(1, 2, 3));
}

#[test]
fn malformed_colors_leave_unconsumed_parameters_as_attributes() {
    for command in ["48;2;0", "48;2;0;25", "48:2;;;0"] {
        let mut terminal = Terminal::new(10, 2, 0);
        terminal.feed(b"\x1b[1;31m");
        terminal.feed(format!("\x1b[{command}m").as_bytes());
        assert_eq!(
            terminal.screen().cursor.style,
            Style::default(),
            "{command}"
        );
    }
    let mut terminal = Terminal::new(10, 2, 0);
    terminal.feed(b"\x1b[31m\x1b[38;5m");
    assert_eq!(terminal.screen().cursor.style.foreground, Color::Indexed(1));
    assert!(terminal.screen().cursor.style.blink);
}

#[test]
fn invalid_colon_groups_preserve_state_and_later_semicolon_attributes() {
    let mut terminal = Terminal::new(10, 2, 0);
    terminal.feed(b"\x1b[1;4:3;31m\x1b[0:1:2;32m");
    assert!(terminal.screen().cursor.style.bold);
    assert_eq!(terminal.screen().cursor.style.underline, Underline::Curly);
    assert_eq!(terminal.screen().cursor.style.foreground, Color::Indexed(2));
    terminal.feed(b"\x1b[4:0:1;34m\x1b[4:m\x1b[58:4:m");
    assert_eq!(terminal.screen().cursor.style.underline, Underline::Curly);
    assert_eq!(terminal.screen().cursor.style.foreground, Color::Indexed(4));
    terminal.feed(b"\x1b[4:65535m");
    assert_eq!(terminal.screen().cursor.style.underline, Underline::Single);
}
