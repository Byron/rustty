use rustty_vt::Terminal;
use std::collections::HashMap;

fn revisions(t: &Terminal) -> Vec<(u64, u64)> {
    t.screen()
        .viewport()
        .map(|row| (row.id, row.revision()))
        .collect()
}

#[test]
fn only_mutated_rows_change_revision_and_read_only_clones_keep_content_revisions() {
    let mut t = Terminal::new(12, 4, 20);
    t.feed(b"first\r\nsecond");
    let before = revisions(&t);
    assert_eq!(before, revisions(&t.clone()));
    for input in [
        b"\x1b[H".as_slice(),
        b"\x1b[6 q",
        b"\x1b[?25l",
        b"\x1b[?5h",
        b"\x1b]10;rgb:ff/00/00\x07",
        b"\x1b]4;1;rgb:00/ff/00\x07",
    ] {
        t.feed(input);
        assert_eq!(before, revisions(&t), "non-cell change {input:?}");
    }
    for input in [
        "\x1b[2;1Hnew",
        "\x1b[2;1H\x1b[31mstyled",
        "\x1b[2;1H漢e\u{301}",
        "\x1b[2;2Hx",
        "\x1b[2;1H\x1b]8;;https://example.com\x07link\x1b]8;;\x07",
        "\x1b[2;1H\x1b[K",
    ] {
        let previous = revisions(&t);
        t.feed(input.as_bytes());
        let after = revisions(&t);
        assert_ne!(previous[1], after[1], "{input}");
        for row in [0, 2, 3] {
            assert_eq!(previous[row], after[row], "{input}");
        }
    }
    let before = revisions(&t);
    t.screen_mut().set_cell_text(2, 0, "x");
    assert_ne!(before[2], revisions(&t)[2]);
}

#[test]
fn reset_decode_and_screen_switch_invalidate_reused_row_ids() {
    let mut t = Terminal::new(12, 4, 20);
    t.feed(b"\x1bcabc");
    let identity = t.screen().render_identity();
    let before = revisions(&t);
    let wire = rustty_vt::snapshot::encode_to_vec(&t).unwrap();
    let restored = rustty_vt::snapshot::decode(wire.as_slice(), Default::default()).unwrap();
    assert_ne!(identity, restored.screen().render_identity());
    t.feed(b"\x1bcxyz");
    assert_ne!(identity, t.screen().render_identity());
    assert_ne!(before, revisions(&t));
    let primary = t.screen().render_identity();
    t.feed(b"\x1b[?1049h");
    assert_ne!(primary, t.screen().render_identity());
    t.feed(b"\x1b[?1049l");
    assert_eq!(primary, t.screen().render_identity());
}

#[test]
fn cached_row_extraction_is_zero_when_unchanged_and_one_for_one_row_rewrite() {
    fn extract(t: &Terminal, cache: &mut HashMap<u64, (u64, String)>) -> usize {
        let mut count = 0;
        for row in t.screen().viewport() {
            if cache
                .get(&row.id)
                .is_some_and(|entry| entry.0 == row.revision())
            {
                continue;
            }
            count += row.cells.len();
            cache.insert(row.id, (row.revision(), t.screen().row_text(row)));
        }
        count
    }
    let mut t = Terminal::new(80, 24, 20);
    t.feed(b"first\r\nsecond");
    let mut cache = HashMap::new();
    assert_eq!(extract(&t, &mut cache), 1920);
    assert_eq!(extract(&t, &mut cache), 0);
    t.feed(b"\x1b[2;1Hrewrite");
    assert_eq!(extract(&t, &mut cache), 80);
    assert_eq!(extract(&t, &mut cache), 0);
}

#[test]
fn revisions_keep_cached_cells_correct_through_scrolling_reflow_and_screen_switches() {
    fn content(row: rustty_vt::Row<'_>) -> String {
        (0..row.cells.len())
            .map(|col| {
                format!(
                    "{:?}|{}|{:?}|{:?}",
                    row.cells[col],
                    row.text(col).as_str(),
                    row.style(col),
                    row.hyperlink(col)
                )
            })
            .collect()
    }
    let mut t = Terminal::new(12, 4, 20);
    let mut cache = HashMap::new();
    for input in [
        "\x1b[31mred 漢e\u{301}\x1b[0m\r\n",
        "\x1b]8;;https://example.com\x07linked\x1b]8;;\x07",
        "\r\nthird\r\nfourth\r\nfifth",
        "\x1b[1;1H\x1b[L",
        "\x1b[2;1H\x1b[M",
        "\x1b[?1049halternate",
        "\x1b[?1049l",
        "\x1b[H\x1b[2X",
        "\x1b[2;1H\x1b[@",
        "\x1b[2;1H\x1b[P",
        "\x1b#8",
    ] {
        t.feed(input.as_bytes());
        for columns in [12, 7, 16] {
            t.resize(columns, 4);
            for row in t.screen().viewport() {
                let key = (t.screen().render_identity(), row.id);
                let value = cache.entry(key).or_insert((u64::MAX, String::new()));
                if value.0 != row.revision() {
                    *value = (row.revision(), content(row));
                }
                assert_eq!(value.1, content(row), "{input:?}, width {columns}");
            }
        }
    }
}
