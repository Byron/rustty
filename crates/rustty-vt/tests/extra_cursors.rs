use rustty_vt::{
    Effect, Screen, Terminal,
    extra_cursors::{Color, Colors},
    formatter::{Content, Format, ScreenExtra},
    snapshot::{self, DecodeOptions},
};

fn command(terminal: &mut Terminal, params: &str) -> Vec<Effect> {
    terminal.feed(format!("\x1b[>{params} q").as_bytes())
}

fn shape(screen: &Screen, y: usize, x: usize) -> u8 {
    screen.extra_cursors().row(y).map_or(0, |row| row.shapes[x])
}

fn overlay(screen: &Screen) -> (Colors, Vec<u8>) {
    (
        screen.extra_cursors().colors(),
        (0..screen.height())
            .flat_map(|y| (0..screen.columns).map(move |x| shape(screen, y, x)))
            .collect(),
    )
}

fn replies(effects: Vec<Effect>) -> Vec<Vec<u8>> {
    effects
        .into_iter()
        .map(|effect| match effect {
            Effect::Write(bytes) => bytes,
            other => panic!("unexpected effect {other:?}"),
        })
        .collect()
}

#[test]
fn queries_are_single_fifo_responses_and_do_not_reflect_responses() {
    let mut terminal = Terminal::new(8, 3, 0);
    let responses = replies(terminal.feed(b"\x1b[> q\x1b[>100 q\x1b[>101 q\x1b[c"));
    assert_eq!(responses.len(), 4);
    assert_eq!(responses[0], b"\x1b[>1;2;3;29;30;40;100;101 q");
    assert_eq!(responses[1], b"\x1b[>100 q");
    assert_eq!(responses[2], b"\x1b[>101;30:0;40:0 q");
    assert!(responses[3].starts_with(b"\x1b[?"));
    command(&mut terminal, "1;4:1:2:1:5");
    command(&mut terminal, "29;2:3:8");
    let responses = replies(command(&mut terminal, "100"));
    assert_eq!(responses, [b"\x1b[>100;1:4:1:2:1:5;29:2:3:8 q".to_vec()]);
    let before = overlay(terminal.screen());
    assert!(terminal.feed(&responses[0]).is_empty());
    assert!(terminal.feed(b"\x1b[>101;30:2:1:2:3;40:5:6 q").is_empty());
    assert_eq!(overlay(terminal.screen()), before);
}

#[test]
fn points_rectangles_and_incomplete_groups_follow_screen_coordinates() {
    let mut terminal = Terminal::new(8, 4, 0);
    terminal.feed(b"\x1b[2;4H");
    command(&mut terminal, "29;0");
    assert_eq!(shape(terminal.screen(), 1, 3), 29);
    command(&mut terminal, "1;2:1:2:1:4:3;4:3:2:4:4:99");
    assert_eq!(terminal.screen().extra_cursors().count(), 9);
    assert_eq!(shape(terminal.screen(), 0, 1), 1);
    assert_eq!(shape(terminal.screen(), 0, 3), 1);
    assert_eq!(shape(terminal.screen(), 2, 2), 1);
    assert_eq!(shape(terminal.screen(), 3, 3), 1);
    command(&mut terminal, "2;2:3:3;2:4:4");
    assert_eq!(terminal.screen().extra_cursors().count(), 9);
    assert_eq!(shape(terminal.screen(), 2, 2), 2);
    command(&mut terminal, "0;2:3:3");
    assert_eq!(terminal.screen().extra_cursors().count(), 8);
    let before = overlay(terminal.screen());
    for invalid in ["0;4:", "0;4:1", "0;4:1:1", "0;4:1:1:4", "1:2:1:1", "99;4"] {
        command(&mut terminal, invalid);
        assert_eq!(overlay(terminal.screen()), before, "{invalid}");
    }
    assert_eq!(
        (terminal.screen().cursor.row, terminal.screen().cursor.col),
        (1, 3)
    );
    command(&mut terminal, "3;4");
    assert_eq!(terminal.screen().extra_cursors().count(), 32);
    command(&mut terminal, "0;4");
    assert!(!terminal.screen().has_extra_cursors());
    assert!(terminal.screen().extra_cursors().row(0).is_none());
}

#[test]
fn coordinates_do_not_saturate_and_rectangles_clip_instead_of_aliasing() {
    let mut terminal = Terminal::new(u16::MAX, 1, 0);
    command(&mut terminal, "1;2:1:65535");
    let before = overlay(terminal.screen());
    for invalid in [
        "0;2:1:65536",
        "2;2:1:65536",
        "3;2:65536:1",
        "3;2:0:1",
        "3;2:1:0",
        "3;2:1:4294967296",
    ] {
        command(&mut terminal, invalid);
        assert_eq!(overlay(terminal.screen()), before, "{invalid}");
    }
    command(&mut terminal, "2;4:0:65534:65536:65536");
    assert_eq!(terminal.screen().extra_cursors().count(), 2);
    assert_eq!(shape(terminal.screen(), 0, 65533), 2);
    assert_eq!(shape(terminal.screen(), 0, 65534), 2);
    command(&mut terminal, "0;4:2:1:1:65535;4:1:10:1:5");
    assert_eq!(terminal.screen().extra_cursors().count(), 2);
}

#[test]
fn extended_parameter_lists_are_bounded_and_atomic() {
    let mut params = String::from("1;2");
    for x in 1..=127 {
        params.push_str(&format!(":1:{x}"));
    }
    let mut terminal = Terminal::new(160, 2, 0);
    // Shape + type + 127 pairs exactly fills the 256-parameter bound.
    command(&mut terminal, &params);
    assert_eq!(terminal.screen().extra_cursors().count(), 127);
    command(&mut terminal, "0;4");
    command(&mut terminal, &(params.clone() + ":1"));
    assert!(!terminal.screen().has_extra_cursors());
    let wire = format!("\x1b[>{params} q");
    for cut in [1, 25, wire.len() / 2, wire.len() - 1] {
        let mut source = Terminal::new(160, 2, 0);
        source.feed(&wire.as_bytes()[..cut]);
        let bytes = snapshot::encode_to_vec(&source).unwrap();
        let mut restored = snapshot::decode(bytes.as_slice(), DecodeOptions::default()).unwrap();
        source.feed(&wire.as_bytes()[cut..]);
        restored.feed(&wire.as_bytes()[cut..]);
        assert_eq!(overlay(restored.screen()), overlay(source.screen()));
        assert_eq!(restored.screen().extra_cursors().count(), 127);
    }
}

#[test]
fn colors_validate_components_and_preserve_query_types() {
    let mut terminal = Terminal::new(8, 3, 0);
    command(&mut terminal, "40;2:10:20:30");
    command(&mut terminal, "30;5:255");
    assert_eq!(
        terminal.screen().extra_cursors().colors(),
        Colors {
            cursor: Color::Rgb(10, 20, 30),
            text: Color::Indexed(255)
        }
    );
    assert!(!terminal.screen().has_extra_cursors());
    assert_eq!(
        replies(command(&mut terminal, "101")),
        [b"\x1b[>101;30:5:255;40:2:10:20:30 q".to_vec()]
    );
    let before = terminal.screen().extra_cursors().colors();
    for invalid in [
        "40;2:1:2",
        "40;2:256:1:2",
        "30;5:256",
        "30;5:",
        "40;7:1:2:3",
        "40:2:1:2:3",
    ] {
        command(&mut terminal, invalid);
        assert_eq!(
            terminal.screen().extra_cursors().colors(),
            before,
            "{invalid}"
        );
    }
    command(&mut terminal, "40;1");
    command(&mut terminal, "30;1");
    assert_eq!(
        terminal.screen().extra_cursors().colors(),
        Colors {
            cursor: Color::Special,
            text: Color::Special
        }
    );
    command(&mut terminal, "40;0");
    command(&mut terminal, "30;0");
    assert_eq!(
        terminal.screen().extra_cursors().colors(),
        Colors::default()
    );
}

#[test]
fn overlays_are_viewport_fixed_and_resize_preserves_coordinate_intersection() {
    let mut terminal = Terminal::new(8, 4, 100);
    terminal.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
    command(&mut terminal, "1;2:1:2;2:3:7;2:4:8");
    let before = overlay(terminal.screen());
    terminal.feed(b"\x1b[4;1H\x1bD\x1bD\x1b[1;1H\x1bM\x1b[L\x1b[M\x1b[?25l");
    assert_eq!(overlay(terminal.screen()), before);
    terminal.screen_mut().viewport_offset = terminal.screen().history_len();
    assert_eq!(overlay(&terminal.screen().snapshot_viewport()), before);
    terminal.resize(7, 3);
    assert_eq!(terminal.screen().extra_cursors().count(), 2);
    assert_eq!(shape(terminal.screen(), 0, 1), 1);
    assert_eq!(shape(terminal.screen(), 2, 6), 1);
    terminal.resize(10, 6);
    assert_eq!(terminal.screen().extra_cursors().count(), 2);
    assert_eq!(shape(terminal.screen(), 2, 6), 1);
    assert!(terminal.screen().extra_cursors().row(5).is_none());
}

#[test]
fn erases_screen_switches_and_resets_have_distinct_color_lifetimes() {
    let mut terminal = Terminal::new(8, 3, 10);
    command(&mut terminal, "40;5:12");
    for mode in [0, 1] {
        command(&mut terminal, "1;4");
        terminal.feed(format!("\x1b[{mode}J").as_bytes());
        assert!(terminal.screen().has_extra_cursors());
    }
    for mode in [2, 3, 22] {
        command(&mut terminal, "1;4");
        terminal.feed(format!("\x1b[{mode}J").as_bytes());
        assert!(!terminal.screen().has_extra_cursors());
        assert_eq!(
            terminal.screen().extra_cursors().colors().cursor,
            Color::Indexed(12)
        );
    }
    for mode in [47, 1047, 1049] {
        command(&mut terminal, "1;4");
        terminal.feed(format!("\x1b[?{mode}h").as_bytes());
        assert!(!terminal.screen().has_extra_cursors());
        assert_eq!(
            terminal.screen().extra_cursors().colors().cursor,
            Color::Indexed(12)
        );
        command(&mut terminal, "2;4");
        command(&mut terminal, "30;1");
        terminal.feed(format!("\x1b[?{mode}l").as_bytes());
        assert!(!terminal.screen().has_extra_cursors());
        assert_eq!(
            terminal.screen().extra_cursors().colors(),
            Colors {
                cursor: Color::Indexed(12),
                text: Color::Special
            }
        );
    }
    for reset in [b"\x1b[!p".as_slice(), b"\x1bc".as_slice()] {
        command(&mut terminal, "1;4");
        command(&mut terminal, "40;1");
        terminal.feed(reset);
        assert!(!terminal.screen().has_extra_cursors());
        assert_eq!(
            terminal.screen().extra_cursors().colors(),
            Colors::default()
        );
    }
}

#[test]
fn revisions_are_local_and_detached_views_keep_previous_overlays() {
    let mut terminal = Terminal::new(8, 3, 0);
    command(&mut terminal, "1;2:1:1;2:3:3");
    let detached = terminal.screen().snapshot_viewport();
    let old = overlay(&detached);
    let key = terminal.screen().render_key();
    let revision = terminal.screen().extra_cursors().row(2).unwrap().revision;
    let generation = terminal.generation;
    command(&mut terminal, "1;2:1:1");
    assert_eq!(terminal.generation, generation);
    assert_eq!(terminal.screen().render_key(), key);
    command(&mut terminal, "2;2:1:2");
    let new_key = terminal.screen().render_key();
    assert_ne!(new_key.extra_cursors_revision, key.extra_cursors_revision);
    assert_eq!(
        terminal.screen().extra_cursors().row(2).unwrap().revision,
        revision
    );
    assert_eq!(overlay(&detached), old);
    command(&mut terminal, "40;5:4");
    assert_eq!(
        terminal.screen().extra_cursors().row(2).unwrap().revision,
        revision
    );
    assert_eq!(detached.extra_cursors().colors(), Colors::default());
    command(&mut terminal, "0;4");
    assert_eq!(overlay(&detached), old);
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
fn binary_and_json_snapshots_preserve_overlays_without_serializing_revisions() {
    let mut terminal = Terminal::new(8, 3, 0);
    command(&mut terminal, "29;4:1:2:3:6");
    command(&mut terminal, "40;2:15:20:25");
    command(&mut terminal, "30;1");
    let expected = overlay(terminal.screen());
    let binary = snapshot::encode_to_vec(&terminal).unwrap();
    let restored = snapshot::decode(binary.as_slice(), DecodeOptions::default()).unwrap();
    assert_eq!(overlay(restored.screen()), expected);
    assert_ne!(
        restored.screen().extra_cursors().revision(),
        terminal.screen().extra_cursors().revision()
    );
    let json = serde_json::to_value(terminal.screen()).unwrap();
    assert!(json["extra_cursors"].get("revision").is_none());
    let restored: Screen = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(overlay(&restored), expected);
    for (field, value) in [
        ("columns", serde_json::json!(4)),
        ("shapes", serde_json::json!([1, 2, 3, 29])),
    ] {
        let mut invalid = json.clone();
        invalid["extra_cursors"][field] = value;
        assert!(serde_json::from_value::<Screen>(invalid).is_err());
    }
    let mut invalid = json.clone();
    invalid["extra_cursors"]["shapes"][0] = serde_json::json!(4);
    assert!(serde_json::from_value::<Screen>(invalid).is_err());
    let mut old = json;
    old.as_object_mut().unwrap().remove("extra_cursors");
    assert!(
        !serde_json::from_value::<Screen>(old)
            .unwrap()
            .has_extra_cursors()
    );
    command(&mut terminal, "0;4");
    let bytes = snapshot::encode_to_vec(&terminal).unwrap();
    let restored = snapshot::decode(bytes.as_slice(), DecodeOptions::default()).unwrap();
    assert_eq!(overlay(restored.screen()), overlay(terminal.screen()));
}

#[test]
fn optional_snapshot_records_validate_shapes_colors_lengths_order_and_crc() {
    let mut terminal = Terminal::new(8, 3, 0);
    command(&mut terminal, "1;4");
    let original = records(&snapshot::encode_to_vec(&terminal).unwrap());
    let index = original.iter().position(|(tag, _)| *tag == 0x8008).unwrap();
    let mut legacy = original.clone();
    legacy.remove(index);
    assert!(
        !snapshot::decode(frame(&legacy).as_slice(), DecodeOptions::default())
            .unwrap()
            .screen()
            .has_extra_cursors()
    );
    for (byte, value) in [
        (0, 2),
        (1, 1),
        (2, 9),
        (3, 1),
        (6, 4),
        (9, 1),
        (10, 23),
        (14, 4),
        (15, 100),
    ] {
        let mut invalid = original.clone();
        invalid[index].1[byte] = value;
        assert!(
            snapshot::decode(frame(&invalid).as_slice(), DecodeOptions::default()).is_err(),
            "byte={byte},value={value}"
        );
    }
    for remove in [true, false] {
        let mut invalid = original.clone();
        if remove {
            invalid[index].1.pop();
        } else {
            invalid[index].1.push(0);
        }
        assert!(snapshot::decode(frame(&invalid).as_slice(), DecodeOptions::default()).is_err());
    }
    let mut invalid = original.clone();
    invalid.insert(index + 1, original[index].clone());
    assert!(snapshot::decode(frame(&invalid).as_slice(), DecodeOptions::default()).is_err());
    let mut invalid = original.clone();
    let entry = invalid.remove(index);
    invalid.insert(0, entry);
    assert!(snapshot::decode(frame(&invalid).as_slice(), DecodeOptions::default()).is_err());
    let mut unknown = original.clone();
    unknown[index].0 = 0x8088;
    assert!(
        !snapshot::decode(frame(&unknown).as_slice(), DecodeOptions::default())
            .unwrap()
            .screen()
            .has_extra_cursors()
    );
    let mut corrupt = frame(&unknown);
    let start = 10
        + unknown[..index]
            .iter()
            .map(|(_, p)| 10 + p.len())
            .sum::<usize>();
    corrupt[start + 10] ^= 1;
    assert!(snapshot::decode(corrupt.as_slice(), DecodeOptions::default()).is_err());
    let mut excessive = original;
    excessive[index].1.resize(24 + 15, 0);
    assert!(
        snapshot::decode(
            frame(&excessive).as_slice(),
            DecodeOptions {
                max_cells: 24,
                ..DecodeOptions::default()
            }
        )
        .is_err()
    );
}

#[test]
fn vt_cursor_exports_replay_nondefault_overlays_with_short_commands() {
    let mut source = Terminal::new(40, 4, 0);
    for x in (1..=40).step_by(2) {
        command(&mut source, &format!("29;2:2:{x}"));
    }
    command(&mut source, "40;5:20");
    command(&mut source, "30;2:1:2:3");
    let mut formatter = source.screen().formatter(Format::Vt);
    formatter.content = Content::None;
    formatter.extra = ScreenExtra {
        cursor: true,
        ..ScreenExtra::NONE
    };
    let bytes = formatter.format().unwrap();
    let mut target = Terminal::new(40, 4, 0);
    command(&mut target, "1;4");
    target.feed(&bytes);
    assert_eq!(overlay(target.screen()), overlay(source.screen()));
    assert_eq!(target.screen().cursor.col, source.screen().cursor.col);
    // Cursor extras remain byte-compatible for screens that never use overlays.
    let source = Terminal::new(8, 3, 0);
    let mut formatter = source.screen().formatter(Format::Vt);
    formatter.content = Content::None;
    formatter.extra = ScreenExtra {
        cursor: true,
        ..ScreenExtra::NONE
    };
    assert!(
        !formatter
            .format()
            .unwrap()
            .windows(3)
            .any(|bytes| bytes == b"\x1b[>")
    );
}
