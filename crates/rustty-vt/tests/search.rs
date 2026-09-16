use regex::Regex;
use rustty_vt::{GridPoint, Screen, Terminal};

fn coordinates(screen: &Screen, point: GridPoint) -> (usize, usize) {
    (
        point.col,
        screen
            .all_rows()
            .position(|row| row.id == point.row)
            .unwrap(),
    )
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn native_page_order_keeps_active_groups_and_repeated_soft_overlap() {
    // The native macOS ARM64 pool holds 46 rows at 1024 columns.
    let mut terminal = Terminal::new(1024, 64, 1000);
    terminal.feed(&[b"X\r\n".repeat(63), b"X".to_vec()].concat());
    let screen = terminal.screen();
    let found = screen.search_literal(b"X");
    let positions: Vec<_> = found
        .iter()
        .map(|found| coordinates(screen, found.start))
        .collect();
    assert_eq!(positions.len(), 64);
    assert_eq!(positions[0], (0, 45));
    assert_eq!(positions[45], (0, 0));
    assert_eq!(positions[46], (0, 63));
    assert_eq!(positions[63], (0, 46));

    let mut terminal = Terminal::new(1024, 4, 1000);
    let row = [vec![b'.'; 1023], b"X".to_vec()].concat();
    terminal.feed(&row.repeat(64));
    let screen = terminal.screen();
    let found = screen.search_literal(b"X.");
    assert_eq!(found.len(), 108);
    assert_eq!(coordinates(screen, found[0].start), (1023, 44));
    assert_eq!(
        found
            .iter()
            .filter(|found| coordinates(screen, found.start) == (1023, 44))
            .count(),
        2
    );
}

#[test]
fn no_scrollback_uses_the_strict_match_endpoint_at_active_top_left() {
    let mut terminal = Terminal::with_limits(8, 4, rustty_vt::ScrollbackLimits::NONE);
    terminal.feed(b"X");
    assert!(terminal.screen().search_literal(b"X").is_empty());
    assert!(terminal.screen().search_literal(b"X\n").is_empty());
    terminal.feed(b"Y");
    let screen = terminal.screen();
    let found = screen.search_literal(b"XY");
    assert_eq!(found.len(), 1);
    assert_eq!(coordinates(screen, found[0].start), (0, 0));
    assert_eq!(coordinates(screen, found[0].end), (1, 0));
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn no_scrollback_prunes_a_prefix_without_filtering_later_page_results() {
    let mut terminal = Terminal::with_limits(
        1024,
        64,
        rustty_vt::ScrollbackLimits {
            bytes: Some(0),
            lines: None,
        },
    );
    terminal.feed(b"X\x1b[64;1HX\x1b[22J\x1b[HX\x1b[64;1HX");
    let screen = terminal.screen();
    assert_eq!(screen.history_len(), 64);
    let positions: Vec<_> = screen
        .search_literal(b"X")
        .iter()
        .map(|found| coordinates(screen, found.start))
        .collect();
    assert_eq!(positions, [(0, 64), (0, 63), (0, 127)]);
}

fn search(cols: u16, text: &str, needle: &[u8]) -> Vec<[(usize, usize); 2]> {
    let mut terminal = Terminal::new(cols, 4, 100);
    terminal.feed(text.as_bytes());
    let screen = terminal.screen();
    screen
        .search_literal(needle)
        .into_iter()
        .map(|found| {
            [
                coordinates(screen, found.start),
                coordinates(screen, found.end),
            ]
        })
        .collect()
}

#[test]
fn literal_search_overlaps_and_folds_only_ascii_case() {
    assert_eq!(
        search(8, "abABaba", b"aBa"),
        [[(4, 0), (6, 0)], [(2, 0), (4, 0)], [(0, 0), (2, 0)]]
    );
    assert_eq!(
        search(8, "aAaAa", b"aa"),
        [
            [(3, 0), (4, 0)],
            [(2, 0), (3, 0)],
            [(1, 0), (2, 0)],
            [(0, 0), (1, 0)]
        ]
    );
    assert_eq!(search(8, "Ééé", "É".as_bytes()), [[(0, 0), (0, 0)]]);
    assert_eq!(
        search(8, "Ééé", "é".as_bytes()),
        [[(2, 0), (2, 0)], [(1, 0), (1, 0)]]
    );
    assert_eq!(search(8, "a.b ab", b"a.b"), [[(0, 0), (2, 0)]]);
    assert!(search(8, "abc", b"").is_empty());
    assert!(search(8, "abc", b"abcdefghijkl").is_empty());
}

#[test]
fn byte_needles_can_match_inside_unicode_cells() {
    assert!(search(8, "abc", b"\xff").is_empty());
    assert_eq!(
        search(8, "a界e\u{301}🙂界", b"\xe7"),
        [[(6, 0), (6, 0)], [(1, 0), (1, 0)]]
    );
    assert_eq!(
        search(8, "a界e\u{301}🙂界", b"\x95\x8ce"),
        [[(1, 0), (3, 0)]]
    );
    assert_eq!(
        search(8, "e\u{301}e\u{301}", b"\x81"),
        [[(1, 0), (1, 0)], [(0, 0), (0, 0)]]
    );
    // A space's combining codepoints disappear with the trimmed space, as in
    // the native plain formatter. This must not change regex/link input.
    assert!(search(8, "a \u{301}b", "\u{301}".as_bytes()).is_empty());
}

#[test]
fn whitespace_keeps_native_byte_mapping_instead_of_normalizing_endpoints() {
    assert_eq!(
        search(8, "a  b", b" "),
        [[(1, 0), (1, 0)], [(2, 0), (2, 0)]]
    );
    assert_eq!(search(8, "a  b", b"  "), [[(2, 0), (1, 0)]]);
    assert_eq!(
        search(4, "a    b", b"  "),
        [[(2, 0), (1, 0)], [(3, 0), (2, 0)], [(0, 1), (3, 0)]]
    );
    // A wide character's skipped spacer head still participates in the
    // formatter's backward coordinate walk for preceding blanks.
    assert_eq!(
        search(4, "a  界", b" "),
        [[(2, 0), (2, 0)], [(3, 0), (3, 0)]]
    );
    assert!(search(8, "a   \r\nb   ", b" ").is_empty());
}

#[test]
fn hard_line_breaks_and_trailing_newline_use_native_coordinates() {
    assert_eq!(search(8, "a\r\nb", b"a\nb"), [[(0, 0), (0, 1)]]);
    assert_eq!(
        search(8, "a\r\n\r\nb", b"\n"),
        [[(0, 2), (0, 2)], [(0, 1), (0, 1)], [(0, 0), (0, 0)]]
    );
    assert_eq!(search(8, "a\r\n\r\nb", b"\n\n"), [[(0, 0), (0, 1)]]);
    assert_eq!(search(8, "a\r\n\r\n", b"\n"), [[(0, 0), (0, 0)]]);
    assert_eq!(search(8, "", b"\n"), [[(0, 0), (0, 0)]]);
    assert_eq!(
        search(8, "  \r\n   ", b"\n"),
        [[(0, 0), (0, 0)], [(0, 0), (0, 0)]]
    );
}

#[test]
fn literal_search_keeps_history_soft_wraps_and_regex_semantics_separate() {
    let mut terminal = Terminal::new(8, 2, 100);
    terminal.feed(b"cat cat\r\ncatcatcat\r\nCAT");
    let screen = terminal.screen();
    let matches: Vec<_> = screen
        .search_literal(b"cat")
        .iter()
        .map(|found| {
            [
                coordinates(screen, found.start),
                coordinates(screen, found.end),
            ]
        })
        .collect();
    assert_eq!(
        matches,
        [
            [(0, 3), (2, 3)],
            [(6, 1), (0, 2)],
            [(3, 1), (5, 1)],
            [(0, 1), (2, 1)],
            [(4, 0), (6, 0)],
            [(0, 0), (2, 0)]
        ]
    );
    assert_eq!(screen.search(&Regex::new("cat").unwrap()).len(), 5);

    let mut terminal = Terminal::new(8, 4, 100);
    terminal.feed(b"abababa\r\na  b");
    let screen = terminal.screen();
    assert_eq!(screen.search(&Regex::new("aba").unwrap()).len(), 2);
    let spaces: Vec<_> = screen
        .search(&Regex::new(" ").unwrap())
        .iter()
        .map(|found| coordinates(screen, found.start))
        .collect();
    assert_eq!(spaces, [(2, 1), (1, 1)]);
    assert!(screen.search(&Regex::new("a\na").unwrap()).is_empty());
}
