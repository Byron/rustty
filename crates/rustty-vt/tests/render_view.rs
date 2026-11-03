use rustty_vt::{GridPoint, Selection, Terminal};

fn selected(terminal: &Terminal) -> Vec<Option<[usize; 2]>> {
    terminal
        .screen()
        .render_view()
        .rows()
        .map(|row| row.selection.map(|range| [*range.start(), *range.end()]))
        .collect()
}

#[test]
fn viewport_selection_handles_direction_rectangles_history_and_detached_snapshots() {
    let mut terminal = Terminal::new(8, 3, 100);
    terminal.feed(b"abcdefgh\r\nijklmnop\r\nqrstuvwx\r\nyz");
    for rectangular in [false, true] {
        for reversed in [false, true] {
            let mut selection = Selection {
                start: terminal.screen().point(0, 6).unwrap(),
                end: terminal.screen().point(3, 2).unwrap(),
                rectangular,
            };
            if reversed {
                std::mem::swap(&mut selection.start, &mut selection.end);
            }
            terminal.screen_mut().selection = Some(selection);
            let expected = if rectangular {
                vec![Some([2, 6]); 3]
            } else {
                vec![Some([0, 7]), Some([0, 7]), Some([0, 2])]
            };
            assert_eq!(selected(&terminal), expected);
            terminal.screen_mut().scroll_viewport(1);
            let snapshot = terminal.screen().snapshot_viewport();
            let ranges: Vec<_> = snapshot
                .render_view()
                .rows()
                .map(|row| row.selection)
                .collect();
            let original: Vec<_> = terminal
                .screen()
                .render_view()
                .rows()
                .map(|row| row.selection)
                .collect();
            assert_eq!(ranges, original);
            terminal.screen_mut().scroll_viewport(-1);
        }
    }
}

#[test]
fn selected_ranges_cover_wide_glyphs_and_cross_wrapped_spacer_heads() {
    for (text, x, expected) in [
        ("界a", 0, vec![Some([0, 1]), None, None]),
        ("界a", 1, vec![Some([0, 1]), None, None]),
        ("1234567界a", 7, vec![Some([7, 7]), Some([0, 1]), None]),
    ] {
        for rectangular in [false, true] {
            let mut terminal = Terminal::new(8, 3, 0);
            terminal.feed(text.as_bytes());
            let point = terminal.screen().point(0, x).unwrap();
            terminal.screen_mut().selection = Some(Selection {
                start: point,
                end: point,
                rectangular,
            });
            if rectangular && x == 7 {
                assert_eq!(selected(&terminal), [Some([7, 7]), None, None]);
            } else {
                assert_eq!(selected(&terminal), expected);
            }
        }
    }
}

#[test]
fn row_keys_only_change_for_changed_content_or_selection_coverage() {
    let mut terminal = Terminal::new(8, 3, 0);
    terminal.feed(b"first\r\nsecond");
    let keys = |t: &Terminal| {
        t.screen()
            .render_view()
            .rows()
            .map(|r| r.key())
            .collect::<Vec<_>>()
    };
    let before = keys(&terminal);
    assert_eq!(before, keys(&terminal));
    let point = terminal.screen().point(0, 1).unwrap();
    terminal.screen_mut().selection = Some(Selection {
        start: point,
        end: point,
        rectangular: false,
    });
    let with_selection = keys(&terminal);
    assert_ne!(with_selection[0], before[0]);
    assert_eq!(with_selection[1..], before[1..]);
    terminal.screen_mut().set_cell_text(1, 0, "x");
    let rewritten = keys(&terminal);
    assert_ne!(rewritten[1], with_selection[1]);
    assert_eq!(rewritten[0], with_selection[0]);
    assert_eq!(rewritten[2], with_selection[2]);
    terminal.screen_mut().selection = Some(Selection {
        start: GridPoint {
            row: u64::MAX,
            col: 0,
        },
        end: point,
        rectangular: false,
    });
    assert!(selected(&terminal).iter().all(Option::is_none));
}

#[test]
fn cursor_text_is_lazy_handles_graphemes_spacers_blanks_and_viewport_visibility() {
    let mut terminal = Terminal::new(8, 3, 10);
    terminal.feed("e\u{301}界".as_bytes());
    terminal.screen_mut().cursor.col = 0;
    let old = {
        let view = terminal.screen().render_view();
        let cursor = view.cursor().unwrap();
        assert_eq!(cursor.text().unwrap().as_str(), "e\u{301}");
        cursor.key()
    };
    assert_eq!(old, terminal.screen().render_view().cursor().unwrap().key());
    terminal.screen_mut().set_cell_text(0, 0, "x");
    assert_ne!(old, terminal.screen().render_view().cursor().unwrap().key());
    terminal.screen_mut().cursor.col = 2;
    assert!(
        terminal
            .screen()
            .render_view()
            .cursor()
            .unwrap()
            .text()
            .is_none()
    );
    terminal.screen_mut().cursor.col = 3;
    assert_eq!(
        terminal
            .screen()
            .render_view()
            .cursor()
            .unwrap()
            .text()
            .unwrap()
            .as_str(),
        " "
    );
    terminal.screen_mut().cursor.visible = false;
    assert!(terminal.screen().render_view().cursor().is_none());
    terminal.screen_mut().cursor.visible = true;
    terminal.screen_mut().viewport_offset = 1;
    assert_eq!(terminal.screen().render_view().cursor().unwrap().y, 1);
    terminal.screen_mut().viewport_offset = 3;
    assert!(terminal.screen().render_view().cursor().is_none());
}

#[test]
fn view_keys_distinguish_incarnations_without_invalidating_rows_for_cursor_changes() {
    let mut terminal = Terminal::new(8, 3, 0);
    terminal.feed(b"\x1bcfirst");
    let before = terminal.screen().render_key();
    terminal.feed(b"\x1b[6 q");
    let moved = terminal.screen().render_key();
    assert_ne!(before, moved);
    assert!(before.same_row_layout(&moved));
    terminal.feed(b"\x1bcother");
    assert!(!before.same_row_layout(&terminal.screen().render_key()));
}

#[test]
fn a_wrapped_selection_rekeys_the_next_row_when_its_cells_did_not_change() {
    let mut terminal = Terminal::new(8, 3, 0);
    terminal.feed("1234567界a".as_bytes());
    let point = terminal.screen().point(0, 7).unwrap();
    terminal.screen_mut().selection = Some(Selection {
        start: point,
        end: point,
        rectangular: false,
    });
    let revision = terminal.screen().row(1).revision();
    let old = terminal.screen().render_view().rows().nth(1).unwrap().key();
    terminal.feed(b"\x1b[1;8Hx");
    assert!(terminal.screen().selection.is_some());
    assert_eq!(terminal.screen().row(1).revision(), revision);
    let view = terminal.screen().render_view();
    let row = view.rows().nth(1).unwrap();
    assert!(row.selection.is_none());
    assert_ne!(
        row.key(),
        old,
        "selection coverage changes independently of this row's content"
    );
}
