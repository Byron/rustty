use rustty_vt::Terminal;

#[test]
fn legacy_mapping_keeps_original_width_and_repeat_codepoint() {
    let mut terminal = Terminal::new(10, 2, 0);
    terminal.feed(b"\x1b(A\xbb");
    terminal.feed("界".as_bytes());
    terminal.feed(b"\x1b(B\x1b[b");
    let cells = &terminal.screen().row(0).cells;
    assert_eq!(
        &*terminal.screen().cell_text(terminal.screen().row(0), 0),
        "�"
    );
    assert_eq!(
        &*terminal.screen().cell_text(terminal.screen().row(0), 1),
        "界"
    );
    assert_eq!(cells[1].width(), 2);
    assert_eq!(cells[2].width(), 0);
    assert_eq!(
        &*terminal.screen().cell_text(terminal.screen().row(0), 3),
        "界"
    );
    assert_eq!(cells[3].width(), 2);
    terminal.feed(b"\x1b(0q\x1b(B\x1b[b");
    assert_eq!(
        &*terminal.screen().cell_text(terminal.screen().row(0), 5),
        "─"
    );
    assert_eq!(
        &*terminal.screen().cell_text(terminal.screen().row(0), 6),
        "q"
    );
    terminal.feed(b"\x1b(0\x1b%Gq");
    assert_eq!(
        &*terminal.screen().cell_text(terminal.screen().row(0), 7),
        "─"
    );
}

#[test]
fn single_shift_is_consumed_by_combining_and_ignored_characters() {
    for (slot, shift) in [("*", "N"), ("+", "O")] {
        for grapheme in [false, true] {
            for mark in ["\u{301}", "\u{fe0f}"] {
                let mut terminal = Terminal::new(10, 2, 0);
                terminal.set_mode(true, 2027, grapheme);
                terminal.feed(format!("\x1b{slot}0A\x1b{shift}{mark}q").as_bytes());
                assert_eq!(
                    &*terminal.screen().cell_text(terminal.screen().row(0), 0),
                    if mark == "\u{301}" { "A\u{301}" } else { "A" }
                );
                assert_eq!(
                    &*terminal.screen().cell_text(terminal.screen().row(0), 1),
                    "q"
                );
            }
        }
    }
    let mut terminal = Terminal::new(3, 2, 0);
    terminal.feed(b"\x1b*0ab\x1b[?7l\x1bN");
    terminal.feed("界\rq".as_bytes());
    assert_eq!(
        &*terminal.screen().cell_text(terminal.screen().row(0), 0),
        "q"
    );
}

#[test]
fn wide_character_consumes_single_shift_before_spacer_writes() {
    let mut terminal = Terminal::new(3, 2, 0);
    terminal.feed(b"\x1b*Aab\x1bN");
    terminal.feed("界#".as_bytes());
    assert!(terminal.screen().row(0).cells[2].spacer_head());
    assert_eq!(
        &*terminal.screen().cell_text(terminal.screen().row(1), 0),
        "界"
    );
    assert_eq!(
        &*terminal.screen().cell_text(terminal.screen().row(1), 2),
        "#"
    );

    let mut terminal = Terminal::new(10, 2, 0);
    terminal.set_mode(true, 2027, true);
    terminal.feed(b"\x1b*A#\x1bN");
    terminal.feed("\u{fe0f}#".as_bytes());
    assert_eq!(terminal.screen().row(0).cells[0].width(), 2);
    assert_eq!(
        &*terminal.screen().cell_text(terminal.screen().row(0), 2),
        "#"
    );
}

#[test]
fn wrapped_existing_grapheme_keeps_its_original_base() {
    let mut terminal = Terminal::new(3, 3, 0);
    terminal.feed(b"\x1b[?2027h\x1b*A");
    terminal.feed("ab☀\u{200d}".as_bytes());
    terminal.feed(b"\x1bN");
    terminal.feed("😀#".as_bytes());
    assert!(terminal.screen().row(0).cells[2].spacer_head());
    assert_eq!(
        &*terminal.screen().cell_text(terminal.screen().row(1), 0),
        "☀\u{200d}😀"
    );
    assert_eq!(terminal.screen().row(1).cells[0].width(), 2);
    assert_eq!(
        &*terminal.screen().cell_text(terminal.screen().row(1), 2),
        "#"
    );
}

#[test]
fn legacy_charsets_leave_unicode_above_their_range_unmapped() {
    for (set, before, after) in [("0", "◆", "▒"), ("A", "`", "a")] {
        let mut terminal = Terminal::new(10, 2, 0);
        terminal.feed(format!("\x1b({set}`😀aĀ界").as_bytes());
        let screen = terminal.screen();
        let row = screen.row(0);
        for (col, text) in [(0, before), (1, "😀"), (3, after), (4, "Ā"), (5, "界")] {
            assert_eq!(&*screen.cell_text(row, col), text);
        }
        assert_eq!(row.cells[1].width(), 2);
        assert_eq!(row.cells[2].width(), 0);
        assert_eq!(row.cells[5].width(), 2);
        assert_eq!(screen.cursor.col, 7);
    }
}

#[test]
fn widening_a_wrapped_character_does_not_apply_a_new_charset() {
    let mut terminal = Terminal::new(3, 3, 0);
    terminal.feed(b"\x1b[?2027h\x1b[3G#\x1b(A");
    terminal.feed("\u{fe0f}".as_bytes());
    let screen = terminal.screen();
    assert!(screen.row(0).cells[2].spacer_head());
    assert_eq!(&*screen.cell_text(screen.row(1), 0), "#\u{fe0f}");
    assert_eq!(screen.row(1).cells[0].width(), 2);
}

#[test]
fn legacy_charset_batches_match_scalar_writes_across_styles_and_boundaries() {
    let bytes: Vec<_> = (b' '..=b'~').cycle().take(1025).collect();
    for cols in [1, 5, 17, 128] {
        for set in ['B', 'A', '0'] {
            for graphemes in [false, true] {
                let mut batched = Terminal::new(cols, 3, 64);
                batched.set_mode(true, 2027, graphemes);
                batched.feed("界a\u{301}\r\x1b[44m\x1b[2J\x1b[1;31m".as_bytes());
                batched.feed(format!("\x1b({set}").as_bytes());
                let mut scalar = batched.clone();
                for style in [b"\x1b[1;31m".as_slice(), b"\x1b[0m", b"\x1b[0m"] {
                    batched.feed(style);
                    scalar.feed(style);
                    batched.feed(&bytes);
                    for &byte in &bytes {
                        scalar.print(char::from(byte));
                    }
                    assert_eq!(
                        rustty_vt::snapshot::encode_to_vec(&batched).unwrap(),
                        rustty_vt::snapshot::encode_to_vec(&scalar).unwrap(),
                        "cols={cols}, set={set}, graphemes={graphemes}"
                    );
                }
            }
        }
    }
}
