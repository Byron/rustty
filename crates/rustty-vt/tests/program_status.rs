use rustty_vt::{
    Effect, Terminal,
    program_status::{Report, State},
};

fn terminal() -> Terminal {
    let mut t = Terminal::new(10, 3, 0);
    t.program_status_events = true;
    t
}

#[test]
fn status_is_opt_in_and_replies_precede_following_queries() {
    let input = b"\x1b]7501;?\x07\x1b[c";
    assert_eq!(
        Terminal::new(10, 3, 0).feed(input),
        [Effect::Write(b"\x1b[?62;22c".to_vec())]
    );
    for split in 0..=input.len() {
        let mut t = terminal();
        let mut effects = t.feed(&input[..split]);
        effects.extend(t.feed(&input[split..]));
        assert_eq!(effects, [Effect::Write(b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x07".to_vec()), Effect::Write(b"\x1b[?62;22c".to_vec())]);
    }
}

#[test]
fn reports_decode_all_states_and_reset_in_order() {
    for (name, state) in [
        ("idle", State::Idle),
        ("working", State::Working),
        ("blocked", State::Blocked),
        ("done", State::Done),
        ("error", State::Error),
        ("clear", State::Clear),
    ] {
        for ending in [b"\x07".as_slice(), b"\x1b\\"] {
            let input = [
                format!("\x1b]7501;state={name}:id=a/b:msg=5a6J5YWo").as_bytes(),
                ending,
            ]
            .concat();
            for split in 0..=input.len() {
                let mut t = terminal();
                let mut effects = t.feed(&input[..split]);
                effects.extend(t.feed(&input[split..]));
                assert_eq!(
                    effects,
                    [Effect::ProgramStatus(Report {
                        state,
                        id: Some("a/b".into()),
                        message: "安全".into()
                    })],
                    "{ending:?} split {split}"
                );
            }
        }
    }
    let mut t = terminal();
    assert_eq!(
        t.feed(b"\x1bc\x1b]7501;state=clear\x07"),
        [
            Effect::ProgramStatusReset,
            Effect::ProgressReset,
            Effect::ProgramStatus(Report {
                state: State::Clear,
                id: None,
                message: String::new()
            })
        ]
    );
}

#[test]
fn validation_matches_the_old_parser_and_recovers_after_rejected_records() {
    let good = [
        "garbage:=x:Bad=1:app=b@d:future=yes:state=working:app=ok:::".to_owned(),
        "state=idle: state = done :msg=QQ==:msg=Qg".into(),
        format!("state=done:msg={}", "QUFB".repeat(682)),
        format!("state=idle:{}", "x".repeat(4087 - 11)),
    ];
    for body in good {
        assert!(
            matches!(
                terminal()
                    .feed(format!("\x1b]7501;{body}\x07").as_bytes())
                    .as_slice(),
                [Effect::ProgramStatus(_)]
            ),
            "{body}"
        );
    }
    let bad = [
        "state=nope".to_owned(),
        "state=done:id=".into(),
        "state=clear:id=a//b".into(),
        "state=clear:id=@:id=a".into(),
        format!("state=done:id={}", "a".repeat(33)),
        format!("state=done:id={}", ["a"; 9].join("/")),
        format!("state=done:app={}:app=a", "@".repeat(33)),
        format!("state=done:{}=a", "x".repeat(17)),
        "state=done:msg=a".into(),
        "state=done:msg=QQ=".into(),
        "state=done:msg=YQpi".into(),
        "state=done:msg=wps=".into(),
        "state=done:msg=/w==".into(),
        "state=done:title=a".into(),
        format!("state=done:title={}", "QUFB".repeat(65)),
        format!("state=done:msg={}:msg=", "QUFB".repeat(684)),
        format!("state=idle:{}", "x".repeat(4088 - 11)),
    ];
    for body in bad {
        let input = format!("\x1b]7501;{body}\x07ok\x1b[c");
        let mut t = terminal();
        assert_eq!(
            t.feed(input.as_bytes()),
            [Effect::Write(b"\x1b[?62;22c".to_vec())],
            "{body}"
        );
        assert_eq!(t.screen().cursor.col, 2);
    }
    assert_eq!(
        terminal().feed(b"\x1b]7501;state=idle:msg=QQ==:msg=Qg\x07"),
        [Effect::ProgramStatus(Report {
            state: State::Idle,
            id: None,
            message: "B".into()
        })]
    );
    for cancel in [0x18, 0x1a] {
        let input = [b"\x1b]7501;state=done".as_slice(), &[cancel], b"ok\x1b[c"].concat();
        for split in 0..=input.len() {
            let mut t = terminal();
            let mut effects = t.feed(&input[..split]);
            effects.extend(t.feed(&input[split..]));
            assert_eq!(effects, [Effect::Write(b"\x1b[?62;22c".to_vec())]);
        }
    }
}

#[test]
fn synchronous_status_queries_keep_order_and_host_opt_in_is_not_serialized() {
    #[derive(Default)]
    struct Handler(Vec<Effect>);
    impl rustty_vt::EffectHandler for Handler {
        fn effect(&mut self, effect: Effect) {
            self.0.push(effect);
        }
        fn device_attributes(&mut self) -> Option<rustty_vt::query::DeviceAttributes> {
            Some(Default::default())
        }
    }
    let mut t = terminal();
    let mut handler = Handler::default();
    t.feed_with_handler(b"\x1b]7501;?\x1b\\\x1b[c", &mut handler);
    assert_eq!(handler.0, [Effect::Write(b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\".to_vec()), Effect::Write(b"\x1b[?62;22c".to_vec())]);
    let wire = rustty_vt::snapshot::encode_to_vec(&t).unwrap();
    let restored = rustty_vt::snapshot::decode(wire.as_slice(), Default::default()).unwrap();
    assert!(!restored.program_status_events);
}
