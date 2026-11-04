use tmuxx_terminal::Terminal;

#[test]
fn native_snapshots_preserve_saved_modes_and_encoder_selection() {
    let mut t = Terminal::new(12, 4, 100);
    t.feed(b"\x1b[4h\x1b[?1000;1006;2004h\x1b[?s\x1b[4l\x1b[?1002;1016h\x1b[?2004l");
    for mut restored in [
        t.snapshot().restore().unwrap(),
        t.checkpoint_snapshot().restore().unwrap(),
    ] {
        restored.feed(b"\x1b[?r");
        let vt = restored.into_backend();
        assert!(vt.modes.get(false, 4));
        assert!(vt.modes.dec(2004));
        assert_eq!((vt.mouse_mode, vt.mouse_format), (1000, 1006));
    }
}
