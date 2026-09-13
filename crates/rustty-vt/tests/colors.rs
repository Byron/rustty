use rustty_vt::{Effect, Terminal, color::Target, parse_color};

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
fn color_forms_share_native_precision_names_and_decimal_rules() {
    assert_eq!(parse_color(" Light Blue\t"), Some([173, 216, 230]));
    assert_eq!(parse_color("#fff000888"), Some([255, 0, 136]));
    assert_eq!(parse_color("rgb:f_f/0/8"), Some([15, 0, 136]));
    assert_eq!(parse_color("rgbi:0.3/+.5/-0"), Some([76, 127, 0]));
    assert_eq!(
        parse_color("rgbi:1.0000000000000001/0/0"),
        Some([255, 0, 0])
    );
    for invalid in [
        "\nred",
        "RGB:f/0/0",
        "123456789",
        "rgbi:1e-1/0/0",
        "rgbi:-.1/0/0",
    ] {
        assert_eq!(parse_color(invalid), None, "{invalid:?}");
    }
}

#[test]
fn unset_protocol_colors_remain_distinct_from_render_fallbacks() {
    let mut terminal = Terminal::new(80, 24, 0);
    assert!(writes(&mut terminal, b"\x1b]10;?;?;?\x07").is_empty());
    assert_eq!(
        writes(
            &mut terminal,
            b"\x1b]21;foreground=?;background=?;cursor=?\x07"
        ),
        [b"\x1b]21;foreground=;background=;cursor=\x07".to_vec()]
    );
    terminal.set_default_colors(Some([1, 2, 3]), None, None, &[]);
    assert_eq!(
        writes(
            &mut terminal,
            b"\x1b]10;?;?;?\x1b\\\x1b]21;foreground=?;cursor=?\x1b\\"
        ),
        [
            b"\x1b]10;rgb:0101/0202/0303\x1b\\\x1b]12;rgb:0101/0202/0303\x1b\\".to_vec(),
            b"\x1b]21;foreground=rgb:01/02/03;cursor=\x1b\\".to_vec(),
        ]
    );
    assert_eq!(terminal.color_current(Target::Cursor), None);
    assert_eq!(terminal.background, [0; 3]);
}

#[test]
fn explicit_colors_survive_default_changes_ris_and_snapshots_until_reset() {
    let mut terminal = Terminal::new(80, 24, 0);
    terminal.set_default_colors(Some([1, 2, 3]), None, None, &[[1, 2, 3]]);
    terminal.feed(b"\x1b]10;#010203\x07\x1b]4;0;#010203\x07");
    terminal.set_default_colors(Some([9, 8, 7]), None, None, &[[9, 8, 7]]);
    terminal.feed(b"\x1bc");
    let bytes = rustty_vt::snapshot::encode_to_vec(&terminal).unwrap();
    let mut terminal = rustty_vt::snapshot::decode(bytes.as_slice(), Default::default()).unwrap();
    for target in [Target::Foreground, Target::Palette(0)] {
        assert_eq!(terminal.color_default(target), Some([9, 8, 7]));
        assert_eq!(terminal.color_override(target), Some([1, 2, 3]));
        assert_eq!(terminal.color_current(target), Some([1, 2, 3]));
    }
    terminal.feed(b"\x1b]110;extra\x07");
    assert_eq!(terminal.color_override(Target::Foreground), Some([1, 2, 3]));
    terminal.feed(b"\x1b]110;;;\x07\x1b]104;bad\x07");
    for target in [Target::Foreground, Target::Palette(0)] {
        assert_eq!(terminal.color_current(target), Some([9, 8, 7]));
        assert_eq!(terminal.color_override(target), None);
    }
}

#[test]
fn color_lists_keep_valid_prefixes_but_discard_overflowed_requests() {
    let mut terminal = Terminal::new(80, 24, 0);
    assert_eq!(
        writes(&mut terminal, b"\x1b]4;;1;red;1;?;bad;blue;1;?\x07"),
        [b"\x1b]4;1;rgb:ffff/0000/0000\x07".to_vec()]
    );
    terminal.feed(b"\x1b]21;1=invalid;2=blue;selection_background=red\x07");
    assert_eq!(
        terminal.color_override(Target::Palette(2)),
        Some([0, 0, 255])
    );

    let payload = format!("#010203{}", " ".repeat(2048 - 7));
    terminal.feed(format!("\x1b]10;{payload} \x07").as_bytes());
    assert_eq!(terminal.color_current(Target::Foreground), None);
    terminal.feed(format!("\x1b]10;{payload}\x07").as_bytes());
    assert_eq!(terminal.color_current(Target::Foreground), Some([1, 2, 3]));

    let requests = ["1"; 526].join(";");
    terminal.feed(format!("\x1b]21;{requests};\x07").as_bytes());
    assert_eq!(
        terminal.color_override(Target::Palette(1)),
        Some([255, 0, 0])
    );
    terminal.feed(format!("\x1b]21;{requests}\x07").as_bytes());
    assert_eq!(terminal.color_override(Target::Palette(1)), None);
}
