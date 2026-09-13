use rustty_vt::snapshot::{DecodeOptions, Decoder, decode, encode_to_vec};
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
fn streaming_restore_discards_history_after_limits_or_resize_change() {
    for resize in [false, true] {
        let bytes = fixture();
        let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
        let mut terminal = decoder.ready().unwrap();
        if resize {
            terminal.resize(3, 3);
        } else {
            terminal.set_limits(ScrollbackLimits::NONE);
        }
        while let Some(progress) = decoder.next_history(&mut terminal).unwrap() {
            assert_eq!(progress.rows, 0);
        }
        assert!(terminal.primary_screen().history.is_empty());
    }
}

#[test]
fn ghostty_sparse_page_preserves_styles_links_graphemes_and_wide_cells() {
    let mut stream = records(&encode_to_vec(&Terminal::new(3, 2, 100)).unwrap());
    stream[1].1[2..4].copy_from_slice(&1u16.to_le_bytes());
    stream[2].1 = hex(include_str!(
        "../../../src/terminal/snapshot/testdata/page-v1.hex"
    ));
    stream.remove(3);
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
    terminal.set_default_colors([1, 2, 3], [4, 5, 6], Some([7, 8, 9]), &palette);
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
    terminal.set_default_colors([10, 20, 30], [4, 5, 6], None, &palette);
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
    terminal.feed(b"\x1b]2;title\xff\x07\x1b]7;file:///\xfe\x07\x1b]8;id=\xff;https://x/\xfd\x07z");
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
