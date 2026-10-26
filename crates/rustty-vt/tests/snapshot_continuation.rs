use rustty_vt::{Terminal, snapshot};

#[test]
fn snapshots_resume_c1_restarts_without_replaying_aborted_commands() {
    for (restart, rest) in [
        (b"\x9b31".as_slice(), b"mX".as_slice()),
        (b"\x9d2;title", b"\x07X"),
        (b"\x90$q", b"m\x1b\\X"),
    ] {
        let input = [b"\x1b_Ga=t,f=24,s=1,v=1,i=7;AAAA".as_slice(), restart].concat();
        for split in 0..=input.len() {
            let mut source = Terminal::new(10, 3, 0);
            source.feed(&input[..split]);
            source.feed(&input[split..]);
            let bytes = snapshot::encode_to_vec(&source).unwrap();
            let mut restored = snapshot::decode(bytes.as_slice(), Default::default()).unwrap();
            assert_eq!(source.feed(rest), restored.feed(rest));
            assert_eq!(source.screen().cursor, restored.screen().cursor);
            assert!(restored.graphics().images.is_empty());
            for (source_row, target_row) in source.screen().rows().zip(restored.screen().rows()) {
                assert_eq!(
                    source.screen().row_text(source_row),
                    restored.screen().row_text(target_row),
                );
            }
        }
    }
}
