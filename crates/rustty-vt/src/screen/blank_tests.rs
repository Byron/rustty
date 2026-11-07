use super::*;

#[test]
fn blank_destination_batches_retain_styles_and_invalidate_the_hint() {
    for prefix in [b"\x1b[0m".as_slice(), b"\x1b[1;31;44m\x1b[1\"q"] {
        for col in [1, 4] {
            for text in ["ascii", "éøa", "界語"] {
                let mut terminal = Terminal::new(16, 3, 100);
                terminal.feed(prefix);
                terminal.feed(format!("\x1b[1;{col}H").as_bytes());
                assert!(terminal.screen().row(0).blank);
                let mut scalar = terminal.clone();
                for cp in text.chars() {
                    scalar.print(cp);
                }
                terminal.feed(text.as_bytes());
                assert!(!terminal.screen().row(0).blank);
                assert!(terminal.screen().rows().skip(1).all(|row| row.blank));
                assert_eq!(
                    snapshot::encode_to_vec(&terminal).unwrap(),
                    snapshot::encode_to_vec(&scalar).unwrap()
                );
                assert_references(terminal.screen());
                assert_references(scalar.screen());

                // The second write must inspect the previously written row.
                let movement = format!("\x1b[1;{col}H\x1b[32m");
                terminal.feed(movement.as_bytes());
                scalar.feed(movement.as_bytes());
                terminal.feed(b"XY");
                scalar.print('X');
                scalar.print('Y');
                assert_eq!(
                    snapshot::encode_to_vec(&terminal).unwrap(),
                    snapshot::encode_to_vec(&scalar).unwrap()
                );
                assert_references(terminal.screen());
                terminal.feed(b"\x1b[0m\x1b[2K");
                assert!(terminal.screen().row(0).blank);
                assert_references(terminal.screen());
            }
        }
    }
}

#[test]
fn repeated_blank_erases_preserve_clean_rows_and_revisions() {
    let mut terminal = Terminal::new(16, 4, 0);
    for page in &mut terminal.screen_mut().pages.pages {
        for header in &mut page.headers {
            header.set(RowHeader::DIRTY, false);
        }
    }
    let revisions: Vec<_> = terminal.screen().rows().map(RowView::revision).collect();
    terminal.feed(b"\x1b[2K\x1b[8X\x1b[2J\x1b[2J");
    assert_eq!(
        terminal
            .screen()
            .rows()
            .map(RowView::revision)
            .collect::<Vec<_>>(),
        revisions
    );
    assert!(terminal.screen().rows().all(|row| !row.dirty));
    assert_references(terminal.screen());
}

#[test]
fn blank_rows_track_writes_erases_copies_and_reflow() {
    for input in [
        "ascii",
        "界界",
        "e\u{301}",
        "\x1b#8",
        "\x1b[31mstyled",
        "\x1b]8;;https://example.test\x07linked\x1b]8;;\x07",
        "\x1b[1\"qprotected\x1b[0\"q",
        "\x1b]66;w=3;span\x07",
    ] {
        let mut terminal = Terminal::new(8, 4, 100);
        assert!(terminal.screen().rows().all(|row| row.blank));
        terminal.feed(input.as_bytes());
        assert!(terminal.screen().rows().any(|row| !row.blank), "{input:?}");
        assert_references(terminal.screen());
        terminal
            .screen_mut()
            .copy_row_cells(0, 2, 0, 8, Color::Default);
        assert_references(terminal.screen());
        terminal.screen_mut().pages.pages[0].rotate_rows(0..4, true);
        assert_references(terminal.screen());
        for cols in [3, 9, 8] {
            terminal.resize(cols, 4);
            assert_references(terminal.screen());
        }
        terminal.feed(b"\x1b[0m\x1b[2J");
        assert!(terminal.screen().rows().all(|row| row.blank), "{input:?}");
        assert_references(terminal.screen());
        assert_references(terminal.clone().screen());
        assert_references(&terminal.screen().snapshot_viewport());
    }
}

#[test]
fn colored_and_protected_rows_are_never_known_blank() {
    let mut terminal = Terminal::new(8, 3, 100);
    terminal.feed(b"\x1b[41m\x1b[2J");
    assert!(terminal.screen().rows().all(|row| !row.blank));
    assert_references(terminal.screen());
    terminal.feed(b"\x1b[0m\x1b[2J\x1b[1\"qX\x1b[0\"q");
    terminal.erase_display(2, true);
    assert_eq!(terminal.screen().row(0).cells[0].codepoint(), Some('X'));
    assert!(!terminal.screen().row(0).blank);
    assert_references(terminal.screen());
    terminal.erase_display(2, false);
    assert!(terminal.screen().rows().all(|row| row.blank));
    assert_references(terminal.screen());

    terminal.screen_mut().cursor.style.background = Color::Indexed(2);
    terminal.feed(b"\x1b[1X");
    assert!(!terminal.screen().row(0).blank);
    assert_eq!(
        terminal.screen().row(0).style(1).background,
        Color::Indexed(2)
    );
    assert_references(terminal.screen());
}

#[test]
fn blank_erases_preserve_required_semantic_and_wrap_metadata_changes() {
    let mut terminal = Terminal::new(8, 3, 100);
    let screen = terminal.screen_mut();
    screen.semantic_prompt_seen = true;
    let (index, row) = screen.cursor_location();
    let header = &mut screen.pages.pages[index].headers[row];
    header.set_semantic(SemanticContent::Input);
    header.set(RowHeader::WRAPPED | RowHeader::CONTINUATION, true);
    assert!(header.has(RowHeader::BLANK));
    terminal.erase_display(2, true);
    assert_eq!(terminal.screen().row(0).semantic, SemanticContent::Input);
    assert!(terminal.screen().row(0).wrapped);
    terminal.erase_line(2, false);
    assert_eq!(terminal.screen().row(0).semantic, SemanticContent::Input);
    terminal.erase_display(2, false);
    let row = terminal.screen().row(0);
    assert!(row.blank && !row.wrapped && !row.wrap_continuation);
    assert_eq!(row.semantic, SemanticContent::Output);
    assert_references(terminal.screen());
}

#[test]
fn restored_and_exposed_zero_rows_recover_the_blank_hint() {
    let mut terminal = Terminal::new(8, 4, 100);
    terminal.feed(b"\x1b[2;1Htext");
    let wire = snapshot::encode_to_vec(&terminal).unwrap();
    let binary = snapshot::decode(wire.as_slice(), Default::default()).unwrap();
    let json: Screen =
        serde_json::from_value(serde_json::to_value(terminal.screen()).unwrap()).unwrap();
    for screen in [binary.screen(), &json] {
        for (index, row) in screen.rows().enumerate() {
            assert_eq!(row.blank, index != 1);
        }
        assert_references(screen);
    }
    let mut page = Page::new(
        PageCapacity {
            cols: 8,
            rows: 4,
            ..PageCapacity::STANDARD
        },
        4,
        0,
    );
    page.row_cells_mut(2)[1].set_codepoint(Some('x'));
    assert!(!page.row(2).blank);
    page.truncate(2);
    page.expose(5, Color::Default);
    assert!(page.row(2).blank);
    page.expose(6, Color::Indexed(0));
    assert!(!page.row(3).blank);
    page.reset_row(3, 6, Color::Default);
    assert!(page.row(3).blank);
    page.recycle(page.capacity, 1);
    page.expose(7, Color::Default);
    assert!(page.row(0).blank);
}
