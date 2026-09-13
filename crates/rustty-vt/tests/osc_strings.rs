use rustty_vt::{Effect, SemanticContent, Terminal};

fn osc(number: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!("\x1b]{number};").into_bytes();
    bytes.extend_from_slice(body);
    bytes.push(7);
    bytes
}

#[test]
fn title_validation_precedes_byte_truncation_and_raw_setters_preserve_data() {
    let mut terminal = Terminal::new(80, 24, 0);
    let mut title = vec![b'a'; 1023];
    title.extend_from_slice("é".as_bytes());
    assert_eq!(
        terminal.feed(&osc("2", &title)),
        [Effect::Title(title[..1024].to_vec())]
    );
    assert_eq!(terminal.title_bytes(), &title[..1024]);
    assert!(std::str::from_utf8(terminal.title_bytes()).is_err());
    title.push(0xff);
    assert!(terminal.feed(&osc("2", &title)).is_empty());
    assert_eq!(terminal.title_bytes(), &title[..1024]);
    assert!(terminal.feed(&osc("2", &[b'x'; 2048])).is_empty());
    assert_eq!(terminal.title_bytes(), &title[..1024]);

    let mut raw = vec![0xff; 4097];
    raw.push(0);
    terminal.set_title(&raw);
    terminal.set_working_directory(&raw);
    let snapshot = rustty_vt::snapshot::encode_to_vec(&terminal).unwrap();
    let mut terminal =
        rustty_vt::snapshot::decode(snapshot.as_slice(), Default::default()).unwrap();
    assert_eq!(terminal.title_bytes(), raw);
    assert_eq!(terminal.working_directory_bytes(), raw);
    terminal.title_report = true;
    let mut report = b"\x1b]l".to_vec();
    report.extend_from_slice(&raw);
    report.extend_from_slice(b"\x1b\\");
    assert_eq!(terminal.feed(b"\x1b[21t"), [Effect::Write(report)]);
}

#[test]
fn pwd_aliases_preserve_bytes_and_require_exact_command_framing() {
    let mut terminal = Terminal::new(80, 24, 0);
    for (number, body) in [
        ("7", b"file:///raw\xff".as_slice()),
        ("9", b"9;file:///raw\xff"),
        ("1337", b"cUrReNtDiR=file:///raw\xff"),
    ] {
        assert_eq!(
            terminal.feed(&osc(number, body)),
            [Effect::WorkingDirectory(b"file:///raw\xff".to_vec())]
        );
        assert_eq!(terminal.working_directory_bytes(), b"file:///raw\xff");
    }
    for bytes in [
        osc("07", b"changed"),
        osc("+7", b"changed"),
        osc("7", &[b'x'; 2048]),
        osc("1337", b"CurrentDir="),
        b"\x1b]7\x07".to_vec(),
    ] {
        assert!(terminal.feed(&bytes).is_empty());
        assert_eq!(terminal.working_directory_bytes(), b"file:///raw\xff");
    }
    assert_eq!(
        terminal.feed(&osc("9", b"9;")),
        [Effect::WorkingDirectory(Vec::new())]
    );
    assert!(terminal.working_directory_bytes().is_empty());
}

#[test]
fn conemu_commands_are_distinct_from_notifications_and_progress_at_capture_limit() {
    let mut terminal = Terminal::new(80, 24, 0);
    for body in [
        "1;2",
        "10",
        "10;0suffix",
        "11;comment",
        "2;hi",
        "3;",
        "5suffix",
        "6;m",
        "7;run",
        "8;ENV",
    ] {
        assert!(
            terminal.feed(&osc("9", body.as_bytes())).is_empty(),
            "{body}"
        );
    }
    for body in ["1", "10;", "10;4", "2", "3", "4;5", "6", "7", "8", "9"] {
        assert_eq!(
            terminal.feed(&osc("9", body.as_bytes())),
            [Effect::Notification {
                title: Vec::new(),
                body: body.as_bytes().to_vec()
            }]
        );
    }
    assert_eq!(
        terminal.feed(&osc("9", &[b'x'; 2047])),
        [Effect::Notification {
            title: Vec::new(),
            body: vec![b'x'; 2047]
        }]
    );
    assert!(terminal.feed(&osc("9", &[b'x'; 2048])).is_empty());
    let progress = format!("4;1;{}", "0".repeat(2044));
    assert_eq!(
        terminal.feed(&osc("9", progress.as_bytes())),
        [Effect::Progress {
            state: 1,
            value: Some(0)
        }]
    );
    terminal.feed(b"xx");
    assert!(terminal.feed(&osc("9", b"12suffix")).is_empty());
    assert_eq!(
        (terminal.screen().cursor.col, terminal.screen().cursor.row),
        (0, 1)
    );
    assert_eq!(terminal.screen().cursor.semantic, SemanticContent::Prompt);
    assert_eq!(terminal.screen().rows[1].semantic, SemanticContent::Prompt);
}
