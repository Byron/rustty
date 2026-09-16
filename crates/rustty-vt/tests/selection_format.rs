use rustty_vt::{
    Selection, Terminal,
    formatter::{CodepointMap, Content, Format, Options, Replacement, TerminalExtra},
};

#[test]
fn selection_formats_preserve_styles_utf8_bytes_and_leave_the_terminal_unchanged() {
    let mut terminal = Terminal::new(8, 3, 100);
    terminal.feed("\x1b[1;31mA界\r\n\x1b[0mé<&\"".as_bytes());
    let screen = terminal.screen();
    let selection = Selection {
        start: screen.point(0, 0).unwrap(),
        end: screen.point(1, 3).unwrap(),
        rectangular: false,
    };
    let before = rustty_vt::snapshot::encode_to_vec(&terminal).unwrap();
    let plain = terminal
        .format_selection(selection, Options::default())
        .unwrap();
    assert_eq!(plain, "A界\né<&\"".as_bytes());
    let vt = terminal
        .format_selection(
            selection,
            Options {
                emit: Format::Vt,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        vt.windows(b"\x1b[0m\x1b[1m\x1b[38;5;1m".len())
            .any(|bytes| bytes == b"\x1b[0m\x1b[1m\x1b[38;5;1m")
    );
    let html = terminal
        .format_selection(
            selection,
            Options {
                emit: Format::Html,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        String::from_utf8(html)
            .unwrap()
            .contains("&#233;&lt;&amp;&quot;")
    );
    assert_eq!(
        rustty_vt::snapshot::encode_to_vec(&terminal).unwrap(),
        before
    );
    assert!(terminal.screen().selection.is_none());
}

#[test]
fn full_exports_include_history_and_preserve_wrapped_rows_by_default() {
    let mut terminal = Terminal::new(4, 2, 100);
    terminal.feed(b"abcdefghi");
    let mut formatter = terminal.screen().formatter(Format::Plain);
    assert_eq!(formatter.format().unwrap(), b"abcd\nefgh\ni");
    formatter.options.unwrap = true;
    assert_eq!(formatter.format().unwrap(), b"abcdefghi");
    formatter.content = Content::None;
    assert!(formatter.format().unwrap().is_empty());
}

#[test]
fn byte_maps_cover_utf8_escapes_replacements_and_terminal_extras() {
    let mut terminal = Terminal::new(8, 3, 100);
    terminal.feed("A  \x1b[31mé\x1b[0m\r\nZ".as_bytes());
    let screen = terminal.screen();
    let (bytes, map) = screen.formatter(Format::Plain).format_with_map().unwrap();
    assert_eq!(bytes, "A  é\nZ".as_bytes());
    let expected = [(0, 0), (2, 0), (1, 0), (3, 0), (3, 0), (3, 0), (0, 1)];
    assert_eq!(map.len(), expected.len());
    for (offset, (x, y)) in expected.into_iter().enumerate() {
        assert_eq!(map.point(offset), screen.point(y, x));
    }
    assert!(map.get(bytes.len()).is_none());
    assert!(map.point(usize::MAX).is_none());

    let replacements = [CodepointMap {
        range: ['é', 'é'],
        replacement: Replacement::String("<&é"),
    }];
    for (emit, text) in [(Format::Vt, "<&é"), (Format::Html, "&lt;&amp;&#233;")] {
        let mut formatter = terminal.formatter(emit);
        formatter.options.codepoint_map = &replacements;
        formatter.extra = TerminalExtra::ALL;
        let (bytes, map) = formatter.format_with_map().unwrap();
        assert_eq!(bytes, formatter.format().unwrap());
        assert_eq!(bytes.len(), map.len());
        let start = bytes
            .windows(text.len())
            .position(|bytes| bytes == text.as_bytes())
            .unwrap();
        for offset in start..start + text.len() {
            assert_eq!(map.point(offset), screen.point(0, 3));
        }
        assert_eq!(map.point(0), screen.point(0, 0));
        assert_eq!(map.point(bytes.len() - 1), screen.point(1, 0));
        formatter.content = Content::None;
        let (bytes, map) = formatter.format_with_map().unwrap();
        assert_eq!(bytes.len(), map.len());
        assert!((0..map.len()).all(|offset| map.point(offset) == screen.point(0, 0)));
        formatter.extra = TerminalExtra::NONE;
        let (bytes, map) = formatter.format_with_map().unwrap();
        assert!(bytes.is_empty());
        assert!(map.is_empty());
    }
}

#[test]
fn byte_maps_keep_native_blank_line_coordinates_without_resolving_invalid_cells() {
    let mut terminal = Terminal::new(1024, 3, 100);
    terminal.feed(b"first\r\n");
    terminal.feed(&b"\r\n".repeat(80));
    terminal.feed(b"last");
    let screen = terminal.screen();
    let formatter = screen.formatter(Format::Plain);
    let (bytes, map) = formatter.format_with_map().unwrap();
    assert_eq!(bytes, formatter.format().unwrap());
    assert_eq!(bytes.len(), map.len());
    // Blank rows carried from the preceding page are attributed to the next
    // page, even when their coordinates exceed that page's physical height.
    let last_newline = bytes.iter().rposition(|&byte| byte == b'\n').unwrap();
    let position = map.get(last_newline).unwrap();
    assert_eq!((position.page, position.x, position.y), (1, 0, 80));
    assert!(map.point(last_newline).is_none());
    assert_eq!(
        map.point(bytes.len() - 1),
        screen.point(screen.history_len() + 2, 3)
    );
}

#[test]
fn formatter_options_borrow_colors_and_replace_text_without_changing_the_source() {
    let mut terminal = Terminal::new(8, 2, 100);
    terminal.feed(b"\x1b[31;44;58;5;2ma b");
    let before = rustty_vt::snapshot::encode_to_vec(&terminal).unwrap();
    let mut palette: [[u8; 3]; 256] = rustty_vt::default_palette().try_into().unwrap();
    palette[1] = [1, 2, 3];
    palette[2] = [4, 5, 6];
    palette[4] = [7, 8, 9];
    let maps = [
        CodepointMap {
            range: ['a', 'z'],
            replacement: Replacement::Codepoint('X'),
        },
        CodepointMap {
            range: ['b', 'b'],
            replacement: Replacement::String("<&é"),
        },
    ];
    let mut formatter = terminal.screen().formatter(Format::Html);
    formatter.options.foreground = Some([10, 11, 12]);
    formatter.options.background = Some([13, 14, 15]);
    formatter.options.palette = Some(&palette);
    formatter.options.codepoint_map = &maps;
    assert_eq!(formatter.format().unwrap(), b"<div style=\"font-family: monospace; white-space: pre;background-color: #0d0e0f;color: #0a0b0c;\"><div style=\"display: inline;color: rgb(1, 2, 3);background-color: rgb(7, 8, 9);text-decoration-color: rgb(4, 5, 6);\">X &lt;&amp;&#233;</div></div>");
    formatter.options.emit = Format::Plain;
    assert_eq!(formatter.format().unwrap(), "X <&é".as_bytes());
    assert_eq!(
        rustty_vt::snapshot::encode_to_vec(&terminal).unwrap(),
        before
    );
}

#[test]
fn full_vt_export_restores_pending_wrap_before_the_active_cursor_attributes() {
    let mut terminal = Terminal::new(4, 3, 100);
    terminal
        .feed(b"\x1b[31mabcd\x1b[32m\x1b]8;id=current;https://example.org\x07\x1b[1\"q\x1b[=21;1u");
    let before = rustty_vt::snapshot::encode_to_vec(&terminal).unwrap();
    let mut formatter = terminal.formatter(Format::Vt);
    formatter.extra = TerminalExtra::ALL;
    let output = formatter.format().unwrap();
    assert_eq!(
        rustty_vt::snapshot::encode_to_vec(&terminal).unwrap(),
        before
    );

    let mut replayed = Terminal::new(4, 3, 100);
    replayed.feed(&output);
    assert!(replayed.screen().cursor.pending_wrap);
    assert_eq!(replayed.screen().cursor, terminal.screen().cursor);
    assert_eq!(replayed.screen().kitty_keyboard.current(), 21);
    terminal.feed(b"e");
    replayed.feed(b"e");
    assert_eq!(
        serde_json::to_value(replayed.screen()).unwrap()["rows"],
        serde_json::to_value(terminal.screen()).unwrap()["rows"],
    );
    assert_eq!(replayed.screen().cursor, terminal.screen().cursor);
}

#[test]
fn state_only_vt_export_restores_terminal_settings_without_printing_content() {
    let mut terminal = Terminal::new(8, 4, 100);
    terminal.feed(b"text\x1b[?1h\x1b[?7l\x1b[?69h\x1b[2;7s\x1b[2;3r\x1b[3g\x1b[2G\x1bH\x1b[6G\x1bH\x1b[>4;2m\x1b]7;file://host/\xffpath\x07\x1b[2;4H");
    let mut formatter = terminal.formatter(Format::Vt);
    formatter.content = Content::None;
    formatter.extra = TerminalExtra::ALL;
    let mut replayed = Terminal::new(8, 4, 100);
    replayed.feed(&formatter.format().unwrap());
    assert!(
        replayed
            .screen()
            .rows()
            .all(|row| replayed.screen().row_text(row).is_empty())
    );
    assert_eq!(replayed.screen().cursor, terminal.screen().cursor);
    for mode in [1, 7, 69] {
        assert_eq!(replayed.modes.dec(mode), terminal.modes.dec(mode));
    }
    assert_eq!(replayed.tabstops(), terminal.tabstops());
    assert!(replayed.modify_other_keys);
    assert_eq!(
        replayed.working_directory_bytes(),
        terminal.working_directory_bytes()
    );
    assert_eq!(
        (
            replayed.margins.top,
            replayed.margins.bottom,
            replayed.margins.left,
            replayed.margins.right
        ),
        (
            terminal.margins.top,
            terminal.margins.bottom,
            terminal.margins.left,
            terminal.margins.right
        )
    );
}
