use tmuxx_client::system::{SystemClient, SystemEvent};
use tmuxx_command::{Command, parse};
use tmuxx_protocol::{Guard, encode_command_result};

fn respond(client: &mut SystemClient, data: &[u8], failed: bool) -> Vec<SystemEvent> {
    let writes = client.take_writes();
    assert_eq!(writes.len(), 1);
    let commands = parse(&writes[0]).unwrap();
    let marker = &commands.last().unwrap().args[2];
    let mut bytes = encode_command_result(
        Guard {
            time: 1,
            number: 8,
            flags: 1,
        },
        data,
        failed,
    );
    if !failed && commands.first().unwrap().args[0] == b"capture-pane" {
        bytes.extend(encode_command_result(
            Guard {
                time: 1,
                number: 9,
                flags: 1,
            },
            b"3|1|1|0|0",
            false,
        ));
    }
    bytes.extend(encode_command_result(
        Guard {
            time: 1,
            number: 10,
            flags: 1,
        },
        marker,
        false,
    ));
    client.feed(&bytes)
}

#[test]
fn disappearing_pane_during_capture_restarts_discovery_without_losing_user_commands() {
    let mut client = SystemClient::new(b"race".to_vec()).unwrap();
    client.bootstrap().unwrap();
    respond(&mut client, b"$0\n", false);
    respond(&mut client, b"$0|@0|0|1|layout|layout|*\n", false);
    respond(&mut client, b"%0|@0|80|24|0|0|1\n", false);
    respond(&mut client, b"main\n", false);
    respond(&mut client, b"window\n", false);
    let user = client
        .queue(vec![Command {
            blocks: Default::default(),
            args: vec![
                b"display-message".to_vec(),
                b"-p".to_vec(),
                b"once".to_vec(),
            ],
        }])
        .unwrap();
    client.feed(b"%layout-change @0 updated updated *\n");
    let events = respond(&mut client, b"can't find pane: %0\n", true);
    assert!(
        !events.iter().any(|e| matches!(e, SystemEvent::Error(_))),
        "{events:?}"
    );
    respond(&mut client, b"$0\n", false);
    respond(&mut client, b"$0|@0|0|1|updated|updated|*\n", false);
    respond(&mut client, b"%1|@0|80|24|0|0|1\n", false);
    respond(&mut client, b"main\n", false);
    respond(&mut client, b"window\n", false);
    let events = respond(&mut client, b"replacement\n", false);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SystemEvent::PaneCapture { pane_id: 1, .. }))
    );
    assert!(
        respond(&mut client, b"once\n", false).contains(&SystemEvent::CommandFinished {
            request_id: user,
            output: b"once\n".to_vec(),
            failed: false
        })
    );
    assert!(client.take_writes().is_empty());
}

#[test]
fn repeated_bootstrap_failures_stop_with_the_failed_command_and_server_diagnostic() {
    let mut client = SystemClient::new(b"failure".to_vec()).unwrap();
    client.bootstrap().unwrap();
    for attempt in 0..3 {
        let events = respond(&mut client, b"client is read-only\n", true);
        if attempt < 2 {
            assert!(events.is_empty(), "{events:?}");
        } else {
            assert!(events.iter().any(|event| matches!(event, SystemEvent::Error(error) if error.contains("list-sessions") && error.contains("client is read-only"))), "{events:?}");
        }
    }
    assert!(client.take_writes().is_empty());
}
#[test]
fn bootstrap_discovers_names_and_visible_capture_before_live_bytes() {
    let mut client = SystemClient::new(b"testnonce".to_vec()).unwrap();
    client.bootstrap().unwrap();
    assert!(respond(&mut client, b"$0\n", false).is_empty());
    assert!(respond(&mut client, b"$0|@0|0|1|layout|layout|*\n", false).is_empty());
    assert!(
        respond(&mut client, b"%0|@0|80|24|0|0|1\n", false)
            .iter()
            .any(|e| matches!(e, SystemEvent::State(_)))
    );
    respond(&mut client, b"session\nname\n", false);
    respond(&mut client, b"window\n", false);
    let events = respond(&mut client, b"first\nsecond\n", false);
    assert!(events.contains(&SystemEvent::PaneCapture {
        pane_id: 0,
        data: b"\x1b[?1049l\x1b[H\x1b[2Jfirst\r\nsecond\x1b[2;4H\x1b[?25h\x1b[?1l".to_vec(),
        cols: 80,
        rows: 24
    }));
    assert_eq!(
        client.state().sessions[&0].name.as_deref(),
        Some(b"session\nname".as_slice())
    );
    assert!(
        client.state().panes[&0].needs_snapshot,
        "visible capture cannot certify complete native terminal state"
    );
    assert_eq!(
        client.feed(b"%output %0 binary\\000\\377\n"),
        vec![SystemEvent::PaneOutput {
            pane_id: 0,
            data: b"binary\0\xff".to_vec()
        }]
    );
    assert!(
        client.state().panes[&0].needs_snapshot,
        "live output cannot recover missing history, parser or inactive-screen state"
    );
    assert!(client.take_writes().is_empty());
}
#[test]
fn user_group_error_does_not_poison_next_request_and_disconnect_never_retries() {
    let mut client = SystemClient::new(b"testnonce".to_vec()).unwrap();
    let id = client
        .queue(vec![Command {
            blocks: Default::default(),
            args: vec![
                b"display-message".to_vec(),
                b"-p".to_vec(),
                b"test".to_vec(),
            ],
        }])
        .unwrap();
    let events = respond(&mut client, b"failure\n", true);
    assert!(events.contains(&SystemEvent::CommandFinished {
        request_id: id,
        output: b"failure\n".to_vec(),
        failed: true
    }));
    let id = client
        .queue(vec![Command {
            blocks: Default::default(),
            args: vec![b"split-window".to_vec()],
        }])
        .unwrap();
    assert_eq!(client.take_writes().len(), 1);
    let queued = client
        .queue(vec![Command {
            blocks: Default::default(),
            args: vec![b"new-window".to_vec()],
        }])
        .unwrap();
    let events = client.finish();
    assert!(events.contains(&SystemEvent::Indeterminate { request_id: id }));
    assert!(events.contains(&SystemEvent::Cancelled { request_id: queued }));
    assert!(client.take_writes().is_empty());
}

#[test]
fn continuous_graph_churn_exhausts_retries_until_explicit_bootstrap() {
    let mut client = SystemClient::new(b"churn".to_vec()).unwrap();
    client.bootstrap().unwrap();
    for attempt in 0..3 {
        respond(&mut client, b"$0\n", false);
        respond(&mut client, b"$0|@0|0|1|layout|layout|*\n", false);
        client.feed(format!("%window-add @{}\n", attempt + 1).as_bytes());
        let events = respond(&mut client, b"%0|@0|80|24|0|0|1\n", false);
        assert_eq!(
            events
                .iter()
                .any(|event| matches!(event, SystemEvent::DiscoveryDeferred)),
            attempt == 2,
            "{events:?}"
        );
    }
    assert!(client.take_writes().is_empty());
    assert!(client.discovery_pending());
    assert!(!client.state().disconnected);
    client.feed(b"%window-add @9\n");
    assert!(
        client.take_writes().is_empty(),
        "notifications must not renew an exhausted retry budget"
    );
    client.bootstrap().unwrap();
    assert_eq!(client.take_writes().len(), 1);
}

#[test]
fn deferred_discovery_resumes_without_replaying_a_user_command() {
    let mut client = SystemClient::new(b"deferred".to_vec()).unwrap();
    client.bootstrap().unwrap();
    for attempt in 0..3 {
        respond(&mut client, b"$0\n", false);
        respond(&mut client, b"$0|@0|0|1|layout|layout|*\n", false);
        client.feed(format!("%window-add @{}\n", attempt + 1).as_bytes());
        respond(&mut client, b"%0|@0|80|24|0|0|1\n", false);
    }
    let id = client
        .queue(parse(b"set-option -g @once applied").unwrap())
        .unwrap();
    let writes = client.take_writes();
    assert_eq!(writes.len(), 1);
    let commands = parse(&writes[0]).unwrap();
    assert_eq!(commands[0].args[0], b"set-option");
    let marker = &commands.last().unwrap().args[2];
    client.bootstrap().unwrap();
    assert!(
        client.take_writes().is_empty(),
        "resuming discovery replayed an active user request"
    );
    let mut bytes = encode_command_result(
        Guard {
            time: 1,
            number: 20,
            flags: 1,
        },
        b"",
        false,
    );
    bytes.extend(encode_command_result(
        Guard {
            time: 1,
            number: 21,
            flags: 1,
        },
        marker,
        false,
    ));
    let events = client.feed(&bytes);
    assert_eq!(events.iter().filter(|event| matches!(event, SystemEvent::CommandFinished { request_id, .. } if *request_id == id)).count(), 1);
    let resumed = client.take_writes();
    assert_eq!(resumed.len(), 1);
    assert_eq!(parse(&resumed[0]).unwrap()[0].args[0], b"list-sessions");
    assert!(!client.finish().iter().any(|event| matches!(event, SystemEvent::Indeterminate { request_id } | SystemEvent::Cancelled { request_id } if *request_id == id)));
}

#[test]
fn stale_cross_list_rows_are_discarded_before_current_snapshot_validation() {
    for stale in [false, true] {
        let mut client = SystemClient::new(b"crossed-lists".to_vec()).unwrap();
        client.bootstrap().unwrap();
        respond(&mut client, b"$0\n", false);
        if stale {
            client.feed(b"%window-add @1\n");
        }
        // A linked window observed after session creation may refer to a
        // session absent from the already completed list-sessions response.
        let mut events = respond(&mut client, b"$1|@1|0|1|layout|layout|*\n", false);
        if stale {
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, SystemEvent::Error(_))),
                "{events:?}"
            );
            let next = client.take_writes();
            assert_eq!(next.len(), 1);
            assert_eq!(parse(&next[0]).unwrap()[0].args[0], b"list-sessions");
        } else {
            if !events
                .iter()
                .any(|event| matches!(event, SystemEvent::Error(_)))
            {
                events.extend(respond(&mut client, b"%1|@1|80|24|0|0|1\n", false));
            }
            assert!(events.iter().any(|event|matches!(event,SystemEvent::Error(error) if error.contains("malformed discovery"))),"current malformed snapshot was accepted: {events:?}");
            assert!(client.take_writes().is_empty());
        }
    }
}
