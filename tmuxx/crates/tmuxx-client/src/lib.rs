//! A control client driven by caller-supplied bytes; this crate does no I/O.
//!
//! Pane bytes and command output are streamed to the caller, never retained in
//! the model. Discovery is explicit because notifications are not full snapshots.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use tmuxx_command::Command;
use tmuxx_protocol::{DecodeError, Decoder, Event, Notification};

pub mod discovery;
pub mod system;
pub use discovery::{Discovery, DiscoveryKind, DiscoverySnapshot};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Session {
    pub id: u64,
    pub name: Option<Vec<u8>>,
    /// Window index -> ID; one window can be linked at multiple indices.
    pub windows: BTreeMap<u32, u64>,
    pub active_window: Option<u64>,
    pub active_index: Option<u32>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Window {
    pub id: u64,
    pub name: Option<Vec<u8>>,
    pub layout: Vec<u8>,
    pub visible_layout: Vec<u8>,
    pub flags: Vec<u8>,
    pub panes: BTreeSet<u64>,
    pub active_pane: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Pane {
    pub id: u64,
    pub window_id: u64,
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub modal: bool,
    pub paused: bool,
    /// Pause, reconnect, and discovery do not provide a replay cursor.
    pub needs_snapshot: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct State {
    pub sessions: BTreeMap<u64, Session>,
    pub windows: BTreeMap<u64, Window>,
    pub panes: BTreeMap<u64, Pane>,
    pub attached_session: Option<u64>,
    pub discovery_needed: bool,
    pub disconnected: bool,
    revision: u64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            sessions: BTreeMap::new(),
            windows: BTreeMap::new(),
            panes: BTreeMap::new(),
            attached_session: None,
            discovery_needed: true,
            disconnected: false,
            revision: 0,
        }
    }
}

impl State {
    /// Save before discovery. Structural notifications invalidate that attempt.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn reconcile(
        &mut self,
        expected_revision: u64,
        mut snapshot: DiscoverySnapshot,
    ) -> Result<(), Error> {
        if self.disconnected {
            return Err(Error::Disconnected);
        }
        if self.revision != expected_revision {
            return Err(Error::StaleDiscovery);
        }
        snapshot.validate()?;
        for (&id, session) in &mut snapshot.sessions {
            session.name = self.sessions.get(&id).and_then(|s| s.name.clone());
        }
        for (&id, window) in &mut snapshot.windows {
            window.name = self.windows.get(&id).and_then(|w| w.name.clone());
        }
        self.sessions = snapshot.sessions;
        self.windows = snapshot.windows;
        self.panes = snapshot.panes;
        if self
            .attached_session
            .is_some_and(|id| !self.sessions.contains_key(&id))
        {
            self.attached_session = None;
        }
        self.discovery_needed = false;
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Only call after the consumer has established its snapshot/live boundary.
    pub fn mark_pane_synchronized(&mut self, pane_id: u64) -> Result<(), Error> {
        let pane = self.panes.get_mut(&pane_id).ok_or(Error::UnknownObject)?;
        pane.needs_snapshot = false;
        Ok(())
    }

    /// Name results are separate, one-object replies so embedded newlines remain data.
    pub fn set_name(&mut self, object: Object, name: Vec<u8>) -> Result<(), Error> {
        match object {
            Object::Session(id) => {
                self.sessions.get_mut(&id).ok_or(Error::UnknownObject)?.name = Some(name)
            }
            Object::Window(id) => {
                self.windows.get_mut(&id).ok_or(Error::UnknownObject)?.name = Some(name)
            }
        }
        Ok(())
    }

    fn notify(&mut self, notification: &Notification) {
        use Notification::*;
        match notification {
            Output { pane_id, .. } => {
                if !self.panes.contains_key(pane_id) {
                    self.discovery_needed = true;
                }
                return;
            }
            Message(_)
            | ConfigError(_)
            | PasteBufferChanged { .. }
            | PasteBufferDeleted { .. }
            | SubscriptionChanged { .. }
            | ClientDetached { .. }
            | ClientSessionChanged { .. } => return,
            SessionChanged { session_id, name } => {
                self.attached_session = Some(*session_id);
                self.sessions
                    .entry(*session_id)
                    .or_insert_with(|| Session {
                        id: *session_id,
                        ..Session::default()
                    })
                    .name = Some(name.clone());
                self.discovery_needed = true;
                for pane in self.panes.values_mut() {
                    pane.needs_snapshot = true;
                }
            }
            SessionRenamed { session_id, name } => {
                if let Some(id) = session_id.or(self.attached_session) {
                    if let Some(session) = self.sessions.get_mut(&id) {
                        session.name = Some(name.clone());
                    } else {
                        self.discovery_needed = true;
                    }
                }
            }
            SessionWindowChanged {
                session_id,
                window_id,
            } => {
                if let Some(session) = self.sessions.get_mut(session_id) {
                    session.active_window = Some(*window_id);
                    let links = session
                        .windows
                        .iter()
                        .filter(|(_, id)| **id == *window_id)
                        .map(|(index, _)| *index)
                        .collect::<Vec<_>>();
                    session.active_index = (links.len() == 1).then(|| links[0]);
                    if session.active_index.is_none() {
                        self.discovery_needed = true;
                    }
                } else {
                    self.discovery_needed = true;
                }
            }
            LayoutChange {
                window_id,
                layout,
                visible_layout,
                flags,
            } => {
                if let Some(window) = self.windows.get_mut(window_id) {
                    window.layout.clone_from(layout);
                    window.visible_layout.clone_from(visible_layout);
                    window.flags.clone_from(flags);
                }
                // Pane creation/deletion and geometry are not all separate notifications.
                self.discovery_needed = true;
            }
            WindowRenamed {
                window_id, name, ..
            } => {
                if let Some(window) = self.windows.get_mut(window_id) {
                    window.name = Some(name.clone());
                } else {
                    self.discovery_needed = true;
                }
            }
            WindowPaneChanged { window_id, pane_id } => {
                if let Some(window) = self.windows.get_mut(window_id) {
                    window.active_pane = Some(*pane_id);
                    if !window.panes.contains(pane_id) {
                        self.discovery_needed = true;
                    }
                } else {
                    self.discovery_needed = true;
                }
            }
            WindowClose {
                window_id,
                linked: true,
            } => {
                if let Some(session) = self
                    .attached_session
                    .and_then(|id| self.sessions.get_mut(&id))
                {
                    // The notification omits the index, so duplicate links need discovery.
                    if session
                        .windows
                        .values()
                        .filter(|id| **id == *window_id)
                        .count()
                        <= 1
                    {
                        session.windows.retain(|_, id| *id != *window_id);
                    }
                    if session.active_window == Some(*window_id) {
                        session.active_window = None;
                        session.active_index = None;
                    }
                }
                if !self
                    .sessions
                    .values()
                    .any(|s| s.windows.values().any(|id| id == window_id))
                {
                    self.windows.remove(window_id);
                    self.panes.retain(|_, pane| pane.window_id != *window_id);
                }
                self.discovery_needed = true;
            }
            Pause { pane_id } | Continue { pane_id } => {
                if let Some(pane) = self.panes.get_mut(pane_id) {
                    pane.paused = matches!(notification, Pause { .. });
                    pane.needs_snapshot = true;
                } else {
                    self.discovery_needed = true;
                }
            }
            PaneModeChanged { pane_id } => {
                if let Some(pane) = self.panes.get_mut(pane_id) {
                    pane.needs_snapshot = true;
                }
                self.discovery_needed = true;
            }
            Exit { .. } => self.disconnect(),
            SessionsChanged | WindowAdd { .. } | WindowClose { .. } | Unknown(_) => {
                self.discovery_needed = true
            }
        }
        self.revision = self.revision.wrapping_add(1);
    }

    fn disconnect(&mut self) {
        self.disconnected = true;
        self.discovery_needed = true;
        for pane in self.panes.values_mut() {
            pane.needs_snapshot = true;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Object {
    Session(u64),
    Window(u64),
}

pub fn name_command(object: Object) -> Command {
    let (target, format) = match object {
        Object::Session(id) => (format!("${id}"), "#{session_name}"),
        Object::Window(id) => (format!("@{id}"), "#{window_name}"),
    };
    Command {
        blocks: Default::default(),
        args: vec![
            b"display-message".to_vec(),
            b"-p".to_vec(),
            b"-t".to_vec(),
            target.into_bytes(),
            format.as_bytes().to_vec(),
        ],
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientEvent {
    Protocol(Event),
    /// All preceding commands reached the barrier; inspect their individual results.
    BatchFinished {
        request_id: u64,
    },
    /// Connection lost before a barrier: effects may already have occurred.
    Indeterminate {
        request_id: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    Protocol(DecodeError),
    InvalidCommand,
    InvalidBarrier,
    DuplicateRequest,
    PendingLimit,
    Disconnected,
    InvalidDiscovery,
    DiscoveryLimit,
    ReplyLimit,
    StaleDiscovery,
    UnknownObject,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => error.fmt(f),
            Self::InvalidCommand => f.write_str("empty command or NUL in a command argument"),
            Self::InvalidBarrier => f.write_str(
                "barrier must be a unique alphanumeric token (._:- allowed), not starting with '-'",
            ),
            Self::DuplicateRequest => f.write_str("request ID is already pending"),
            Self::PendingLimit => f.write_str("too many pending requests"),
            Self::Disconnected => f.write_str("client is disconnected; create a fresh client"),
            Self::InvalidDiscovery => f.write_str("inconsistent or malformed discovery snapshot"),
            Self::ReplyLimit => f.write_str("command reply exceeds client limit"),
            Self::DiscoveryLimit => f.write_str("discovery object limit exceeded"),
            Self::StaleDiscovery => f.write_str("a structural event invalidated discovery"),
            Self::UnknownObject => f.write_str("object no longer exists"),
        }
    }
}

impl std::error::Error for Error {}
impl From<DecodeError> for Error {
    fn from(error: DecodeError) -> Self {
        Self::Protocol(error)
    }
}

struct Pending {
    request_id: u64,
    barrier: Vec<u8>,
}

#[derive(Default)]
struct ReplyMarker {
    lines: usize,
    barrier: bool,
}

pub struct Client {
    decoder: Decoder,
    pub state: State,
    pending: VecDeque<Pending>,
    pending_limit: usize,
    reply: ReplyMarker,
}

impl Default for Client {
    fn default() -> Self {
        Self::with_limits(8 * 1024 * 1024, 128)
    }
}

impl Client {
    pub fn with_limits(max_record_bytes: usize, max_pending_requests: usize) -> Self {
        Self {
            decoder: Decoder::with_limit(max_record_bytes),
            state: State::default(),
            pending: VecDeque::new(),
            pending_limit: max_pending_requests,
            reply: ReplyMarker::default(),
        }
    }

    /// Returns bytes for the caller to write. This does not perform I/O or retry.
    ///
    /// `barrier` must be an unpredictable ASCII alphanumeric token (._:- allowed),
    /// distinct from command output and not starting with '-'. This prevents
    /// tmux option/format expansion. It is not an authentication token.
    /// The barrier is a separate command group so command errors cannot remove it.
    pub fn submit(
        &mut self,
        request_id: u64,
        commands: &[Command],
        barrier: &[u8],
    ) -> Result<Vec<u8>, Error> {
        if self.state.disconnected {
            return Err(Error::Disconnected);
        }
        if self.pending.len() >= self.pending_limit {
            return Err(Error::PendingLimit);
        }
        if self.pending.iter().any(|p| p.request_id == request_id) {
            return Err(Error::DuplicateRequest);
        }
        if commands.is_empty()
            || commands.iter().any(|c| {
                c.args.is_empty()
                    || c.args[0].is_empty()
                    || c.args.iter().any(|arg| arg.contains(&0))
            })
        {
            return Err(Error::InvalidCommand);
        }
        if barrier.is_empty()
            || barrier.starts_with(b"-")
            || !barrier
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(b))
            || self.pending.iter().any(|p| p.barrier == barrier)
        {
            return Err(Error::InvalidBarrier);
        }
        let mut bytes = tmuxx_command::serialize(commands);
        bytes.push(b'\n');
        bytes.extend(tmuxx_command::serialize(&[Command {
            blocks: Default::default(),
            args: vec![
                b"display-message".to_vec(),
                b"-p".to_vec(),
                barrier.to_vec(),
            ],
        }]));
        bytes.push(b'\n');
        self.pending.push_back(Pending {
            request_id,
            barrier: barrier.to_vec(),
        });
        Ok(bytes)
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<ClientEvent>, Error> {
        let mut events = Vec::new();
        self.feed_into(bytes, |event| events.push(event))?;
        Ok(events)
    }

    pub fn feed_into(
        &mut self,
        bytes: &[u8],
        mut emit: impl FnMut(ClientEvent),
    ) -> Result<(), Error> {
        if self.state.disconnected {
            return Err(Error::Disconnected);
        }
        let Self {
            decoder,
            state,
            pending,
            reply,
            ..
        } = self;
        let result = decoder.feed_into(bytes, |event| {
            match &event {
                Event::Begin(_) => *reply = ReplyMarker::default(),
                Event::CommandOutput(line) => {
                    reply.lines = reply.lines.saturating_add(1);
                    reply.barrier =
                        reply.lines == 1 && pending.front().is_some_and(|p| p.barrier == *line);
                }
                Event::End { failed, .. } => {
                    if !failed && reply.barrier {
                        let done = pending
                            .pop_front()
                            .expect("marker requires a pending request");
                        emit(ClientEvent::Protocol(event));
                        emit(ClientEvent::BatchFinished {
                            request_id: done.request_id,
                        });
                        *reply = ReplyMarker::default();
                        return;
                    }
                    *reply = ReplyMarker::default();
                }
                Event::Notification(notification) => state.notify(notification),
                Event::UnframedOutput(_) => {}
            }
            let exiting = matches!(event, Event::Notification(Notification::Exit { .. }));
            emit(ClientEvent::Protocol(event));
            if exiting {
                for request in pending.drain(..) {
                    emit(ClientEvent::Indeterminate {
                        request_id: request.request_id,
                    });
                }
            }
        });
        if let Err(error) = result {
            state.disconnect();
            for request in pending.drain(..) {
                emit(ClientEvent::Indeterminate {
                    request_id: request.request_id,
                });
            }
            return Err(error.into());
        }
        Ok(())
    }

    /// Ends a transport, validates EOF, and exposes unresolved requests without retrying.
    pub fn finish(&mut self, mut emit: impl FnMut(ClientEvent)) -> Result<(), Error> {
        let result = self.decoder.finish();
        self.state.disconnect();
        for request in self.pending.drain(..) {
            emit(ClientEvent::Indeterminate {
                request_id: request.request_id,
            });
        }
        result.map_err(Error::from)
    }
}
