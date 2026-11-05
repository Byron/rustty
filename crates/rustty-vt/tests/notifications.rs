use base64::{Engine, engine::general_purpose::STANDARD};
use rustty_vt::{
    Effect, EffectHandler, Terminal,
    notification::{self, Action, Capabilities, Command, Message, Reply},
    snapshot,
};

fn enabled() -> Terminal {
    let mut terminal = Terminal::new(10, 3, 10);
    terminal.notification_capabilities = Some(Capabilities {
        buttons: true,
        close_events: true,
    });
    terminal
}
fn packet(metadata: &str, payload: &[u8]) -> Vec<u8> {
    [
        format!("\x1b]99;{metadata};").as_bytes(),
        payload,
        b"\x1b\\",
    ]
    .concat()
}
fn feed(terminal: &mut Terminal, metadata: &str, payload: &[u8]) -> Vec<Effect> {
    terminal.feed(&packet(metadata, payload))
}
fn command(effects: Vec<Effect>) -> Command {
    let [Effect::DesktopNotification(command)] = effects.as_slice() else {
        panic!("expected one notification command: {effects:?}");
    };
    command.clone()
}
fn shown(effects: Vec<Effect>) -> Message {
    let Action::Show(message) = command(effects).action else {
        panic!("expected Show");
    };
    message
}
fn message(title: &str) -> Message {
    Message {
        identifier: None,
        title: title.into(),
        body: String::new(),
        buttons: Vec::new(),
        focus: true,
        report: false,
        close_report: false,
        sound: true,
    }
}

#[test]
fn default_noop_and_legacy_notifications_are_unchanged() {
    let mut terminal = Terminal::new(10, 3, 10);
    let before = snapshot::encode_to_vec(&terminal).unwrap();
    for (meta, body) in [
        ("", b"hello".as_slice()),
        ("i=a:d=0", b"partial"),
        ("p=?", b""),
        ("i=a:p=close", b""),
        ("i=a:p=alive", b""),
    ] {
        assert!(feed(&mut terminal, meta, body).is_empty());
    }
    assert_eq!(snapshot::encode_to_vec(&terminal).unwrap(), before);
    assert_eq!(
        terminal.feed(b"\x1b]777;notify;old;body\x1b\\"),
        [Effect::Notification {
            title: b"old".to_vec(),
            body: b"body".to_vec()
        }]
    );
    terminal.notification_capabilities = Some(Capabilities {
        buttons: true,
        close_events: true,
    });
    assert_eq!(command(feed(&mut terminal, "", b"enabled")).serial, 1);
    assert_eq!(snapshot::encode_to_vec(&terminal).unwrap(), before);
    assert_eq!(terminal.generation, 0);
}

#[test]
fn multipart_unicode_survives_every_wire_split_and_both_terminators() {
    let mut input = packet(
        "i=build:a=-focus,report:c=1:s=c2lsZW50:d=0",
        "Build 🦀".as_bytes(),
    );
    input.extend(packet(
        "i=build:p=body:e=1:d=0",
        STANDARD.encode("line one\nline two").as_bytes(),
    ));
    input.extend(packet(
        "i=build:p=buttons",
        "Open\u{2028}Dismiss".as_bytes(),
    ));
    let expected = Message {
        identifier: Some("build".into()),
        title: "Build 🦀".into(),
        body: "line one\nline two".into(),
        buttons: vec!["Open".into(), "Dismiss".into()],
        focus: false,
        report: true,
        close_report: true,
        sound: false,
    };
    for bell in [false, true] {
        let input = if bell {
            input
                .split(|&b| b == b'\\')
                .flat_map(|chunk| {
                    if chunk.ends_with(b"\x1b") {
                        [chunk[..chunk.len() - 1].to_vec(), vec![7]].concat()
                    } else {
                        chunk.to_vec()
                    }
                })
                .collect::<Vec<_>>()
        } else {
            input.clone()
        };
        for split in 0..=input.len() {
            let mut terminal = enabled();
            let mut effects = terminal.feed(&input[..split]);
            effects.extend(terminal.feed(&input[split..]));
            assert_eq!(
                command(effects),
                Command {
                    serial: 1,
                    action: Action::Show(expected.clone())
                },
                "bell={bell} split={split}"
            );
            assert_eq!(terminal.generation, 0);
        }
    }
}

#[derive(Default)]
struct Handler(Vec<Effect>);
impl EffectHandler for Handler {
    fn effect(&mut self, effect: Effect) {
        self.0.push(effect);
    }
    fn device_attributes(&mut self) -> Option<rustty_vt::query::DeviceAttributes> {
        Some(Default::default())
    }
}

#[test]
fn support_query_is_synchronous_before_da1_without_consuming_a_serial() {
    for buttons in [false, true] {
        for close_events in [false, true] {
            let input = [packet("i=query:p=?", b"").as_slice(), b"\x1b[c"].concat();
            let types = if buttons {
                "title,body,buttons,close,alive,?"
            } else {
                "title,body,close,alive,?"
            };
            let expected = format!(
                "\x1b]99;i=query:p=?;a=focus,report:o=always:p={types}:s=system,silent{}\x1b\\",
                if close_events { ":c=1" } else { "" }
            );
            for split in 0..=input.len() {
                let mut terminal = enabled();
                terminal.notification_capabilities = Some(Capabilities {
                    buttons,
                    close_events,
                });
                let mut handler = Handler::default();
                terminal.feed_with_handler(&input[..split], &mut handler);
                terminal.feed_with_handler(&input[split..], &mut handler);
                assert_eq!(
                    handler.0,
                    [
                        Effect::Write(expected.as_bytes().to_vec()),
                        Effect::Write(b"\x1b[?62;22c".to_vec())
                    ]
                );
                assert_eq!(command(feed(&mut terminal, "", b"hello")).serial, 1);
            }
        }
    }
}

#[test]
fn base64_accepts_every_split_independent_padding_and_unpadded_final_groups() {
    let text = "a🦀b漢字é";
    for encoded in [
        STANDARD.encode(text),
        STANDARD.encode(text).trim_end_matches('=').to_owned(),
    ] {
        for split in 0..=encoded.len() {
            let mut terminal = enabled();
            assert!(feed(&mut terminal, "i=a:e=1:d=0", &encoded.as_bytes()[..split]).is_empty());
            assert_eq!(
                shown(feed(&mut terminal, "i=a:e=1", &encoded.as_bytes()[split..])).title,
                text,
                "split={split}"
            );
        }
    }
    let mut terminal = enabled();
    for part in [b"a".as_slice(), b"\xf0", b"\x9f\xa6\x80", b"bc"] {
        assert!(
            feed(
                &mut terminal,
                "i=a:e=1:d=0",
                STANDARD.encode(part).as_bytes()
            )
            .is_empty()
        );
    }
    assert_eq!(shown(feed(&mut terminal, "i=a", b"end")).title, "a🦀bcend");
    let mut terminal = enabled();
    assert!(feed(&mut terminal, "i=a:e=1:d=0", b"YQ").is_empty());
    assert!(feed(&mut terminal, "i=a:p=body:e=1:d=0", b"Ym9keQ").is_empty());
    assert_eq!(
        shown(feed(&mut terminal, "i=a", b"z")),
        Message {
            identifier: Some("a".into()),
            title: "az".into(),
            body: "body".into(),
            ..message("")
        }
    );
}

#[test]
fn metadata_carries_actions_and_sound_but_payload_defaults_reset_each_chunk() {
    let mut terminal = enabled();
    assert!(
        feed(
            &mut terminal,
            "i=a:d=0:a=-focus,report:c=1:s=c2lsZW50",
            b"one"
        )
        .is_empty()
    );
    assert!(feed(&mut terminal, "i=a:d=0:p=body", b"body1").is_empty());
    assert!(
        feed(
            &mut terminal,
            "i=a:d=0:p=body:a=-report,focus:c=0:s=dW5rbm93bg==",
            b"body2"
        )
        .is_empty()
    );
    assert_eq!(
        shown(feed(&mut terminal, "i=a", b"two")),
        Message {
            identifier: Some("a".into()),
            title: "onetwo".into(),
            body: "body1body2".into(),
            sound: false,
            ..message("")
        }
    );
    assert!(shown(feed(&mut terminal, "s=c3lzdGVt", b"system")).sound);
    assert!(!shown(feed(&mut terminal, "s=c2lsZW50:s=bad", b"silent")).sound);
    assert_eq!(
        shown(feed(&mut terminal, "p=body", b"only body")),
        message("only body")
    );
    assert!(feed(&mut terminal, "p=buttons", b"No title").is_empty());
    assert!(feed(&mut terminal, "i=a:d=0", b"title").is_empty());
    assert_eq!(
        shown(feed(
            &mut terminal,
            "i=a:p=icon:e=1:x=future",
            b"not base64"
        )),
        Message {
            identifier: Some("a".into()),
            ..message("title")
        }
    );
}

#[test]
fn identifiers_replacement_and_queries_do_not_confuse_pending_transactions() {
    let mut terminal = enabled();
    assert!(feed(&mut terminal, "i=old:d=0", b"discard").is_empty());
    assert_eq!(shown(feed(&mut terminal, "i=new", b"fresh")).title, "fresh");
    assert!(feed(&mut terminal, "i=wait:d=0", b"start").is_empty());
    assert!(matches!(
        feed(&mut terminal, "i=query:p=?", b"").as_slice(),
        [Effect::Write(_)]
    ));
    assert_eq!(
        command(feed(&mut terminal, "i=other:p=close", b"")),
        Command {
            serial: 2,
            action: Action::Close {
                identifier: "other".into()
            }
        }
    );
    assert_eq!(
        command(feed(&mut terminal, "i=query:p=alive", b"")),
        Command {
            serial: 3,
            action: Action::Alive {
                identifier: "query".into()
            }
        }
    );
    assert_eq!(
        command(feed(&mut terminal, "i=wait", b"end")),
        Command {
            serial: 4,
            action: Action::Show(Message {
                identifier: Some("wait".into()),
                ..message("startend")
            })
        }
    );
    for expected in [5, 6] {
        let command = command(feed(&mut terminal, "", b"anonymous"));
        assert_eq!(command.serial, expected);
        assert!(matches!(
            command.action,
            Action::Show(Message {
                identifier: None,
                ..
            })
        ));
    }
    assert!(feed(&mut terminal, "p=close", b"").is_empty());
    assert!(feed(&mut terminal, "p=alive", b"").is_empty());
}

#[test]
fn invalid_metadata_and_utf8_never_become_commands_or_reply_injection() {
    let mut terminal = enabled();
    for metadata in [
        "i=bad,id",
        "i=bad/id",
        "i=bad=id",
        "i=é",
        "i=bad,id:i=good",
        "broken",
        "e=2",
        "d=2",
        "c=2",
        "unknown=one",
    ] {
        assert!(
            feed(&mut terminal, metadata, b"text").is_empty(),
            "{metadata}"
        );
    }
    for data in [
        b"\xff".as_slice(),
        b"\xc0\xaf",
        b"\xed\xa0\x80",
        b"\xe2\x82",
        b"\xc2\x80",
    ] {
        assert!(feed(&mut terminal, "", data).is_empty(), "{data:?}");
    }
    for data in [
        b"bad\x01text".as_slice(),
        b"bad\x7ftext",
        b"bad\ntext",
        b"bad\ttext",
        b"bad\xc2\x85text",
    ] {
        let mut handler = Handler::default();
        terminal.feed_osc_with_handler(&[b"99;;".as_slice(), data].concat(), Some(7), &mut handler);
        assert!(handler.0.is_empty(), "{data:?}");
    }
    assert_eq!(
        shown(feed(&mut terminal, "", b"bad\x01text")).title,
        "badtext"
    );
    assert_eq!(
        shown(feed(
            &mut terminal,
            "e=1",
            STANDARD.encode("a\0b\t\u{85}c\nd").as_bytes()
        ))
        .title,
        "abc\nd"
    );
    for data in [b"!".as_slice(), b"Y", b"Zh==", b"YQ=", b"/w==", b"wK8="] {
        assert!(feed(&mut terminal, "e=1", data).is_empty(), "{data:?}");
    }
    assert_eq!(
        shown(feed(&mut terminal, "x=future", b"semi;colon=equals")).title,
        "semi;colon=equals"
    );
}

#[test]
fn inclusive_metadata_and_button_bounds_are_enforced() {
    let mut terminal = enabled();
    assert_eq!(
        shown(feed(
            &mut terminal,
            &format!("i={}", "a".repeat(128)),
            b"ok"
        ))
        .identifier
        .unwrap()
        .len(),
        128
    );
    assert!(feed(&mut terminal, &format!("i={}", "a".repeat(129)), b"bad").is_empty());
    assert_eq!(
        shown(feed(
            &mut terminal,
            &format!("x={}", "a".repeat(4094)),
            b"ok"
        ))
        .title,
        "ok"
    );
    assert!(feed(&mut terminal, &format!("x={}", "a".repeat(4095)), b"bad").is_empty());
    assert_eq!(
        shown(feed(&mut terminal, &vec!["x=1"; 64].join(":"), b"ok")).title,
        "ok"
    );
    assert!(feed(&mut terminal, &vec!["x=1"; 65].join(":"), b"bad").is_empty());
    for (buttons, valid) in [
        ("one\u{2028}two\u{2028}three".to_owned(), true),
        ("1\u{2028}2\u{2028}3\u{2028}4".into(), false),
        ("🦀".repeat(256), true),
        ("x".repeat(1025), false),
    ] {
        assert!(feed(&mut terminal, "i=a:d=0", b"title").is_empty());
        let effects = feed(&mut terminal, "i=a:p=buttons", buttons.as_bytes());
        assert_eq!(!effects.is_empty(), valid);
    }
    terminal.notification_capabilities.as_mut().unwrap().buttons = false;
    assert!(feed(&mut terminal, "i=a:d=0", b"title").is_empty());
    assert!(
        shown(feed(&mut terminal, "i=a:p=buttons", b"one"))
            .buttons
            .is_empty()
    );
}

#[test]
fn wire_and_aggregate_limits_poison_until_the_transaction_finishes() {
    let mut terminal = enabled();
    assert_eq!(
        shown(feed(&mut terminal, "", &vec![b'a'; 2048]))
            .title
            .len(),
        2048
    );
    assert!(feed(&mut terminal, "", &vec![b'a'; 2049]).is_empty());
    assert_eq!(
        shown(feed(
            &mut terminal,
            "e=1",
            STANDARD.encode(vec![b'a'; 3072]).as_bytes()
        ))
        .title
        .len(),
        3072
    );
    assert!(feed(&mut terminal, "e=1", &vec![b'A'; 4097]).is_empty());
    for overflow in [false, true] {
        for _ in 0..32 {
            assert!(feed(&mut terminal, "i=large:d=0", &vec![b'a'; 2048]).is_empty());
        }
        if overflow {
            assert!(feed(&mut terminal, "i=large:d=0", b"!").is_empty());
        }
        let effects = feed(&mut terminal, "i=large", b"");
        if overflow {
            assert!(effects.is_empty());
        } else {
            assert_eq!(shown(effects).title.len(), notification::MAX_MESSAGE_BYTES);
        }
    }
    for bad in [
        packet("i=a:d=0", &vec![b'a'; 2049]),
        packet("i=a:e=1:d=0", b"?"),
        packet("broken", b"data"),
    ] {
        assert!(feed(&mut terminal, "i=a:d=0", b"before").is_empty());
        assert!(terminal.feed(&bad).is_empty());
        assert!(feed(&mut terminal, "i=a", b"after").is_empty());
        assert_eq!(shown(feed(&mut terminal, "i=a", b"fresh")).title, "fresh");
    }
}

#[test]
fn parser_capture_overflow_cannot_silently_drop_a_multipart_chunk() {
    let mut terminal = enabled();
    assert!(feed(&mut terminal, "i=a:d=0", b"before").is_empty());
    let effects = feed(
        &mut terminal,
        "i=a:d=0",
        &vec![b'x'; rustty_parser::MAX_OSC_BYTES + 1],
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::UnknownSequence(_)))
    );
    assert!(feed(&mut terminal, "i=a", b"after").is_empty());
    assert!(feed(&mut terminal, "i=a:d=0", b"before").is_empty());
    let mut handler = Handler::default();
    let direct = [
        b"99;i=a:d=0;".as_slice(),
        &vec![b'x'; rustty_parser::MAX_OSC_BYTES + 1],
    ]
    .concat();
    terminal.feed_osc_with_handler(&direct, None, &mut handler);
    assert!(handler.0.is_empty());
    assert!(feed(&mut terminal, "i=a", b"after").is_empty());
}

#[test]
fn ris_preserves_serial_and_policy_but_clears_multipart_and_snapshots_omit_it() {
    for tmux in [false, true] {
        let mut terminal = enabled();
        terminal.tmux_graphics = tmux;
        assert_eq!(command(feed(&mut terminal, "i=a", b"one")).serial, 1);
        let before = snapshot::encode_to_vec(&terminal).unwrap();
        assert!(feed(&mut terminal, "i=a:d=0", b"discard").is_empty());
        assert_eq!(snapshot::encode_to_vec(&terminal).unwrap(), before);
        terminal.feed(b"\x1bc");
        assert!(terminal.notification_capabilities.is_some());
        assert_eq!(
            command(feed(&mut terminal, "i=a", b"two")),
            Command {
                serial: 2,
                action: Action::Show(Message {
                    identifier: Some("a".into()),
                    ..message("two")
                })
            }
        );
        terminal.reset();
        assert_eq!(command(feed(&mut terminal, "i=a:p=alive", b"")).serial, 3);
        let mut restored = snapshot::decode(&before[..], Default::default()).unwrap();
        assert!(restored.notification_capabilities.is_none());
        restored.notification_capabilities = terminal.notification_capabilities;
        assert_eq!(command(feed(&mut restored, "i=a", b"fresh")).serial, 1);
    }
}

#[test]
fn reply_codec_checks_original_command_and_orders_activation_before_close() {
    let show = Command {
        serial: 1,
        action: Action::Show(Message {
            identifier: Some("a_+-1.2".into()),
            buttons: vec!["yes".into(), "no".into()],
            report: true,
            close_report: true,
            ..message("title")
        }),
    };
    assert_eq!(
        show.reply(&Reply::Activated { button: 0 }).unwrap(),
        b"\x1b]99;i=a_+-1.2;\x1b\\\x1b]99;i=a_+-1.2:p=close;\x1b\\"
    );
    assert_eq!(
        show.reply(&Reply::Activated { button: 2 }).unwrap(),
        b"\x1b]99;i=a_+-1.2;2\x1b\\\x1b]99;i=a_+-1.2:p=close;\x1b\\"
    );
    assert!(show.reply(&Reply::Activated { button: 3 }).is_err());
    for reply in [Reply::Closed, Reply::Failed] {
        assert_eq!(
            show.reply(&reply).unwrap(),
            b"\x1b]99;i=a_+-1.2:p=close;\x1b\\"
        );
    }
    assert_eq!(
        show.reply(&Reply::Untracked).unwrap(),
        b"\x1b]99;i=a_+-1.2:p=close;untracked\x1b\\"
    );
    assert!(show.reply(&Reply::Finished).unwrap().is_empty());
    assert!(
        show.reply(&Reply::Alive {
            identifiers: vec![]
        })
        .is_err()
    );
    let anonymous = Command {
        serial: 2,
        action: Action::Show(Message {
            report: true,
            ..message("anonymous")
        }),
    };
    assert_eq!(
        anonymous.reply(&Reply::Activated { button: 0 }).unwrap(),
        b"\x1b]99;i=0;\x1b\\"
    );
    for reply in [
        Reply::Closed,
        Reply::Failed,
        Reply::Untracked,
        Reply::Finished,
    ] {
        assert!(anonymous.reply(&reply).unwrap().is_empty());
    }
    let close = Command {
        serial: 3,
        action: Action::Close {
            identifier: "a".into(),
        },
    };
    for reply in [Reply::Finished, Reply::Failed] {
        assert!(close.reply(&reply).unwrap().is_empty());
    }
    for reply in [
        Reply::Closed,
        Reply::Untracked,
        Reply::Activated { button: 0 },
    ] {
        assert!(close.reply(&reply).is_err());
    }
    let alive = Command {
        serial: 4,
        action: Action::Alive {
            identifier: "query".into(),
        },
    };
    assert_eq!(
        alive
            .reply(&Reply::Alive {
                identifiers: vec!["one".into(), "two".into()]
            })
            .unwrap(),
        b"\x1b]99;i=query:p=alive;one,two\x1b\\"
    );
    assert_eq!(
        alive.reply(&Reply::Failed).unwrap(),
        b"\x1b]99;i=query:p=alive;\x1b\\"
    );
    for reply in [
        Reply::Finished,
        Reply::Closed,
        Reply::Untracked,
        Reply::Activated { button: 0 },
        Reply::Alive {
            identifiers: vec!["bad,id".into()],
        },
        Reply::Alive {
            identifiers: vec!["a".into(); 129],
        },
    ] {
        assert!(alive.reply(&reply).is_err());
    }
}

#[test]
fn transport_roundtrips_and_rejects_oversized_or_unsafe_host_values() {
    let mut terminal = enabled();
    let command = command(feed(&mut terminal, "i=transport", b"hello"));
    assert_eq!(
        serde_json::from_slice::<Command>(&serde_json::to_vec(&command).unwrap()).unwrap(),
        command
    );
    let mut value = serde_json::to_value(&command).unwrap();
    value["action"]["Show"]["buttons"] = serde_json::json!(["1", "2", "3", "4"]);
    assert!(serde_json::from_value::<Command>(value).is_err());
    for id in ["bad\x1b\\".into(), "a".repeat(129)] {
        let command = Command {
            serial: 1,
            action: Action::Alive { identifier: id },
        };
        assert!(command.validate().is_err());
        assert!(serde_json::from_slice::<Command>(&serde_json::to_vec(&command).unwrap()).is_err());
        assert!(command.reply(&Reply::Failed).is_err());
    }
    for (title, body) in [
        ("x".repeat(65537), String::new()),
        ("x".repeat(40000), "x".repeat(40000)),
        ("bad\x1btext".into(), String::new()),
    ] {
        let command = Command {
            serial: 1,
            action: Action::Show(Message {
                title,
                body,
                ..message("")
            }),
        };
        assert!(command.validate().is_err());
    }
    assert!(
        Command {
            serial: 0,
            action: Action::Show(message("title"))
        }
        .validate()
        .is_err()
    );
    for count in [129, 10000] {
        let reply = serde_json::json!({"Alive": {"identifiers": vec!["a"; count]}});
        assert!(serde_json::from_value::<Reply>(reply).is_err());
    }
}
