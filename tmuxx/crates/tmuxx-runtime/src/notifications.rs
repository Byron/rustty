//! OSC 99 reuses host election and guarded replies; notifications are never replayed.
use super::*;
use tmuxx_terminal::notification::{Action, Reply};

#[derive(Default)]
pub(super) struct PendingReply {
    bytes: Vec<u8>,
    pub(super) finished: bool,
}

impl Host {
    pub(super) fn notification_owner_alive(&self, pending: &PendingHostAction) -> bool {
        self.processes.get(&pending.pane).is_some_and(|process| {
            Some(process.generation) == pending.process_generation
                && process.status.is_none()
                && !process.eof
        }) && self
            .engine
            .panes
            .get(&pending.pane)
            .is_some_and(|pane| pane.status.is_none())
    }

    pub(super) fn release_notifications(&mut self, client: u64, pane: Id) {
        let events = self
            .host_actions
            .iter()
            .filter_map(|(&event, pending)| {
                (pending.client == client
                    && pending.pane == pane
                    && pending.action.desktop_notification().is_some())
                .then_some(event)
            })
            .collect();
        self.deny_host_events(events);
    }

    pub(super) fn dispatch_notification(
        &mut self,
        pane: Id,
        action: HostAction,
    ) -> Result<(), String> {
        let command = action.desktop_notification().unwrap();
        command.validate().map_err(|error| error.to_string())?;
        let generation = self.processes.get(&pane).map(|process| process.generation);
        let eligible = |id: u64| {
            self.clients.get(&id).is_some_and(|client| {
                client.subscribed
                    && !client.control
                    && client.context.host_effects
                    && client.context.desktop_notifications
                    && (self.view_states.has_view(id, pane)
                        || self.pane_in_session(pane, client.context.session))
            })
        };
        // Keep close/alive/replacement requests with the GUI that owns this pane's toasts.
        let client = self
            .host_actions
            .values()
            .find(|pending| {
                pending.pane == pane
                    && pending.process_generation == generation
                    && pending.action.desktop_notification().is_some()
                    && !pending
                        .notification_reply
                        .as_ref()
                        .is_some_and(|reply| reply.finished)
                    && eligible(pending.client)
            })
            .map(|pending| pending.client)
            .or_else(|| {
                self.clients
                    .keys()
                    .copied()
                    .filter(|id| eligible(*id))
                    .min_by_key(|id| {
                        (
                            !self.view_states.has_view(*id, pane),
                            !self.view_states.focused(*id, pane),
                            *id,
                        )
                    })
            });
        if let Action::Show(message) = &command.action
            && let Some(identifier) = &message.identifier
        {
            let replaced: Vec<_> = self.host_actions.iter().filter_map(|(&event, pending)| {
                (pending.pane == pane && pending.action.desktop_notification().is_some_and(|old| {
                    matches!(&old.action, Action::Show(old) if old.identifier.as_ref() == Some(identifier))
                })).then_some(event)
            }).collect();
            for event in replaced {
                // Replacement suppresses the old close report, but an already
                // accepted activation/untracked reply still belongs to the TUI.
                if !self.host_actions[&event]
                    .notification_reply
                    .as_ref()
                    .is_some_and(|reply| reply.finished)
                {
                    let _ = self.respond_notification(event, Reply::Finished);
                }
            }
        }
        let show = matches!(command.action, Action::Show(_));
        let count = self
            .host_actions
            .values()
            .filter(|pending| Some(pending.client) == client)
            .filter_map(|pending| pending.action.desktop_notification())
            .filter(|pending| matches!(pending.action, Action::Show(_)) == show)
            .count();
        // One in-flight Show lets the GUI evict its oldest of 128 live toasts.
        if generation.is_none() || count >= if show { 129 } else { 32 } {
            return self.dispatch_host_to(pane, action, None);
        }
        self.dispatch_host_to(pane, action, client)
    }

    pub(super) fn respond_notification(
        &mut self,
        event: u64,
        reply: Reply,
    ) -> Result<Outcome, String> {
        let pending = self
            .host_actions
            .get(&event)
            .ok_or("notification has expired")?;
        let command = pending
            .action
            .desktop_notification()
            .ok_or("not a notification request")?;
        if !self.notification_owner_alive(pending) {
            return Err("notification owner has changed or exited".into());
        }
        if pending
            .notification_reply
            .as_ref()
            .is_some_and(|reply| reply.finished)
        {
            return Err("notification reply was already accepted".into());
        }
        let bytes = command.reply(&reply).map_err(|error| error.to_string())?;
        let keep = matches!(reply, Reply::Untracked);
        if keep && pending.untracked {
            return Ok(Outcome::default());
        }
        let pending = self.host_actions.get_mut(&event).unwrap();
        if keep {
            pending.untracked = true;
        }
        let queued = pending
            .notification_reply
            .get_or_insert_with(PendingReply::default);
        // Each action accepts at most Untracked plus one final reply. Both are
        // bounded by the original command's validated identifiers and buttons.
        queued.bytes.extend(bytes);
        queued.finished = !keep;
        if !keep {
            pending.deadline = None;
        }
        self.flush_notification_reply(event);
        Ok(Outcome::default())
    }

    pub(super) fn retire_notification(&mut self, event: u64) {
        let Some(pending) = self.host_actions.get(&event) else {
            return;
        };
        if !self.notification_owner_alive(pending) {
            self.host_actions.remove(&event);
        } else if !pending
            .notification_reply
            .as_ref()
            .is_some_and(|reply| reply.finished)
        {
            let _ = self.respond_notification(event, Reply::Failed);
        }
    }

    /// Runs before host expiry and at least once per 50ms idle wait. Finished
    /// callbacks remain owner-owned after the source GUI releases/disconnects.
    pub(super) fn flush_notification_replies(&mut self) {
        let events: Vec<_> = self
            .host_actions
            .iter()
            .filter_map(|(&event, pending)| pending.notification_reply.is_some().then_some(event))
            .collect();
        for event in events {
            self.flush_notification_reply(event);
        }
    }

    fn flush_notification_reply(&mut self, event: u64) {
        let Some(pending) = self.host_actions.get(&event) else {
            return;
        };
        if !self.notification_owner_alive(pending) {
            self.host_actions.remove(&event);
            return;
        }
        let pane = pending.pane;
        let Some(queued) = self
            .host_actions
            .get_mut(&event)
            .unwrap()
            .notification_reply
            .take()
        else {
            return;
        };
        if queued.bytes.is_empty() {
            if queued.finished {
                self.host_actions.remove(&event);
            }
            return;
        }
        match self.processes[&pane].try_write_input(queued.bytes) {
            Ok(()) => {
                if queued.finished {
                    self.host_actions.remove(&event);
                }
            }
            Err((bytes, io::ErrorKind::WouldBlock)) => {
                self.host_actions
                    .get_mut(&event)
                    .unwrap()
                    .notification_reply = Some(PendingReply {
                    bytes,
                    finished: queued.finished,
                });
            }
            Err((_, _)) => self.input_failed(pane, "notification input writer disconnected".into()),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    struct Fixture {
        host: Host,
        clients: Vec<Receiver<ServerMessage>>,
        inputs: Vec<Receiver<Vec<u8>>>,
        _writers: Vec<SyncSender<Vec<u8>>>,
        _events: Receiver<Event>,
    }

    fn fixture() -> Fixture {
        let (events, received) = mpsc::sync_channel(512);
        let mut host = Host::new(
            events,
            &Options::new("/unused-notification-test"),
            BTreeMap::new(),
        );
        let mut clients = Vec::new();
        for id in 1..=3 {
            let (tx, rx) = mpsc::sync_channel(512);
            host.clients.insert(
                id,
                Client {
                    tx,
                    subscribed: true,
                    control: false,
                    context: ClientContext {
                        session: Some(0),
                        host_effects: true,
                        desktop_notifications: id != 3,
                        ..Default::default()
                    },
                    delivery_bytes: Arc::new(AtomicUsize::new(0)),
                },
            );
            clients.push(rx);
        }
        for command in [
            b"new-session -d ''".as_slice(),
            b"split-window -d ''",
            b"set-window-option synchronize-panes on",
        ] {
            host.engine
                .execute(
                    &tmuxx_command::parse(command).unwrap()[0],
                    &mut ClientContext::default(),
                )
                .unwrap();
        }
        let mut inputs = Vec::new();
        let mut writers = Vec::new();
        for pane in [0, 1] {
            host.spawn(
                pane,
                "/bin/sh".into(),
                ["/bin/sh", "-c", "read wait"]
                    .map(|arg| arg.as_bytes().to_vec())
                    .into(),
                None,
                std::env::vars().collect(),
                (80, 24),
            )
            .unwrap();
            host.engine.panes.get_mut(&pane).unwrap().empty = false;
            let (tx, rx) = mpsc::sync_channel(256);
            writers.push(std::mem::replace(
                &mut host.processes.get_mut(&pane).unwrap().input,
                tx,
            ));
            inputs.push(rx);
        }
        focus(&mut host, 1, true);
        focus(&mut host, 2, false);
        for rx in &clients {
            rx.try_iter().for_each(drop);
        }
        Fixture {
            host,
            clients,
            inputs,
            _writers: writers,
            _events: received,
        }
    }

    fn focus(host: &mut Host, client: u64, focused: bool) {
        let command = tmuxx_command::parse(b"tmuxx-host-state -t %0")
            .unwrap()
            .remove(0);
        let input = serde_json::to_vec(
            &serde_json::json!({"focused":focused,"visible":true,"color_scheme":null}),
        )
        .unwrap();
        let mut context = host.clients[&client].context.clone();
        host.view_state(client, &command, Some(input), &mut context)
            .unwrap();
    }

    impl Fixture {
        fn issue(&mut self, bytes: &[u8]) -> (u64, tmuxx_terminal::notification::Command) {
            self.host.pane_output(0, bytes);
            let messages = self.clients[0].try_iter().collect::<Vec<_>>();
            let (event, command) = messages
                .iter()
                .find_map(|message| {
                    let ServerMessage::HostEffect {
                        event_id, action, ..
                    } = message
                    else {
                        return None;
                    };
                    let action: HostAction = serde_json::from_value(action.clone()).unwrap();
                    Some((*event_id, action.desktop_notification().unwrap().clone()))
                })
                .expect("elected GUI received a notification");
            for client in &self.clients[1..] {
                assert!(
                    !client
                        .try_iter()
                        .any(|message| matches!(message, ServerMessage::HostEffect { .. }))
                );
            }
            (event, command)
        }

        fn reply(&mut self, client: u64, event: u64, reply: Reply) -> Result<Outcome, String> {
            self.host.respond_host(
                client,
                &[b"tmuxx-host-reply".to_vec(), event.to_string().into_bytes()],
                Some(serde_json::to_vec(&HostResponse::Notification(reply)).unwrap()),
            )
        }

        fn read(&self) -> Vec<u8> {
            let bytes: Vec<u8> = self.inputs[0].try_iter().flatten().collect();
            self.host.processes[&0]
                .input_bytes
                .fetch_sub(bytes.len(), Ordering::AcqRel);
            assert!(
                self.inputs[1].try_recv().is_err(),
                "host reply must not fan out"
            );
            bytes
        }
    }

    #[test]
    fn notification_election_affinity_and_replies_are_owner_scoped() {
        let mut f = fixture();
        let (show, command) = f.issue(b"\x1b]99;i=job:a=report:c=1;Finished\x1b\\");
        assert!(f.reply(2, show, Reply::Closed).is_err());
        assert!(f.reply(1, show, Reply::Activated { button: 1 }).is_err());
        assert!(f.read().is_empty());
        f.reply(1, show, Reply::Untracked).unwrap();
        f.reply(1, show, Reply::Untracked).unwrap();
        assert_eq!(f.read(), command.reply(&Reply::Untracked).unwrap());
        focus(&mut f.host, 1, false);
        focus(&mut f.host, 2, true);
        let (alive, query) = f.issue(b"\x1b]99;i=query:p=alive;\x1b\\");
        let reply = Reply::Alive {
            identifiers: vec!["job".into()],
        };
        f.reply(1, alive, reply.clone()).unwrap();
        assert_eq!(f.read(), query.reply(&reply).unwrap());
        f.reply(1, show, Reply::Activated { button: 0 }).unwrap();
        assert_eq!(
            f.read(),
            command.reply(&Reply::Activated { button: 0 }).unwrap()
        );
        assert!(f.reply(1, show, Reply::Closed).is_err());
        assert!(f.read().is_empty());
    }

    #[test]
    fn notification_replacement_close_and_process_changes_retire_callbacks() {
        let mut f = fixture();
        let (old, _) = f.issue(b"\x1b]99;i=job:a=report:c=1;Old\x1b\\");
        let (new, command) = f.issue(b"\x1b]99;i=job:a=report:c=1;New\x1b\\");
        assert!(f.reply(1, old, Reply::Activated { button: 0 }).is_err());
        let (close, _) = f.issue(b"\x1b]99;i=job:p=close;\x1b\\");
        f.reply(1, new, Reply::Closed).unwrap();
        f.reply(1, close, Reply::Finished).unwrap();
        assert_eq!(f.read(), command.reply(&Reply::Closed).unwrap());
        let (stale, _) = f.issue(b"\x1b]99;i=job:a=report:c=1;Stale\x1b\\");
        f.host.processes.get_mut(&0).unwrap().generation += 1;
        assert!(f.reply(1, stale, Reply::Closed).is_err());
        f.host.disconnect_host(1);
        assert!(
            f.read().is_empty(),
            "old notification must not write to replacement process"
        );
    }

    #[test]
    fn notification_capacity_allows_one_in_flight_eviction_and_then_bounds_bursts() {
        let mut f = fixture();
        let (oldest, _) = f.issue(b"\x1b]99;i=first;capped\x1b\\");
        for index in 1..129 {
            f.issue(format!("\x1b]99;i=item-{index};capped\x1b\\").as_bytes());
        }
        assert_eq!(f.host.host_actions.len(), 129);
        f.host
            .pane_output(0, b"\x1b]99;i=overflow:c=1;rejected\x1b\\");
        assert!(
            !f.clients[0]
                .try_iter()
                .any(|message| matches!(message, ServerMessage::HostEffect { .. }))
        );
        assert_eq!(f.read(), b"\x1b]99;i=overflow:p=close;\x1b\\");
        f.reply(1, oldest, Reply::Closed).unwrap();
        f.issue(b"\x1b]99;i=next;rolling capacity\x1b\\");
        assert_eq!(f.host.host_actions.len(), 129);
    }

    #[test]
    fn notification_queue_accepts_one_shot_callbacks_and_legacy_clients_get_no_extension() {
        let mut f = fixture();
        let (event, command) = f.issue(b"\x1b]99;i=job:a=report:c=1;Ready\x1b\\");
        f.host.processes[&0]
            .input_bytes
            .store(MAX_REQUEST_BYTES, Ordering::Release);
        f.reply(1, event, Reply::Untracked).unwrap();
        f.reply(1, event, Reply::Untracked).unwrap();
        f.reply(1, event, Reply::Activated { button: 0 }).unwrap();
        assert!(f.reply(1, event, Reply::Activated { button: 0 }).is_err());
        assert!(f.host.host_actions.contains_key(&event));
        assert!(f.host.host_actions[&event].deadline.is_none());
        // The client has received success and can forget the callback. Closing
        // its view must not discard bytes already accepted by the owner.
        f.host.release_notifications(1, 0);
        f.host.expire_host();
        assert!(f.host.host_actions.contains_key(&event));
        f.host.processes[&0].input_bytes.store(0, Ordering::Release);
        f.host.flush_notification_replies();
        assert_eq!(
            f.read(),
            [
                command.reply(&Reply::Untracked).unwrap(),
                command.reply(&Reply::Activated { button: 0 }).unwrap()
            ]
            .concat()
        );
        f.host.flush_notification_replies();
        assert!(f.read().is_empty());
        assert!(!f.host.host_actions.contains_key(&event));
        f.host.clients.remove(&1);
        f.host.clients.remove(&2);
        f.host
            .pane_output(0, b"\x1b]99;i=unavailable:c=1;Hidden\x1b\\");
        assert!(
            !f.clients[2]
                .try_iter()
                .any(|message| matches!(message, ServerMessage::HostEffect { .. }))
        );
        assert_eq!(f.read(), b"\x1b]99;i=unavailable:p=close;\x1b\\");
    }

    #[test]
    fn full_writer_channel_drains_accepted_replies_without_another_gui_request() {
        let mut f = fixture();
        let (old, command) = f.issue(b"\x1b]99;i=job:a=report:c=1;First\x1b\\");
        for _ in 0..256 {
            f.host.processes[&0].write_input(vec![b'x']).unwrap();
        }
        f.reply(1, old, Reply::Activated { button: 0 }).unwrap();
        assert!(
            f.host.host_actions[&old]
                .notification_reply
                .as_ref()
                .unwrap()
                .finished
        );
        let (new, _) = f.issue(b"\x1b]99;i=job;Replacement\x1b\\");
        assert!(
            f.host.host_actions.contains_key(&old),
            "replacement must retain acknowledged bytes"
        );
        assert_eq!(f.read(), vec![b'x'; 256]);
        f.host.flush_notification_replies();
        assert_eq!(
            f.read(),
            command.reply(&Reply::Activated { button: 0 }).unwrap()
        );
        assert!(!f.host.host_actions.contains_key(&old));
        assert!(f.host.host_actions.contains_key(&new));
        assert!(f.reply(1, old, Reply::Activated { button: 0 }).is_err());
    }

    #[test]
    fn persistent_view_release_frees_records_and_next_show_uses_a_remaining_view() {
        let mut f = fixture();
        f.host.engine.panes.get_mut(&0).unwrap().persistent = true;
        let (old, command) = f.issue(b"\x1b]99;i=job:c=1;Before detach\x1b\\");
        f.host.release_view_state(1, 0);
        assert!(!f.host.host_actions.contains_key(&old));
        assert_eq!(f.read(), command.reply(&Reply::Failed).unwrap());
        assert!(f.host.processes.contains_key(&0));
        f.host
            .pane_output(0, b"\x1b]99;i=next;Remaining view\x1b\\");
        assert!(
            !f.clients[0]
                .try_iter()
                .any(|message| matches!(message, ServerMessage::HostEffect { .. }))
        );
        let next = f.clients[1]
            .try_iter()
            .find_map(|message| match message {
                ServerMessage::HostEffect { event_id, .. } => Some(event_id),
                _ => None,
            })
            .expect("registered view wins over the detached session-context fallback");
        assert_eq!(f.host.host_actions[&next].client, 2);
        assert!(f.reply(1, old, Reply::Closed).is_err());
        focus(&mut f.host, 1, true);
        f.host.pane_output(0, b"\x1b]99;i=query:p=alive;\x1b\\");
        assert!(
            !f.clients[0]
                .try_iter()
                .any(|message| matches!(message, ServerMessage::HostEffect { .. }))
        );
        assert!(
            f.clients[1]
                .try_iter()
                .any(|message| matches!(message, ServerMessage::HostEffect { .. }))
        );
    }

    #[test]
    fn exited_owner_before_pty_eof_discards_callbacks_and_queued_bytes() {
        let mut f = fixture();
        let (queued, _) = f.issue(b"\x1b]99;i=queued:c=1;Queued\x1b\\");
        let (active, _) = f.issue(b"\x1b]99;i=active:c=1;Active\x1b\\");
        f.host.processes[&0]
            .input_bytes
            .store(MAX_REQUEST_BYTES, Ordering::Release);
        f.reply(1, queued, Reply::Closed).unwrap();
        f.host.processes.get_mut(&0).unwrap().status = Some(0);
        f.host.engine.record_exit(0, 0, None);
        assert!(!f.host.processes[&0].eof);
        f.host.processes[&0].input_bytes.store(0, Ordering::Release);
        f.host.flush_notification_replies();
        f.host.expire_host();
        assert!(f.host.host_actions.is_empty());
        assert!(f.read().is_empty());
        assert!(f.reply(1, active, Reply::Closed).is_err());
    }
}
