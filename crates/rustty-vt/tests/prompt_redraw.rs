use rustty_vt::{Color, SemanticContent, Terminal, snapshot};

#[test]
fn resizing_applies_all_none_and_last_prompt_redraw_policies() {
    for policy in ["1", "0", "last"] {
        let mut terminal = Terminal::new(16, 4, 20);
        terminal.feed(format!("\x1b]133;A;redraw={policy}\x07").as_bytes());
        terminal.feed(b"\x1b[31;44m\x1bVfirst\r\nsecond\x1b]133;B\x07input");
        let metadata: Vec<_> = terminal
            .screen()
            .rows
            .iter()
            .map(|row| (row.semantic, row.wrapped, row.wrap_continuation))
            .collect();
        let bytes = snapshot::encode_to_vec(&terminal).unwrap();
        let mut terminal = snapshot::decode(bytes.as_slice(), Default::default()).unwrap();
        terminal.resize(20, 4);
        assert_eq!(
            terminal.screen().row_text(&terminal.screen().rows[0]),
            if policy == "1" { "" } else { "first" }
        );
        assert_eq!(
            terminal.screen().row_text(&terminal.screen().rows[1]),
            if policy == "0" { "secondinput" } else { "" }
        );
        assert_eq!(terminal.screen().cursor.style.background, Color::Indexed(4));
        assert_eq!(terminal.screen().cursor.semantic, SemanticContent::Input);
        if policy != "0" {
            assert!(
                terminal.screen().rows[1]
                    .cells
                    .iter()
                    .all(|cell| { cell.style.background == Color::Default && !cell.protected })
            );
        }
        assert_eq!(
            terminal
                .screen()
                .rows
                .iter()
                .map(|row| (row.semantic, row.wrapped, row.wrap_continuation))
                .collect::<Vec<_>>(),
            metadata
        );
    }
}

#[test]
fn primary_prompt_redraw_runs_while_the_alternate_screen_is_active() {
    let mut terminal = Terminal::new(16, 4, 20);
    terminal.feed(b"\x1b]133;A\x07prompt\x1b[?1049hALT");
    terminal.resize(20, 4);
    assert!(
        terminal
            .primary_screen()
            .row_text(&terminal.primary_screen().rows[0])
            .is_empty()
    );
    assert!(
        terminal
            .screen()
            .row_text(&terminal.screen().rows[0])
            .contains("ALT")
    );

    let mut terminal = Terminal::new(16, 4, 20);
    terminal.feed(b"\x1b[?1049h\x1b]133;A\x07prompt");
    terminal.resize(20, 4);
    assert_eq!(
        terminal.screen().row_text(&terminal.screen().rows[0]),
        "prompt"
    );
}

#[test]
fn last_redraw_clears_unmarked_input_and_same_size_resize_keeps_it() {
    for policy in ["1", "last"] {
        let mut terminal = Terminal::new(16, 4, 20);
        terminal.feed(
            format!("\x1b]133;A;redraw={policy}\x07\x1b]133;C\x07\x1b[2J\x1b]133;B\x07input")
                .as_bytes(),
        );
        terminal.resize(16, 4);
        assert_eq!(
            terminal.screen().row_text(&terminal.screen().rows[0]),
            "input"
        );
        terminal.resize(20, 4);
        assert_eq!(
            terminal.screen().row_text(&terminal.screen().rows[0]),
            if policy == "last" { "" } else { "input" }
        );
    }
}

#[test]
fn all_redraw_reaches_prompt_history_and_rows_below_the_cursor() {
    let mut terminal = Terminal::new(16, 3, 20);
    terminal.feed(b"\x1b]133;A\x07first\r\nsecond\r\nthird\r\nfourth\r\nfifth");
    terminal.resize(20, 3);
    assert!(!terminal.screen().history.is_empty());
    assert!(
        terminal
            .screen()
            .all_rows()
            .all(|row| terminal.screen().row_text(row).is_empty())
    );

    let mut terminal = Terminal::new(16, 4, 20);
    terminal.feed(b"\x1b]133;A\x07prompt\x1b[4;1Hbelow\x1b[1;3H");
    terminal.resize(20, 4);
    assert!(
        terminal
            .screen()
            .all_rows()
            .all(|row| terminal.screen().row_text(row).is_empty())
    );
}
