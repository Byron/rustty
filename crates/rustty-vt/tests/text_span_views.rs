use regex::Regex;
use rustty_vt::{
    Selection, Terminal,
    formatter::{Format, Options, TerminalExtra},
    search::LinkMatcher,
    selection::{DEFAULT_WORD_BOUNDARIES, SelectLine},
};

#[test]
fn explicit_spans_copy_search_and_export_the_whole_payload_from_every_column() {
    let text = " ab界e\u{301}<&  ";
    let regex = Regex::new("界e\u{301}").unwrap();
    for width in 1..=7 {
        let mut terminal = Terminal::new(24, 3, 100);
        terminal.feed(format!("x\x1b]66;w={width};{text}\x1b\\Z").as_bytes());
        assert_eq!(terminal.screen().cursor.col, width + 2);
        for col in 1..=width {
            for rectangular in [false, true] {
                let point = terminal.screen().point(0, col).unwrap();
                let selection = Selection {
                    start: point,
                    end: point,
                    rectangular,
                };
                assert_eq!(
                    terminal
                        .format_selection(selection, Options::default())
                        .unwrap(),
                    text.as_bytes(),
                    "width={width} col={col} rectangular={rectangular}",
                );
                terminal.screen_mut().selection = Some(selection);
                assert_eq!(
                    terminal
                        .screen()
                        .render_view()
                        .rows()
                        .next()
                        .unwrap()
                        .selection,
                    Some(1..=width),
                );
                let vt = terminal
                    .format_selection(selection, Options::new(Format::Vt))
                    .unwrap();
                let mut replay = Terminal::new(24, 3, 0);
                replay.feed(&vt);
                assert_eq!(replay.screen().row(0).cell_span(0).width as usize, width);
                assert_eq!(replay.screen().row(0).text(0).as_str(), text);
                assert_eq!(replay.screen().cursor.col, width);
            }
        }
        let matches = terminal.screen().search(&regex);
        assert_eq!(matches.len(), 1);
        assert_eq!((matches[0].start.col, matches[0].end.col), (1, 1));
        let matches = terminal.screen().search_literal(b" ab");
        assert_eq!(matches.len(), 1);
        assert_eq!((matches[0].start.col, matches[0].end.col), (1, 1));
        let html = terminal
            .format_selection(
                terminal.screen().selection.unwrap(),
                Options::new(Format::Html),
            )
            .unwrap();
        let html = String::from_utf8(html).unwrap();
        assert!(html.contains(&format!("width: {width}ch")));
        assert!(html.contains("&lt;&amp;"));
    }
}

#[test]
fn multiple_wrap_gap_cells_do_not_enter_copy_search_or_word_selection() {
    let mut terminal = Terminal::new(5, 3, 100);
    terminal.feed(b"abc\x1b]66;w=4;wide\x1b\\x");
    let mut formatter = terminal.screen().formatter(Format::Plain);
    formatter.options.unwrap = true;
    assert_eq!(formatter.format().unwrap(), b"abcwidex");
    assert_eq!(terminal.screen().search_literal(b"abcwide").len(), 1);
    assert_eq!(
        terminal
            .screen()
            .search(&Regex::new("abcwide").unwrap())
            .len(),
        1
    );
    for col in [3, 4] {
        let point = terminal.screen().point(0, col).unwrap();
        let word = terminal
            .screen()
            .select_word(point, DEFAULT_WORD_BOUNDARIES)
            .unwrap();
        assert_eq!(
            terminal.format_selection(word, Options::default()).unwrap(),
            b"abcwidex",
        );
    }
}

#[test]
fn explicit_leading_whitespace_is_retained_by_line_selection() {
    let mut terminal = Terminal::new(12, 3, 0);
    terminal.feed(b"\x1b]66;w=7;  word  \x1b\\");
    for col in 0..7 {
        let selection = terminal
            .screen()
            .select_line(
                terminal.screen().point(0, col).unwrap(),
                SelectLine::default(),
            )
            .unwrap();
        assert_eq!((selection.start.col, selection.end.col), (0, 6));
        assert_eq!(
            terminal
                .format_selection(selection, Options::default())
                .unwrap(),
            b"  word  ",
        );
    }
    assert!(terminal.screen().select_all().is_some());
}

#[test]
fn link_hints_use_one_target_per_atomic_span_and_tail_clicks_find_it() {
    let mut terminal = Terminal::new(24, 3, 0);
    terminal.feed(b"\x1b]66;w=7;https://a.test https://b.test\x1b\\");
    let mut matcher = LinkMatcher::default();
    let hints = matcher.viewport_hints(terminal.screen());
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].link.uri, "https://a.test");
    for col in 0..7 {
        let point = terminal.screen().point(0, col).unwrap();
        assert_eq!(
            matcher.link_at(terminal.screen(), point).unwrap().uri,
            "https://a.test"
        );
    }
    terminal.feed(b"\r\x1b]8;;https://explicit.test\x1b\\\x1b]66;w=7;label\x1b\\\x1b]8;;\x1b\\");
    let hints = matcher.viewport_hints(terminal.screen());
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].link.uri, "https://explicit.test");
}

#[test]
fn vt_export_restores_pending_wrap_on_an_explicit_tail() {
    let mut terminal = Terminal::new(7, 3, 0);
    terminal.feed(b"\x1b]66;w=7;edge\x1b\\");
    assert!(terminal.screen().cursor.pending_wrap);
    let mut formatter = terminal.formatter(Format::Vt);
    formatter.extra = TerminalExtra::ALL;
    let bytes = formatter.format().unwrap();
    let mut replay = Terminal::new(7, 3, 0);
    replay.feed(&bytes);
    assert!(replay.screen().cursor.pending_wrap);
    assert_eq!(replay.screen().cursor.col, 6);
    assert_eq!(replay.screen().row(0).text(0).as_str(), "edge");
    replay.feed(b"Z");
    assert_eq!(replay.screen().row(1).text(0).as_str(), "Z");
}
