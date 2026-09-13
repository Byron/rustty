use rustty_vt::{Effect, Terminal};

fn writes(terminal: &mut Terminal, bytes: &[u8]) -> Vec<Vec<u8>> {
    terminal
        .feed(bytes)
        .into_iter()
        .filter_map(|effect| match effect {
            Effect::Write(bytes) => Some(bytes),
            _ => None,
        })
        .collect()
}

#[test]
fn status_queries_report_current_attributes_and_supported_margins() {
    let mut terminal = Terminal::new(80, 24, 0);
    terminal.feed(b"\x1b[1;2;3;4:3;53;5;7;8;9;38:2::1:2:3;48;5;123m");
    assert_eq!(
        writes(&mut terminal, b"\x1bP$qm\x1b\\"),
        [b"\x1bP1$r0;1;2;3;4:3;53;5;7;8;9;38:2::1:2:3;48:5:123m\x1b\\".to_vec()]
    );
    assert_eq!(
        writes(&mut terminal, b"\x1bP$qs\x1b\\"),
        [b"\x1bP0$r\x1b\\".to_vec()]
    );
    terminal.feed(b"\x1b[?69h\x1b[3;60s\x1b[2;10r");
    assert_eq!(
        writes(&mut terminal, b"\x1bP$qs\x1b\\\x1bP$qr\x1b\\"),
        [
            b"\x1bP1$r3;60s\x1b\\".to_vec(),
            b"\x1bP1$r2;10r\x1b\\".to_vec()
        ]
    );
    assert_eq!(
        writes(&mut terminal, b"\x1bP$q\"q\x1b\\\x1bP$q?\x1b\\"),
        [b"\x1bP0$r\x1b\\".to_vec(), b"\x1bP0$r\x1b\\".to_vec()]
    );
    assert!(writes(&mut terminal, b"\x1bP$qtoolong\x1b\\").is_empty());
}

#[test]
fn capability_queries_handle_names_values_and_unknown_keys_independently() {
    let mut terminal = Terminal::new(80, 24, 0);
    let result = writes(
        &mut terminal,
        b"\x1bP+q436f;5463;536d756c78;6B63757531;invalid;544E\x1b\\",
    );
    assert_eq!(
        result,
        [
            b"\x1bP1+r436F=323536\x1b\\".to_vec(),
            b"\x1bP1+r5463\x1b\\".to_vec(),
            b"\x1bP1+r536D756C78=5C455B343A25703125646D\x1b\\".to_vec(),
            b"\x1bP1+r6B63757531=1B4F41\x1b\\".to_vec(),
        ]
    );
    terminal.terminfo_name = Some("xterm-256color".into());
    terminal.reset();
    assert_eq!(
        writes(&mut terminal, b"\x1bP+q544e\x1b\\"),
        [b"\x1bP1+r544E=787465726D2D323536636F6C6F72\x1b\\".to_vec()]
    );
    for name in [String::new(), "x".repeat(129)] {
        terminal.terminfo_name = Some(name.into_bytes());
        assert!(writes(&mut terminal, b"\x1bP+q544E\x1b\\").is_empty());
    }
    let query = format!("\x1bP+q{}436F\x1b\\", "00;".repeat(3000));
    assert_eq!(
        writes(&mut terminal, query.as_bytes()),
        [b"\x1bP1+r436F=323536\x1b\\".to_vec()]
    );
}
