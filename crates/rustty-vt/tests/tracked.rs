use rustty_vt::{Selection, Terminal, snapshot};

#[test]
fn tracked_cells_belong_to_one_live_screen() {
    let mut terminal = Terminal::new(8, 2, 4);
    terminal.feed(b"A");
    let point = terminal.screen().point(0, 0).unwrap();
    let primary = terminal.screen_mut().track(point);
    terminal.feed(b"\x1b[?47hB");
    let alternate = terminal.screen_mut().track(point);
    assert_ne!(primary, alternate);
    assert_eq!(terminal.screen().resolve(primary), None);
    assert_eq!(terminal.primary_screen().resolve(alternate), None);
    terminal.untrack(primary);
    assert_eq!(terminal.primary_screen().resolve(primary), None);
    assert_eq!(terminal.screen().resolve(alternate), Some(point));
    terminal.feed(b"\x1b[?47l");
    terminal.untrack(alternate);
    assert_eq!(
        terminal.alternate_screen().unwrap().resolve(alternate),
        None
    );

    let mut other = Terminal::new(8, 2, 4);
    let other_handle = other.screen_mut().track(point);
    terminal.untrack(other_handle);
    assert_eq!(other.screen().resolve(other_handle), Some(point));
    assert_eq!(terminal.screen().resolve(other_handle), None);
}

#[test]
fn reset_never_rebinds_an_old_tracked_cell() {
    for alternate in [false, true] {
        for stream_reset in [false, true] {
            let mut terminal = Terminal::new(8, 2, 4);
            if alternate {
                terminal.feed(b"\x1b[?1049h");
            }
            terminal.feed(b"A");
            let point = terminal.screen().point(0, 0).unwrap();
            let stale = terminal.screen_mut().track(point);
            if stream_reset {
                terminal.feed(b"\x1bc");
            } else {
                terminal.reset();
            }
            terminal.feed(b"B");
            let current = terminal.screen_mut().track(point);
            assert_ne!(stale, current);
            assert_eq!(terminal.screen().resolve(stale), None);
            terminal.untrack(stale);
            assert_eq!(terminal.screen().resolve(current), Some(point));
        }
    }
}

#[test]
fn copying_screen_contents_does_not_copy_external_handles() {
    let mut terminal = Terminal::new(8, 2, 4);
    terminal.feed(b"A");
    let point = terminal.screen().point(0, 0).unwrap();
    let original = terminal.screen_mut().track(point);
    let wire = snapshot::encode_to_vec(&terminal).unwrap();
    let restored = snapshot::decode(wire.as_slice(), Default::default()).unwrap();
    assert_eq!(terminal.clone().screen().resolve(original), None);
    assert_eq!(restored.screen().resolve(original), None);
    for mut copy in [
        terminal.screen().clone(),
        terminal.screen().snapshot_viewport(),
        restored.screen().clone(),
    ] {
        assert_eq!(copy.resolve(original), None);
        let copied_point = copy.point(0, 0).unwrap();
        let copied = copy.track(copied_point);
        assert_ne!(original, copied);
        assert_eq!(copy.resolve(original), None);
        copy.untrack(original);
        assert_eq!(copy.resolve(copied), Some(copied_point));
        assert_eq!(terminal.screen().resolve(copied), None);
    }
    assert_eq!(terminal.screen().resolve(original), Some(point));
}

#[test]
fn line_edits_keep_tracked_cells_and_selection_at_physical_coordinates() {
    for alternate in [false, true] {
        for command in [
            b"\x1b[H\x1b[L".as_slice(),
            b"\x1b[H\x1b[M",
            b"\x1b[T",
            b"\x1b[2;3r\x1b[S",
        ] {
            let mut terminal = Terminal::new(8, 4, 10);
            if alternate {
                terminal.feed(b"\x1b[?47h");
            }
            terminal.feed(b"aa\r\nbb\r\ncc\r\ndd");
            let points: Vec<_> = (0..4)
                .map(|y| terminal.screen().point(y, 1).unwrap())
                .collect();
            let pins: Vec<_> = points
                .iter()
                .map(|&point| terminal.screen_mut().track(point))
                .collect();
            terminal.screen_mut().selection = Some(Selection {
                start: points[0],
                end: points[3],
                rectangular: false,
            });
            terminal.feed(command);
            for (y, pin) in pins.into_iter().enumerate() {
                assert_eq!(
                    terminal.screen().resolve(pin),
                    terminal.screen().point(y, 1)
                );
            }
            let selected = terminal.screen().selection.unwrap();
            assert_eq!(Some(selected.start), terminal.screen().point(0, 1));
            assert_eq!(Some(selected.end), terminal.screen().point(3, 1));
        }
    }
}

#[test]
fn scroll_pins_follow_native_history_insertion_and_no_history_clamping() {
    for (alternate, partial) in [(false, true), (true, true), (true, false)] {
        let mut terminal = Terminal::new(8, 4, 10);
        if alternate {
            terminal.feed(b"\x1b[?47h");
        }
        terminal.feed(b"aa\r\nbb\r\ncc\r\ndd");
        let first = terminal.screen().point(0, 1).unwrap();
        let last = terminal.screen().point(3, 1).unwrap();
        let first = terminal.screen_mut().track(first);
        let last = terminal.screen_mut().track(last);
        if partial {
            terminal.feed(b"\x1b[1;3r");
        }
        terminal.feed(b"\x1b[S");
        let screen = terminal.screen();
        assert_eq!(screen.resolve(first), screen.point(0, 1));
        assert_eq!(
            screen.resolve(last),
            screen.point(if partial { 3 } else { 2 }, 1)
        );
        let last = screen.resolve(last).unwrap();
        assert_eq!(
            &*screen.cell_text(screen.row_by_id(last.row).unwrap(), last.col),
            if alternate { "d" } else { "" }
        );
    }
}
