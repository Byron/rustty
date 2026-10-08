use base64::{Engine, engine::general_purpose::STANDARD};
use rustty_vt::{Effect, EffectHandler, Terminal, agent, snapshot};

fn osc(json: &[u8], terminator: &[u8]) -> Vec<u8> {
    let mut frame = format!("\x1b]777;rustty-agent;1;{}", STANDARD.encode(json)).into_bytes();
    frame.extend_from_slice(terminator);
    frame
}

#[test]
fn every_state_and_unicode_survive_both_terminators_and_every_split() {
    for state in [
        "idle",
        "working",
        "needs_input",
        "done",
        "error",
        "paused",
        "unknown",
    ] {
        let json = format!(
            r#"{{"op":"update","state":"{state}","label":"Review Δ 🦀","thread_id":"thread-A","turn_id":"turn-1"}}"#
        );
        let expected = agent::Event::from_json(json.as_bytes()).unwrap();
        for terminator in [&b"\x07"[..], &b"\x1b\\"[..]] {
            let frame = osc(json.as_bytes(), terminator);
            for split in 0..=frame.len() {
                let mut terminal = Terminal::new(8, 3, 0);
                terminal.agent_status_events = true;
                let mut effects = terminal.feed(&frame[..split]);
                effects.extend(terminal.feed(&frame[split..]));
                assert_eq!(effects, [Effect::AgentStatus(expected.clone())]);
                assert!(terminal.title.is_empty());
                assert_eq!(terminal.generation, 0);
            }
        }
        let frame = expected.frame().unwrap();
        assert_eq!(
            agent::Event::decode(&frame[19..frame.len() - 2]),
            Some(expected)
        );
    }
}

#[test]
fn malformed_packets_have_no_host_effects() {
    let mut terminal = Terminal::new(8, 3, 0);
    terminal.agent_status_events = true;
    let mut invalid = vec![
        b"".to_vec(),
        b"[]".to_vec(),
        b"null".to_vec(),
        br#"{"op":"begin"}"#.to_vec(),
        br#"{"op":"update","state":"waiting"}"#.to_vec(),
        br#"{"op":"update","state":"done"}"#.to_vec(),
        br#"{"op":"update","state":"idle","label":null}"#.to_vec(),
        br#"{"op":"update","state":"idle","thread_id":null}"#.to_vec(),
        br#"{"op":"update","state":"idle","turn_id":null}"#.to_vec(),
        br#"{"op":"update","state":"idle","label":3}"#.to_vec(),
        br#"{"op":"update","state":"idle","label":"one","label":"two"}"#.to_vec(),
        br#"{"op":"update","op":"begin","state":"idle"}"#.to_vec(),
        br#"{"op":"update","state":"idle","pane_id":"123"}"#.to_vec(),
        br#"{"op":"end","state":"idle"}"#.to_vec(),
        br#"{"op":"end","label":"bad"}"#.to_vec(),
        br#"{"op":"end","op":"end"}"#.to_vec(),
        br#"{"op":"end","extra":null}"#.to_vec(),
        br#"{"op":"exit"}"#.to_vec(),
        br#"{"op":"update","state":"idle","label":"escape\u001b"}"#.to_vec(),
        br#"{"op":"update","state":"idle","label":"control\u0085"}"#.to_vec(),
        br#"{"op":"update","state":"idle","thread_id":"tab\t"}"#.to_vec(),
        br#"{"op":"update","state":"done","turn_id":""}"#.to_vec(),
        br#"{"op":"end"} {}"#.to_vec(),
    ];
    for (field, value) in [
        ("label", "é".repeat(65)),
        ("thread_id", "é".into()),
        ("thread_id", "a".repeat(129)),
        ("turn_id", "a".repeat(129)),
        ("turn_id", "\u{7f}".into()),
    ] {
        invalid.push(
            serde_json::to_vec(&serde_json::json!({
                "op":"update", "state":"idle", field:value,
            }))
            .unwrap(),
        );
    }
    invalid.push(
        [
            br#"{"op":"update","state":"idle","label":""#.as_slice(),
            &[0xff],
            br#""}"#,
        ]
        .concat(),
    );
    invalid.push([br#"{"op":"end"}"#.as_slice(), &[b' '; 1025]].concat());
    for json in invalid {
        assert!(terminal.feed(&osc(&json, b"\x07")).is_empty(), "{json:?}");
    }
    let valid = STANDARD.encode(br#"{"op":"end" }"#);
    for payload in [
        format!("2;{valid}"),
        format!("01;{valid}"),
        format!("1;{valid};"),
        format!("1; {valid}"),
        format!("1;{}", valid.trim_end_matches('=')),
        "1;!!!!".into(),
        format!("1;{}", "A".repeat(2048)),
    ] {
        assert!(
            terminal
                .feed(format!("\x1b]777;rustty-agent;{payload}\x1b\\").as_bytes())
                .is_empty(),
            "{payload}"
        );
    }
}

#[test]
fn json_and_metadata_limits_are_inclusive_and_no_agent_data_is_saved() {
    let json = serde_json::to_vec(&serde_json::json!({
        "op":"begin", "state":"done", "label":"é".repeat(64),
        "thread_id":"t".repeat(128), "turn_id":"u".repeat(128),
    }))
    .unwrap();
    let mut padded = json.clone();
    padded.resize(1024, b' ');
    assert!(agent::Event::from_json(&padded).is_ok());
    padded.push(b' ');
    assert!(agent::Event::from_json(&padded).is_err());

    let mut terminal = Terminal::new(8, 3, 0);
    let original = snapshot::encode_to_vec(&terminal).unwrap();
    assert!(terminal.feed(&osc(&json, b"\x07")).is_empty());
    terminal.agent_status_events = true;
    assert_eq!(terminal.feed(&osc(&json, b"\x07")).len(), 1);
    assert_eq!(snapshot::encode_to_vec(&terminal).unwrap(), original);
    terminal.reset();
    assert!(terminal.agent_status_events);
    assert!(
        !terminal
            .feed(b"\x1bc")
            .iter()
            .any(|effect| matches!(effect, Effect::AgentStatus(_)))
    );
    assert!(terminal.agent_status_events);
    let restored = snapshot::decode(&original[..], Default::default()).unwrap();
    assert!(!restored.agent_status_events);
    terminal.agent_status_events = false;
    assert!(
        terminal
            .feed(&agent::Event::End.frame().unwrap())
            .is_empty()
    );
}

#[test]
fn synchronous_host_keeps_agent_lifecycle_and_notifications_ordered() {
    #[derive(Default)]
    struct Host(Vec<Effect>);
    impl EffectHandler for Host {
        fn effect(&mut self, effect: Effect) {
            self.0.push(effect);
        }
    }
    let begin = agent::Event::from_json(br#"{"op":"begin","state":"idle"}"#).unwrap();
    let update = agent::Event::from_json(br#"{"op":"update","state":"working"}"#).unwrap();
    let mut input = begin.frame().unwrap();
    input.extend_from_slice(b"\x1b]777;notify;title;body\x07");
    input.extend(update.frame().unwrap());
    input.extend(agent::Event::End.frame().unwrap());
    let mut terminal = Terminal::new(8, 3, 0);
    terminal.agent_status_events = true;
    let mut host = Host::default();
    for byte in input {
        terminal.feed_with_handler(&[byte], &mut host);
    }
    assert_eq!(
        host.0,
        [
            Effect::AgentStatus(begin),
            Effect::Notification {
                title: b"title".to_vec(),
                body: b"body".to_vec()
            },
            Effect::AgentStatus(update),
            Effect::AgentStatus(agent::Event::End),
        ]
    );
}
