use rustty_vt::{
    CellSpan, Terminal,
    formatter::Format,
    snapshot::{self, DecodeOptions, Decoder},
};

fn span(terminal: &mut Terminal, width: u8, text: &str) {
    terminal.feed(format!("\x1b]66;w={width};{text}\x1b\\").as_bytes());
}

fn text(terminal: &Terminal) -> Vec<u8> {
    let mut formatter = terminal.screen().formatter(Format::Plain);
    formatter.options.unwrap = true;
    formatter.format().unwrap()
}

fn assert_span(terminal: &Terminal, row: usize, col: usize, width: u8, text: &str) {
    let row = terminal.screen().row(row);
    assert_eq!(row.text(col).as_str(), text);
    for x in col..col + usize::from(width) {
        assert_eq!(
            row.cell_span(x),
            CellSpan {
                start: col,
                width,
                explicit: true
            }
        );
        if x != col {
            assert!(row.text(x).is_empty());
        }
    }
}

#[test]
fn width_only_probes_and_normal_width_requests_are_honest() {
    let mut terminal = Terminal::new(24, 3, 0);
    terminal.feed(b"\x1b]66;w=2; \x1b\\");
    assert_eq!(terminal.screen().cursor.col, 2);
    assert_span(&terminal, 0, 0, 2, " ");
    terminal.feed(b"\x1b]66;s=2; \x1b\\\x1b]66;w=0:s=7;abc\x1b\\\x1b]66;;z\x1b\\");
    assert_eq!(terminal.screen().cursor.col, 7);
    assert!(!terminal.screen().row(0).cell_span(2).explicit);
    assert_eq!(
        terminal.screen().row_text(terminal.screen().row(0)),
        "  abcz"
    );
    terminal.feed(b"\x1b]66;s=7:w=3:n=2:d=3:h=2:v=1;scaled\x1b\\");
    assert_span(&terminal, 0, 7, 3, "scaled");
    assert_eq!(terminal.screen().cursor.row, 0);
}

#[test]
fn text_capture_is_atomic_bounded_and_safe_utf8() {
    let value = "e\u{301}".repeat(1365) + "x";
    assert_eq!(value.len(), 4096);
    let request = format!("\x1b]66;w=7;{value}\x1b\\");
    for cut in [0, 1, 8, 9, 100, request.len() - 1, request.len()] {
        let mut terminal = Terminal::new(16, 2, 0);
        terminal.feed(&request.as_bytes()[..cut]);
        // The parser dispatches OSC when ESC starts ST, before its final backslash.
        if cut < request.len() - 1 {
            assert_eq!(terminal.screen().cursor.col, 0);
        }
        terminal.feed(&request.as_bytes()[cut..]);
        assert_span(&terminal, 0, 0, 7, &value);
        assert_eq!(terminal.screen().cursor.col, 7);
    }
    for metadata in ["w=8", "w=-1", "w=2:s=0", "w=2:n=16", "w=2:h=3", "w=", "x=2"] {
        let mut terminal = Terminal::new(12, 2, 0);
        terminal.feed(format!("\x1b]66;{metadata};text\x1b\\").as_bytes());
        assert_eq!(terminal.screen().cursor.col, 0, "{metadata}");
    }
    for invalid in [
        Vec::new(),
        vec![b'a'; 4097],
        vec![0xff],
        "bad\u{85}text".as_bytes().to_vec(),
    ] {
        let mut terminal = Terminal::new(12, 2, 0);
        terminal.feed(b"\x1b]66;w=2;");
        terminal.feed(&invalid);
        terminal.feed(b"\x1b\\");
        assert_eq!(terminal.screen().cursor.col, 0, "{invalid:?}");
    }
    let mut terminal = Terminal::new(12, 2, 0);
    terminal.feed(b"\x1b]66;w=2;bad\x01text\x1b\\");
    assert_span(&terminal, 0, 0, 2, "badtext");

    struct NoEffects;
    impl rustty_vt::EffectHandler for NoEffects {
        fn effect(&mut self, effect: rustty_vt::Effect) {
            panic!("invalid text emitted {effect:?}");
        }
    }
    for control in (0..=0x1f).chain([0x7f]) {
        let mut terminal = Terminal::new(12, 2, 0);
        let mut payload = b"66;w=2;bad".to_vec();
        payload.push(control);
        payload.extend_from_slice(b"text");
        terminal.feed_osc_with_handler(&payload, None, &mut NoEffects);
        assert_eq!(terminal.screen().cursor.col, 0);
    }
}

#[test]
fn snapshot_during_an_osc_upload_keeps_the_text_pending_until_its_terminator() {
    let mut terminal = Terminal::new(12, 2, 0);
    terminal.feed(b"a\x1b]66;w=3;part");
    let bytes = snapshot::encode_to_vec(&terminal).unwrap();
    let mut restored = snapshot::decode(bytes.as_slice(), DecodeOptions::default()).unwrap();
    assert_eq!(restored.screen().cursor.col, 1);
    assert_eq!(text(&restored), b"a");
    terminal.feed(b"rest\x1b\\");
    restored.feed(b"rest\x1b\\");
    assert_span(&restored, 0, 1, 3, "partrest");
    assert_eq!(text(&restored), text(&terminal));
}

#[test]
fn combining_input_preserves_explicit_width_and_detached_text() {
    for width in 1..=7 {
        let mut terminal = Terminal::new(width.into(), 2, 0);
        span(&mut terminal, width, "a☺");
        let detached = terminal.screen().snapshot_viewport();
        let revision = terminal.screen().row(0).revision();
        terminal.feed("\u{301}\u{fe0f}\u{fe0e}".as_bytes());
        assert_span(&terminal, 0, 0, width, "a☺\u{301}\u{fe0f}\u{fe0e}");
        assert_eq!(terminal.screen().cursor.col, usize::from(width) - 1);
        assert!(terminal.screen().cursor.pending_wrap);
        assert_ne!(revision, terminal.screen().row(0).revision());
        assert_eq!(detached.row(0).text(0).as_str(), "a☺");
    }
    let mut terminal = Terminal::new(8, 2, 0);
    span(&mut terminal, 2, &"a".repeat(4096));
    terminal.feed("\u{301}".as_bytes());
    assert_eq!(terminal.screen().row(0).text(0).len(), 4096);
}

#[test]
fn overwriting_a_head_or_tail_removes_the_entire_span() {
    for (col, expected) in [(2, "AAX   ZZ"), (4, "AA  X ZZ")] {
        let mut terminal = Terminal::new(12, 2, 0);
        terminal.feed(b"AA");
        span(&mut terminal, 4, "four");
        terminal.feed(format!("ZZ\x1b[1;{}HX", col + 1).as_bytes());
        assert_eq!(
            terminal.screen().row_text(terminal.screen().row(0)),
            expected
        );
        assert!((0..12).all(|x| !terminal.screen().row(0).cell_span(x).explicit));
        terminal.feed(b"\r0123456789");
        assert_eq!(
            terminal.screen().row_text(terminal.screen().row(0)),
            "0123456789"
        );
    }
}

#[test]
fn overwriting_span_tails_preserves_the_current_erase_background() {
    for (sgr, expected) in [
        ("41", rustty_vt::Color::Indexed(1)),
        ("48;2;12;34;56", rustty_vt::Color::Rgb(12, 34, 56)),
    ] {
        let mut terminal = Terminal::new(8, 2, 0);
        terminal.feed(b"\x1b[44m");
        span(&mut terminal, 4, "source");
        terminal.feed(format!("\x1b[{sgr}m\x1b[1;3HX").as_bytes());
        let row = terminal.screen().row(0);
        for col in 0..4 {
            assert_eq!(row.style(col).background, expected, "col={col}");
            assert!(!row.cell_span(col).explicit);
        }
    }
}

#[test]
fn insert_delete_and_erase_keep_only_complete_spans() {
    let mut terminal = Terminal::new(12, 3, 0);
    terminal.feed(b"ab");
    span(&mut terminal, 3, "three");
    terminal.feed(b"c\r\x1b[2@");
    assert_span(&terminal, 0, 4, 3, "three");
    terminal.feed(b"\x1b[2P");
    assert_span(&terminal, 0, 2, 3, "three");
    terminal.feed(b"\x1b[1;4H\x1b[P");
    assert!((0..12).all(|x| !terminal.screen().row(0).cell_span(x).explicit));
    for operation in ["\x1b[@", "\x1b[X", "\x1b[K", "\x1b[1K", "\x1b[J"] {
        terminal.feed(b"\x1b[2J\x1b[H");
        span(&mut terminal, 7, "seven");
        terminal.feed(format!("\x1b[1;4H{operation}").as_bytes());
        assert!(
            (0..12).all(|x| !terminal.screen().row(0).cell_span(x).explicit),
            "{operation:?}"
        );
    }
    terminal.feed(b"\x1b[H\x1b[1\"q");
    span(&mut terminal, 7, "protected");
    terminal.feed(b"\x1b[0\"q\x1b[?2K");
    assert_span(&terminal, 0, 0, 7, "protected");
    terminal.feed(b"\x1b[2K");
    assert!(!terminal.screen().row(0).cell_span(0).explicit);
}

#[test]
fn wrapping_no_wrap_and_reflow_keep_spans_atomic_and_omit_padding() {
    let mut terminal = Terminal::new(5, 3, 100);
    terminal.feed(b"abc");
    span(&mut terminal, 3, "wide");
    terminal.feed(b"Z");
    assert!(
        terminal.screen().row(0).cells[3..]
            .iter()
            .all(|c| c.spacer_head())
    );
    assert_span(&terminal, 1, 0, 3, "wide");
    let expected = text(&terminal);
    assert_eq!(expected, b"abcwideZ");
    for width in [6, 9, 4, 5] {
        terminal.resize(width, 3);
        assert_eq!(text(&terminal), expected, "width={width}");
        assert_eq!(
            terminal
                .screen()
                .all_rows()
                .flat_map(|row| (0..row.cells.len())
                    .filter(move |&x| row.cell_span(x).explicit && row.cell_span(x).start == x))
                .count(),
            1
        );
    }
    terminal.resize(2, 3);
    assert!(
        !terminal
            .screen()
            .all_rows()
            .any(|row| (0..row.cells.len()).any(|x| row.cell_span(x).explicit))
    );
    let mut terminal = Terminal::new(5, 2, 0);
    terminal.feed(b"1234\x1b[?7l");
    span(&mut terminal, 3, "back");
    assert_span(&terminal, 0, 2, 3, "back");
    assert_eq!(terminal.screen().cursor.row, 0);
    span(&mut terminal, 6, "too wide");
    assert_span(&terminal, 0, 2, 3, "back");
}

#[test]
fn replacing_a_wrapped_span_retires_obsolete_padding() {
    let mut terminal = Terminal::new(5, 3, 0);
    terminal.feed(b"abc");
    span(&mut terminal, 3, "wide");
    terminal.feed(b"\x1b[2;1H");
    span(&mut terminal, 3, "same");
    assert!(
        terminal.screen().row(0).cells[3..]
            .iter()
            .all(|c| c.spacer_head())
    );
    terminal.feed(b"\x1b[2;1HX");
    assert!(
        terminal
            .screen()
            .row(0)
            .cells
            .iter()
            .all(|c| !c.spacer_head())
    );
    assert!(!terminal.screen().row(1).cell_span(0).explicit);
}

#[test]
fn history_row_moves_compression_and_snapshots_preserve_sources() {
    let mut terminal = Terminal::new(215, 4, 1000);
    terminal.set_scrollback_compression(true);
    for index in 0..300 {
        span(
            &mut terminal,
            5,
            &format!("command output {index} 界e\u{301}"),
        );
        terminal.feed(b"\r\n");
    }
    terminal.request_scrollback_compression();
    while terminal.compress_scrollback_step() {}
    let expected = text(&terminal);
    let bytes = snapshot::encode_to_vec(&terminal).unwrap();
    let restored = snapshot::decode(bytes.as_slice(), DecodeOptions::default()).unwrap();
    assert_eq!(text(&restored), expected);
    let json = serde_json::to_vec(terminal.screen()).unwrap();
    let mut restored = Terminal::new(215, 4, 1000);
    *restored.screen_mut() = serde_json::from_slice(&json).unwrap();
    assert_eq!(text(&restored), expected);
    assert_eq!(
        restored.screen().physical_row(0).text(0).as_str(),
        "command output 0 界e\u{301}"
    );
    terminal.feed(b"\x1b[H\x1b[L");
    assert_span(&terminal, 1, 0, 5, "command output 297 界e\u{301}");
    terminal.feed(b"\x1b[M");
    assert_span(&terminal, 0, 0, 5, "command output 297 界e\u{301}");
}

fn records(bytes: &[u8]) -> Vec<(u16, Vec<u8>)> {
    let mut data = &bytes[10..];
    let mut result = Vec::new();
    while !data.is_empty() {
        let tag = u16::from_le_bytes(data[..2].try_into().unwrap());
        let len = u32::from_le_bytes(data[2..6].try_into().unwrap()) as usize;
        result.push((tag, data[10..10 + len].to_vec()));
        data = &data[10 + len..];
    }
    result
}

fn frame(records: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut result = b"GHOSTSNP\x01\0".to_vec();
    for (tag, data) in records {
        let mut header = tag.to_le_bytes().to_vec();
        header.extend_from_slice(&(data.len() as u32).to_le_bytes());
        let crc = crc32c::crc32c_append(crc32c::crc32c(&header), data);
        result.extend_from_slice(&header);
        result.extend_from_slice(&crc.to_le_bytes());
        result.extend_from_slice(data);
    }
    result
}

#[test]
fn snapshot_metadata_is_optional_bounded_and_validated() {
    let mut terminal = Terminal::new(10, 2, 0);
    span(&mut terminal, 7, "exact source");
    let original = records(&snapshot::encode_to_vec(&terminal).unwrap());
    let index = original.iter().position(|(tag, _)| *tag == 0x8007).unwrap();
    let legacy = frame(
        &original
            .iter()
            .filter(|(tag, _)| *tag != 0x8007)
            .cloned()
            .collect::<Vec<_>>(),
    );
    let restored = snapshot::decode(legacy.as_slice(), DecodeOptions::default()).unwrap();
    assert_eq!(restored.screen().cursor.col, 7);
    assert!(!restored.screen().row(0).cell_span(0).explicit);
    assert_eq!(restored.screen().row(0).text(0).as_str(), "�");
    for (byte, value) in [
        (0, 2),
        (1, 2),
        (2, 100),
        (4, 100),
        (6, 0),
        (6, 8),
        (9, 0x1b),
    ] {
        let mut invalid = original.clone();
        invalid[index].1[byte] = value;
        assert!(
            snapshot::decode(frame(&invalid).as_slice(), DecodeOptions::default()).is_err(),
            "offset={byte}"
        );
    }
    let mut duplicate = original.clone();
    let entry = duplicate[index].1[2..].to_vec();
    duplicate[index].1.extend_from_slice(&entry);
    assert!(snapshot::decode(frame(&duplicate).as_slice(), DecodeOptions::default()).is_err());
    assert!(
        snapshot::decode(
            frame(&original).as_slice(),
            DecodeOptions {
                max_text_span_bytes: 3,
                ..DecodeOptions::default()
            }
        )
        .is_err()
    );
    let mut incomplete = original;
    incomplete[index].1[1] = 0;
    assert!(snapshot::decode(frame(&incomplete).as_slice(), DecodeOptions::default()).is_err());
}

#[test]
fn mixed_legacy_graphemes_and_span_metadata_keep_independent_snapshot_sources() {
    let mut terminal = Terminal::new(6, 2, 0);
    terminal.feed("a\u{301} b\u{301}".as_bytes());
    let mut value = serde_json::to_value(terminal.screen()).unwrap();
    value["rows"][0]["spans"] =
        serde_json::json!([{ "col":0, "width":3, "text":"x".repeat(4096) }]);
    *terminal.screen_mut() = serde_json::from_value(value).unwrap();
    let bytes = snapshot::encode_to_vec(&terminal).unwrap();
    let restored = snapshot::decode(bytes.as_slice(), DecodeOptions::default()).unwrap();
    assert_eq!(restored.screen().row(0).text(0).len(), 4096);
    let legacy = frame(
        &records(&bytes)
            .into_iter()
            .filter(|(tag, _)| *tag != 0x8007)
            .collect::<Vec<_>>(),
    );
    let restored = snapshot::decode(legacy.as_slice(), DecodeOptions::default()).unwrap();
    assert_eq!(restored.screen().row(0).text(0).as_str(), "a\u{301}");
    assert_eq!(restored.screen().row(0).text(2).as_str(), "b\u{301}");
}

#[test]
fn erasing_a_legacy_orphan_wide_head_stays_inside_its_physical_row() {
    for row in 0..2 {
        let mut terminal = Terminal::new(3, 2, 0);
        terminal.feed(b"abc\r\nxyz");
        let mut value = serde_json::to_value(terminal.screen()).unwrap();
        value["rows"][row]["cells"][2]["text"] = serde_json::json!("界");
        value["rows"][row]["cells"][2]["width"] = serde_json::json!(2);
        *terminal.screen_mut() = serde_json::from_value(value).unwrap();
        terminal.feed(format!("\x1b[{};1H\x1b[2K", row + 1).as_bytes());
        assert!(
            terminal
                .screen()
                .row_text(terminal.screen().row(row))
                .is_empty()
        );
        assert_eq!(
            terminal.screen().row_text(terminal.screen().row(1 - row)),
            if row == 0 { "xyz" } else { "abc" }
        );
    }
}

#[test]
fn large_span_pages_use_bounded_complete_snapshot_chunks() {
    let mut terminal = Terminal::new(260, 1, 0);
    for _ in 0..260 {
        span(&mut terminal, 1, &"x".repeat(4096));
    }
    let data = records(&snapshot::encode_to_vec(&terminal).unwrap());
    let chunks = data
        .iter()
        .filter(|(tag, _)| *tag == 0x8007)
        .collect::<Vec<_>>();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].1[1], 0);
    assert_eq!(chunks[1].1[1], 1);
    assert!(chunks.iter().all(|(_, data)| data.len() <= 1024 * 1024));
    let restored = snapshot::decode(frame(&data).as_slice(), DecodeOptions::default()).unwrap();
    assert_eq!(restored.screen().row(0).text(259).len(), 4096);
    let mut missing = data;
    let last = missing.iter().rposition(|(tag, _)| *tag == 0x8007).unwrap();
    missing.remove(last);
    assert!(snapshot::decode(frame(&missing).as_slice(), DecodeOptions::default()).is_err());
}

#[test]
fn skipped_history_still_validates_span_sources() {
    let mut terminal = Terminal::new(215, 2, 1000);
    for _ in 0..300 {
        span(&mut terminal, 3, "safe");
        terminal.feed(b"\r\n");
    }
    let mut parts = records(&snapshot::encode_to_vec(&terminal).unwrap());
    let ready = parts.iter().position(|(tag, _)| *tag == 5).unwrap();
    let index = (ready + 1..parts.len())
        .find(|&i| parts[i].0 == 0x8007)
        .unwrap();
    parts[index].1[9] = 0x1b;
    let bytes = frame(&parts);
    let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
    let mut restored = decoder.ready().unwrap();
    restored.resize(216, 2);
    assert!(decoder.next_history(&mut restored).is_err());
}
