use rustty_vt::{Effect, Terminal};

#[test]
fn cancelled_and_aborted_apc_commands_are_discarded_at_every_feed_boundary() {
    for command in [
        "Ga=t,f=24,s=1,v=1,i=7;AAAA",
        "25a1;r;cp=e000;AAAAAAAAAAAAAA==",
    ] {
        for ending in [0x18, 0x1a, 0x9b, 0x90, 0x9d, 0x85] {
            let mut input = format!("\x1b_{command}").into_bytes();
            input.push(ending);
            for split in 0..=input.len() {
                let mut terminal = Terminal::new(10, 3, 0);
                assert!(terminal.feed(&input[..split]).is_empty());
                assert!(terminal.feed(&input[split..]).is_empty());
                assert!(terminal.graphics().images.is_empty());
                assert!(terminal.glyphs.entries().is_empty());
                terminal.feed(b"\x18");
                assert_eq!(
                    terminal.feed(b"\x1b_Ga=t,f=24,s=1,v=1,i=7;AAAA\x1b\\"),
                    [Effect::Write(b"\x1b_Gi=7;OK\x1b\\".to_vec())],
                );
                assert!(terminal.graphics().images.contains_key(&7));
            }
        }
    }
}

#[test]
fn both_string_terminators_complete_apc_commands() {
    for ending in [b"\x1b\\".as_slice(), b"\x9c"] {
        let input = [b"\x1b_Ga=t,f=24,s=1,v=1,i=7;AAAA".as_slice(), ending].concat();
        let mut terminal = Terminal::new(10, 3, 0);
        assert_eq!(
            terminal.feed(&input),
            [Effect::Write(b"\x1b_Gi=7;OK\x1b\\".to_vec())],
        );
        assert!(terminal.graphics().images.contains_key(&7));
    }
}
