use super::*;
use crate::{
    Color, ScrollbackLimits, Style, Terminal, page_list::PageList, page_resources::HyperlinkKey,
};

fn add_styles(page: &mut Page, count: usize) {
    for index in 0..count {
        page.styles
            .acquire(Style {
                foreground: Color::Rgb(index as u8, (index >> 8) as u8, 1),
                ..Style::default()
            })
            .unwrap();
    }
}

fn advance(
    pages: &mut PageList,
    columns: u16,
    active: usize,
    limits: ScrollbackLimits,
    memory: Option<usize>,
) -> Vec<u64> {
    while pages.pages.back().unwrap().rows < pages.pages.back().unwrap().capacity.rows {
        pages.grow(columns, active, limits, memory, 0, Color::Default);
    }
    pages.grow(columns, active, limits, memory, 0, Color::Default)
}

#[test]
fn new_pages_carry_live_style_capacity() {
    // Regression-first: this test and its helpers use only pre-port APIs.
    let mut pages = PageList::new(80, 24);
    let page = pages.pages.back_mut().unwrap();
    page.rebuild(Some(PageResource::Styles)).unwrap();
    page.rebuild(Some(PageResource::Styles)).unwrap();
    let styles = page.capacity.styles;
    let count = page.capacity.metadata().unwrap().styles_layout.cap / 2;
    add_styles(page, count);
    advance(&mut pages, 80, 24, ScrollbackLimits::default(), None);
    assert_eq!(pages.pages.back().unwrap().capacity.styles, styles);
}

#[test]
fn three_previous_pages_survive_light_use_but_two_empty_pages_drop_a_burst() {
    let mut pages = PageList::new(80, 24);
    let page = pages.pages.back_mut().unwrap();
    page.rebuild(Some(PageResource::Styles)).unwrap();
    page.rebuild(Some(PageResource::Styles)).unwrap();
    let styles = page.capacity.styles;
    add_styles(page, 200);
    advance(&mut pages, 80, 24, ScrollbackLimits::default(), None);
    for _ in 0..2 {
        assert_eq!(pages.pages.back().unwrap().capacity.styles, styles);
        add_styles(pages.pages.back_mut().unwrap(), 12);
        advance(&mut pages, 80, 24, ScrollbackLimits::default(), None);
    }
    assert_eq!(pages.pages.back().unwrap().capacity.styles, styles);
    advance(&mut pages, 80, 24, ScrollbackLimits::default(), None);
    assert_eq!(pages.pages.back().unwrap().capacity.styles, styles);
    advance(&mut pages, 80, 24, ScrollbackLimits::default(), None);
    assert_eq!(
        pages.pages.back().unwrap().capacity,
        PageCapacity::initial(80).unwrap()
    );
}

#[test]
fn history_ages_each_resource_independently_and_next_is_read_only() {
    let standard = PageCapacity::initial(80).unwrap();
    let mut hints = GrowHints::default();
    let mut heavy = standard;
    heavy.styles *= 4;
    heavy.grapheme_bytes *= 4;
    hints.push(GrowHint {
        need: heavy,
        used: (1 << PageResource::Styles as u8) | (1 << PageResource::Graphemes as u8),
    });
    hints.push(GrowHint {
        need: standard,
        used: 1 << PageResource::Graphemes as u8,
    });
    let page = Page::new(standard, standard.rows, 0);
    let recent = hints.recent;
    let next = hints.next;
    let planned = hints.next(80, &page);
    assert_eq!(planned.capacity.styles, standard.styles);
    assert_eq!(planned.capacity.grapheme_bytes, heavy.grapheme_bytes);
    assert_eq!(hints.recent, recent);
    assert_eq!(hints.next, next);
    let retry = hints.next(80, &page);
    assert_eq!(planned.capacity, retry.capacity);
    assert_eq!(planned.hint, retry.hint);
    hints.push(planned.hint);
    assert_ne!(hints.next, next);
}

#[test]
fn trimmed_pages_measure_style_and_string_demand_per_standard_row() {
    let standard = PageCapacity::initial(80).unwrap();
    let big = PageCapacity {
        styles: 1024,
        string_bytes: 65_536,
        ..standard
    };
    let mut capacity = big;
    let mut hints = GrowHints::default();
    for index in 0..4 {
        let mut page = Page::new(capacity, capacity.rows, index);
        let usage = page.usage();
        add_styles(
            &mut page,
            (usage.styles.capacity * 4 / 10 * u64::from(capacity.rows) / u64::from(standard.rows))
                as usize,
        );
        let bytes =
            usage.strings.capacity * 4 / 10 * u64::from(capacity.rows) / u64::from(standard.rows);
        page.links
            .insert(HyperlinkKey::new(&vec![b'u'; bytes as usize], None, 1))
            .unwrap();
        let growth = hints.next(80, &page);
        // Check uncarried demand as well; remembered heavy pages must not hide
        // a shrink caused by treating a shorter page as less dense content.
        assert_eq!(growth.hint.need.styles, big.styles);
        assert_eq!(growth.hint.need.string_bytes, big.string_bytes);
        assert!(growth.capacity.rows < standard.rows);
        hints.push(growth.hint);
        capacity = growth.capacity;
    }
}

#[test]
fn underused_resources_shrink_to_scaled_headroom_and_keep_standard_floors() {
    let standard = PageCapacity::initial(80).unwrap();
    let capacity = PageCapacity {
        styles: 2048,
        string_bytes: 32_768,
        ..standard
    };
    let mut page = Page::new(capacity, capacity.rows, 0);
    add_styles(
        &mut page,
        capacity.metadata().unwrap().styles_layout.cap / 4,
    );
    page.links
        .insert(HyperlinkKey::new(&vec![b'u'; 9800], None, 1))
        .unwrap();
    let usage = page.usage();
    let growth = GrowHints::default().next(80, &page);
    assert_eq!(
        u64::from(growth.hint.need.styles),
        set_capacity_for_count(usage.styles.used * 3 / 2)
    );
    assert_eq!(
        u64::from(growth.hint.need.string_bytes),
        usage.strings.used * 3 / 2
    );
    assert!(growth.hint.need.styles > standard.styles && growth.hint.need.styles < capacity.styles);
    assert!(
        growth.hint.need.string_bytes > standard.string_bytes
            && growth.hint.need.string_bytes < capacity.string_bytes
    );

    let empty = Page::new(capacity, capacity.rows, 0);
    assert_eq!(GrowHints::default().next(80, &empty).hint.need, standard);
}

#[test]
fn a_light_link_page_carries_at_least_32_unique_links_when_shrinking() {
    let standard = PageCapacity::initial(80).unwrap();
    let capacity = PageCapacity {
        hyperlink_bytes: 6144,
        ..standard
    };
    let mut page = Page::new(capacity, capacity.rows, 0);
    page.links
        .insert(HyperlinkKey::new(b"https://example.test", None, 1))
        .unwrap();
    let need = GrowHints::default().next(80, &page).hint.need;
    assert_eq!(
        u64::from(need.hyperlink_bytes),
        hyperlink_bytes_for_count(32)
    );
    assert_eq!(need.hyperlink_cells, standard.hyperlink_cells);
}

#[test]
fn pool_sized_fit_keeps_90_percent_density_or_retains_full_rows() {
    let standard = PageCapacity::initial(80).unwrap();
    let standard_bytes = PageCapacity::STANDARD.layout().unwrap().total_size;
    let mut saw_trim = false;
    let mut saw_full = false;
    for shift in 1..=8 {
        let capacity = PageCapacity {
            styles: standard.styles << shift,
            ..standard
        };
        let mut page = Page::new(capacity, capacity.rows, 0);
        add_styles(
            &mut page,
            capacity.metadata().unwrap().styles_layout.cap / 2,
        );
        let heap_bytes = capacity.layout().unwrap().total_size;
        if heap_bytes <= standard_bytes {
            continue;
        }
        let result = GrowHints::default().next(80, &page).capacity;
        assert_eq!(result.styles, capacity.styles);
        let fitted = capacity.fit_rows(standard_bytes);
        // Independent floating-point witness for the integer cross-product.
        let may_trim = fitted.is_some_and(|fit| {
            f64::from(fit.rows) / standard_bytes as f64
                >= 0.9 * f64::from(capacity.rows) / heap_bytes as f64
        });
        if may_trim {
            saw_trim = true;
            assert_eq!(Some(result), fitted);
            assert!(result.rows < capacity.rows);
        } else {
            saw_full = true;
            assert_eq!(result, capacity);
            break;
        }
    }
    assert!(saw_trim && saw_full);
}

#[test]
fn fit_rows_handles_empty_one_row_and_u16_row_limits() {
    let capacity = PageCapacity::STANDARD;
    let bytes = capacity.layout().unwrap().total_size;
    let fitted = capacity.fit_rows(bytes).unwrap();
    assert!(fitted.rows >= capacity.rows);
    assert!(fitted.layout().unwrap().total_size <= bytes);
    assert!(
        PageCapacity {
            rows: fitted.rows + 1,
            ..fitted
        }
        .layout()
        .unwrap()
        .total_size
            > bytes
    );
    assert_eq!(capacity.fit_rows(0), None);
    assert_eq!(
        capacity.fit_rows(capacity.metadata().unwrap().total_size),
        None
    );
    let single = PageCapacity {
        cols: 65_535,
        rows: 1,
        ..capacity
    };
    assert_eq!(
        single
            .fit_rows(single.layout().unwrap().total_size)
            .unwrap()
            .rows,
        1
    );
    let tall = PageCapacity {
        cols: 1,
        rows: u16::MAX,
        ..capacity
    };
    assert_eq!(
        tall.fit_rows(tall.layout().unwrap().total_size)
            .unwrap()
            .rows,
        u16::MAX
    );
    let narrow = PageCapacity::initial(20).unwrap();
    let mut hints = GrowHints::default();
    hints.push(GrowHint {
        need: PageCapacity {
            styles: 129,
            ..narrow
        },
        used: 1 << PageResource::Styles as u8,
    });
    let page = Page::new(narrow, narrow.rows, 0);
    assert_eq!(
        hints.next(20, &page).capacity,
        PageCapacity {
            styles: 129,
            ..narrow
        }
        .fit_rows(bytes)
        .unwrap()
    );
}

#[test]
fn fit_rows_preserves_aligned_grid_boundaries_with_independent_link_storage() {
    let standard = PageCapacity::initial(80).unwrap();
    for hyperlink_bytes in [0, 48, 192, 240, u16::MAX] {
        for hyperlink_cells in [0, 1, 63, 64, 65, 1024] {
            let original = PageCapacity {
                hyperlink_bytes,
                hyperlink_cells,
                ..standard
            };
            let bytes = original.layout().unwrap().total_size;
            for cols in [47_969, 47_970, 47_971, 47_972] {
                let request = PageCapacity { cols, ..original };
                match request.fit_rows(bytes) {
                    Some(fitted) => {
                        assert_eq!(original.adjust_columns(cols).unwrap(), fitted);
                        assert!(fitted.layout().unwrap().total_size <= bytes);
                        assert!(
                            PageCapacity {
                                rows: fitted.rows + 1,
                                ..fitted
                            }
                            .layout()
                            .unwrap()
                            .total_size
                                > bytes
                        );
                    }
                    None => {
                        assert!(original.adjust_columns(cols).is_err());
                        assert!(
                            PageCapacity { rows: 1, ..request }
                                .layout()
                                .unwrap()
                                .total_size
                                > bytes
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn impossible_carried_metadata_falls_back_without_committing_history() {
    let standard = PageCapacity::initial(80).unwrap();
    let mut hints = GrowHints::default();
    hints.push(GrowHint {
        need: PageCapacity {
            grapheme_bytes: u32::MAX,
            ..standard
        },
        used: 1 << PageResource::Graphemes as u8,
    });
    let recent = hints.recent;
    let page = Page::new(standard, standard.rows, 0);
    assert_eq!(hints.next(80, &page).capacity, standard);
    assert_eq!(hints.recent, recent);
}

#[test]
fn growing_large_carried_pages_enforces_bytes_and_owned_history_budget() {
    let standard = PageCapacity::initial(80).unwrap();
    let standard_bytes = PageCapacity::STANDARD.layout().unwrap().total_size;
    let limit = 8 * standard_bytes;
    let limits = ScrollbackLimits {
        bytes: Some(limit),
        lines: None,
    };
    let mut pages = PageList::new(80, 24);
    while pages.pages.len() < 8 {
        advance(&mut pages, 80, 24, limits, None);
    }
    let page = pages.pages.back_mut().unwrap();
    page.rows = page.capacity.rows;
    for _ in 0..7 {
        page.rebuild(Some(PageResource::Styles)).unwrap();
    }
    let count = page.capacity.metadata().unwrap().styles_layout.cap / 2;
    add_styles(page, count);
    assert!(pages.allocation_bytes() > limit);
    let removed = advance(&mut pages, 80, 24, limits, None);
    assert!(removed.len() > usize::from(standard.rows));
    assert!(pages.allocation_bytes() <= limit);
    assert!(pages.total_rows() >= 24);
    assert!(pages.pages.back().unwrap().capacity.styles > standard.styles);
    for _ in 0..5 {
        advance(&mut pages, 80, 24, limits, Some(1));
    }
    assert!(pages.owned_history_bytes(24) <= 1);
    assert!(pages.total_rows() >= 24);
}

#[test]
fn a_tall_styled_burst_returns_to_the_byte_floor() {
    let limits = ScrollbackLimits {
        bytes: Some(0),
        lines: None,
    };
    let mut pages = PageList::new(215, 215);
    let page = pages.pages.back_mut().unwrap();
    for _ in 0..3 {
        page.rebuild(Some(PageResource::Styles)).unwrap();
    }
    let count = page.capacity.metadata().unwrap().styles_layout.cap / 2;
    add_styles(page, count);
    let carried = GrowHints::default().next(215, page).capacity;
    assert!(
        carried.rows < 215,
        "exercise trimmed pages in the active area"
    );
    for _ in 0..10 {
        advance(&mut pages, 215, 215, limits, None);
    }
    assert_eq!(
        pages.pages.back().unwrap().capacity,
        PageCapacity::initial(215).unwrap()
    );
    assert!(
        pages.allocation_bytes() <= PageList::effective_limits(215, 215, limits).bytes.unwrap()
    );
    assert!(pages.total_rows() >= 215);
}

#[test]
fn clones_restores_reflow_and_reset_start_fresh_hint_history() {
    let mut terminal = Terminal::new(80, 2, 100_000);
    for _ in 0..PageCapacity::initial(80).unwrap().rows {
        terminal.feed(b"line\r\n");
    }
    assert!(
        terminal
            .screen()
            .pages
            .grow_hints
            .recent
            .iter()
            .any(Option::is_some)
    );
    let copy = terminal.clone();
    assert_eq!(copy.screen().pages.grow_hints, GrowHints::default());
    let json = serde_json::to_value(terminal.screen()).unwrap();
    assert!(json["pages"].get("grow_hints").is_none());
    let restored: crate::Screen = serde_json::from_value(json).unwrap();
    assert_eq!(restored.pages.grow_hints, GrowHints::default());
    let restored = crate::snapshot::decode_exact(
        crate::snapshot::encode_to_vec(&terminal)
            .unwrap()
            .as_slice(),
        Default::default(),
    )
    .unwrap();
    assert_eq!(restored.screen().pages.grow_hints, GrowHints::default());
    terminal.resize(79, 2);
    assert_eq!(terminal.screen().pages.grow_hints, GrowHints::default());
    terminal.reset();
    assert_eq!(terminal.screen().pages.grow_hints, GrowHints::default());
}

#[test]
fn one_unique_link_per_row_projects_each_hyperlink_resource_once() {
    let standard = PageCapacity::initial(80).unwrap();
    let mut pages = PageList::new(80, 1);
    let mut grows = [0usize; 5];
    for y in 0..usize::from(standard.rows) {
        if y > 0 {
            pages.grow(
                80,
                1,
                ScrollbackLimits::default(),
                None,
                y as u64,
                Color::Default,
            );
        }
        let uri = format!("http://example.com/{y}");
        let key = HyperlinkKey::new(uri.as_bytes(), None, y as u32);
        for col in 0..8 {
            loop {
                let page = pages.pages.back_mut().unwrap();
                match page.links.copy_cell(key, 0) {
                    Ok(id) => {
                        let slot = page.slot(y, col);
                        page.link_map.insert(slot as u32, id);
                        page.cells[slot].set_codepoint(Some('L'));
                        page.cells[slot].set_hyperlink(true);
                        page.mark_cell(y, page.cells[slot]);
                        break;
                    }
                    Err(error) => {
                        let resource = PageResource::for_link(error);
                        if let Some(resource) = resource {
                            grows[resource as usize] += 1;
                        }
                        page.rebuild(resource).unwrap();
                    }
                }
            }
        }
    }
    assert_eq!(pages.pages.len(), 1);
    let page = pages.pages.back().unwrap();
    assert_eq!(page.links.iter().count(), usize::from(standard.rows));
    assert_eq!(page.link_map.len(), usize::from(standard.rows) * 8);
    assert_eq!(grows[PageResource::Links as usize], 1);
    assert_eq!(grows[PageResource::LinkCells as usize], 1);
    assert_eq!(grows[PageResource::Strings as usize], 1);
    assert_eq!(grows[PageResource::Styles as usize], 0);
    assert_eq!(grows[PageResource::Graphemes as usize], 0);
    page.links
        .assert_references(page.link_map.values().copied(), None);
}

#[test]
fn resource_usage_counts_live_values_and_retained_string_storage() {
    let capacity = PageCapacity {
        cols: 10,
        rows: 10,
        ..PageCapacity::default()
    };
    let mut page = Page::new(capacity, capacity.rows, 0);
    let empty = page.usage();
    for resource in PageResource::ALL {
        assert_eq!(resource.usage(empty).used, 0);
    }
    let style = page
        .styles
        .acquire(Style {
            bold: true,
            ..Style::default()
        })
        .unwrap();
    let grapheme = page.graphemes.acquire(64).unwrap();
    let link = page
        .links
        .copy_cell(HyperlinkKey::new(b"https://example.test", None, 1), 0)
        .unwrap();
    page.link_map.insert(0, link);
    let usage = page.usage();
    assert_eq!(usage.styles.used, 1);
    assert_eq!(usage.graphemes.used, 256);
    assert_eq!(usage.links.used, 1);
    assert_eq!(usage.link_cells.used, 1);
    assert_eq!(usage.strings.used, 32);
    page.styles.release(style);
    page.graphemes.release(grapheme);
    page.link_map.clear();
    page.links.release_cell(link);
    let dead = page.usage();
    assert_eq!(dead.links.used, 0);
    assert_eq!(dead.link_cells.used, 0);
    assert_eq!(
        dead.strings.used, 32,
        "dead set values retain strings until reclaimed"
    );
    page.recycle(capacity, 1);
    assert_eq!(page.usage(), empty);
}
