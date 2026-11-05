use super::*;
use rustty_vt::notification::{Command, Reply};

// One full callback window: 128 Shows plus 32 maximum-size Alive replies.
const MAX_PENDING_REPLIES: usize = 1024 * 1024;

/// Queue ownership releases admission even when the writer or receiver stops.
pub(super) struct QueuedReply {
    pub(super) bytes: Vec<u8>,
    pending: Arc<PendingInput>,
}
impl Drop for QueuedReply {
    fn drop(&mut self) {
        self.pending.release(self.bytes.len());
    }
}

impl Session {
    /// Shared by views of one live owner; replacement or disconnect invalidates callbacks.
    pub fn notification_identity(&self) -> io::Result<u64> {
        if !self.accepts_input() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if let Some(attached) = &self.attached {
            return attached.notification_identity();
        }
        Ok(self.drag.lock().map_err(error)?.identity)
    }

    /// Reports a native notification result directly to its original owner.
    pub fn reply_notification(&self, command: &Command, reply: Reply) -> io::Result<()> {
        if !self.accepts_input() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if let Some(attached) = &self.attached {
            return attached.reply_notification(command.serial, reply);
        }
        let bytes = command.reply(&reply).map_err(error)?;
        if !bytes.is_empty() {
            self.notification_input
                .reserve_with_limit(bytes.len(), false, MAX_PENDING_REPLIES)?;
            self.input
                .send(IoCommand::Notification(QueuedReply {
                    bytes,
                    pending: self.notification_input.clone(),
                }))
                .map_err(error)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Session, mpsc::Receiver<IoCommand>, Command) {
        let (input, received) = mpsc::channel();
        let (_, events) = mpsc::channel();
        let mut terminal = Terminal::new(20, 4, 0);
        terminal.notification_capabilities = Some(rustty_vt::notification::Capabilities {
            buttons: true,
            close_events: true,
        });
        let command = terminal
            .feed(b"\x1b]99;i=job:a=report:c=1;Finished\x1b\\")
            .into_iter()
            .find_map(|effect| match effect {
                Effect::DesktopNotification(command) => Some(command),
                _ => None,
            })
            .unwrap();
        let session = Session {
            wake: Arc::new(|| {}),
            shell: ShellInfo::new(vec!["test".into()], "test".into()),
            attached: None,
            terminal: Arc::new(Mutex::new(terminal)),
            input,
            events,
            pending_input: Arc::new(PendingInput::default()),
            notification_input: Arc::new(PendingInput::default()),
            compression_pending: Arc::new(AtomicBool::new(false)),
            restored: Arc::new(Mutex::new(None)),
            exited: Arc::new(AtomicBool::new(false)),
            drag: Default::default(),
            close_child: None,
        };
        (session, received, command)
    }

    #[test]
    fn local_notification_replies_have_a_separate_bounded_raii_budget() {
        let (session, received, command) = fixture();
        let identity = session.notification_identity().unwrap();
        assert_eq!(identity, session.notification_identity().unwrap());
        assert!(
            session
                .reply_notification(&command, Reply::Activated { button: 1 })
                .is_err()
        );
        assert!(received.try_recv().is_err());
        session
            .reply_notification(&command, Reply::Activated { button: 0 })
            .unwrap();
        let expected = b"\x1b]99;i=job;\x1b\\\x1b]99;i=job:p=close;\x1b\\";
        assert!(
            matches!(received.try_recv().unwrap(), IoCommand::Notification(reply) if reply.bytes == expected)
        );
        assert_eq!(*session.notification_input.used.lock().unwrap(), Some(0));
        session
            .pending_input
            .reserve(MAX_PENDING_INPUT, false)
            .unwrap();
        session.reply_notification(&command, Reply::Closed).unwrap();
        let IoCommand::Notification(reply) = received.try_recv().unwrap() else {
            panic!("queued notification");
        };
        let mut output = Vec::new();
        write_pty(&mut output, &reply.bytes, false).unwrap();
        assert_eq!(output, command.reply(&Reply::Closed).unwrap());
        drop(reply);
        assert_eq!(*session.notification_input.used.lock().unwrap(), Some(0));
        session.pending_input.release(MAX_PENDING_INPUT);

        session
            .notification_input
            .reserve_with_limit(MAX_PENDING_REPLIES, false, MAX_PENDING_REPLIES)
            .unwrap();
        assert_eq!(
            session
                .reply_notification(&command, Reply::Closed)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(received.try_recv().is_err());
        session.notification_input.release(MAX_PENDING_REPLIES);
        // A receiver disappearing with queued replies releases every reservation.
        session.reply_notification(&command, Reply::Closed).unwrap();
        session
            .reply_notification(&command, Reply::Untracked)
            .unwrap();
        assert!(session.notification_input.used.lock().unwrap().unwrap() > 0);
        drop(received);
        assert_eq!(*session.notification_input.used.lock().unwrap(), Some(0));
        assert!(session.reply_notification(&command, Reply::Closed).is_err());
        assert_eq!(*session.notification_input.used.lock().unwrap(), Some(0));
        session.exited.store(true, Ordering::Release);
        assert!(session.notification_identity().is_err());
        assert!(session.reply_notification(&command, Reply::Closed).is_err());
    }

    #[test]
    fn reply_budget_holds_a_full_notification_window_while_regular_input_is_full() {
        let (session, received, mut show) = fixture();
        let rustty_vt::notification::Action::Show(message) = &mut show.action else {
            unreachable!();
        };
        message.identifier = Some("n".repeat(128));
        let query = Command {
            serial: 2,
            action: rustty_vt::notification::Action::Alive {
                identifier: "q".repeat(128),
            },
        };
        let alive = Reply::Alive {
            identifiers: vec!["n".repeat(128); 128],
        };
        session
            .pending_input
            .reserve(MAX_PENDING_INPUT, false)
            .unwrap();
        for _ in 0..128 {
            session.reply_notification(&show, Reply::Untracked).unwrap();
            session
                .reply_notification(&show, Reply::Activated { button: 0 })
                .unwrap();
        }
        for _ in 0..32 {
            session.reply_notification(&query, alive.clone()).unwrap();
        }
        let used = session.notification_input.used.lock().unwrap().unwrap();
        assert!((512 * 1024..MAX_PENDING_REPLIES).contains(&used));
        drop(received);
        assert_eq!(*session.notification_input.used.lock().unwrap(), Some(0));
        session.pending_input.release(MAX_PENDING_INPUT);
    }

    #[test]
    fn notification_writer_errors_and_skipped_commands_release_reservations() {
        struct Failing;
        impl Write for Failing {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let pending = Arc::new(PendingInput::default());
        pending
            .reserve_with_limit(10, false, MAX_PENDING_REPLIES)
            .unwrap();
        let (input, received) = mpsc::channel();
        input
            .send(IoCommand::Notification(QueuedReply {
                bytes: vec![b'x'; 5],
                pending: pending.clone(),
            }))
            .unwrap();
        input
            .send(IoCommand::Notification(QueuedReply {
                bytes: vec![b'y'; 5],
                pending: pending.clone(),
            }))
            .unwrap();
        let IoCommand::Notification(reply) = received.recv().unwrap() else {
            panic!("notification");
        };
        assert!(write_pty(&mut Failing, &reply.bytes, false).is_err());
        drop(reply);
        assert_eq!(*pending.used.lock().unwrap(), Some(5));
        drop(received);
        assert_eq!(*pending.used.lock().unwrap(), Some(0));
    }
}
