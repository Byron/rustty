use rustty_vt::{Terminal, unicode};

#[test]
fn unicode18_emoji_occupy_two_cells() {
    // Lighthouse, meteor, eraser, net, butterfly, pickle, cracking face and thumbs.
    let emoji = [
        '\u{1f6d9}',
        '\u{1fa8b}',
        '\u{1fa8c}',
        '\u{1fa8d}',
        '\u{1facc}',
        '\u{1fadd}',
        '\u{1faeb}',
        '\u{1faf9}',
        '\u{1fafa}',
    ];
    let text: String = emoji.into_iter().collect();
    let mut terminal = Terminal::new(20, 2, 0);
    terminal.feed(text.as_bytes());
    for (index, cp) in emoji.into_iter().enumerate() {
        assert_eq!(unicode::codepoint_width(cp), 2, "{cp:?}");
        let cells = terminal.screen().row(0).cells;
        assert_eq!(cells[index * 2].codepoint(), Some(cp));
        assert_eq!(cells[index * 2].width(), 2);
        assert_eq!(cells[index * 2 + 1].width(), 0);
    }
    assert_eq!(terminal.screen().cursor.col, 18);
}

#[test]
fn unicode18_linkers_and_emoji_sequences_use_updated_boundaries() {
    for text in [
        "a\u{094d}\u{0915}",
        "\u{094d}\u{0300}\u{200d}\u{0915}",
        "\u{1cf5}\u{0300}\u{0915}",
        "\u{1cf6}\u{0915}",
        "\u{11a3a}\u{11a0b}",
        "😀\u{094d}\u{0300}\u{0915}",
        "😀\u{094d}\u{0300}\u{200d}😀",
        "👍🏻\u{094d}\u{0915}",
        "\u{1faf9}🏻",
        "\u{1fafa}🏿",
    ] {
        assert_eq!(unicode::grapheme_width(text), (text.len(), 2), "{text:?}");
    }
    for (text, first) in [
        ("a\u{1cf5}\u{0915}", "a"),
        ("😀\u{1cf5}\u{0915}", "😀"),
        ("\u{0915}\u{0300}\u{0915}", "\u{0915}\u{0300}"),
        ("\u{094d}\u{200c}\u{0915}", "\u{094d}\u{200c}"),
    ] {
        assert_eq!(unicode::grapheme_width(text).0, first.len(), "{text:?}");
    }
}

#[test]
fn unicode18_clusters_survive_chunked_terminal_input() {
    let text = "a\u{094d}\u{0915}\u{1cf5}\u{0915}\u{1faf9}🏻";
    let mut terminal = Terminal::new(12, 2, 0);
    terminal.set_mode(true, 2027, true);
    let mut chunked = terminal.clone();
    terminal.feed(text.as_bytes());
    for byte in text.as_bytes() {
        chunked.feed(std::slice::from_ref(byte));
    }
    assert_eq!(
        rustty_vt::snapshot::encode_to_vec(&terminal).unwrap(),
        rustty_vt::snapshot::encode_to_vec(&chunked).unwrap()
    );
    for (col, expected) in [
        (0, "a\u{094d}\u{0915}"),
        (2, "\u{1cf5}\u{0915}"),
        (4, "\u{1faf9}🏻"),
    ] {
        assert_eq!(
            &*terminal.screen().cell_text(terminal.screen().row(0), col),
            expected
        );
        assert_eq!(terminal.screen().row(0).cells[col].width(), 2);
    }
}
