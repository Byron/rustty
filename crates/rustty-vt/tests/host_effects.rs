use rustty_vt::clipboard::{
    self, Content, Location, Read, ReadResult, ReadSuccess, Terminator, Write, WriteResult,
};
use rustty_vt::{Effect, EffectHandler, Terminal};

#[test]
fn notifications_preserve_bytes_and_require_complete_rxvt_fields() {
    let mut terminal = Terminal::new(10, 2, 0);
    assert_eq!(
        terminal.feed(b"\x1b]9;hello\xff\x07\x1b]777;notify;title\xfe;body\xff\x1b\\"),
        [
            Effect::Notification {
                title: Vec::new(),
                body: b"hello\xff".to_vec(),
            },
            Effect::Notification {
                title: b"title\xfe".to_vec(),
                body: b"body\xff".to_vec(),
            },
        ]
    );
    assert!(terminal.feed(b"\x1b]777;notify;title\x07").is_empty());
    for body in [b"4;".as_slice(), b"4;5;100"] {
        let mut input = b"\x1b]9;".to_vec();
        input.extend_from_slice(body);
        input.push(7);
        assert_eq!(
            terminal.feed(&input),
            [Effect::Notification {
                title: Vec::new(),
                body: body.to_vec(),
            }]
        );
    }
}

#[test]
fn progress_distinguishes_missing_invalid_and_clamped_values() {
    let mut terminal = Terminal::new(10, 2, 0);
    for (data, state, value) in [
        ("0;70", 0, None),
        ("1", 1, Some(0)),
        ("1;42", 1, Some(42)),
        ("1;256", 1, Some(100)),
        ("1;-1", 1, None),
        ("1;+1", 1, None),
        ("1;1__0", 1, Some(10)),
        ("1;_10", 1, None),
        ("1;10_", 1, None),
        ("1;", 1, None),
        ("1;10;", 1, None),
        ("2", 2, None),
        ("2;25", 2, Some(25)),
        ("3;25", 3, None),
        ("4", 4, None),
        ("4;75", 4, Some(75)),
        ("10", 1, Some(0)),
    ] {
        assert_eq!(
            terminal.feed(format!("\x1b]9;4;{data}\x07").as_bytes()),
            [Effect::Progress { state, value }],
            "{data}"
        );
    }
    assert_eq!(
        terminal.feed(b"\x1bc"),
        [Effect::Progress {
            state: 0,
            value: None,
        }]
    );
}

#[test]
fn clipboard_writes_validate_selectors_and_base64_before_dispatch() {
    let mut terminal = Terminal::new(10, 2, 0);
    for (selector, location) in [
        ("", Location::Standard),
        ("c", Location::Standard),
        ("p", Location::Primary),
        ("s", Location::Selection),
        ("x", Location::Standard),
    ] {
        for encoded in ["Zg==", "Zg", "Zh=="] {
            assert_eq!(
                terminal.feed(format!("\x1b]52;{selector};{encoded}\x07").as_bytes()),
                [Effect::ClipboardWrite(Write::osc52(
                    location,
                    vec![Content {
                        mime: b"text/plain".to_vec(),
                        data: b"f".as_slice().into(),
                    }],
                ))]
            );
        }
        assert_eq!(
            terminal.feed(format!("\x1b]52;{selector};\x07").as_bytes()),
            [Effect::ClipboardWrite(Write::osc52(location, Vec::new()))]
        );
    }
    for input in [
        b"\x1b]52;cp;Zg==\x07".as_slice(),
        b"\x1b]52;c;Z\x07",
        b"\x1b]52;c;Zg=\x07",
        b"\x1b]52;c;Zg??\x07",
        b"\x1b]52;c;Zg== \x07",
        b"\x1b]52;\x07",
    ] {
        assert!(terminal.feed(input).is_empty(), "{input:?}");
    }
}

#[test]
fn clipboard_reads_reply_in_event_order_and_preserve_binary_text_and_terminator() {
    #[derive(Default)]
    struct Host(Vec<Effect>);
    impl EffectHandler for Host {
        fn effect(&mut self, effect: Effect) {
            self.0.push(effect);
        }

        fn clipboard_read(&mut self, request: &Read) -> ReadResult {
            self.0.push(Effect::ClipboardRead(request.clone()));
            ReadResult::Success(ReadSuccess {
                contents: vec![
                    Content {
                        mime: b"image/png".to_vec(),
                        data: b"ignored".as_slice().into(),
                    },
                    Content {
                        mime: b"UTF8_STRING".to_vec(),
                        data: b"a\0\xff".as_slice().into(),
                    },
                    Content {
                        mime: b"text/plain".to_vec(),
                        data: b"second text ignored".as_slice().into(),
                    },
                ],
                ..ReadSuccess::default()
            })
        }

        fn clipboard_write(&mut self, request: &Write) -> WriteResult {
            self.0.push(Effect::ClipboardWrite(request.clone()));
            WriteResult::Success { remember: false }
        }
    }

    let mut terminal = Terminal::new(10, 2, 0);
    let mut host = Host::default();
    terminal.feed_with_handler(
        b"\x07\x1b]52;p;?\x07\x1b[6n\x1b]52;s;?\x1b\\\x1b]52;c;\x07",
        &mut host,
    );
    assert_eq!(
        host.0,
        [
            Effect::Bell,
            Effect::ClipboardRead(Read::osc52(Location::Primary, Terminator::Bell)),
            Effect::Write(b"\x1b]52;p;YQD/\x07".to_vec()),
            Effect::Write(b"\x1b[1;1R".to_vec()),
            Effect::ClipboardRead(Read::osc52(Location::Selection, Terminator::St)),
            Effect::Write(b"\x1b]52;s;YQD/\x1b\\".to_vec()),
            Effect::ClipboardWrite(Write::osc52(Location::Standard, Vec::new())),
        ]
    );

    // Deferred callers use the same encoder. A denied/unavailable read still
    // gets a reply so the querying application does not wait indefinitely.
    let [Effect::ClipboardRead(request)]: [Effect; 1] =
        terminal.feed(b"\x1b]52;x;?\x07").try_into().unwrap()
    else {
        panic!("missing clipboard request");
    };
    assert_eq!(request.reply(&[]), b"\x1b]52;c;\x07");
    assert!(clipboard::is_text_mime(b"text/plain;charset=utf-8"));
    assert!(!clipboard::is_text_mime(b"text/plain;charset=UTF-8"));
}
