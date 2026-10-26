use rustty_parser::{
    BatchEvent, Boundaries as B, BoundaryStop, Parser, State, StringEnd, StringIntroducer,
    StringKind,
};

fn strings() -> B {
    B::APC_START | B::APC_END | B::DCS_START | B::DCS_END | B::OSC_START | B::OSC_END
}

#[test]
fn ground_stops_include_pending_utf8_and_do_not_consume_when_already_ground() {
    let mut parser = Parser::new();
    assert_eq!(
        parser.advance_until(b"unused", B::GROUND, |_| panic!()),
        BoundaryStop {
            reached: B::GROUND,
            ..Default::default()
        },
    );
    parser.advance(b"\xf0\x9f", |_| panic!());
    let mut events = Vec::new();
    let stop = parser.advance_until(b"\x98\x80X", B::GROUND, |event| {
        events.push(format!("{event:?}"))
    });
    assert_eq!(stop.consumed, 2);
    assert_eq!(stop.reached, B::GROUND);
    assert_eq!(stop.byte, None);
    assert_eq!(events, ["Print('😀')"]);
    parser.advance(b"\x1b[31", |_| {});
    assert_eq!(parser.advance_until(b"mX", B::GROUND, |_| {}).consumed, 1);
    assert!(parser.continuation().unwrap().is_empty());
}

#[test]
fn string_starts_report_7_bit_and_c1_introducers() {
    for (prefix, kind, boundary, introducer) in [
        (
            b"\x1b_".as_slice(),
            StringKind::Apc,
            B::APC_START,
            StringIntroducer::Esc,
        ),
        (
            b"\x1bX",
            StringKind::Apc,
            B::APC_START,
            StringIntroducer::Esc,
        ),
        (
            b"\x1b^",
            StringKind::Apc,
            B::APC_START,
            StringIntroducer::Esc,
        ),
        (
            b"\x1bP",
            StringKind::Dcs,
            B::DCS_START,
            StringIntroducer::Esc,
        ),
        (
            b"\x1b]",
            StringKind::Osc,
            B::OSC_START,
            StringIntroducer::Esc,
        ),
        (
            b"\x1b\x9f",
            StringKind::Apc,
            B::APC_START,
            StringIntroducer::C1,
        ),
        (
            b"\x1b\x98",
            StringKind::Apc,
            B::APC_START,
            StringIntroducer::C1,
        ),
        (
            b"\x1b\x9e",
            StringKind::Apc,
            B::APC_START,
            StringIntroducer::C1,
        ),
        (
            b"\x1b\x90",
            StringKind::Dcs,
            B::DCS_START,
            StringIntroducer::C1,
        ),
        (
            b"\x1b\x9d",
            StringKind::Osc,
            B::OSC_START,
            StringIntroducer::C1,
        ),
    ] {
        let input = [prefix, b"payload"].concat();
        let mut parser = Parser::new();
        let stop = parser.advance_until(&input, strings(), |_| {});
        assert_eq!(stop.consumed, prefix.len());
        assert_eq!(stop.reached, boundary);
        assert_eq!(stop.kind, Some(kind));
        assert_eq!(stop.introducer, Some(introducer));
        assert_eq!(stop.end, None);
        assert_eq!(stop.byte, prefix.last().copied());
    }
}

#[test]
fn string_ends_report_termination_cancellation_and_abort_reasons() {
    for (prefix, boundary, endings) in [
        (
            b"\x1b_data".as_slice(),
            B::APC_END,
            &[0x1b, 0x9c, 0x18, 0x1a, 0x9b, 0x85][..],
        ),
        (
            b"\x1bP1;",
            B::DCS_END,
            &[0x1b, 0x9c, 0x18, 0x1a, 0x9b, 0x85][..],
        ),
        (b"\x1bP:q", B::DCS_END, &[0x1b, 0x18, 0x1a][..]),
        (b"\x1bPqdata", B::DCS_END, &[0x1b, 0x18, 0x1a][..]),
        (b"\x1b]2;title", B::OSC_END, &[0x1b, 0x18, 0x1a, 7][..]),
    ] {
        for &ending in endings {
            let input = [prefix, &[ending], b"rest"].concat();
            let mut parser = Parser::new();
            let stop = parser.advance_until(&input, boundary, |_| {});
            assert_eq!(stop.consumed, prefix.len() + 1, "{input:?}");
            assert_eq!(stop.reached, boundary);
            assert_eq!(stop.introducer, None);
            assert_eq!(stop.byte, Some(ending));
            assert_eq!(
                stop.end,
                Some(match ending {
                    0x1b => StringEnd::StEsc,
                    0x9c => StringEnd::StC1,
                    0x18 | 0x1a => StringEnd::Cancelled,
                    7 => StringEnd::Bel,
                    _ => StringEnd::Aborted,
                })
            );
        }
    }
}

#[test]
fn one_byte_can_end_and_start_strings_or_reach_ground() {
    let mut parser = Parser::new();
    parser.advance(b"\x1b_data", |_| {});
    let stop = parser.advance_until(b"\x9d2;title\x07", strings(), |_| {});
    assert_eq!(stop.consumed, 1);
    assert_eq!(stop.reached, B::APC_END | B::OSC_START);
    assert_eq!(stop.kind, Some(StringKind::Apc));
    assert_eq!(stop.end, Some(StringEnd::Aborted));
    assert_eq!(stop.introducer, Some(StringIntroducer::C1));
    let stop = parser.advance_until(b"2;title\x07X", B::OSC_END | B::GROUND, |_| {});
    assert_eq!(stop.reached, B::OSC_END | B::GROUND);
    assert_eq!(stop.end, Some(StringEnd::Bel));

    let mut parser = Parser::new();
    parser.advance(b"\x1b_data", |_| {});
    let stop = parser.advance_until(b"\x9d", B::OSC_START, |_| {});
    assert_eq!(stop.reached, B::OSC_START);
    assert_eq!(stop.kind, Some(StringKind::Osc));
    assert_eq!(stop.end, None);
}

#[test]
fn header_restarts_and_c1_payload_bytes_do_not_create_boundaries() {
    for input in [
        b"\x1bP1\x90qdata".as_slice(),
        b"\x1bPq\x9b\x9c",
        b"\x1b]2;\x9d\x9b",
    ] {
        let mut parser = Parser::new();
        parser.advance(&input[..2], |_| {});
        let stop = parser.advance_until(&input[2..], strings(), |_| {});
        assert_eq!(stop.consumed, input.len() - 2);
        assert!(stop.reached.is_empty());
        assert_eq!(stop.kind, None);
        assert_eq!(stop.byte, None);
    }
    let mut parser = Parser::new();
    let stop = parser.advance_until(b"\xc2\x9b\xf0\x9f\x98\x80", strings(), |_| {});
    assert!(stop.reached.is_empty());
    assert!(parser.is_ground());
}

#[test]
fn split_input_preserves_boundaries_events_and_only_the_consumed_continuation() {
    fn parse(chunks: &[&[u8]]) -> (Vec<BoundaryStop>, Vec<String>) {
        let mut parser = Parser::new();
        let mut stops = Vec::new();
        let mut events = Vec::new();
        let mut total = 0;
        for chunk in chunks {
            let mut offset = 0;
            while offset < chunk.len() {
                let mut stop = parser.advance_until(&chunk[offset..], strings(), |event| {
                    events.push(format!("{event:?}"))
                });
                assert!(stop.consumed > 0);
                offset += stop.consumed;
                if !stop.reached.is_empty() {
                    stop.consumed = total + offset;
                    stops.push(stop);
                }
                Parser::validate_continuation(&parser.continuation().unwrap()).unwrap();
            }
            total += chunk.len();
        }
        (stops, events)
    }
    let input = b"text\xf0\x9f\x98\x80\x1b[31m\x1bP1;2q\x9b\x1b\\\x1b_data\x9d2;t\x07\x1b_more\x18\x1b_X\x9c";
    let expected = parse(&[input]);
    for split in 0..=input.len() {
        assert_eq!(parse(&[&input[..split], &input[split..]]), expected);
    }
    let mut events = Vec::new();
    Parser::new().advance(input, |event| events.push(format!("{event:?}")));
    assert_eq!(expected.1, events);
}

#[test]
fn tmux_dcs_stops_after_its_escaped_terminator() {
    let mut parser = Parser::new();
    parser.set_tmux_strings(true);
    parser.advance(b"\x1bPtmux;", |_| {});
    let stop = parser.advance_until(b"x\x1b\x1b_\x1b\\rest", B::DCS_END | B::GROUND, |_| {});
    assert_eq!(stop.consumed, 6);
    assert_eq!(stop.reached, B::DCS_END | B::GROUND);
    assert_eq!(stop.end, Some(StringEnd::StEsc));
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn no_boundaries_retains_the_batched_interface() {
    let mut parser = Parser::new();
    let mut events = Vec::new();
    let stop = parser.advance_batched_until(b"plain text", B::NONE, |event| {
        if let BatchEvent::PrintAscii(text) = event {
            events.push(text.to_vec());
        } else {
            panic!("{event:?}");
        }
    });
    assert_eq!(stop.consumed, 10);
    assert!(stop.reached.is_empty());
    assert_eq!(events, [b"plain text"]);
}
