use rustty_vt::{Effect, EffectHandler, Terminal, snapshot};

#[test]
fn all_modes_restore_ansi_flags_without_cursor_screen_margin_or_history_transitions() {
    let mut t = Terminal::new(12, 4, 100);
    t.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\x1b[4;20h\x1b[?1;5;12;2004;2027h\x1b[?s");
    t.feed(b"\x1b[4;20l\x1b[?1;5;12;2004;2027l\x1b[?1049h\x1b[2;4r\x1b[?6;69h\x1b[3;9s\x1b[2;2Halt\x1b[?2026h");
    let cursor = t.screen().cursor.clone();
    let margins = t.margins;
    let primary_text = t
        .primary_screen()
        .all_rows()
        .map(|row| row.id)
        .collect::<Vec<_>>();
    let text = t.plain_text();
    let held_generation = t.synchronized_output_generation;
    t.feed(b"\x1b[?r");
    assert!(t.is_alternate_screen());
    assert_eq!(t.margins, margins);
    assert_eq!(
        (
            t.screen().cursor.row,
            t.screen().cursor.col,
            t.screen().cursor.pending_wrap
        ),
        (cursor.row, cursor.col, cursor.pending_wrap)
    );
    assert_eq!(t.plain_text(), text);
    assert_eq!(
        t.primary_screen()
            .all_rows()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        primary_text
    );
    assert_eq!(t.synchronized_output_generation, held_generation);
    for mode in [1, 5, 6, 12, 69, 1049, 2004, 2026, 2027] {
        assert!(t.modes.dec(mode), "{mode}");
    }
    assert!(t.modes.get(false, 4));
    assert!(t.modes.get(false, 20));
    assert!(t.screen().cursor.blink);
}

#[test]
fn mouse_selection_and_saved_flags_survive_snapshot_and_chunking() {
    for tmux in [false, true] {
        for split in 0..=4 {
            let mut t = Terminal::new(12, 4, 0);
            t.tmux_graphics = tmux;
            t.feed(b"\x1b[?1003;1000;1016;1006h\x1b[?s");
            let keys = [9, 1000, 1002, 1003, 1005, 1006, 1015, 1016];
            let expected = keys.map(|mode| t.modes.dec(mode));
            assert_eq!((t.mouse_mode, t.mouse_format), (1000, 1006));
            t.feed(b"\x1b[?1002;1015h");
            let bytes = snapshot::encode_to_vec(&t).unwrap();
            let mut t = snapshot::decode(bytes.as_slice(), Default::default()).unwrap();
            t.tmux_graphics = tmux;
            t.feed(&b"\x1b[?r"[..split]);
            t.feed(&b"\x1b[?r"[split..]);
            assert_eq!(keys.map(|mode| t.modes.dec(mode)), expected);
            assert_eq!((t.mouse_mode, t.mouse_format), (1000, 1006));
            t.feed(b"\x1bc\x1b[?r");
            assert_eq!((t.mouse_mode, t.mouse_format), (0, 0));
        }
    }
}

#[test]
fn restore_emits_ordered_host_updates_and_observes_actual_mouse_change() {
    #[derive(Default)]
    struct Host {
        effects: Vec<Effect>,
        tracking: Vec<(u16, u16)>,
    }
    impl EffectHandler for Host {
        fn effect(&mut self, effect: Effect) {
            self.effects.push(effect);
        }
        fn mouse_tracking_changed(&mut self, mode: u16, format: u16) {
            self.tracking.push((mode, format));
        }
    }
    let mut t = Terminal::new(12, 4, 0);
    t.linefeed_mode_events = true;
    t.query_defaults.focused = Some(true);
    t.feed(b"\x1b[20h\x1b[?1000;1006;1004h\x1b[?s\x1b[20l\x1b[?1002;1016h\x1b[?1004l");
    let mut host = Host::default();
    t.feed_with_handler(b"\x1b[?r\x1b[20l", &mut host);
    assert_eq!(host.tracking, [(1000, 1006)]);
    assert_eq!(
        host.effects,
        [
            Effect::LinefeedMode(true),
            Effect::Write(b"\x1b[I".to_vec()),
            Effect::LinefeedMode(false)
        ]
    );
}

#[test]
fn explicit_zero_does_not_mean_all_and_explicit_lists_still_work() {
    let mut t = Terminal::new(12, 4, 0);
    t.feed(b"\x1b[?1h\x1b[?0s\x1b[?1l\x1b[?r");
    assert!(!t.modes.dec(1));
    t.feed(b"\x1b[?1h\x1b[?1s\x1b[?1l\x1b[?0r");
    assert!(!t.modes.dec(1));
    t.feed(b"\x1b[?1r");
    assert!(t.modes.dec(1));
}
