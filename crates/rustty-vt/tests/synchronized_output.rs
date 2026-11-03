use rustty_vt::{Effect, EffectHandler, Terminal};

#[derive(Default)]
struct Handler(Vec<(bool, String)>);
impl EffectHandler for Handler {
    fn effect(&mut self, _: Effect) {}
    fn synchronized_output(&mut self, terminal: &Terminal, enabled: bool) {
        assert_eq!(terminal.modes.dec(2026), enabled);
        self.0.push((
            enabled,
            terminal
                .screen()
                .row_text(terminal.screen().row(0))
                .trim_end()
                .into(),
        ));
    }
}

#[test]
fn hold_callbacks_observe_each_frame_before_later_batched_text() {
    let bytes = b"before\x1b[?2026hafter\x1b[?2026hmore\x1b[?2026l\x1b[?2026h\x1bc";
    for split in 0..=bytes.len() {
        let mut terminal = Terminal::new(30, 3, 0);
        let mut handler = Handler::default();
        terminal.feed_with_handler(&bytes[..split], &mut handler);
        terminal.feed_with_handler(&bytes[split..], &mut handler);
        assert_eq!(
            handler.0,
            [
                (true, "before".into()),
                (true, "beforeafter".into()),
                (false, "beforeaftermore".into()),
                (true, "beforeaftermore".into()),
                (false, "".into())
            ],
            "split {split}"
        );
    }
}

#[test]
fn resize_and_host_timeout_end_holds_without_changing_batched_output() {
    let mut terminal = Terminal::new(20, 3, 0);
    let mut handler = Handler::default();
    terminal.feed_with_handler(b"\x1b[?2026h", &mut handler);
    terminal.resize(20, 3);
    assert!(!terminal.modes.dec(2026));
    terminal.feed_with_handler(b"\x1b[?2026h", &mut handler);
    terminal.resize(30, 4);
    assert!(!terminal.modes.dec(2026));
    terminal.feed_with_handler(b"\x1b[?2026h", &mut handler);
    terminal.set_mode(true, 2026, false);
    assert!(!terminal.modes.dec(2026));
    let mut batched = Terminal::new(20, 3, 0);
    let mut ordinary = batched.clone();
    let bytes = "styled \x1b[31mred\x1b[0m 漢e\u{301}".as_bytes();
    batched.feed_with_handler(bytes, &mut handler);
    ordinary.feed(bytes);
    assert_eq!(
        batched.screen().row_text(batched.screen().row(0)),
        ordinary.screen().row_text(ordinary.screen().row(0))
    );
    assert_eq!(batched.generation, ordinary.generation);
}
