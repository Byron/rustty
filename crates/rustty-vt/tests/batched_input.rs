use rustty_vt::{Color, Effect, EffectHandler, Terminal, snapshot, unicode};

#[derive(Default)]
struct Effects(Vec<Effect>);

impl EffectHandler for Effects {
    fn effect(&mut self, effect: Effect) {
        self.0.push(effect);
    }
}

fn deliver(terminal: &mut Terminal, input: &[u8], chunk_size: usize, handler: bool) -> Vec<Effect> {
    let mut effects = Effects::default();
    for chunk in input.chunks(chunk_size) {
        if handler {
            terminal.feed_with_handler(chunk, &mut effects);
        } else {
            effects.0.extend(terminal.feed(chunk));
        }
    }
    effects.0
}

fn same_state(actual: &Terminal, expected: &Terminal, context: &str) {
    assert_eq!(actual.generation, expected.generation, "{context}");
    assert_eq!(
        actual.synchronized_output_generation, expected.synchronized_output_generation,
        "{context}"
    );
    // Includes both screens, history, pens, modes, previous character, resources
    // and parser continuation, rather than only the visible text.
    assert!(
        snapshot::encode_to_vec(actual).unwrap() == snapshot::encode_to_vec(expected).unwrap(),
        "{context}: terminal snapshots differ"
    );
}

#[test]
fn printable_runs_match_scalar_printing() {
    let cases = [
        (
            "wrap and scroll",
            8,
            "",
            "abcdefgh01234567ijklmnopQRSTUVWXyz",
            "\x1b[3b",
        ),
        ("one column", 1, "", "abcd", "\x1b[b"),
        ("two columns", 2, "", "abcd", "\x1b[b"),
        ("no wrap", 8, "\x1b[?7l", "abcdefghijklmnop", "\x1b[3b"),
        ("insert", 8, "01234567\r\x1b[4h", "abcde", "\x1b[4l"),
        (
            "margins",
            8,
            "\x1b[?69h\x1b[2;6s\x1b[2;3r\x1b[?6h",
            "abcdefghijklmnop",
            "\x1b[3b",
        ),
        (
            "past right margin",
            8,
            "\x1b[?69h\x1b[2;6s\x1b[2;3r\x1b[2;8H",
            "abcdefghijk",
            "",
        ),
        ("UK charset", 8, "\x1b(A", "##ascii##", "\x1b(B\x1b[b"),
        ("line drawing", 8, "\x1b(0", "lqkxmj#q", "\x1b(B\x1b[b"),
        ("locking shift", 8, "\x1b)0\x0e", "lqkxmj#q", "\x0f\x1b[b"),
        ("single shift", 8, "\x1b*0\x1bN", "qqqq", "\x1b[b"),
        (
            "ASCII single shift",
            8,
            "\x1b(0\x1b*B\x1bN",
            "qqqq",
            "\x1b[b",
        ),
        (
            "status display",
            8,
            "Z\x1b[1$}",
            "ignored text",
            "\x1b[0$}\x1b[3b",
        ),
        (
            "protected styled link prompt",
            8,
            "\x1b[1;31;44m\x1b]8;id=run;https://example.org\x07\x1b[1\"q\x1b]133;P\x07",
            "abcdefghijklmnop",
            "\x1b[?2K",
        ),
        (
            "protected styled prompt",
            8,
            "\x1b[1;31;44m\x1b[1\"q\x1b]133;P\x07",
            "abcdefghijklmnop",
            "\x1b[?2K",
        ),
        (
            "input semantics",
            8,
            "\x1b]133;I\x07",
            "abcdefghijklmnop",
            "\r\noutput",
        ),
        (
            "wide and grapheme overwrite",
            8,
            "\x1b[?2027h界a\u{301}👩\u{200d}💻XYZ\x1b[H",
            "abcdefgh",
            "\u{301}\x1b[2b",
        ),
        ("wide tail overwrite", 8, "界界界界\x1b[1;2H", "abcdefg", ""),
        ("wrapped wide head", 8, "1234567界\x1b[2;1H", "abcdefgh", ""),
        ("wrapped wide tail", 8, "1234567界\x1b[2;2H", "abcdefg", ""),
        (
            "grapheme joins after ASCII",
            8,
            "\x1b[?2027h",
            "abc#",
            "\u{fe0f}\u{200d}😀\x1b[2b",
        ),
        (
            "alternate screen",
            8,
            "primary\x1b[?1049h",
            "abcdefghijklmnop",
            "\x1b[?1049l",
        ),
        (
            "grapheme mode",
            8,
            "\x1b[?2027h",
            "天地玄黄宇宙洪荒",
            "\x1b[3b",
        ),
        (
            "prepend prefix",
            8,
            "\x1b[?2027h\u{0d4e}",
            "ക天地",
            "\x1b[b",
        ),
        ("RI prefix", 8, "\x1b[?2027h🇦", "🇧🇨🇩🇪", "\x1b[b"),
        ("Hangul prefix", 8, "\x1b[?2027hᄀ", "ᅡᆨ나", "\x1b[b"),
        ("ZWJ prefix", 8, "\x1b[?2027h👩\u{200d}", "💻天地", "\x1b[b"),
        (
            "combining prefix",
            8,
            "\x1b[?2027ha",
            "\u{301}天地",
            "\x1b[b",
        ),
        (
            "variation prefix",
            8,
            "\x1b[?2027habcdefg#",
            "\u{fe0f}天地",
            "\x1b[b",
        ),
    ];
    for (name, cols, setup, text, suffix) in cases {
        let mut base = Terminal::new(cols, 3, 12);
        assert!(base.feed(setup.as_bytes()).is_empty(), "{name}");
        // Count every scalar even when a printable run wraps the generation.
        base.generation = u64::MAX - 2;
        // Apply the same mode, margin and existing-cell setups to non-ASCII
        // runs. Scalar print intentionally assigns every Latin-1 scalar width
        // one, including soft hyphen; grapheme handling starts above U+00FF.
        for text in std::iter::once(text).chain([
            "天地玄黄宇宙洪荒天地玄黄宇宙洪荒天地",
            "éÿ\u{ad}\u{a0}éÿ\u{ad}\u{a0}",
            "abc天地édef界gh",
            "a\u{301}b\u{302}界c\u{303}d\u{308}",
            "👩\u{200d}💻👨\u{200d}🚀天地",
            "\u{0d4e}ക\u{0d4e}界abc",
            "🇦🇧🇨🇩🇪🇫🇬",
            "각나다天地",
        ]) {
            let mut expected = base.clone();
            for cp in text.chars() {
                expected.print(cp);
            }
            for handler in [false, true] {
                for chunk_size in [usize::MAX, 1, 3, 7] {
                    let context =
                        format!("{name}, text={text:?}, handler={handler}, chunk={chunk_size}");
                    let mut actual = base.clone();
                    assert!(deliver(&mut actual, text.as_bytes(), chunk_size, handler).is_empty());
                    same_state(&actual, &expected, &context);
                    let mut finished = expected.clone();
                    assert_eq!(
                        deliver(&mut actual, suffix.as_bytes(), chunk_size, handler),
                        finished.feed(suffix.as_bytes()),
                        "{context}"
                    );
                    same_state(&actual, &finished, &context);
                }
            }
        }
    }
}

#[test]
fn zwj_pairs_preserve_scalar_admission_and_wrapping() {
    for columns in [1, 2, 7] {
        for alternate in [false, true] {
            for suffixes in [0, 1, 2, 3, 4, 7, 60, 61, 62, 63, 64] {
                for (prefix, input) in [
                    ("a", "\u{200d}界Z"),
                    ("☺", "\u{200d}❤Z"),
                    ("👩", "\u{200d}💻Z"),
                    ("👩", "\u{200d}\u{301}Z"),
                    ("👩", "\u{200d}AZ"),
                    ("👩", "\u{200d}\u{fe0e}Z"),
                    ("👩", "\u{200d}\u{fe0f}Z"),
                    ("👩", "\u{fe0e}\u{200d}💻Z"),
                ] {
                    let mut base = Terminal::new(columns, 1, 128);
                    base.feed(b"\x1b[?2027h");
                    if alternate {
                        base.feed(b"\x1b[?1049h");
                    }
                    base.feed(
                        "x".repeat(usize::from(columns.saturating_sub(2)))
                            .as_bytes(),
                    );
                    base.feed(prefix.as_bytes());
                    base.feed("\u{301}".repeat(suffixes).as_bytes());
                    base.generation = u64::MAX - 1;
                    let mut expected = base.clone();
                    for cp in input.chars() {
                        expected.print(cp);
                    }
                    for chunk in [1, 3, 7, usize::MAX] {
                        let mut actual = base.clone();
                        deliver(&mut actual, input.as_bytes(), chunk, false);
                        same_state(
                            &actual,
                            &expected,
                            &format!(
                                "cols={columns}, alt={alternate}, prefix={prefix}, suffixes={suffixes}, chunk={chunk}"
                            ),
                        );
                        assert_eq!(
                            actual.screen().owned_bytes(),
                            expected.screen().owned_bytes()
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn ignored_scalars_preserve_public_cursor_edits_before_batched_printing() {
    assert_eq!(unicode::codepoint_width('\u{200b}'), 0);
    for (ignored, suffixes) in [('\u{301}', 64), ('\u{200b}', 0)] {
        let mut base = Terminal::new(8, 3, 12);
        base.feed(b"\x1b[?2027h\x1b[31m\x1b]8;id=old;https://example.org\x07");
        base.feed(format!("A{}", "\u{301}".repeat(suffixes)).as_bytes());
        let cursor = &mut base.screen_mut().cursor;
        cursor.style.foreground = Color::Indexed(2);
        cursor.hyperlink = None;
        let mut expected = base.clone();
        expected.print(ignored);
        same_state(&expected, &base, "first scalar must be ignored");
        for cp in "BCD".chars() {
            expected.print(cp);
        }
        let input = format!("{ignored}BCD");
        for handler in [false, true] {
            for chunk_size in [usize::MAX, 1, 3, 7] {
                let mut actual = base.clone();
                assert!(deliver(&mut actual, input.as_bytes(), chunk_size, handler).is_empty());
                same_state(
                    &actual,
                    &expected,
                    &format!("ignored={ignored:?}, handler={handler}, chunk={chunk_size}"),
                );
            }
        }
    }
}

#[test]
fn packed_runs_cross_scratch_and_destination_boundaries() {
    for columns in [7, 17, 80] {
        let mut base = Terminal::new(columns, 4, 1000);
        base.feed(b"\x1b[?2027h");
        for col in 0..columns * 4 {
            base.feed(format!("\x1b[{}mX", 31 + col % 4).as_bytes());
        }
        base.feed("\x1b[H界a\u{301}👩\u{200d}💻\x1b[1;2H\x1b[1;34;44m".as_bytes());
        for pattern in ["a", "é", "界", "é界a\u{301}👩\u{200d}💻"] {
            for count in [1, 15, 16, 31, 32, 255, 256, 257, 513] {
                let text = pattern.repeat(count);
                let mut expected = base.clone();
                for cp in text.chars() {
                    expected.print(cp);
                }
                let mut actual = base.clone();
                actual.feed(text.as_bytes());
                same_state(
                    &actual,
                    &expected,
                    &format!("columns={columns}, pattern={pattern:?}, count={count}"),
                );
            }
        }
    }
}

#[test]
fn printable_runs_preserve_effect_order_and_cursor_queries() {
    let input = b"abc\x07\x1b[6nDEF\x1b]2;title\x07ghi\x1b[6njkl\x07";
    let effects = vec![
        Effect::Bell,
        Effect::Write(b"\x1b[1;4R".to_vec()),
        Effect::Title(b"title".to_vec()),
        Effect::Write(b"\x1b[2;2R".to_vec()),
        Effect::Bell,
    ];
    let mut expected = Terminal::new(8, 3, 12);
    assert_eq!(deliver(&mut expected, input, 1, false), effects);
    for handler in [false, true] {
        for chunk_size in [usize::MAX, 1, 3, 7] {
            let mut actual = Terminal::new(8, 3, 12);
            assert_eq!(deliver(&mut actual, input, chunk_size, handler), effects);
            same_state(&actual, &expected, "ordered effects");
        }
    }
}

#[test]
fn long_utf8_groups_and_malformed_tails_match_byte_delivery() {
    for pattern in ["éÿ", "水界", "😀😁", "é", "👩🏽‍💻", "界abcé"] {
        let input = [
            b"\x1b[?2027h\x1b[31m".as_slice(),
            pattern.repeat(257).as_bytes(),
            b"\xf0\x9f\x1b[0mTAIL\r\n\xc2\x85done",
        ]
        .concat();
        for alternate in [false, true] {
            let mut base = Terminal::new(73, 5, 128);
            if alternate {
                base.feed(b"\x1b[?1049h");
            }
            let mut expected = base.clone();
            let effects = deliver(&mut expected, &input, 1, false);
            for chunk in [8, 23, 255, 1024, usize::MAX] {
                for handler in [false, true] {
                    let mut actual = base.clone();
                    assert_eq!(deliver(&mut actual, &input, chunk, handler), effects);
                    same_state(
                        &actual,
                        &expected,
                        &format!("{pattern:?}, alt={alternate}, chunk={chunk}, handler={handler}"),
                    );
                }
            }
        }
    }
}

#[test]
fn printable_runs_resume_from_snapshots_at_every_byte_boundary() {
    let input = "\x1b[?2027habcdefgh\x1b[31m界a\u{301}👩\u{200d}💻\r\n\x1b*0\x1bNqqqq\x1b[2b\x1b[6n\x1b]2;title\x07done".as_bytes();
    let mut expected = Terminal::new(8, 3, 12);
    let effects = deliver(&mut expected, input, 1, false);
    for split in 0..=input.len() {
        let mut prefix = Terminal::new(8, 3, 12);
        let prefix_effects = deliver(&mut prefix, &input[..split], 1, false);
        let encoded = snapshot::encode_to_vec(&prefix).unwrap();
        let mut restored = snapshot::decode(encoded.as_slice(), Default::default()).unwrap();
        // Presentation generation is deliberately not persisted. Restored page
        // capacities may differ, so compare both deliveries from that state.
        restored.generation = prefix.generation;
        let mut scalar = restored.clone();
        let mut scalar_effects = prefix_effects.clone();
        scalar_effects.extend(deliver(&mut scalar, &input[split..], 1, false));
        assert_eq!(scalar_effects, effects, "split={split}");
        assert_eq!(scalar.plain_text(), expected.plain_text(), "split={split}");
        assert_eq!(scalar.generation, expected.generation, "split={split}");
        for handler in [false, true] {
            let mut actual = restored.clone();
            let mut actual_effects = prefix_effects.clone();
            actual_effects.extend(deliver(&mut actual, &input[split..], usize::MAX, handler));
            assert_eq!(actual_effects, effects, "split={split}, handler={handler}");
            same_state(
                &actual,
                &scalar,
                &format!("snapshot split={split}, handler={handler}"),
            );
        }
    }
}
