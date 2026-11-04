use rustty_vt::{Color, Effect, Selection, Style, Terminal, Underline};

fn styles(terminal: &Terminal) -> Vec<Vec<Style>> {
    terminal
        .screen()
        .rows()
        .map(|row| {
            (0..usize::from(terminal.cols))
                .map(|x| row.style(x))
                .collect()
        })
        .collect()
}

#[test]
fn extended_region_rendition_accepts_fragmented_colon_groups() {
    let input = b"\x1b[;;;;4:3;38:5:10;48:2:1:2:3;1$r";
    for split in 0..=input.len() {
        let mut t = Terminal::new(5, 4, 100);
        assert!(t.feed(&input[..split]).is_empty());
        assert!(t.feed(&input[split..]).is_empty());
        let expected = Style {
            bold: true,
            underline: Underline::Curly,
            foreground: Color::Indexed(10),
            background: Color::Rgb(1, 2, 3),
            ..Style::default()
        };
        assert!(styles(&t).iter().flatten().all(|&style| style == expected));
        assert_eq!(t.screen().cursor.style, Style::default());
        assert_eq!(t.screen().history_len(), 0);
    }
}

#[test]
fn stream_and_rectangular_extents_preserve_unaddressed_attributes() {
    let mut t = Terminal::new(5, 4, 100);
    t.feed(b"\x1b[;;;;1;31$r\x1b[1;2;2;3;221;39$r");
    for y in 0..4 {
        for x in 0..5 {
            let addressed = (y == 0 && x >= 1) || (y == 1 && x < 3);
            let style = t.screen().row(y).style(x);
            assert_eq!(style.bold, !addressed);
            assert_eq!(
                style.foreground,
                if addressed {
                    Color::Default
                } else {
                    Color::Indexed(1)
                }
            );
        }
    }
    t.feed(b"\x1b[2*x\x1b[3;2;4;3;44;2$r\x1b[*x");
    for y in 0..4 {
        for x in 0..5 {
            let addressed = y >= 2 && (1..3).contains(&x);
            let style = t.screen().row(y).style(x);
            assert_eq!(style.faint, addressed);
            assert_eq!(
                style.background,
                if addressed {
                    Color::Indexed(4)
                } else {
                    Color::Default
                }
            );
        }
    }
    t.feed(b"\x1b[$r");
    assert!(
        styles(&t)
            .iter()
            .flatten()
            .all(|&style| style == Style::default())
    );
}

#[test]
fn coordinates_default_clip_and_follow_origin_without_moving_the_cursor() {
    let mut t = Terminal::new(6, 5, 100);
    t.feed(b"\x1b[2;4r\x1b[?69h\x1b[2;5s\x1b[?6h\x1b[2*x");
    let cursor = t.screen().cursor.clone();
    t.feed(b"\x1b[1;1;2;2;7$r");
    for y in 0..5 {
        for x in 0..6 {
            assert_eq!(
                t.screen().row(y).style(x).inverse,
                (1..3).contains(&y) && (1..3).contains(&x)
            );
        }
    }
    assert_eq!(t.screen().cursor, cursor);
    t.feed(b"\x1b[?6l\x1b[*x\x1b[1;2;99;1;3$r");
    for y in 0..5 {
        for x in 0..6 {
            assert_eq!(t.screen().row(y).style(x).italic, y != 0 || x >= 1);
        }
    }
    let before = styles(&t);
    for input in [
        b"\x1b[99;1;99;5;22$r".as_slice(),
        b"\x1b[4;1;2;5;22$r",
        b"\x1b[3;4;3;2;22$r",
        b"\x1b[2;2;4;4$r",
        b"\x1b[2:2;4;4;22$r",
        b"\x1b[1;1;1;1:22$r",
    ] {
        t.feed(input);
        assert_eq!(styles(&t), before, "{input:?}");
    }
}

#[test]
fn styling_preserves_wide_text_links_protection_selection_and_pen() {
    let mut t = Terminal::new(6, 3, 100);
    t.feed(
        "\x1b[?2027h\x1b[1\"q\x1b]8;id=x;https://example.org\x07界\u{301}ab\x1b]8;;\x07\x1b[31m"
            .as_bytes(),
    );
    let start = t.screen().point(0, 0).unwrap();
    let end = t.screen().point(0, 3).unwrap();
    t.screen_mut().selection = Some(Selection {
        start,
        end,
        rectangular: false,
    });
    let selection = t.screen().selection;
    let text = t.screen().selection_text();
    let cursor = t.screen().cursor.clone();
    let row_id = t.screen().row(0).id;
    let before = t.screen().snapshot_viewport();
    t.feed(b"\x1b[2*x\x1b[1;2;1;4;48;2;7;8;9;1;2;221$r");
    assert_eq!(t.screen().cursor, cursor);
    assert_eq!(t.screen().row(0).id, row_id);
    assert_eq!(t.screen().selection, selection);
    assert_eq!(t.screen().selection_text(), text);
    for x in 0..6 {
        let row = t.screen().row(0);
        let original = before.row(0);
        assert_eq!(&*row.text(x), &*original.text(x));
        assert_eq!(row.cells[x].width(), original.cells[x].width());
        assert_eq!(row.cells[x].protected(), original.cells[x].protected());
        assert_eq!(row.hyperlink(x), original.hyperlink(x));
        if (1..4).contains(&x) {
            assert_eq!(row.style(x).background, Color::Rgb(7, 8, 9));
            assert!(row.style(x).faint);
            assert!(!row.style(x).bold);
        }
    }
    let snapshot = rustty_vt::snapshot::encode_to_vec(&t).unwrap();
    let restored = rustty_vt::snapshot::decode(snapshot.as_slice(), Default::default()).unwrap();
    assert_eq!(styles(&restored), styles(&t));
}

#[test]
fn extent_survives_screen_switch_and_snapshot_and_resets_with_ris() {
    for tmux in [false, true] {
        let mut t = Terminal::new(5, 3, 100);
        t.tmux_graphics = tmux;
        t.feed(b"\x1b[2*x\x1b[?1049h");
        let bytes = rustty_vt::snapshot::encode_to_vec(&t).unwrap();
        let mut restored =
            rustty_vt::snapshot::decode(bytes.as_slice(), Default::default()).unwrap();
        restored.tmux_graphics = tmux;
        for terminal in [&mut t, &mut restored] {
            terminal.feed(b"\x1b[1;2;2;3;1$r\x1b[?1049l\x1b[1;2;2;3;1$r");
            assert!(!terminal.screen().row(0).style(4).bold);
            assert!(terminal.screen().row(1).style(1).bold);
            terminal.feed(b"\x1bc\x1b[1;2;2;3;1$r");
            assert!(terminal.screen().row(0).style(4).bold);
            assert!(terminal.screen().row(1).style(0).bold);
        }
    }
}

#[test]
fn extent_snapshot_defaults_old_records_and_rejects_invalid_extensions() {
    let mut t = Terminal::new(5, 3, 100);
    t.feed(b"\x1b[2*x");
    let bytes = rustty_vt::snapshot::encode_to_vec(&t).unwrap();
    let mut offset = 10;
    while u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) != 0x8003 {
        offset +=
            10 + u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().unwrap()) as usize;
    }
    let end = offset + 12;
    let old = [&bytes[..offset], &bytes[end..]].concat();
    let mut restored = rustty_vt::snapshot::decode(old.as_slice(), Default::default()).unwrap();
    restored.feed(b"\x1b[1;2;2;3;1$r");
    assert!(restored.screen().row(0).style(4).bold);
    for payload in [b"".as_slice(), &[1, 1], &[2, 2], &[1, 2, 0]] {
        let mut header = vec![3, 128];
        header.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        let crc = crc32c::crc32c_append(crc32c::crc32c(&header), payload);
        header.extend_from_slice(&crc.to_le_bytes());
        let invalid = [&bytes[..offset], &header, payload, &bytes[end..]].concat();
        assert!(rustty_vt::snapshot::decode(invalid.as_slice(), Default::default()).is_err());
    }
}

#[test]
fn soft_reset_restores_stream_extent_without_erasing_text() {
    let mut t = Terminal::new(5, 3, 100);
    t.feed(b"text\x1b[2*x\x1b[!p\x1b[1;2;2;3;1$r");
    assert_eq!(t.screen().row_text(t.screen().row(0)), "text");
    assert!(t.screen().row(0).style(4).bold);
    assert!(t.screen().row(1).style(0).bold);
}

#[test]
fn region_reset_preserves_retained_tmux_tabs_and_unknown_csi_colons_stay_ignored() {
    let mut t = Terminal::new(12, 3, 100);
    t.set_tmux_strings(true);
    t.tmux_graphics = true;
    t.feed(b"\t\x1b[1;1H");
    let tabs: Vec<_> = (0..12)
        .map(|x| t.screen().row(0).style(x).tmux_tab)
        .collect();
    assert!(tabs.iter().any(|&tab| tab != 0));
    t.feed(b"\x1b[;;;;1;0$r");
    assert_eq!(
        (0..12)
            .map(|x| t.screen().row(0).style(x).tmux_tab)
            .collect::<Vec<_>>(),
        tabs
    );
    assert!(t.feed(b"\x1b[2:3H").is_empty());
    assert_eq!((t.screen().cursor.row, t.screen().cursor.col), (0, 0));
    assert_eq!(
        t.feed(b"\x1b[c"),
        vec![Effect::Write(b"\x1b[?62;22c".to_vec())]
    );
}
