use rustty_vt::{Color, CursorShape, Effect, ScrollbackLimits, Selection, Terminal, Underline};

fn lines(t: &Terminal) -> Vec<String> {
    t.screen().rows.iter().map(|row| row.text()).collect()
}

fn invariant(t: &Terminal) {
    let screen = t.screen();
    assert_eq!(screen.rows.len(), t.rows as usize);
    assert!(screen.cursor.row < t.rows as usize);
    assert!(screen.cursor.col < t.cols as usize);
    for row in screen.all_rows() {
        assert_eq!(row.cells.len(), t.cols as usize);
        for (i, cell) in row.cells.iter().enumerate() {
            if cell.width == 0 {
                assert!(i > 0 && row.cells[i - 1].width == 2);
            }
            if cell.width == 2 {
                assert!(i + 1 < row.cells.len() && row.cells[i + 1].width == 0);
            }
            assert!(cell.text.chars().count() <= 65);
        }
    }
}

#[test]
fn wrapping_scrolling_and_tracked_references() {
    let mut t = Terminal::new(4, 2, 2);
    t.feed(b"abcd");
    assert_eq!((t.screen().cursor.row, t.screen().cursor.col), (0, 3));
    assert!(t.screen().cursor.pending_wrap);
    let point = t.screen().point(0, 1).unwrap();
    let pin = t.screen_mut().track(point);
    t.feed(b"efghijkl");
    assert_eq!(lines(&t), ["efgh", "ijkl"]);
    assert_eq!(t.screen().history[0].text(), "abcd");
    assert_eq!(t.screen().resolve(pin), Some(point));
    t.feed(b"mnopqrstuvwx");
    assert_eq!(t.screen().resolve(pin), Some(point));
    assert_eq!(t.screen().history.len(), 4);
    invariant(&t);
}

#[test]
fn vt_overwrites_wide_cells_as_a_unit() {
    let mut t = Terminal::new(4, 2, 10);
    t.feed("abc界".as_bytes());
    assert!(t.screen().rows[0].cells[3].spacer_head);
    assert!(t.screen().rows[0].wrapped);
    assert_eq!(t.screen().rows[1].cells[0].width, 2);
    t.feed(b"\x1b[2;2HX");
    assert!(t.screen().rows[1].cells[0].text.is_empty());
    assert_eq!(t.screen().rows[1].cells[1].text, "X");
    invariant(&t);
    t.feed("\x1b[H界\x1b[2GX".as_bytes());
    invariant(&t);
}

#[test]
fn graphemes_have_bounded_storage_and_track_width() {
    let mut t = Terminal::new(6, 2, 10);
    t.feed("a\u{301}".as_bytes());
    assert_eq!(t.screen().rows[0].cells[0].text, "a\u{301}");
    t.feed(b"\x1b[?2027h");
    t.feed("👩🏽‍🚀".as_bytes());
    assert_eq!(t.screen().rows[0].cells[1].text, "👩🏽‍🚀");
    assert_eq!(t.screen().rows[0].cells[1].width, 2);
    for _ in 0..200 {
        t.feed("\u{301}".as_bytes());
    }
    invariant(&t);
    let mut t = Terminal::new(3, 3, 10);
    t.feed("\x1b[?2027hab❤\u{fe0f}".as_bytes());
    assert_eq!(t.screen().rows[1].cells[0].text, "❤\u{fe0f}");
    assert_eq!(t.screen().rows[1].cells[0].width, 2);
    invariant(&t);
}

#[test]
fn combining_at_right_edge_respects_grapheme_and_wrap_modes() {
    for grapheme in [false, true] {
        for wrap in [false, true] {
            let mut terminal = Terminal::new(2, 2, 0);
            terminal.set_mode(true, 7, wrap);
            terminal.set_mode(true, 2027, grapheme);
            terminal.feed("ab\u{596}".as_bytes());
            let cells = &terminal.screen().rows[0].cells;
            if wrap || grapheme {
                assert_eq!(cells[0].text, "a");
                assert_eq!(cells[1].text, "b\u{596}");
            } else {
                assert_eq!(cells[0].text, "a\u{596}");
                assert_eq!(cells[1].text, "b");
            }
        }
    }
}

#[test]
fn legacy_combining_without_wrap_is_ignored_at_column_zero() {
    let mut terminal = Terminal::new(1, 2, 0);
    terminal.feed("\x1b[?7la\u{596}".as_bytes());
    assert_eq!(terminal.screen().rows[0].cells[0].text, "a");
}

#[test]
fn erase_retains_background_but_clears_other_style_and_protects_cells() {
    let mut t = Terminal::new(5, 2, 0);
    t.feed(b"\x1b[1;31;44mabc\x1b[2K");
    for c in &t.screen().rows[0].cells {
        assert_eq!(c.style.background, Color::Indexed(4));
        assert!(!c.style.bold);
        assert_eq!(c.style.foreground, Color::Default);
    }
    t.feed(b"\x1b[H\x1b[1\"qA\x1b[0\"qB\x1b[?2K");
    assert_eq!(t.screen().rows[0].cells[0].text, "A");
    assert!(t.screen().rows[0].cells[1].text.is_empty());
}

#[test]
fn alternate_screen_modes_preserve_the_primary() {
    let mut t = Terminal::new(6, 2, 10);
    t.feed(b"main\x1b[?1049h\x1b[Hother\x1b[?1049l");
    assert_eq!(lines(&t)[0], "main");
    assert_eq!(t.screen().cursor.col, 4);
    t.feed(b"\x1b[?47h");
    assert_eq!(lines(&t)[0], "other");
    t.feed(b"\x1b[?1047l\x1b[?47h");
    assert!(lines(&t).iter().all(|line| line.is_empty()));
    invariant(&t);
}

#[test]
fn margins_scroll_only_the_defined_region() {
    let mut t = Terminal::new(5, 4, 10);
    t.feed(b"one\r\ntwo\r\nthree\r\nfour");
    t.feed(b"\x1b[2;3r\x1b[3;1H\n");
    assert_eq!(lines(&t), ["one", "three", "", "four"]);
    assert!(t.screen().history.is_empty());
    t.feed(b"\x1b[?6h\x1b[1;2HX");
    assert_eq!(t.screen().rows[1].cells[1].text, "X");
    invariant(&t);
}

#[test]
fn reflow_preserves_text_selection_and_reference() {
    let mut t = Terminal::new(6, 3, 100);
    t.feed("ab界cdefgh".as_bytes());
    let start = t.screen().point(0, 0).unwrap();
    let end = t.screen().point(1, 3).unwrap();
    t.screen_mut().selection = Some(Selection {
        start,
        end,
        rectangular: false,
    });
    let before = t.screen().selection_text().unwrap();
    let pin = t.screen_mut().track(start);
    t.resize(4, 4);
    assert_eq!(t.screen().selection_text().unwrap(), before);
    assert_eq!(t.screen().resolve(pin).unwrap().col, 0);
    invariant(&t);
    t.resize(10, 4);
    assert_eq!(t.screen().selection_text().unwrap(), before);
    invariant(&t);
}

#[test]
fn resize_reflows_cursor_blanks_and_keeps_wide_padding_at_the_edge() {
    let mut t = Terminal::new(12, 4, 100);
    t.feed(b"\t");
    t.resize(4, 2);
    assert_eq!((t.screen().cursor.col, t.screen().cursor.row), (0, 1));

    let mut t = Terminal::new(12, 4, 100);
    t.feed("abc界".as_bytes());
    t.resize(4, 4);
    assert!(t.screen().rows[0].cells[3].spacer_head);
    assert_eq!(t.screen().rows[1].cells[0].text, "界");
    t.resize(1, 4);
    assert!(t.screen().all_rows().all(|r| r.cells[0].text != "界"));
    invariant(&t);
}

#[test]
fn resize_unwraps_before_growing_the_active_area() {
    let mut t = Terminal::new(12, 4, 100);
    t.feed(b"\nabcdefghijklmnopq\x1b[H");
    t.resize(8, 3);
    t.resize(26, 5);
    assert_eq!(lines(&t), ["abcdefghijklmnopq", "", "", "", ""]);
    assert_eq!((t.screen().cursor.col, t.screen().cursor.row), (0, 0));
}

#[test]
fn resize_remaps_saved_cursor_separately_from_the_live_cursor() {
    let mut t = Terminal::new(12, 4, 100);
    t.feed(b"abcdefghi\n\x1b[?1049h");
    t.resize(17, 4);
    assert_eq!(t.primary_screen().cursor.col, 9);
    t.feed(b"\x1b[?1049l");
    // Ghostty clamps saved positions in blank columns using the preceding
    // reflow row's remaining width; the live cursor preserves its blanks.
    assert_eq!((t.screen().cursor.col, t.screen().cursor.row), (7, 1));
}

#[test]
fn insertion_preserves_soft_wrap_and_edits_remove_stale_wide_padding() {
    let mut t = Terminal::new(4, 3, 100);
    t.feed(b"abcdef\x1b[H\x1b[@");
    assert!(t.screen().rows[0].wrapped);
    assert!(!t.screen().cursor.pending_wrap);

    let mut t = Terminal::new(4, 3, 100);
    t.feed("abc界\x1b[2;1HX".as_bytes());
    assert!(!t.screen().rows[0].cells[3].spacer_head);
    let mut t = Terminal::new(4, 3, 100);
    t.feed("abc界\x1b[H\x1b[P".as_bytes());
    assert!(t.screen().rows[0].cells.iter().all(|c| !c.spacer_head));
    let mut t = Terminal::new(4, 3, 100);
    t.feed("abc界\x1b[2;1H\x1b[2P".as_bytes());
    assert!(!t.screen().rows[0].cells[3].spacer_head);
    invariant(&t);
}

#[test]
fn effects_and_terminal_replies_are_ordered() {
    let mut t = Terminal::new(80, 24, 100);
    t.shell_command_events = true;
    let e = t.feed(
        b"\x1b]2;test\x07\x1b[4;3H\x1b[6n\x07\x1b]133;C\x07\x1b]133;D;2\x07\x1b]52;c;aGVsbG8=\x07",
    );
    assert_eq!(
        e,
        [
            Effect::Title("test".into()),
            Effect::Write(b"\x1b[4;3R".to_vec()),
            Effect::Bell,
            Effect::CommandStart,
            Effect::CommandEnd { exit_code: Some(2) },
            Effect::ClipboardWrite(rustty_vt::clipboard::Write::osc52(
                rustty_vt::clipboard::Location::Standard,
                vec![rustty_vt::clipboard::Content {
                    mime: b"text/plain".to_vec(),
                    data: b"hello".as_slice().into(),
                }],
            ))
        ]
    );
    assert_eq!(
        t.feed(b"\x1b[?117$p"),
        [Effect::Write(b"\x1b[?117;4$y".to_vec())]
    );
}

#[test]
fn sgr_colon_subparameters_and_cursor_shapes() {
    let mut t = Terminal::new(10, 2, 0);
    t.feed(b"\x1b[38:2::12:34:56;4:3;58:5:128mX\x1b[5 q");
    let c = &t.screen().rows[0].cells[0];
    assert_eq!(c.style.foreground, Color::Rgb(12, 34, 56));
    assert_eq!(c.style.underline, Underline::Curly);
    assert_eq!(c.style.underline_color, Color::Indexed(128));
    assert_eq!(t.screen().cursor.shape, CursorShape::Bar);
    assert!(t.screen().cursor.blink);
}

#[test]
fn chunk_boundaries_do_not_change_terminal_results() {
    let bytes = "one\r\n\x1b[38;2;50;60;70m界\x1b[?2027h👩🏽‍🚀\x1b]8;;https://example.org\x1b\\link\x1b]8;;\x07\x1b[6n".as_bytes();
    let mut full = Terminal::new(10, 4, 30);
    let effects = full.feed(bytes);
    for size in 1..=bytes.len() {
        let mut split = Terminal::new(10, 4, 30);
        let mut actual = Vec::new();
        for chunk in bytes.chunks(size) {
            actual.extend(split.feed(chunk));
            invariant(&split);
        }
        assert_eq!(actual, effects);
        assert_eq!(split.screen().rows, full.screen().rows);
        assert_eq!(split.screen().cursor, full.screen().cursor);
    }
}

#[test]
fn generated_edits_keep_wide_cells_and_cursor_in_bounds() {
    let choices = [
        "界", "a", "👩", "\u{301}", "\r", "\n", "\x1b[2K", "\x1b[1P", "\x1b[2@", "\x1b[1D",
        "\x1b[4C", "\x1b[2X", "\x1b[2J", "\x1b[H", "\x1b[M", "\x1b[L",
    ];
    let mut t = Terminal::new(7, 4, 20);
    let mut state: u64 = 0x12345678;
    for n in 0..2000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        t.feed(choices[state as usize % choices.len()].as_bytes());
        if n % 31 == 0 {
            t.resize(2 + (state % 9) as u16, 2 + ((state >> 4) % 5) as u16);
        }
        invariant(&t);
    }
}

#[test]
fn viewport_snapshot_excludes_history_and_hides_scrolled_cursor() {
    let mut t = Terminal::new(8, 2, 100);
    t.feed(b"one\r\ntwo\r\nthree\r\nfour");
    t.screen_mut().scroll_viewport(2);
    let snapshot = t.screen().snapshot_viewport();
    assert!(snapshot.history.is_empty());
    assert_eq!(
        snapshot.rows.iter().map(|r| r.text()).collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert!(!snapshot.cursor.visible);
    assert_eq!(snapshot.rows[0].id, t.screen().history[0].id);
}

#[test]
fn limits_prune_by_bytes_and_lines_and_invalidate_removed_content() {
    let mut t = Terminal::with_limits(1024, 2, ScrollbackLimits::default());
    t.feed(b"line\r\n".repeat(180).as_slice());
    let old_count = t.screen().history.len();
    let point = t.screen().point(0, 0).unwrap();
    let tracked = t.screen_mut().track(point);
    t.screen_mut().selection = Some(Selection {
        start: point,
        end: point,
        rectangular: false,
    });
    t.screen_mut().scroll_viewport(100);
    let budget = t.screen().storage_bytes() - 1;
    t.set_limits(ScrollbackLimits {
        bytes: Some(budget),
        lines: Some(2),
    });
    assert!(t.screen().history.len() < old_count);
    assert!(t.screen().storage_bytes() <= budget);
    assert_eq!(t.screen().resolve(tracked), None);
    assert_eq!(t.screen().selection, None);
    assert!(t.screen().viewport_offset <= t.screen().history.len());

    let visible = lines(&t);
    t.set_limits(ScrollbackLimits {
        bytes: Some(0),
        lines: None,
    });
    assert!(t.screen().history.is_empty());
    assert_eq!(t.screen().history_bytes(), 0);
    assert_eq!(lines(&t), visible);
    t.feed(b"\r\nmore\r\noutput");
    assert!(t.screen().history.is_empty());
    let limits = t.limits();
    t.reset();
    assert_eq!(t.limits(), limits);

    t.set_limits(ScrollbackLimits {
        bytes: None,
        lines: Some(1),
    });
    t.feed(b"a\r\nb\r\nc\r\nd");
    // A small line limit retains at least one native page worth of history.
    assert_eq!(t.screen().history.len(), 2);
    t.feed(b"\x1b[3J");
    assert_eq!(t.screen().history_bytes(), 0);
}

#[test]
fn minimum_byte_budget_retains_small_linked_history_after_reflow() {
    let mut plain = Terminal::with_limits(8, 2, ScrollbackLimits::default());
    plain.feed(b"a\r\nb\r\nc");
    let mut linked = Terminal::with_limits(8, 2, ScrollbackLimits::default());
    linked.feed(
        format!(
            "\x1b]8;;https://example.org/{}\x07a\x1b]8;;\x07\r\nb\r\nc",
            "x".repeat(1024)
        )
        .as_bytes(),
    );
    assert!(linked.screen().history_bytes() > plain.screen().history_bytes() + 1024);
    let limit = plain.screen().storage_bytes();
    linked.set_limits(ScrollbackLimits {
        bytes: Some(limit),
        lines: None,
    });
    assert_eq!(linked.screen().history.len(), 1);
    linked.feed(b"\r\nd\r\ne");
    linked.resize(4, 2);
    assert!(!linked.screen().history.is_empty());
    assert_eq!(linked.screen().storage_bytes(), limit);
    invariant(&linked);
}

#[test]
fn zero_line_limit_keeps_history_but_zero_bytes_disables_it() {
    for lines in [0, 1, 4] {
        let mut terminal = Terminal::new(8, 2, lines);
        terminal.feed(b"a\r\nb\r\nc\r\nd\r\ne");
        assert_eq!(terminal.screen().history.len(), 3);
        assert_eq!(terminal.limits().lines, Some(lines));
        terminal.set_limits(ScrollbackLimits::NONE);
        assert!(terminal.screen().history.is_empty());
        terminal.feed(b"\r\nf\r\ng");
        assert!(terminal.screen().history.is_empty());
    }
}
