use rustty_vt::{Color, Selection, Terminal};

fn terminal(tmux: bool) -> Terminal {
    let mut t = Terminal::new(6, 4, 100);
    if tmux {
        t.tmux_graphics = true;
        t.set_tmux_strings(true);
        t.set_tmux_history_limit(100);
    }
    t.feed(b"000000\r\n111111\r\n222222\r\n333333\r\n444444\r\n555555");
    assert_eq!(t.screen().history_len(), 2);
    t
}

fn rows(t: &Terminal) -> Vec<String> {
    t.screen()
        .rows()
        .map(|row| t.screen().row_text(row))
        .collect()
}

#[test]
fn unscroll_restores_history_with_stable_ids_and_pending_wrap_at_every_split() {
    let input = b"\x1b[2+T";
    for tmux in [false, true] {
        for split in 0..=input.len() {
            let mut t = terminal(tmux);
            let ids: Vec<_> = t.screen().all_rows().take(4).map(|row| row.id).collect();
            let cursor = t.screen().cursor.clone();
            t.feed(&input[..split]);
            t.feed(&input[split..]);
            assert_eq!(rows(&t), ["000000", "111111", "222222", "333333"]);
            assert_eq!(t.screen().history_len(), 0);
            assert_eq!(t.screen().rows().map(|row| row.id).collect::<Vec<_>>(), ids);
            assert_eq!(t.screen().cursor, cursor);
            assert!(t.screen().tmux_history.is_none_or(|h| h.scrolled == 0));
        }
    }
}

#[test]
fn unscroll_obeys_vertical_margins_and_discards_only_the_region_bottom() {
    for tmux in [false, true] {
        for (margins, expected) in [
            (
                b"\x1b[1;3r".as_slice(),
                ["111111", "222222", "333333", "555555"],
            ),
            (
                b"\x1b[2;3r".as_slice(),
                ["222222", "111111", "333333", "555555"],
            ),
        ] {
            let mut t = terminal(tmux);
            let removed = t.screen().row(2).id;
            let retained: Vec<_> = t
                .screen()
                .all_rows()
                .filter(|r| r.id != removed)
                .map(|r| r.id)
                .collect();
            t.feed(margins);
            t.feed(b"\x1b[+T");
            assert_eq!(rows(&t), expected);
            assert_eq!(t.screen().history_len(), 1);
            assert!(t.screen().row_by_id(removed).is_none());
            assert!(
                retained
                    .iter()
                    .all(|&id| t.screen().row_by_id(id).is_some())
            );
        }
    }
}

#[test]
fn horizontal_margins_restore_only_selected_cells_and_preserve_the_exterior() {
    for tmux in [false, true] {
        let mut t = terminal(tmux);
        let ids: Vec<_> = t.screen().rows().map(|r| r.id).collect();
        let incoming = t.screen().history().last().unwrap().id;
        t.screen_mut().selection = Some(Selection {
            start: rustty_vt::GridPoint {
                row: incoming,
                col: 2,
            },
            end: rustty_vt::GridPoint {
                row: incoming,
                col: 3,
            },
            rectangular: false,
        });
        t.feed(b"\x1b[2;3r\x1b[?69h\x1b[2;5s\x1b[+T");
        assert_eq!(rows(&t), ["222222", "311113", "433334", "555555"]);
        assert_eq!(t.screen().rows().map(|r| r.id).collect::<Vec<_>>(), ids);
        assert_eq!(t.screen().selection_text().as_deref(), Some("11"));
        assert_eq!(t.screen().selection.unwrap().start.row, ids[1]);
        assert_eq!(t.screen().history_len(), 1);
    }
}

#[test]
fn exhausted_history_and_alternate_screen_fill_with_the_current_background() {
    for tmux in [false, true] {
        let mut t = terminal(tmux);
        t.feed(b"\x1b[44m\x1b[3+T");
        assert_eq!(rows(&t), ["", "000000", "111111", "222222"]);
        assert_eq!(t.screen().row(0).style(0).background, Color::Indexed(4));
        t.feed(b"\x1b[?1049h\x1b[HABCDE\x1b[+T");
        assert!(t.is_alternate_screen());
        assert_eq!(rows(&t), ["", "ABCDE", "", ""]);
        assert_eq!(t.primary_screen().history_len(), 0);
    }
    let mut t = terminal(false);
    let history = t.screen().history_len();
    t.feed(b"\x1b[?1049h\x1b[999+T\x1b[?1049l");
    assert_eq!(t.screen().history_len(), history);
}

#[test]
fn invalid_counts_do_not_consume_history_and_large_counts_stop_at_one_screen() {
    let mut t = terminal(false);
    let before = rustty_vt::snapshot::encode_to_vec(&t).unwrap();
    for input in [b"\x1b[0+T".as_slice(), b"\x1b[1;2+T", b"\x1b[1:2+T"] {
        t.feed(input);
        assert_eq!(rustty_vt::snapshot::encode_to_vec(&t).unwrap(), before);
    }
    t.feed(b"\r\n666666\r\n777777\r\n888888\r\n999999");
    assert_eq!(t.screen().history_len(), 6);
    t.feed(b"\x1b[65535+T");
    assert_eq!(t.screen().history_len(), 2);
    assert_eq!(rows(&t), ["222222", "333333", "444444", "555555"]);
}

#[test]
fn restored_rows_keep_rich_cells_images_selection_viewport_and_snapshot_state() {
    for tmux in [false, true] {
        let mut t = Terminal::new(6, 3, 100);
        t.tmux_graphics = tmux;
        if tmux {
            t.set_tmux_history_limit(100);
        }
        t.feed(b"\x1b_Ga=T,i=9,C=1,f=32,s=1,v=1;AQID/w==\x1b\\");
        t.feed("\x1b[?2027h\x1b[1\"q\x1b[31m\x1b]8;id=link;https://example.org\x07界\u{301}\x1b]8;;\x07\r\nline1\r\nline2\r\nline3".as_bytes());
        let incoming = t.screen().history().last().unwrap().id;
        let text = t.screen().history().last().unwrap().text(0).to_string();
        let point = rustty_vt::GridPoint {
            row: incoming,
            col: 0,
        };
        t.screen_mut().selection = Some(Selection {
            start: point,
            end: point,
            rectangular: false,
        });
        t.screen_mut().scroll_viewport(1);
        let selected = t.screen().selection_text();
        let pixels = t.graphics().images[&9].pixels.clone();
        t.feed(b"\x1b[+T");
        assert_eq!(t.screen().row(0).id, incoming);
        assert_eq!(&*t.screen().row(0).text(0), text);
        assert!(t.screen().row(0).cells[0].protected());
        assert_eq!(t.screen().row(0).cells[0].width(), 2);
        assert!(t.screen().row(0).hyperlink(0).is_some());
        assert_eq!(t.screen().selection_text(), selected);
        assert_eq!(t.screen().viewport_offset, 0);
        assert_eq!(t.graphics().placements[0].row, incoming);
        assert_eq!(t.graphics().images[&9].pixels, pixels);
        let bytes = rustty_vt::snapshot::encode_to_vec(&t).unwrap();
        let restored = rustty_vt::snapshot::decode(bytes.as_slice(), Default::default()).unwrap();
        assert_eq!(rows(&restored), rows(&t));
        assert_eq!(restored.graphics().placements[0].row, incoming);
        let mut style = t.screen().row(0).style(0);
        // Native GHOSTSNP stores visual style; tmux capture spelling travels
        // in the wrapper's separate view metadata.
        style.tmux_ansi = 0;
        assert_eq!(restored.screen().row(0).style(0), style);
    }
}

#[test]
fn discarded_bottom_rows_release_their_selection_and_image_placements() {
    let mut t = terminal(false);
    t.feed(b"\x1b[4;1H\x1b_Ga=T,i=7,C=1,f=32,s=1,v=1;AQID/w==\x1b\\");
    let point = rustty_vt::GridPoint {
        row: t.screen().row(3).id,
        col: 3,
    };
    t.screen_mut().selection = Some(Selection {
        start: point,
        end: point,
        rectangular: false,
    });
    assert_eq!(t.graphics().placements.len(), 1);
    t.feed(b"\x1b[+T");
    assert!(t.screen().selection.is_none());
    assert!(t.graphics().placements.is_empty());
    assert!(t.screen().row_by_id(point.row).is_none());
}

#[test]
fn partial_width_restoration_repairs_split_wide_cells() {
    let mut t = Terminal::new(6, 2, 100);
    t.feed("界A界z\r\nBBBBBB\r\nCCCCCC".as_bytes());
    t.feed(b"\x1b[?69h\x1b[2;5s\x1b[+T");
    assert_eq!(rows(&t), ["B A界B", "CBBBBC"]);
    let row = t.screen().row(0);
    assert_eq!(row.cells[1].width(), 1);
    assert_eq!(row.cells[3].width(), 2);
    assert_eq!(row.cells[4].width(), 0);
}

#[test]
fn partial_width_restoration_keeps_only_anchors_in_the_restored_columns() {
    let mut t = Terminal::new(6, 3, 100);
    t.feed(b"\x1b_Ga=T,i=8,C=1,f=32,s=1,v=1;AQID/w==\x1b\\\x1b[1;3H\x1b_Ga=T,i=7,C=1,f=32,s=1,v=1;AQID/w==\x1b\\");
    t.feed(b"\x1b[Habcdef\r\n111111\r\n222222\r\n333333");
    let destination = t.screen().row(0).id;
    t.feed(b"\x1b[?69h\x1b[2;5s\x1b[+T");
    assert_eq!(rows(&t), ["1bcde1", "211112", "322223"]);
    assert_eq!(t.graphics().placements.len(), 1);
    assert_eq!(t.graphics().placements[0].image_id, 7);
    assert_eq!(t.graphics().placements[0].row, destination);
    assert_eq!(t.graphics().placements[0].col, 2);
}
