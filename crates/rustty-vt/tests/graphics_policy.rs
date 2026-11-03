use base64::{Engine, engine::general_purpose::STANDARD};
use rustty_vt::{Effect, Terminal};

const ERROR: &[u8] = b"\x1b_Gi=1;ENOTSUP: graphics disabled\x1b\\";

fn rejection() -> Effect {
    Effect::Write(ERROR.to_vec())
}

const RGB: &[u8] = b"\x1b_Ga=q,i=1,s=1,v=1,f=24;AQID\x1b\\";

fn png() -> String {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
    encoder.set_color(png::ColorType::Rgb);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&[1, 2, 3])
        .unwrap();
    STANDARD.encode(bytes)
}

#[test]
fn disabled_graphics_survives_resets_and_screen_creation_in_the_same_feed() {
    for prefix in [
        b"".as_slice(),
        b"\x1bc",
        b"\x1b[?1049h",
        b"\x1b[?47h\x1b[?47l\x1b[?1049h",
        b"\x1b[?1049h\x1bc",
    ] {
        let input = [prefix, RGB, b"text\x1b[c"].concat();
        for split in 0..=input.len() {
            let mut t = Terminal::new(10, 3, 10);
            t.set_graphics_limit(0);
            let mut effects = t.feed(&input[..split]);
            effects.extend(t.feed(&input[split..]));
            effects.retain(|effect| matches!(effect, Effect::Write(_)));
            assert_eq!(
                effects,
                [rejection(), Effect::Write(b"\x1b[?62;22c".to_vec())],
                "{prefix:?}, {split}"
            );
            assert_eq!(t.graphics().limit, 0);
            assert!(t.graphics().images.is_empty());
            assert!(t.graphics().placements.is_empty());
            assert_eq!(t.screen().cursor.col, 4);
            assert_eq!(t.screen().row(0).text(0).as_str(), "t");
            t.resize(12, 4);
            assert_eq!(t.feed(RGB), [rejection()]);
        }
    }
}

#[test]
fn disabled_graphics_discards_rgb_png_chunks_and_cancelled_transfers() {
    for (format, payload) in [(24, "AQID".to_owned()), (100, png())] {
        for action in ['q', 't', 'T'] {
            for ending in ["\x1b\\", "\u{18}", "\u{1a}"] {
                let input =
                    format!("\x1b_Ga={action},i=1,s=1,v=1,f={format};{payload}{ending}ok\x1b[c");
                for split in 0..=input.len() {
                    let mut t = Terminal::new(10, 3, 0);
                    t.set_graphics_limit(0);
                    let mut effects = t.feed(&input.as_bytes()[..split]);
                    effects.extend(t.feed(&input.as_bytes()[split..]));
                    let mut expected = Vec::new();
                    if ending == "\x1b\\" {
                        expected.push(rejection());
                    }
                    expected.push(Effect::Write(b"\x1b[?62;22c".to_vec()));
                    assert_eq!(effects, expected);
                    assert!(t.graphics().images.is_empty());
                    assert!(t.graphics().placements.is_empty());
                    assert_eq!(t.screen().cursor.col, 2);
                }
            }
            let input = format!(
                "\x1b_Ga={action},i=1,s=1,v=1,f={format},m=1;{}\x1b\\\x1b_Gm=0;{}\x1b\\",
                &payload[..2],
                &payload[2..]
            );
            let mut t = Terminal::new(10, 3, 0);
            t.set_graphics_limit(0);
            let mut effects = Vec::new();
            for byte in input.bytes() {
                effects.extend(t.feed(&[byte]));
            }
            assert_eq!(
                effects,
                [rejection()],
                "the first chunk rejects immediately with its ID"
            );
            t.set_graphics_limit(320_000_000);
            assert_eq!(t.feed(RGB), [Effect::Write(b"\x1b_Gi=1;OK\x1b\\".to_vec())]);
        }
    }
}

#[test]
fn restoring_view_graphics_cannot_enable_a_disabled_host() {
    let mut source = Terminal::new(10, 3, 0);
    source.feed(b"\x1b_Ga=T,i=1,s=1,v=1,f=24;AQID\x1b\\");
    let mut target = Terminal::new(10, 3, 0);
    target.set_graphics_limit(0);
    target
        .restore_view_graphics(source.graphics().view_snapshot(), None)
        .unwrap();
    assert_eq!(target.graphics().limit, 0);
    assert!(target.graphics().images.is_empty());
    assert!(target.graphics().placements.is_empty());
    assert_eq!(target.feed(RGB), [rejection()]);
}

#[test]
fn graphical_hosts_keep_rgb_and_png_support() {
    for (format, payload) in [(24, "AQID".to_owned()), (100, png())] {
        let mut t = Terminal::new(10, 3, 0);
        for action in ['q', 'T'] {
            assert_eq!(
                t.feed(
                    format!("\x1b_Ga={action},i=1,s=1,v=1,f={format};{payload}\x1b\\").as_bytes()
                ),
                [Effect::Write(b"\x1b_Gi=1;OK\x1b\\".to_vec())]
            );
        }
        assert_eq!(t.graphics().images.len(), 1);
        assert_eq!(t.graphics().placements.len(), 1);
        t.set_graphics_limit(0);
        assert!(t.graphics().images.is_empty());
        assert!(t.graphics().placements.is_empty());
        assert_eq!(t.feed(b"\x1b_Ga=p,i=1;\x1b\\"), [rejection()]);
        assert_eq!(t.feed(b"\x1b[?u"), [Effect::Write(b"\x1b[?0u".to_vec())]);
        assert!(matches!(
            t.feed(b"\x1b]52;c;YQ==\x07").as_slice(),
            [Effect::ClipboardWrite(_)]
        ));
    }
}

#[test]
fn streaming_and_visual_restores_preserve_disabled_graphics() {
    use rustty_vt::{
        snapshot::{self, Decoder},
        visual::VisualSnapshot,
    };
    let mut source = Terminal::new(10, 3, 10);
    source.feed(b"\x1b_Ga=T,i=1,s=1,v=1,f=24;AQID\x1b\\");
    let bytes = snapshot::encode_to_vec(&source).unwrap();
    let mut decoder = Decoder::new(bytes.as_slice(), Default::default());
    let mut target = decoder.ready().unwrap();
    target.set_graphics_limit(0);
    while decoder.next(&mut target).unwrap().is_some() {}
    assert_eq!(target.graphics().limit, 0);
    assert!(target.graphics().images.is_empty());
    assert!(target.graphics().placements.is_empty());
    let visual = VisualSnapshot::capture(&source).unwrap();
    visual.restore_into(&mut target).unwrap();
    assert_eq!(target.graphics().limit, 0);
    assert!(target.graphics().images.is_empty());
    assert!(target.graphics().placements.is_empty());
    assert_eq!(target.feed(RGB), [rejection()]);
}

#[test]
fn disabled_graphics_rejects_probes_with_identifiers_and_honors_quiet() {
    for quiet in [0, 1, 2, 3] {
        for (format, payload) in [(24, "AQID".to_owned()), (100, png())] {
            for action in ['q', 't', 'T', 'p'] {
                let input = format!(
                    "\x1b_Ga={action},i=42,p=7,s=1,v=1,f={format},q={quiet};{payload}\x1b\\ok\x1b[c"
                );
                for split in 0..=input.len() {
                    let mut t = Terminal::new(10, 3, 0);
                    t.set_graphics_limit(0);
                    let mut effects = t.feed(&input.as_bytes()[..split]);
                    effects.extend(t.feed(&input.as_bytes()[split..]));
                    let mut expected = Vec::new();
                    if quiet < 2 {
                        expected.push(Effect::Write(
                            b"\x1b_Gi=42,p=7;ENOTSUP: graphics disabled\x1b\\".to_vec(),
                        ));
                    }
                    expected.push(Effect::Write(b"\x1b[?62;22c".to_vec()));
                    assert_eq!(
                        effects, expected,
                        "{action}, {format}, quiet={quiet}, split={split}"
                    );
                    assert!(t.graphics().images.is_empty());
                    assert!(t.graphics().placements.is_empty());
                    assert_eq!(t.screen().cursor.col, 2);
                }
            }
        }
    }
}

#[test]
fn rejection_reuses_image_numbers_duplicate_fields_and_unaddressed_reply_rules() {
    for (command, expected) in [
        (
            "Gi=1,q=2,q=1;",
            b"\x1b_Gi=1;ENOTSUP: graphics disabled\x1b\\".as_slice(),
        ),
        (
            "GI=7,p=8;",
            b"\x1b_GI=7,p=8;ENOTSUP: graphics disabled\x1b\\",
        ),
        (
            "Gi=4294967295;",
            b"\x1b_Gi=4294967295;ENOTSUP: graphics disabled\x1b\\",
        ),
        ("Gi=1,q=1,q=2;", b""),
        ("Ga=q;", b""),
        ("Gi=bad;", b""),
        ("Ga=x,i=1;", b""),
    ] {
        let input = format!("\x1b_{command}not-even-base64\x1b\\");
        for split in 0..=input.len() {
            let mut terminal = Terminal::new(10, 3, 0);
            terminal.set_graphics_limit(0);
            let mut effects = terminal.feed(&input.as_bytes()[..split]);
            effects.extend(terminal.feed(&input.as_bytes()[split..]));
            let expected = if expected.is_empty() {
                Vec::new()
            } else {
                vec![Effect::Write(expected.to_vec())]
            };
            assert_eq!(effects, expected, "{command}, split={split}");
        }
    }
}
