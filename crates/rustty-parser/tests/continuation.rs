use rustty_parser::{ContinuationError, Parser};

#[test]
fn unfinished_sequences_resume_identically_across_all_feed_boundaries() {
    let cases: &[(&[u8], &[u8])] = &[
        (b"\x1b", b"[31mX"),
        (b"\x1b[38:2::1:2", b":3mX"),
        (b"\x1b]2;title", b"\x07X"),
        (b"\x1bP$qm", b"\x1b\\X"),
        (b"\x1b_Ga=q,i=1", b";AAAA\x1b\\X"),
        (b"\xf0\x9f\x98", b"\x80X"),
    ];
    for &(pending, rest) in cases {
        for split in 0..=pending.len() {
            let mut original = Parser::new();
            original.advance(b"committed", |_| {});
            original.advance(&pending[..split], |_| {});
            original.advance(&pending[split..], |_| {});
            let continuation = original.continuation().unwrap();
            assert_eq!(continuation, pending);
            Parser::validate_continuation(&continuation).unwrap();
            let mut restored = Parser::new();
            restored.advance(&continuation, |_| {});
            assert_eq!(restored.continuation().unwrap(), continuation);
            let mut expected = Vec::new();
            original.advance(rest, |event| expected.push(format!("{event:?}")));
            let mut actual = Vec::new();
            restored.advance(rest, |event| actual.push(format!("{event:?}")));
            assert_eq!(actual, expected);
            assert!(restored.continuation().unwrap().is_empty());
        }
    }
}

#[test]
fn export_omits_committed_controls_and_replaces_superseded_state() {
    let mut parser = Parser::new();
    parser.advance(b"old\x1b[1\x07", |_| {});
    parser.advance(b";2\x08", |_| {});
    assert_eq!(parser.continuation().unwrap(), b"\x1b[1;2");
    parser.advance(b"\x1b]2;new", |_| {});
    assert_eq!(parser.continuation().unwrap(), b"\x1b]2;new");
    parser.advance(b"\x07\xe0\xa0\xf0", |_| {});
    assert_eq!(parser.continuation().unwrap(), b"\xf0");
}

#[test]
fn validation_rejects_completed_noncanonical_and_effectful_replay() {
    for bytes in [b"A".as_slice(), b"\x1b[31m", b"\x1b]2;x\x07"] {
        assert_eq!(
            Parser::validate_continuation(bytes),
            Err(ContinuationError::NoPendingState)
        );
    }
    for bytes in [b"A\x1b[31".as_slice(), b"\x1b[1\x1b[2", b"\xe0\xa0\xf0"] {
        assert_eq!(
            Parser::validate_continuation(bytes),
            Err(ContinuationError::NonCanonical)
        );
    }
    assert_eq!(
        Parser::validate_continuation(b"\x1b[1\x07"),
        Err(ContinuationError::ReplayWouldCommit)
    );
    Parser::validate_continuation(b"").unwrap();
}

#[test]
fn overflow_recovers_only_with_a_fresh_start_or_ground() {
    let mut parser = Parser::new();
    parser.set_continuation_limit(4);
    parser.advance(b"\x1b[123", |_| {});
    assert_eq!(parser.continuation(), Err(ContinuationError::LimitExceeded));
    parser.advance(b"4", |_| {});
    assert_eq!(parser.continuation(), Err(ContinuationError::LimitExceeded));
    parser.advance(b"\x1b[2", |_| {});
    assert_eq!(parser.continuation().unwrap(), b"\x1b[2");
    parser.set_continuation_limit(0);
    assert_eq!(parser.continuation(), Err(ContinuationError::LimitExceeded));
    parser.advance(b"m", |_| {});
    assert!(parser.continuation().unwrap().is_empty());
}
