use rustty_vt::{Color, Effect, Terminal};

#[test]
fn host_reset_preserves_partial_utf8_and_ansi_controls() {
    let cases: &[(&[u8], usize, &str, Color)] = &[
        ("界".as_bytes(), 1, "界", Color::Default),
        (b"\x1b(0q", 2, "─", Color::Default),
        (b"\x1b[31mX", 4, "X", Color::Indexed(1)),
    ];
    for &(input, cut, text, foreground) in cases {
        let mut terminal = Terminal::new(8, 3, 0);
        terminal.feed(b"old");
        terminal.feed(&input[..cut]);
        terminal.reset();
        terminal.feed(&input[cut..]);
        let cell = &terminal.screen().rows[0].cells[0];
        assert_eq!(
            &*terminal.screen().cell_text(&terminal.screen().rows[0], 0),
            text
        );
        assert_eq!(cell.style.foreground, foreground);
        assert!(
            terminal
                .screen()
                .row_text(&terminal.screen().rows[1])
                .trim()
                .is_empty()
        );
    }
}

#[test]
fn host_reset_preserves_osc_dcs_and_apc_capture() {
    for (input, expected) in [
        (
            b"\x1b]2;retained title\x1b\\".as_slice(),
            Effect::Title(b"retained title".to_vec()),
        ),
        (
            b"\x1bP$qm\x1b\\".as_slice(),
            Effect::Write(b"\x1bP1$r0m\x1b\\".to_vec()),
        ),
        (
            b"\x1b_25a1;r;cp=e0a0;AAAAAAAAAAAAAA==\x1b\\".as_slice(),
            Effect::Write(b"\x1b_25a1;r;cp=e0a0;status=0\x1b\\".to_vec()),
        ),
    ] {
        // The final ESC commits these string commands before the backslash.
        for cut in 1..input.len() - 1 {
            let mut terminal = Terminal::new(8, 3, 0);
            assert!(terminal.feed(&input[..cut]).is_empty());
            terminal.reset();
            assert_eq!(
                terminal.feed(&input[cut..]).as_slice(),
                std::slice::from_ref(&expected)
            );
        }
    }
}

#[test]
fn host_reset_keeps_capture_overflow_and_ris_still_clears_state() {
    let mut terminal = Terminal::new(8, 3, 0);
    terminal.glyphs.set_apc_limit(Some(1));
    terminal.feed(b"\x1b_25a1;s;");
    terminal.reset();
    assert!(terminal.feed(b"ignored\x1b\\").is_empty());
    terminal.feed(b"\x1bP$qtoo");
    terminal.reset();
    assert!(terminal.feed(b"long\x1b\\").is_empty());

    terminal.feed(b"\x1b[31mold\x1bcnew");
    assert_eq!(
        &*terminal.screen().cell_text(&terminal.screen().rows[0], 0),
        "n"
    );
    assert_eq!(
        terminal.screen().rows[0].cells[0].style.foreground,
        Color::Default
    );
    assert_eq!(terminal.screen().cursor.col, 3);
}
