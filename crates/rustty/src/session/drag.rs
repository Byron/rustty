use super::*;
use rustty_vt::dnd;
use std::sync::atomic::AtomicU64;

pub(crate) fn new_identity() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        value.checked_add(1)
    })
    .expect("drag identities exhausted")
}

pub(super) struct Cache {
    identity: u64,
    view: Option<Arc<dnd::View>>,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            identity: new_identity(),
            view: None,
        }
    }
}

impl Session {
    /// Changes when an attached owner view is replaced or resynchronized.
    pub fn drag_identity(&self) -> io::Result<u64> {
        if !self.accepts_input() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if let Some(attached) = &self.attached
            && attached.is_native()
        {
            return attached.drag_identity();
        }
        Ok(self.drag.lock().map_err(error)?.identity)
    }

    /// Registration and acceptance only; dropped data remains with the owner.
    pub fn drag_view(&self) -> io::Result<Option<Arc<dnd::View>>> {
        if !self.accepts_input() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if let Some(attached) = &self.attached
            && attached.is_native()
            && let Some(view) = attached.drag_view()?
        {
            return Ok(Some(view));
        }
        let terminal = self.terminal()?;
        let mut cached = self.drag.lock().map_err(error)?;
        let state = terminal.kitty_dnd.as_ref();
        if cached.view.as_ref().map(|view| view.revision) != state.map(dnd::State::revision) {
            cached.view = state.map(|state| {
                let mut view = state.view();
                if self.attached.is_some() {
                    // Missing owner metadata must not turn a registered drop into a paste.
                    view.ready = false;
                    view.accepted = None;
                }
                Arc::new(view)
            });
        }
        Ok(cached.view.clone())
    }

    /// Queues one guarded native drag event without synchronized input fanout.
    pub fn drag(&self, command: dnd::Command) -> io::Result<()> {
        if !self.accepts_input() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if let Some(attached) = &self.attached {
            return if attached.is_native() {
                attached.drag(command)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "dragging requires a local or native tmuxx session",
                ))
            };
        }
        // Reserve before committing the drop; a full input queue must leave it untouched.
        const REPLY_BUDGET: usize = 8 * 1024;
        self.pending_input.reserve(REPLY_BUDGET, false)?;
        let result = (|| {
            let mut terminal = self.terminal()?;
            if let dnd::Action::Move { event, .. } | dnd::Action::Drop { event, .. } =
                &command.action
                && (event.cell_x >= u32::from(terminal.cols)
                    || event.cell_y >= u32::from(terminal.rows))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "drag position is outside the pane",
                ));
            }
            let state = terminal
                .kitty_dnd
                .as_mut()
                .ok_or_else(|| error("drag target is no longer registered"))?;
            let mut next = state.clone();
            let bytes = next.apply_host(command).map_err(error)?;
            assert!(
                bytes.len() <= REPLY_BUDGET,
                "bounded drag report exceeded reserved input"
            );
            let used = bytes.len();
            if !bytes.is_empty() {
                self.input.send(IoCommand::Write(bytes)).map_err(error)?;
            }
            *state = next;
            Ok(used)
        })();
        let released = REPLY_BUDGET - result.as_ref().copied().unwrap_or(0);
        if self.pending_input.release(released) {
            (self.wake)();
        }
        (self.wake)();
        result.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> (Session, mpsc::Receiver<IoCommand>) {
        let (input, received) = mpsc::channel();
        let (_, events) = mpsc::channel();
        let mut terminal = Terminal::new(20, 4, 0);
        terminal.feed(b"\x1b]72;t=a:i=77;text/plain\x1b\\");
        (
            Session {
                wake: Arc::new(|| {}),
                shell: ShellInfo::new(vec!["test-shell".into()], "test".into()),
                attached: None,
                terminal: Arc::new(Mutex::new(terminal)),
                input,
                events,
                pending_input: Arc::new(PendingInput::default()),
                compression_pending: Arc::new(AtomicBool::new(false)),
                restored: Arc::new(Mutex::new(None)),
                exited: Arc::new(AtomicBool::new(false)),
                drag: Default::default(),
                close_child: None,
            },
            received,
        )
    }

    fn command(session: &Session, action: dnd::Action) -> dnd::Command {
        dnd::Command {
            registration: session.drag_view().unwrap().unwrap().registration,
            offer: 42,
            action,
        }
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

    fn start(session: &Session) {
        session
            .drag(command(
                session,
                dnd::Action::Move {
                    event: position(),
                    mimes: vec![b"text/plain".to_vec()],
                },
            ))
            .unwrap();
        session
            .terminal()
            .unwrap()
            .feed(b"\x1b]72;t=m:i=77:o=1;text/plain\x1b\\");
    }

    fn drop_command(session: &Session) -> dnd::Command {
        command(
            session,
            dnd::Action::Drop {
                event: position(),
                items: vec![dnd::Item {
                    mime: b"text/plain".to_vec(),
                    data: b"private drop".to_vec(),
                }],
            },
        )
    }

    #[test]
    fn local_drag_keeps_bytes_in_owner_and_reuses_unchanged_projection() {
        let (session, received) = session();
        let initial = session.drag_view().unwrap().unwrap();
        assert!(Arc::ptr_eq(
            &initial,
            &session.drag_view().unwrap().unwrap()
        ));
        let identity = session.drag_identity().unwrap();
        start(&session);
        session.drag(drop_command(&session)).unwrap();
        assert!(matches!(received.try_recv().unwrap(), IoCommand::Write(_)));
        assert!(matches!(received.try_recv().unwrap(), IoCommand::Write(_)));
        let view = session.drag_view().unwrap().unwrap();
        assert!(view.dropped);
        assert_eq!(view.offer, Some(42));
        assert_eq!(identity, session.drag_identity().unwrap());
        assert!(serde_json::to_value(&*view).unwrap().get("items").is_none());
        assert_eq!(
            session
                .terminal()
                .unwrap()
                .kitty_dnd
                .as_ref()
                .unwrap()
                .drop
                .items
                .as_ref()
                .unwrap()[0]
                .data,
            b"private drop"
        );
        session.drag(command(&session, dnd::Action::Leave)).unwrap();
        assert!(
            session
                .terminal()
                .unwrap()
                .kitty_dnd
                .as_ref()
                .unwrap()
                .drop
                .items
                .is_some()
        );
    }

    #[test]
    fn full_local_input_queue_does_not_commit_a_drop() {
        let (session, received) = session();
        start(&session);
        let IoCommand::Write(bytes) = received.try_recv().unwrap() else {
            panic!("move report")
        };
        session.pending_input.release(bytes.len());
        session
            .pending_input
            .reserve(MAX_PENDING_INPUT, false)
            .unwrap();
        let before = session.drag_view().unwrap().unwrap();
        assert_eq!(
            session.drag(drop_command(&session)).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(Arc::ptr_eq(&before, &session.drag_view().unwrap().unwrap()));
        assert!(
            session
                .terminal()
                .unwrap()
                .kitty_dnd
                .as_ref()
                .unwrap()
                .drop
                .items
                .is_none()
        );
        assert!(received.try_recv().is_err());
        session.pending_input.release(MAX_PENDING_INPUT);
        session.drag(drop_command(&session)).unwrap();
        assert!(session.drag_view().unwrap().unwrap().dropped);
    }

    #[test]
    fn closed_local_input_queue_does_not_commit_a_drop() {
        let (session, received) = session();
        start(&session);
        let IoCommand::Write(bytes) = received.try_recv().unwrap() else {
            panic!("move report")
        };
        session.pending_input.release(bytes.len());
        std::mem::drop(received);
        let before = session.drag_view().unwrap().unwrap();
        assert!(session.drag(drop_command(&session)).is_err());
        assert!(Arc::ptr_eq(&before, &session.drag_view().unwrap().unwrap()));
        assert_eq!(*session.pending_input.used.lock().unwrap(), Some(0));
        assert!(
            session
                .terminal()
                .unwrap()
                .kitty_dnd
                .as_ref()
                .unwrap()
                .drop
                .items
                .is_none()
        );
    }

    #[test]
    fn local_drag_rejects_stale_registration_and_outside_coordinates() {
        let (session, received) = session();
        let mut outside = command(
            &session,
            dnd::Action::Move {
                event: dnd::MoveEvent {
                    cell_x: 20,
                    ..position()
                },
                mimes: vec![],
            },
        );
        assert_eq!(
            session.drag(outside.clone()).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        outside.registration += 1;
        outside.action = dnd::Action::Move {
            event: position(),
            mimes: vec![],
        };
        assert!(session.drag(outside).is_err());
        assert!(received.try_recv().is_err());
        assert_eq!(session.drag_view().unwrap().unwrap().offer, None);
        assert_eq!(*session.pending_input.used.lock().unwrap(), Some(0));
    }
}
