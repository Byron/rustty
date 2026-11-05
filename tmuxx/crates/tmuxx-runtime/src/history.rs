//! Idle owner storage maintenance; input and scheduled commands always win.
use super::*;
use std::ops::Bound::{Excluded, Unbounded};

impl Host {
    pub(super) fn next_event(
        &mut self,
        rx: &Receiver<Event>,
        command_work: bool,
    ) -> Result<Option<Event>, mpsc::RecvTimeoutError> {
        match rx.try_recv() {
            Ok(event) => return Ok(Some(event)),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(mpsc::RecvTimeoutError::Disconnected);
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if command_work || self.compress_history_step() {
            // Return to the event check after each bounded page; do not sleep
            // once per page or drain a whole history while holding up input.
            return Ok(None);
        }
        rx.recv_timeout(Duration::from_millis(50)).map(Some)
    }

    /// Select one pending pane, including retained panes without a live PTY.
    pub(super) fn compress_history_step(&mut self) -> bool {
        let pending =
            |(_, pane): &(&Id, &tmuxx_core::Pane)| pane.terminal.scrollback_compression_pending();
        let next = self
            .compression_cursor
            .and_then(|cursor| {
                self.engine
                    .panes
                    .range((Excluded(cursor), Unbounded))
                    .find(pending)
            })
            .or_else(|| self.engine.panes.iter().find(pending))
            .map(|(&id, _)| id);
        let Some(id) = next else {
            return false;
        };
        self.compression_cursor = Some(id);
        self.engine
            .panes
            .get_mut(&id)
            .unwrap()
            .terminal
            .compress_scrollback_step();
        true
    }

    /// Full snapshots read every pane, even if later delivery is rejected.
    pub(super) fn request_history_compression(&mut self) {
        for pane in self.engine.panes.values_mut() {
            pane.terminal.request_scrollback_compression();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(host: &mut Host, source: &[u8]) -> Result<Outcome, String> {
        host.engine.execute(
            &tmuxx_command::parse(source).unwrap()[0],
            &mut ClientContext::default(),
        )
    }

    fn host() -> (Host, Receiver<Event>, Receiver<ServerMessage>) {
        let (events, rx) = mpsc::sync_channel(32);
        let mut host = Host::new(events, &Options::new("/unused"), BTreeMap::new());
        command(&mut host, b"new-session -d -x 80 -y 4 ''").unwrap();
        command(&mut host, b"new-window -d ''").unwrap();
        for pane in host.engine.panes.values_mut() {
            pane.terminal.set_history_limit(5000);
            pane.terminal.feed(&b"Build history\r\n".repeat(2000));
        }
        let (tx, received) = mpsc::sync_channel(32);
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
        (host, rx, received)
    }

    fn owned(host: &Host, id: Id) -> usize {
        host.engine.panes[&id]
            .terminal
            .format_value("history_bytes")
            .unwrap()
            .parse()
            .unwrap()
    }

    fn finish(host: &mut Host) {
        for _ in 0..1000 {
            if !host.compress_history_step() {
                assert!(
                    host.engine
                        .panes
                        .values()
                        .all(|pane| !pane.terminal.scrollback_compression_pending())
                );
                return;
            }
        }
        panic!("owner idle pass did not finish");
    }

    #[test]
    fn queued_events_and_scheduled_commands_preempt_each_idle_page() {
        let (mut host, rx, _) = host();
        let resident = [owned(&host, 0), owned(&host, 1)];
        assert!(host.next_event(&rx, true).unwrap().is_none());
        assert_eq!([owned(&host, 0), owned(&host, 1)], resident);
        host.tx
            .send(Event::Request {
                id: 1,
                request: Request {
                    request_id: 7,
                    argv: vec![b"display-message".to_vec()],
                    stdin: None,
                    subscribe: false,
                    control: false,
                },
            })
            .unwrap();
        assert!(matches!(
            host.next_event(&rx, false).unwrap(),
            Some(Event::Request {
                request: Request { request_id: 7, .. },
                ..
            })
        ));
        assert_eq!([owned(&host, 0), owned(&host, 1)], resident);
        assert!(host.next_event(&rx, false).unwrap().is_none());
        let after_page = [owned(&host, 0), owned(&host, 1)];
        assert!(after_page[0] < resident[0]);
        assert_eq!(after_page[1], resident[1]);
        host.tx.send(Event::Shutdown).unwrap();
        assert!(matches!(
            host.next_event(&rx, false).unwrap(),
            Some(Event::Shutdown)
        ));
        assert_eq!([owned(&host, 0), owned(&host, 1)], after_page);
    }

    #[test]
    fn owner_idle_is_fair_finite_and_handles_exited_removed_and_respawned_panes() {
        let (mut host, _, _) = host();
        host.engine.panes.get_mut(&1).unwrap().status = Some(0);
        let resident = [owned(&host, 0), owned(&host, 1)];
        assert!(host.compress_history_step());
        host.engine
            .panes
            .get_mut(&0)
            .unwrap()
            .terminal
            .request_scrollback_compression();
        assert!(host.compress_history_step());
        assert!(owned(&host, 0) < resident[0] && owned(&host, 1) < resident[1]);
        finish(&mut host);
        let cold = [owned(&host, 0), owned(&host, 1)];
        let sequence = host.engine.sequence;
        for _ in 0..4 {
            command(
                &mut host,
                b"display-message -p -t %0 '#{C:Build}:#{history_bytes}'",
            )
            .unwrap();
            assert!(!host.compress_history_step());
            assert_eq!([owned(&host, 0), owned(&host, 1)], cold);
        }
        assert_eq!(host.engine.sequence, sequence);
        command(&mut host, b"kill-pane -t %1").unwrap();
        command(&mut host, b"respawn-pane -k -t %0 ''").unwrap();
        assert!(
            host.engine.panes[&0]
                .terminal
                .scrollback_compression_pending()
        );
        finish(&mut host);
        command(&mut host, b"new-window -d ''").unwrap();
        assert!(
            host.engine.panes[&2]
                .terminal
                .scrollback_compression_pending()
        );
        finish(&mut host);
    }

    #[test]
    fn capture_rearms_only_its_target_and_pending_input_is_inert() {
        let (mut host, _, _) = host();
        finish(&mut host);
        let cold = [owned(&host, 0), owned(&host, 1)];
        command(&mut host, b"capture-pane -p -P -t %0").unwrap();
        assert!(
            !host.engine.panes[&0]
                .terminal
                .scrollback_compression_pending()
        );
        let result = command(&mut host, b"capture-pane -p -S - -t %0").unwrap();
        assert!(result.output.starts_with(b"Build history"));
        assert!(
            host.engine.panes[&0]
                .terminal
                .scrollback_compression_pending()
        );
        assert!(
            !host.engine.panes[&1]
                .terminal
                .scrollback_compression_pending()
        );
        assert!(owned(&host, 0) > cold[0]);
        assert_eq!(owned(&host, 1), cold[1]);
        finish(&mut host);
        assert_eq!([owned(&host, 0), owned(&host, 1)], cold);
    }

    #[test]
    fn snapshot_rejection_reclaims_sources_and_success_keeps_queued_state_immutable() {
        let (mut host, _, received) = host();
        let expected = host.engine.panes[&0].terminal.capture();
        finish(&mut host);
        let cold = [owned(&host, 0), owned(&host, 1)];
        let sequence = host.engine.sequence;
        host.snapshot_limit = 1;
        assert!(!host.snapshot_now(1));
        assert!(matches!(
            received.recv().unwrap(),
            ServerMessage::SnapshotAborted { .. }
        ));
        assert!(
            host.engine
                .panes
                .values()
                .all(|pane| pane.terminal.scrollback_compression_pending())
        );
        assert!(owned(&host, 0) > cold[0] && owned(&host, 1) > cold[1]);
        finish(&mut host);
        assert_eq!([owned(&host, 0), owned(&host, 1)], cold);
        assert_eq!(host.engine.sequence, sequence);
        host.snapshot_limit = tmuxx_protocol::native::MAX_SNAPSHOT_BYTES;
        assert!(host.snapshot_now(1));
        finish(&mut host);
        host.engine
            .panes
            .get_mut(&0)
            .unwrap()
            .terminal
            .feed(b"later live output");
        let ServerMessage::Snapshot { state, .. } = received.recv().unwrap() else {
            panic!("missing snapshot")
        };
        assert_eq!(
            state.panes[0]
                .terminal
                .as_ref()
                .unwrap()
                .restore()
                .unwrap()
                .capture(),
            expected
        );
    }
}
