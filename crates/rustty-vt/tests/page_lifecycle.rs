// These regression constants are measured from the native macOS ARM64 ABI.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use rustty_vt::{PageAllocationInfo, ScrollbackLimits, Terminal, snapshot};

fn pages(terminal: &Terminal) -> Vec<PageAllocationInfo> {
    let pages: Vec<_> = terminal.screen().page_allocations().collect();
    assert_eq!(
        pages
            .iter()
            .map(|page| usize::from(page.rows))
            .sum::<usize>(),
        terminal.screen().all_rows().count()
    );
    assert_eq!(
        pages
            .iter()
            .map(|page| page.allocation_bytes)
            .sum::<usize>(),
        terminal.screen().storage_bytes()
    );
    pages
}

#[test]
fn tail_growth_and_byte_recycling_keep_whole_native_pages() {
    let mut terminal = Terminal::with_limits(80, 24, ScrollbackLimits::default());
    let initial = pages(&terminal)[0];
    assert_eq!(initial.rows, 24);
    assert_eq!(initial.capacity.rows, 591);
    assert_eq!(initial.allocation_bytes, 409_600);
    assert!(initial.pooled);

    let capacity = usize::from(initial.capacity.rows);
    terminal.feed(&b"x\r\n".repeat(capacity - 1));
    assert_eq!(pages(&terminal).len(), 1);
    assert_eq!(pages(&terminal)[0].rows, initial.capacity.rows);
    terminal.feed(b"x\r\n");
    assert_eq!(
        pages(&terminal)
            .iter()
            .map(|page| page.rows)
            .collect::<Vec<_>>(),
        [591, 1]
    );

    terminal.set_limits(ScrollbackLimits {
        bytes: Some(1),
        lines: None,
    });
    terminal.feed(&b"x\r\n".repeat(capacity));
    assert_eq!(
        pages(&terminal)
            .iter()
            .map(|page| page.rows)
            .collect::<Vec<_>>(),
        [591, 1]
    );
    assert_eq!(terminal.screen().history_len(), capacity + 1 - 24);
    assert_eq!(
        terminal.screen().storage_bytes(),
        initial.allocation_bytes * 2
    );
}

#[test]
fn line_pruning_and_history_clear_preserve_the_active_boundary_page() {
    let mut terminal = Terminal::with_limits(80, 24, ScrollbackLimits::default());
    terminal.feed(&b"x\r\n".repeat(591 + 6));
    assert_eq!(
        pages(&terminal)
            .iter()
            .map(|page| page.rows)
            .collect::<Vec<_>>(),
        [591, 7]
    );
    terminal.feed(b"\x1b[3J");
    let cleared = pages(&terminal);
    assert!(terminal.screen().history().next().is_none());
    assert_eq!(
        cleared.iter().map(|page| page.rows).collect::<Vec<_>>(),
        [17, 7]
    );
    assert!(cleared.iter().all(|page| page.capacity.rows == 591));

    let mut terminal = Terminal::with_limits(80, 24, ScrollbackLimits::default());
    terminal.feed(&b"x\r\n".repeat(591 * 3 - 1));
    terminal.set_limits(ScrollbackLimits {
        bytes: None,
        lines: Some(0),
    });
    assert_eq!(
        pages(&terminal)
            .iter()
            .map(|page| page.rows)
            .collect::<Vec<_>>(),
        [591]
    );
    terminal.feed(&b"x\r\n".repeat(24));
    assert_eq!(
        pages(&terminal)
            .iter()
            .map(|page| page.rows)
            .collect::<Vec<_>>(),
        [591, 24]
    );
    terminal.feed(b"x\r\n");
    assert_eq!(
        pages(&terminal)
            .iter()
            .map(|page| page.rows)
            .collect::<Vec<_>>(),
        [25]
    );
}

#[test]
fn snapshot_keeps_page_groups_and_charges_restored_pages_at_pool_size() {
    let mut terminal = Terminal::with_limits(80, 24, ScrollbackLimits::default());
    terminal.feed(&b"x\r\n".repeat(591 * 2 + 6));
    let encoded = snapshot::encode_to_vec(&terminal).unwrap();
    let mut decoder =
        snapshot::Decoder::new(encoded.as_slice(), snapshot::DecodeOptions::default());
    let mut restored = decoder.ready().unwrap();
    let active = pages(&restored);
    assert_eq!(
        active.iter().map(|page| page.rows).collect::<Vec<_>>(),
        [591, 7]
    );
    // The last SCREEN page restores its used size as capacity. Its allocation
    // is still a full native pool item, even though the layout is much smaller.
    assert_eq!(active[1].capacity.rows, 7);
    assert_eq!(active[1].allocation_bytes, 409_600);
    assert_eq!(restored.screen().history_len(), 591 + 7 - 24);
    assert_eq!(
        decoder.next_history(&mut restored).unwrap().unwrap().rows,
        591
    );
    assert!(decoder.next_history(&mut restored).unwrap().is_none());
    assert_eq!(
        pages(&restored)
            .iter()
            .map(|page| page.rows)
            .collect::<Vec<_>>(),
        [591, 591, 7]
    );
    assert_eq!(
        restored
            .screen()
            .all_rows()
            .map(|row| restored.screen().row_text(row))
            .collect::<Vec<_>>(),
        terminal
            .screen()
            .all_rows()
            .map(|row| terminal.screen().row_text(row))
            .collect::<Vec<_>>()
    );
    restored.feed(b"x\r\n");
    let grown = pages(&restored);
    assert_eq!(
        grown.iter().map(|page| page.rows).collect::<Vec<_>>(),
        [591, 591, 7, 1]
    );
    assert_eq!(grown[3].capacity.rows, 591);
}
