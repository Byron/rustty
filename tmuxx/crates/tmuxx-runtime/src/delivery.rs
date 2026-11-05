//! Bounded native view recovery. Missing deltas are replaced by an ordered
//! authoritative snapshot; replies and host actions are never replayed or dropped.
use super::*;
use std::collections::VecDeque;

const BYTE_LIMIT: usize = 64 * 1024 * 1024;
const RECORD_LIMIT: usize = 256;

#[derive(Default)]
pub(super) struct Pending {
    messages: VecDeque<Queued>,
    bytes: usize,
    needs_snapshot: bool,
    closing: Option<Instant>,
}
impl Pending {
    pub(super) fn is_closing(&self) -> bool {
        self.closing.is_some()
    }
}
struct Queued {
    message: ServerMessage,
    // Frozen final snapshots count their encoded size while waiting here.
    bytes: usize,
}

fn replayable(message: &ServerMessage) -> bool {
    matches!(
        message,
        ServerMessage::PaneOutput { .. }
            | ServerMessage::Changed { .. }
            | ServerMessage::Selection { .. }
    )
}

fn deliver(client: &Client, message: ServerMessage) -> Result<(), TrySendError<ServerMessage>> {
    let bytes = control::message_bytes(&message);
    if client
        .delivery_bytes
        .load(Ordering::Acquire)
        .saturating_add(bytes)
        > BYTE_LIMIT
    {
        return Err(TrySendError::Full(message));
    }
    client.delivery_bytes.fetch_add(bytes, Ordering::AcqRel);
    client.tx.try_send(message).map_err(|error| {
        client.delivery_bytes.fetch_sub(bytes, Ordering::AcqRel);
        error
    })
}

impl Host {
    pub(super) fn send_native(&mut self, id: u64, message: ServerMessage) {
        let Some(client) = self.clients.get(&id) else {
            return;
        };
        if let Some(pending) = self.native_delivery.get_mut(&id) {
            if pending.closing.is_some() {
                return;
            }
            if replayable(&message) || matches!(message, ServerMessage::Snapshot { .. }) {
                pending.needs_snapshot = true;
                return;
            }
            if !pending.messages.is_empty() {
                self.queue_native(id, message, None);
                return;
            }
        }
        let snapshot = matches!(message, ServerMessage::Snapshot { .. });
        match deliver(client, message) {
            Ok(()) => {
                if snapshot {
                    self.snapshots_pending.insert(id);
                }
            }
            Err(TrySendError::Disconnected(_)) => self.disconnect(id),
            Err(TrySendError::Full(message)) => {
                if replayable(&message) || snapshot {
                    self.native_delivery.entry(id).or_default().needs_snapshot = true;
                } else {
                    self.queue_native(id, message, None);
                }
            }
        }
    }

    fn queue_native(&mut self, id: u64, message: ServerMessage, retained_bytes: Option<usize>) {
        let Some(client) = self.clients.get(&id) else {
            return;
        };
        let bytes = retained_bytes.unwrap_or_else(|| control::message_bytes(&message));
        let pending = self.native_delivery.entry(id).or_default();
        if pending.messages.len() >= RECORD_LIMIT
            || client
                .delivery_bytes
                .load(Ordering::Acquire)
                .saturating_add(pending.bytes)
                .saturating_add(bytes)
                > BYTE_LIMIT
        {
            // Output alone never reaches this path. A client must consume its
            // replies, host actions and final-state barriers within a finite budget.
            self.disconnect(id);
            return;
        }
        pending.bytes += bytes;
        pending.messages.push_back(Queued { message, bytes });
    }

    /// Called on every owner turn, including the existing 50ms idle tick. This
    /// recovers a final idle tail without new PTY bytes or per-output drain ACKs.
    pub(super) fn flush_native(&mut self) {
        if self.native_delivery.is_empty() {
            return;
        }
        let ids = self.native_delivery.keys().copied().collect::<Vec<_>>();
        let mut snapshot_ready = Vec::new();
        for id in ids {
            let Some(mut pending) = self.native_delivery.remove(&id) else {
                continue;
            };
            if pending
                .closing
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(10))
            {
                // Match control-client closing: an abandoned peer must not keep
                // an empty owner alive indefinitely while refusing its final state.
                self.disconnect(id);
                continue;
            }
            let mut disconnected = false;
            for _ in 0..64 {
                let Some(queued) = pending.messages.front() else {
                    break;
                };
                let snapshot = matches!(queued.message, ServerMessage::Snapshot { .. });
                if snapshot && self.snapshots_pending.contains(&id) {
                    break;
                }
                let queued = pending.messages.pop_front().unwrap();
                let Some(client) = self.clients.get(&id) else {
                    disconnected = true;
                    break;
                };
                match deliver(client, queued.message) {
                    Ok(()) => {
                        pending.bytes -= queued.bytes;
                        if snapshot {
                            self.snapshots_pending.insert(id);
                        }
                    }
                    Err(TrySendError::Full(message)) => {
                        pending.messages.push_front(Queued {
                            message,
                            bytes: queued.bytes,
                        });
                        break;
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            if disconnected || pending.closing.is_some() && pending.messages.is_empty() {
                self.disconnect(id);
                continue;
            }
            if pending.messages.is_empty()
                && pending.needs_snapshot
                && !self.snapshots_pending.contains(&id)
                && self
                    .clients
                    .get(&id)
                    .is_some_and(|client| client.delivery_bytes.load(Ordering::Acquire) == 0)
            {
                snapshot_ready.push(id);
            }
            if pending.needs_snapshot || !pending.messages.is_empty() {
                self.native_delivery.insert(id, pending);
            }
        }
        // Building a checkpoint is synchronous. Bound it to one ready client
        // per owner turn, rotating so one busy view cannot monopolize recovery.
        if let Some(id) = snapshot_ready
            .iter()
            .copied()
            .find(|id| *id > self.native_recovery_cursor)
            .or_else(|| snapshot_ready.first().copied())
        {
            self.native_recovery_cursor = id;
            self.native_delivery.remove(&id);
            self.snapshots_requested.remove(&id);
            if !self.snapshot_now(id)
                && self.clients.contains_key(&id)
                && !self.native_delivery.contains_key(&id)
            {
                // A rejected checkpoint cannot bridge missing parser input.
                // SnapshotAborted is queued first; close rather than resume deltas.
                self.disconnect(id);
            }
        }
    }

    pub(super) fn defer_native_snapshot(&mut self, id: u64) -> bool {
        if let Some(pending) = self.native_delivery.get_mut(&id) {
            if pending.closing.is_none() {
                pending.needs_snapshot = true;
            }
            true
        } else {
            false
        }
    }

    /// A naturally removed pane must leave its final terminal in already attached
    /// views. Freeze only for clients with a replay gap, before removing the pane.
    pub(super) fn final_native_barrier(&mut self, pane: Id, status: i32) {
        if self
            .engine
            .pane_option(pane, "remain-on-exit")
            .is_some_and(|value| {
                matches!(value, "on" | "1" | "key")
                    || matches!(value, "failed" | "failed-key") && status != 0
            })
        {
            return;
        }
        let ids = self
            .native_delivery
            .iter()
            .filter(|(_, pending)| pending.needs_snapshot && pending.closing.is_none())
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return;
        }
        // Output is fully drained. Match exited_with_signal's terminal cleanup
        // before freezing the view which that method is about to remove.
        if let Some(pane) = self.engine.panes.get_mut(&pane) {
            pane.status = Some(status);
            pane.terminal.clear_agent_status();
            pane.terminal.program_status_exited();
        }
        let state = self.engine.native_snapshot();
        let bytes = match native::snapshot_bytes(&state, self.snapshot_limit.min(BYTE_LIMIT as u64))
        {
            Ok(bytes) => bytes as usize,
            Err(_) => {
                for id in ids {
                    self.disconnect(id);
                }
                return;
            }
        };
        for id in ids {
            self.queue_native(
                id,
                ServerMessage::Snapshot {
                    instance_id: self.instance.clone(),
                    sequence: self.engine.sequence,
                    state: state.clone(),
                },
                Some(bytes.saturating_add(128)),
            );
        }
    }

    /// Normal transport closure is immediate. A lifecycle Exit/Detached already
    /// queued behind a recovery barrier must first reach the existing FIFO.
    pub(super) fn native_closing(&self, id: u64) -> bool {
        self.native_delivery
            .get(&id)
            .is_some_and(|pending| pending.closing.is_some())
    }

    pub(super) fn disconnect_after_native_delivery(&mut self, id: u64) {
        self.scheduler.lost(id);
        if let Some(pending) = self.native_delivery.get_mut(&id)
            && !pending.messages.is_empty()
        {
            pending.closing = Some(Instant::now());
            pending.needs_snapshot = false;
            // Keep only transport ownership while final messages drain. A closed
            // view cannot elect host actions, retain focus/panes, or control size.
            if let Some(client) = self.clients.get_mut(&id) {
                let context = std::mem::take(&mut client.context);
                client.context.client_name = context.client_name.clone();
                client.subscribed = false;
                client.control = false;
                self.disconnect_format_jobs(&context.client_name);
                self.client_closed(id, &context);
            }
            self.server_watchers.remove(&id);
            self.management_clients.remove(&id);
            self.snapshots_requested.remove(&id);
            self.pane_waiters.remove(&id);
            self.stdin_streams.remove(&id);
            self.file_clients.remove(&id);
            self.size_views.retain(|(client, _)| *client != id);
            self.size_activity.remove(&id);
            self.disconnect_host(id);
            self.recalculate_sizes();
            self.check_unattached_sessions();
        } else {
            self.disconnect(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn host(capacity: usize) -> (Host, Receiver<ServerMessage>) {
        let (events, _) = mpsc::sync_channel(32);
        let mut host = Host::new(events, &Options::new("/unused"), BTreeMap::new());
        let (tx, rx) = mpsc::sync_channel(capacity);
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
        host.engine
            .execute(
                &tmuxx_command::parse(b"new-session -d ''").unwrap()[0],
                &mut ClientContext::default(),
            )
            .unwrap();
        (host, rx)
    }
    fn receive(host: &mut Host, rx: &Receiver<ServerMessage>) -> ServerMessage {
        let message = rx.try_recv().unwrap();
        if let Some(client) = host.clients.get(&1) {
            client
                .delivery_bytes
                .fetch_sub(control::message_bytes(&message), Ordering::AcqRel);
        }
        if matches!(message, ServerMessage::Snapshot { .. }) {
            host.snapshots_pending.remove(&1);
        }
        message
    }
    fn reply(id: u64) -> ServerMessage {
        ServerMessage::Reply(Response {
            request_id: id,
            status: None,
            output: vec![b'x'],
            error: None,
        })
    }

    #[test]
    fn lagging_view_recovers_current_checkpoint_and_keeps_critical_order() {
        let (mut host, rx) = host(2);
        for index in 0..1000 {
            host.pane_output(0, format!("line {index}\r\n").as_bytes());
        }
        host.pane_output(0, b"\x1b]2;final\x07\x1b[1;31mTAIL\x1b[22m");
        assert!(host.clients.contains_key(&1));
        assert!(host.native_delivery[&1].needs_snapshot);
        let expected = serde_json::to_value(host.engine.native_snapshot()).unwrap();
        host.send(1, reply(7));
        host.send(
            1,
            ServerMessage::HostEffect {
                sequence: host.engine.sequence,
                pane_id: 0,
                event_id: 42,
                action: serde_json::json!("bell"),
            },
        );
        host.send(1, reply(8));
        receive(&mut host, &rx);
        receive(&mut host, &rx);
        host.flush_native();
        assert!(matches!(
            receive(&mut host, &rx),
            ServerMessage::Reply(Response { request_id: 7, .. })
        ));
        assert!(matches!(
            receive(&mut host, &rx),
            ServerMessage::HostEffect { event_id: 42, .. }
        ));
        host.flush_native();
        assert!(matches!(
            receive(&mut host, &rx),
            ServerMessage::Reply(Response { request_id: 8, .. })
        ));
        host.flush_native();
        let ServerMessage::Snapshot {
            state, sequence, ..
        } = receive(&mut host, &rx)
        else {
            panic!("missing recovery");
        };
        assert_eq!(serde_json::to_value(state).unwrap(), expected);
        assert_eq!(sequence, host.engine.sequence);
        assert!(!host.native_delivery.contains_key(&1));
        host.pane_output(0, b"after");
        assert!(
            matches!(receive(&mut host, &rx), ServerMessage::PaneOutput { data, .. } if data == b"after")
        );
    }

    #[test]
    fn full_snapshot_queue_retries_after_ack_without_resuming_skipped_deltas() {
        let (mut host, rx) = host(1);
        host.pane_output(0, b"before");
        host.snapshot(1);
        assert!(!host.snapshots_pending.contains(&1));
        assert!(host.native_delivery[&1].needs_snapshot);
        host.pane_output(0, b"missed");
        receive(&mut host, &rx);
        host.flush_native();
        assert!(host.snapshots_pending.contains(&1));
        host.pane_output(0, b"during-snapshot");
        host.snapshot(1);
        host.flush_native();
        assert!(host.native_delivery[&1].needs_snapshot);
        let first = receive(&mut host, &rx);
        assert!(matches!(first, ServerMessage::Snapshot { .. }));
        host.flush_native();
        let ServerMessage::Snapshot { state, .. } = receive(&mut host, &rx) else {
            panic!("missing second snapshot");
        };
        assert_eq!(
            serde_json::to_value(state).unwrap(),
            serde_json::to_value(host.engine.native_snapshot()).unwrap()
        );
        assert!(!host.native_delivery.contains_key(&1));
    }

    #[test]
    fn rejected_recovery_snapshot_closes_without_resuming_delta_stream() {
        let (mut host, rx) = host(1);
        host.pane_output(0, b"before");
        host.pane_output(0, b"gap");
        receive(&mut host, &rx);
        host.snapshot_limit = 1;
        host.flush_native();
        assert!(!host.clients.contains_key(&1));
        assert!(matches!(
            rx.try_recv().unwrap(),
            ServerMessage::SnapshotAborted { .. }
        ));
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn critical_backlog_and_final_barriers_share_finite_budgets() {
        let (mut host, _rx) = host(1);
        host.send(1, reply(0));
        for id in 1..=RECORD_LIMIT as u64 {
            host.send(1, reply(id));
        }
        assert_eq!(host.native_delivery[&1].messages.len(), RECORD_LIMIT);
        host.send(1, reply(999));
        assert!(!host.clients.contains_key(&1));
        assert!(!host.native_delivery.contains_key(&1));

        let (mut host, _rx) = self::host(1);
        host.pane_output(0, b"queued");
        host.pane_output(0, b"gap");
        host.queue_native(1, reply(1), Some(BYTE_LIMIT));
        assert!(!host.clients.contains_key(&1));
    }

    #[test]
    fn two_final_barriers_wait_for_snapshot_ack_and_precede_close() {
        let (mut host, rx) = host(1);
        host.engine
            .execute(
                &tmuxx_command::parse(b"split-window -d ''").unwrap()[0],
                &mut ClientContext::default(),
            )
            .unwrap();
        host.snapshot(1); // Deliberately leave a regular snapshot outstanding.
        host.pane_output(0, b"FIRST-FINAL");
        host.final_native_barrier(0, 0);
        host.engine.exited(0, 0);
        host.pane_output(1, b"SECOND-FINAL");
        host.final_native_barrier(1, 0);
        host.engine.exited(1, 0);
        host.send(
            1,
            ServerMessage::Exit {
                reason: "session destroyed".into(),
            },
        );
        host.disconnect_after_native_delivery(1);
        assert!(host.clients.contains_key(&1));
        host.flush_native();
        assert_eq!(host.native_delivery[&1].messages.len(), 3);
        receive(&mut host, &rx);
        host.flush_native();
        let ServerMessage::Snapshot { state, .. } = receive(&mut host, &rx) else {
            panic!("missing first final");
        };
        assert!(
            state
                .panes
                .iter()
                .find(|p| p.id == 0)
                .unwrap()
                .terminal
                .as_ref()
                .unwrap()
                .restore()
                .unwrap()
                .capture()
                .contains("FIRST-FINAL")
        );
        host.flush_native();
        let ServerMessage::Snapshot { state, .. } = receive(&mut host, &rx) else {
            panic!("missing second final");
        };
        assert!(
            state
                .panes
                .iter()
                .find(|p| p.id == 1)
                .unwrap()
                .terminal
                .as_ref()
                .unwrap()
                .restore()
                .unwrap()
                .capture()
                .contains("SECOND-FINAL")
        );
        host.flush_native();
        assert!(!host.clients.contains_key(&1));
        assert!(matches!(rx.try_recv().unwrap(), ServerMessage::Exit { .. }));
    }

    #[test]
    fn native_closing_cancels_unstarted_commands_and_rejects_late_requests() {
        let (mut host, _rx) = host(1);
        let original = host.engine.sessions[&0].name.clone();
        let rename = |id, name: &[u8]| Request {
            request_id: id,
            argv: vec![
                b"rename-session".to_vec(),
                b"-t".to_vec(),
                b"$0".to_vec(),
                name.to_vec(),
            ],
            stdin: None,
            subscribe: false,
            control: false,
        };
        host.request(1, rename(1, b"queued"));
        host.send(1, reply(0));
        host.send(
            1,
            ServerMessage::Exit {
                reason: "closed".into(),
            },
        );
        host.disconnect_after_native_delivery(1);
        assert!(host.native_closing(1));
        host.request(1, rename(2, b"late"));
        for _ in 0..4 {
            host.scheduled_work();
        }
        assert_eq!(host.engine.sessions[&0].name, original);
        assert!(host.clients.contains_key(&1));
    }

    #[test]
    fn closing_transport_releases_host_election_focus_and_size_authority() {
        let (mut host, _rx) = host(1);
        let client = host.clients.get_mut(&1).unwrap();
        client.context.session = Some(0);
        client.context.host_effects = true;
        client.context.size_requested = true;
        client.context.window_size_requested = true;
        client.context.window_sizes.insert(0, (120, 40));
        let (healthy, messages) = mpsc::sync_channel(8);
        let mut client = host.clients[&1].clone();
        client.tx = healthy;
        client.delivery_bytes = Arc::new(AtomicUsize::new(0));
        host.clients.insert(2, client);
        let mut context = host.clients[&1].context.clone();
        host.view_state(
            1,
            &tmuxx_command::parse(b"tmuxx-view-state -t %0").unwrap()[0],
            Some(br#"{"focused":true,"visible":true,"color_scheme":"dark"}"#.to_vec()),
            &mut context,
        )
        .unwrap();
        assert!(host.view_states.focused(1, 0));
        host.size_views.push((1, 0));
        host.size_activity.insert(1, Instant::now());
        host.latest_clients.insert(0, 1);
        host.send(1, reply(0));
        host.send(
            1,
            ServerMessage::Exit {
                reason: "closed".into(),
            },
        );
        host.disconnect_after_native_delivery(1);
        let client = &host.clients[&1];
        assert!(!client.subscribed && !client.context.host_effects);
        assert!(!host.view_states.has_view(1, 0));
        assert!(client.context.session.is_none());
        assert!(!client.context.size_requested && !client.context.window_size_requested);
        assert!(client.context.window_sizes.is_empty());
        assert!(host.size_views.iter().all(|(id, _)| *id != 1));
        assert!(!host.size_activity.contains_key(&1));
        assert!(host.latest_clients.values().all(|id| *id != 1));
        host.dispatch_host(0, HostAction::Bell).unwrap();
        assert!(
            messages
                .try_iter()
                .any(|message| matches!(message, ServerMessage::HostEffect { .. }))
        );
    }

    #[test]
    fn closing_latest_size_owner_falls_back_among_multiple_survivors() {
        let (mut host, _rx) = host(1);
        host.engine
            .windows
            .get_mut(&0)
            .unwrap()
            .options
            .insert("window-size".into(), "latest".into());
        let mut receivers = Vec::new();
        for (id, cols) in [(1, 120), (2, 80), (3, 90)] {
            if id != 1 {
                let (tx, rx) = mpsc::sync_channel(8);
                let mut client = host.clients[&1].clone();
                client.tx = tx;
                client.delivery_bytes = Arc::new(AtomicUsize::new(0));
                host.clients.insert(id, client);
                receivers.push(rx);
            }
            let context = &mut host.clients.get_mut(&id).unwrap().context;
            context.session = Some(0);
            context.size_requested = true;
            context.cols = cols;
            context.rows = 24;
            host.size_activity
                .insert(id, Instant::now() + Duration::from_millis(id));
        }
        host.latest_clients.insert(0, 1);
        host.recalculate_sizes();
        assert_eq!(host.engine.windows[&0].cols, 120);
        host.send(1, reply(0));
        host.send(
            1,
            ServerMessage::Exit {
                reason: "closed".into(),
            },
        );
        host.disconnect_after_native_delivery(1);
        assert_eq!(host.latest_clients[&0], 3);
        assert_eq!(host.engine.windows[&0].cols, 90);
    }

    #[test]
    fn abandoned_closing_client_releases_its_final_barrier() {
        let (mut host, _rx) = host(1);
        host.snapshot(1);
        host.pane_output(0, b"final");
        host.final_native_barrier(0, 0);
        host.send(
            1,
            ServerMessage::Exit {
                reason: "session destroyed".into(),
            },
        );
        host.disconnect_after_native_delivery(1);
        host.native_delivery.get_mut(&1).unwrap().closing =
            Some(Instant::now() - Duration::from_secs(11));
        host.flush_native();
        assert!(!host.clients.contains_key(&1));
        assert!(!host.native_delivery.contains_key(&1));
        assert!(!host.snapshots_pending.contains(&1));
    }

    #[test]
    fn remain_on_exit_uses_latest_snapshot_without_a_final_barrier() {
        for (value, status) in [("on", 0), ("key", 0), ("failed", 1), ("failed-key", 1)] {
            let (mut host, _rx) = host(1);
            host.engine
                .panes
                .get_mut(&0)
                .unwrap()
                .options
                .insert("remain-on-exit".into(), value.into());
            host.pane_output(0, b"queued");
            host.pane_output(0, b"gap");
            host.final_native_barrier(0, status);
            assert!(host.native_delivery[&1].messages.is_empty());
        }
    }
}
