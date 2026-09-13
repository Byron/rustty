use rustty_vt::{ScrollbackLimits, Selection, Terminal};

#[test]
fn index_scrolling_preserves_complete_row_metadata() {
    let mut corpus = Terminal::new(80, 24, 10);
    corpus.feed(b"\x1b[5W\x1b[4r\x1b[\t33BhD");
    assert!(corpus.screen().rows[22].wrapped);

    for mode in ["primary", "no-history", "alternate"] {
        for command in [b"\n".as_slice(), b"\x1bD", b"\x1bE"] {
            let mut terminal = Terminal::new(8, 4, 10);
            if mode == "no-history" {
                terminal.set_limits(ScrollbackLimits::NONE);
            } else if mode == "alternate" {
                terminal.feed(b"\x1b[?47h");
            }
            terminal.feed(b"abcdefghijklmnopqrstuvwxy\x1b[2;4r\x1b[4;3H\x1b[44m");
            let before = terminal.screen().rows.clone();
            terminal.feed(command);
            let after = &terminal.screen().rows;
            for row in 1..3 {
                assert_eq!(after[row].cells, before[row + 1].cells, "{mode}");
                assert_eq!(after[row].wrapped, before[row + 1].wrapped, "{mode}");
                assert_eq!(
                    after[row].wrap_continuation,
                    before[row + 1].wrap_continuation,
                    "{mode}"
                );
            }
            assert!(!after[3].wrapped && !after[3].wrap_continuation);
            assert!(after[3].cells.iter().all(|cell| cell.text.is_empty()
                && cell.style.background == rustty_vt::screen::Color::Indexed(4)));
        }
    }
}

#[test]
fn index_scrolling_moves_pins_and_clamps_erased_page_start() {
    for (cols, rows, top, bottom, history) in [
        (8, 1, 0, 0, false),
        (8, 4, 0, 3, false),
        (8, 4, 1, 3, false),
        (8, 4, 0, 3, true),
        (1024, 48, 0, 47, false),
        (1024, 48, 45, 47, false),
        (1024, 48, 46, 47, false),
    ] {
        let mut terminal = Terminal::with_limits(cols, rows, ScrollbackLimits::NONE);
        if history {
            terminal.feed(b"A\x1b[22J");
        }
        let offset = terminal.screen().history.len();
        let original = (0..usize::from(rows))
            .map(|row| terminal.screen().point(offset + row, 2).unwrap())
            .collect::<Vec<_>>();
        let pins = original
            .iter()
            .map(|&point| terminal.screen_mut().track(point))
            .collect::<Vec<_>>();
        terminal.screen_mut().selection = Some(Selection {
            start: original[top],
            end: original[bottom],
            rectangular: false,
        });
        let page_start = terminal
            .screen()
            .page_allocations()
            .scan(0, |start, page| {
                let range = *start..*start + usize::from(page.rows);
                *start = range.end;
                Some(range)
            })
            .find(|range| range.contains(&(offset + top)))
            .unwrap()
            .start;
        terminal.feed(
            format!(
                "\x1b[{};{}r\x1b[{};3H\x1bD",
                top + 1,
                bottom + 1,
                bottom + 1
            )
            .as_bytes(),
        );
        let screen = terminal.screen();
        for (row, pin) in pins.iter().enumerate() {
            let expected = if rows == 1 {
                screen.point(offset + row, 2)
            } else if row == top {
                if offset + top == page_start {
                    screen.point(offset + top, 0)
                } else {
                    screen.point(offset + top - 1, 2)
                }
            } else {
                screen.point(offset + row - usize::from(row > top && row <= bottom), 2)
            };
            assert_eq!(
                screen.resolve(*pin),
                expected,
                "{cols}/{top}/{row}/{history}"
            );
        }
        let selection = screen.selection.unwrap();
        assert_eq!(Some(selection.start), screen.resolve(pins[top]));
        assert_eq!(Some(selection.end), screen.resolve(pins[bottom]));
    }
}

#[test]
fn full_width_line_shifts_detach_wrapped_rows() {
    for command in ["\x1b[L", "\x1b[M", "\x1b[T", "\x1b[2;4r\x1b[S"] {
        let mut terminal = Terminal::new(8, 4, 10);
        terminal.feed(b"abcdefghijklmnopqr\r\nlast\x1b[H");
        terminal.feed(command.as_bytes());
        let start = usize::from(command.contains('r'));
        for row in &terminal.screen().rows[start..] {
            assert!(!row.wrapped && !row.wrap_continuation, "{command:?}");
        }
    }
    let mut terminal = Terminal::new(8, 4, 10);
    terminal.feed(b"abcdefghijklmnopqr\r\nlast\x1b[S");
    assert!(terminal.screen().history[0].wrapped);
    assert!(terminal.screen().rows[0].wrapped);
}

#[test]
fn partial_width_line_shifts_preserve_row_wrap_metadata() {
    for command in b"LMST" {
        let mut terminal = Terminal::new(8, 4, 10);
        terminal.feed(b"abcdefghijklmnopqr\r\nlast\x1b[?69h\x1b[3;6s\x1b[1;3H");
        let wraps: Vec<_> = terminal
            .screen()
            .rows
            .iter()
            .map(|row| (row.wrapped, row.wrap_continuation))
            .collect();
        terminal.feed(&[0x1b, b'[', *command]);
        assert_eq!(
            terminal
                .screen()
                .rows
                .iter()
                .map(|row| (row.wrapped, row.wrap_continuation))
                .collect::<Vec<_>>(),
            wraps
        );
    }
}

#[test]
fn moving_rows_removes_orphaned_wide_wrap_padding() {
    let mut terminal = Terminal::new(8, 4, 10);
    terminal.feed("\x1b[8G界\x1b[T".as_bytes());
    assert!(!terminal.screen().rows[1].cells[7].spacer_head);
    assert_eq!(terminal.screen().rows[2].cells[0].text, "界");
}

#[test]
fn margin_splits_clear_wide_text_and_preserve_surviving_attributes() {
    for command in b"LMST" {
        let mut terminal = Terminal::new(8, 4, 10);
        terminal.feed(b"\x1b[31;44m\x1b]8;id=wide;uri\x1b\\");
        for row in 1..=4 {
            terminal.feed(format!("\x1b[{row};1Ha界b界cd").as_bytes());
        }
        let before = terminal.screen().rows[0].cells.clone();
        terminal.feed(b"\x1b[?69h\x1b[3;5s\x1b[1;3H");
        terminal.feed(&[0x1b, b'[', *command]);
        for row in &terminal.screen().rows {
            for col in [1, 5] {
                let cell = &row.cells[col];
                assert!(cell.text.is_empty());
                assert_eq!(cell.width, 1);
                assert_eq!(cell.style, before[col].style);
                assert_eq!(cell.hyperlink_id, before[col].hyperlink_id);
            }
        }
    }
}
