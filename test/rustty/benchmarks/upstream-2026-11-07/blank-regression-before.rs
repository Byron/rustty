
#[test]
fn repeated_blank_erases_preserve_clean_rows_and_revisions_before() {
    let mut terminal = Terminal::new(16, 4, 0);
    for page in &mut terminal.screen_mut().pages.pages {
        for header in &mut page.headers { header.set(RowHeader::DIRTY, false); }
    }
    let revisions: Vec<_> = terminal.screen().rows().map(RowView::revision).collect();
    terminal.feed(b"\x1b[2K\x1b[8X\x1b[2J\x1b[2J");
    assert_eq!(terminal.screen().rows().map(RowView::revision).collect::<Vec<_>>(), revisions);
    assert!(terminal.screen().rows().all(|row| !row.dirty));
}
