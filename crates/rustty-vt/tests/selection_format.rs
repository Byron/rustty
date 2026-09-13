use rustty_vt::{
    Selection, Terminal,
    formatter::{Format, Options},
};

#[test]
fn selection_formats_preserve_styles_utf8_bytes_and_leave_the_terminal_unchanged() {
    let mut terminal = Terminal::new(8, 3, 100);
    terminal.feed("\x1b[1;31mA界\r\n\x1b[0mé<&\"".as_bytes());
    let screen = terminal.screen();
    let selection = Selection {
        start: screen.point(0, 0).unwrap(),
        end: screen.point(1, 3).unwrap(),
        rectangular: false,
    };
    let before = rustty_vt::snapshot::encode_to_vec(&terminal).unwrap();
    let plain = terminal
        .format_selection(selection, Options::default())
        .unwrap();
    assert_eq!(plain, "A界\né<&\"".as_bytes());
    let vt = terminal
        .format_selection(
            selection,
            Options {
                emit: Format::Vt,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        vt.windows(b"\x1b[0m\x1b[1m\x1b[38;5;1m".len())
            .any(|bytes| bytes == b"\x1b[0m\x1b[1m\x1b[38;5;1m")
    );
    let html = terminal
        .format_selection(
            selection,
            Options {
                emit: Format::Html,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        String::from_utf8(html)
            .unwrap()
            .contains("&#233;&lt;&amp;&quot;")
    );
    assert_eq!(
        rustty_vt::snapshot::encode_to_vec(&terminal).unwrap(),
        before
    );
    assert!(terminal.screen().selection.is_none());
}
