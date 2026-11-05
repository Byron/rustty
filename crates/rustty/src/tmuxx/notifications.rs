use super::*;
use rustty_vt::notification::{Action, Command, Reply};

const MAX_DEFERRED_BYTES: usize = 1024 * 1024;
const MAX_DEFERRED_REQUESTS: usize = 1024;

/// Lives with the connection so accepted replies survive their last view.
#[derive(Default)]
pub(super) struct DeferredRequests {
    queue: std::collections::VecDeque<Request>,
    bytes: usize,
}

fn request_bytes(request: &Request) -> usize {
    request
        .argv
        .iter()
        .fold(64usize, |bytes, arg| bytes.saturating_add(arg.len()))
        .saturating_add(request.stdin.as_ref().map_or(0, Vec::len))
}

/// Ordinary producers retry rather than taking slots ahead of deferred replies.
pub(super) fn try_request(shared: &Shared, request: Request) -> io::Result<()> {
    let pending = shared.deferred_requests.lock().map_err(err)?;
    if shared.closed.load(Ordering::Acquire) {
        return Err(io::ErrorKind::BrokenPipe.into());
    }
    if !pending.queue.is_empty() {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    shared
        .requests
        .try_send(request)
        .map_err(|error| match error {
            mpsc::TrySendError::Full(_) => io::ErrorKind::WouldBlock.into(),
            mpsc::TrySendError::Disconnected(_) => io::ErrorKind::BrokenPipe.into(),
        })
}

/// Only notification replies and their view lifecycle use this overflow budget.
/// Once deferred, later entries join the same FIFO even if a channel slot opens.
pub(super) fn queue_request(shared: &Shared, request: Request) -> io::Result<()> {
    let mut pending = shared.deferred_requests.lock().map_err(err)?;
    if shared.closed.load(Ordering::Acquire) {
        return Err(io::ErrorKind::BrokenPipe.into());
    }
    let request = if pending.queue.is_empty() {
        match shared.requests.try_send(request) {
            Ok(()) => return Ok(()),
            Err(mpsc::TrySendError::Full(request)) => request,
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
        }
    } else {
        request
    };
    let bytes = request_bytes(&request);
    if pending.queue.len() == MAX_DEFERRED_REQUESTS
        || bytes > MAX_DEFERRED_BYTES.saturating_sub(pending.bytes)
    {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "notification reply queue is full",
        ));
    }
    pending.bytes += bytes;
    pending.queue.push_back(request);
    Ok(())
}

pub(super) fn lifecycle(shared: &Shared, argv: Vec<Vec<u8>>) -> io::Result<()> {
    queue_request(
        shared,
        Request {
            request_id: shared.next.fetch_add(1, Ordering::Relaxed),
            argv,
            stdin: None,
            subscribe: false,
            control: false,
        },
    )
}

/// Called by both existing request writers as they drain the ordinary channel.
pub(super) fn flush_requests(shared: &Shared) -> io::Result<()> {
    let mut pending = shared.deferred_requests.lock().map_err(err)?;
    if shared.closed.load(Ordering::Acquire) {
        *pending = DeferredRequests::default();
        return Ok(());
    }
    while let Some(request) = pending.queue.pop_front() {
        let bytes = request_bytes(&request);
        match shared.requests.try_send(request) {
            Ok(()) => pending.bytes -= bytes,
            Err(mpsc::TrySendError::Full(request)) => {
                pending.queue.push_front(request);
                break;
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                *pending = DeferredRequests::default();
                return Err(io::ErrorKind::BrokenPipe.into());
            }
        }
    }
    Ok(())
}

pub(super) fn clear_requests(shared: &Shared) {
    if let Ok(mut pending) = shared.deferred_requests.lock() {
        *pending = DeferredRequests::default();
    }
}

impl PaneInput {
    pub(crate) fn notification_identity(&self) -> io::Result<u64> {
        if !self.native {
            return Err(io::ErrorKind::Unsupported.into());
        }
        let shared = self.connection()?;
        let model = shared.model.lock().map_err(err)?;
        let pane = model
            .panes
            .get(&self.pane)
            .filter(|pane| Arc::ptr_eq(&pane.owners, &self.owners))
            .ok_or(io::ErrorKind::BrokenPipe)?;
        Ok(pane.notification_identity)
    }

    pub(crate) fn reply_notification(&self, serial: u64, reply: Reply) -> io::Result<()> {
        if !self.native {
            return Err(io::ErrorKind::Unsupported.into());
        }
        let shared = self.connection()?;
        let mut model = shared.model.lock().map_err(err)?;
        let pane = model
            .panes
            .get_mut(&self.pane)
            .filter(|pane| Arc::ptr_eq(&pane.owners, &self.owners))
            .ok_or(io::ErrorKind::BrokenPipe)?;
        let Some(pending) = pane.notifications.get(&serial) else {
            return if matches!(reply, Reply::Finished) {
                Ok(())
            } else {
                Err(err("notification has expired"))
            };
        };
        pending.command.reply(&reply).map_err(err)?;
        let keep = matches!(reply, Reply::Untracked);
        if keep && pending.untracked {
            return Ok(());
        }
        host_reply(&shared, pending.event_id, HostResponse::Notification(reply))?;
        if keep {
            pane.notifications.get_mut(&serial).unwrap().untracked = true;
        } else {
            pane.notifications.remove(&serial);
        }
        Ok(())
    }
}

pub(super) fn deliver(
    shared: &Shared,
    model: &mut Model,
    pane_id: u64,
    event_id: u64,
    command: Command,
) -> io::Result<()> {
    command.validate().map_err(err)?;
    let mut delivered = false;
    if let Some(pane) = model.panes.get_mut(&pane_id) {
        if let Action::Show(message) = &command.action
            && let Some(identifier) = &message.identifier
        {
            pane.notifications.retain(|_, pending| {
                !matches!(&pending.command.action, Action::Show(old) if old.identifier.as_ref() == Some(identifier))
            });
        }
        pane.views.retain(|view| view.terminal.strong_count() > 0);
        if pane.notifications.len() < 161
            && let Some(view) = pane
                .views
                .iter()
                .find(|view| view.focused.load(Ordering::Acquire))
                .or_else(|| pane.views.first())
            && view
                .events
                .try_send(SessionEvent::DesktopNotificationDelivery(command.clone()))
                .is_ok()
        {
            pane.notifications.insert(
                command.serial,
                PendingNotification {
                    event_id,
                    command,
                    untracked: false,
                },
            );
            (view.wake)();
            delivered = true;
        }
    }
    if !delivered {
        host_reply(shared, event_id, HostResponse::Notification(Reply::Failed))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(bytes: usize) -> Request {
        Request {
            request_id: 1,
            argv: vec![b"test".to_vec()],
            stdin: Some(vec![0; bytes]),
            subscribe: false,
            control: false,
        }
    }

    fn fill(shared: &Shared) {
        for _ in 0..256 {
            shared.requests.try_send(request(0)).unwrap();
        }
    }

    #[test]
    fn deferred_notification_window_fits_and_keeps_fifo_when_channel_has_room() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        let shared = &connection.shared;
        fill(shared);
        for id in 0..128 {
            host_reply(shared, id, HostResponse::Notification(Reply::Untracked)).unwrap();
            host_reply(
                shared,
                id,
                HostResponse::Notification(Reply::Activated { button: 0 }),
            )
            .unwrap();
        }
        let identifiers = (0..128)
            .map(|index| format!("{index:0>128}"))
            .collect::<Vec<_>>();
        for id in 128..160 {
            host_reply(
                shared,
                id,
                HostResponse::Notification(Reply::Alive {
                    identifiers: identifiers.clone(),
                }),
            )
            .unwrap();
        }
        {
            let pending = shared.deferred_requests.lock().unwrap();
            assert_eq!(pending.queue.len(), 288);
            assert!(pending.bytes > 512 * 1024 && pending.bytes < MAX_DEFERRED_BYTES);
        }
        assert_eq!(requests.try_iter().count(), 256);
        // Later lifecycle commands must not jump past accepted replies now that
        // the ordinary channel has room but the overflow queue still exists.
        lifecycle(shared, vec![b"tmuxx-release-pane".to_vec()]).unwrap();
        assert!(requests.try_recv().is_err());
        flush_requests(shared).unwrap();
        let mut sent = requests.try_iter().collect::<Vec<_>>();
        assert_eq!(sent.len(), 256);
        flush_requests(shared).unwrap();
        sent.extend(requests.try_iter());
        assert_eq!(sent.len(), 289);
        for (index, request) in sent[..288].iter().enumerate() {
            let event_id = if index < 256 { index / 2 } else { index - 128 };
            assert_eq!(request.argv[1], event_id.to_string().as_bytes());
        }
        assert_eq!(sent.last().unwrap().argv[0], b"tmuxx-release-pane");
        let pending = shared.deferred_requests.lock().unwrap();
        assert!(pending.queue.is_empty());
        assert_eq!(pending.bytes, 0);
    }

    #[test]
    fn deferred_notification_queue_bounds_and_disconnection_release_storage() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        let shared = &connection.shared;
        fill(shared);
        assert_eq!(
            queue_request(shared, request(MAX_DEFERRED_BYTES))
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(shared.deferred_requests.lock().unwrap().bytes, 0);
        for _ in 0..MAX_DEFERRED_REQUESTS {
            queue_request(shared, request(0)).unwrap();
        }
        assert_eq!(
            queue_request(shared, request(0)).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(requests);
        assert_eq!(
            flush_requests(shared).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        let pending = shared.deferred_requests.lock().unwrap();
        assert!(pending.queue.is_empty());
        assert_eq!(pending.bytes, 0);
        drop(pending);
        assert_eq!(
            queue_request(shared, request(0)).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn connection_failure_discards_deferred_notification_requests() {
        let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
        fill(&connection.shared);
        queue_request(&connection.shared, request(100)).unwrap();
        fail(&connection.shared, "owner disconnected");
        let pending = connection.shared.deferred_requests.lock().unwrap();
        assert!(pending.queue.is_empty());
        assert_eq!(pending.bytes, 0);
    }
}
