use super::*;
use crate::{PageCapacity, Terminal, snapshot};

fn assert_references(screen: &Screen) {
    let mut expected = HashMap::<(u64, u16), usize>::new();
    for page in &screen.pages.pages {
        let mut slots = HashSet::new();
        for row in 0..usize::from(page.rows) {
            let view = page.row(row);
            assert!(slots.insert(page.headers[row].offset()));
            for (col, cell) in view.cells.iter().enumerate() {
                assert_eq!(cell.bits() >> 48, 0);
                if cell.style_id() != 0 {
                    *expected.entry((page.serial, cell.style_id())).or_default() += 1;
                    assert!(page.headers[row].has(RowHeader::STYLED));
                }
                assert_eq!(
                    cell.has_hyperlink(),
                    page.link_map.contains_key(&((view.offset + col) as u32))
                );
                assert_eq!(
                    cell.has_grapheme(),
                    page.grapheme_map
                        .contains_key(&((view.offset + col) as u32))
                );
                if cell.has_hyperlink() {
                    assert!(page.headers[row].has(RowHeader::HYPERLINK));
                }
                if cell.has_grapheme() {
                    assert!(page.headers[row].has(RowHeader::GRAPHEME));
                    assert_eq!(
                        view.text(col).chars().count(),
                        usize::from(view.grapheme(col).unwrap().len) + 1
                    );
                }
            }
        }
        page.links.assert_references(
            page.link_map.values().copied(),
            screen
                .cursor_link
                .filter(|(owner, _)| *owner == page.serial)
                .map(|(_, id)| id),
        );
        page.graphemes
            .assert_allocations(page.grapheme_map.values().copied());
        let mut charged = page.clone();
        charged.refresh_charge();
        assert!(
            page.storage_bytes() >= charged.storage_bytes(),
            "cached charge underestimated page {}",
            page.serial
        );
    }
    if let Some((owner, id)) = screen.cursor_style {
        assert_eq!(
            owner,
            screen
                .pages
                .page_at(screen.history_len() + screen.cursor.row)
                .0
                .serial
        );
        *expected.entry((owner, id)).or_default() += 1;
    }
    for page in &screen.pages.pages {
        for (id, _) in page.styles.iter() {
            assert_eq!(
                usize::from(page.styles.reference_count(id)),
                expected.remove(&(page.serial, id)).unwrap_or(0),
                "page {} style {id}",
                page.serial
            );
        }
    }
    assert!(expected.is_empty());
}

#[test]
fn index_preserves_cursor_resources_and_generation() {
    for (rows, history, alternate, changes) in [
        (1, false, false, 1),
        (3, false, false, 1),
        (1, true, false, 2),
        (3, true, false, 2),
        (1, true, true, 1),
        (3, true, true, 1),
    ] {
        for linked in [false, true] {
            let mut terminal = Terminal::with_limits(
                8,
                rows,
                if history {
                    ScrollbackLimits::default()
                } else {
                    ScrollbackLimits::NONE
                },
            );
            if alternate {
                terminal.feed(b"\x1b[?1049h");
            }
            terminal.cursor_position(usize::from(rows), 1);
            let style = Style {
                foreground: Color::Rgb(1, 2, 3),
                bold: true,
                ..Style::default()
            };
            // Public pen edits must still be admitted before index returns.
            terminal.screen_mut().cursor.style = style;
            if linked {
                terminal.screen_mut().cursor.hyperlink = Some(Arc::new(HyperlinkData::new(
                    b"https://example.org/\xff",
                    None,
                )));
            }
            terminal.generation = u64::MAX;
            #[cfg(feature = "allocation-probe")]
            crate::allocation_probe::reset();
            terminal.index();
            #[cfg(feature = "allocation-probe")]
            {
                let admissions = if !linked {
                    0
                } else if history && !alternate {
                    3
                } else if rows == 1 {
                    1
                } else {
                    2
                };
                let counts = crate::allocation_probe::counts();
                assert_eq!(counts.hyperlink_admissions, admissions);
                assert_eq!(counts.string_reservations, admissions);
            }
            assert_eq!(terminal.generation, changes - 1);
            let screen = terminal.screen();
            assert_references(screen);
            let (index, _) = screen.cursor_location();
            let (_, id) = screen.cursor_style.expect("index admits the pen style");
            assert_eq!(*screen.pages.pages[index].styles.get(id), style);
            assert_eq!(screen.cursor_link.is_some(), linked);
            if linked {
                assert_eq!(
                    screen.cursor.hyperlink.as_ref().unwrap().uri_bytes(),
                    b"https://example.org/\xff"
                );
            }
        }
    }
}

#[test]
fn resource_ownership_survives_edits_reflow_snapshots_and_eviction() {
    for columns in [8, 80, 1024] {
        let mut terminal = Terminal::with_limits(columns, 4, ScrollbackLimits::default());
        terminal.feed(b"\x1b[?2027h");
        for value in 0..160 {
            terminal.feed(format!("\x1b[38;2;{value};0;0m\x1b]8;id={value};https://example.org/{value}\x07a\u{301}界\x1b]8;;\x07").as_bytes());
            assert_references(terminal.screen());
        }
        for sequence in [
            "\x1b[H\x1b[2@",
            "\x1b[2P",
            "\x1b[2X",
            "\x1b[2S",
            "\x1b[2T",
            "\x1b[2;4r\x1b[4;1H\n",
            "\x1b[r\x1b[?69h\x1b[2;7s\x1b[2S",
            "\x1b[?69l\x1b[H\x1b[44m\x1b[2J",
            "\x1b#8",
            "\x1b[22J",
            "\x1b[3J",
        ] {
            terminal.feed(sequence.as_bytes());
            assert_references(terminal.screen());
        }
        for width in [12, 3, 32, columns] {
            terminal.resize(width, 5);
            assert_references(terminal.screen());
            terminal.feed("\x1b[1;31mwide:界a\u{301}\x1b[0m\r\n".as_bytes());
            assert_references(terminal.screen());
        }
        for width in [3, 20, columns] {
            terminal.feed(b"\x1b[?7l");
            terminal.resize(width, 3);
            assert_references(terminal.screen());
        }
        let detached = terminal.screen().snapshot_viewport();
        assert_references(&detached);
        let wire = snapshot::encode_to_vec(&terminal).unwrap();
        let mut restored = snapshot::decode(wire.as_slice(), Default::default()).unwrap();
        assert_references(restored.screen());
        restored.set_scrollback_memory_limit(Some(1));
        restored.feed(&b"line\r\n".repeat(1000));
        assert_references(restored.screen());
    }
}

#[test]
fn scrolling_widens_all_active_pages_after_restore() {
    let mut terminal = Terminal::new(128, 64, 1000);
    let screen = terminal.screen_mut();
    screen.pages = PageList::default();
    for rows in [1, 32, 33] {
        screen.pages.append(
            PageCapacity {
                cols: 4,
                rows,
                ..PageCapacity::STANDARD
            },
            rows,
        );
    }
    let mut id = 0;
    for page in &mut screen.pages.pages {
        for row in 0..usize::from(page.rows) {
            page.row_ids[row] = id;
            let slot = page.slot(row, 0);
            page.cells[slot].set_codepoint(char::from_u32('A' as u32 + id as u32));
            page.mark_cell(row, page.cells[slot]);
            id += 1;
        }
    }
    screen.next_row = id;
    let wire = snapshot::encode_to_vec(&terminal).unwrap();
    let mut terminal = snapshot::decode(wire.as_slice(), Default::default()).unwrap();
    terminal.feed(b"\x1b[T");

    let screen = terminal.screen();
    assert_eq!(screen.history_len(), 2);
    assert_eq!(screen.physical_row(0).cells.len(), 4);
    assert!(screen.rows().all(|row| row.cells.len() == 128));
    assert_eq!(screen.row(0).cells[0].codepoint(), None);
    for row in 1..64 {
        assert_eq!(
            screen.row(row).cells[0].codepoint(),
            char::from_u32('A' as u32 + row as u32 + 1)
        );
    }
    assert_references(screen);
}

#[test]
fn printing_widens_before_clamping_public_cursor_edits() {
    for alternate in [false, true] {
        for batched in [false, true] {
            for col in [3, usize::MAX] {
                let mut terminal = Terminal::new(8, 2, 0);
                if alternate {
                    terminal.feed(b"\x1b[?1049h");
                }
                let screen = terminal.screen_mut();
                screen.pages = PageList::default();
                screen.pages.append(
                    PageCapacity {
                        cols: 4,
                        rows: 2,
                        ..PageCapacity::STANDARD
                    },
                    2,
                );
                screen.pages.pages[0].row_ids[..2].copy_from_slice(&[0, 1]);
                screen.cursor.col = col;
                if batched {
                    for text in ["éé", "界界"] {
                        let mut reference = terminal.clone();
                        for cp in text.chars() {
                            reference.print(cp);
                        }
                        let mut actual = terminal.clone();
                        actual.feed(text.as_bytes());
                        assert_eq!(actual.generation, reference.generation);
                        assert!(
                            snapshot::encode_to_vec(&actual).unwrap()
                                == snapshot::encode_to_vec(&reference).unwrap()
                        );
                        assert_references(actual.screen());
                    }
                    terminal.feed(b"X");
                } else {
                    terminal.print('X');
                }
                let screen = terminal.screen();
                assert_eq!(screen.row(0).cells.len(), 8);
                assert_eq!(screen.row(0).cells[col.min(7)].codepoint(), Some('X'));
                assert_eq!(screen.cursor.col, col.min(7).saturating_add(1).min(7));
                assert_eq!(screen.cursor.pending_wrap, col >= 7);
                assert_references(screen);
            }
        }
    }
}

#[test]
fn plain_copies_release_payload_charges() {
    for whole_row in [false, true] {
        let mut terminal = Terminal::new(8, 2, 0);
        terminal.feed("\x1b]8;id=link;https://example.org\x07a\u{301}\x1b]8;;\x07\r\np".as_bytes());
        let screen = terminal.screen_mut();
        let before = screen.pages.pages[0].storage_bytes();
        if whole_row {
            let mut copy = RowCopy::from_view(screen.row(1));
            copy.id = screen.row(0).id;
            screen.install_row(0, copy, usize::MAX);
        } else {
            let copy = screen.row(1).copy_cell(0);
            screen.install_cell(0, 0, copy, false).unwrap();
        }
        assert_eq!(&*screen.row(0).text(0), "p");
        assert!(screen.row(0).hyperlink(0).is_none());
        let page = &mut screen.pages.pages[0];
        let charged = page.storage_bytes();
        assert!(charged < before);
        page.refresh_charge();
        assert_eq!(charged, page.storage_bytes(), "whole row: {whole_row}");
        assert_references(screen);
    }
}

#[test]
fn detached_resources_outlive_source_mutation_and_destruction() {
    let (detached, json) = {
        let mut terminal = Terminal::new(8, 2, 1000);
        terminal.feed(
            "\x1b[?2027h\x1b[31m\x1b]8;id=link;https://example.org/\u{fffd}\x07界\u{301}"
                .as_bytes(),
        );
        terminal.feed(b"\x1b]8;;\x07\x1b[44m\x1b[K");
        let detached = terminal.screen().snapshot_viewport();
        let json = serde_json::to_value(&detached).unwrap()["rows"].clone();
        terminal.feed(b"\x1b[0m\x1b[H\x1b[2Jnew");
        terminal.resize(3, 4);
        assert_references(terminal.screen());
        (detached, json)
    };
    assert_eq!(serde_json::to_value(&detached).unwrap()["rows"], json);
    assert_eq!(&*detached.row(0).text(0), "界\u{301}");
    assert_eq!(detached.row(0).style(0).foreground, Color::Indexed(1));
    assert_eq!(detached.row(0).style(2).background, Color::Indexed(4));
    assert!(detached.row(0).hyperlink(0).is_some());
    assert_references(&detached);
}

#[test]
fn blank_backgrounds_switch_between_inline_and_managed_styles() {
    let mut terminal = Terminal::new(8, 2, 0);
    let screen = terminal.screen_mut();
    for background in [Color::Indexed(255), Color::Rgb(0, 128, 255), Color::Default] {
        screen.set_cell_style(
            0,
            0,
            Style {
                background,
                ..Style::default()
            },
        );
        assert_eq!(screen.row(0).style(0).background, background);
        assert_eq!(screen.row(0).cells[0].style_id(), 0);
        let managed = Style {
            bold: true,
            background: Color::Rgb(12, 34, 56),
            ..Style::default()
        };
        screen.set_cell_style(0, 0, managed);
        assert_eq!(screen.row(0).style(0), managed);
        assert_eq!(screen.row(0).cells[0].background(), None);
        assert_references(screen);
    }
}

#[test]
fn failed_style_rebuild_preserves_words_maps_and_references() {
    let rgb = |value: u32| Style {
        foreground: Color::Rgb(value as u8, (value >> 8) as u8, (value >> 16) as u8),
        ..Style::default()
    };
    let ordinary = (0..)
        .map(rgb)
        .find(|style| style.native_hash() & 127 > 32)
        .unwrap();
    let colliding: Vec<_> = (0..)
        .map(rgb)
        .filter(|style| style.native_hash() & 255 == 0)
        .take(32)
        .collect();
    let capacity = PageCapacity {
        cols: 80,
        rows: 2,
        ..PageCapacity::STANDARD
    };
    let mut original = Page::new(capacity, 2, 0);
    let ordinary_id = original.styles.acquire(ordinary).unwrap();
    for (col, style) in colliding.into_iter().enumerate() {
        let id = original.styles.acquire(style).unwrap();
        original.cells[col].set_style_id(id);
        original.mark_cell(0, original.cells[col]);
    }
    original.cells[80].set_style_id(ordinary_id);
    original.mark_cell(1, original.cells[80]);
    original.cells[0].set_codepoint(Some('a'));
    original.cells[0].set_grapheme(true);
    original.mark_cell(0, original.cells[0]);
    let grapheme = original.graphemes.acquire(5).unwrap();
    original
        .graphemes
        .set_text(grapheme, Arc::from("a\u{301}\u{302}\u{303}\u{304}\u{305}"));
    original.grapheme_map.insert(0, grapheme);
    for grow in [None, Some(PageResource::Styles)] {
        let mut page = original.clone();
        assert_eq!(page.rebuild(grow), Err(SetFull::OutOfMemory));
        assert_eq!(page.capacity, capacity);
        assert_eq!(page.cells, original.cells);
        assert_eq!(page.grapheme_map, original.grapheme_map);
        page.graphemes.assert_allocations(std::iter::once(grapheme));
        for cell in page.cells.iter().filter(|cell| cell.style_id() != 0) {
            assert_eq!(page.styles.reference_count(cell.style_id()), 1);
            assert_eq!(
                page.styles.get(cell.style_id()),
                original.styles.get(cell.style_id())
            );
        }
    }
}

#[test]
fn page_rotations_preserve_slots_and_recycling_renews_identity() {
    let mut terminal = Terminal::with_limits(80, 4, ScrollbackLimits::default());
    terminal.feed(
        "\x1b[31m\x1b]8;id=link;https://example.org\x07a\u{301}\x1b]8;;\x07\x1b[0m".as_bytes(),
    );
    let page = &terminal.screen().pages.pages[0];
    let slot = page.headers[0].offset();
    let word = page.cells[slot];
    let map = page.grapheme_map.clone();
    terminal.screen_mut().pages.pages[0].rotate_rows(0..4, true);
    let page = &terminal.screen().pages.pages[0];
    assert_eq!(page.headers[3].offset(), slot);
    assert_eq!(page.cells[slot], word);
    assert_eq!(page.grapheme_map, map);
    assert_references(terminal.screen());
    let mut page = terminal.screen_mut().pages.pages.pop_front().unwrap();
    let cells = page.cells.as_ptr();
    let serial = page.serial;
    page.recycle(page.capacity, serial + 1);
    assert_eq!(page.cells.as_ptr(), cells);
    assert_ne!(page.serial, serial);
    assert!(page.cells.iter().all(|cell| cell.bits() == 0));
    assert!(page.link_map.is_empty() && page.grapheme_map.is_empty());
    assert!(page.row_ids.iter().all(|id| *id == 0));
    assert_eq!(page.styles.count(), 0);
    page.links.assert_references(std::iter::empty(), None);
    page.graphemes.assert_allocations(std::iter::empty());
}

#[test]
fn inline_text_matches_utf8_for_every_scalar() {
    let empty = CellText::scalar(None);
    assert_eq!(empty.as_str(), "");
    assert_eq!(empty.chars().next(), None);
    for cp in (0..=0x10ffff).filter_map(char::from_u32) {
        let text = CellText::scalar(Some(cp));
        assert_eq!(text.as_str(), cp.encode_utf8(&mut [0; 4]));
        assert_eq!(text.chars().next(), Some(cp));
        assert_eq!(text.chars().next_back(), Some(cp));
        assert_eq!(text.chars().count(), 1);
    }
    for value in ["", "a\u{301}", "👩\u{200d}💻"] {
        let text = CellText(CellTextStorage::Grapheme(value));
        assert_eq!(text.as_str(), value);
        assert!(text.chars().eq(value.chars()));
        assert!(text.chars().rev().eq(value.chars().rev()));
    }
}

#[test]
fn wrapped_transfers_share_unchanged_text_and_preserve_detached_snapshots() {
    for base in ['☺', ' '] {
        let mut screen = Screen::new(3, 2, ScrollbackLimits::default());
        screen.set_cell_text(0, 2, "☺\u{200d}");
        let (shared, len) = screen.row(0).copy_cell(2).text.unwrap();
        assert_eq!(len, 1);
        let detached = screen.snapshot_viewport();
        screen.cell_mut(0, 2).set_codepoint(None);
        screen.cursor.row = 1;
        screen.cursor.col = 0;
        screen.write_cursor_cell(Some(base), 2, false, screen.cursor_location());
        screen.move_wrapped_grapheme(2, "\u{200d}");
        let (moved, len) = screen.row(1).copy_cell(0).text.unwrap();
        assert_eq!(len, 1);
        assert_eq!(Arc::ptr_eq(&shared, &moved), base == '☺');
        assert_eq!(&*moved, format!("{base}\u{200d}"));
        screen.append_grapheme(0, '❤').unwrap();
        screen.pages.pages[0]
            .rebuild(Some(PageResource::Graphemes))
            .unwrap();
        assert_eq!(&*screen.row(1).text(0), format!("{base}\u{200d}❤"));
        assert_eq!(&*detached.row(0).text(2), "☺\u{200d}");
        assert_eq!(&*shared, "☺\u{200d}");
        assert_references(&screen);
        assert_references(&detached);
    }
}

#[test]
fn exposed_rows_are_blank_after_truncation_rotation_and_recycling() {
    let mut terminal = Terminal::new(8, 4, 0);
    for row in 1..=4 {
        terminal.feed(format!("\x1b[{row};1H\x1b[31m\x1b]8;id=test;https://example.org\x07a\u{301}界\x1b]8;;\x07\x1b[0m").as_bytes());
    }
    let original = &terminal.screen().pages.pages[0];
    for background in [Color::Default, Color::Indexed(4), Color::Rgb(12, 34, 56)] {
        for operation in 0..4 {
            let mut page = original.clone();
            match operation {
                0 => {}
                1 => page.truncate(2),
                2 => {
                    page.rotate_rows(0..4, true);
                    page.remove_prefix(2);
                }
                _ => page.recycle(page.capacity, page.serial + 1),
            }
            let row = usize::from(page.rows);
            page.expose(100, background);
            assert_eq!(page.row_ids[row], 100);
            assert!(page.headers[row].has(RowHeader::DIRTY));
            assert!(!page.headers[row].has(
                RowHeader::MANAGED
                    | RowHeader::WRAPPED
                    | RowHeader::CONTINUATION
                    | RowHeader::PLACEHOLDER
            ));
            assert_eq!(page.headers[row].semantic(), SemanticContent::Output);
            assert!(
                page.row_cells(row)
                    .iter()
                    .all(|cell| *cell == Cell::blank(background))
            );
            page.graphemes
                .assert_allocations(page.grapheme_map.values().copied());
            page.links
                .assert_references(page.link_map.values().copied(), None);
        }
    }
}
