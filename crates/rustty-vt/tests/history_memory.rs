use rustty_vt::{ScrollbackLimits, Selection, Terminal, snapshot};

fn bounded(terminal: &Terminal, limit: usize) {
    let screen = terminal.screen();
    assert!(screen.history_bytes() <= limit);
    assert_eq!(
        screen
            .page_allocations()
            .map(|page| usize::from(page.rows))
            .sum::<usize>(),
        screen.history_len() + screen.height(),
    );
    assert!(screen.viewport_offset <= screen.history_len());
}

#[test]
fn owned_history_budget_evicts_whole_pages_and_invalidates_pins() {
    let mut terminal = Terminal::with_limits(80, 2, ScrollbackLimits::default());
    let page_rows = usize::from(
        terminal
            .screen()
            .page_allocations()
            .next()
            .unwrap()
            .capacity
            .rows,
    );
    terminal.feed(&b"1234567\r\n".repeat(page_rows + 1));
    let limit = terminal.screen().history_bytes();
    assert!(limit > 0);
    terminal.screen_mut().scroll_viewport(page_rows as isize);
    let point = terminal.screen().point(0, 0).unwrap();
    let pin = terminal.screen_mut().track(point);
    terminal.screen_mut().selection = Some(Selection {
        start: point,
        end: point,
        rectangular: false,
    });
    terminal.set_scrollback_memory_limit(Some(limit));
    let mut evictions = 0;
    for _ in 0..page_rows * 3 {
        let before = terminal.screen().history_len();
        terminal.feed(b"1234567\r\n");
        bounded(&terminal, limit);
        let after = terminal.screen().history_len();
        if after < before {
            assert_eq!(before + 1 - after, page_rows);
            evictions += 1;
        }
    }
    assert!(evictions >= 2);
    assert_eq!(terminal.screen().resolve(pin), None);
    assert_eq!(terminal.screen().selection, None);

    // A page containing a long opaque URI has a larger host charge. Once it
    // becomes fully historical it must be evicted under the plain-page cap.
    let mut link = b"\x1b]8;id=".to_vec();
    link.extend_from_slice(&vec![b'i'; 256]);
    link.extend_from_slice(b";https://example.org/");
    link.extend_from_slice(&vec![b'x'; 4096]);
    link.extend_from_slice(b"\xff\x07A\x1b]8;;\x07");
    terminal.feed(&link);
    terminal.feed(&b"\r\nx".repeat(page_rows * 2));
    bounded(&terminal, limit);
    assert!(
        terminal
            .screen()
            .history()
            .all(|row| row.cells.iter().all(|cell| !cell.has_hyperlink()))
    );

    let active = serde_json::to_value(terminal.screen()).unwrap()["rows"].clone();
    terminal.set_scrollback_memory_limit(Some(0));
    bounded(&terminal, 0);
    assert_eq!(terminal.screen().history_len(), 0);
    assert_eq!(
        serde_json::to_value(terminal.screen()).unwrap()["rows"],
        active
    );
    terminal.feed(&b"\r\n1234567".repeat(page_rows));
    assert_eq!(terminal.screen().history_len(), 0);
    terminal.set_scrollback_memory_limit(None);
    terminal.feed(&b"\r\n1234567".repeat(page_rows * 3));
    assert!(terminal.screen().history_bytes() > limit);
}

#[test]
fn active_page_capacity_is_the_minimum_allowance() {
    let mut terminal = Terminal::with_limits(80, 2, ScrollbackLimits::default());
    terminal.set_scrollback_memory_limit(Some(1));
    terminal.feed(b"a\r\nb\r\nc");
    assert_eq!(terminal.screen().history_len(), 1);
    assert_eq!(terminal.screen().history_bytes(), 0);
    assert!(terminal.screen().owned_bytes() > 1);
    let page_rows = usize::from(
        terminal
            .screen()
            .page_allocations()
            .next()
            .unwrap()
            .capacity
            .rows,
    );
    for _ in 0..page_rows * 4 {
        terminal.feed(b"\r\na");
        bounded(&terminal, 1);
        assert!(terminal.screen().page_allocations().count() <= 2);
        assert!(terminal.screen().history_len() < page_rows);
    }
}

#[test]
fn owned_history_budget_survives_resize_reset_and_alternate_screen() {
    let limit = 24 * 1024;
    let mut terminal = Terminal::new(80, 4, 1000);
    terminal.set_scrollback_memory_limit(Some(limit));
    terminal.feed(&b"1234567\r\n".repeat(80));
    for (cols, rows) in [(8, 2), (160, 8), (80, 2)] {
        terminal.resize(cols, rows);
        bounded(&terminal, limit);
        terminal.feed(&b"1234567\r\n".repeat(80));
        bounded(&terminal, limit);
    }
    terminal.feed(b"\x1bc");
    terminal.feed(&b"1234567\r\n".repeat(80));
    bounded(&terminal, limit);

    // ED22 can retain history even on the otherwise history-free alternate screen.
    terminal.feed(b"\x1b[?47h");
    for _ in 0..20 {
        terminal.feed(b"alternate\x1b[22J");
        bounded(&terminal, limit);
    }
    terminal.set_scrollback_memory_limit(Some(0));
    bounded(&terminal, 0);
    terminal.feed(b"\x1b[?47l");
    bounded(&terminal, 0);
}

#[test]
fn pruning_a_wrapped_graphemes_source_preserves_active_text() {
    for limit in [0, 1] {
        let mut terminal = Terminal::new(3, 1, 1000);
        terminal.set_scrollback_memory_limit(Some(limit));
        terminal.feed("\x1b[?2027hab☀\u{200d}😀".as_bytes());
        bounded(&terminal, limit);
        let cell = &terminal.screen().row(0).cells[0];
        assert_eq!(
            &*terminal.screen().cell_text(&terminal.screen().row(0), 0),
            "☀\u{200d}😀"
        );
        assert_eq!(cell.width(), 2);
    }
}

#[test]
fn streamed_history_respects_the_hosts_owned_memory_budget() {
    let mut source = Terminal::new(80, 4, 2000);
    source.feed(&b"history\r\n".repeat(1400));
    let wire = snapshot::encode_to_vec(&source).unwrap();
    let mut decoder = snapshot::Decoder::new(wire.as_slice(), Default::default());
    let mut terminal = decoder.ready().unwrap();
    let limit = 24 * 1024;
    terminal.set_scrollback_memory_limit(Some(limit));
    bounded(&terminal, limit);
    let mut pages = 0;
    while let Some(progress) = decoder.next_history(&mut terminal).unwrap() {
        pages += 1;
        assert_eq!(
            progress.rows, 0,
            "an older native page exceeds the owned byte cap"
        );
        bounded(&terminal, limit);
    }
    assert!(pages > 0);
}
