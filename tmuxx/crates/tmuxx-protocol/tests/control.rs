use tmuxx_protocol::{
    DecodeError, Decoder, Event, Guard, Notification, encode_command_result, encode_output,
    escape_output, unescape_output,
};

fn decode_chunks(bytes: &[u8], chunk: usize) -> Vec<Event> {
    let mut decoder = Decoder::default();
    let mut events = Vec::new();
    for bytes in bytes.chunks(chunk) {
        decoder
            .feed_into(bytes, |event| events.push(event))
            .unwrap();
    }
    decoder.finish().unwrap();
    events
}

#[test]
fn all_bytes_round_trip_at_every_fragment_size() {
    let bytes: Vec<u8> = (0..=255).collect();
    let escaped = escape_output(&bytes);
    assert!(!escaped.contains(&b'\n'));
    assert_eq!(unescape_output(&escaped).unwrap(), bytes);
    // C leaves DEL and high bytes intact rather than imposing UTF-8.
    assert!(escaped.ends_with(&(93..=255).collect::<Vec<_>>()));
    let transcript = encode_output(17, &bytes);
    let expected = vec![Event::Notification(Notification::Output {
        pane_id: 17,
        data: bytes,
        age_ms: None,
    })];
    for chunk in 1..=transcript.len() {
        assert_eq!(decode_chunks(&transcript, chunk), expected, "chunk {chunk}");
    }
}

#[test]
fn guards_are_exact_and_body_is_opaque() {
    let guard = Guard {
        time: -5,
        number: 812,
        flags: 1,
    };
    let body = b"%begin 1 2 3\n%end -5 813 1\n%error -5 812 0\n%pause %0\n\xff\n";
    let mut transcript = encode_command_result(guard, body, false);
    transcript.extend_from_slice(b"%sessions-changed\n%exit detached\n");
    let expected = vec![
        Event::Begin(guard),
        Event::CommandOutput(b"%begin 1 2 3".to_vec()),
        Event::CommandOutput(b"%end -5 813 1".to_vec()),
        Event::CommandOutput(b"%error -5 812 0".to_vec()),
        Event::CommandOutput(b"%pause %0".to_vec()),
        Event::CommandOutput(vec![255]),
        Event::End {
            guard,
            failed: false,
        },
        Event::Notification(Notification::SessionsChanged),
        Event::Notification(Notification::Exit {
            reason: Some(b"detached".to_vec()),
        }),
    ];
    for split in 0..=transcript.len() {
        let mut decoder = Decoder::default();
        let mut events = decoder.feed(&transcript[..split]).unwrap();
        events.extend(decoder.feed(&transcript[split..]).unwrap());
        decoder.finish().unwrap();
        assert_eq!(events, expected, "split {split}");
    }
}

#[test]
fn errors_and_empty_results_keep_their_guards() {
    let guard = Guard {
        time: 10,
        number: 88,
        flags: 0,
    };
    assert_eq!(
        decode_chunks(&encode_command_result(guard, b"", true), 1),
        [
            Event::Begin(guard),
            Event::End {
                guard,
                failed: true
            },
        ]
    );
}

#[test]
fn extended_output_ignores_future_header_fields() {
    assert_eq!(
        decode_chunks(
            b"%extended-output %3 10 future fields : a\\000\\012\\134\xff\n",
            1
        ),
        [Event::Notification(Notification::Output {
            pane_id: 3,
            data: vec![b'a', 0, 10, 92, 255],
            age_ms: Some(10)
        }),]
    );
    assert_eq!(
        decode_chunks(b"%extended-output %3 10 : \n", 2),
        [Event::Notification(Notification::Output {
            pane_id: 3,
            data: vec![],
            age_ms: Some(10)
        }),]
    );
}

#[test]
fn known_notifications_preserve_names_and_unknown_records() {
    let transcript = b"%session-changed $4 two words\n%session-renamed $4 a  b\n%session-renamed old name\n%layout-change @2 legacy visible *\n%window-add @2\n%unlinked-window-add @3\n%window-close @2\n%unlinked-window-close @3\n%window-renamed @3 raw\xff name\n%unlinked-window-renamed @4 other\n%window-pane-changed @3 %8\n%session-window-changed $4 @3\n%pane-mode-changed %8\n%pause %8\n%continue %8\n%client-detached /dev/pts/1\n%client-session-changed /dev/pts/2 $4 my session\n%paste-buffer-changed 0\n%paste-buffer-deleted 0\n%config-error trouble\n%message hi\n%subscription-changed sub $4 @3 0 %8 future : a:b\n%future-event \xff  \n";
    use Notification::*;
    let expected = vec![
        SessionChanged {
            session_id: 4,
            name: b"two words".to_vec(),
        },
        SessionRenamed {
            session_id: Some(4),
            name: b"a  b".to_vec(),
        },
        SessionRenamed {
            session_id: None,
            name: b"old name".to_vec(),
        },
        LayoutChange {
            window_id: 2,
            layout: b"legacy".to_vec(),
            visible_layout: b"visible".to_vec(),
            flags: b"*".to_vec(),
        },
        WindowAdd {
            window_id: 2,
            linked: true,
        },
        WindowAdd {
            window_id: 3,
            linked: false,
        },
        WindowClose {
            window_id: 2,
            linked: true,
        },
        WindowClose {
            window_id: 3,
            linked: false,
        },
        WindowRenamed {
            window_id: 3,
            name: b"raw\xff name".to_vec(),
            linked: true,
        },
        WindowRenamed {
            window_id: 4,
            name: b"other".to_vec(),
            linked: false,
        },
        WindowPaneChanged {
            window_id: 3,
            pane_id: 8,
        },
        SessionWindowChanged {
            session_id: 4,
            window_id: 3,
        },
        PaneModeChanged { pane_id: 8 },
        Pause { pane_id: 8 },
        Continue { pane_id: 8 },
        ClientDetached {
            client: b"/dev/pts/1".to_vec(),
        },
        ClientSessionChanged {
            client: b"/dev/pts/2".to_vec(),
            session_id: 4,
            name: b"my session".to_vec(),
        },
        PasteBufferChanged {
            name: b"0".to_vec(),
        },
        PasteBufferDeleted {
            name: b"0".to_vec(),
        },
        ConfigError(b"trouble".to_vec()),
        Message(b"hi".to_vec()),
        SubscriptionChanged {
            fields: [b"sub".as_slice(), b"$4", b"@3", b"0", b"%8", b"future"]
                .iter()
                .map(|s| s.to_vec())
                .collect(),
            value: b"a:b".to_vec(),
        },
        Unknown(b"%future-event \xff  ".to_vec()),
    ]
    .into_iter()
    .map(Event::Notification)
    .collect::<Vec<_>>();
    for split in 0..=transcript.len() {
        let mut decoder = Decoder::default();
        let mut events = decoder.feed(&transcript[..split]).unwrap();
        events.extend(decoder.feed(&transcript[split..]).unwrap());
        decoder.finish().unwrap();
        assert_eq!(events, expected, "split {split}");
    }
}

#[test]
fn malformed_records_fail_without_panicking() {
    for input in [
        b"%begin 1 2\n".as_slice(),
        b"%begin 1 2 3 4\n",
        b"%begin 1 18446744073709551616 0\n",
        b"%end 1 2 3\n",
        b"%output wrong hi\n",
        b"%output %0 \\0\n",
        b"%output %0 \\400\n",
        b"%extended-output %0 4 not-a-colon\n",
        b"%layout-change @0 only\n",
        b"%subscription-changed a b : value\n",
    ] {
        let mut decoder = Decoder::default();
        assert!(decoder.feed(input).is_err(), "input {input:?}");
        assert_eq!(decoder.feed(b"%exit\n"), Err(DecodeError::Failed));
    }
    for invalid in [
        b"\\".as_slice(),
        b"\\0",
        b"\\00",
        b"\\0000\\",
        b"\\888",
        b"\\377\\400",
    ] {
        assert_eq!(unescape_output(invalid), Err(DecodeError::InvalidEscape));
    }
}

#[test]
fn deferred_output_outside_guards_survives_every_byte_boundary() {
    let transcript = b"%begin 1 2 1\n%end 1 2 1\njob output\nfile: denied\n\xff\n%begin 1 3 1\nbarrier\n%end 1 3 1\n";
    let expected = Decoder::default().feed(transcript).unwrap();
    assert_eq!(
        &expected[2..5],
        &[
            Event::UnframedOutput(b"job output".to_vec()),
            Event::UnframedOutput(b"file: denied".to_vec()),
            Event::UnframedOutput(vec![0xff]),
        ]
    );
    for split in 0..=transcript.len() {
        let mut decoder = Decoder::default();
        let mut events = decoder.feed(&transcript[..split]).unwrap();
        events.extend(decoder.feed(&transcript[split..]).unwrap());
        decoder.finish().unwrap();
        assert_eq!(events, expected, "split {split}");
    }
}

#[test]
fn limits_apply_to_records_not_entire_replies() {
    let guard = Guard {
        time: 1,
        number: 1,
        flags: 1,
    };
    let mut bytes = b"%begin 1 1 1\n".to_vec();
    for _ in 0..10_000 {
        bytes.extend_from_slice(b"data\n");
    }
    bytes.extend_from_slice(b"%end 1 1 1\n");
    let mut decoder = Decoder::with_limit(12);
    let mut count = 0;
    decoder.feed_into(&bytes, |_| count += 1).unwrap();
    decoder.finish().unwrap();
    assert_eq!(count, 10_002);
    assert_eq!(
        guard,
        Guard {
            time: 1,
            number: 1,
            flags: 1
        }
    );

    let mut decoder = Decoder::with_limit(4);
    assert!(decoder.feed(b"%abc").unwrap().is_empty());
    assert_eq!(
        decoder.feed(b"d"),
        Err(DecodeError::RecordTooLong { limit: 4 })
    );
    let mut decoder = Decoder::with_limit(4);
    assert_eq!(decoder.feed(b"%abc\n").unwrap().len(), 1);
}

#[test]
fn eof_validates_partial_records_and_unclosed_guards() {
    let mut decoder = Decoder::default();
    decoder.feed(b"%out").unwrap();
    assert_eq!(decoder.finish(), Err(DecodeError::TruncatedRecord));
    let mut decoder = Decoder::default();
    decoder.feed(b"%begin 1 2 3\n").unwrap();
    assert_eq!(
        decoder.finish(),
        Err(DecodeError::UnterminatedCommand(Guard {
            time: 1,
            number: 2,
            flags: 3
        }))
    );
    let mut decoder = Decoder::default();
    decoder.finish().unwrap();
    assert_eq!(decoder.feed(b""), Err(DecodeError::Closed));
}

#[test]
fn dcs_wrapper_and_crlf_can_split_at_every_byte() {
    let bytes = b"\x1bP1000p%begin 1 2 3\r\ntext\r\n%end 1 2 3\r\n%exit\r\n\x1b\\";
    let expected = decode_chunks(b"%begin 1 2 3\ntext\n%end 1 2 3\n%exit\n", 1);
    for split in 0..=bytes.len() {
        let mut decoder = Decoder::dcs(12);
        let mut events = decoder.feed(&bytes[..split]).unwrap();
        events.extend(decoder.feed(&bytes[split..]).unwrap());
        decoder.finish().unwrap();
        assert_eq!(events, expected);
    }
    let mut decoder = Decoder::dcs(32);
    decoder.feed(b"\x1bP1000p%exit\n\x1b").unwrap();
    assert_eq!(decoder.finish(), Err(DecodeError::TruncatedDcs));
}

#[test]
fn deterministic_noise_never_panics_or_exceeds_record_limit() {
    let mut seed = 9_u64;
    for _ in 0..500 {
        let mut decoder = Decoder::with_limit(64);
        for _ in 0..256 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            if decoder.feed(&[(seed >> 32) as u8]).is_err() {
                break;
            }
        }
        let _ = decoder.finish();
    }
}
