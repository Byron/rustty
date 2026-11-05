use super::*;

fn filled() -> Terminal {
    let mut terminal = Terminal::new(80, 4, 8000);
    terminal.feed(&b"historical text\r\n".repeat(6000));
    terminal.feed("\x1b[H\x1b[2Jprimary界 e\u{301}\r\nsecond".as_bytes());
    terminal
}

fn finish(terminal: &mut Terminal) {
    terminal.request_scrollback_compression();
    for _ in 0..10000 {
        if !terminal.compress_scrollback_step() {
            return;
        }
    }
    panic!("compression did not finish");
}

fn owned(terminal: &Terminal) -> usize {
    terminal.inner.primary_screen().owned_bytes()
}

#[test]
fn capture_ranges_restore_only_selected_cold_pages() {
    let mut terminal = filled();
    terminal.inner.screen_mut().viewport_offset = 20;
    finish(&mut terminal);
    let cold = owned(&terminal);
    assert_eq!(
        terminal
            .capture_for_owner(CaptureOptions::default())
            .unwrap(),
        "primary界 e\u{301}\nsecond\n\n\n".as_bytes()
    );
    assert_eq!(owned(&terminal), cold);
    finish(&mut terminal);

    for (start, end) in [(-3000, -3000), (-3002, -2998), (-1, 0)] {
        let expected = terminal.clone();
        let before = owned(&expected);
        let history = expected.history_len();
        let first = (history as i64 + i64::from(start)) as usize;
        let count = (end - start + 1) as usize;
        let mut text = Vec::<u8>::new();
        for (offset, row) in expected
            .inner
            .primary_screen()
            .all_rows()
            .skip(first)
            .take(count)
            .enumerate()
        {
            text.extend(
                format!(
                    "{} {}\n",
                    start + offset as i32,
                    expected.inner.primary_screen().row_text(row)
                )
                .as_bytes(),
            );
        }
        let expected_growth = owned(&expected) - before;
        let bytes = terminal
            .capture_for_owner(CaptureOptions {
                start: Some(start),
                end: Some(end),
                number_lines: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(bytes, text);
        assert_eq!(owned(&terminal) - cold, expected_growth);
        if start < -20 {
            assert!(expected_growth > 0);
        }
        finish(&mut terminal);
        assert_eq!(owned(&terminal), cold);
    }
}

#[test]
fn capture_alternate_ranges_skip_saved_primary_history() {
    let mut terminal = filled();
    terminal.feed(b"\x1b[?1049h\x1b[HALTERNATE");
    finish(&mut terminal);
    let cold = owned(&terminal);
    for (options, expected) in [
        (CaptureOptions::default(), "ALTERNATE\n\n\n\n"),
        (
            CaptureOptions {
                alternate: true,
                ..Default::default()
            },
            "primary界 e\u{301}\nsecond\n\n\n",
        ),
        (
            CaptureOptions {
                alternate: true,
                start: Some(i32::MIN),
                end: Some(0),
                number_lines: true,
                ..Default::default()
            },
            "0 primary界 e\u{301}\n",
        ),
    ] {
        assert_eq!(
            terminal.capture_for_owner(options).unwrap(),
            expected.as_bytes()
        );
        assert_eq!(owned(&terminal), cold);
        finish(&mut terminal);
    }
    assert_eq!(
        terminal
            .capture_for_owner(CaptureOptions {
                start: Some(-1),
                end: Some(0),
                number_lines: true,
                ..Default::default()
            })
            .unwrap(),
        b"-1 historical text\n0 ALTERNATE\n"
    );
    finish(&mut terminal);
    assert_eq!(owned(&terminal), cold);
    terminal.feed(b"\x1b[?1049l");
    assert!(
        terminal
            .capture_with(CaptureOptions {
                alternate: true,
                ..Default::default()
            })
            .is_err()
    );
    assert!(
        terminal
            .capture_with(CaptureOptions {
                alternate: true,
                quiet: true,
                ..Default::default()
            })
            .unwrap()
            .is_empty()
    );
}

#[test]
fn clipped_capture_preserves_wraps_bounds_and_physical_line_numbers() {
    let mut terminal = Terminal::new(4, 2, 20);
    terminal.feed(b"abcdef\r\nlast");
    for (start, end, join, numbers, expected) in [
        (-1, -1, true, false, "abcd"),
        (0, 0, true, false, "ef\n"),
        (-1, 0, true, true, "-1 abcd0 ef\n"),
        (0, -1, false, true, "-1 abcd\n0 ef\n"),
        (i32::MIN, i32::MAX, false, true, "-1 abcd\n0 ef\n1 last\n"),
        (i32::MAX, i32::MAX, false, true, "1 last\n"),
    ] {
        assert_eq!(
            terminal
                .capture_with(CaptureOptions {
                    start: Some(start),
                    end: Some(end),
                    join,
                    number_lines: numbers,
                    ..Default::default()
                })
                .unwrap(),
            expected.as_bytes()
        );
    }
}
