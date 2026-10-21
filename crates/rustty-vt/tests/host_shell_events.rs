use rustty_vt::{Effect, EffectHandler, Terminal, snapshot};

fn command_lines(effects: Vec<Effect>) -> Vec<Option<Vec<u8>>> {
    effects
        .into_iter()
        .filter_map(|effect| match effect {
            Effect::CommandStart { command_line } => Some(command_line),
            _ => None,
        })
        .collect()
}

#[test]
fn command_identity_uses_explicit_metadata_or_marked_input_across_split_reads() {
    for tmux in [false, true] {
        for (stream, expected) in [
            (
                "\x1b]133;A\x07$ \x1b]133;B\x07ssh host\r\n\x1b]133;C\x07",
                Some("ssh host"),
            ),
            (
                "\x1b]133;A\x07$ \x1b]133;B\x07tix\x1b]133;C\x07",
                Some("tix"),
            ),
            (
                "\x1b]133;A\x07$ \x1b]133;B\x07ssh old\r\x1b[2C\x1b[Kbuild\r\n\x1b]133;C\x07",
                Some("build"),
            ),
            (
                "\x1b]133;A\x07$ \x1b]133;B\x07alias\r\n\x1b]133;C;cmdline_url=ssh%20host\x07",
                Some("ssh host"),
            ),
            ("\x1b]133;C;cmdline_url=%ff\x07", None),
            ("\x1b]2;ssh\x07\x1b]133;C\x07", None),
        ] {
            for split in 0..=stream.len() {
                let mut terminal = Terminal::new(40, 5, 10);
                terminal.shell_command_events = true;
                terminal.set_tmux_strings(tmux);
                let mut effects = terminal.feed(&stream.as_bytes()[..split]);
                effects.extend(terminal.feed(&stream.as_bytes()[split..]));
                assert_eq!(
                    command_lines(effects),
                    [expected.map(|s| s.as_bytes().to_vec())],
                    "tmux={tmux}, split={split}, {stream:?}"
                );
            }
        }
    }
}

#[test]
fn command_input_survives_reflow_and_never_reuses_a_previous_command() {
    for tmux in [false, true] {
        let mut terminal = Terminal::new(20, 5, 10);
        terminal.shell_command_events = true;
        terminal.set_tmux_strings(tmux);
        terminal.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07ssh host");
        terminal.resize(5, 5);
        assert_eq!(
            command_lines(terminal.feed(b"\r\n\x1b]133;C\x07")),
            [Some(b"ssh host".to_vec())]
        );
        for input in [
            &b"\x1b]133;C\x07"[..],
            &b"\x1b]133;A\x07$ \x1b]133;B\x07\r\n\x1b]133;C\x07"[..],
            &b"\x1b]133;B\x07tix\x1bc\x1b]133;C\x07"[..],
        ] {
            assert_eq!(command_lines(terminal.feed(input)), [None], "tmux={tmux}");
        }
    }
}

#[test]
fn command_identity_is_unknown_when_marked_input_is_lost_or_too_large() {
    for history in [0, 1000] {
        let mut terminal = Terminal::new(40, 4, history);
        terminal.shell_command_events = true;
        terminal.feed(b"\x1b]133;B\x07ssh\r\n");
        terminal.feed(&b"x".repeat(20000));
        assert_eq!(command_lines(terminal.feed(b"\x1b]133;C\x07")), [None]);
    }
}

#[test]
fn command_lifecycle_effects_require_host_opt_in() {
    let mut terminal = Terminal::new(8, 3, 10);
    let input = b"\x1b]133;C\x07\x1b]133;D;7\x07";
    assert!(terminal.feed(input).is_empty());

    terminal.shell_command_events = true;
    assert_eq!(
        terminal.feed(input),
        [
            Effect::CommandStart { command_line: None },
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
            Effect::CommandStart { command_line: None },
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
    assert_eq!(
        terminal.feed(b"\x1b]133;C\x07"),
        [Effect::CommandStart { command_line: None }]
    );

    let restored = snapshot::decode(&disabled[..], snapshot::DecodeOptions::default()).unwrap();
    assert!(!restored.shell_command_events);
}
