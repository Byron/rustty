use rustty_parser::{CsiParam, Event, MAX_EXTENDED_PARAMS, MAX_PARAMS, Parser};

fn capture(parser: &mut Parser, bytes: &[u8]) -> Vec<(bool, Vec<CsiParam>)> {
    let mut commands = Vec::new();
    parser.advance(bytes, |event| match event {
        Event::Csi {
            params,
            colon_separators,
            ..
        } => commands.push((
            false,
            params
                .iter()
                .enumerate()
                .map(|(index, &value)| CsiParam {
                    value: u32::from(value),
                    colon_after: colon_separators & (1 << index) != 0,
                })
                .collect(),
        )),
        Event::CsiExtended {
            intermediates,
            params,
            final_byte,
        } => {
            assert_eq!(intermediates, b"> ");
            assert_eq!(final_byte, b'q');
            commands.push((true, params.to_vec()));
        }
        _ => {}
    });
    commands
}

#[test]
fn ordinary_events_keep_their_size_and_short_cursor_commands_stay_inline() {
    #[cfg(target_pointer_width = "64")]
    assert_eq!(std::mem::size_of::<Event<'_>>(), 40);
    let mut parser = Parser::new();
    assert_eq!(MAX_PARAMS, 24);
    assert_eq!(
        capture(&mut parser, b"\x1b[>0;4: q"),
        vec![(
            false,
            vec![
                CsiParam {
                    value: 0,
                    colon_after: false
                },
                CsiParam {
                    value: 4,
                    colon_after: true
                },
            ]
        )]
    );
    assert!(capture(&mut parser, b"\x1b[>0;4: r\x1b[1:2H").is_empty());
}

#[test]
fn long_commands_preserve_group_boundaries_and_large_coordinates_at_every_split() {
    let mut bytes = b"\x1b[>1;2".to_vec();
    let mut expected = vec![
        CsiParam {
            value: 1,
            colon_after: false,
        },
        CsiParam {
            value: 2,
            colon_after: true,
        },
    ];
    for index in 0..80 {
        let value = if index == 71 { 65536 } else { index + 1 };
        bytes.extend_from_slice(format!(":{value}").as_bytes());
        expected.push(CsiParam {
            value,
            colon_after: true,
        });
        if index == 35 {
            bytes.extend_from_slice(b";2");
            expected.last_mut().unwrap().colon_after = false;
            expected.push(CsiParam {
                value: 2,
                colon_after: true,
            });
        }
    }
    expected.last_mut().unwrap().colon_after = false;
    bytes.extend_from_slice(b" q");
    for split in 0..=bytes.len() {
        let mut parser = Parser::new();
        let mut commands = capture(&mut parser, &bytes[..split]);
        commands.extend(capture(&mut parser, &bytes[split..]));
        assert_eq!(commands, vec![(true, expected.clone())], "split {split}");
    }
}

#[test]
fn parameter_limit_is_atomic_and_accepts_a_final_separator() {
    for count in [23, 24, 25, MAX_EXTENDED_PARAMS, MAX_EXTENDED_PARAMS + 1] {
        for trailing in ["", ":", ";"] {
            let values = vec!["1"; count].join(":");
            let mut parser = Parser::new();
            let commands = capture(
                &mut parser,
                format!("\x1b[>{values}{trailing} q").as_bytes(),
            );
            if count > MAX_EXTENDED_PARAMS {
                assert!(commands.is_empty());
            } else {
                assert_eq!(commands.len(), 1);
                assert_eq!(commands[0].1.len(), count);
                assert_eq!(commands[0].1.last().unwrap().colon_after, trailing == ":");
            }
            assert_eq!(capture(&mut parser, b"\x1b[> q"), vec![(false, vec![])]);
        }
    }
}

#[test]
fn extended_numeric_range_does_not_change_legacy_saturation() {
    for prefix in ["", ">", "?"] {
        let commands = capture(
            &mut Parser::new(),
            format!("\x1b[{prefix}999999999999c").as_bytes(),
        );
        assert_eq!(
            commands[0],
            (
                false,
                vec![CsiParam {
                    value: 65535,
                    colon_after: false
                }]
            )
        );
    }
    let commands = capture(&mut Parser::new(), b"\x1b[>1;2:65536:4294967296 q");
    assert!(commands[0].0);
    assert_eq!(commands[0].1[2].value, 65536);
    assert_eq!(commands[0].1[3].value, u32::MAX);
    let mut hooks = Vec::new();
    Parser::new().advance(b"\x1bP>999999q\x1b\\", |event| {
        if let Event::DcsHook { params, .. } = event {
            hooks.push(params.to_vec());
        }
    });
    assert_eq!(hooks, vec![vec![65535]]);
}

#[test]
fn cancellation_restart_and_reset_discard_extended_state() {
    for cancel in [b"\x18".as_slice(), b"\x1a", b"\x1b[> q", b"\x9b> q"] {
        let mut parser = Parser::new();
        assert!(capture(&mut parser, b"\x1b[>1;2:999999999").is_empty());
        capture(&mut parser, cancel);
        assert_eq!(capture(&mut parser, b"\x1b[>100 q")[0].1[0].value, 100);
    }
    let mut parser = Parser::new();
    capture(&mut parser, b"\x1b[>1;2:999999999");
    parser.reset();
    assert_eq!(
        capture(&mut parser, b"\x1b[>100 q"),
        vec![(
            false,
            vec![CsiParam {
                value: 100,
                colon_after: false
            }]
        )]
    );
}

#[test]
fn extended_continuation_replays_without_a_new_wire_format() {
    let prefix = format!("\x1b[>1;2:{}:65536:", vec!["1"; 60].join(":"));
    for split in 0..=prefix.len() {
        let mut original = Parser::new();
        capture(&mut original, &prefix.as_bytes()[..split]);
        capture(&mut original, &prefix.as_bytes()[split..]);
        let continuation = original.continuation().unwrap();
        Parser::validate_continuation(&continuation).unwrap();
        assert_eq!(continuation, prefix.as_bytes());
        let mut restored = Parser::new();
        assert!(capture(&mut restored, &continuation).is_empty());
        assert_eq!(
            capture(&mut restored, b"2 q"),
            capture(&mut original, b"2 q")
        );
    }
}
