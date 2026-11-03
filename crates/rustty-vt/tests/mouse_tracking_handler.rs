use rustty_vt::{Effect, EffectHandler, Terminal};

#[derive(Default)]
struct Handler(Vec<(u16, u16)>);
impl EffectHandler for Handler {
    fn effect(&mut self, _: Effect) {}
    fn mouse_tracking_changed(&mut self, mode: u16, format: u16) {
        self.0.push((mode, format));
    }
}

#[test]
fn tracking_restarts_and_reset_are_observed_inside_a_batch() {
    let input = b"\x1b[?1003h\x1b[?1006h\x1b[?1003l\x1b[?1003h\x1bc";
    for split in 0..=input.len() {
        let mut t = Terminal::new(10, 3, 0);
        let mut handler = Handler::default();
        t.feed_with_handler(&input[..split], &mut handler);
        t.feed_with_handler(&input[split..], &mut handler);
        assert_eq!(
            handler.0,
            [(1003, 0), (1003, 1006), (0, 1006), (1003, 1006), (0, 0)]
        );
    }
}
