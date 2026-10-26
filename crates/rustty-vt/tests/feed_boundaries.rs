use rustty_vt::{Boundaries as B, Effect, EffectHandler, Terminal, snapshot};

#[test]
fn boundary_feeds_preserve_full_feed_effect_order_and_terminal_state() {
    let input = b"A\x1b_Ga=t,f=24,s=1,v=1,i=7;AAAA\x1b\\\x1b]2;title\x07\x1b[6ntail";
    let mut full = Terminal::new(10, 3, 0);
    let expected = full.feed(input);
    let mut terminal = Terminal::new(10, 3, 0);
    let mut effects = Vec::new();
    let mut offset = 0;
    while offset < input.len() {
        let (stop, next) = terminal.feed_until(
            &input[offset..],
            B::APC_START | B::APC_END | B::OSC_START | B::OSC_END,
        );
        assert!(stop.consumed > 0);
        offset += stop.consumed;
        effects.extend(next);
    }
    assert_eq!(effects, expected);
    assert_eq!(terminal.title_bytes(), full.title_bytes());
    assert_eq!(terminal.screen().cursor, full.screen().cursor);
    assert_eq!(
        terminal.graphics().images[&7].pixels,
        full.graphics().images[&7].pixels
    );
    for (row, expected) in terminal.screen().rows().zip(full.screen().rows()) {
        assert_eq!(
            terminal.screen().row_text(row),
            full.screen().row_text(expected)
        );
    }
}

#[test]
fn host_callbacks_finish_before_the_boundary_and_leave_the_suffix_unprocessed() {
    #[derive(Default)]
    struct Host(Vec<Effect>);
    impl EffectHandler for Host {
        fn effect(&mut self, effect: Effect) {
            self.0.push(effect);
        }
        fn enquiry(&mut self) -> Vec<u8> {
            b"reply".to_vec()
        }
    }
    let mut terminal = Terminal::new(10, 3, 0);
    let mut host = Host::default();
    let input = b"A\x05\x1b]2;stopped\x07tail";
    let stop = terminal.feed_until_with_handler(input, B::OSC_END, &mut host);
    assert_eq!(
        host.0,
        [
            Effect::Write(b"reply".to_vec()),
            Effect::Title(b"stopped".to_vec())
        ]
    );
    assert_eq!(terminal.screen().cursor.col, 1);
    let (ground, effects) = terminal.feed_until(&input[stop.consumed..], B::GROUND);
    assert_eq!(ground.consumed, 0);
    assert!(effects.is_empty());
    terminal.feed(&input[stop.consumed..]);
    assert_eq!(terminal.screen().cursor.col, 5);
}

#[test]
fn snapshot_at_a_start_boundary_only_retains_consumed_input() {
    let input = b"\x1b_Ga=t,f=24,s=1,v=1,i=7;AAAA\x1b\\";
    let mut source = Terminal::new(10, 3, 0);
    let (stop, effects) = source.feed_until(input, B::APC_START);
    assert_eq!(stop.consumed, 2);
    assert!(effects.is_empty());
    assert_eq!(source.parser().continuation().unwrap(), b"\x1b_");
    let bytes = snapshot::encode_to_vec(&source).unwrap();
    let mut restored = snapshot::decode(bytes.as_slice(), Default::default()).unwrap();
    assert_eq!(source.feed(&input[2..]), restored.feed(&input[2..]));
    assert!(restored.graphics().images.contains_key(&7));
}
