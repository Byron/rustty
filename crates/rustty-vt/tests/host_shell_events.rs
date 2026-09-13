use rustty_vt::{Effect, EffectHandler, Terminal, snapshot};

#[test]
fn command_lifecycle_effects_require_host_opt_in() {
    let mut terminal = Terminal::new(8, 3, 10);
    let input = b"\x1b]133;C\x07\x1b]133;D;7\x07";
    assert!(terminal.feed(input).is_empty());

    terminal.shell_command_events = true;
    assert_eq!(
        terminal.feed(input),
        [
            Effect::CommandStart,
            Effect::CommandEnd { exit_code: Some(7) }
        ]
    );

    terminal.shell_command_events = false;
    assert!(terminal.feed(input).is_empty());
}

#[test]
fn synchronous_hosts_receive_opted_in_commands_in_stream_order() {
    #[derive(Default)]
    struct Host(Vec<Effect>);
    impl EffectHandler for Host {
        fn effect(&mut self, effect: Effect) {
            self.0.push(effect);
        }
    }

    let mut terminal = Terminal::new(8, 3, 10);
    let mut host = Host::default();
    terminal.feed_with_handler(b"\x1b]133;C\x07\x1b]133;D\x07", &mut host);
    assert!(host.0.is_empty());

    terminal.shell_command_events = true;
    for bytes in [
        &b"\x1b]133;"[..],
        &b"C\x07\x07\x1b]133;D;-"[..],
        &b"2\x07\x1b]133;D\x07"[..],
    ] {
        terminal.feed_with_handler(bytes, &mut host);
    }
    assert_eq!(
        host.0,
        [
            Effect::CommandStart,
            Effect::Bell,
            Effect::CommandEnd {
                exit_code: Some(-2)
            },
            Effect::CommandEnd { exit_code: None },
        ]
    );
}

#[test]
fn host_opt_in_survives_reset_without_entering_snapshot_state() {
    let mut terminal = Terminal::new(8, 3, 10);
    let disabled = snapshot::encode_to_vec(&terminal).unwrap();
    terminal.shell_command_events = true;
    assert_eq!(snapshot::encode_to_vec(&terminal).unwrap(), disabled);

    terminal.reset();
    assert!(terminal.shell_command_events);
    terminal.feed(b"\x1bc");
    assert!(terminal.shell_command_events);
    assert_eq!(terminal.feed(b"\x1b]133;C\x07"), [Effect::CommandStart]);

    let restored = snapshot::decode(&disabled[..], snapshot::DecodeOptions::default()).unwrap();
    assert!(!restored.shell_command_events);
}
