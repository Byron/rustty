use rustty_vt::{SemanticContent, Terminal, snapshot};

#[test]
fn reflow_copies_source_prompt_metadata_to_each_destination_segment() {
    for (kind, expected) in [
        ("i", SemanticContent::Prompt),
        ("s", SemanticContent::Input),
    ] {
        let mut terminal = Terminal::new(12, 4, 20);
        terminal
            .feed(format!("\x1b]133;A;redraw=0\x07\x1b]133;P;k={kind}\x07abcdefghij").as_bytes());
        terminal.resize(4, 4);
        for (row, text) in terminal.screen().rows[..3]
            .iter()
            .zip(["abcd", "efgh", "ij"])
        {
            assert_eq!(row.semantic, expected);
            assert_eq!(row.text(), text);
        }
        let bytes = snapshot::encode_to_vec(&terminal).unwrap();
        let mut terminal = snapshot::decode(bytes.as_slice(), Default::default()).unwrap();
        terminal.resize(12, 4);
        assert_eq!(terminal.screen().rows[0].semantic, expected);
        assert_eq!(terminal.screen().rows[0].text(), "abcdefghij");
    }
}
