use rustty_vt::{Effect, EffectHandler, Terminal};

#[test]
fn cancelled_osc_has_no_effect_and_the_next_command_still_works() {
    for command in ["2;cancelled", "52;c;Zg==", "4;1;rgb:ff/00/00"] {
        for cancel in [0x18, 0x1a] {
            let mut input = format!("\x1b]{command}").into_bytes();
            input.push(cancel);
            for split in 0..=input.len() {
                let mut terminal = Terminal::new(10, 2, 0);
                terminal.feed(b"\x1b]2;before\x07");
                let palette = terminal.palette.clone();
                assert!(terminal.feed(&input[..split]).is_empty());
                assert!(terminal.feed(&input[split..]).is_empty());
                assert_eq!(terminal.title_bytes(), b"before");
                assert_eq!(terminal.palette, palette);
                assert_eq!(
                    terminal.feed(b"\x1b]2;after\x1b\\"),
                    [Effect::Title(b"after".to_vec())]
                );
            }
        }
    }
}

#[test]
fn direct_osc_cancellation_preserves_pending_stream_input() {
    struct NoEffects;
    impl EffectHandler for NoEffects {
        fn effect(&mut self, effect: Effect) {
            panic!("cancelled OSC emitted {effect:?}");
        }
    }
    for cancel in [0x18, 0x1a] {
        let mut terminal = Terminal::new(10, 2, 0);
        terminal.feed(b"\x1b]2;pending");
        terminal.feed_osc_with_handler(b"2;cancelled", Some(cancel), &mut NoEffects);
        assert_eq!(terminal.title_bytes(), b"");
        assert_eq!(terminal.feed(b"\x07"), [Effect::Title(b"pending".to_vec())]);
    }
}
