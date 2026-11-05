use rustty_vt::selection::{DEFAULT_WORD_BOUNDARIES, SelectLine};
use rustty_vt::{Selection, Terminal};

#[test]
fn selectors_return_inclusive_bounds_without_replacing_the_active_selection() {
    let mut terminal = Terminal::new(8, 4, 100);
    terminal.feed(b"one:two\r\n  more");
    let screen = terminal.screen_mut();
    let selected = |start: (usize, usize), end: (usize, usize)| Selection {
        start: screen.point(start.0, start.1).unwrap(),
        end: screen.point(end.0, end.1).unwrap(),
        rectangular: false,
    };
    let initial = selected((0, 1), (0, 1));
    let word = selected((0, 0), (0, 2));
    let punctuation = selected((0, 3), (0, 3));
    let whole = selected((0, 0), (0, 6));
    let more = selected((1, 2), (1, 5));
    screen.selection = Some(initial);
    assert_eq!(
        screen.select_word(screen.point(0, 1).unwrap(), DEFAULT_WORD_BOUNDARIES),
        Some(word)
    );
    assert_eq!(
        screen.select_word(screen.point(0, 3).unwrap(), DEFAULT_WORD_BOUNDARIES),
        Some(punctuation)
    );
    assert_eq!(
        screen.select_word(screen.point(0, 1).unwrap(), &[]),
        Some(whole)
    );
    assert_eq!(
        screen.select_word(screen.point(1, 7).unwrap(), DEFAULT_WORD_BOUNDARIES),
        None
    );
    assert_eq!(
        screen.select_word_between(
            screen.point(1, 7).unwrap(),
            screen.point(1, 3).unwrap(),
            DEFAULT_WORD_BOUNDARIES
        ),
        Some(more)
    );
    assert_eq!(screen.selection, Some(initial));
    assert_eq!(
        screen.select_all(),
        Some(Selection {
            start: word.start,
            end: more.end,
            rectangular: false
        })
    );
}

#[test]
fn logical_line_selection_respects_cell_semantics_and_distinguishes_trimming_options() {
    let mut terminal = Terminal::new(16, 3, 100);
    terminal.feed(b"\x1b]133;A\x07P \x1b]133;B\x07ab\x1b]133;C\x07  xy");
    let screen = terminal.screen();
    for (x, start, end) in [(0, 0, 0), (2, 2, 3), (4, 6, 7)] {
        let selected = screen
            .select_line(screen.point(0, x).unwrap(), SelectLine::default())
            .unwrap();
        assert_eq!((selected.start.col, selected.end.col), (start, end));
    }
    let selected = screen
        .select_line(
            screen.point(0, 2).unwrap(),
            SelectLine {
                semantic_prompt_boundary: false,
                ..SelectLine::default()
            },
        )
        .unwrap();
    assert_eq!((selected.start.col, selected.end.col), (0, 7));
    let blank = screen.point(1, 3).unwrap();
    assert!(screen.select_line(blank, SelectLine::default()).is_none());
    assert!(
        screen
            .select_line(
                blank,
                SelectLine {
                    whitespace: Some(&[]),
                    ..SelectLine::default()
                }
            )
            .is_none()
    );
    let selected = screen
        .select_line(
            blank,
            SelectLine {
                whitespace: None,
                ..SelectLine::default()
            },
        )
        .unwrap();
    assert_eq!((selected.start.col, selected.end.col), (0, 15));

    terminal.feed(b"\r\n   ");
    let screen = terminal.screen();
    let selected = screen
        .select_line(
            blank,
            SelectLine {
                whitespace: Some(&[]),
                ..SelectLine::default()
            },
        )
        .unwrap();
    assert_eq!((selected.start.col, selected.end.col), (0, 2));
}

#[test]
fn word_selection_stops_at_hard_edges_and_includes_wide_spacers() {
    let mut terminal = Terminal::new(4, 3, 100);
    terminal.feed(b"abcd\r\nefgh");
    let screen = terminal.screen();
    let before_edge = screen
        .select_word(screen.point(0, 2).unwrap(), DEFAULT_WORD_BOUNDARIES)
        .unwrap();
    assert_eq!(before_edge.end, screen.point(0, 3).unwrap());
    let at_edge = screen
        .select_word(screen.point(0, 3).unwrap(), DEFAULT_WORD_BOUNDARIES)
        .unwrap();
    assert_eq!(at_edge.end, screen.point(0, 3).unwrap());

    let mut terminal = Terminal::new(4, 3, 100);
    terminal.feed("a界b".as_bytes());
    let screen = terminal.screen();
    for col in 0..4 {
        let selected = screen
            .select_word(screen.point(0, col).unwrap(), DEFAULT_WORD_BOUNDARIES)
            .unwrap();
        assert_eq!(selected.start, screen.point(0, 0).unwrap());
        assert_eq!(selected.end, screen.point(0, 3).unwrap());
    }
}

#[test]
fn word_selection_resolves_wide_characters_across_soft_wraps() {
    let mut terminal = Terminal::new(5, 3, 100);
    terminal.feed("abcd界ef".as_bytes());
    let screen = terminal.screen();
    for (boundaries, start, end) in [
        (DEFAULT_WORD_BOUNDARIES, 0, 8),
        (&['界'][..], 0, 3),
        (&['界'][..], 4, 6),
        (&['界'][..], 7, 8),
    ] {
        for offset in start..=end {
            let selected = screen
                .select_word(screen.point(offset / 5, offset % 5).unwrap(), boundaries)
                .unwrap();
            assert_eq!(selected.start, screen.point(start / 5, start % 5).unwrap());
            assert_eq!(selected.end, screen.point(end / 5, end % 5).unwrap());
        }
    }
}

#[test]
fn output_selection_uses_prompt_groups_and_retains_explicit_spaces() {
    let mut terminal = Terminal::new(16, 5, 100);
    terminal.feed(
        b"pre\r\n\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07  out \r\n\r\n\x1b]133;A\x07$ ",
    );
    let screen = terminal.screen_mut();
    let initial = screen.select_all();
    screen.selection = initial;
    for (clicked, start, end) in [
        ((0, 5), (0, 0), (0, 2)),
        ((2, 3), (2, 0), (2, 5)),
        ((2, 8), (2, 0), (2, 5)),
        ((3, 1), (2, 0), (2, 5)),
    ] {
        assert_eq!(
            screen.select_output(screen.point(clicked.0, clicked.1).unwrap()),
            Some(Selection {
                start: screen.point(start.0, start.1).unwrap(),
                end: screen.point(end.0, end.1).unwrap(),
                rectangular: false,
            })
        );
    }
    for (row, col) in [(1, 0), (1, 2), (4, 7)] {
        assert!(
            screen
                .select_output(screen.point(row, col).unwrap())
                .is_none()
        );
    }
    assert_eq!(screen.selection, initial);

    let mut terminal = Terminal::new(4, 2, 100);
    terminal.feed(b"free");
    let screen = terminal.screen();
    assert!(screen.select_output(screen.point(0, 0).unwrap()).is_none());

    terminal.feed(b"\x1b[2J\x1b[2;1H\x1b]133;A\x07$ ");
    let screen = terminal.screen();
    let origin = screen.point(0, 0).unwrap();
    assert_eq!(
        screen.select_output(origin),
        Some(Selection {
            start: origin,
            end: origin,
            rectangular: false,
        })
    );
}

#[test]
fn last_command_output_preserves_spaces_wraps_and_selection_in_history() {
    let mut terminal = Terminal::new(8, 3, 100);
    terminal.feed(
        concat!(
            "\x1b]133;A\x07$ \x1b]133;B\x07first\r\n\x1b]133;C\x07old\r\n",
            "\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07second\r\n",
            "\x1b]133;C\x07  12345界abcdefgh \r\nlast  \r\n\x1b]133;D;0\x07",
            "\x1b]133;A\x07$ \x1b]133;B\x07editing"
        )
        .as_bytes(),
    );
    let selection = terminal.screen().select_all();
    terminal.screen_mut().selection = selection;
    terminal.screen_mut().scroll_viewport(isize::MAX);
    assert_eq!(
        terminal.last_command_output_text().as_deref(),
        Some("  12345界abcdefgh \nlast  "),
    );
    assert_eq!(terminal.screen().selection, selection);
    assert!(terminal.screen().viewport_offset > 0);
}

#[test]
fn last_command_output_uses_only_the_latest_executed_prompt_group() {
    let mut terminal = Terminal::new(20, 5, 100);
    terminal.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07first");
    assert_eq!(terminal.last_command_output_text(), None);
    terminal.feed(b"\r\n\x1b]133;C\x07old\r\n\x1b]133;D;0\x07");
    assert_eq!(terminal.last_command_output_text().as_deref(), Some("old"));
    terminal.feed(b"\x1b]133;A\x07$ ");
    assert_eq!(terminal.last_command_output_text().as_deref(), Some("old"));
    terminal.feed(b"\x1b]133;B\x07empty\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
    assert_eq!(terminal.last_command_output_text(), None);
    terminal.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07editing");
    assert_eq!(terminal.last_command_output_text(), None);
    terminal.feed(b"\r\n\x1b]133;C\x07   ");
    assert_eq!(terminal.last_command_output_text().as_deref(), Some("   "));
}

#[test]
fn last_command_output_handles_same_row_output_and_secondary_prompts() {
    let mut terminal = Terminal::new(40, 6, 100);
    terminal.feed(
        concat!(
            "\x1b]133;A\x07$ \x1b]133;B\x07command\r\n",
            "\x1b]133;P;k=s\x07> \x1b]133;B\x07continued",
            "\x1b]133;C\x07  output  \x1b]133;D;0\x07"
        )
        .as_bytes(),
    );
    assert_eq!(
        terminal.last_command_output_text().as_deref(),
        Some("  output  ")
    );
    terminal.feed(
        concat!(
            "\x1b]133;A\x07$ \x1b]133;B\x07editing\r\n",
            "\x1b]133;P;k=s\x07> \x1b]133;B\x07continued"
        )
        .as_bytes(),
    );
    assert_eq!(
        terminal.last_command_output_text().as_deref(),
        Some("  output  ")
    );
}

#[test]
fn last_command_output_requires_a_retained_prompt_and_primary_screen() {
    let mut terminal = Terminal::with_limits(10, 2, rustty_vt::ScrollbackLimits::NONE);
    terminal.feed(b"unmarked\r\n\x1b]133;A\x07$ ");
    assert_eq!(terminal.last_command_output_text(), None);
    terminal.feed(b"\x1b]133;B\x07cmd\r\n\x1b]133;C\x07out");
    assert_eq!(terminal.last_command_output_text().as_deref(), Some("out"));
    terminal.feed(b"\x1b[?1049h\x1b]133;A\x07$ \x1b]133;C\x07alt");
    assert_eq!(terminal.last_command_output_text(), None);
    terminal.feed(b"\x1b[?1049l");
    assert_eq!(terminal.last_command_output_text().as_deref(), Some("out"));
    terminal.feed(b"\r\nmore\r\nlast");
    assert_eq!(terminal.last_command_output_text(), None);
}

#[test]
fn last_command_output_follows_cursor_groups_without_including_later_commands() {
    let mut terminal = Terminal::new(20, 8, 100);
    terminal.feed(
        concat!(
            "\x1b]133;A\x07$ \x1b]133;B\x07first\r\n",
            "\x1b]133;C\x07first line\r\nsecond line\r\n\x1b]133;D;0\x07",
            "\x1b]133;A\x07$ \x1b]133;B\x07empty\r\n\x1b]133;C\x07\x1b]133;D;0\x07",
            "\x1b]133;A\x07$ \x1b]133;B\x07last\r\n\x1b]133;C\x07later"
        )
        .as_bytes(),
    );
    assert_eq!(
        terminal.last_command_output_text().as_deref(),
        Some("later")
    );
    terminal.feed(b"\x1b[2;1H");
    assert_eq!(
        terminal.last_command_output_text().as_deref(),
        Some("first line\nsecond line"),
    );
    terminal.feed(b"\x1b[4;1H");
    assert_eq!(terminal.last_command_output_text(), None);
}

#[test]
fn last_command_output_delegates_native_tmux_boundaries() {
    let mut terminal = Terminal::new(8, 5, 100);
    terminal.set_tmux_strings(true);
    terminal.feed(
        concat!(
            "\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07",
            "12345678wide界\r\n\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ "
        )
        .as_bytes(),
    );
    let screen = terminal.screen();
    let cursor = rustty_vt::GridPoint {
        row: screen.row(screen.cursor.row).id,
        col: screen.cursor.col,
    };
    assert_eq!(
        terminal.last_command_output_text(),
        screen.tmux_output_text(cursor)
    );
    assert_eq!(
        terminal.last_command_output_text().as_deref(),
        Some("12345678wide界\n\n"),
    );
    terminal.feed(b"\x1b]133;B\x07empty\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
    assert_eq!(terminal.last_command_output_text(), None);
}

#[test]
fn selection_preserves_native_whitespace_wrap_and_wide_cell_boundaries() {
    for (text, start, end, rectangular, expected) in [
        ("", (1, 0), (6, 1), false, ""),
        ("ab\r\n \r\n", (0, 0), (7, 3), false, "ab\n"),
        ("ab\r\n\r\ncd", (0, 0), (7, 3), false, "ab\n\ncd"),
        ("abcd    ef", (0, 0), (1, 1), false, "abcd    ef"),
        ("abcd    ef", (0, 0), (7, 0), false, "abcd"),
        ("abcdefghijklmnopqr", (1, 0), (6, 1), true, "bcdefgjklmno"),
        ("界aé🙂", (1, 0), (1, 0), false, "界"),
        ("1234567界abc", (0, 0), (7, 0), false, "1234567界"),
        ("1234567界abc", (7, 0), (7, 0), false, "界"),
    ] {
        for reverse in [false, true] {
            let mut terminal = Terminal::new(8, 4, 100);
            terminal.feed(text.as_bytes());
            let screen = terminal.screen_mut();
            let mut start = screen.point(start.1, start.0).unwrap();
            let mut end = screen.point(end.1, end.0).unwrap();
            if reverse {
                std::mem::swap(&mut start, &mut end);
            }
            screen.selection = Some(Selection {
                start,
                end,
                rectangular,
            });
            assert_eq!(
                screen.selection_text().as_deref(),
                Some(expected),
                "{text:?}"
            );
        }
    }
}
