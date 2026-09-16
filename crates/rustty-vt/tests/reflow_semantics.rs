use rustty_vt::{GridPoint, Selection, SemanticContent, Terminal, snapshot};

#[test]
fn reflow_initializes_retained_blank_gaps_and_viewport_padding() {
    let mut terminal = Terminal::new(8, 6, 20);
    terminal.feed(b"A\x1b[3;1HZ");
    for width in [4, 8] {
        terminal.resize(width, 6);
        let screen = terminal.screen();
        assert!(
            screen
                .all_rows()
                .all(|row| row.cells.len() == usize::from(width))
        );
        assert_eq!(
            screen
                .rows()
                .map(|row| screen.row_text(row))
                .collect::<Vec<_>>(),
            ["A", "", "Z", "", "", ""],
        );
        assert_eq!((screen.cursor.row, screen.cursor.col), (2, 1));
        let bytes = snapshot::encode_to_vec(&terminal).unwrap();
        terminal = snapshot::decode(bytes.as_slice(), Default::default()).unwrap();
    }
    // Editing a retained gap after reflow must have real cell storage.
    terminal.feed(b"\x1b[2;8HX");
    assert_eq!(terminal.screen().row(1).cells[7].codepoint(), Some('X'));

    // Borrowed rows expose exactly their physical width; spare storage belongs to the page.
    for width in [1, 2, 3] {
        terminal.resize(width, 6);
        assert!(
            terminal
                .screen()
                .all_rows()
                .all(|row| row.cells.len() == usize::from(width))
        );
    }
}

#[test]
fn narrowing_with_more_active_rows_counts_continuations_at_the_active_boundary() {
    let mut terminal = Terminal::new(4, 2, 20);
    terminal.feed(b"ABCDEFGHI");
    terminal.resize(2, 4);

    assert_eq!(terminal.screen().history_len(), 1);
    assert_eq!(
        terminal
            .screen()
            .row_text(&terminal.screen().physical_row(0)),
        "AB"
    );
    assert_eq!(
        terminal
            .screen()
            .rows()
            .map(|row| terminal.screen().row_text(row))
            .collect::<Vec<_>>(),
        ["CD", "EF", "GH", "I"],
    );
    assert_eq!(terminal.screen().cursor.row, 3);
    assert_eq!(terminal.screen().cursor.col, 1);
}

#[test]
fn reflow_copies_source_prompt_metadata_to_each_destination_segment() {
    for (kind, expected) in [
        ("i", SemanticContent::Prompt),
        ("s", SemanticContent::Input),
    ] {
        let mut terminal = Terminal::new(12, 4, 20);
        terminal
            .feed(format!("\x1b]133;A;redraw=0\x07\x1b]133;P;k={kind}\x07abcdefghij").as_bytes());
        terminal.resize(4, 4);
        for (row, text) in terminal.screen().rows().take(3).zip(["abcd", "efgh", "ij"]) {
            assert_eq!(row.semantic, expected);
            assert_eq!(terminal.screen().row_text(row), text);
        }
        let bytes = snapshot::encode_to_vec(&terminal).unwrap();
        let mut terminal = snapshot::decode(bytes.as_slice(), Default::default()).unwrap();
        terminal.resize(12, 4);
        assert_eq!(terminal.screen().row(0).semantic, expected);
        assert_eq!(
            terminal.screen().row_text(&terminal.screen().row(0)),
            "abcdefghij"
        );
    }
}

#[test]
fn reflow_remaps_duplicate_anchors_and_both_halves_of_a_wide_cell() {
    let mut terminal = Terminal::new(8, 4, 20);
    terminal.feed("abc界z".as_bytes());
    let base = terminal.screen().point(0, 3).unwrap();
    let tail = terminal.screen().point(0, 4).unwrap();
    let first = terminal.screen_mut().track(base);
    let duplicate = terminal.screen_mut().track(base);
    let second = terminal.screen_mut().track(tail);
    terminal.screen_mut().selection = Some(Selection {
        start: base,
        end: tail,
        rectangular: false,
    });

    terminal.resize(4, 4);
    let screen = terminal.screen();
    let spacer = screen.point(0, 3).unwrap();
    let wide_tail = screen.point(1, 1).unwrap();
    assert!(screen.row(0).cells[3].spacer_head());
    assert_eq!(screen.row(1).cells[1].width(), 0);
    assert_eq!(screen.resolve(first), Some(spacer));
    assert_eq!(screen.resolve(duplicate), Some(spacer));
    assert_eq!(screen.resolve(second), Some(wide_tail));
    assert_eq!(screen.selection.unwrap().start, spacer);
    assert_eq!(screen.selection.unwrap().end, wide_tail);

    terminal.resize(8, 4);
    let screen = terminal.screen();
    let base = screen.point(0, 3).unwrap();
    let tail = screen.point(0, 4).unwrap();
    assert_eq!(screen.resolve(first), Some(base));
    assert_eq!(screen.resolve(duplicate), Some(base));
    assert_eq!(screen.resolve(second), Some(tail));
    assert_eq!(screen.selection.unwrap().start, base);
    assert_eq!(screen.selection.unwrap().end, tail);
}

#[test]
fn reflow_maps_graphics_in_trailing_blanks_without_retaining_the_blanks() {
    use rustty_vt::graphics::{Placement, PlacementId};

    let mut terminal = Terminal::new(8, 4, 20);
    terminal.feed(b"A\x1b[2;1HZ\x1b[1;1H");
    let source = terminal.screen().point(0, 7).unwrap();
    let placement = Placement {
        image_id: 1,
        placement_id: PlacementId::External(1),
        row: source.row,
        col: source.col,
        columns: 1,
        rows: 1,
        viewport_row: None,
        z: 0,
        source: [0; 4],
        offset: [0; 2],
        virtual_placement: false,
        parent: None,
        parent_offset: [0; 2],
    };
    // Duplicate anchors share a mapping. Out-of-bounds ordinary anchors vanish,
    // while virtual and parent-relative placements do not own grid coordinates.
    terminal.screen_mut().graphics.placements = vec![
        placement.clone(),
        Placement {
            placement_id: PlacementId::External(2),
            ..placement.clone()
        },
        Placement {
            placement_id: PlacementId::External(3),
            col: 8,
            ..placement.clone()
        },
        Placement {
            placement_id: PlacementId::External(4),
            virtual_placement: true,
            ..placement.clone()
        },
        Placement {
            placement_id: PlacementId::External(5),
            parent: Some((1, PlacementId::External(1))),
            ..placement
        },
    ];
    terminal.resize(4, 4);
    let screen = terminal.screen();
    assert_eq!(screen.row(1).cells[0].codepoint(), Some('Z'));
    let mapped = screen.point(0, 3).unwrap();
    let placements = &screen.graphics.placements;
    assert_eq!(placements.len(), 4);
    for placement in &placements[..2] {
        assert_eq!(
            GridPoint {
                row: placement.row,
                col: placement.col
            },
            mapped
        );
    }
    for placement in &placements[2..] {
        assert_eq!(
            GridPoint {
                row: placement.row,
                col: placement.col
            },
            source
        );
    }
    assert_eq!(placements[2].placement_id, PlacementId::External(4));
    assert_eq!(placements[3].placement_id, PlacementId::External(5));
}
