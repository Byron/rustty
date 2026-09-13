use rustty_vt::{HyperlinkId, SemanticContent, Terminal};

fn link(params: &[u8], uri: &[u8]) -> Vec<u8> {
    let mut bytes = b"\x1b]8;".to_vec();
    bytes.extend_from_slice(params);
    bytes.push(b';');
    bytes.extend_from_slice(uri);
    bytes.push(7);
    bytes
}

#[test]
fn hyperlink_options_keep_the_last_nonempty_id_until_traversal_stops() {
    for (params, expected) in [
        (b"id=first:id=last:id=".as_slice(), Some(b"last".as_slice())),
        (b"id=first::id=ignored", Some(b"first")),
        (b"id=first:broken:id=ignored", Some(b"first")),
        (b"=value:id=found", Some(b"found")),
        (b"id=:id=last", Some(b"last")),
        (b"id=first:id=\xff", Some(b"\xff")),
        (b":id=ignored", None),
        (b"broken:id=ignored", None),
    ] {
        let mut terminal = Terminal::new(10, 2, 0);
        terminal.feed(&link(params, b"https://example.org"));
        assert_eq!(
            terminal.screen().cursor.hyperlink_id,
            Some(expected.map_or(HyperlinkId::Implicit(0), |id| {
                HyperlinkId::Explicit(id.to_vec())
            })),
            "{params:?}"
        );
        terminal.feed(&link(b"", b"https://next.example.org"));
        assert_eq!(
            terminal.screen().cursor.hyperlink_id,
            Some(HyperlinkId::Implicit(u32::from(expected.is_none())))
        );
    }
}

#[test]
fn invalid_link_endings_preserve_the_prior_link_and_snapshot_identity() {
    let mut terminal = Terminal::new(10, 2, 0);
    terminal.feed(&link(b"id=retained", b"https://raw/\xff"));
    terminal.feed(b"a");
    terminal.feed(&link(b"id=invalid:id=", b""));
    terminal.feed(b"b");
    let snapshot = rustty_vt::snapshot::encode_to_vec(&terminal).unwrap();
    let mut terminal =
        rustty_vt::snapshot::decode(snapshot.as_slice(), Default::default()).unwrap();
    for cell in &terminal.screen().rows[0].cells[..2] {
        assert_eq!(
            cell.hyperlink_raw.as_deref(),
            Some(b"https://raw/\xff".as_slice())
        );
        assert_eq!(
            cell.hyperlink_id,
            Some(HyperlinkId::Explicit(b"retained".to_vec()))
        );
    }
    assert_eq!(
        terminal.screen().cursor.hyperlink_id,
        Some(HyperlinkId::Explicit(b"retained".to_vec()))
    );
    // The incomplete option stops traversal before the ID, so this is a
    // valid end command with no recognized explicit ID.
    terminal.feed(&link(b"broken:id=ignored", b""));
    assert!(terminal.screen().cursor.hyperlink.is_none());
}

#[test]
fn restoring_a_cursor_preserves_the_active_hyperlink() {
    for (save, restore) in [
        (b"\x1b7".as_slice(), b"\x1b8".as_slice()),
        (b"\x1b[s", b"\x1b[u"),
        (b"\x1b[?1048h", b"\x1b[?1048l"),
    ] {
        let mut terminal = Terminal::new(10, 2, 0);
        terminal.feed(&link(b"id=saved", b"saved"));
        terminal.feed(save);
        terminal.feed(&link(b"id=current", b"current/\xff"));
        terminal.feed(restore);
        terminal.feed(b"A");
        assert_eq!(
            terminal.screen().rows[0].cells[0].hyperlink_raw.as_deref(),
            Some(b"current/\xff".as_slice()),
        );
        assert_eq!(
            terminal.screen().cursor.hyperlink_id,
            Some(HyperlinkId::Explicit(b"current".to_vec())),
        );
        terminal.feed(&link(b"", b""));
        terminal.feed(restore);
        assert!(terminal.screen().cursor.hyperlink.is_none());
    }
    let mut terminal = Terminal::new(10, 2, 0);
    terminal.feed(&link(b"", b"saved"));
    terminal.feed(b"A\x1b[?1049h\x1b[?1049lB");
    assert!(terminal.screen().rows[0].cells[1].hyperlink.is_none());
    assert!(terminal.screen().cursor.hyperlink.is_none());
}

#[test]
fn restoring_a_cursor_does_not_restore_or_default_semantic_content() {
    for save in [false, true] {
        let mut terminal = Terminal::new(10, 2, 0);
        terminal.screen_mut().cursor.semantic = SemanticContent::Prompt;
        if save {
            terminal.save_cursor();
        }
        terminal.screen_mut().cursor.semantic = SemanticContent::Input;
        terminal.restore_cursor();
        terminal.feed(b"A");
        assert_eq!(
            terminal.screen().rows[0].cells[0].semantic,
            SemanticContent::Input
        );
    }
}
