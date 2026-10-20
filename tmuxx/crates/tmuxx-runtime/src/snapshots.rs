use super::*;
use tmuxx_protocol::native::{SnapshotAssembler, write_snapshot};

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
            state: serde_json::json!({"data":"x".repeat(65 * 1024 * 1024)}),
        },
    );
    assert!(host.clients.contains_key(&1));
    assert!(matches!(rx.recv().unwrap(), ServerMessage::Snapshot { .. }));
    assert!(host.clients[&1].delivery_bytes.load(Ordering::Acquire) < 1024);
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
    for line in wire
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let message: ServerMessage = serde_json::from_slice(line).unwrap();
        chunks += usize::from(matches!(message, ServerMessage::SnapshotChunk { .. }));
        if let Some(message) = assembler.push(message).unwrap() {
            restored = Some(message);
        }
    }
    assert!(chunks > 1);
    let Some(ServerMessage::Snapshot { state, .. }) = restored else {
        panic!("missing completed snapshot")
    };
    let terminal: tmuxx_terminal::Snapshot =
        serde_json::from_value(state["panes"][0]["terminal"].clone()).unwrap();
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
    let (slow_tx, slow_rx) = mpsc::sync_channel(1024);
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
            sent < 1024,
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
