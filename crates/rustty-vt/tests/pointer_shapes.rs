use rustty_vt::{Effect, Screen, Terminal, snapshot};

fn command(terminal: &mut Terminal, value: &str) {
    assert!(
        terminal
            .feed(format!("\x1b]22;{value}\x1b\\").as_bytes())
            .is_empty()
    );
}

fn current(terminal: &mut Terminal, expected: &str) {
    assert_eq!(
        terminal.feed(b"\x1b]22;?__current__\x1b\\"),
        [Effect::Write(
            format!("\x1b]22;{expected}\x1b\\").into_bytes()
        )]
    );
}

#[test]
fn pointer_queries_support_current_defaults_and_both_terminators_at_every_split() {
    for terminator in ["\x07", "\x1b\\"] {
        let input = format!(
            "\x1b]22;>pointer,wait{terminator}\x1b]22;?__current__,__default__,__grabbed__,pointer,crosshair,no-such-name,wait{terminator}"
        );
        for split in 0..=input.len() {
            let mut terminal = Terminal::new(8, 3, 0);
            let mut effects = terminal.feed(&input.as_bytes()[..split]);
            effects.extend(terminal.feed(&input.as_bytes()[split..]));
            assert_eq!(
                effects,
                [Effect::Write(
                    format!("\x1b]22;wait,text,text,1,1,0,1{terminator}").into_bytes()
                )]
            );
            assert_eq!(terminal.mouse_shape(), "wait");
        }
    }
    let mut terminal = Terminal::new(8, 3, 0);
    current(&mut terminal, "0");
    terminal.feed(b"\x1b[?1003h");
    assert_eq!(terminal.mouse_shape(), "text");
    current(&mut terminal, "0");
    assert_eq!(
        terminal.feed(b"\x1b]22;?\x07"),
        [Effect::Write(b"\x1b]22;0\x07".to_vec())]
    );
}

#[test]
fn pointer_set_push_pop_unknown_and_default_entries_match_kitty() {
    let mut terminal = Terminal::new(8, 3, 0);
    for (request, expected) in [
        ("default", "default"),
        ("=crosshair", "crosshair"),
        ("<", "0"),
        ("=crosshair", "crosshair"),
        ("", "0"),
        (">help", "help"),
        (">wait", "wait"),
        ("<ignored,names", "help"),
        ("<", "0"),
        ("default,help", "help"),
        ("<", "0"),
        (">default,help", "help"),
        ("<", "default"),
        ("<", "0"),
        ("=left_ptr", "default"),
        (">wait", "wait"),
        ("=", "0"),
        ("<", "default"),
        (">,unknown,,help,invalid,", "help"),
        ("unknown", "help"),
        ("<", "default"),
        ("<", "0"),
        ("<", "0"),
    ] {
        command(&mut terminal, request);
        current(&mut terminal, expected);
    }
}

#[test]
fn pointer_stack_evicts_only_the_oldest_of_sixteen_entries() {
    let names = [
        "alias",
        "cell",
        "copy",
        "crosshair",
        "help",
        "move",
        "wait",
        "text",
        "pointer",
        "progress",
        "zoom-in",
        "zoom-out",
        "e-resize",
        "n-resize",
        "s-resize",
        "w-resize",
        "ne-resize",
    ];
    let mut terminal = Terminal::new(8, 3, 0);
    command(&mut terminal, &format!(">{}", names.join(",")));
    for name in names[1..].iter().rev() {
        current(&mut terminal, name);
        command(&mut terminal, "<");
    }
    current(&mut terminal, "0");
}

#[test]
fn pointer_stacks_belong_to_screens_and_reset_on_ris_and_decstr() {
    for tmux in [false, true] {
        for reset in [b"\x1bc".as_slice(), b"\x1b[!p"] {
            let mut terminal = Terminal::new(8, 3, 0);
            terminal.tmux_graphics = tmux;
            command(&mut terminal, ">pointer,help");
            terminal.feed(b"\x1b[?1049h");
            current(&mut terminal, "0");
            command(&mut terminal, ">wait,crosshair");
            terminal.feed(b"\x1b[?1049l");
            current(&mut terminal, "help");
            command(&mut terminal, "<");
            current(&mut terminal, "pointer");
            terminal.feed(b"\x1b[?1049h");
            current(&mut terminal, "crosshair");
            terminal.feed(reset);
            current(&mut terminal, "0");
            terminal.feed(b"\x1b[?1049l");
            current(&mut terminal, "0");
            terminal.feed(b"\x1b[?1049h");
            current(&mut terminal, "0");
        }
    }
}

#[test]
fn pointer_changes_keep_history_selection_cursor_and_detached_view_state() {
    let mut terminal = Terminal::new(8, 3, 100);
    terminal.feed(b"one\r\ntwo\r\nthree\r\nfour");
    let start = terminal.screen().point(0, 0).unwrap();
    let end = terminal.screen().point(1, 2).unwrap();
    terminal.screen_mut().selection = Some(rustty_vt::Selection {
        start,
        end,
        rectangular: false,
    });
    terminal.screen_mut().scroll_viewport(1);
    let selection = terminal.screen().selection;
    let text = terminal.screen().selection_text();
    let history = terminal.screen().history_len();
    let cursor = terminal.screen().cursor.clone();
    let generation = terminal.generation;
    command(&mut terminal, ">pointer,wait");
    current(&mut terminal, "wait");
    assert_eq!(terminal.screen().selection, selection);
    assert_eq!(terminal.screen().selection_text(), text);
    assert_eq!(terminal.screen().history_len(), history);
    assert_eq!(terminal.screen().viewport_offset, 1);
    assert_eq!(terminal.screen().cursor, cursor);
    assert_eq!(terminal.generation, generation);
    let viewport = terminal.screen().snapshot_viewport();
    let value = serde_json::to_value(&viewport).unwrap();
    assert_eq!(value["pointer_shapes"], serde_json::json!([3, 5]));
    let restored: Screen = serde_json::from_value(value).unwrap();
    assert_eq!(
        serde_json::to_value(restored).unwrap()["pointer_shapes"],
        serde_json::json!([3, 5])
    );
}

#[test]
fn pointer_snapshot_preserves_both_stacks_and_pending_queries() {
    let mut terminal = Terminal::new(8, 3, 100);
    terminal.feed(b"history\r\n1\r\n2\r\n3\r\n4");
    command(&mut terminal, ">pointer,help");
    terminal.feed(b"\x1b[?1049h");
    command(&mut terminal, ">wait,crosshair");
    terminal.feed(b"\x1b]22;?__curr");
    let bytes = snapshot::encode_to_vec(&terminal).unwrap();
    let mut decoder = snapshot::Decoder::new(bytes.as_slice(), Default::default());
    let mut restored = decoder.ready().unwrap();
    assert_eq!(
        restored.feed(b"ent__\x1b\\"),
        [Effect::Write(b"\x1b]22;crosshair\x1b\\".to_vec())]
    );
    while decoder.next_history(&mut restored).unwrap().is_some() {}
    command(&mut restored, "<");
    current(&mut restored, "wait");
    restored.feed(b"\x1b[?1049l");
    current(&mut restored, "help");
    command(&mut restored, "<");
    current(&mut restored, "pointer");
    assert!(restored.screen().history_len() > 0);
}

fn rewrite_records(bytes: &[u8], mut rewrite: impl FnMut(u16, &mut Vec<u8>) -> bool) -> Vec<u8> {
    let mut output = bytes[..10].to_vec();
    let mut rest = &bytes[10..];
    while !rest.is_empty() {
        let tag = u16::from_le_bytes(rest[..2].try_into().unwrap());
        let len = u32::from_le_bytes(rest[2..6].try_into().unwrap()) as usize;
        let mut payload = rest[10..10 + len].to_vec();
        rest = &rest[10 + len..];
        if !rewrite(tag, &mut payload) {
            continue;
        }
        let mut header = tag.to_le_bytes().to_vec();
        header.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        let crc = crc32c::crc32c_append(crc32c::crc32c(&header), &payload);
        output.extend(header);
        output.extend(crc.to_le_bytes());
        output.extend(payload);
    }
    output
}

#[test]
fn pointer_snapshot_accepts_legacy_headers_and_rejects_malformed_extensions() {
    let mut terminal = Terminal::new(8, 3, 0);
    command(&mut terminal, ">pointer,help");
    terminal.feed(b"\x1b[?1049h");
    command(&mut terminal, ">wait,crosshair");
    let bytes = snapshot::encode_to_vec(&terminal).unwrap();
    let legacy = rewrite_records(&bytes, |tag, _| tag != 0x8001);
    let mut restored = snapshot::decode(legacy.as_slice(), Default::default()).unwrap();
    current(&mut restored, "crosshair");
    restored.feed(b"\x1b[?1049l");
    current(&mut restored, "crosshair");

    for bad in [vec![], vec![2, 0], vec![1, 2], vec![1, 0, 34], vec![1; 19]] {
        let corrupt = rewrite_records(&bytes, |tag, payload| {
            if tag == 0x8001 {
                *payload = bad.clone();
            }
            true
        });
        assert!(snapshot::decode(corrupt.as_slice(), Default::default()).is_err());
    }
    for bad in [serde_json::json!([34]), serde_json::json!(vec![0; 17])] {
        let mut screen = serde_json::to_value(terminal.screen()).unwrap();
        screen["pointer_shapes"] = bad;
        assert!(serde_json::from_value::<Screen>(screen).is_err());
    }
}
