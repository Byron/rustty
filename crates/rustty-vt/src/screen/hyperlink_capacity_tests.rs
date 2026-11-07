use super::*;
use crate::page_resources::{HyperlinkFull, HyperlinkKey};
use crate::{PageCapacity, Terminal};

#[test]
fn printing_one_long_link_does_not_grow_the_distinct_link_set() {
    // This regression uses only pre-existing APIs and can run before the port.
    let mut terminal = Terminal::new(200, 1, 0);
    let before = terminal.screen().pages.pages[0].capacity.hyperlink_bytes;
    terminal.feed(b"\x1b]8;id=one;https://example.test/one\x07");
    terminal.feed(&[b'x'; 200]);
    terminal.feed(b"\x1b]8;;\x07");
    let screen = terminal.screen();
    let (index, row) = screen.cursor_location();
    let page = &screen.pages.pages[index];
    assert_eq!(page.capacity.hyperlink_bytes, before);
    assert_eq!(page.link_map.len(), 200);
    for col in 0..200 {
        assert_eq!(
            page.row(row).hyperlink(col).unwrap().uri_bytes(),
            b"https://example.test/one"
        );
    }
    page.links
        .assert_references(page.link_map.values().copied(), None);
}

#[test]
fn growing_a_full_hyperlink_map_preserves_set_and_string_capacity() {
    let capacity = PageCapacity {
        cols: 200,
        rows: 1,
        hyperlink_bytes: 192,
        hyperlink_cells: 8,
        ..PageCapacity::default()
    };
    let mut page = Page::new(capacity, 1, 0);
    let key = HyperlinkKey::new(b"https://example.test/reused", None, 1);
    let count = page.links.available_cells();
    assert_eq!(count, 12);
    for col in 0..count {
        let id = page.links.copy_cell(key, 0).unwrap();
        let slot = page.slot(0, col);
        page.link_map.insert(slot as u32, id);
        page.cells[slot].set_codepoint(Some('x'));
        page.cells[slot].set_hyperlink(true);
        page.mark_cell(0, page.cells[slot]);
    }
    assert!(matches!(
        page.links.copy_cell(key, 0),
        Err(HyperlinkFull::Map)
    ));
    assert!(matches!(
        PageResource::for_link(HyperlinkFull::Map),
        Some(PageResource::LinkCells)
    ));
    page.rebuild(Some(PageResource::LinkCells)).unwrap();
    assert!(page.capacity.hyperlink_cells > capacity.hyperlink_cells);
    assert_eq!(page.capacity.hyperlink_bytes, capacity.hyperlink_bytes);
    assert_eq!(page.capacity.string_bytes, capacity.string_bytes);
    assert!(page.links.available_cells() > 0);
    page.links
        .assert_references(page.link_map.values().copied(), None);
    for col in 0..count {
        assert_eq!(page.row(0).hyperlink(col).unwrap().uri_bytes(), key.uri);
    }

    let cells = page.capacity.hyperlink_cells;
    page.rebuild(Some(PageResource::Links)).unwrap();
    assert!(page.capacity.hyperlink_bytes > capacity.hyperlink_bytes);
    assert_eq!(page.capacity.hyperlink_cells, cells);
    page.links
        .assert_references(page.link_map.values().copied(), None);
}

#[test]
fn zero_hyperlink_dimensions_grow_independently() {
    let mut page = Page::new(
        PageCapacity {
            cols: 32,
            rows: 1,
            hyperlink_bytes: 0,
            hyperlink_cells: 0,
            ..PageCapacity::default()
        },
        1,
        0,
    );
    let key = HyperlinkKey::new(b"https://example.test/zero", None, 1);
    assert!(matches!(
        page.links.insert(key),
        Err(HyperlinkFull::Set(SetFull::OutOfMemory))
    ));
    page.rebuild(Some(PageResource::LinkCells)).unwrap();
    assert_eq!(page.capacity.hyperlink_bytes, 0);
    assert_eq!(page.capacity.hyperlink_cells, 64);
    page.rebuild(Some(PageResource::Links)).unwrap();
    assert_eq!(page.capacity.hyperlink_cells, 64);
    let id = page.links.insert(key).unwrap();
    let free = page.links.available_cells();
    page.links.retain_cell(id).unwrap();
    assert_eq!(page.links.available_cells(), free - 1);
    page.links.release_cell(id);
    page.links.release(id);
    assert_eq!(page.links.available_cells(), free);
}

#[test]
fn legacy_screen_json_restores_dense_links_without_the_new_capacity_field() {
    let mut terminal = Terminal::new(200, 1, 0);
    terminal.feed(b"\x1b]8;id=one;https://example.test/legacy\x07");
    terminal.feed(&[b'L'; 200]);
    terminal.feed(b"\x1b]8;;\x07");
    for compact in [false, true] {
        let mut wire = if compact {
            serde_json::to_value(terminal.screen().compact()).unwrap()
        } else {
            serde_json::to_value(terminal.screen()).unwrap()
        };
        let capacity = wire["pages"]["pages"][0]["capacity"]
            .as_object_mut()
            .unwrap();
        assert_eq!(
            capacity.len(),
            6,
            "old readers deny unknown capacity fields"
        );
        assert!(!capacity.contains_key("hyperlink_cells"));
        // A previous writer grew the set bytes to make room for 200 cell
        // links. Defaulting its omitted map field to 64 admits only 102.
        capacity.insert("hyperlink_bytes".into(), 768.into());
        let restored: Screen = serde_json::from_value(wire).unwrap();
        assert_eq!(restored.pages.pages[0].capacity.hyperlink_cells, 256);
        assert_eq!(restored.row_text(restored.row(0)), "L".repeat(200));
        for col in 0..200 {
            assert_eq!(
                restored.row(0).hyperlink(col).unwrap().uri_bytes(),
                b"https://example.test/legacy"
            );
        }
        let page = &restored.pages.pages[0];
        assert_eq!(page.link_map.len(), 200);
        page.links
            .assert_references(page.link_map.values().copied(), None);
        let again: Screen =
            serde_json::from_value(serde_json::to_value(&restored).unwrap()).unwrap();
        assert_eq!(again.pages.pages[0].link_map.len(), 200);
    }
}

#[test]
fn page_json_accepts_an_explicit_independent_zero_map() {
    let page = Page::new(
        PageCapacity {
            cols: 80,
            rows: 1,
            ..PageCapacity::default()
        },
        1,
        0,
    );
    let mut wire = serde_json::to_value(page).unwrap();
    wire["capacity"]["hyperlink_cells"] = 0.into();
    let decoded: Page = serde_json::from_value(wire).unwrap();
    assert_eq!(decoded.capacity.hyperlink_cells, 0);
    assert_eq!(decoded.capacity.hyperlink_bytes, 192);
}

#[test]
fn dense_links_survive_reflow_and_cell_copies_without_growing_the_set() {
    let mut terminal = Terminal::new(200, 2, 0);
    terminal.feed(b"\x1b]8;id=one;https://example.test/reflow\x07");
    terminal.feed(&[b'R'; 300]);
    terminal.feed(b"\x1b]8;;\x07");
    terminal.resize(150, 2);
    let screen = terminal.screen();
    assert_eq!(
        screen
            .all_rows()
            .map(|row| row.cells.iter().filter(|cell| cell.has_hyperlink()).count())
            .sum::<usize>(),
        300
    );
    for row in screen.all_rows() {
        for (col, cell) in row.cells.iter().enumerate() {
            if cell.codepoint().is_none() {
                assert!(row.hyperlink(col).is_none());
            } else {
                assert_eq!(cell.codepoint(), Some('R'));
                assert_eq!(
                    row.hyperlink(col).unwrap().uri_bytes(),
                    b"https://example.test/reflow"
                );
            }
        }
    }
    for page in &screen.pages.pages {
        assert_eq!(
            page.capacity.hyperlink_bytes,
            PageCapacity::STANDARD.hyperlink_bytes
        );
        page.links
            .assert_references(page.link_map.values().copied(), None);
    }
}

#[test]
fn legacy_or_explicit_small_hints_keep_existing_json_growth_behavior() {
    let mut terminal = Terminal::new(200, 1, 0);
    terminal.feed(b"\x1b]8;id=one;https://example.test/bounded\x07");
    terminal.feed(&[b'L'; 200]);
    terminal.feed(b"\x1b]8;;\x07");
    let original = serde_json::to_value(terminal.screen()).unwrap();
    for (bytes, cells) in [(0, None), (192, None), (192, Some(0)), (0, Some(256))] {
        let mut wire = original.clone();
        let capacity = wire["pages"]["pages"][0]["capacity"]
            .as_object_mut()
            .unwrap();
        capacity.insert("hyperlink_bytes".into(), bytes.into());
        if let Some(cells) = cells {
            capacity.insert("hyperlink_cells".into(), cells.into());
        }
        // JSON has always treated these as initial hints and allowed the
        // ordinary bounded resource-growth path while reinstalling cells.
        let restored: Screen = serde_json::from_value(wire).unwrap();
        assert_eq!(restored.row_text(restored.row(0)), "L".repeat(200));
        for col in 0..200 {
            assert_eq!(
                restored.row(0).hyperlink(col).unwrap().uri_bytes(),
                b"https://example.test/bounded"
            );
        }
        let page = &restored.pages.pages[0];
        assert_eq!(page.link_map.len(), 200);
        page.links
            .assert_references(page.link_map.values().copied(), None);
    }
}
