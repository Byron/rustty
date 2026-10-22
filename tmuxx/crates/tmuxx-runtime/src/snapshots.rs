use super::*;
use tmuxx_protocol::native::{
    SnapshotAssembler, SnapshotSession, SnapshotState, decode, read_record, snapshot_bytes,
    write_snapshot,
};

fn host() -> (Host, Receiver<ServerMessage>) {
    let (events, _) = mpsc::sync_channel(32);
    let mut host = Host::new(events, &Options::new("/unused"), BTreeMap::new());
    let (tx, rx) = mpsc::sync_channel(32);
    host.clients.insert(
        1,
        Client {
            tx,
            context: ClientContext::default(),
            subscribed: true,
            control: false,
            delivery_bytes: Arc::new(AtomicUsize::new(0)),
        },
    );
    (host, rx)
}
#[test]
fn one_large_snapshot_does_not_consume_the_slow_output_budget() {
    let (mut host, rx) = host();
    host.snapshots_pending.insert(1);
    host.send(
        1,
        ServerMessage::Snapshot {
            instance_id: "owner".into(),
            sequence: 1,
            state: SnapshotState {
                sessions: vec![SnapshotSession {
                    id: 0,
                    name: "x".repeat(65 * 1024 * 1024),
                    active: 0,
                    windows: Default::default(),
                }],
                ..Default::default()
            },
        },
    );
    assert!(host.clients.contains_key(&1));
    assert!(matches!(rx.recv().unwrap(), ServerMessage::Snapshot { .. }));
    assert!(host.clients[&1].delivery_bytes.load(Ordering::Acquire) < 1024);
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[test]
fn process_directory_changes_send_only_negotiated_light_metadata_updates() {
    let (mut host, received) = host();
    host.clients.get_mut(&1).unwrap().context.selection_updates = true;
    let mut muted = Vec::new();
    for (id, subscribed, negotiated) in [(2, true, false), (3, false, true)] {
        let (tx, rx) = mpsc::sync_channel(32);
        let mut client = host.clients[&1].clone();
        client.tx = tx;
        client.subscribed = subscribed;
        client.context.selection_updates = negotiated;
        host.clients.insert(id, client);
        muted.push(rx);
    }
    host.engine
        .execute(
            &tmuxx_command::parse(b"new-session -d ''").unwrap()[0],
            &mut ClientContext::default(),
        )
        .unwrap();
    host.engine
        .windows
        .get_mut(&0)
        .unwrap()
        .options
        .insert("automatic-rename".into(), "off".into());
    let cwd = std::env::current_dir().unwrap();
    #[cfg(unix)]
    let cwd = cwd.canonicalize().unwrap();
    let cwd = cwd.into_os_string().into_string().unwrap();
    let shell = defaults::system_shell();
    #[cfg(unix)]
    let argv = [&shell, "-c", "read wait"];
    #[cfg(windows)]
    let argv = [&shell, "/D", "/Q", "/K"];
    host.spawn(
        0,
        shell.clone(),
        argv.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
        Some(cwd.clone()),
        std::env::vars().collect(),
        (80, 24),
    )
    .unwrap();
    let expected = Path::new(&cwd).canonicalize().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let reported = loop {
        host.refresh_process_metadata();
        if let Some(path) = &host.engine.panes[&0].current_path
            && Path::new(path)
                .canonicalize()
                .is_ok_and(|path| path == expected)
        {
            break path.clone();
        }
        assert!(Instant::now() < deadline, "shell CWD was never available");
        thread::sleep(Duration::from_millis(5));
    };
    received.try_iter().for_each(drop);
    let pane = host.engine.panes.get_mut(&0).unwrap();
    pane.cwd = Some(reported.clone());
    pane.current_path = None;
    let sequence = host.engine.sequence;
    host.refresh_process_metadata();
    assert_eq!(host.engine.sequence, sequence);
    assert_eq!(received.try_iter().count(), 0);

    host.engine.panes.get_mut(&0).unwrap().current_path = Some("stale path".into());
    host.next_name_check = Instant::now();
    host.check_window_names();
    let ServerMessage::Selection {
        sequence: updated,
        state,
    } = received.try_recv().unwrap()
    else {
        panic!("missing directory metadata update");
    };
    assert_eq!(updated, sequence + 1);
    assert_eq!(state["panes"][0]["current_path"], reported);
    assert!(state["panes"][0].get("terminal").is_none());
    host.refresh_process_metadata();
    assert_eq!(host.engine.sequence, updated);
    assert_eq!(received.try_iter().count(), 0);
    assert!(muted.iter().all(|rx| rx.try_iter().count() == 0));
}

#[test]
fn checkpoint_snapshots_keep_log_history_and_agent_state_without_duplicate_cells() {
    let (mut host, rx) = host();
    let command = tmuxx_command::parse(b"new-session -d -s logs ''")
        .unwrap()
        .remove(0);
    host.engine
        .execute(&command, &mut ClientContext::default())
        .unwrap();
    for row in 0..1000 {
        host.engine.feed(
            0,
            format!(
                "\x1b[3{}m{row:04}: compiled module successfully with ordinary diagnostic text\x1b[0m\r\n",
                row % 8
            )
            .as_bytes(),
        );
    }
    // Agent begin: {"op":"begin","state":"working","thread_id":"fixture"}.
    host.engine.feed(0, b"\x1b]133;C\x07\x1b]777;rustty-agent;1;eyJvcCI6ImJlZ2luIiwic3RhdGUiOiJ3b3JraW5nIiwidGhyZWFkX2lkIjoiZml4dHVyZSJ9\x1b\\");
    let capture = tmuxx_terminal::CaptureOptions {
        start: Some(i32::MIN),
        escape_sequences: true,
        ..Default::default()
    };
    let expected = host.engine.panes[&0]
        .terminal
        .capture_with(capture)
        .unwrap();
    let agent = host.engine.panes[&0].terminal.agent_status().clone();
    assert_eq!(
        agent.snapshot.as_ref().unwrap().thread_id.as_deref(),
        Some("fixture")
    );
    assert!(agent.enclosing_command);
    host.snapshot(1);
    let ServerMessage::Snapshot { state, .. } = rx.recv().unwrap() else {
        panic!("missing snapshot");
    };
    let terminal = state.panes[0].terminal.as_ref().unwrap();
    assert_eq!(terminal.version, 2);
    let restored = terminal.restore().unwrap();
    assert_eq!(restored.capture_with(capture).unwrap(), expected);
    assert_eq!(restored.agent_status(), &agent);
    let bytes = snapshot_bytes(&state, u64::MAX).unwrap() as usize;
    let checkpoint_bytes = terminal.checkpoint.as_ref().unwrap().len();
    assert!(
        bytes < checkpoint_bytes + 64 * 1024,
        "checkpoint bytes expanded during serialization: {bytes} versus {checkpoint_bytes}"
    );
    assert!(
        bytes < 2 * 1024 * 1024,
        "ordinary logs exceeded snapshot budget: {bytes}"
    );
}

#[test]
fn pane_removal_notifies_other_views_when_requester_gets_a_snapshot() {
    fn request(host: &mut Host, client: u64, argv: Vec<Vec<u8>>, subscribe: bool) {
        host.request(
            client,
            Request {
                request_id: client,
                argv,
                stdin: None,
                subscribe,
                control: false,
            },
        );
        for _ in 0..1024 {
            if !host.scheduled_work() {
                return;
            }
        }
        panic!("native request queue failed to quiesce");
    }

    let (mut host, first) = host();
    let (tx, second) = mpsc::sync_channel(32);
    host.clients.insert(
        2,
        Client {
            tx,
            delivery_bytes: Arc::new(AtomicUsize::new(0)),
            ..host.clients[&1].clone()
        },
    );
    for command in tmuxx_command::parse(b"new-session -d -s removal ''; new-window -d ''").unwrap()
    {
        host.engine
            .execute(&command, &mut ClientContext::default())
            .unwrap();
    }
    for (pane, subscribe) in [(0, true), (1, false)] {
        request(
            &mut host,
            1,
            vec![
                b"kill-pane".to_vec(),
                b"-t".to_vec(),
                format!("%{pane}").into_bytes(),
            ],
            subscribe,
        );
        let replies = first.try_iter().collect::<Vec<_>>();
        assert!(replies.iter().any(
            |message| matches!(message, ServerMessage::Reply(reply) if reply.error.is_none())
        ));
        assert_eq!(
            replies
                .iter()
                .filter(|message| matches!(message, ServerMessage::Snapshot { .. }))
                .count(),
            usize::from(subscribe)
        );
        assert_eq!(
            replies
                .iter()
                .filter(|message| matches!(message, ServerMessage::Changed { .. }))
                .count(),
            usize::from(!subscribe),
            "a requester receiving a snapshot needs no redundant change notice"
        );
        assert_eq!(
            second.try_iter().filter(|message| matches!(message, ServerMessage::Changed { sequence } if *sequence == host.engine.sequence)).count(),
            1,
            "the other view must learn that the last pane in a window disappeared"
        );

        request(&mut host, 2, Vec::new(), true);
        let state = second
            .try_iter()
            .find_map(|message| match message {
                ServerMessage::Snapshot { state, .. } => Some(state),
                _ => None,
            })
            .expect("the notified view must be able to refresh its graph");
        assert_eq!(state.panes.len(), 1 - pane);
        assert_eq!(state.windows.len(), 1 - pane);
        assert_eq!(state.sessions.len(), 1 - pane);
        host.snapshots_pending.clear();
    }
}
#[test]
fn focus_updates_do_not_transfer_scrollback_and_other_mutations_still_refresh() {
    let (mut host, first) = host();
    host.clients.get_mut(&1).unwrap().context.selection_updates = true;
    host.clients.get_mut(&1).unwrap().context.host_effects = true;
    let (tx, second) = mpsc::sync_channel(32);
    host.clients.insert(
        2,
        Client {
            tx,
            delivery_bytes: Arc::new(AtomicUsize::new(0)),
            ..host.clients[&1].clone()
        },
    );
    let (tx, legacy) = mpsc::sync_channel(32);
    let mut legacy_client = host.clients[&1].clone();
    legacy_client.tx = tx;
    legacy_client.context.selection_updates = false;
    legacy_client.delivery_bytes = Arc::new(AtomicUsize::new(0));
    host.clients.insert(3, legacy_client);
    for command in tmuxx_command::parse(
        b"new-session -d -x 140 -y 50 ''; split-window -h ''; new-window -d ''",
    )
    .unwrap()
    {
        host.engine
            .execute(&command, &mut ClientContext::default())
            .unwrap();
    }
    host.engine.feed(
        0,
        b"ordinary build output with enough text to create scrollback\r\n"
            .repeat(1000)
            .as_slice(),
    );
    let run = |host: &mut Host, argv: &[&str], stdin| {
        host.request(
            1,
            Request {
                request_id: 10,
                argv: argv.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
                stdin,
                subscribe: false,
                control: false,
            },
        );
        for _ in 0..1024 {
            if !host.scheduled_work() {
                return;
            }
        }
        panic!("selection request did not finish");
    };
    for (window, target) in [
        ("$0:0", "%0"),
        ("$0:1", "%2"),
        ("$0:0", "%1"),
        ("$0:0", "%1"),
    ] {
        run(
            &mut host,
            &[
                "select-window",
                "-t",
                window,
                ";",
                "select-pane",
                "-t",
                target,
            ],
            None,
        );
        let messages = first.try_iter().collect::<Vec<_>>();
        let reply = messages
            .iter()
            .position(
                |message| matches!(message, ServerMessage::Reply(reply) if reply.error.is_none()),
            )
            .unwrap();
        let update = messages
            .iter()
            .position(|message| matches!(message, ServerMessage::Selection { .. }))
            .unwrap();
        assert!(reply < update, "selection must follow its acknowledgement");
        for messages in [messages, second.try_iter().collect()] {
            assert!(!messages.iter().any(|message| matches!(
                message,
                ServerMessage::Snapshot { .. } | ServerMessage::Changed { .. }
            )));
            let bytes = serde_json::to_vec(&messages).unwrap().len();
            assert!(bytes < 4096, "focus transferred {bytes} bytes");
        }
        assert!(
            !legacy
                .try_iter()
                .any(|message| matches!(message, ServerMessage::Selection { .. }))
        );
    }
    run(
        &mut host,
        &["tmuxx-host-state", "-t", "%0"],
        Some(br#"{"focused":true,"visible":true,"color_scheme":"light"}"#.to_vec()),
    );
    for messages in [
        first.try_iter().collect::<Vec<_>>(),
        second.try_iter().collect(),
    ] {
        assert!(
            messages
                .iter()
                .any(|message| matches!(message, ServerMessage::Selection { .. }))
        );
        assert!(!messages.iter().any(|message| matches!(
            message,
            ServerMessage::Snapshot { .. } | ServerMessage::Changed { .. }
        )));
    }
    let sequence = host.engine.sequence;
    host.pane_output(0, b"after focus");
    for messages in [
        first.try_iter().collect::<Vec<_>>(),
        second.try_iter().collect(),
    ] {
        assert!(messages.iter().any(|message| matches!(message, ServerMessage::PaneOutput { sequence: output_sequence, data, .. } if *output_sequence > sequence && data == b"after focus")));
    }
    for command in [
        vec!["select-pane", "-t", "%0", "-T", "changed title"],
        vec!["resize-pane", "-t", "%0", "-Z"],
        vec!["select-pane", "-t", "%1"],
        vec![
            "set-hook",
            "-g",
            "after-select-pane",
            "select-pane -t %0 -T hook",
        ],
        vec!["select-pane", "-t", "%0"],
    ] {
        run(&mut host, &command, None);
        for messages in [
            first.try_iter().collect::<Vec<_>>(),
            second.try_iter().collect(),
        ] {
            assert!(
                messages
                    .iter()
                    .any(|message| matches!(message, ServerMessage::Changed { .. })),
                "mutation failed to invalidate: {command:?}"
            );
        }
        legacy.try_iter().for_each(drop);
    }
    run(
        &mut host,
        &["set-hook", "-g", "after-select-pane", "wait-for focus-hook"],
        None,
    );
    first.try_iter().for_each(drop);
    second.try_iter().for_each(drop);
    run(&mut host, &["select-pane", "-t", "%1"], None);
    assert!(
        !first
            .try_iter()
            .any(|message| matches!(message, ServerMessage::Reply(_)))
    );
    host.request(
        2,
        Request {
            request_id: 11,
            argv: ["wait-for", "-S", "focus-hook"]
                .map(|arg| arg.as_bytes().to_vec())
                .to_vec(),
            stdin: None,
            subscribe: false,
            control: false,
        },
    );
    while host.scheduled_work() {}
    let messages = first.try_iter().collect::<Vec<_>>();
    let reply = messages.iter().position(|message| matches!(message, ServerMessage::Reply(reply) if reply.request_id == 10 && reply.error.is_none())).unwrap();
    assert!(
        messages[reply + 1..]
            .iter()
            .any(|message| matches!(message, ServerMessage::Selection { .. })),
        "selection must follow a delayed hook acknowledgement"
    );
}

#[test]
fn image_snapshots_stream_atomically_and_rejection_preserves_the_live_owner() {
    let (mut host, rx) = host();
    let command = tmuxx_command::parse(b"new-session -d -s images ''")
        .unwrap()
        .remove(0);
    host.engine
        .execute(&command, &mut ClientContext::default())
        .unwrap();
    let upload = format!(
        "\x1b_Ga=T,i=7,f=32,s=256,v=256,C=1;{}\x1b\\",
        "////".repeat(256 * 256 * 4 / 3) + "/w=="
    );
    host.engine.feed(0, upload.as_bytes());
    host.engine.set_process(0, Some(1234), "test-pty".into());
    host.snapshot(1);
    host.snapshot(1);
    assert!(host.snapshots_requested.contains(&1));
    assert!(rx.try_iter().count() == 1);
    host.snapshots_pending.clear();
    host.snapshots_requested.clear();
    host.snapshot(1);
    let ServerMessage::Snapshot {
        instance_id,
        sequence,
        state,
    } = rx.recv().unwrap()
    else {
        panic!("missing snapshot");
    };
    let mut wire = Vec::new();
    write_snapshot(
        &mut wire,
        &instance_id,
        sequence,
        &state,
        tmuxx_protocol::native::MAX_SNAPSHOT_BYTES,
    )
    .unwrap();
    let mut assembler = SnapshotAssembler::default();
    let mut restored = None;
    let mut chunks = 0;
    let mut reader = wire.as_slice();
    while let Some(bytes) = read_record(&mut reader, 512 * 1024).unwrap() {
        let message: ServerMessage = decode(&bytes).unwrap();
        chunks += usize::from(matches!(message, ServerMessage::SnapshotChunk { .. }));
        if let Some(message) = assembler.push(message).unwrap() {
            restored = Some(message);
        }
    }
    assert!(chunks > 1);
    let Some(ServerMessage::Snapshot { state, .. }) = restored else {
        panic!("missing completed snapshot")
    };
    let terminal = state.panes[0].terminal.as_ref().unwrap();
    assert_eq!(
        terminal.primary_graphics.images[0].pixels.len(),
        256 * 256 * 4
    );
    assert!(terminal.restore_with_image_limit(3).is_err());
    assert_eq!(
        terminal
            .restore()
            .unwrap()
            .snapshot()
            .primary_graphics
            .images[0]
            .pixels
            .len(),
        256 * 256 * 4
    );
    host.snapshots_pending.clear();
    host.snapshot_limit = 16;
    host.snapshot(1);
    assert!(matches!(
        rx.recv().unwrap(),
        ServerMessage::SnapshotAborted { .. }
    ));
    assert_eq!(host.engine.panes[&0].pid, Some(1234));
    assert_eq!(
        host.engine.panes[&0]
            .terminal
            .snapshot()
            .primary_graphics
            .images[0]
            .pixels
            .len(),
        256 * 256 * 4
    );
    host.snapshot_limit = tmuxx_protocol::native::MAX_SNAPSHOT_BYTES;
    host.snapshot(1);
    assert!(matches!(rx.recv().unwrap(), ServerMessage::Snapshot { .. }));
}

#[test]
fn image_changes_refresh_native_graphs_including_other_pane_evictions() {
    let (mut host, rx) = host();
    let mut context = ClientContext::default();
    for command in tmuxx_command::parse(
        b"new-session -d -s images ''; split-window -d ''; set-option -s image-total-count-limit 1",
    )
    .unwrap()
    {
        host.engine.execute(&command, &mut context).unwrap();
    }
    host.pane_output(0, b"plain");
    assert!(matches!(
        rx.recv().unwrap(),
        ServerMessage::PaneOutput { .. }
    ));
    assert!(rx.try_recv().is_err());
    host.pane_output(0, b"\x1b_Ga=t,i=1,f=32,s=1,v=1;AAAA/w==\x1b\\");
    assert!(matches!(
        rx.recv().unwrap(),
        ServerMessage::PaneOutput { .. }
    ));
    assert!(matches!(rx.recv().unwrap(), ServerMessage::Changed { .. }));
    host.pane_output(1, b"\x1b_Ga=q,i=2,f=32,s=1,v=1;AAAA/w==\x1b\\");
    assert!(matches!(
        rx.recv().unwrap(),
        ServerMessage::PaneOutput { pane_id: 1, .. }
    ));
    assert!(matches!(rx.recv().unwrap(), ServerMessage::Changed { .. }));
    assert!(
        host.engine.panes[&0]
            .terminal
            .snapshot()
            .primary_graphics
            .images
            .is_empty()
    );
}

#[test]
fn native_byte_backlog_disconnects_before_record_capacity() {
    let (mut host, _) = host();
    host.engine
        .execute(
            &tmuxx_command::parse(b"new-session -d -s backlog ''").unwrap()[0],
            &mut ClientContext::default(),
        )
        .unwrap();
    let (slow_tx, slow_rx) = mpsc::sync_channel(4096);
    host.clients.get_mut(&1).unwrap().tx = slow_tx;
    let (healthy_tx, healthy_rx) = mpsc::sync_channel(1);
    host.clients.insert(
        2,
        Client {
            tx: healthy_tx,
            context: ClientContext::default(),
            subscribed: true,
            control: false,
            delivery_bytes: Arc::new(AtomicUsize::new(0)),
        },
    );
    let output = vec![0; 32768];
    let mut sent = 0;
    let mut previous = 0;
    while host.clients.contains_key(&1) {
        assert!(
            sent < 4096,
            "byte budget must precede the enlarged record capacity"
        );
        host.pane_output(0, &output);
        let message = healthy_rx.recv().unwrap();
        let cost = control::message_bytes(&message);
        host.clients[&2]
            .delivery_bytes
            .fetch_sub(cost, Ordering::AcqRel);
        let ServerMessage::PaneOutput { sequence, data, .. } = message else {
            panic!("expected output");
        };
        assert_eq!(data, output);
        assert!(sequence > previous);
        previous = sequence;
        sent += 1;
    }
    let records = slow_rx.try_iter().collect::<Vec<_>>();
    let bytes = records.iter().map(control::message_bytes).sum::<usize>();
    let next = control::message_bytes(records.last().unwrap());
    assert_eq!(records.len(), sent - 1);
    assert!(bytes <= 64 * 1024 * 1024 && bytes + next > 64 * 1024 * 1024);
    assert!(host.clients.contains_key(&2) && host.engine.panes.contains_key(&0));
    assert_eq!(host.engine.sequence, previous);
    assert!(!host.snapshots_pending.contains(&1) && !host.snapshots_requested.contains(&1));
}
