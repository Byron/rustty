use tmuxx_client::{
    Client, ClientEvent, Discovery, DiscoveryKind, Error, Object, State, name_command,
};
use tmuxx_command::Command;
use tmuxx_protocol::{DecodeError, Event, Guard, Notification, encode_command_result};

fn command() -> Command {
    Command {
        blocks: Default::default(),
        args: vec![b"list-sessions".to_vec()],
    }
}

fn snapshot() -> tmuxx_client::DiscoverySnapshot {
    let mut discovery = Discovery::default();
    for session in [b"$1".as_slice(), b"$2"] {
        discovery
            .add_line(DiscoveryKind::Sessions, session)
            .unwrap();
    }
    discovery
        .add_line(DiscoveryKind::Windows, b"$1|@10|0|1|layout|visible|*")
        .unwrap();
    discovery
        .add_line(DiscoveryKind::Windows, b"$2|@10|3|1|layout|visible|-")
        .unwrap();
    for _ in 0..2 {
        discovery
            .add_line(DiscoveryKind::Panes, b"%4|@10|80|24|0|0|1")
            .unwrap();
    }
    discovery.finish().unwrap()
}

#[test]
fn submitted_bytes_round_trip_and_barrier_survives_group_failure() {
    let mut client = Client::default();
    let data: Vec<u8> = (1..=255).collect();
    let commands = vec![
        Command {
            blocks: Default::default(),
            args: vec![b"display-message".to_vec(), data],
        },
        command(),
    ];
    let bytes = client.submit(1, &commands, b"unique-marker").unwrap();
    // Two wire records: the user command group, followed by an independent barrier.
    let lines: Vec<_> = bytes.split(|&b| b == b'\n').collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(tmuxx_command::parse(lines[0]).unwrap(), commands);
    assert_eq!(
        tmuxx_command::parse(lines[1]).unwrap(),
        [Command {
            blocks: Default::default(),
            args: vec![
                b"display-message".to_vec(),
                b"-p".to_vec(),
                b"unique-marker".to_vec()
            ]
        }]
    );
}

#[test]
fn server_global_guards_and_opaque_bodies_do_not_drive_request_ids() {
    let mut client = Client::default();
    client.submit(7, &[command()], b"marker-seven").unwrap();
    let mut bytes = encode_command_result(
        Guard {
            time: 1,
            number: 813,
            flags: 0,
        },
        b"startup",
        false,
    );
    bytes.extend(encode_command_result(
        Guard {
            time: 2,
            number: 914,
            flags: 1,
        },
        b"bad command\n%begin 3 4 5",
        true,
    ));
    bytes.extend_from_slice(b"%sessions-changed\nmarker-seven\n");
    bytes.extend(encode_command_result(
        Guard {
            time: 3,
            number: 1004,
            flags: 1,
        },
        b"marker-seven",
        false,
    ));
    let mut events = Vec::new();
    for byte in bytes {
        client
            .feed_into(&[byte], |event| events.push(event))
            .unwrap();
    }
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ClientEvent::BatchFinished { .. }))
            .collect::<Vec<_>>(),
        [&ClientEvent::BatchFinished { request_id: 7 }]
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ClientEvent::Protocol(Event::End { failed: true, .. })))
    );
    assert!(client.state.discovery_needed);
}

#[test]
fn pipelined_markers_are_ordered_and_must_be_the_only_output_line() {
    let mut client = Client::default();
    client.submit(1, &[command()], b"one").unwrap();
    client.submit(2, &[command()], b"two").unwrap();
    let mut events = Vec::new();
    for (number, body) in [
        (40, b"two".as_slice()),
        (41, b"one\nother"),
        (42, b"one"),
        (43, b"two"),
    ] {
        let bytes = encode_command_result(
            Guard {
                time: 1,
                number,
                flags: 1,
            },
            body,
            false,
        );
        events.extend(client.feed(&bytes).unwrap());
    }
    assert_eq!(
        events
            .iter()
            .filter_map(|e| match e {
                ClientEvent::BatchFinished { request_id } => Some(*request_id),
                _ => None,
            })
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

#[test]
fn invalid_submissions_do_not_enter_the_pending_queue() {
    let mut client = Client::with_limits(128, 1);
    assert_eq!(client.submit(1, &[], b"x"), Err(Error::InvalidCommand));
    assert_eq!(
        client.submit(
            1,
            &[Command {
                blocks: Default::default(),
                args: vec![b"display-message".to_vec(), vec![0]]
            }],
            b"x"
        ),
        Err(Error::InvalidCommand)
    );
    for marker in [b"".as_slice(), b"\n", b"a\0b", b"\r"] {
        assert_eq!(
            client.submit(1, &[command()], marker),
            Err(Error::InvalidBarrier)
        );
    }
    client.submit(1, &[command()], b"x").unwrap();
    assert_eq!(
        client.submit(2, &[command()], b"y"),
        Err(Error::PendingLimit)
    );
}

#[test]
fn eof_and_protocol_failure_make_pending_mutations_indeterminate() {
    let mut client = Client::default();
    client.submit(8, &[command()], b"eight").unwrap();
    client.feed(b"%begin 1 2 3\npartial").unwrap();
    let mut events = Vec::new();
    assert_eq!(
        client.finish(|e| events.push(e)),
        Err(Error::Protocol(DecodeError::TruncatedRecord))
    );
    assert_eq!(events, [ClientEvent::Indeterminate { request_id: 8 }]);
    assert_eq!(
        client.submit(9, &[command()], b"nine"),
        Err(Error::Disconnected)
    );

    let mut client = Client::default();
    client.submit(9, &[command()], b"nine").unwrap();
    let mut events = Vec::new();
    assert!(
        client
            .feed_into(b"%begin invalid\n", |e| events.push(e))
            .is_err()
    );
    assert_eq!(events, [ClientEvent::Indeterminate { request_id: 9 }]);
    assert!(client.state.disconnected);
}

#[test]
fn exit_is_forwarded_and_does_not_retry_requests() {
    let mut client = Client::default();
    client.submit(8, &[command()], b"eight").unwrap();
    assert_eq!(
        client.feed(b"%exit lost session\n").unwrap(),
        [
            ClientEvent::Protocol(Event::Notification(Notification::Exit {
                reason: Some(b"lost session".to_vec())
            })),
            ClientEvent::Indeterminate { request_id: 8 },
        ]
    );
    assert!(client.state.disconnected);
}

#[test]
fn discovery_preserves_window_graph_and_requires_terminal_snapshots() {
    let snapshot = snapshot();
    assert_eq!(snapshot.sessions.len(), 2);
    assert_eq!(snapshot.windows.len(), 1);
    assert_eq!(snapshot.panes.len(), 1);
    assert_eq!(snapshot.sessions[&1].windows[&0], 10);
    assert_eq!(snapshot.sessions[&2].windows[&3], 10);
    assert_eq!(snapshot.windows[&10].active_pane, Some(4));
    let mut state = State::default();
    state.reconcile(state.revision(), snapshot).unwrap();
    assert_eq!(state.windows[&10].active_pane, Some(4));
    assert!(state.panes[&4].needs_snapshot);
    state.mark_pane_synchronized(4).unwrap();
    assert!(!state.panes[&4].needs_snapshot);
    state
        .set_name(Object::Window(10), b"two\nlines\xff".to_vec())
        .unwrap();
    assert_eq!(
        state.windows[&10].name.as_deref(),
        Some(b"two\nlines\xff".as_slice())
    );
    assert_eq!(name_command(Object::Session(1)).args[3], b"$1");
}

#[test]
fn layout_mutation_invalidates_discovery_and_pause_needs_rehydration() {
    let mut client = Client::default();
    client.state.reconcile(0, snapshot()).unwrap();
    let revision = client.state.revision();
    client.feed(b"%layout-change @10 new visible *\n").unwrap();
    assert_eq!(
        client.state.reconcile(revision, snapshot()),
        Err(Error::StaleDiscovery)
    );
    client.state.mark_pane_synchronized(4).unwrap();
    client
        .feed(b"%pause %4\n%continue %4\n%output %4 after\n")
        .unwrap();
    assert!(!client.state.panes[&4].paused);
    assert!(client.state.panes[&4].needs_snapshot);
}

#[test]
fn closing_one_link_does_not_delete_shared_window_or_panes() {
    let mut client = Client::default();
    client.state.reconcile(0, snapshot()).unwrap();
    client
        .feed(b"%session-changed $1 primary\n%window-close @10\n")
        .unwrap();
    assert!(
        !client.state.sessions[&1]
            .windows
            .values()
            .any(|id| *id == 10)
    );
    assert!(
        client.state.sessions[&2]
            .windows
            .values()
            .any(|id| *id == 10)
    );
    assert!(client.state.panes.contains_key(&4));
    client
        .feed(b"%session-changed $2 other\n%window-close @10\n")
        .unwrap();
    assert!(!client.state.windows.contains_key(&10));
    assert!(!client.state.panes.contains_key(&4));
}

#[test]
fn discovery_checks_ranges_references_duplicates_and_limits() {
    let mut discovery = Discovery::with_limit(1);
    discovery.add_line(DiscoveryKind::Sessions, b"$0").unwrap();
    assert_eq!(
        discovery.add_line(DiscoveryKind::Sessions, b"$1"),
        Err(Error::DiscoveryLimit)
    );
    assert_eq!(discovery.finish(), Err(Error::InvalidDiscovery));
    for line in [
        b"$-1".as_slice(),
        b"$18446744073709551616",
        b"name",
        b"$1|trailing",
    ] {
        assert_eq!(
            Discovery::default().add_line(DiscoveryKind::Sessions, line),
            Err(Error::InvalidDiscovery)
        );
    }
    let mut discovery = Discovery::default();
    assert_eq!(
        discovery.add_line(DiscoveryKind::Windows, b"$1|@1|0|1|a|b|*"),
        Err(Error::InvalidDiscovery)
    );
    let mut discovery = Discovery::default();
    discovery.add_line(DiscoveryKind::Sessions, b"$1").unwrap();
    discovery
        .add_line(DiscoveryKind::Windows, b"$1|@1|0|1|a|b|*")
        .unwrap();
    assert_eq!(
        discovery.add_line(DiscoveryKind::Panes, b"%0|@1|0|24|0|0|1"),
        Err(Error::InvalidDiscovery)
    );
}

#[test]
fn same_session_duplicate_winlinks_keep_indices_and_request_discovery_when_ambiguous() {
    let mut discovery = Discovery::default();
    discovery.add_line(DiscoveryKind::Sessions, b"$1").unwrap();
    discovery
        .add_line(DiscoveryKind::Windows, b"$1|@10|0|0|layout|layout|")
        .unwrap();
    discovery
        .add_line(DiscoveryKind::Windows, b"$1|@10|3|1|layout|layout|*")
        .unwrap();
    let snapshot = discovery.finish().unwrap();
    assert_eq!(snapshot.sessions[&1].windows, [(0, 10), (3, 10)].into());
    assert_eq!(snapshot.sessions[&1].active_index, Some(3));
    let mut client = Client::default();
    client.state.reconcile(0, snapshot).unwrap();
    client.feed(b"%session-window-changed $1 @10\n").unwrap();
    assert!(client.state.discovery_needed);
    assert_eq!(client.state.sessions[&1].active_index, None);
}

#[test]
fn discovery_keeps_signed_floating_geometry_and_modal_flag_with_older_server_fallback() {
    for (tail, modal) in [("", false), ("|", false), ("|0", false), ("|1", true)] {
        let mut discovery = Discovery::default();
        discovery.add_line(DiscoveryKind::Sessions, b"$1").unwrap();
        discovery
            .add_line(DiscoveryKind::Windows, b"$1|@1|0|1|layout|layout|*")
            .unwrap();
        discovery
            .add_line(
                DiscoveryKind::Panes,
                format!("%7|@1|20|5|-3|-2|1{tail}").as_bytes(),
            )
            .unwrap();
        let snapshot = discovery.finish().unwrap();
        let pane = &snapshot.panes[&7];
        assert_eq!((pane.left, pane.top, pane.modal), (-3, -2, modal));
    }
}
