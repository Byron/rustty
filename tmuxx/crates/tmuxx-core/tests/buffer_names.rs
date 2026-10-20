use tmuxx_command::Command;
use tmuxx_core::{ClientContext, Engine, Outcome};

fn run(
    engine: &mut Engine,
    context: &mut ClientContext,
    args: &[&[u8]],
) -> Result<Outcome, String> {
    engine.execute(
        &Command::new(args.iter().map(|arg| arg.to_vec()).collect()),
        context,
    )
}

#[test]
fn buffer_creation_cleans_names_once_and_lookup_remains_raw() {
    let mut engine = Engine::default();
    let mut context = ClientContext::default();
    let raw = "line\n\t\\é".as_bytes();
    let cleaned = "line\\n\\t\\\\é";
    let out = run(
        &mut engine,
        &mut context,
        &[b"set-buffer", b"-b", raw, b"first"],
    )
    .unwrap();
    assert_eq!(engine.buffers[cleaned], b"first");
    assert!(
        matches!(&out.notifications[0], tmuxx_core::Notification::Raw(data) if data == format!("%paste-buffer-changed {cleaned}").as_bytes())
    );
    assert!(run(&mut engine, &mut context, &[b"show-buffer", b"-b", raw]).is_err());
    assert_eq!(
        run(
            &mut engine,
            &mut context,
            &[b"show-buffer", b"-b", cleaned.as_bytes()]
        )
        .unwrap()
        .output,
        b"first"
    );
    run(
        &mut engine,
        &mut context,
        &[b"set-buffer", b"-ab", raw, b"replacement"],
    )
    .unwrap();
    assert_eq!(engine.buffers[cleaned], b"replacement");
    run(
        &mut engine,
        &mut context,
        &[b"set-buffer", b"-ab", cleaned.as_bytes(), b" appended"],
    )
    .unwrap();
    assert_eq!(engine.buffers[cleaned], b"replacement");
    assert_eq!(
        engine.buffers["line\\\\n\\\\t\\\\\\\\é"],
        b"replacement appended"
    );
    let before = engine.buffers.clone();
    let sequence = engine.sequence;
    let out = run(
        &mut engine,
        &mut context,
        &[b"set-buffer", b"-b", cleaned.as_bytes(), b"-n", raw],
    )
    .unwrap();
    assert!(!out.changed);
    assert!(out.notifications.is_empty() && out.effects.is_empty());
    assert_eq!(engine.buffers, before);
    assert_eq!(engine.sequence, sequence);
}

#[test]
fn invalid_buffer_names_reject_without_mutation_and_diagnostics_are_line_safe() {
    let mut engine = Engine::default();
    let mut context = ClientContext::default();
    run(&mut engine, &mut context, &[b"new-session", b"-dE"]).unwrap();
    run(
        &mut engine,
        &mut context,
        &[b"set-buffer", b"-b", b"kept", b"old"],
    )
    .unwrap();
    for name in [
        b"bad\r".as_slice(),
        b"bad\x1b",
        b"bad\x7f",
        b"\xff\n%exit",
        b"\xc3",
        b"\xc0\x80",
        b"",
    ] {
        let before = engine.buffers.clone();
        let state = engine.snapshot();
        for args in [
            vec![b"set-buffer".as_slice(), b"-wb", name, b"new"],
            vec![b"set-buffer", b"-b", b"kept", b"-n", name],
            vec![b"capture-pane", b"-b", name],
        ] {
            let error = run(&mut engine, &mut context, &args).unwrap_err();
            assert!(
                !error.as_bytes().iter().any(u8::is_ascii_control),
                "{error:?}"
            );
            if !name.is_empty() {
                assert_eq!(
                    error,
                    format!("invalid buffer name: {}", name.escape_ascii())
                );
            }
            assert_eq!(engine.buffers, before);
            assert_eq!(engine.snapshot(), state);
        }
        assert!(
            engine
                .store_buffer(Some(name.to_vec()), b"new".to_vec())
                .is_err()
        );
        assert_eq!(engine.buffers, before);
    }
    assert_eq!(
        run(
            &mut engine,
            &mut context,
            &[b"set-buffer", b"-b", b"missing", b"-n", b"\xff"]
        )
        .unwrap_err(),
        "unknown buffer: missing"
    );
    run(
        &mut engine,
        &mut context,
        &[b"set-buffer", b"-b", "�".as_bytes(), b"valid Unicode"],
    )
    .unwrap();
    let before = engine.buffers.clone();
    for args in [
        vec![b"show-buffer".as_slice(), b"-b", b"\xff"],
        vec![b"delete-buffer", b"-b", b"\xff"],
        vec![b"set-buffer", b"-b", b"\xff", b"-n", b"renamed"],
        vec![b"paste-buffer", b"-db", b"\xff"],
        vec![b"save-buffer", b"-b", b"\xff", b"-"],
    ] {
        let error = run(&mut engine, &mut context, &args).unwrap_err();
        assert!(error.ends_with("\\xff"));
        assert_eq!(engine.buffers, before);
    }
    run(
        &mut engine,
        &mut context,
        &[b"set-buffer", b"first automatic"],
    )
    .unwrap();
    assert_eq!(engine.buffers["buffer0"], b"first automatic");
}

#[test]
fn empty_buffer_input_and_pending_capture_do_not_allocate_or_notify() {
    let mut engine = Engine::default();
    let mut context = ClientContext::default();
    run(&mut engine, &mut context, &[b"new-session", b"-dE"]).unwrap();
    for name in [None, Some(Vec::new()), Some(b"invalid\xff\n".to_vec())] {
        assert_eq!(engine.store_buffer(name, Vec::new()).unwrap(), None);
    }
    for args in [
        vec![b"capture-pane".as_slice(), b"-P"],
        vec![b"capture-pane", b"-P", b"-b", b"\xff"],
        vec![b"set-buffer", b"-b", b"\xff", b""],
    ] {
        let out = run(&mut engine, &mut context, &args).unwrap();
        assert!(!out.changed);
        assert!(out.notifications.is_empty() && out.effects.is_empty());
        assert!(engine.buffers.is_empty());
    }
    run(
        &mut engine,
        &mut context,
        &[b"set-buffer", b"first automatic"],
    )
    .unwrap();
    assert_eq!(engine.buffers["buffer0"], b"first automatic");
}
