use rustty_vt::{CursorShape, Terminal, snapshot};

#[test]
fn repeated_cursor_defaults_reapply_after_direct_blink_changes() {
    for shape in [
        CursorShape::Block,
        CursorShape::Bar,
        CursorShape::Underline,
        CursorShape::HollowBlock,
    ] {
        for blink in [None, Some(false), Some(true)] {
            for restore in [false, true] {
                let mut terminal = Terminal::new(8, 3, 10);
                terminal.set_default_cursor(shape, blink);
                terminal.feed(if blink.unwrap_or(true) {
                    b"\x1b[?12l"
                } else {
                    b"\x1b[?12h"
                });
                assert_ne!(terminal.screen().cursor.blink, blink.unwrap_or(true));
                if restore {
                    terminal = snapshot::decode_exact(
                        snapshot::encode_to_vec(&terminal).unwrap().as_slice(),
                        Default::default(),
                    )
                    .unwrap();
                }
                terminal.set_default_cursor(shape, blink);
                assert_eq!(terminal.default_cursor(), (shape, blink));
                assert_eq!(terminal.screen().cursor.shape, shape);
                assert_eq!(terminal.screen().cursor.blink, blink.unwrap_or(true));
                assert_eq!(terminal.modes.dec(12), blink.unwrap_or(true));
            }
        }
    }
}

#[test]
fn repeated_defaults_preserve_explicit_application_styles_until_default_selection() {
    for style in 1..=6 {
        let mut terminal = Terminal::new(8, 3, 10);
        terminal.set_default_cursor(CursorShape::HollowBlock, Some(false));
        terminal.feed(format!("\x1b[{style} q").as_bytes());
        let explicit = terminal.screen().cursor.clone();
        terminal.set_default_cursor(CursorShape::HollowBlock, Some(false));
        assert_eq!(terminal.screen().cursor, explicit);
        terminal.set_default_cursor(CursorShape::Underline, Some(true));
        assert_eq!(terminal.screen().cursor, explicit);
        terminal.feed(b"\x1b[0 q");
        assert_eq!(terminal.screen().cursor.shape, CursorShape::Underline);
        assert!(terminal.screen().cursor.blink);
    }
}
