use tmuxx_terminal::{Effect, Terminal};

#[test]
fn snapshots_restore_pointer_stacks_for_both_native_screen_encodings() {
    let mut terminal = Terminal::new(8, 3, 100);
    terminal.feed(b"history\r\n1\r\n2\r\n3\x1b]22;>pointer,help\x07");
    terminal.feed(b"\x1b[?1049h\x1b]22;>wait,crosshair\x07");
    let full = serde_json::from_value::<tmuxx_terminal::Snapshot>(
        serde_json::to_value(terminal.snapshot()).unwrap(),
    )
    .unwrap();
    let compact = serde_json::from_value::<tmuxx_terminal::CheckpointSnapshot>(
        serde_json::to_value(terminal.checkpoint_snapshot()).unwrap(),
    )
    .unwrap();
    for mut restored in [full.restore().unwrap(), compact.restore().unwrap()] {
        assert_eq!(
            restored.feed(b"\x1b]22;<\x07\x1b]22;?__current__\x07"),
            [Effect::Write(b"\x1b]22;wait\x07".to_vec())]
        );
        restored.feed(b"\x1b[?1049l");
        assert_eq!(
            restored.feed(b"\x1b]22;<\x07\x1b]22;?__current__\x07"),
            [Effect::Write(b"\x1b]22;pointer\x07".to_vec())]
        );
        restored.feed(b"\x1bc");
        assert_eq!(
            restored.feed(b"\x1b]22;?__current__\x07"),
            [Effect::Write(b"\x1b]22;0\x07".to_vec())]
        );
        restored.feed(b"\x1b[?1049h");
        assert_eq!(
            restored.feed(b"\x1b]22;?__current__\x07"),
            [Effect::Write(b"\x1b]22;0\x07".to_vec())]
        );
    }
}
