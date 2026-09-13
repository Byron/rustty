use rustty_vt::snapshot::{DecodeOptions, Decoder, decode, decode_exact, encode_to_vec};
use rustty_vt::{
    Color, CursorShape, HyperlinkId, Row, Screen, ScrollbackLimits, SemanticContent, Terminal,
    default_palette,
};
use std::io::{Cursor, Read};

fn fixture() -> Vec<u8> {
    hex(include_str!(
        "../../../src/terminal/snapshot/testdata/complete-v1.hex"
    ))
}

fn hex(text: &str) -> Vec<u8> {
    text.lines()
        .flat_map(|line| line.split('#').next().unwrap().split_whitespace())
        .map(|pair| u8::from_str_radix(pair, 16).unwrap())
        .collect()
}

#[test]
fn snapshot_grapheme_capacity_drops_whole_clusters_and_limits_suffix_length() {
    for (capacity, counts, expected) in [
        (0u32, [1u16; 4], [1usize; 4]),
        (16, [1; 4], [2, 1, 1, 1]),
        (512, [64, 64, 8, 61], [65, 65, 9, 1]),
        (8192, [65, 128, 0, 1], [65, 65, 1, 2]),
    ] {
        let mut parts = records(&encode_to_vec(&Terminal::new(4, 1, 0)).unwrap());
        let page = &mut parts.iter_mut().find(|(tag, _)| *tag == 3).unwrap().1;
        page.clear();
        for value in [4u16, 1, 0, 0, 0, 0] {
            page.extend_from_slice(&value.to_le_bytes());
        }
        page.extend_from_slice(&capacity.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes());
        page.extend_from_slice(&[0, 4, 0, b'A', b'B', b'C', b'D']);
        page.extend_from_slice(&4u32.to_le_bytes());
        for (col, count) in counts.into_iter().enumerate() {
            for value in [0, col as u16, count] {
                page.extend_from_slice(&value.to_le_bytes());
            }
            for _ in 0..count {
                page.extend_from_slice(&0x301u32.to_le_bytes());
            }
        }
        let terminal = decode(frame(&parts).as_slice(), DecodeOptions::default()).unwrap();
        let actual = terminal.screen().rows[0]
            .cells
            .iter()
            .map(|cell| cell.text.chars().count())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "capacity={capacity}");
    }
}

fn records(bytes: &[u8]) -> Vec<(u16, Vec<u8>)> {
    let mut data = &bytes[10..];
    let mut records = Vec::new();
    while !data.is_empty() {
        let tag = u16::from_le_bytes(data[..2].try_into().unwrap());
        let len = u32::from_le_bytes(data[2..6].try_into().unwrap()) as usize;
        records.push((tag, data[10..10 + len].to_vec()));
        data = &data[10 + len..];
    }
    records
}

fn frame(records: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = b"GHOSTSNP\x01\0".to_vec();
    for (tag, payload) in records {
        let mut header = tag.to_le_bytes().to_vec();
        header.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        let crc = crc32c::crc32c_append(crc32c::crc32c(&header), payload);
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&crc.to_le_bytes());
        bytes.extend_from_slice(payload);
    }
    bytes
}

#[test]
fn unknown_cursor_default_flags_preserve_host_cursor_preferences() {
    for (flag, expected) in [
        (0, CursorShape::Block),
        (1, CursorShape::Bar),
        (2, CursorShape::Bar),
        (255, CursorShape::Bar),
    ] {
        let mut parts = records(&encode_to_vec(&Terminal::new(8, 3, 0)).unwrap());
        parts[0].1[29] = flag;
        let mut terminal = decode(frame(&parts).as_slice(), DecodeOptions::default()).unwrap();
        terminal.set_default_cursor(CursorShape::Bar, Some(true));
        assert_eq!(terminal.screen().cursor.shape, expected, "flag={flag}");
        let encoded = records(&encode_to_vec(&terminal).unwrap());
        assert_eq!(encoded[0].1[29], u8::from(flag != 0));
    }
}

#[test]
fn repeated_nul_prints_empty_cells_with_the_current_pen() {
    for restored in [false, true] {
        let mut terminal = Terminal::new(8, 3, 0);
        terminal.feed(b"abc\x1b[31m\x1b]8;;https://example.org\x07");
        if restored {
            let mut parts = records(&encode_to_vec(&terminal).unwrap());
            parts[0].1[25..29].copy_from_slice(&0u32.to_le_bytes());
            terminal = decode(frame(&parts).as_slice(), DecodeOptions::default()).unwrap();
            terminal.feed(b"\x1b[3b");
        } else {
            terminal.print('\0');
            terminal.feed(b"\x1b[2b");
        }
        assert_eq!(terminal.screen().cursor.col, 6, "restored={restored}");
        for cell in &terminal.screen().rows[0].cells[3..6] {
            assert!(cell.text.is_empty(), "restored={restored}");
            assert_eq!(cell.width, 1);
            assert_eq!(cell.style.foreground, Color::Indexed(1));
            assert_eq!(cell.hyperlink.as_deref(), Some("https://example.org"));
        }
    }
}

#[test]
fn snapshot_propagates_io_failures_at_record_boundaries() {
    use std::io::{Error, ErrorKind, Write};

    struct FailingReader<'a> {
        source: &'a [u8],
        remaining: usize,
    }
    impl Read for FailingReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(Error::from(ErrorKind::ConnectionReset));
            }
            let count = buffer.len().min(self.remaining).min(self.source.len());
            buffer[..count].copy_from_slice(&self.source[..count]);
            self.source = &self.source[count..];
            self.remaining -= count;
            Ok(count)
        }
    }
    struct FailingWriter {
        bytes: Vec<u8>,
        remaining: usize,
    }
    impl Write for FailingWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(Error::from(ErrorKind::BrokenPipe));
            }
            let count = buffer.len().min(self.remaining);
            self.bytes.extend_from_slice(&buffer[..count]);
            self.remaining -= count;
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let terminal = decode(fixture().as_slice(), DecodeOptions::default()).unwrap();
    let bytes = encode_to_vec(&terminal).unwrap();
    let mut cuts = vec![0, 1, 9, 10];
    let mut offset = 10;
    for (_, payload) in records(&bytes) {
        cuts.extend([
            offset,
            offset + 1,
            offset + 6,
            offset + 9,
            offset + 10,
            offset + 10 + payload.len() / 2,
            offset + 10 + payload.len(),
        ]);
        offset += 10 + payload.len();
    }
    cuts.sort_unstable();
    cuts.dedup();
    for cut in cuts.into_iter().filter(|&cut| cut < bytes.len()) {
        let reader = FailingReader {
            source: &bytes,
            remaining: cut,
        };
        assert_eq!(
            decode(reader, DecodeOptions::default()).unwrap_err().kind(),
            ErrorKind::ConnectionReset,
            "read cut={cut}"
        );
        let mut writer = FailingWriter {
            bytes: Vec::new(),
            remaining: cut,
        };
        assert_eq!(
            rustty_vt::snapshot::encode(&terminal, &mut writer)
                .unwrap_err()
                .kind(),
            ErrorKind::BrokenPipe,
            "write cut={cut}"
        );
        assert_eq!(writer.bytes, bytes[..cut], "write cut={cut}");
    }
}

#[test]
fn decode_continuation_budget_does_not_limit_later_capture() {
    for prefix in [b"".as_slice(), b"\x1b"] {
        let mut original = Terminal::new(8, 3, 0);
        original.feed(prefix);
        let encoded = encode_to_vec(&original).unwrap();
        let options = DecodeOptions {
            max_continuation_bytes: prefix.len(),
            ..Default::default()
        };
        let mut terminal = decode(encoded.as_slice(), options).unwrap();
        terminal.feed(b"\x1b]2;longer title");
        let encoded = encode_to_vec(&terminal).unwrap();
        let mut resumed = decode(encoded.as_slice(), DecodeOptions::default()).unwrap();
        resumed.feed(b"\x07");
        assert_eq!(resumed.title, "longer title");
        assert!(decode(encoded.as_slice(), options).is_err());
    }
}

#[test]
fn raised_decode_budget_keeps_the_normal_capture_limit() {
    let mut parts = records(&encode_to_vec(&Terminal::new(8, 3, 0)).unwrap());
    let continuation = &mut parts.iter_mut().find(|(tag, _)| *tag == 7).unwrap().1;
    continuation.extend_from_slice(b"\x1bPq");
    continuation.resize(rustty_parser::MAX_OSC_BYTES + 1, b'x');
    let options = DecodeOptions {
        max_record_bytes: continuation.len(),
        max_continuation_bytes: continuation.len(),
        ..Default::default()
    };
    let encoded = frame(&parts);
    let mut terminal = decode(encoded.as_slice(), options).unwrap();
    assert!(matches!(encode_to_vec(&terminal), Err(error)
        if error.kind() == std::io::ErrorKind::InvalidData));
    terminal.feed(b"\x1b\\\x1b]2;recovered\x07");
    assert_eq!(terminal.title, "recovered");
    assert!(encode_to_vec(&terminal).is_ok());
}

fn text<'a>(rows: impl IntoIterator<Item = &'a Row>) -> Vec<String> {
    rows.into_iter().map(Row::text).collect()
}

fn same_screen(expected: &Screen, actual: &Screen) {
    assert_eq!(expected.cursor, actual.cursor);
    for (a, b) in expected
        .history
        .iter()
        .chain(&expected.rows)
        .zip(actual.history.iter().chain(&actual.rows))
    {
        assert_eq!(a.cells, b.cells);
        assert_eq!(a.wrapped, b.wrapped);
        assert_eq!(a.wrap_continuation, b.wrap_continuation);
        assert_eq!(a.semantic, b.semantic);
    }
    assert_eq!(expected.history.len(), actual.history.len());
    assert_eq!(expected.rows.len(), actual.rows.len());
}

fn same_terminal(expected: &Terminal, actual: &Terminal) {
    assert_eq!(expected.cols, actual.cols);
    assert_eq!(expected.rows, actual.rows);
    assert_eq!(expected.title, actual.title);
    assert_eq!(expected.working_directory, actual.working_directory);
    assert_eq!(expected.palette, actual.palette);
    assert_eq!(expected.foreground, actual.foreground);
    assert_eq!(expected.background, actual.background);
    assert_eq!(expected.cursor_color, actual.cursor_color);
    assert_eq!(expected.margins, actual.margins);
    assert_eq!(expected.tabstops(), actual.tabstops());
    assert_eq!(expected.is_alternate_screen(), actual.is_alternate_screen());
    same_screen(expected.primary_screen(), actual.primary_screen());
    match (expected.alternate_screen(), actual.alternate_screen()) {
        (Some(a), Some(b)) => same_screen(a, b),
        (None, None) => {}
        _ => panic!("alternate screen presence differs"),
    }
}

#[test]
fn ghostty_fixture_streams_history_and_leaves_transport_unread() {
    let mut bytes = fixture();
    bytes.extend_from_slice(b"transport tail");
    let mut decoder = Decoder::new(Cursor::new(bytes), DecodeOptions::default());
    let mut terminal = decoder.ready().unwrap();
    assert_eq!((terminal.cols, terminal.rows), (2, 3));
    assert_eq!(terminal.title, "complete snapshot");
    assert_eq!(terminal.working_directory, "file:///tmp/snapshot");
    assert_eq!(terminal.palette[7], [1, 2, 3]);
    assert!(terminal.is_alternate_screen());
    assert_eq!(text(&terminal.primary_screen().rows), ["C", "D", "E"]);
    assert_eq!(text(&terminal.screen().rows), ["rn", "at", "e"]);
    assert_eq!(decoder.history_rows(), [4, 0]);
    assert!(terminal.primary_screen().history.is_empty());
    assert_eq!(
        decoder.next_history(&mut terminal).unwrap().unwrap().rows,
        2
    );
    assert_eq!(text(&terminal.primary_screen().history), ["B", ""]);
    assert_eq!(
        decoder.next_history(&mut terminal).unwrap().unwrap().rows,
        2
    );
    assert_eq!(text(&terminal.primary_screen().history), ["A", "", "B", ""]);
    assert!(decoder.next_history(&mut terminal).unwrap().is_none());
    let mut tail = String::new();
    decoder.into_inner().read_to_string(&mut tail).unwrap();
    assert_eq!(tail, "transport tail");
    let restored = decode(
        encode_to_vec(&terminal).unwrap().as_slice(),
        DecodeOptions::default(),
    )
    .unwrap();
    same_terminal(&terminal, &restored);
}

#[test]
fn snapshot_preserves_screen_attributes_and_pending_parser() {
    let streams: &[&[u8]] = &[
        b"abc\x1b[31;48;2;12;23;34mZ\x1b[0m\x1b[3;2Hq",
        "wide:界😀\r\n\x1b]8;id=hello;https://example.com\x1b\\link\x1b]8;;\x07".as_bytes(),
        b"\x1b(0lqk\x1b(B\x1b[?1049hhello\x1b[?1049l\x1b[5 q",
        b"\x1bP$qm\x1b\\\x1b]2;pending title\x07\x1b_Ga=q,i=1;AAAA\x1b\\",
        "a\u{301}😃\x1b[2;2H!".as_bytes(),
    ];
    for &stream in streams {
        for split in 0..=stream.len() {
            let mut expected = Terminal::new(14, 4, 100);
            expected.feed(&stream[..split]);
            let mut actual = decode(
                encode_to_vec(&expected).unwrap().as_slice(),
                DecodeOptions::default(),
            )
            .unwrap();
            assert_eq!(
                expected.feed(&stream[split..]),
                actual.feed(&stream[split..]),
                "split {split} of {stream:?}"
            );
            same_terminal(&expected, &actual);
        }
    }
}

#[test]
fn corrupt_truncated_and_excessive_snapshots_are_rejected() {
    let bytes = fixture();
    for length in 0..bytes.len() {
        assert!(
            decode(&bytes[..length], DecodeOptions::default()).is_err(),
            "length {length}"
        );
    }
    for offset in [0, 8, 16, 50, 1024, bytes.len() - 1] {
        let mut corrupt = bytes.clone();
        corrupt[offset] ^= 1;
        assert!(
            decode(corrupt.as_slice(), DecodeOptions::default()).is_err(),
            "offset {offset}"
        );
    }
    assert!(
        decode(
            bytes.as_slice(),
            DecodeOptions {
                max_record_bytes: 900,
                ..DecodeOptions::default()
            }
        )
        .is_err()
    );
    assert!(
        decode(
            bytes.as_slice(),
            DecodeOptions {
                max_cells: 5,
                ..DecodeOptions::default()
            }
        )
        .is_err()
    );
    let mut decoder = Decoder::new(&bytes[..30], DecodeOptions::default());
    assert!(decoder.ready().is_err());
    assert!(decoder.ready().is_err());
}

#[test]
fn exact_snapshot_decode_checks_eof_without_consuming_transport_bytes() {
    let bytes = fixture();
    assert!(decode_exact(bytes.as_slice(), DecodeOptions::default()).is_ok());
    let mut source = Cursor::new([bytes.as_slice(), b"tail"].concat());
    let error = decode_exact(&mut source, DecodeOptions::default()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(source.position(), bytes.len() as u64);
    let mut tail = String::new();
    source.read_to_string(&mut tail).unwrap();
    assert_eq!(tail, "tail");

    // Peek may fill the reader's buffer past FINISH, but must not consume
    // any exposed trailing bytes, including a complete following snapshot.
    for capacity in [1, 7, 1024, bytes.len() * 3] {
        let data = [bytes.as_slice(), bytes.as_slice()].concat();
        let mut source = std::io::BufReader::with_capacity(capacity, Cursor::new(data));
        let error = decode_exact(&mut source, DecodeOptions::default()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            std::io::Seek::stream_position(&mut source).unwrap(),
            bytes.len() as u64
        );
        // Decoding again begins at the second snapshot's magic, not one byte
        // after it. The same reader retains any bytes read ahead internally.
        assert!(decode_exact(&mut source, DecodeOptions::default()).is_ok());
    }

    struct BrokenTail;
    impl Read for BrokenTail {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::ConnectionReset.into())
        }
    }
    impl std::io::BufRead for BrokenTail {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            Err(std::io::ErrorKind::ConnectionReset.into())
        }
        fn consume(&mut self, _: usize) {}
    }
    let source = Cursor::new(bytes).chain(BrokenTail);
    let error = decode_exact(source, DecodeOptions::default()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
}

#[test]
fn streaming_restore_uses_the_minimum_scrollback_budget() {
    for (limits, expected) in [
        (
            ScrollbackLimits {
                bytes: None,
                lines: Some(0),
            },
            vec!["A", "", "B", ""],
        ),
        (
            ScrollbackLimits {
                bytes: Some(1),
                lines: None,
            },
            vec!["B", ""],
        ),
    ] {
        let bytes = fixture();
        let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
        let mut terminal = decoder.ready().unwrap();
        terminal.set_limits(limits);
        let mut rows = 0;
        while let Some(progress) = decoder.next_history(&mut terminal).unwrap() {
            rows += progress.rows;
        }
        assert_eq!(rows, expected.len());
        assert_eq!(text(&terminal.primary_screen().history), expected);
    }
}

#[test]
fn streaming_restore_obeys_page_budget_and_discards_history_after_resize() {
    for resize in [false, true] {
        let bytes = fixture();
        let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
        let mut terminal = decoder.ready().unwrap();
        if resize {
            terminal.resize(3, 3);
        } else {
            terminal.set_limits(ScrollbackLimits::NONE);
        }
        let mut rows = 0;
        while let Some(progress) = decoder.next_history(&mut terminal).unwrap() {
            rows += progress.rows;
        }
        let expected = if resize { vec![] } else { vec!["B", ""] };
        assert_eq!(rows, expected.len());
        assert_eq!(text(&terminal.primary_screen().history), expected);
    }
}

#[test]
fn streaming_history_checks_width_when_each_page_arrives() {
    for before in 0..=1 {
        for consume_while_narrow in [false, true] {
            let bytes = fixture();
            let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
            let mut terminal = decoder.ready().unwrap();
            for _ in 0..before {
                assert_eq!(
                    decoder.next_history(&mut terminal).unwrap().unwrap().rows,
                    2
                );
            }
            terminal.resize(3, 3);
            if consume_while_narrow {
                assert_eq!(
                    decoder.next_history(&mut terminal).unwrap().unwrap().rows,
                    0
                );
            }
            terminal.resize(2, 3);
            while let Some(progress) = decoder.next_history(&mut terminal).unwrap() {
                assert_eq!(progress.rows, if consume_while_narrow { 0 } else { 2 });
            }
            let expected = if !consume_while_narrow {
                vec!["A", "", "B", ""]
            } else if before == 1 {
                vec!["B", ""]
            } else {
                vec![]
            };
            assert_eq!(text(&terminal.primary_screen().history), expected);
        }
    }
}

#[test]
fn ghostty_sparse_page_preserves_styles_links_graphemes_and_wide_cells() {
    let mut stream = records(&encode_to_vec(&Terminal::new(3, 2, 100)).unwrap());
    stream[1].1[2..4].copy_from_slice(&1u16.to_le_bytes());
    stream[2].1 = hex(include_str!(
        "../../../src/terminal/snapshot/testdata/page-v1.hex"
    ));
    let terminal = decode(frame(&stream).as_slice(), DecodeOptions::default()).unwrap();
    let rows = &terminal.screen().rows;
    let first = &rows[0].cells[0];
    assert_eq!(first.text, "A");
    assert_eq!(first.width, 2);
    assert!(first.protected && first.style.bold);
    assert_eq!(first.semantic, SemanticContent::Prompt);
    assert_eq!(first.hyperlink.as_deref(), Some("alpha"));
    assert_eq!(
        first.hyperlink_id,
        Some(HyperlinkId::Explicit(b"a".to_vec()))
    );
    assert_eq!(rows[0].cells[1].width, 0);
    assert_eq!(rows[0].cells[1].style.background, Color::Indexed(42));
    assert_eq!(
        rows[0].cells[1].hyperlink_id,
        Some(HyperlinkId::Implicit(0x01020304))
    );
    assert_eq!(rows[0].cells[2].style.background, Color::Indexed(7));
    assert_eq!(rows[1].cells[0].text, "x\u{301}\u{302}");
    assert_eq!(
        rows[1].cells[1].style.background,
        Color::Rgb(0xaa, 0xbb, 0xcc)
    );
    assert!(rows[1].cells[2].spacer_head && rows[1].wrapped && rows[1].wrap_continuation);
    let restored = decode(
        encode_to_vec(&terminal).unwrap().as_slice(),
        DecodeOptions::default(),
    )
    .unwrap();
    same_terminal(&terminal, &restored);
}

#[test]
fn restored_defaults_survive_configuration_changes_and_protocol_resets() {
    let mut terminal = Terminal::new(5, 3, 10);
    let mut palette = default_palette();
    palette[7] = [44, 55, 66];
    terminal.set_default_colors(Some([1, 2, 3]), Some([4, 5, 6]), Some([7, 8, 9]), &palette);
    terminal.set_default_cursor(CursorShape::HollowBlock, Some(false));
    terminal.modes.set_default(true, 2027, true);
    terminal.feed(b"\x1b]10;#aabbcc\x07\x1b]4;7;#112233\x07\x1b[5 q\x1b[?2027l");
    let mut terminal = decode(
        encode_to_vec(&terminal).unwrap().as_slice(),
        DecodeOptions::default(),
    )
    .unwrap();
    terminal.set_default_cursor(CursorShape::Underline, Some(false));
    assert_eq!(terminal.screen().cursor.shape, CursorShape::Bar);
    assert!(terminal.screen().cursor.blink);
    terminal.set_default_colors(Some([10, 20, 30]), Some([4, 5, 6]), None, &palette);
    assert_eq!(terminal.foreground, [0xaa, 0xbb, 0xcc]);
    assert_eq!(terminal.palette[7], [0x11, 0x22, 0x33]);
    terminal.feed(b"\x1b]110\x07\x1b]104;7\x07\x1b[0 q");
    assert_eq!(terminal.foreground, [10, 20, 30]);
    assert_eq!(terminal.palette[7], [44, 55, 66]);
    assert_eq!(terminal.screen().cursor.shape, CursorShape::Underline);
    assert!(!terminal.screen().cursor.blink);
    terminal.feed(b"\x1b[2 q\x1bc");
    assert_eq!(terminal.screen().cursor.shape, CursorShape::Underline);
    assert!(!terminal.screen().cursor.blink);
    assert!(terminal.modes.dec(2027));
    assert_eq!(terminal.foreground, [10, 20, 30]);
}

#[test]
fn invalid_utf8_metadata_and_links_survive_without_loss() {
    let mut terminal = Terminal::new(5, 3, 10);
    terminal.set_title(b"title\xff");
    terminal.feed(b"\x1b]7;file:///\xfe\x07\x1b]8;id=\xff;https://x/\xfd\x07z");
    let encoded = encode_to_vec(&terminal).unwrap();
    let restored = decode(encoded.as_slice(), DecodeOptions::default()).unwrap();
    same_terminal(&terminal, &restored);
    assert_eq!(
        records(&encoded)[0],
        records(&encode_to_vec(&restored).unwrap())[0]
    );
    assert_eq!(
        restored.screen().cursor.hyperlink_raw.as_deref(),
        Some(b"https://x/\xfd".as_slice())
    );
}

#[test]
fn kitty_keyboard_ring_overflows_pops_and_resumes_after_restore() {
    let mut terminal = Terminal::new(5, 3, 10);
    for flag in 1..=10 {
        terminal.feed(format!("\x1b[>{flag}u").as_bytes());
    }
    let mut restored = decode(
        encode_to_vec(&terminal).unwrap().as_slice(),
        DecodeOptions::default(),
    )
    .unwrap();
    assert_eq!(
        terminal.screen().kitty_keyboard,
        restored.screen().kitty_keyboard
    );
    for (sequence, expected) in [
        (b"\x1b[<2u".as_slice(), 8),
        (b"\x1b[<1u", 7),
        (b"\x1b[=16;2u", 23),
        (b"\x1b[=3;3u", 20),
        (b"\x1b[<8u", 0),
        (b"\x1b[>5u", 5),
        (b"\x1b[<65535u", 0),
    ] {
        terminal.feed(sequence);
        restored.feed(sequence);
        assert_eq!(restored.screen().kitty_keyboard.current(), expected);
        assert_eq!(
            terminal.screen().kitty_keyboard,
            restored.screen().kitty_keyboard
        );
    }
}

#[test]
fn snapshot_clamps_cursor_to_logical_and_physical_widths() {
    for (physical, logical, x, expected, pending) in
        [(4, 8, 3, 3, true), (8, 4, 7, 3, true), (8, 4, 0, 0, false)]
    {
        let mut source = Terminal::new(physical, 1, 10);
        source.feed(&b"ABCDEFGH"[..usize::from(physical)]);
        let mut stream = records(&encode_to_vec(&source).unwrap());
        stream[0].1[..2].copy_from_slice(&u16::to_le_bytes(logical));
        stream[0].1[18..20].copy_from_slice(&u16::to_le_bytes(logical - 1));
        stream[1].1[12..14].copy_from_slice(&u16::to_le_bytes(x));
        let mut terminal = decode(frame(&stream).as_slice(), DecodeOptions::default()).unwrap();
        assert_eq!(terminal.screen().cursor.col, expected);
        assert_eq!(terminal.screen().cursor.pending_wrap, pending);
        assert_eq!(terminal.screen().rows[0].cells.len(), usize::from(physical));
        terminal.feed(b"\x1b[C");
        assert_eq!(
            terminal.screen().cursor.col,
            (expected + 1).min(usize::from(logical) - 1)
        );
        assert!(terminal.screen().cursor.col < terminal.screen().rows[0].cells.len());
    }
}

#[test]
fn mixed_physical_widths_survive_observation_and_grow_before_mutation() {
    let mut source = Terminal::new(4, 1, 10);
    source.feed(b"abcd");
    let narrow_page = records(&encode_to_vec(&source).unwrap())[2].clone();
    let blank_wide_page = records(&encode_to_vec(&Terminal::new(8, 1, 10)).unwrap())[2].clone();
    let mut logical = Terminal::new(8, 2, 10);
    logical.feed(b"\x1b[31");
    let mut stream = records(&encode_to_vec(&logical).unwrap());
    stream[2] = narrow_page;
    stream.insert(3, blank_wide_page);
    stream[1].1[2..4].copy_from_slice(&2u16.to_le_bytes());
    stream[1].1[12..14].copy_from_slice(&3u16.to_le_bytes());
    stream[1].1[17] = 1; // pending wrap at the physical edge, x=3
    let mut terminal = decode(frame(&stream).as_slice(), DecodeOptions::default()).unwrap();
    assert_eq!(terminal.screen().columns, 8);
    assert_eq!(terminal.screen().rows[0].cells.len(), 4);
    assert_eq!(terminal.screen().cursor.col, 3);
    assert!(terminal.screen().cursor.pending_wrap);
    let restored = decode(
        encode_to_vec(&terminal).unwrap().as_slice(),
        DecodeOptions::default(),
    )
    .unwrap();
    same_terminal(&terminal, &restored);
    terminal.feed(b"");
    assert_eq!(terminal.screen().rows[0].cells.len(), 4);
    terminal.feed(b"mX");
    assert_eq!(terminal.screen().rows[0].cells.len(), 8);
    assert_eq!(text(&terminal.screen().rows), ["abcd", "X"]);
    assert!(!terminal.screen().rows[0].wrapped);
    assert_eq!(
        terminal.screen().rows[1].cells[0].style.foreground,
        Color::Indexed(1)
    );

    let mut query = decode(frame(&stream).as_slice(), DecodeOptions::default()).unwrap();
    query.feed(b"m\x1b[6n\x1b[?7$p\x1bP$qm\x1b\\");
    assert_eq!(query.screen().rows[0].cells.len(), 4);
    assert_eq!(query.screen().cursor.col, 3);
    query.feed(b"\x1b[1;8H!");
    assert_eq!(query.screen().rows[0].cells.len(), 8);
    assert_eq!(query.screen().rows[0].cells[7].text, "!");
    assert_eq!(query.screen().page_allocations().next().unwrap().columns, 8);
    let restored = decode(
        encode_to_vec(&query).unwrap().as_slice(),
        DecodeOptions::default(),
    )
    .unwrap();
    same_terminal(&query, &restored);

    // Wider physical rows preserve their hidden suffix through safe edits.
    let mut source = Terminal::new(8, 1, 10);
    source.feed(b"abcdef");
    let wide_page = records(&encode_to_vec(&source).unwrap())[2].clone();
    let blank_narrow_page = records(&encode_to_vec(&Terminal::new(4, 1, 10)).unwrap())[2].clone();
    let mut stream = records(&encode_to_vec(&Terminal::new(4, 2, 10)).unwrap());
    stream[2] = wide_page;
    stream.insert(3, blank_narrow_page);
    stream[1].1[2..4].copy_from_slice(&2u16.to_le_bytes());
    let bytes = frame(&stream);
    for input in [
        b"X".as_slice(),
        b"\x1b[8;8H!",
        b"\x1b[?69h\x1b[2;3s\x1b[2S",
        b"\x1b[3J",
        b"\x1b[?1049hY\x1b[?1049lZ",
    ] {
        let mut terminal = decode(bytes.as_slice(), DecodeOptions::default()).unwrap();
        assert_eq!(terminal.screen().rows[0].text(), "abcdef");
        terminal.feed(input);
        assert!(terminal.screen().all_rows().all(|row| row.cells.len() >= 4));
        assert!(terminal.screen().cursor.col < 4);
        assert!(terminal.screen().cursor.row < 2);
        terminal.resize(5, 2);
        assert!(terminal.screen().all_rows().all(|row| row.cells.len() == 5));
    }
    let mut terminal = decode(bytes.as_slice(), DecodeOptions::default()).unwrap();
    terminal.feed(b"X");
    assert_eq!(terminal.screen().rows[0].cells.len(), 8);
    assert_eq!(terminal.screen().rows[0].text(), "Xbcdef");
    terminal.feed(b"\x1b[2J");
    assert_eq!(terminal.screen().rows[0].cells.len(), 8);
    assert_eq!(terminal.screen().rows[0].text(), "");
}

#[test]
fn narrow_restored_pages_support_direct_cursor_and_row_edits() {
    let mut stream = records(&encode_to_vec(&Terminal::new(8, 2, 10)).unwrap());
    let mut pages = Vec::new();
    for value in [b"ABCD", b"EFGH"] {
        let mut source = Terminal::new(4, 1, 10);
        source.feed(value);
        pages.push(records(&encode_to_vec(&source).unwrap())[2].clone());
    }
    stream.splice(2..3, pages);
    stream[1].1[2..4].copy_from_slice(&2u16.to_le_bytes());
    let bytes = frame(&stream);
    for (input, expected) in [
        (b"\x1b[8X".as_slice(), ["", "EFGH"]),
        (b"\x1b[2K", ["", "EFGH"]),
        (b"\x1b[L", ["", "ABCD"]),
        (b"\x1b[M", ["EFGH", ""]),
        (b"12345678Z", ["12345678", "ZFGH"]),
    ] {
        let mut terminal = decode(bytes.as_slice(), DecodeOptions::default()).unwrap();
        assert_eq!(terminal.screen().rows[0].cells.len(), 4);
        terminal.feed(input);
        assert_eq!(text(&terminal.screen().rows), expected);
        assert_eq!(terminal.screen().rows[0].cells.len(), 8);
        let restored = decode(
            encode_to_vec(&terminal).unwrap().as_slice(),
            DecodeOptions::default(),
        )
        .unwrap();
        same_terminal(&terminal, &restored);
    }
    let mut terminal = decode(bytes.as_slice(), DecodeOptions::default()).unwrap();
    terminal.feed(b"\x1b[1;7H\x1b[B");
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (1, 6)
    );
    assert!(
        terminal
            .screen()
            .rows
            .iter()
            .all(|row| row.cells.len() == 8)
    );
}

#[test]
fn multirow_page_snapshots_keep_all_owned_resources() {
    let mut styles = Terminal::new(64, 4, 10);
    for index in 0..256 {
        styles.feed(format!("\x1b[38;5;{index}mX").as_bytes());
    }
    let mut links = Terminal::new(80, 4, 10);
    for index in 0..40 {
        links.feed(
            format!(
                "\x1b]8;id={index};https://example.org/{index}/{}\x07XXXXX",
                "x".repeat(128)
            )
            .as_bytes(),
        );
    }
    let mut graphemes = Terminal::new(16, 4, 10);
    graphemes.feed(
        format!(
            "\x1b[?2027h{}",
            format!("A{}", "\u{301}".repeat(64)).repeat(64)
        )
        .as_bytes(),
    );
    for terminal in [styles, links, graphemes] {
        let restored = decode(
            encode_to_vec(&terminal).unwrap().as_slice(),
            DecodeOptions::default(),
        )
        .unwrap();
        same_terminal(&terminal, &restored);
    }
}

#[test]
fn snapshot_rejects_empty_link_strings_before_resource_admission() {
    for (uri, id) in [
        ("", HyperlinkId::Implicit(1)),
        ("https://example.org", HyperlinkId::Explicit(Vec::new())),
    ] {
        let mut terminal = Terminal::new(2, 1, 10);
        let cell = &mut terminal.screen_mut().rows[0].cells[0];
        cell.hyperlink = Some(uri.into());
        cell.hyperlink_id = Some(id);
        assert_eq!(
            encode_to_vec(&terminal).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn index_scroll_widens_mixed_pages_before_moving_rows() {
    for (widths, expected) in [
        (
            [8, 4, 8],
            ["AAAAAAAA", "CCCC", "DDDD", "EEEEEEEE", "FFFFFFFF", ""],
        ),
        (
            [4, 8, 4],
            ["AAAA", "CCCCCCCC", "DDDDDDDD", "EEEE", "FFFF", ""],
        ),
    ] {
        let mut stream = records(&encode_to_vec(&Terminal::new(8, 6, 10)).unwrap());
        let mut pages = Vec::new();
        for (page, columns) in widths.into_iter().enumerate() {
            let mut source = Terminal::new(columns, 2, 10);
            for (row, contents) in source.screen_mut().rows.iter_mut().enumerate() {
                let value = char::from(b'A' + (page * 2 + row) as u8);
                for cell in &mut contents.cells {
                    cell.text = value.to_string();
                }
                contents.wrapped = row == 0;
            }
            pages.push(records(&encode_to_vec(&source).unwrap())[2].clone());
        }
        stream.splice(2..3, pages);
        stream[1].1[2..4].copy_from_slice(&3u16.to_le_bytes());
        let mut terminal = decode(frame(&stream).as_slice(), DecodeOptions::default()).unwrap();
        let mut history = terminal.clone();
        terminal.set_limits(ScrollbackLimits::NONE);
        let wide_pin = (widths[1] == 8).then(|| {
            let point = terminal.screen().point(2, 6).unwrap();
            terminal.screen_mut().track(point)
        });
        terminal.feed(b"\x1b[2;6r\x1b[6;2H\x1bD");
        assert_eq!(text(&terminal.screen().rows), expected);
        assert_eq!(terminal.screen().rows[1].wrapped, widths[0] < widths[1]);
        assert_eq!(
            terminal
                .screen()
                .rows
                .iter()
                .map(|row| row.cells.len())
                .collect::<Vec<_>>(),
            [8; 6]
        );
        assert!(!terminal.screen().rows[5].wrapped);
        if let Some(pin) = wide_pin {
            // The formerly narrow destination can now represent the tracked
            // logical column without leaving the pin outside physical storage.
            let point = terminal.screen().resolve(pin).unwrap();
            assert_eq!(point.row, terminal.screen().rows[1].id);
            assert_eq!(point.col, 6);
            assert!(
                terminal
                    .screen()
                    .row_by_id(point.row)
                    .unwrap()
                    .cells
                    .get(point.col)
                    .is_some()
            );
        }

        let point = history.screen().point(3, 1).unwrap();
        let pin = history.screen_mut().track(point);
        history.feed(b"\x1b[1;3r\x1b[3;2H\x1bD");
        assert_eq!(
            text(&history.screen().rows),
            if widths[0] == 8 {
                ["BBBBBBBB", "CCCC", "", "DDDD", "EEEEEEEE", "FFFFFFFF"]
            } else {
                ["BBBB", "CCCCCCCC", "", "DDDDDDDD", "EEEE", "FFFF"]
            }
        );
        assert_eq!(history.screen().history.len(), 1);
        assert_eq!(history.screen().resolve(pin), history.screen().point(3, 1));
        history.feed(b"\x1b[2;6r\x1b[6;2H\x1bD");
        assert_eq!(
            text(&history.screen().rows),
            if widths[0] == 8 {
                ["BBBBBBBB", "", "DDDD", "EEEEEEEE", "FFFFFFFF", ""]
            } else {
                ["BBBB", "", "DDDDDDDD", "EEEE", "FFFF", ""]
            }
        );
        assert_eq!(
            history
                .screen()
                .rows
                .iter()
                .map(|row| row.cells.len())
                .collect::<Vec<_>>(),
            [8; 6]
        );
    }
}

#[test]
fn history_restore_preserves_mixed_physical_rows_while_live_input_arrives() {
    let mut stream = records(&fixture());
    let narrow_page = records(&encode_to_vec(&Terminal::new(1, 1, 10)).unwrap())[2].clone();
    let first_history_page = stream.iter().position(|(tag, _)| *tag == 4).unwrap() + 1;
    stream[first_history_page] = narrow_page;
    let bytes = frame(&stream);
    let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
    let mut terminal = decoder.ready().unwrap();
    assert_eq!(
        decoder.next_history(&mut terminal).unwrap().unwrap().rows,
        1
    );
    assert_eq!(terminal.primary_screen().history[0].cells.len(), 1);
    terminal.feed(b"X");
    assert_eq!(
        decoder.next_history(&mut terminal).unwrap().unwrap().rows,
        2
    );
    assert!(decoder.next_history(&mut terminal).unwrap().is_none());
    assert_eq!(
        terminal
            .primary_screen()
            .history
            .iter()
            .map(|row| row.cells.len())
            .collect::<Vec<_>>(),
        [2, 2, 1]
    );
}

#[test]
fn terminal_reset_keeps_the_primary_history_restore_destination() {
    for ready_pages in [0, 1] {
        let bytes = fixture();
        let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
        let mut terminal = decoder.ready().unwrap();
        if ready_pages == 1 {
            decoder.next_history(&mut terminal).unwrap();
        }
        terminal.feed(b"\x1bc");
        assert!(terminal.primary_screen().history.is_empty());
        while decoder.next_history(&mut terminal).unwrap().is_some() {}
        let expected = if ready_pages == 0 {
            vec!["A", "", "B", ""]
        } else {
            vec!["A", ""]
        };
        assert_eq!(text(&terminal.primary_screen().history), expected);
        assert!(terminal.alternate_screen().is_none());
    }
}
