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
                        message: "安全".into(),
                        ..Default::default()
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
                message: String::new(),
                ..Default::default()
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
            message: "B".into(),
            ..Default::default()
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

#[test]
fn retained_status_follows_prompt_reset_and_actual_focus() {
    for tmux in [false, true] {
        let mut t = terminal();
        t.set_tmux_strings(tmux);
        t.tmux_graphics = tmux;
        t.program_status_focus(true);
        let records = b"\x1b]7501;state=working:id=w\x07\x1b]7501;state=blocked:id=b\x07\x1b]7501;state=idle:id=i\x07\x1b]7501;state=done:id=d\x07\x1b]7501;state=error:id=e\x07";
        t.feed(records);
        assert_eq!(t.program_status().len(), 5);
        let before = t.program_status().clone();
        for inert in [
            b"\x1b[?1049h\x1b[?1049l".as_slice(),
            b"\x1b[!p",
            b"\x1b]133;C\x07\x1b]133;D;0\x07",
            b"\x1b]133;P\x07",
            b"\x1b]133;A;k=s\x07",
            b"\x1b]133;A;k=c\x07",
            b"\x1b]133;N;k=s;k=p\x07",
            b"\x1b]7501;process-exited\x07",
        ] {
            t.feed(inert);
            assert_eq!(t.program_status(), &before);
        }
        assert!(!t.program_status_focus(true));
        t.feed(b"\x1b]133;A\x07");
        assert_eq!(t.program_status().len(), 2);
        assert!(t.program_status().get(Some("d")).is_some());
        assert!(!t.program_status_focus(false));
        assert!(t.program_status_focus(true));
        assert!(t.program_status().is_empty());
        t.feed(records);
        t.feed(b"\x1bc");
        assert!(t.program_status().is_empty());
        t.feed(b"\x1b]7501;state=done\x07");
        // RIS keeps real host focus; only the reports were reset.
        assert!(!t.program_status_focus(true));
        assert_eq!(t.program_status().len(), 1);
        assert!(t.program_status_seen());
        assert!(!t.program_status_seen());
        t.feed(records);
        t.feed(b"\x1b]133;N;aid=1\x07");
        assert_eq!(t.program_status().len(), 2);
    }
}

#[test]
fn retained_status_snapshots_validate_optional_metadata_and_preserve_orders() {
    let mut t = terminal();
    t.feed(b"\x1b]7501;state=working:app=root\x07\x1b]7501;state=blocked:id=a:kind=auth:progress=50\x07\x1b]7501;state=working:app=root\x07");
    let wire = rustty_vt::snapshot::encode_to_vec(&t).unwrap();
    let restored = rustty_vt::snapshot::decode_exact(wire.as_slice(), Default::default()).unwrap();
    assert_eq!(restored.program_status(), t.program_status());
    assert!(!restored.program_status_events);
    let visual = rustty_vt::visual::VisualSnapshot::capture(&t).unwrap();
    let mut fresh = terminal();
    visual.restore_into(&mut fresh).unwrap();
    assert!(fresh.program_status().is_empty());
    let mut offset = 10;
    while u16::from_le_bytes(wire[offset..offset + 2].try_into().unwrap()) != 0x8004 {
        offset +=
            10 + u32::from_le_bytes(wire[offset + 2..offset + 6].try_into().unwrap()) as usize;
    }
    let end =
        offset + 10 + u32::from_le_bytes(wire[offset + 2..offset + 6].try_into().unwrap()) as usize;
    let old = [&wire[..offset], &wire[end..]].concat();
    assert!(
        rustty_vt::snapshot::decode_exact(old.as_slice(), Default::default())
            .unwrap()
            .program_status()
            .is_empty()
    );
    for payload in [
        b"".as_slice(),
        b"\x02{}",
        b"\x01{}",
        b"\x01{\"records\":[{\"state\":\"clear\"}],\"reported\":[null]}",
    ] {
        let mut header = vec![4, 128];
        header.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        let crc = crc32c::crc32c_append(crc32c::crc32c(&header), payload);
        header.extend_from_slice(&crc.to_le_bytes());
        let bad = [&wire[..offset], &header, payload, &wire[end..]].concat();
        assert!(rustty_vt::snapshot::decode_exact(bad.as_slice(), Default::default()).is_err());
    }
    // Reject the announced allocation before attempting to read an oversized body.
    let mut oversized = wire[..offset].to_vec();
    oversized.extend(0x8004u16.to_le_bytes());
    oversized.extend((2 * 1024 * 1024 + 1u32).to_le_bytes());
    oversized.extend([0; 4]);
    let error =
        rustty_vt::snapshot::decode_exact(oversized.as_slice(), Default::default()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn maximum_program_records_fit_the_snapshot_budget() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let mut t = terminal();
    let title = STANDARD.encode("\"".repeat(192));
    let message = STANDARD.encode("\\".repeat(2048));
    for index in 0..rustty_vt::program_status::MAX_RECORDS {
        t.feed(format!("\x1b]7501;state=blocked:id=job{index}:kind=question:progress=100:title={title}:msg={message}\x07").as_bytes());
    }
    assert_eq!(t.program_status().len(), 256);
    let bytes = rustty_vt::snapshot::encode_to_vec(&t).unwrap();
    assert!(bytes.len() < 2 * 1024 * 1024);
    let restored = rustty_vt::snapshot::decode_exact(bytes.as_slice(), Default::default()).unwrap();
    assert_eq!(restored.program_status(), t.program_status());
}
