use rustty_vt::{Color, CursorShape, Effect, EffectHandler, Terminal, modes::Modes};

#[test]
fn raw_defaults_and_saved_modes_have_independent_reset_lifetimes() {
    let mut modes = Modes::default();
    assert_eq!(modes.get_saved(true, 2027), Some(false));
    assert!(modes.set_default(true, 2027, true));
    modes.save(true, 2027);
    modes.set(true, 2027, false);
    assert!(modes.restore(true, 2027));
    modes.reset();
    assert!(modes.dec(2027));
    assert_eq!(modes.get_default(true, 2027), Some(true));
    assert_eq!(modes.get_saved(true, 2027), Some(false));
    assert!(!modes.restore(true, 2027));
    modes.reset();
    assert!(modes.dec(2027));

    modes.save(true, 999);
    assert_eq!(modes.get_saved(true, 999), None);
    assert_eq!(modes.get_default(true, 999), None);
    assert!(!modes.set_default(true, 999, true));
    assert!(!Modes::default_configurable(true, 999));
}

#[test]
fn host_defaults_reject_transitions_and_survive_ris_and_snapshots() {
    let mut terminal = Terminal::new(80, 24, 0);
    assert!(terminal.set_default_mode(true, 25, false));
    assert!(!terminal.screen().cursor.visible);
    assert!(terminal.set_default_mode(true, 2027, true));
    for mode in [3, 6, 12, 47, 69, 1000, 1006, 1048, 1049, 2026, 2033] {
        assert!(!terminal.set_default_mode(true, mode, true));
        assert_eq!(terminal.modes.get_default(true, mode), Some(false));
    }
    terminal.feed(b"\x1b[?25h\x1b[?2027l\x1bc");
    assert!(!terminal.screen().cursor.visible);
    assert!(terminal.modes.dec(2027));
    let bytes = rustty_vt::snapshot::encode_to_vec(&terminal).unwrap();
    let mut terminal = rustty_vt::snapshot::decode(bytes.as_slice(), Default::default()).unwrap();
    terminal.feed(b"\x1b[?25h\x1b[?2027l\x1bc");
    assert!(!terminal.screen().cursor.visible);
    assert!(terminal.modes.dec(2027));
}

#[test]
fn column_mode_requires_permission_and_restore_rechecks_it() {
    let mut terminal = Terminal::new(80, 24, 0);
    terminal.feed(b"text\x1b[?3h");
    assert_eq!(terminal.cols, 80);
    assert!(!terminal.modes.dec(3));
    assert!(terminal.plain_text().starts_with("text"));

    terminal.feed(b"\x1b[?40h\x1b[?3h\x1b[?3s");
    assert_eq!(terminal.cols, 132);
    assert!(terminal.modes.dec(3));
    assert!(terminal.plain_text().is_empty());
    terminal.feed(b"\x1b[?40l\x1b[?3r");
    assert_eq!(terminal.cols, 132);
    assert!(!terminal.modes.dec(3));
}

#[test]
fn alternate_screen_restore_retains_the_primary_cursor_shape() {
    for (mode, shape) in [
        (47, CursorShape::Block),
        (1047, CursorShape::Block),
        (1049, CursorShape::Bar),
    ] {
        let mut terminal = Terminal::new(80, 24, 0);
        terminal.feed(format!("\x1b[5 q\x1b[?{mode}h\x1b[2 q\x1b[?{mode}l").as_bytes());
        assert_eq!(terminal.screen().cursor.shape, shape);
        assert!(!terminal.screen().cursor.blink);
    }
}

#[test]
fn stream_mode_queries_follow_native_parameter_and_host_rules() {
    #[derive(Default)]
    struct Readonly(Vec<Vec<u8>>);
    impl EffectHandler for Readonly {
        fn effect(&mut self, effect: Effect) {
            if let Effect::Write(bytes) = effect {
                self.0.push(bytes);
            }
        }
        fn clipboard_read_enabled(&self) -> bool {
            false
        }
    }
    let mut terminal = Terminal::new(80, 24, 0);
    let mut handler = Readonly::default();
    terminal.feed(b"\x1b[5;2 q");
    assert_eq!(terminal.screen().cursor.shape, CursorShape::Block);
    terminal.feed(b"\x1b[?7:25l");
    assert!(terminal.modes.dec(7));
    assert!(terminal.screen().cursor.visible);
    terminal.feed_with_handler(
        b"\x1b[31m\x1b[?7l\x1b[!p\x1b[4$p\x1b[?1;7$p\x1b[?$p\x1b[?7$p\x1b[?5522$p\x1b[?32885$p\x1b[?32775$p\x1b[?65536$p",
        &mut handler,
    );
    assert_eq!(terminal.screen().cursor.style.foreground, Color::Indexed(1));
    assert!(!terminal.modes.dec(7));
    assert_eq!(
        handler.0,
        [
            b"\x1b[?7;2$y".to_vec(),
            b"\x1b[?5522;0$y".to_vec(),
            b"\x1b[?117;4$y".to_vec(),
            b"\x1b[?7;2$y".to_vec(),
            b"\x1b[?32767;0$y".to_vec(),
        ]
    );
}
