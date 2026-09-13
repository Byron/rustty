use rustty_vt::{Effect, Terminal};

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
