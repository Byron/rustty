use rustty_vt::{Color, CursorShape, Style, Terminal, color::Target};

const RESET_MODES: &[(bool, u16)] = &[
    (true, 25),
    (false, 4),
    (true, 6),
    (true, 7),
    (true, 45),
    (true, 1045),
    (false, 2),
    (true, 1),
    (true, 66),
    (true, 69),
];

#[test]
fn decstr_restores_selected_mode_defaults_and_keeps_other_modes() {
    for configured in [false, true] {
        let mut terminal = Terminal::new(10, 10, 100);
        for &(private, mode) in RESET_MODES {
            let default = terminal.modes.get_default(private, mode).unwrap() ^ configured;
            terminal.modes.set_default(private, mode, default);
            terminal.modes.set(private, mode, !default);
        }
        for mode in [1000, 2004, 2026, 2027] {
            terminal.set_mode(true, mode, true);
        }
        terminal.feed(b"\x1b[!p");
        for &(private, mode) in RESET_MODES {
            assert_eq!(
                terminal.modes.get(private, mode),
                terminal.modes.get_default(private, mode).unwrap(),
                "private={private}, mode={mode}, configured={configured}",
            );
        }
        assert_eq!(terminal.screen().cursor.visible, terminal.modes.dec(25));
        for mode in [1000, 2004, 2026, 2027] {
            assert!(terminal.modes.dec(mode), "mode={mode}");
        }
    }
}

#[test]
fn decstr_keeps_active_screen_content_cursor_wrap_and_tabstops() {
    let mut terminal = Terminal::new(10, 4, 100);
    terminal.feed(b"primary\x1b[?1049h\x1b[3g\x1b[4G\x1bH\x1b[1Gabcdefghij");
    let screen = terminal.screen();
    let row = screen.row(0).id;
    let point = screen.point(0, 2).unwrap();
    let tracked = terminal.screen_mut().track(point);
    terminal.feed(b"\x1b[!p");
    assert!(terminal.is_alternate_screen());
    let screen = terminal.screen();
    assert_eq!(screen.row(0).id, row);
    assert_eq!(screen.row_text(screen.row(0)), "abcdefghij");
    assert_eq!((screen.cursor.row, screen.cursor.col), (0, 9));
    assert!(screen.cursor.pending_wrap);
    assert_eq!(screen.resolve(tracked), Some(point));
    terminal.feed(b"\r\t");
    assert_eq!(terminal.screen().cursor.col, 3);
    terminal.feed(b"\x1b[?1049l");
    assert_eq!(
        terminal.screen().row_text(terminal.screen().row(0)),
        "primary"
    );
}

#[test]
fn decstr_resets_margins_pen_charsets_and_saved_cursor_without_moving_cursor() {
    let mut terminal = Terminal::new(10, 10, 100);
    terminal.feed(b"\x1b[3;8r\x1b[?69h\x1b[4;9s\x1b[?6h\x1b[2;2H\x1b[1;31m\x1b7");
    terminal.feed(b"\x1b(0\x1b)0\x1b*0\x1b+0\x1bo\x1bN\x1bV\x1b[>4;2m");
    let cursor = terminal.screen().cursor.clone();
    terminal.feed(b"\x1b[!p");
    let screen = terminal.screen();
    assert_eq!(
        (screen.cursor.row, screen.cursor.col),
        (cursor.row, cursor.col)
    );
    assert_eq!(screen.cursor.style, Style::default());
    assert!(!screen.cursor.protected);
    assert_eq!((terminal.margins.top, terminal.margins.bottom), (0, 9));
    assert_eq!((terminal.margins.left, terminal.margins.right), (0, 9));
    assert!(!terminal.modify_other_keys);
    terminal.feed(b"q\x1b8");
    assert_eq!(
        terminal.screen().row(cursor.row).text(cursor.col).as_str(),
        "q"
    );
    let cursor = &terminal.screen().cursor;
    assert_eq!((cursor.row, cursor.col), (0, 0));
    assert_eq!(cursor.style, Style::default());
    assert!(!cursor.pending_wrap);
}

#[test]
fn decstr_ends_iso_protection_and_leaves_status_display() {
    let mut terminal = Terminal::new(10, 2, 100);
    terminal.feed(b"\x1bVprotected\x1bW\x1b[1$}");
    terminal.feed(b"\x1b[!pX");
    assert_eq!(
        terminal.screen().row_text(terminal.screen().row(0)),
        "protectedX"
    );
    terminal.feed(b"\x1b[2J");
    assert!(
        terminal
            .screen()
            .rows()
            .all(|row| terminal.screen().row_text(row).is_empty())
    );
}

#[test]
fn decstr_resets_palette_and_cursor_appearance_to_configuration() {
    let mut terminal = Terminal::new(10, 2, 100);
    terminal.set_default_colors(Some([1, 2, 3]), None, None, &[[4, 5, 6]]);
    terminal.set_default_cursor(CursorShape::Underline, Some(false));
    terminal.feed(b"\x1b]4;0;#ffffff\x07\x1b]10;#778899\x07\x1b[1 q\x1b[!p");
    assert_eq!(terminal.color_current(Target::Palette(0)), Some([4, 5, 6]));
    assert_eq!(terminal.color_override(Target::Palette(0)), None);
    assert_eq!(
        terminal.color_current(Target::Foreground),
        Some([0x77, 0x88, 0x99])
    );
    assert_eq!(terminal.screen().cursor.shape, CursorShape::Underline);
    assert!(!terminal.screen().cursor.blink);
}

#[test]
fn decstr_accepts_only_the_parameterless_sequence_even_when_chunked() {
    for sequence in [b"\x1b[1!p".as_slice(), b"\x1b[0!p", b"\x1b[?!p", b"\x1b[$p"] {
        let mut terminal = Terminal::new(10, 2, 0);
        terminal.feed(b"\x1b[31m");
        terminal.feed(sequence);
        assert_eq!(terminal.screen().cursor.style.foreground, Color::Indexed(1));
    }
    for split in 0..=4 {
        let mut terminal = Terminal::new(10, 2, 0);
        terminal.feed(b"\x1b[31m");
        terminal.feed(&b"\x1b[!p"[..split]);
        terminal.feed(&b"\x1b[!p"[split..]);
        assert_eq!(terminal.screen().cursor.style, Style::default());
    }
}

#[test]
fn soft_reset_keeps_pending_parser_input_and_hyperlink() {
    let mut terminal = Terminal::new(10, 2, 100);
    terminal.feed(b"\x1b]8;id=link;https://example.com\x07\x1b[31m\x1b]2;title");
    let link = terminal.screen().cursor.hyperlink.clone();
    terminal.soft_reset();
    assert_eq!(terminal.screen().cursor.hyperlink, link);
    assert_eq!(terminal.screen().cursor.style, Style::default());
    terminal.feed(b"\x07X");
    assert_eq!(terminal.title, "title");
    assert!(terminal.screen().row(0).cells[0].has_hyperlink());
}
