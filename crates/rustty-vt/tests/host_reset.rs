use rustty_vt::{Key, KeyEvent, Terminal};

#[test]
fn recovery_discards_partial_protocols_and_restores_keyboard_input() {
    for unfinished in [
        b"\x1b]52;c;Zg==".as_slice(),
        b"\x1bP$q",
        b"\x1b_Ga=t,f=32,s=1,v=1;AAAA",
        b"\x1b[31",
        b"\xf0\x9f",
    ] {
        let mut terminal = Terminal::new(8, 3, 100);
        terminal.feed(b"old output\x1b[?1049h\x1b[2h\x1b[?2026h\x1b[?1003h");
        let key = KeyEvent::new(Key::Enter);
        assert!(terminal.encode_key(&key).is_empty());
        terminal.feed(unfinished);
        terminal.reset_from_host();
        assert!(terminal.parser().is_ground());
        assert!(!terminal.is_alternate_screen());
        assert!(!terminal.modes.dec(2026));
        assert!(!terminal.modes.dec(1003));
        assert_eq!(terminal.encode_key(&key), b"\r");
        assert!(terminal.feed(b"recovered").is_empty());
        assert_eq!(
            terminal.screen().row_text(terminal.screen().row(0)),
            "recovere"
        );
    }
}
