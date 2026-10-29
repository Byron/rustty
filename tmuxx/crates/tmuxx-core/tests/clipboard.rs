use tmuxx_command::Command;
use tmuxx_core::{ClientContext, Effect, Engine, HostAction, HostResponse};
use tmuxx_terminal::clipboard::{Content, ReadResult, ReadSuccess};

fn command(engine: &mut Engine, context: &mut ClientContext, args: &[&str]) {
    engine
        .execute(
            &Command {
                blocks: Default::default(),
                args: args.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
            },
            context,
        )
        .unwrap();
}

fn engine() -> (Engine, ClientContext) {
    let mut engine = Engine::default();
    let mut context = ClientContext::default();
    command(&mut engine, &mut context, &["new-session", "-d"]);
    (engine, context)
}

fn writes(effects: &[Effect]) -> Vec<u8> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Write { data, .. } => Some(data.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect()
}

#[test]
fn clipboard_defaults_block_application_access_and_buffer_reads_are_owner_local() {
    let (mut engine, mut context) = engine();
    engine.store_buffer(None, b"private".to_vec()).unwrap();
    engine
        .store_buffer(Some("existing".into()), b"named".to_vec())
        .unwrap();
    assert!(
        engine
            .feed(0, b"\x1b]52;c;bmV3\x07\x1b]52;c;?\x07")
            .is_empty()
    );
    assert_eq!(engine.buffers.len(), 2);
    command(
        &mut engine,
        &mut context,
        &["set-option", "-s", "set-clipboard", "on"],
    );
    let effects = engine.feed(0, b"\x1b]52;;?\x07\x1b]52;x7cc;?\x1b\\");
    assert_eq!(
        writes(&effects),
        b"\x1b]52;;cHJpdmF0ZQ==\x07\x1b]52;7;cHJpdmF0ZQ==\x1b\\"
    );
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Host { .. }))
    );
    command(
        &mut engine,
        &mut context,
        &["set-option", "-s", "get-clipboard", "off"],
    );
    assert!(engine.feed(0, b"\x1b]52;c;?\x07").is_empty());
}

#[test]
fn tmux_wrapped_osc52_uses_the_existing_clipboard_policies() {
    let write = b"\x1bPtmux;\x1b\x1b]52;c;c2VsZWN0ZWQ=\x07\x1b\\";
    let read = b"\x1bPtmux;\x1b\x1b]52;c;?\x1b\x1b\\\x1b\\";
    for policy in ["off", "external", "on"] {
        let (mut engine, mut context) = engine();
        command(
            &mut engine,
            &mut context,
            &["set-option", "-s", "set-clipboard", policy],
        );
        let effects = engine.feed(0, write);
        if policy != "on" {
            assert!(effects.is_empty());
            assert!(engine.buffers.is_empty());
            assert!(engine.feed(0, read).is_empty());
            continue;
        }
        assert!(effects.iter().any(|effect| matches!(effect,
            Effect::Host { action: HostAction::ClipboardWrite(request), .. }
                if request.contents[0].data.as_ref() == b"selected")));
        assert_eq!(engine.buffers["buffer0"], b"selected");
        assert_eq!(
            writes(&engine.feed(0, read)),
            b"\x1b]52;c;c2VsZWN0ZWQ=\x1b\\"
        );
        command(
            &mut engine,
            &mut context,
            &["set-option", "-s", "get-clipboard", "request"],
        );
        assert!(matches!(engine.feed(0, read).as_slice(),
            [Effect::Host { action: HostAction::ClipboardRead(request), .. }]
                if request.terminator == tmuxx_terminal::clipboard::Terminator::St));
        command(
            &mut engine,
            &mut context,
            &["set-option", "-s", "get-clipboard", "off"],
        );
        assert!(engine.feed(0, read).is_empty());
    }
}
#[test]
fn clipboard_writes_keep_user_buffers_and_report_automatic_eviction() {
    let (mut engine, mut context) = engine();
    engine
        .store_buffer(Some("buffer0".into()), b"user".to_vec())
        .unwrap();
    command(
        &mut engine,
        &mut context,
        &["set-option", "-s", "set-clipboard", "on"],
    );
    command(
        &mut engine,
        &mut context,
        &["set-option", "-s", "buffer-limit", "1"],
    );
    let effects = engine.feed(0, b"\x1b]52;cp;YQ==\x07\x1b]52;c;Yg==\x07");
    assert_eq!(engine.buffers["buffer0"], b"user");
    assert_eq!(engine.buffers["buffer2"], b"b");
    assert!(!engine.buffers.contains_key("buffer1"));
    assert!(effects.iter().any(|effect| matches!(effect, Effect::ClipboardChanged { pane: Some(0), buffer: Some(name), deleted } if name == "buffer2" && deleted == &["buffer1"])));
    for bytes in [
        b"\x1b]52;c;\x07".as_slice(),
        b"\x1b]52;c;YQ\x07",
        b"\x1b]52;c;YR==\x07",
        b"\x1b]52;c;!\x07",
    ] {
        assert!(engine.feed(0, bytes).is_empty(), "{bytes:?}");
    }
    assert_eq!(engine.buffers.len(), 2);
}

#[test]
fn clipboard_request_denial_is_silent_and_both_caches_only_successful_data() {
    let (mut engine, mut context) = engine();
    command(
        &mut engine,
        &mut context,
        &["set-option", "-s", "set-clipboard", "on"],
    );
    command(
        &mut engine,
        &mut context,
        &["set-option", "-s", "get-clipboard", "both"],
    );
    let effects = engine.feed(0, b"\x1b]52;p;?\x07");
    let [Effect::Host { action, .. }] = effects.as_slice() else {
        panic!("{effects:?}")
    };
    assert!(matches!(action, HostAction::ClipboardRead(_)));
    assert!(
        engine
            .host_response(0, action.clone(), HostResponse::Read(ReadResult::Denied))
            .unwrap()
            .is_empty()
    );
    let success = HostResponse::Read(ReadResult::Success(ReadSuccess {
        contents: vec![Content {
            mime: b"text/plain".to_vec(),
            data: b"host".to_vec().into(),
        }],
        ..Default::default()
    }));
    let effects = engine
        .host_response(0, action.clone(), success.clone())
        .unwrap();
    assert_eq!(writes(&effects), b"\x1b]52;c;aG9zdA==\x07");
    assert_eq!(engine.buffers["buffer0"], b"host");
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::ClipboardChanged { pane: None, .. }))
    );
    command(
        &mut engine,
        &mut context,
        &["set-option", "-s", "get-clipboard", "off"],
    );
    assert!(
        engine
            .host_response(0, action.clone(), success)
            .unwrap()
            .is_empty()
    );
    assert_eq!(engine.buffers.len(), 1);
}

#[test]
fn clipboard_compatibility_profile_survives_ris_and_snapshot_restore() {
    let mut terminal = tmuxx_terminal::Terminal::new(80, 24, 2000);
    terminal.feed(b"\x1bc");
    let mut terminal = terminal.snapshot().restore().unwrap();
    let effects = terminal.feed(b"\x1b]52;qc;?\x07");
    assert!(
        matches!(effects.as_slice(), [tmuxx_terminal::Effect::ClipboardRead(read)] if read.tmux_selector == Some(b'q'))
    );
}
