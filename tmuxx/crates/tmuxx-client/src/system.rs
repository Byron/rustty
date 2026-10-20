//! A transport-independent adapter for stock `tmux -C` streams.
//!
//! Capture bootstrap is best effort: stock tmux cannot export an atomic terminal
//! snapshot and replay cursor. Only the attached session receives live output.
use crate::{Client, ClientEvent, Discovery, DiscoveryKind, Error, Object, State, name_command};
use std::collections::{BTreeSet, VecDeque};
use tmuxx_command::Command;
use tmuxx_protocol::{Event, Notification};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SystemEvent {
    State(State),
    PaneOutput {
        pane_id: u64,
        data: Vec<u8>,
    },
    /// Server output outside command guards has no request identity.
    UnframedOutput {
        data: Vec<u8>,
    },
    /// Visible rows with SGR escapes; not complete terminal state or scrollback.
    PaneCapture {
        pane_id: u64,
        data: Vec<u8>,
        cols: u32,
        rows: u32,
    },
    CommandFinished {
        request_id: u64,
        output: Vec<u8>,
        failed: bool,
    },
    Indeterminate {
        request_id: u64,
    },
    Cancelled {
        request_id: u64,
    },
    /// Structural churn exhausted the bounded automatic discovery attempts.
    /// The transport and queued user commands remain valid. A consumer may
    /// explicitly resume read-only bootstrap after a bounded quiet interval.
    DiscoveryDeferred,
    Error(String),
    Exit {
        reason: Option<Vec<u8>>,
    },
}

#[derive(Clone, Copy)]
enum Kind {
    Discover(DiscoveryKind),
    Name(Object),
    Capture(u64),
    User,
}
struct Job {
    id: u64,
    kind: Kind,
    commands: Vec<Command>,
    marker: Vec<u8>,
    replies: Vec<(Vec<Vec<u8>>, bool)>,
    bytes: usize,
    revision: u64,
}
struct Scan {
    revision: u64,
    discovery: Discovery,
}

pub struct SystemClient {
    client: Client,
    nonce: Vec<u8>,
    next_id: u64,
    active: Option<Job>,
    users: VecDeque<Job>,
    internal: VecDeque<Kind>,
    writes: Vec<Vec<u8>>,
    reply: Option<Vec<Vec<u8>>>,
    scan: Option<Scan>,
    bootstrap_requested: bool,
    stale_attempts: u8,
    captured: BTreeSet<u64>,
    max_reply_bytes: usize,
}

impl SystemClient {
    /// `nonce` must be a fresh unpredictable ASCII token for this connection.
    pub fn new(nonce: Vec<u8>) -> Result<Self, Error> {
        if nonce.is_empty()
            || nonce.len() > 128
            || nonce.starts_with(b"-")
            || !nonce
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(b))
        {
            return Err(Error::InvalidBarrier);
        }
        Ok(Self {
            client: Client::default(),
            nonce,
            next_id: 0,
            active: None,
            users: VecDeque::new(),
            internal: VecDeque::new(),
            writes: Vec::new(),
            reply: None,
            scan: None,
            bootstrap_requested: false,
            stale_attempts: 0,
            captured: BTreeSet::new(),
            max_reply_bytes: 16 * 1024 * 1024,
        })
    }
    pub fn state(&self) -> &State {
        &self.client.state
    }
    pub fn discovery_pending(&self) -> bool {
        self.bootstrap_requested
            || self.scan.is_some()
            || !self.internal.is_empty()
            || self.stale_attempts >= 3
            || self
                .active
                .as_ref()
                .is_some_and(|job| !matches!(job.kind, Kind::User))
    }
    pub fn take_writes(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.writes)
    }
    pub fn bootstrap(&mut self) -> Result<(), Error> {
        if self.client.state.disconnected {
            return Err(Error::Disconnected);
        }
        self.bootstrap_requested = true;
        self.stale_attempts = 0;
        self.pump()
    }
    /// Queue one command group. Errors cancel the rest of this group on tmux.
    pub fn queue(&mut self, commands: Vec<Command>) -> Result<u64, Error> {
        if self.client.state.disconnected {
            return Err(Error::Disconnected);
        }
        if self.users.len() >= 128 {
            return Err(Error::PendingLimit);
        }
        if commands.is_empty()
            || commands.iter().any(|c| {
                c.args.is_empty() || c.args[0].is_empty() || c.args.iter().any(|a| a.contains(&0))
            })
        {
            return Err(Error::InvalidCommand);
        }
        let job = self.job(Kind::User, commands);
        let id = job.id;
        self.users.push_back(job);
        self.pump()?;
        Ok(id)
    }
    /// Feed arbitrary byte fragments. Errors and partial results are delivered in order.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SystemEvent> {
        let before = self.client.state.revision();
        let mut events = Vec::new();
        let result = self.client.feed_into(bytes, |event| events.push(event));
        let mut output = Vec::new();
        for event in events {
            if let Err(error) = self.process(event, &mut output) {
                output.push(SystemEvent::Error(error.to_string()));
                output.extend(self.finish());
                return output;
            }
        }
        if let Err(error) = result {
            output.push(SystemEvent::Error(error.to_string()));
        }
        if self.client.state.revision() != before {
            output.push(SystemEvent::State(self.client.state.clone()));
            if self.client.state.discovery_needed && self.scan.is_none() && self.stale_attempts < 3
            {
                self.bootstrap_requested = true;
            }
        }
        if !self.client.state.disconnected {
            if let Err(error) = self.pump() {
                output.push(SystemEvent::Error(error.to_string()));
            }
        } else {
            self.cancel_queued(&mut output);
        }
        output
    }
    /// End the transport. A sent command without a barrier remains indeterminate.
    pub fn finish(&mut self) -> Vec<SystemEvent> {
        let mut events = Vec::new();
        let result = self.client.finish(|event| events.push(event));
        let mut output = Vec::new();
        for event in events {
            let _ = self.process(event, &mut output);
        }
        if let Err(error) = result {
            output.push(SystemEvent::Error(error.to_string()));
        }
        self.cancel_queued(&mut output);
        output.push(SystemEvent::State(self.client.state.clone()));
        output
    }
    fn cancel_queued(&mut self, output: &mut Vec<SystemEvent>) {
        for job in self.users.drain(..) {
            output.push(SystemEvent::Cancelled { request_id: job.id });
        }
        self.internal.clear();
        self.writes.clear();
        self.scan = None;
    }
    fn job(&mut self, kind: Kind, commands: Vec<Command>) -> Job {
        self.next_id += 1;
        let mut marker = self.nonce.clone();
        marker.extend_from_slice(format!("-{}", self.next_id).as_bytes());
        Job {
            id: self.next_id,
            kind,
            commands,
            marker,
            replies: Vec::new(),
            bytes: 0,
            revision: self.client.state.revision(),
        }
    }
    fn pump(&mut self) -> Result<(), Error> {
        if self.active.is_some() {
            return Ok(());
        }
        if self.bootstrap_requested {
            self.bootstrap_requested = false;
            self.internal.clear();
            self.captured.clear();
            self.scan = Some(Scan {
                revision: self.client.state.revision(),
                discovery: Discovery::default(),
            });
            self.internal.extend([
                Kind::Discover(DiscoveryKind::Sessions),
                Kind::Discover(DiscoveryKind::Windows),
                Kind::Discover(DiscoveryKind::Panes),
            ]);
        }
        let job = if let Some(kind) = self.internal.pop_front() {
            let commands = match kind {
                Kind::Discover(kind) => vec![kind.command()],
                Kind::Name(object) => vec![name_command(object)],
                Kind::Capture(id) => vec![
                    Command { blocks: Default::default(), args: vec![b"capture-pane".to_vec(), b"-e".to_vec(), b"-p".to_vec(), b"-t".to_vec(), format!("%{id}").into_bytes()] },
                    Command { blocks: Default::default(), args: vec![b"display-message".to_vec(), b"-p".to_vec(), b"-t".to_vec(), format!("%{id}").into_bytes(), b"#{cursor_x}|#{cursor_y}|#{cursor_flag}|#{alternate_on}|#{keypad_cursor_flag}".to_vec()] },
                ],
                Kind::User => unreachable!(),
            };
            self.job(kind, commands)
        } else if let Some(job) = self.users.pop_front() {
            job
        } else {
            return Ok(());
        };
        self.writes
            .push(self.client.submit(job.id, &job.commands, &job.marker)?);
        self.active = Some(job);
        Ok(())
    }
    fn process(&mut self, event: ClientEvent, output: &mut Vec<SystemEvent>) -> Result<(), Error> {
        match event {
            ClientEvent::Protocol(Event::Begin(guard)) => {
                // Initial attach/config guards have flags zero and precede our requests.
                self.reply = (guard.flags & 1 != 0).then(Vec::new);
            }
            ClientEvent::Protocol(Event::CommandOutput(line)) => {
                if let (Some(reply), Some(job)) = (&mut self.reply, &mut self.active) {
                    job.bytes = job.bytes.saturating_add(line.len() + 1);
                    if job.bytes > self.max_reply_bytes {
                        return Err(Error::ReplyLimit);
                    }
                    reply.push(line);
                }
            }
            ClientEvent::Protocol(Event::UnframedOutput(data)) => {
                output.push(SystemEvent::UnframedOutput { data })
            }
            ClientEvent::Protocol(Event::End { failed, .. }) => {
                if let (Some(reply), Some(job)) = (self.reply.take(), &mut self.active)
                    && (failed || reply.len() != 1 || reply[0] != job.marker)
                {
                    job.replies.push((reply, failed));
                }
            }
            ClientEvent::Protocol(Event::Notification(notification)) => match notification {
                Notification::Output { pane_id, data, .. } if self.captured.contains(&pane_id) => {
                    output.push(SystemEvent::PaneOutput { pane_id, data })
                }
                Notification::Pause { pane_id } | Notification::Continue { pane_id } => {
                    self.captured.remove(&pane_id);
                    self.bootstrap_requested = true;
                }
                Notification::Exit { reason } => output.push(SystemEvent::Exit { reason }),
                _ => {}
            },
            ClientEvent::BatchFinished { request_id } => {
                let job = self.active.take().ok_or(Error::InvalidDiscovery)?;
                if job.id != request_id {
                    return Err(Error::InvalidDiscovery);
                }
                self.complete(job, output)?;
            }
            ClientEvent::Indeterminate { request_id } => {
                if let Some(job) = self.active.take()
                    && matches!(job.kind, Kind::User)
                {
                    output.push(SystemEvent::Indeterminate { request_id });
                }
            }
        }
        Ok(())
    }
    fn complete(&mut self, job: Job, output: &mut Vec<SystemEvent>) -> Result<(), Error> {
        let failed = job.replies.iter().any(|(_, failed)| *failed);
        if matches!(job.kind, Kind::User) {
            let mut data = Vec::new();
            for (lines, _) in job.replies {
                for line in lines {
                    data.extend(line);
                    data.push(b'\n');
                }
            }
            output.push(SystemEvent::CommandFinished {
                request_id: job.id,
                output: data,
                failed,
            });
            return Ok(());
        }
        if matches!(job.kind, Kind::Name(_) | Kind::Capture(_))
            && job.revision != self.client.state.revision()
        {
            self.retry_bootstrap(Error::StaleDiscovery.to_string(), true, output);
            return Ok(());
        }
        if failed
            || job.replies.len()
                != if matches!(job.kind, Kind::Capture(_)) {
                    2
                } else {
                    1
                }
        {
            let diagnostic = job
                .replies
                .iter()
                .find(|(_, failed)| *failed)
                .and_then(|(lines, _)| lines.first())
                .map(|line| {
                    format!(
                        "{:?}",
                        String::from_utf8_lossy(&line[..line.len().min(4096)])
                    )
                })
                .unwrap_or_else(|| "unexpected reply count".into());
            let command = job
                .commands
                .first()
                .and_then(|command| command.args.first())
                .map(|name| String::from_utf8_lossy(name))
                .unwrap_or_default();
            self.retry_bootstrap(
                format!("stock tmux bootstrap {command} failed: {diagnostic}"),
                false,
                output,
            );
            return Ok(());
        }
        let mut replies = job.replies.into_iter();
        let lines = replies.next().unwrap().0;
        if matches!(job.kind, Kind::Discover(_))
            && self
                .scan
                .as_ref()
                .is_some_and(|scan| scan.revision != self.client.state.revision())
        {
            // Lists are separate C commands. A structural event can invalidate
            // their cross-list references before `finish`/`reconcile` gets a
            // chance to reject the stale revision. Discard those read-only
            // rows first; current-revision malformed records remain fatal.
            self.retry_bootstrap(Error::StaleDiscovery.to_string(), true, output);
            return Ok(());
        }
        match job.kind {
            Kind::Discover(kind) => {
                let scan = self.scan.as_mut().ok_or(Error::InvalidDiscovery)?;
                for line in lines {
                    scan.discovery.add_line(kind, &line)?;
                }
                if kind == DiscoveryKind::Panes {
                    let scan = self.scan.take().unwrap();
                    let snapshot = scan.discovery.finish()?;
                    match self.client.state.reconcile(scan.revision, snapshot) {
                        Ok(()) => {
                            self.internal.extend(
                                self.client
                                    .state
                                    .sessions
                                    .keys()
                                    .map(|id| Kind::Name(Object::Session(*id))),
                            );
                            self.internal.extend(
                                self.client
                                    .state
                                    .windows
                                    .keys()
                                    .map(|id| Kind::Name(Object::Window(*id))),
                            );
                            self.internal.extend(
                                self.client.state.panes.keys().map(|id| Kind::Capture(*id)),
                            );
                            output.push(SystemEvent::State(self.client.state.clone()));
                        }
                        Err(Error::StaleDiscovery) => {
                            self.retry_bootstrap(Error::StaleDiscovery.to_string(), true, output);
                            return Ok(());
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            Kind::Name(object) => {
                let mut name = Vec::new();
                for (i, line) in lines.into_iter().enumerate() {
                    if i != 0 {
                        name.push(b'\n');
                    }
                    name.extend(line);
                }
                if self.client.state.set_name(object, name).is_ok() {
                    output.push(SystemEvent::State(self.client.state.clone()));
                }
            }
            Kind::Capture(pane_id) => {
                if let Some(pane) = self.client.state.panes.get(&pane_id) {
                    let cursor = replies.next().unwrap().0;
                    if cursor.len() != 1 {
                        return Err(Error::InvalidDiscovery);
                    }
                    let values = cursor[0]
                        .split(|b| *b == b'|')
                        .map(|value| {
                            std::str::from_utf8(value)
                                .ok()
                                .and_then(|v| v.parse::<u32>().ok())
                        })
                        .collect::<Option<Vec<_>>>()
                        .ok_or(Error::InvalidDiscovery)?;
                    if values.len() != 5
                        || values[0] > pane.width
                        || values[1] >= pane.height
                        || values[2..].iter().any(|v| *v > 1)
                    {
                        return Err(Error::InvalidDiscovery);
                    }
                    let mut data = if values[3] == 1 {
                        b"\x1b[?1049h".to_vec()
                    } else {
                        b"\x1b[?1049l".to_vec()
                    };
                    data.extend_from_slice(b"\x1b[H\x1b[2J");
                    for (index, line) in lines.into_iter().enumerate() {
                        if index != 0 {
                            data.extend_from_slice(b"\r\n");
                        }
                        data.extend(line);
                    }
                    data.extend_from_slice(
                        format!(
                            "\x1b[{};{}H\x1b[?25{}\x1b[?1{}",
                            values[1] + 1,
                            values[0].min(pane.width - 1) + 1,
                            if values[2] == 1 { 'h' } else { 'l' },
                            if values[4] == 1 { 'h' } else { 'l' }
                        )
                        .as_bytes(),
                    );
                    self.captured.insert(pane_id);
                    output.push(SystemEvent::PaneCapture {
                        pane_id,
                        data,
                        cols: pane.width,
                        rows: pane.height,
                    });
                }
            }
            Kind::User => unreachable!(),
        }
        if self.scan.is_none() && self.internal.is_empty() && !self.bootstrap_requested {
            self.stale_attempts = 0;
        }
        Ok(())
    }
    fn retry_bootstrap(&mut self, error: String, structural: bool, output: &mut Vec<SystemEvent>) {
        self.scan = None;
        self.internal.clear();
        self.stale_attempts += 1;
        self.bootstrap_requested = self.stale_attempts < 3;
        if !self.bootstrap_requested {
            output.push(if structural {
                SystemEvent::DiscoveryDeferred
            } else {
                SystemEvent::Error(error)
            });
        }
    }
}
