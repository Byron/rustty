use rustty_vt::Terminal;

#[test]
fn legacy_mapping_keeps_original_width_and_repeat_codepoint() {
    let mut terminal = Terminal::new(10, 2, 0);
    terminal.feed(b"\x1b(A\xbb");
    terminal.feed("界".as_bytes());
    terminal.feed(b"\x1b(B\x1b[b");
    let cells = &terminal.screen().rows[0].cells;
    assert_eq!(cells[0].text, " ");
    assert_eq!(cells[1].text, " ");
    assert_eq!(cells[1].width, 2);
    assert_eq!(cells[2].width, 0);
    assert_eq!(cells[3].text, "界");
    assert_eq!(cells[3].width, 2);
    terminal.feed(b"\x1b(0q\x1b(B\x1b[b");
    assert_eq!(terminal.screen().rows[0].cells[5].text, "─");
    assert_eq!(terminal.screen().rows[0].cells[6].text, "q");
    terminal.feed(b"\x1b(0\x1b%Gq");
    assert_eq!(terminal.screen().rows[0].cells[7].text, "─");
}

#[test]
fn single_shift_survives_combining_and_ignored_characters() {
    for grapheme in [false, true] {
        let mut terminal = Terminal::new(10, 2, 0);
        terminal.set_mode(true, 2027, grapheme);
        terminal.feed(b"\x1b*0A\x1bN");
        terminal.feed("\u{301}\u{fe0f}q".as_bytes());
        assert_eq!(terminal.screen().rows[0].cells[0].text, "A\u{301}");
        assert_eq!(terminal.screen().rows[0].cells[1].text, "─");
    }
    let mut terminal = Terminal::new(3, 2, 0);
    terminal.feed(b"\x1b*0ab\x1b[?7l\x1bN");
    terminal.feed("界\rq".as_bytes());
    assert_eq!(terminal.screen().rows[0].cells[0].text, "─");
}

#[test]
fn wide_spacers_consume_single_shifts() {
    let mut terminal = Terminal::new(3, 2, 0);
    terminal.feed(b"\x1b*Aab\x1bN");
    terminal.feed("界#".as_bytes());
    assert!(terminal.screen().rows[0].cells[2].spacer_head);
    assert_eq!(terminal.screen().rows[1].cells[0].text, "界");
    assert_eq!(terminal.screen().rows[1].cells[2].text, "#");

    let mut terminal = Terminal::new(10, 2, 0);
    terminal.set_mode(true, 2027, true);
    terminal.feed(b"\x1b*A#\x1bN");
    terminal.feed("\u{fe0f}#".as_bytes());
    assert_eq!(terminal.screen().rows[0].cells[0].width, 2);
    assert_eq!(terminal.screen().rows[0].cells[2].text, "#");
}

#[test]
fn wrapped_existing_grapheme_maps_the_base_without_consuming_a_spacer_shift() {
    let mut terminal = Terminal::new(3, 3, 0);
    terminal.feed(b"\x1b[?2027h\x1b*A");
    terminal.feed("ab☀\u{200d}".as_bytes());
    terminal.feed(b"\x1bN");
    terminal.feed("😀#".as_bytes());
    assert!(terminal.screen().rows[0].cells[2].spacer_head);
    assert_eq!(terminal.screen().rows[1].cells[0].text, " \u{200d}😀");
    assert_eq!(terminal.screen().rows[1].cells[0].width, 2);
    assert_eq!(terminal.screen().rows[1].cells[2].text, "#");
}
