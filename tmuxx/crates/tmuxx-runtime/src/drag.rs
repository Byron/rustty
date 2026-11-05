//! OSC 72 data stays with the pane owner; native views only receive metadata.
use super::*;
use tmuxx_terminal::dnd;

const MAX_VIEW_OFFERS: usize = 16_384;

#[derive(Clone, Copy)]
struct Binding {
    client: u64,
    registration: u64,
    offer: u64,
}

#[derive(Default)]
pub(super) struct Bindings {
    active: BTreeMap<Id, Binding>,
    /// Retain one tombstone per view/pane so a delayed Move cannot restart a drag.
    latest: BTreeMap<(u64, Id), u64>,
}

impl Bindings {
    pub(super) fn forget_client(&mut self, client: u64) {
        self.latest.retain(|(source, _), _| *source != client);
    }
}

impl Host {
    pub(super) fn native_drag(
        &mut self,
        client: u64,
        command: &tmuxx_command::Command,
        input: Option<Vec<u8>>,
    ) -> Result<Outcome, String> {
        if !self
            .clients
            .get(&client)
            .is_some_and(|client| client.context.host_effects && !client.control)
        {
            return Err("native drag requires a GUI connection".into());
        }
        let args = tmuxx_command::Args::parse(&command.args[1..], "t:")?;
        args.no_rest()?;
        let pane = args
            .get(b't')
            .and_then(|target| target.strip_prefix(b"%"))
            .and_then(|target| std::str::from_utf8(target).ok())
            .and_then(|target| target.parse::<Id>().ok())
            .ok_or("native drag requires -t %PANE")?;
        let input = input.ok_or("missing native drag command")?;
        if input.len() > MAX_REQUEST_BYTES {
            return Err("native drag exceeds sixteen MiB".into());
        }
        let command: dnd::Command = native::decode(&input).map_err(|error| error.to_string())?;
        self.apply_drag(client, pane, command)?;
        Ok(Outcome::default())
    }

    fn apply_drag(&mut self, client: u64, pane: Id, command: dnd::Command) -> Result<(), String> {
        command.validate().map_err(|error| error.to_string())?;
        if !self.view_states.has_view(client, pane) || !self.view_states.visible(client, pane) {
            return Err("native drag requires a visible registered GUI view".into());
        }
        let state = self
            .engine
            .panes
            .get(&pane)
            .ok_or("drag pane no longer exists")?;
        if state.status.is_some() || state.empty {
            return Err("drag pane is not running".into());
        }
        if state.input_disabled {
            return Err("pane input is disabled".into());
        }
        let process = self
            .processes
            .get(&pane)
            .filter(|process| process.status.is_none() && !process.eof)
            .ok_or("drag pane is not running")?;
        let binding = Binding {
            client,
            registration: command.registration,
            offer: command.offer,
        };
        let same = self.drags.active.get(&pane).is_some_and(|active| {
            active.client == client
                && active.offer == command.offer
                && active.registration == command.registration
        });
        if !same {
            if !matches!(command.action, dnd::Action::Move { .. }) {
                return Err("drag offer belongs to another view or has ended".into());
            }
            if self
                .drags
                .active
                .get(&pane)
                .is_some_and(|active| active.client != client)
            {
                return Err("drag offer belongs to another view".into());
            }
            if self
                .drags
                .latest
                .get(&(client, pane))
                .is_some_and(|last| command.offer <= *last)
            {
                return Err("drag offer token is stale".into());
            }
            if self.drags.latest.len() >= MAX_VIEW_OFFERS
                && !self.drags.latest.contains_key(&(client, pane))
            {
                return Err("too many native drag views".into());
            }
        }
        let before = state.terminal.drag_revision();
        self.engine
            .panes
            .get_mut(&pane)
            .unwrap()
            .terminal
            .drag_with(command, |bytes| {
                if bytes.is_empty() {
                    return Ok(());
                }
                process.write_input(bytes).map_err(io::Error::other)
            })
            .map_err(|error| error.to_string())?;
        self.drags.active.insert(pane, binding);
        self.drags.latest.insert((client, pane), binding.offer);
        self.publish_drag_if_changed(pane, before);
        Ok(())
    }

    pub(super) fn publish_drag_if_changed(&mut self, pane: Id, before: Option<u64>) {
        let Some(terminal) = self.engine.panes.get(&pane).map(|pane| &pane.terminal) else {
            return;
        };
        if terminal.drag_revision() == before {
            return;
        }
        let view = terminal.drag_view();
        if self.drags.active.get(&pane).is_some_and(|active| {
            view.as_ref().is_none_or(|view| {
                view.registration != active.registration || view.offer != Some(active.offer)
            })
        }) {
            self.drags.active.remove(&pane);
        }
        self.engine.sequence = self.engine.sequence.wrapping_add(1);
        let update = native::DragUpdate {
            instance_id: self.instance.clone(),
            sequence: self.engine.sequence,
            pane_id: pane,
            view,
        };
        let mut data = native::DND_CONTROL_PREFIX.to_vec();
        // All fields are bounded ordinary values; serialization cannot fail.
        data.extend(rmp_serde::to_vec_named(&update).expect("serializable drag metadata"));
        let clients = self
            .clients
            .iter()
            .filter(|(_, client)| {
                client.subscribed && client.context.host_effects && !client.control
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for client in clients {
            self.send(client, ServerMessage::Control { data: data.clone() });
        }
    }

    pub(super) fn release_drag(&mut self, client: u64, pane: Id) {
        let Some(binding) = self
            .drags
            .active
            .get(&pane)
            .copied()
            .filter(|binding| binding.client == client)
        else {
            return;
        };
        self.drags.active.remove(&pane);
        let Some(terminal) = self
            .engine
            .panes
            .get_mut(&pane)
            .map(|pane| &mut pane.terminal)
        else {
            return;
        };
        let before = terminal.drag_revision();
        let bytes = terminal.cancel_drag(binding.offer);
        if !bytes.is_empty()
            && let Some(process) = self.processes.get(&pane)
        {
            let _ = process.write_input(bytes);
        }
        self.publish_drag_if_changed(pane, before);
    }

    pub(super) fn prune_drags(&mut self) {
        let unavailable = self
            .drags
            .active
            .iter()
            .filter_map(|(&pane, binding)| {
                self.engine
                    .panes
                    .get(&pane)
                    .is_some_and(|pane| pane.status.is_some() || pane.empty || pane.input_disabled)
                    .then_some((binding.client, pane))
            })
            .collect::<Vec<_>>();
        for (client, pane) in unavailable {
            self.release_drag(client, pane);
        }
        self.drags
            .active
            .retain(|pane, _| self.engine.panes.contains_key(pane));
        self.drags
            .latest
            .retain(|(_, pane), _| self.engine.panes.contains_key(pane));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    struct Fixture {
        host: Host,
        clients: Vec<Receiver<ServerMessage>>,
        inputs: Vec<Receiver<Vec<u8>>>,
        // Keep the real writer channels alive while assertions inspect isolated queues.
        _writers: Vec<SyncSender<Vec<u8>>>,
        _events: Receiver<Event>,
    }

    fn fixture() -> Fixture {
        let (events, received) = mpsc::sync_channel(512);
        let mut host = Host::new(events, &Options::new("/unused-drag-test"), BTreeMap::new());
        let mut clients = Vec::new();
        for id in 1..=3 {
            let (tx, rx) = mpsc::sync_channel(512);
            host.clients.insert(
                id,
                Client {
                    tx,
                    context: ClientContext {
                        host_effects: id != 3,
                        ..Default::default()
                    },
                    subscribed: true,
                    control: false,
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
            let (tx, rx) = mpsc::sync_channel(64);
            writers.push(std::mem::replace(
                &mut host.processes.get_mut(&pane).unwrap().input,
                tx,
            ));
            inputs.push(rx);
            for client in [1, 2] {
                register_view(&mut host, client, pane, true);
            }
            host.pane_output(pane, b"\x1b]72;t=a:i=7;text/plain\x1b\\");
        }
        for rx in &clients {
            let _ = rx.try_iter().collect::<Vec<_>>();
        }
        Fixture {
            host,
            clients,
            inputs,
            _writers: writers,
            _events: received,
        }
    }

    fn register_view(host: &mut Host, client: u64, pane: Id, visible: bool) {
        let source = format!("tmuxx-host-state -t %{pane}");
        let command = tmuxx_command::parse(source.as_bytes()).unwrap().remove(0);
        let input = serde_json::to_vec(
            &serde_json::json!({"focused":false,"visible":visible,"color_scheme":null}),
        )
        .unwrap();
        let mut context = host.clients[&client].context.clone();
        host.view_state(client, &command, Some(input), &mut context)
            .unwrap();
    }

    fn position() -> dnd::MoveEvent {
        dnd::MoveEvent {
            cell_x: 2,
            cell_y: 1,
            pixel_x: 20,
            pixel_y: 18,
            operations: dnd::Operations {
                copy: true,
                move_: false,
            },
        }
    }
    fn moving(host: &Host, pane: Id, offer: u64) -> dnd::Command {
        dnd::Command {
            registration: host.engine.panes[&pane]
                .terminal
                .drag_view()
                .unwrap()
                .registration,
            offer,
            action: dnd::Action::Move {
                event: position(),
                mimes: vec![b"text/plain".to_vec()],
            },
        }
    }
    fn dropping(host: &Host, pane: Id, offer: u64, data: Vec<u8>) -> dnd::Command {
        let mut command = moving(host, pane, offer);
        command.action = dnd::Action::Drop {
            event: position(),
            items: vec![dnd::Item {
                mime: b"text/plain".to_vec(),
                data,
            }],
        };
        command
    }
    fn send(
        host: &mut Host,
        client: u64,
        pane: Id,
        input: &dnd::Command,
    ) -> Result<Outcome, String> {
        let source = format!("tmuxx-dnd -t %{pane}");
        let command = tmuxx_command::parse(source.as_bytes()).unwrap().remove(0);
        host.native_drag(
            client,
            &command,
            Some(rmp_serde::to_vec_named(input).unwrap()),
        )
    }
    fn take_input(f: &Fixture, pane: usize) -> Vec<u8> {
        let bytes = f.inputs[pane].try_recv().expect("one owner PTY message");
        f.host.processes[&(pane as u64)]
            .input_bytes
            .fetch_sub(bytes.len(), Ordering::AcqRel);
        bytes
    }
    fn accept(f: &mut Fixture, pane: Id) {
        f.host
            .pane_output(pane, b"\x1b]72;t=m:o=1;text/plain\x1b\\");
    }
    fn updates(rx: &Receiver<ServerMessage>) -> Vec<native::DragUpdate> {
        rx.try_iter()
            .filter_map(|message| match message {
                ServerMessage::Control { data } => data
                    .strip_prefix(native::DND_CONTROL_PREFIX)
                    .map(|bytes| native::decode(bytes).unwrap()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn drag_targets_one_owner_pane_and_cannot_be_stolen_by_another_gui() {
        let mut f = fixture();
        let move_first = moving(&f.host, 0, 10);
        assert!(
            send(&mut f.host, 3, 0, &move_first)
                .unwrap_err()
                .contains("GUI")
        );
        send(&mut f.host, 1, 0, &move_first).unwrap();
        assert!(take_input(&f, 0).starts_with(b"\x1b]72;t=m"));
        assert!(f.inputs[1].try_recv().is_err());
        assert!(
            send(&mut f.host, 2, 0, &move_first)
                .unwrap_err()
                .contains("another view")
        );
        let move_second = moving(&f.host, 1, 20);
        send(&mut f.host, 2, 1, &move_second).unwrap();
        take_input(&f, 1);
        accept(&mut f, 0);
        let drop = dropping(&f.host, 0, 10, b"owner-only secret".to_vec());
        assert!(send(&mut f.host, 2, 0, &drop).is_err());
        send(&mut f.host, 1, 0, &drop).unwrap();
        assert!(take_input(&f, 0).starts_with(b"\x1b]72;t=M"));
        assert!(f.inputs[1].try_recv().is_err());
        for rx in &f.clients[..2] {
            let projected = updates(rx);
            assert!(projected.iter().any(|update| update.pane_id == 0
                && update.view.as_ref().is_some_and(|view| view.dropped)));
            assert!(
                !serde_json::to_string(&projected)
                    .unwrap()
                    .contains("owner-only secret")
            );
        }
        assert!(updates(&f.clients[2]).is_empty());
        assert!(
            !serde_json::to_string(&f.host.engine.native_snapshot())
                .unwrap()
                .contains("owner-only secret")
        );
        f.host.pane_output(0, b"\x1b]72;t=r:x=1\x1b\\");
        assert!(
            take_input(&f, 0)
                .windows(8)
                .any(|bytes| bytes == b"b3duZXIt")
        );
        assert!(f.inputs[1].try_recv().is_err());
    }

    #[test]
    fn stale_offers_registration_changes_release_and_disconnect_invalidate_only_their_source() {
        let mut f = fixture();
        let first = moving(&f.host, 0, 10);
        send(&mut f.host, 1, 0, &first).unwrap();
        take_input(&f, 0);
        accept(&mut f, 0);
        f.host.release_view_state(1, 0);
        assert!(
            take_input(&f, 0)
                .windows(9)
                .any(|bytes| bytes == b"x=-1:y=-1")
        );
        register_view(&mut f.host, 1, 0, true);
        assert!(
            send(&mut f.host, 1, 0, &first)
                .unwrap_err()
                .contains("stale")
        );
        let next = moving(&f.host, 0, 11);
        send(&mut f.host, 1, 0, &next).unwrap();
        take_input(&f, 0);
        f.host.pane_output(0, b"\x1b]72;t=a:i=8:m=1;text/\x1b\\");
        assert!(!f.host.engine.panes[&0].terminal.drag_view().unwrap().ready);
        assert!(!f.host.drags.active.contains_key(&0));
        assert!(send(&mut f.host, 1, 0, &next).is_err());
        f.host.pane_output(0, b"\x1b]72;m=0;plain\x1b\\");
        let newest = moving(&f.host, 0, 12);
        send(&mut f.host, 1, 0, &newest).unwrap();
        take_input(&f, 0);
        accept(&mut f, 0);
        let drop = dropping(&f.host, 0, 12, b"retained".to_vec());
        send(&mut f.host, 1, 0, &drop).unwrap();
        take_input(&f, 0);
        f.host.disconnect(1);
        assert!(!f.host.drags.active.contains_key(&0));
        assert!(
            f.host.engine.panes[&0]
                .terminal
                .drag_view()
                .unwrap()
                .dropped
        );
        f.host.pane_output(0, b"\x1b]72;t=r:x=1\x1b\\");
        assert!(
            take_input(&f, 0)
                .windows(8)
                .any(|bytes| bytes == b"cmV0YWlu")
        );
        assert!(send(&mut f.host, 1, 0, &newest).is_err());
        let other = moving(&f.host, 0, 1);
        send(&mut f.host, 2, 0, &other).unwrap();
        take_input(&f, 0);
        assert!(
            !f.host.engine.panes[&0]
                .terminal
                .drag_view()
                .unwrap()
                .dropped
        );
        assert_eq!(
            f.host.engine.panes[&0]
                .terminal
                .drag_view()
                .unwrap()
                .accepted,
            None
        );
    }

    #[test]
    fn projection_follows_output_and_partial_acceptance_is_published_immediately() {
        let mut f = fixture();
        let first = moving(&f.host, 0, 1);
        send(&mut f.host, 1, 0, &first).unwrap();
        take_input(&f, 0);
        accept(&mut f, 0);
        for rx in &f.clients {
            let _ = rx.try_iter().collect::<Vec<_>>();
        }
        let source = b"\x1b]72;t=m:o=1:m=1;text/\x1b\\";
        f.host.pane_output(0, source);
        for rx in &f.clients[..2] {
            let messages = rx.try_iter().collect::<Vec<_>>();
            let output = messages.iter().position(|message| matches!(message, ServerMessage::PaneOutput { data, .. } if data == source)).unwrap();
            let projection = messages.iter().position(|message| matches!(message, ServerMessage::Control { data } if data.starts_with(native::DND_CONTROL_PREFIX))).unwrap();
            assert!(projection > output);
            let ServerMessage::PaneOutput { sequence, .. } = messages[output] else {
                unreachable!()
            };
            let ServerMessage::Control { ref data } = messages[projection] else {
                unreachable!()
            };
            let update: native::DragUpdate =
                native::decode(&data[native::DND_CONTROL_PREFIX.len()..]).unwrap();
            assert!(update.sequence > sequence);
            assert_eq!(update.instance_id, f.host.instance);
            assert_eq!(update.view.unwrap().accepted, None);
            // Older native peers still deserialize the existing Control variant.
            let bytes = rmp_serde::to_vec_named(&messages[projection]).unwrap();
            assert!(matches!(
                native::decode::<ServerMessage>(&bytes).unwrap(),
                ServerMessage::Control { .. }
            ));
        }
        assert!(updates(&f.clients[2]).is_empty());
    }

    #[test]
    fn input_budget_and_full_record_queue_never_commit_an_undelivered_drop() {
        let mut f = fixture();
        let first = moving(&f.host, 0, 1);
        f.host.processes[&0]
            .input_bytes
            .store(MAX_REQUEST_BYTES, Ordering::Release);
        let before = f.host.engine.panes[&0].terminal.drag_view();
        assert!(
            send(&mut f.host, 1, 0, &first)
                .unwrap_err()
                .contains("byte budget")
        );
        assert_eq!(f.host.engine.panes[&0].terminal.drag_view(), before);
        assert!(!f.host.drags.active.contains_key(&0));
        f.host.processes[&0].input_bytes.store(0, Ordering::Release);
        send(&mut f.host, 1, 0, &first).unwrap();
        take_input(&f, 0);
        accept(&mut f, 0);
        let before = f.host.engine.panes[&0].terminal.drag_view();
        let (blocked, receiver) = mpsc::sync_channel(1);
        blocked.try_send(vec![b'x']).unwrap();
        f.host.processes.get_mut(&0).unwrap().input = blocked;
        let drop = dropping(&f.host, 0, 1, b"must not commit".to_vec());
        assert!(
            send(&mut f.host, 1, 0, &drop)
                .unwrap_err()
                .contains("byte budget")
        );
        assert_eq!(f.host.engine.panes[&0].terminal.drag_view(), before);
        assert_eq!(f.host.processes[&0].input_bytes.load(Ordering::Acquire), 0);
        std::mem::drop(receiver);
        assert!(send(&mut f.host, 1, 0, &drop).is_err());
        assert_eq!(f.host.engine.panes[&0].terminal.drag_view(), before);
    }

    #[test]
    fn limits_dead_hidden_and_disabled_targets_reject_without_side_effects() {
        let mut f = fixture();
        let first = moving(&f.host, 0, 1);
        f.host.engine.panes.get_mut(&0).unwrap().input_disabled = true;
        assert!(
            send(&mut f.host, 1, 0, &first)
                .unwrap_err()
                .contains("disabled")
        );
        f.host.engine.panes.get_mut(&0).unwrap().input_disabled = false;
        register_view(&mut f.host, 1, 0, false);
        assert!(
            send(&mut f.host, 1, 0, &first)
                .unwrap_err()
                .contains("visible")
        );
        register_view(&mut f.host, 1, 0, true);
        let command = tmuxx_command::parse(b"tmuxx-dnd -t %0").unwrap().remove(0);
        assert!(
            f.host
                .native_drag(1, &command, Some(vec![0; MAX_REQUEST_BYTES + 1]))
                .unwrap_err()
                .contains("sixteen")
        );
        let mut trailing = rmp_serde::to_vec_named(&first).unwrap();
        trailing.push(0);
        assert!(f.host.native_drag(1, &command, Some(trailing)).is_err());
        let mut oversized = first.clone();
        oversized.action = dnd::Action::Drop {
            event: position(),
            items: vec![dnd::Item {
                mime: b"text/plain".to_vec(),
                data: vec![0; dnd::MAX_DROP_BYTES + 1],
            }],
        };
        assert!(send(&mut f.host, 1, 0, &oversized).is_err());
        assert!(f.inputs[0].try_recv().is_err());
        assert!(!f.host.drags.active.contains_key(&0));
        send(&mut f.host, 1, 0, &first).unwrap();
        take_input(&f, 0);
        accept(&mut f, 0);
        let maximum = dropping(&f.host, 0, 1, vec![0; dnd::MAX_DROP_BYTES]);
        send(&mut f.host, 1, 0, &maximum).unwrap();
        assert!(take_input(&f, 0).len() < 4096);
        for rx in &f.clients[..2] {
            assert!(
                updates(rx)
                    .iter()
                    .all(|update| rmp_serde::to_vec_named(update).unwrap().len() < 4096)
            );
        }
        f.host.engine.panes.get_mut(&0).unwrap().status = Some(0);
        assert!(
            send(&mut f.host, 1, 0, &first)
                .unwrap_err()
                .contains("not running")
        );
        f.host.engine.panes.remove(&0);
        f.host.prune_drags();
        assert!(
            send(&mut f.host, 1, 0, &first)
                .unwrap_err()
                .contains("no longer exists")
        );
    }
}
