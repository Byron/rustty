//! Native tmuxx connections and independent Rustty pane views.
//!
//! The server owns PTYs and application replies. Views own selection, scroll,
//! and rendering. Closing a view never kills its server-side pane.
use crate::session::{Input, SessionEvent};
use rustty_vt::{Effect, Terminal};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread;
use std::time::Duration;
pub use tmuxx_layout::{Axis, Kind, Layout};
use tmuxx_protocol::native::{Hello, Request, ServerMessage, SnapshotAssembler};

mod system;

const RECORD_LIMIT: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub struct Graph {
    pub sessions: Vec<ServerSession>,
    pub windows: Vec<ServerWindow>,
    pub panes: Vec<ServerPane>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct ServerSession {
    pub id: u64,
    pub name: String,
    pub active: u64,
    pub windows: BTreeMap<u32, u64>,
}
#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub id: u64,
    pub name: String,
    pub windows: Vec<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct ServerWindow {
    pub id: u64,
    pub name: String,
    pub active: u64,
    pub cols: u16,
    pub rows: u16,
    pub panes: Vec<u64>,
    pub layout: String,
    #[serde(default)]
    pub zoomed: bool,
    #[serde(default)]
    pub zoomed_pane: Option<u64>,
    #[serde(default)]
    pub visible_layout: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct ServerPane {
    pub id: u64,
    pub window: u64,
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
    pub cols: u16,
    pub rows: u16,
    pub status: Option<i32>,
    #[serde(default)]
    pub persistent: bool,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub modal: bool,
    #[serde(default)]
    pub modal_capture_keys: bool,
    #[serde(default)]
    pub modal_close_click: bool,
    #[serde(default)]
    pub modal_close_cancel: bool,
    #[serde(default)]
    pub terminal: serde_json::Value,
}

#[derive(Default)]
struct Model {
    instance: String,
    sequence: u64,
    graph: Option<Graph>,
    panes: BTreeMap<u64, PaneState>,
    errors: Vec<String>,
    last_host_event: Option<u64>,
}
struct PaneState {
    host_state: Option<(bool, bool, bool)>,
    owners: Arc<AtomicUsize>,
    terminal: Terminal,
    agent_status: tmuxx_terminal::AgentStatus,
    views: Vec<View>,
}
struct View {
    terminal: Weak<Mutex<Terminal>>,
    events: mpsc::SyncSender<SessionEvent>,
    exited: Weak<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
    focused: Arc<AtomicBool>,
    visible: Arc<AtomicBool>,
    dark: Arc<AtomicBool>,
    pending: Arc<Mutex<Vec<(u64, Effect)>>>,
}
struct Shared {
    model: Mutex<Model>,
    requests: mpsc::SyncSender<Request>,
    next: AtomicU64,
    wake: Arc<dyn Fn() + Send + Sync>,
    closed: AtomicBool,
    subscribe: bool,
}

/// One connection owns its workers and (only for `embedded`) its daemon lifetime.
#[derive(Clone)]
enum Source {
    None,
    #[cfg(unix)]
    Unix(PathBuf),
    Ssh {
        host: String,
        command: String,
    },
    SystemSsh(String),
}
type SizeViews = BTreeMap<u64, (Connection, (u16, u16, u16, u16))>;

pub struct Connection {
    system: Option<system::SystemConnection>,
    source: Source,
    size_views: Mutex<SizeViews>,
    shared: Arc<Shared>,
    owner: Option<tmuxx_runtime::ServerHandle>,
    child: Option<Child>,
    #[cfg(unix)]
    socket: Option<std::os::unix::net::UnixStream>,
}

#[derive(Clone)]
pub struct PaneInput {
    shared: Weak<Shared>,
    pane: u64,
    focused: Arc<AtomicBool>,
    visible: Arc<AtomicBool>,
    dark: Arc<AtomicBool>,
    pending: Arc<Mutex<Vec<(u64, Effect)>>>,
    owners: Arc<AtomicUsize>,
    released: Arc<AtomicBool>,
    native: bool,
}
impl PaneInput {
    pub fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.set_host_state(false, false, self.dark.load(Ordering::Acquire));
        if self.owners.fetch_sub(1, Ordering::AcqRel) == 1
            && self.native
            && let Some(shared) = self.shared.upgrade()
        {
            if shared
                .model
                .lock()
                .is_ok_and(|model| !model.panes.contains_key(&self.pane))
            {
                return;
            }
            let _ = send(
                &shared,
                vec![
                    b"tmuxx-release-pane".to_vec(),
                    b"-t".to_vec(),
                    format!("%{}", self.pane).into_bytes(),
                ],
            );
        }
    }
    pub fn set_persistent(&self, persistent: bool) -> io::Result<()> {
        if !self.native {
            return Err(err("retention leases require a native tmuxx server"));
        }
        let shared = self
            .shared
            .upgrade()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        send(
            &shared,
            vec![
                b"tmuxx-retain-pane".to_vec(),
                b"-t".to_vec(),
                format!("%{}", self.pane).into_bytes(),
                if persistent {
                    b"on".to_vec()
                } else {
                    b"off".to_vec()
                },
            ],
        )
        .map(|_| ())
    }
    pub fn set_host_state(&self, focused: bool, visible: bool, dark: bool) -> io::Result<()> {
        self.focused.store(focused, Ordering::Release);
        self.visible.store(visible, Ordering::Release);
        self.dark.store(dark, Ordering::Release);
        if !self.native {
            return Ok(());
        }
        let shared = self
            .shared
            .upgrade()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        let mut model = shared.model.lock().map_err(err)?;
        let Some(pane) = model.panes.get_mut(&self.pane) else {
            return Ok(());
        };
        let focused = pane.views.iter().any(|v| {
            v.terminal.strong_count() > 0
                && v.focused.load(Ordering::Acquire)
                && v.visible.load(Ordering::Acquire)
        });
        let visible = pane
            .views
            .iter()
            .any(|v| v.terminal.strong_count() > 0 && v.visible.load(Ordering::Acquire));
        let dark = pane
            .views
            .iter()
            .filter(|v| v.terminal.strong_count() > 0 && v.visible.load(Ordering::Acquire))
            .max_by_key(|v| v.focused.load(Ordering::Acquire))
            .map(|v| v.dark.load(Ordering::Acquire))
            .or(pane.host_state.map(|state| state.2))
            .unwrap_or(dark);
        if pane.host_state == Some((focused, visible, dark)) {
            return Ok(());
        }
        let request_id = shared.next.fetch_add(1, Ordering::Relaxed);
        shared.requests.try_send(Request {
            request_id, argv: vec![b"tmuxx-host-state".to_vec(), b"-t".to_vec(), format!("%{}", self.pane).into_bytes()],
            stdin: Some(serde_json::to_vec(&serde_json::json!({"focused":focused,"visible":visible,"color_scheme":if dark { "dark" } else { "light" }})).map_err(err)?),
            subscribe: true, control: false,
        }).map_err(err)?;
        pane.host_state = Some((focused, visible, dark));
        Ok(())
    }
    pub fn host_reply(&self, effect: Effect, response: HostResponse) -> io::Result<()> {
        let shared = self
            .shared
            .upgrade()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        let mut pending = self.pending.lock().map_err(err)?;
        let index = pending
            .iter()
            .position(|(_, request)| *request == effect)
            .ok_or_else(|| err("clipboard request has expired"))?;
        let (event_id, _) = pending.remove(index);
        host_reply(&shared, event_id, response)
    }
    pub fn is_native(&self) -> bool {
        self.native
    }
    pub fn write_input(&self, input: &Input) -> io::Result<()> {
        if input.queue_bytes() > 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "tmuxx input exceeds one MiB",
            ));
        }
        let shared = self
            .shared
            .upgrade()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        shared
            .requests
            .try_send(Request {
                request_id: shared.next.fetch_add(1, Ordering::Relaxed),
                argv: vec![
                    b"tmuxx-input".to_vec(),
                    b"-t".to_vec(),
                    format!("%{}", self.pane).into_bytes(),
                ],
                stdin: Some(serde_json::to_vec(input).map_err(err)?),
                subscribe: true,
                control: false,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => io::Error::from(io::ErrorKind::WouldBlock),
                mpsc::TrySendError::Disconnected(_) => io::Error::from(io::ErrorKind::BrokenPipe),
            })
    }
    /// Stock tmux's send-keys synchronizes literal bytes; protocol reports must not.
    pub fn write_direct(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "stock tmux direct input cannot represent NUL bytes",
            ));
        }
        if bytes.len() > 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "tmuxx input exceeds one MiB",
            ));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let shared = self
            .shared
            .upgrade()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        shared
            .requests
            .try_send(Request {
                request_id: shared.next.fetch_add(1, Ordering::Relaxed),
                argv: vec![
                    b"tmuxx-system-direct".to_vec(),
                    format!("%{}", self.pane).into_bytes(),
                ],
                stdin: Some(bytes.to_vec()),
                subscribe: true,
                control: false,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => io::Error::from(io::ErrorKind::WouldBlock),
                mpsc::TrySendError::Disconnected(_) => io::Error::from(io::ErrorKind::BrokenPipe),
            })
    }
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        if self.native {
            return self.write_input(&Input::Text(bytes.to_vec()));
        }
        let shared = self
            .shared
            .upgrade()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        if bytes.len() > 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "tmuxx input exceeds one MiB",
            ));
        }
        let mut argv = vec![
            b"send-keys".to_vec(),
            b"-H".to_vec(),
            b"-t".to_vec(),
            format!("%{}", self.pane).into_bytes(),
        ];
        argv.extend(bytes.iter().map(|b| format!("{b:02x}").into_bytes()));
        send(&shared, argv).map(|_| ())
    }
}

impl Drop for PaneInput {
    fn drop(&mut self) {
        self.release();
    }
}

pub(crate) struct Attached {
    pub terminal: Arc<Mutex<Terminal>>,
    pub input: PaneInput,
    pub events: mpsc::Receiver<SessionEvent>,
    pub exited: Arc<AtomicBool>,
}

fn err(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}
fn send(shared: &Shared, argv: Vec<Vec<u8>>) -> io::Result<u64> {
    if shared.closed.load(Ordering::Acquire) {
        return Err(err("tmuxx connection is closed"));
    }
    let request_id = shared.next.fetch_add(1, Ordering::Relaxed);
    shared
        .requests
        .try_send(Request {
            request_id,
            argv,
            stdin: None,
            subscribe: shared.subscribe,
            control: !shared.subscribe,
        })
        .map_err(err)?;
    Ok(request_id)
}
impl Connection {
    fn base(wake: Arc<dyn Fn() + Send + Sync>, subscribe: bool) -> (Self, mpsc::Receiver<Request>) {
        let (requests, receiver) = mpsc::sync_channel(256);
        let shared = Arc::new(Shared {
            model: Mutex::new(Model::default()),
            requests,
            next: AtomicU64::new(1),
            wake,
            closed: AtomicBool::new(false),
            subscribe,
        });
        (
            Self {
                system: None,
                source: Source::None,
                size_views: Mutex::new(BTreeMap::new()),
                shared,
                owner: None,
                child: None,
                #[cfg(unix)]
                socket: None,
            },
            receiver,
        )
    }
    pub fn embedded(endpoint: &Path, wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        Self::embedded_with_command(endpoint, wake, Vec::new())
    }
    pub fn embedded_with_command(
        endpoint: &Path,
        wake: Arc<dyn Fn() + Send + Sync>,
        initial: Vec<Vec<u8>>,
    ) -> io::Result<Self> {
        let owner = tmuxx_runtime::start(tmuxx_runtime::Options::new(endpoint))?;
        let client = owner.connect_with_context(tmuxx_runtime::ClientContext {
            host_effects: true,
            ..Default::default()
        })?;
        // Only this newly created owner adopts Rustty's clipboard policy UI.
        // Existing socket/SSH daemons retain their configured tmux privacy policy.
        for (request_id, name, value) in [
            (u64::MAX - 1, "set-clipboard", "on"),
            (u64::MAX, "get-clipboard", "request"),
        ] {
            client.send(Request {
                request_id,
                argv: ["set-option", "-s", name, value]
                    .map(|arg| arg.as_bytes().to_vec())
                    .to_vec(),
                stdin: None,
                subscribe: false,
                control: false,
            })?;
        }
        let mut connection = Self::local(client, wake, true)?;
        connection.owner = Some(owner);
        #[cfg(unix)]
        {
            connection.source = Source::Unix(endpoint.to_owned());
        }
        let mut argv = vec![
            b"new-session".to_vec(),
            b"-d".to_vec(),
            b"-s".to_vec(),
            b"rustty".to_vec(),
        ];
        argv.extend(initial);
        connection.command_bytes(argv)?;
        Ok(connection)
    }
    fn local(
        client: tmuxx_runtime::LocalClient,
        wake: Arc<dyn Fn() + Send + Sync>,
        subscribe: bool,
    ) -> io::Result<Self> {
        let (connection, requests) = Self::base(wake, subscribe);
        let shared = connection.shared.clone();
        thread::Builder::new()
            .name("rustty-tmuxx-local".into())
            .spawn(move || {
                while !shared.closed.load(Ordering::Acquire) {
                    for request in requests.try_iter() {
                        if let Err(error) = client.send(request) {
                            fail(&shared, error);
                            return;
                        }
                    }
                    loop {
                        match client.try_recv() {
                            Ok(message) => {
                                if let Err(error) = apply(&shared, message) {
                                    fail(&shared, error);
                                    return;
                                }
                                if shared.closed.load(Ordering::Acquire) {
                                    return;
                                }
                            }
                            Err(mpsc::TryRecvError::Empty) => break,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                fail(&shared, "tmuxx disconnected");
                                return;
                            }
                        }
                    }
                    thread::sleep(Duration::from_millis(2));
                }
            })?;
        Ok(connection)
    }
    #[cfg(unix)]
    pub fn unix(endpoint: &Path, wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        Self::connect_source(Source::Unix(endpoint.to_owned()), wake, true, true)
    }
    pub fn ssh(host: &str, wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        Self::ssh_command(host, "tmuxx --native-stdio", wake)
    }
    /// Run a configured native helper through the remote user's shell.
    pub fn ssh_command(
        host: &str,
        remote_command: &str,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> io::Result<Self> {
        if host.trim().is_empty() || host.contains('\0') {
            return Err(err("SSH host must be nonempty and contain no NUL"));
        }
        if remote_command.trim().is_empty() || remote_command.contains('\0') {
            return Err(err("SSH command must be nonempty and contain no NUL"));
        }
        Self::connect_source(
            Source::Ssh {
                host: host.to_owned(),
                command: remote_command.to_owned(),
            },
            wake,
            true,
            true,
        )
    }
    fn connect_source(
        source: Source,
        wake: Arc<dyn Fn() + Send + Sync>,
        host_effects: bool,
        subscribe: bool,
    ) -> io::Result<Self> {
        let mut connection = match &source {
            #[cfg(unix)]
            Source::Unix(endpoint) => {
                let stream = std::os::unix::net::UnixStream::connect(endpoint)?;
                let mut connection = Self::stream_inner(
                    stream.try_clone()?,
                    stream.try_clone()?,
                    wake,
                    host_effects,
                    subscribe,
                )?;
                connection.socket = Some(stream);
                connection
            }
            Source::Ssh { host, command } => {
                let mut child = Command::new("ssh")
                    .args([
                        "-T",
                        "-o",
                        "BatchMode=yes",
                        "-o",
                        "LogLevel=ERROR",
                        "--",
                        host,
                    ])
                    .arg(command)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()?;
                match Self::stream_inner(
                    child.stdout.take().unwrap(),
                    child.stdin.take().unwrap(),
                    wake,
                    host_effects,
                    subscribe,
                ) {
                    Ok(mut connection) => {
                        let stderr = child.stderr.take().unwrap();
                        let shared = connection.shared.clone();
                        let host = host.clone();
                        if let Err(error) = thread::Builder::new()
                            .name("rustty-tmuxx-ssh-errors".into())
                            .spawn(move || read_ssh_errors(stderr, &shared, &host))
                        {
                            let _ = child.kill();
                            let _ = child.wait();
                            return Err(error);
                        }
                        connection.child = Some(child);
                        connection
                    }
                    Err(error) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(error);
                    }
                }
            }
            Source::SystemSsh(host) => return Self::system_ssh(host, wake),
            Source::None => return Err(err("stream caller must provide a size client")),
        };
        connection.source = source;
        Ok(connection)
    }
    /// Each OS window registers its own session-size context, preserving the
    /// server's latest/smallest/largest arbitration across remote GUI clients.
    pub fn view_size(
        &self,
        session: u64,
        cols: u16,
        rows: u16,
        cell_width: u16,
        cell_height: u16,
    ) -> io::Result<()> {
        if let Some(system) = &self.system {
            return system.view_size(session, cols, rows);
        }
        let mut views = self.size_views.lock().map_err(err)?;
        if let std::collections::btree_map::Entry::Vacant(entry) = views.entry(session) {
            let connection = if let Some(owner) = &self.owner {
                Self::local(
                    owner.connect_with_context(tmuxx_runtime::ClientContext::default())?,
                    self.shared.wake.clone(),
                    false,
                )?
            } else {
                Self::connect_source(self.source.clone(), self.shared.wake.clone(), false, false)?
            };
            connection.command(["attach-session".into(), "-t".into(), format!("${session}")])?;
            entry.insert((connection, (0, 0, 0, 0)));
        }
        let (connection, size) = views.get_mut(&session).unwrap();
        if *size != (cols, rows, cell_width, cell_height) {
            connection.command([
                "refresh-client".into(),
                "-C".into(),
                format!("{cols},{rows}"),
            ])?;
            connection.command([
                "tmuxx-cell-size".into(),
                cell_width.to_string(),
                cell_height.to_string(),
            ])?;
            *size = (cols, rows, cell_width, cell_height);
        }
        let errors = connection.errors();
        if !errors.is_empty() {
            return Err(err(errors.join("; ")));
        }
        Ok(())
    }
    /// Stop contributing this window's size after its session view is detached.
    pub fn release_view(&self, session: u64) -> io::Result<()> {
        self.size_views.lock().map_err(err)?.remove(&session);
        Ok(())
    }
    /// Attach a negotiated native byte transport. The caller owns its shutdown.
    pub fn stream(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> io::Result<Self> {
        Self::stream_inner(reader, writer, wake, true, true)
    }
    fn stream_inner(
        reader: impl Read + Send + 'static,
        mut writer: impl Write + Send + 'static,
        wake: Arc<dyn Fn() + Send + Sync>,
        host_effects: bool,
        subscribe: bool,
    ) -> io::Result<Self> {
        let (connection, requests) = Self::base(wake, subscribe);
        write_record(
            &mut writer,
            &Hello {
                host_effects,
                ..Default::default()
            },
        )?;
        let shared = connection.shared.clone();
        thread::Builder::new()
            .name("rustty-tmuxx-write".into())
            .spawn(move || {
                while !shared.closed.load(Ordering::Acquire) {
                    match requests.recv_timeout(Duration::from_millis(50)) {
                        Ok(request) => {
                            if let Err(error) = write_record(&mut writer, &request) {
                                fail(&shared, error);
                                return;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            })?;
        let shared = connection.shared.clone();
        thread::Builder::new()
            .name("rustty-tmuxx-read".into())
            .spawn(move || {
                let result = (|| -> io::Result<()> {
                    let mut reader = BufReader::new(reader);
                    let hello: Hello = serde_json::from_slice(
                        &record(&mut reader)?.ok_or_else(|| err("missing tmuxx hello"))?,
                    )
                    .map_err(err)?;
                    if hello.version != tmuxx_protocol::native::VERSION {
                        return Err(err("unsupported tmuxx native version"));
                    }
                    let mut snapshots = SnapshotAssembler::default();
                    while !shared.closed.load(Ordering::Acquire) {
                        let Some(bytes) = record(&mut reader)? else {
                            snapshots.finish()?;
                            return Err(err("tmuxx disconnected"));
                        };
                        if let Some(message) =
                            snapshots.push(serde_json::from_slice(&bytes).map_err(err)?)?
                        {
                            apply(&shared, message)?;
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    fail(&shared, error);
                }
            })?;
        if subscribe {
            connection.command(std::iter::empty::<&str>())?;
        }
        Ok(connection)
    }
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
    }
    pub fn command<I, S>(&self, argv: I) -> io::Result<u64>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.command_bytes(
            argv.into_iter()
                .map(|s| s.as_ref().as_bytes().to_vec())
                .collect(),
        )
    }
    pub fn command_bytes(&self, argv: Vec<Vec<u8>>) -> io::Result<u64> {
        send(&self.shared, argv)
    }
    pub fn persistence_survives_app(&self) -> bool {
        self.owner.is_none() && self.system.is_none()
    }
    pub fn instance(&self) -> String {
        self.shared
            .model
            .lock()
            .map(|m| m.instance.clone())
            .unwrap_or_default()
    }
    pub fn graph(&self) -> Option<Graph> {
        self.shared.model.lock().ok()?.graph.clone()
    }
    /// Session metadata without cloning terminal snapshots for native menus.
    pub fn sessions(&self) -> Vec<SessionInfo> {
        self.shared
            .model
            .lock()
            .ok()
            .and_then(|model| {
                model.graph.as_ref().map(|graph| {
                    graph
                        .sessions
                        .iter()
                        .map(|session| SessionInfo {
                            id: session.id,
                            name: session.name.clone(),
                            windows: session
                                .windows
                                .values()
                                .filter_map(|id| {
                                    graph
                                        .windows
                                        .iter()
                                        .find(|window| window.id == *id)
                                        .map(|window| window.name.clone())
                                })
                                .collect(),
                        })
                        .collect()
                })
            })
            .unwrap_or_default()
    }
    pub fn errors(&self) -> Vec<String> {
        self.shared
            .model
            .lock()
            .map(|mut m| std::mem::take(&mut m.errors))
            .unwrap_or_default()
    }
    pub(crate) fn attach(
        &self,
        pane: u64,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> io::Result<Attached> {
        let mut model = self.shared.model.lock().map_err(err)?;
        let was_exited = model
            .graph
            .as_ref()
            .and_then(|g| g.panes.iter().find(|p| p.id == pane))
            .is_some_and(|p| p.status.is_some());
        let pane_state = model
            .panes
            .get_mut(&pane)
            .ok_or_else(|| err("tmuxx pane has no snapshot"))?;
        let owners = pane_state.owners.clone();
        let native = self.system.is_none();
        if owners.fetch_add(1, Ordering::AcqRel) == 0 && native {
            send(
                &self.shared,
                vec![
                    b"tmuxx-claim-pane".to_vec(),
                    b"-t".to_vec(),
                    format!("%{pane}").into_bytes(),
                ],
            )?;
        }
        let terminal = Arc::new(Mutex::new(pane_state.terminal.clone()));
        let exited = Arc::new(AtomicBool::new(was_exited));
        let (events_tx, events) = mpsc::sync_channel(256);
        if !was_exited && let Some(snapshot) = &pane_state.agent_status.snapshot {
            let _ = events_tx.try_send(SessionEvent::Effect(Effect::AgentStatus(
                rustty_vt::agent::Event::Begin(snapshot.clone()),
            )));
            wake();
        }
        let focused = Arc::new(AtomicBool::new(false));
        let visible = Arc::new(AtomicBool::new(false));
        let dark = Arc::new(AtomicBool::new(true));
        let pending = Arc::new(Mutex::new(Vec::new()));
        pane_state.views.push(View {
            focused: focused.clone(),
            visible: visible.clone(),
            dark: dark.clone(),
            pending: pending.clone(),
            terminal: Arc::downgrade(&terminal),
            events: events_tx,
            exited: Arc::downgrade(&exited),
            wake,
        });
        Ok(Attached {
            terminal,
            input: PaneInput {
                shared: Arc::downgrade(&self.shared),
                pane,
                focused,
                visible,
                dark,
                pending,
                owners,
                released: Arc::new(AtomicBool::new(false)),
                native,
            },
            events,
            exited,
        })
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        if let Some(system) = &mut self.system {
            system.close();
        }
        #[cfg(unix)]
        if let Some(socket) = &self.socket {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Ok(mut views) = self.size_views.lock() {
            views.clear();
        }
        self.owner.take();
    }
}
fn write_record(writer: &mut impl Write, value: &impl serde::Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, value).map_err(err)?;
    writer.write_all(b"\n")?;
    writer.flush()
}
fn record(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    tmuxx_runtime::read_record(reader, RECORD_LIMIT)
}
fn read_ssh_errors(mut reader: impl Read, shared: &Shared, host: &str) {
    let mut bytes = Vec::new();
    let _ = reader.by_ref().take(8192).read_to_end(&mut bytes);
    let diagnostic = String::from_utf8_lossy(&bytes);
    if !diagnostic.trim().is_empty() {
        // Keep diagnostics even if stdout already reported a missing hello or EOF.
        if let Ok(mut model) = shared.model.lock() {
            model
                .errors
                .push(format!("SSH to {host}: {}", diagnostic.trim()));
        }
        (shared.wake)();
    }
    let _ = io::copy(&mut reader, &mut io::sink());
}

#[cfg(test)]
mod ssh_tests {
    use super::*;

    #[test]
    fn failed_handshake_keeps_bounded_ssh_diagnostic_and_drains_stderr() {
        let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
        fail(&connection.shared, "missing tmuxx hello");
        let bytes = vec![b'e'; 16384];
        let mut stderr = io::Cursor::new(bytes);
        read_ssh_errors(&mut stderr, &connection.shared, "example");
        assert_eq!(stderr.position(), 16384);
        let errors = connection.errors();
        assert_eq!(errors[0], "missing tmuxx hello");
        assert_eq!(errors[1], format!("SSH to example: {}", "e".repeat(8192)));
    }

    #[test]
    fn invalid_ssh_host_and_command_are_rejected_before_starting_transport() {
        for (host, command) in [
            ("", "helper"),
            ("host\0", "helper"),
            ("host", "  "),
            ("host", "bad\0command"),
        ] {
            assert!(Connection::ssh_command(host, command, Arc::new(|| {})).is_err());
        }
    }
}

fn fail(shared: &Shared, error: impl std::fmt::Display) {
    if shared.closed.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Ok(mut model) = shared.model.lock() {
        model.errors.push(error.to_string());
        for pane in model.panes.values() {
            for view in &pane.views {
                let _ = view.events.try_send(SessionEvent::Error(error.to_string()));
                (view.wake)();
            }
        }
    }
    (shared.wake)();
}
fn restore(value: &serde_json::Value) -> io::Result<(Terminal, tmuxx_terminal::AgentStatus)> {
    let snapshot: tmuxx_terminal::Snapshot = serde_json::from_value(value.clone()).map_err(err)?;
    let terminal = snapshot.restore()?;
    let agent_status = terminal.agent_status().clone();
    Ok((terminal.into_backend(), agent_status))
}
fn close_views(pane: &PaneState) {
    for view in &pane.views {
        if let Some(exited) = view.exited.upgrade() {
            exited.store(true, Ordering::Release);
        }
        (view.wake)();
    }
}
fn replace_view(view: &mut Terminal, source: &Terminal) {
    let selection = view.screen().selection;
    let offset = view.screen().viewport_offset;
    let same_screen = view.is_alternate_screen() == source.is_alternate_screen();
    let pixels = (view.width_px, view.height_px);
    *view = source.clone();
    view.set_pixel_size(pixels.0, pixels.1);
    if same_screen {
        let screen = view.screen_mut();
        screen.viewport_offset = offset.min(screen.history_len());
        screen.selection = selection.filter(|s| {
            [s.start, s.end].iter().all(|point| {
                screen
                    .all_rows()
                    .any(|r| r.id == point.row && point.col < r.cells.len())
            })
        });
    }
}
fn apply(shared: &Shared, message: ServerMessage) -> io::Result<()> {
    let mut model = shared.model.lock().map_err(err)?;
    match message {
        ServerMessage::File { .. } => {
            return Err(err(
                "unexpected Unix IPC file operation on native transport",
            ));
        }
        ServerMessage::Snapshot {
            instance_id,
            sequence,
            state,
        } => {
            if model.instance == instance_id && sequence < model.sequence {
                return Ok(());
            }
            let graph: Graph = serde_json::from_value(state).map_err(err)?;
            validate_graph(&graph)?;
            let mut restored = BTreeMap::new();
            for pane in &graph.panes {
                restored.insert(pane.id, restore(&pane.terminal)?);
            }
            if model.instance != instance_id {
                for pane in model.panes.values() {
                    close_views(pane);
                }
                model.panes.clear();
                model.last_host_event = None;
            }
            for (id, (terminal, agent_status)) in restored {
                let remote = graph.panes.iter().find(|p| p.id == id).unwrap();
                let replaced = model
                    .graph
                    .as_ref()
                    .and_then(|g| g.panes.iter().find(|p| p.id == id))
                    .is_some_and(|old| old.pid.is_some() && old.pid != remote.pid);
                let pane = model.panes.entry(id).or_insert_with(|| PaneState {
                    host_state: None,
                    owners: Arc::new(AtomicUsize::new(0)),
                    terminal: terminal.clone(),
                    agent_status: tmuxx_terminal::AgentStatus::default(),
                    views: Vec::new(),
                });
                let agent_event = if replaced && agent_status.snapshot.is_some() {
                    agent_status.event_since(&tmuxx_terminal::AgentStatus::default())
                } else {
                    agent_status.event_since(&pane.agent_status)
                };
                pane.agent_status = agent_status;
                pane.terminal = terminal;
                pane.views.retain(|view| {
                    let Some(terminal) = view.terminal.upgrade() else {
                        return false;
                    };
                    if let Some(exited) = view.exited.upgrade() {
                        exited.store(remote.status.is_some(), Ordering::Release);
                    }
                    if let Ok(mut terminal) = terminal.lock() {
                        if replaced {
                            terminal.screen_mut().selection = None;
                            terminal.screen_mut().viewport_offset = 0;
                        }
                        replace_view(&mut terminal, &pane.terminal);
                    }
                    if let Some(event) = &agent_event {
                        let _ = view
                            .events
                            .try_send(SessionEvent::Effect(Effect::AgentStatus(event.clone())));
                    }
                    (view.wake)();
                    true
                });
            }
            model.panes.retain(|id, pane| {
                let present = graph.panes.iter().any(|p| p.id == *id);
                if !present {
                    close_views(pane);
                }
                present
            });
            model.instance = instance_id;
            model.sequence = sequence;
            model.graph = Some(graph);
            (shared.wake)();
        }
        ServerMessage::PaneOutput {
            sequence,
            pane_id,
            data,
        } => {
            if sequence <= model.sequence {
                return Ok(());
            }
            model.sequence = sequence;
            if let Some(pane) = model.panes.get_mut(&pane_id) {
                // Application replies belong to the owner. Host actions require
                // the native elected-recipient protocol, never replay effects.
                let mut effects = Vec::new();
                for effect in pane.terminal.feed(&data) {
                    let ended = pane.agent_status.observe(&effect);
                    if view_effect(&effect) {
                        effects.push(effect);
                    }
                    if let Some(event) = ended {
                        effects.push(Effect::AgentStatus(event));
                    }
                }
                pane.views.retain(|view| {
                    let Some(terminal) = view.terminal.upgrade() else {
                        return false;
                    };
                    if let Ok(mut terminal) = terminal.lock() {
                        let _ = terminal.feed(&data);
                    }
                    for effect in &effects {
                        let _ = view.events.try_send(SessionEvent::Effect(effect.clone()));
                    }
                    (view.wake)();
                    true
                });
            } else {
                send(shared, Vec::new())?;
            }
        }
        ServerMessage::HostEffect {
            pane_id,
            event_id,
            action,
            ..
        } => {
            if model.last_host_event.is_some_and(|last| event_id <= last) {
                return Ok(());
            }
            model.last_host_event = Some(event_id);
            let action: HostAction = serde_json::from_value(action).map_err(err)?;
            let (effect, denied) = match action {
                HostAction::Bell => (Effect::Bell, None),
                HostAction::Notification { title, body } => {
                    (Effect::Notification { title, body }, None)
                }
                HostAction::ClipboardRead(read) => (
                    Effect::ClipboardRead(read),
                    Some(HostResponse::Read(rustty_vt::clipboard::ReadResult::Denied)),
                ),
                HostAction::ClipboardWrite(write) => (
                    Effect::ClipboardWrite(write),
                    Some(HostResponse::Write(
                        rustty_vt::clipboard::WriteResult::Denied,
                    )),
                ),
            };
            let mut delivered = false;
            if let Some(pane) = model.panes.get_mut(&pane_id) {
                pane.views.retain(|v| v.terminal.strong_count() > 0);
                if let Some(view) = pane
                    .views
                    .iter()
                    .find(|v| v.focused.load(Ordering::Acquire))
                    .or_else(|| pane.views.first())
                    && view.pending.lock().map_err(err)?.len() < 256
                    && view
                        .events
                        .try_send(SessionEvent::Effect(effect.clone()))
                        .is_ok()
                {
                    if denied.is_some() {
                        view.pending.lock().map_err(err)?.push((event_id, effect));
                    }
                    (view.wake)();
                    delivered = true;
                }
            }
            if !delivered && let Some(response) = denied {
                host_reply(shared, event_id, response)?;
            }
        }
        ServerMessage::Changed { .. } => {
            send(shared, Vec::new())?;
        }
        ServerMessage::Reply(reply) => {
            if let Some(error) = reply.error {
                model.errors.push(error);
                (shared.wake)();
            }
        }
        ServerMessage::Detached { .. } => {
            shared.closed.store(true, Ordering::Release);
            for pane in model.panes.values() {
                close_views(pane);
            }
            (shared.wake)();
        }
        ServerMessage::Exit { reason } => {
            drop(model);
            fail(shared, reason);
        }
        ServerMessage::SnapshotAborted { error, .. } => {
            return Err(err(format!("tmuxx snapshot aborted: {error}")));
        }
        ServerMessage::SnapshotStart { .. }
        | ServerMessage::SnapshotChunk { .. }
        | ServerMessage::SnapshotEnd { .. } => return Err(err("unassembled tmuxx snapshot")),
        ServerMessage::Control { .. }
        | ServerMessage::ClientFlags { .. }
        | ServerMessage::Stdin { .. } => {}
    }
    Ok(())
}

#[derive(Deserialize)]
enum HostAction {
    Bell,
    Notification { title: Vec<u8>, body: Vec<u8> },
    ClipboardRead(rustty_vt::clipboard::Read),
    ClipboardWrite(rustty_vt::clipboard::Write),
}
#[derive(serde::Serialize)]
pub enum HostResponse {
    Read(rustty_vt::clipboard::ReadResult),
    Write(rustty_vt::clipboard::WriteResult),
}
fn host_reply(shared: &Shared, event_id: u64, response: HostResponse) -> io::Result<()> {
    let request_id = shared.next.fetch_add(1, Ordering::Relaxed);
    shared
        .requests
        .try_send(Request {
            request_id,
            argv: vec![
                b"tmuxx-host-reply".to_vec(),
                event_id.to_string().into_bytes(),
            ],
            stdin: Some(serde_json::to_vec(&response).map_err(err)?),
            subscribe: true,
            control: false,
        })
        .map_err(err)
}

fn view_effect(effect: &Effect) -> bool {
    !matches!(
        effect,
        Effect::Write(_)
            | Effect::Bell
            | Effect::Notification { .. }
            | Effect::ClipboardRead(_)
            | Effect::ClipboardWrite(_)
    )
}
fn feed_view(terminal: &mut Terminal, data: &[u8], events: &mpsc::SyncSender<SessionEvent>) {
    for effect in terminal.feed(data).into_iter().filter(view_effect) {
        let _ = events.try_send(SessionEvent::Effect(effect));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::session::Session;
    use std::time::Instant;

    #[test]
    fn session_menu_metadata_keeps_tab_order_and_duplicate_links() {
        let (connection, _requests) = Connection::base(Arc::new(|| {}), false);
        connection.shared.model.lock().unwrap().graph = Some(serde_json::from_value(serde_json::json!({
            "sessions": [{"id": 7, "name": "work", "active": 0, "windows": {"0": 2, "1": 3, "2": 2}}],
            "windows": [
                {"id": 2, "name": "editor", "active": 0, "cols": 80, "rows": 24, "panes": [], "layout": ""},
                {"id": 3, "name": "logs", "active": 0, "cols": 80, "rows": 24, "panes": [], "layout": ""}
            ], "panes": []
        })).unwrap());
        let sessions = connection.sessions();
        assert_eq!((sessions[0].id, sessions[0].name.as_str()), (7, "work"));
        assert_eq!(sessions[0].windows, ["editor", "logs", "editor"]);
    }

    fn wait(connection: &Connection, predicate: impl Fn(&Graph) -> bool) -> Graph {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(connection.errors().is_empty());
            if let Some(graph) = connection.graph()
                && predicate(&graph)
            {
                return graph;
            }
            assert!(Instant::now() < deadline, "tmuxx view did not converge");
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn native_agent_status_attach_live_and_resync_do_not_replay_host_actions() {
        use rustty_vt::agent::{Event, State};
        let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
        let mut owner = tmuxx_terminal::Terminal::new(20, 4, 20);
        let begin =
            Event::from_json(br#"{"op":"begin","state":"working","thread_id":"thread"}"#).unwrap();
        owner.feed(b"\x1b]133;C\x07");
        owner.feed(&begin.frame().unwrap());
        owner.feed(b"\x07\x1b]52;c;Y2xpcGJvYXJk\x07");
        let snapshot = |owner: &tmuxx_terminal::Terminal, sequence| ServerMessage::Snapshot {
            instance_id: "agent-owner".into(),
            sequence,
            state: serde_json::json!({
                "sessions": [{"id":1,"name":"agent","active":0,"windows":{"0":2}}],
                "windows": [{"id":2,"name":"shell","active":7,"cols":20,"rows":4,"panes":[7],"layout":Layout::pane(7,20,4).to_legacy().unwrap()}],
                "panes": [{"id":7,"window":2,"cols":20,"rows":4,"pid":123,"status":null,"terminal":owner.snapshot()}],
            }),
        };
        apply(&connection.shared, snapshot(&owner, 1)).unwrap();
        let first = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        let second = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        for view in [&first, &second] {
            assert!(matches!(
                view.events().next(),
                Some(SessionEvent::Effect(Effect::AgentStatus(Event::Begin(_))))
            ));
            assert_eq!(view.events().count(), 0);
            assert!(view.terminal().unwrap().agent_status_events);
            assert!(view.terminal().unwrap().shell_command_events);
        }
        apply(&connection.shared, snapshot(&owner, 1)).unwrap();
        assert_eq!(first.events().count(), 0);
        let update =
            Event::from_json(br#"{"op":"update","state":"done","turn_id":"turn"}"#).unwrap();
        let mut live = update.frame().unwrap();
        live.extend(b"\x07\x1b]52;c;Y2xpcGJvYXJk\x07");
        owner.feed(&live);
        apply(
            &connection.shared,
            ServerMessage::PaneOutput {
                sequence: 2,
                pane_id: 7,
                data: live,
            },
        )
        .unwrap();
        for view in [&first, &second] {
            assert!(
                matches!(view.events().next(), Some(SessionEvent::Effect(Effect::AgentStatus(Event::Update(status)))) if status.state == State::Done)
            );
            assert_eq!(view.events().count(), 0);
        }
        apply(&connection.shared, snapshot(&owner, 2)).unwrap();
        assert_eq!(first.events().count(), 0);
        owner.feed(
            &Event::from_json(br#"{"op":"update","state":"needs_input"}"#)
                .unwrap()
                .frame()
                .unwrap(),
        );
        apply(&connection.shared, snapshot(&owner, 3)).unwrap();
        for view in [&first, &second] {
            assert!(
                matches!(view.events().next(), Some(SessionEvent::Effect(Effect::AgentStatus(Event::Update(status)))) if status.state == State::NeedsInput)
            );
            assert_eq!(view.events().count(), 0);
        }
        let ending = b"\x1b]133;D;0\x07";
        owner.feed(ending);
        apply(
            &connection.shared,
            ServerMessage::PaneOutput {
                sequence: 4,
                pane_id: 7,
                data: ending.to_vec(),
            },
        )
        .unwrap();
        for view in [&first, &second] {
            let effects = view
                .events()
                .filter_map(|event| match event {
                    SessionEvent::Effect(effect) => Some(effect),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert!(matches!(
                effects.as_slice(),
                [Effect::CommandEnd { .. }, Effect::AgentStatus(Event::End)]
            ));
        }
        apply(&connection.shared, snapshot(&owner, 4)).unwrap();
        assert_eq!(first.events().count(), 0);
        let third = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        assert_eq!(third.events().count(), 0);
    }

    #[test]
    fn native_command_errors_keep_connection_and_transport_errors_keep_live_panes() {
        let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
        let terminal = tmuxx_terminal::Terminal::new(20, 4, 20);
        apply(&connection.shared, ServerMessage::Snapshot {
            instance_id: "owner".into(), sequence: 1,
            state: serde_json::json!({
                "sessions": [{"id":1,"name":"test","active":0,"windows":{"0":2}}],
                "windows": [{"id":2,"name":"shell","active":7,"cols":20,"rows":4,"panes":[7],"layout":Layout::pane(7,20,4).to_legacy().unwrap()}],
                "panes": [{"id":7,"window":2,"cols":20,"rows":4,"status":null,"terminal":terminal.snapshot()}],
            }),
        }).unwrap();
        let view = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        apply(
            &connection.shared,
            ServerMessage::Reply(tmuxx_protocol::native::Response {
                request_id: 1,
                output: Vec::new(),
                status: None,
                error: Some("invalid command".into()),
            }),
        )
        .unwrap();
        assert!(!connection.is_closed());
        assert!(!view.has_exited());
        assert_eq!(connection.errors(), ["invalid command"]);
        fail(&connection.shared, "connection lost");
        assert!(connection.is_closed());
        assert!(!view.has_exited());
        assert!(
            matches!(view.events().next(), Some(SessionEvent::Error(message)) if message == "connection lost")
        );
        fail(&connection.shared, "duplicate worker error");
        assert_eq!(connection.errors(), ["connection lost"]);
    }

    #[test]
    fn streamed_snapshot_restores_large_image_atomically_and_abort_keeps_live_view() {
        let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
        let mut terminal = tmuxx_terminal::Terminal::new(80, 24, 2000);
        terminal.feed(b"\x1b_Ga=T,i=7,f=32,s=1,v=1;/wAA/w==\x1b\\");
        let mut snapshot = terminal.snapshot();
        snapshot.primary_graphics.images[0].width = 4096;
        snapshot.primary_graphics.images[0].height = 1024;
        snapshot.primary_graphics.images[0].pixels = Arc::new(vec![255; 4096 * 1024 * 4]);
        let state = serde_json::json!({
            "sessions": [{"id": 1,"name":"test","active":0,"windows":{"0":2}}],
            "windows": [{"id":2,"name":"shell","active":7,"cols":80,"rows":24,"panes":[7],"layout":Layout::pane(7,80,24).to_legacy().unwrap()}],
            "panes": [{"id":7,"window":2,"cols":80,"rows":24,"status":null,"terminal":snapshot}],
        });
        let mut wire = Vec::new();
        tmuxx_protocol::native::write_snapshot(
            &mut wire,
            "large",
            1,
            &state,
            tmuxx_protocol::native::MAX_SNAPSHOT_BYTES,
        )
        .unwrap();
        assert!(wire.len() > 16 * tmuxx_protocol::native::SNAPSHOT_CHUNK_BYTES);
        let mut reader = BufReader::new(wire.as_slice());
        let mut snapshots = SnapshotAssembler::default();
        let mut completed = 0;
        while let Some(bytes) = record(&mut reader).unwrap() {
            if let Some(message) = snapshots
                .push(serde_json::from_slice(&bytes).unwrap())
                .unwrap()
            {
                apply(&connection.shared, message).unwrap();
                completed += 1;
            } else {
                assert!(connection.graph().is_none());
            }
        }
        snapshots.finish().unwrap();
        assert_eq!(completed, 1);
        let before = connection.shared.model.lock().unwrap().panes[&7]
            .terminal
            .primary_screen()
            .graphics
            .view_snapshot()
            .images[0]
            .pixels
            .clone();
        assert_eq!(before.len(), 16 * 1024 * 1024);
        assert!(
            apply(
                &connection.shared,
                ServerMessage::SnapshotAborted {
                    instance_id: "large".into(),
                    sequence: 2,
                    error: "test limit".into()
                }
            )
            .is_err()
        );
        assert_eq!(connection.shared.model.lock().unwrap().sequence, 1);
        assert!(Arc::ptr_eq(
            &before,
            &connection.shared.model.lock().unwrap().panes[&7]
                .terminal
                .primary_screen()
                .graphics
                .view_snapshot()
                .images[0]
                .pixels
        ));
    }

    #[test]
    fn native_snapshot_preserves_row_ids_and_image_anchors_after_resize() {
        let mut terminal = tmuxx_terminal::Terminal::new(16, 20, 2000);
        terminal.feed(b"abcdefghijklmnopqrstuvwx\r\n\x1b_Ga=T,i=7,f=32,s=1,v=1;/wAA/w==\x1b\\");
        for cols in [8, 16, 8, 16] {
            terminal.resize(cols, 20);
            let snapshot = terminal.snapshot();
            let (restored, _) = restore(&serde_json::to_value(&snapshot).unwrap()).unwrap();
            let ids = restored
                .primary_screen()
                .all_rows()
                .map(|r| r.id)
                .collect::<Vec<_>>();
            assert_eq!(
                ids,
                snapshot
                    .primary
                    .all_rows()
                    .map(|r| r.id)
                    .collect::<Vec<_>>()
            );
            let graphics = restored.primary_screen().graphics.view_snapshot();
            assert_eq!(graphics.placements.len(), 1);
            assert!(ids.contains(&graphics.placements[0].row));
        }
    }

    #[test]
    fn restored_native_views_search_and_select_shell_output_independently() {
        let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
        let mut terminal = tmuxx_terminal::Terminal::new(12, 3, 2000);
        terminal.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07build\r\n\x1b]133;C\x07retained output with wrap\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        terminal.resize(10, 3);
        apply(&connection.shared, ServerMessage::Snapshot { instance_id: "search".into(), sequence: 1, state: serde_json::json!({
            "sessions": [{"id":1,"name":"test","active":0,"windows":{"0":2}}],
            "windows": [{"id":2,"name":"shell","active":7,"cols":10,"rows":3,"panes":[7],"layout":Layout::pane(7,10,3).to_legacy().unwrap()}],
            "panes": [{"id":7,"window":2,"cols":10,"rows":3,"status":null,"terminal":terminal.snapshot()}],
        }) }).unwrap();
        let first = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        let second = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        {
            let mut terminal = first.terminal().unwrap();
            let mut search = rustty_vt::search::TerminalSearch::new(b"output");
            search.run(&mut terminal);
            assert_eq!(search.total_matches(), 1);
            let point = search.match_at(0).unwrap().start;
            terminal.screen_mut().selection = terminal.screen().select_output(point);
            assert_eq!(
                terminal.screen().selection_text().as_deref(),
                Some("retained output with wrap\n")
            );
            assert_eq!(terminal.screen().snapshot_viewport().height(), 3);
        }
        let terminal = second.terminal().unwrap();
        assert!(terminal.screen().selection.is_none());
        assert_eq!(terminal.screen().search_literal(b"output").len(), 1);
    }

    #[test]
    fn native_views_preserve_wide_cells_viewports_sync_modes_and_stationary_cursor_changes() {
        let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
        let mut owner = tmuxx_terminal::Terminal::new(10, 3, 100);
        owner.feed("old one\r\nold two\r\nold three\r\nwide 你\r\nlast".as_bytes());
        let snapshot = |owner: &mut tmuxx_terminal::Terminal, sequence| ServerMessage::Snapshot {
            instance_id: "damage".into(),
            sequence,
            state: serde_json::json!({
                "sessions": [{"id":1,"name":"test","active":0,"windows":{"0":2}}],
                "windows": [{"id":2,"name":"shell","active":7,"cols":10,"rows":3,"panes":[7],"layout":Layout::pane(7,10,3).to_legacy().unwrap()}],
                "panes": [{"id":7,"window":2,"cols":10,"rows":3,"status":null,"terminal":owner.snapshot()}],
            }),
        };
        apply(&connection.shared, snapshot(&mut owner, 1)).unwrap();
        let first = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        let second = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        first.terminal().unwrap().screen_mut().scroll_viewport(2);
        let top = first.snapshot().unwrap().screen.row(0).id;
        let frame = second.snapshot().unwrap();
        assert_eq!(&*frame.screen.cell_text(frame.screen.row(1), 5), "你");
        assert_eq!(frame.screen.row(1).cells[5].width(), 2);
        assert_eq!(frame.screen.row(1).cells[6].width(), 0);
        let cursor = (frame.screen.cursor.row, frame.screen.cursor.col);
        assert!(frame.screen.cursor.visible);
        let mut sequence = 1;
        for (bytes, visible, synchronized) in [
            (b"\x1b[?25l\x1b[?2026h".as_slice(), false, true),
            (b"\x1b[?25h\x1b[?2026l".as_slice(), true, false),
        ] {
            let generation = second.snapshot().unwrap().generation;
            owner.feed(bytes);
            sequence += 1;
            apply(
                &connection.shared,
                ServerMessage::PaneOutput {
                    sequence,
                    pane_id: 7,
                    data: bytes.to_vec(),
                },
            )
            .unwrap();
            let frame = second.snapshot().unwrap();
            assert_eq!((frame.screen.cursor.row, frame.screen.cursor.col), cursor);
            assert_eq!(frame.screen.cursor.visible, visible);
            assert_eq!(second.terminal().unwrap().modes.dec(2026), synchronized);
            assert!(frame.generation > generation);
            sequence += 1;
            apply(&connection.shared, snapshot(&mut owner, sequence)).unwrap();
            assert_eq!(second.terminal().unwrap().modes.dec(2026), synchronized);
            assert_eq!(second.snapshot().unwrap().screen.cursor.visible, visible);
            assert_eq!(first.terminal().unwrap().screen().viewport_offset, 2);
            assert_eq!(first.snapshot().unwrap().screen.row(0).id, top);
            assert_eq!(second.terminal().unwrap().screen().viewport_offset, 0);
        }
        sequence += 1;
        apply(
            &connection.shared,
            ServerMessage::PaneOutput {
                sequence,
                pane_id: 7,
                data: b"\x1b[2;7HX".to_vec(),
            },
        )
        .unwrap();
        let frame = second.snapshot().unwrap();
        assert_eq!(frame.screen.row(1).cells[5].width(), 1);
        assert!(frame.screen.row(1).cells[5].codepoint().is_none());
        assert_eq!(&*frame.screen.cell_text(frame.screen.row(1), 6), "X");
        assert_eq!(first.terminal().unwrap().screen().viewport_offset, 2);
        assert!(connection.errors().is_empty());
    }

    #[test]
    fn native_detach_closes_views_without_signalling_the_gui_parent() {
        for hangup in [false, true] {
            let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
            let terminal = tmuxx_terminal::Terminal::new(80, 24, 2000);
            apply(&connection.shared, ServerMessage::Snapshot { instance_id: "detach".into(), sequence: 1, state: serde_json::json!({
                "sessions": [{"id":1,"name":"test","active":0,"windows":{"0":2}}],
                "windows": [{"id":2,"name":"shell","active":7,"cols":80,"rows":24,"panes":[7],"layout":Layout::pane(7,80,24).to_legacy().unwrap()}],
                "panes": [{"id":7,"window":2,"cols":80,"rows":24,"status":null,"terminal":terminal.snapshot()}],
            }) }).unwrap();
            let first = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
            let second = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
            apply(
                &connection.shared,
                ServerMessage::Detached {
                    session: "test".into(),
                    hangup,
                },
            )
            .unwrap();
            assert!(first.has_exited() && second.has_exited());
            assert!(connection.shared.closed.load(Ordering::Acquire));
            assert!(connection.errors().is_empty());
            assert!(connection.command(["list-sessions"]).is_err());
        }
    }

    #[test]
    fn new_server_incarnation_closes_stale_views_and_resets_event_identity() {
        let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
        let snapshot = |instance: &str, sequence, text: &[u8]| {
            let mut terminal = tmuxx_terminal::Terminal::new(80, 24, 2000);
            terminal.feed(text);
            ServerMessage::Snapshot {
                instance_id: instance.into(),
                sequence,
                state: serde_json::json!({
                    "sessions": [{"id": 1,"name":"test","active":0,"windows":{"0":2}}],
                    "windows": [{"id":2,"name":"shell","active":7,"cols":80,"rows":24,"panes":[7],"layout":Layout::pane(7,80,24).to_legacy().unwrap()}],
                    "panes": [{"id":7,"window":2,"cols":80,"rows":24,"pid":123,"status":null,"terminal":terminal.snapshot()}],
                }),
            }
        };
        apply(&connection.shared, snapshot("old", 50, b"old owner")).unwrap();
        let old = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        {
            let mut terminal = old.terminal().unwrap();
            let row = terminal.screen().row(0).id;
            terminal.screen_mut().selection = Some(rustty_vt::Selection {
                start: rustty_vt::GridPoint { row, col: 0 },
                end: rustty_vt::GridPoint { row, col: 2 },
                rectangular: false,
            });
        }
        apply(
            &connection.shared,
            ServerMessage::HostEffect {
                sequence: 50,
                pane_id: 7,
                event_id: 90,
                action: serde_json::json!("Bell"),
            },
        )
        .unwrap();
        assert_eq!(
            old.events()
                .filter(|event| matches!(event, SessionEvent::Effect(Effect::Bell)))
                .count(),
            1
        );
        apply(&connection.shared, snapshot("new", 1, b"new owner")).unwrap();
        assert!(old.has_exited());
        let new = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        assert!(!new.has_exited());
        assert!(new.terminal().unwrap().screen().selection.is_none());
        assert!(old.terminal().unwrap().screen().selection.is_some());
        apply(
            &connection.shared,
            ServerMessage::HostEffect {
                sequence: 1,
                pane_id: 7,
                event_id: 1,
                action: serde_json::json!("Bell"),
            },
        )
        .unwrap();
        assert_eq!(
            new.events()
                .filter(|event| matches!(event, SessionEvent::Effect(Effect::Bell)))
                .count(),
            1
        );
        assert_eq!(old.events().count(), 0);
        apply(
            &connection.shared,
            ServerMessage::PaneOutput {
                sequence: 2,
                pane_id: 7,
                data: b" current".to_vec(),
            },
        )
        .unwrap();
        for (session, expected) in [(&old, "old owner"), (&new, "new owner current")] {
            let terminal = session.terminal().unwrap();
            assert_eq!(
                terminal
                    .screen()
                    .row_text(terminal.screen().row(0))
                    .trim_end(),
                expected
            );
        }
    }

    #[test]
    fn native_host_actions_are_assigned_once_and_clipboard_replies_use_event_ids() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        let mut terminal = tmuxx_terminal::Terminal::new(80, 24, 2000);
        terminal.feed(b"snapshot text");
        let snapshot = terminal.snapshot();
        let state = serde_json::json!({
            "sessions": [{"id": 1,"name":"test","active":0,"windows":{"0":2}}],
            "windows": [{"id":2,"name":"shell","active":7,"cols":80,"rows":24,"panes":[7],"layout":Layout::pane(7,80,24).to_legacy().unwrap()}],
            "panes": [{"id":7,"window":2,"cols":80,"rows":24,"status":null,"terminal":snapshot}],
        });
        apply(
            &connection.shared,
            ServerMessage::Snapshot {
                instance_id: "one".into(),
                sequence: 1,
                state: state.clone(),
            },
        )
        .unwrap();
        let first = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        let second = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        second
            .set_host_state(true, true, rustty_vt::query::ColorScheme::Dark)
            .unwrap();
        apply(
            &connection.shared,
            ServerMessage::PaneOutput {
                sequence: 2,
                pane_id: 7,
                data: b"\x07".to_vec(),
            },
        )
        .unwrap();
        let bell = ServerMessage::HostEffect {
            sequence: 2,
            pane_id: 7,
            event_id: 11,
            action: serde_json::json!("Bell"),
        };
        apply(&connection.shared, bell.clone()).unwrap();
        apply(&connection.shared, bell).unwrap();
        assert_eq!(
            second
                .events()
                .filter(|e| matches!(e, SessionEvent::Effect(Effect::Bell)))
                .count(),
            1
        );
        assert_eq!(first.events().count(), 0);
        let read = rustty_vt::clipboard::Read::osc52(
            rustty_vt::clipboard::Location::Standard,
            rustty_vt::clipboard::Terminator::Bell,
        );
        apply(
            &connection.shared,
            ServerMessage::HostEffect {
                sequence: 2,
                pane_id: 7,
                event_id: 12,
                action: serde_json::json!({"ClipboardRead":read}),
            },
        )
        .unwrap();
        assert!(matches!(
            second.events().next(),
            Some(SessionEvent::Effect(Effect::ClipboardRead(_)))
        ));
        second
            .reply_clipboard_read(read.clone(), rustty_vt::clipboard::ReadResult::Denied)
            .unwrap();
        assert!(
            second
                .reply_clipboard_read(read, rustty_vt::clipboard::ReadResult::Denied)
                .is_err()
        );
        let reply = requests
            .try_iter()
            .find(|r| r.argv[0] == b"tmuxx-host-reply")
            .unwrap();
        assert_eq!(reply.argv[1], b"12");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&reply.stdin.unwrap()).unwrap(),
            serde_json::json!({"Read":"Denied"})
        );
        apply(
            &connection.shared,
            ServerMessage::Snapshot {
                instance_id: "one".into(),
                sequence: 3,
                state,
            },
        )
        .unwrap();
        assert_eq!(first.events().count(), 0);
        assert_eq!(second.events().count(), 0);
    }

    fn test_owner(script: &str) -> Connection {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let endpoint = std::env::temp_dir().join(format!(
            "rustty-tmuxx-io-{}-{}.native",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        Connection::embedded_with_command(
            &endpoint,
            Arc::new(|| {}),
            ["python3", "-u", "-c", script]
                .map(|s| s.as_bytes().to_vec())
                .to_vec(),
        )
        .unwrap()
    }
    fn wait_text(connection: &Connection, session: &Session, marker: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let errors = connection.errors();
            assert!(errors.is_empty(), "{errors:?}");
            let text = session.terminal().unwrap().plain_text().replace('\n', "");
            if text.contains(marker) {
                return text;
            }
            assert!(Instant::now() < deadline, "missing {marker}: {text:?}");
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn native_focus_is_aggregated_across_views_and_connections_and_gated_by_server_option() {
        let script = r#"import os,tty
 tty.setraw(0)
 os.write(1,b'\x1b[?1004hREADY\r\n')
 def take(n,label):
  data=b''
  while len(data)<n: data+=os.read(0,n-len(data))
  os.write(1,label+b':'+data.hex().encode()+b'\r\n')
 take(3,b'FOCUS')
 take(1,b'DUP')
 take(3,b'BLUR')
 take(1,b'END')
 os.read(0,1)
 "#
        .lines()
        .map(|line| line.strip_prefix(' ').unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
        let connection = test_owner(&script);
        let graph = wait(&connection, |g| !g.panes.is_empty());
        let pane = graph.panes[0].id;
        let first = Session::attach_tmuxx(&connection, pane, Arc::new(|| {})).unwrap();
        let second = Session::attach_tmuxx(&connection, pane, Arc::new(|| {})).unwrap();
        wait_text(&connection, &first, "READY");
        connection
            .command(["set-option", "-s", "focus-events", "on"])
            .unwrap();
        first
            .set_host_state(true, true, rustty_vt::query::ColorScheme::Dark)
            .unwrap();
        wait_text(&connection, &first, "FOCUS:1b5b49");
        first
            .set_host_state(true, true, rustty_vt::query::ColorScheme::Dark)
            .unwrap();
        second
            .set_host_state(true, true, rustty_vt::query::ColorScheme::Light)
            .unwrap();
        first
            .set_host_state(false, false, rustty_vt::query::ColorScheme::Dark)
            .unwrap();
        let Source::Unix(endpoint) = &connection.source else {
            panic!("missing endpoint")
        };
        let remote = Connection::unix(endpoint, Arc::new(|| {})).unwrap();
        wait(&remote, |g| g.panes.iter().any(|p| p.id == pane));
        let third = Session::attach_tmuxx(&remote, pane, Arc::new(|| {})).unwrap();
        third
            .set_host_state(true, true, rustty_vt::query::ColorScheme::Dark)
            .unwrap();
        third.write_input(&Input::Reply(b"d".to_vec())).unwrap();
        wait_text(&connection, &first, "DUP:64");
        second
            .set_host_state(false, false, rustty_vt::query::ColorScheme::Light)
            .unwrap();
        drop(third);
        drop(remote);
        wait_text(&connection, &first, "BLUR:1b5b4f");
        first.write(b"e").unwrap();
        wait_text(&connection, &first, "END:65");

        let gated = test_owner(
            "import os,tty; tty.setraw(0); os.write(1,b'\\x1b[?1004hREADY\\r\\n'); data=os.read(0,1); os.write(1,b'GATED:'+data.hex().encode()+b'\\r\\n'); os.read(0,1)",
        );
        let graph = wait(&gated, |g| !g.panes.is_empty());
        let session = Session::attach_tmuxx(&gated, graph.panes[0].id, Arc::new(|| {})).unwrap();
        wait_text(&gated, &session, "READY");
        session
            .set_host_state(true, true, rustty_vt::query::ColorScheme::Dark)
            .unwrap();
        session
            .set_host_state(true, false, rustty_vt::query::ColorScheme::Light)
            .unwrap();
        session.write(b"g").unwrap();
        wait_text(&gated, &session, "GATED:67");
    }

    #[test]
    fn embedded_size_clients_use_the_owner_and_release_their_size_context() {
        let connection = test_owner("import time; time.sleep(30)");
        let graph = wait(&connection, |g| !g.panes.is_empty());
        let session = graph.sessions[0].id;
        let pid = graph.panes[0].pid;
        let rival = Connection::local(
            connection.owner.as_ref().unwrap().connect().unwrap(),
            Arc::new(|| {}),
            false,
        )
        .unwrap();
        rival
            .command(["attach-session".into(), "-t".into(), format!("${session}")])
            .unwrap();
        rival.command(["refresh-client", "-C", "100,40"]).unwrap();
        connection
            .command(["set-option", "-w", "window-size", "smallest"])
            .unwrap();
        let Source::Unix(endpoint) = &connection.source else {
            panic!("missing endpoint");
        };
        std::fs::remove_file(endpoint).unwrap();
        connection.view_size(session, 60, 20, 8, 16).unwrap();
        wait(&connection, |g| {
            g.windows[0].cols == 60 && g.windows[0].rows == 20
        });
        connection.release_view(session).unwrap();
        let graph = wait(&connection, |g| {
            g.windows[0].cols == 100 && g.windows[0].rows == 40
        });
        assert_eq!(graph.panes[0].pid, pid);
        assert!(connection.size_views.lock().unwrap().is_empty());
        connection.release_view(session).unwrap();
        connection.view_size(session, 50, 10, 8, 16).unwrap();
        wait(&connection, |g| {
            g.windows[0].cols == 50 && g.windows[0].rows == 10
        });
    }

    #[test]
    fn native_exit_and_respawn_replace_process_state_and_selection() {
        let connection = test_owner(
            "import os,tty,sys; tty.setraw(0); os.write(1,b'\\x1b]777;rustty-agent;1;eyJvcCI6ImJlZ2luIiwic3RhdGUiOiJuZWVkc19pbnB1dCIsInRocmVhZF9pZCI6InJlbW90ZSJ9\\x07first-ready\\r\\n'); os.read(0,1); sys.exit(17)",
        );
        let graph = wait(&connection, |g| !g.panes.is_empty());
        let pane = graph.panes[0].id;
        let session = Session::attach_tmuxx(&connection, pane, Arc::new(|| {})).unwrap();
        wait_text(&connection, &session, "first-ready");
        assert!(session.events().any(|event| matches!(
            event,
            SessionEvent::Effect(Effect::AgentStatus(rustty_vt::agent::Event::Begin(_)))
        )));
        connection
            .command(["set-option", "-w", "remain-on-exit", "on"])
            .unwrap();
        let old_pid = wait(&connection, |g| g.panes[0].pid.is_some()).panes[0].pid;
        {
            let mut terminal = session.terminal().unwrap();
            let row = terminal.screen().row(0).id;
            terminal.screen_mut().selection = Some(rustty_vt::Selection {
                start: rustty_vt::GridPoint { row, col: 0 },
                end: rustty_vt::GridPoint { row, col: 1 },
                rectangular: false,
            });
        }
        session.write(b"x").unwrap();
        let graph = wait(&connection, |g| g.panes[0].status == Some(17));
        assert!(session.has_exited());
        let terminal: tmuxx_terminal::Snapshot =
            serde_json::from_value(graph.panes[0].terminal.clone()).unwrap();
        assert!(terminal.agent_status.snapshot.is_none());
        assert!(session.events().any(|event| matches!(
            event,
            SessionEvent::Effect(Effect::AgentStatus(rustty_vt::agent::Event::End))
        )));
        connection.command(["respawn-pane", "-k", "-t", &format!("%{pane}"), "python3", "-u", "-c", "import os,tty; tty.setraw(0); os.write(1,b'second-ready\\r\\n'); data=os.read(0,1); os.write(1,b'RECEIVED:'+data.hex().encode()+b'\\r\\n'); os.read(0,1)"]).unwrap();
        wait_text(&connection, &session, "second-ready");
        let graph = wait(&connection, |g| {
            g.panes[0].pid.is_some() && g.panes[0].pid != old_pid
        });
        assert_eq!(graph.panes[0].id, pane);
        assert!(!session.has_exited());
        assert!(session.terminal().unwrap().screen().selection.is_none());
        session.write(b"z").unwrap();
        wait_text(&connection, &session, "RECEIVED:7a");
        connection.command(["split-window", "sleep 60"]).unwrap();
        wait(&connection, |g| g.panes.len() == 2);
        connection
            .command(["kill-pane", "-t", &format!("%{pane}")])
            .unwrap();
        wait(&connection, |g| g.panes.iter().all(|p| p.id != pane));
        assert!(session.has_exited());
        drop(session);
    }
    #[test]
    fn native_typed_keys_and_paste_follow_each_pane_modes_without_broadcasting_mouse() {
        let program = |first: bool| {
            format!(
                r#"import os,tty
 tty.setraw(0)
 os.write(1,{}+b'READY\r\n')
 def take(n,label):
  data=b''
  while len(data)<n: data+=os.read(0,n-len(data))
  os.write(1,label+b':'+data.hex().encode()+b'\r\n')
 take(3,b'KEY')
 take({},b'PASTE')
 {}
 take(1,b'TEXT')
 os.read(0,1)
 "#,
                if first {
                    "b'\\x1b[?1h\\x1b[?2004h\\x1b[?1000h\\x1b[?1006h'"
                } else {
                    "b''"
                },
                if first { 15 } else { 3 },
                if first {
                    "take(9,b'MOUSE')\n take(1,b'REPLY')"
                } else {
                    ""
                }
            )
            .lines()
            .map(|line| line.strip_prefix(' ').unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n")
        };
        let connection = test_owner(&program(true));
        let graph = wait(&connection, |g| !g.panes.is_empty());
        let pid = graph.panes[0].id;
        let first = Session::attach_tmuxx(&connection, pid, Arc::new(|| {})).unwrap();
        wait_text(&connection, &first, "READY");
        connection
            .command(["split-window", "-h", "python3", "-u", "-c", &program(false)])
            .unwrap();
        let graph = wait(&connection, |g| g.panes.len() == 2);
        let other = graph.panes.iter().find(|p| p.id != pid).unwrap().id;
        let second = Session::attach_tmuxx(&connection, other, Arc::new(|| {})).unwrap();
        wait_text(&connection, &second, "READY");
        connection
            .command(["set-option", "-w", "synchronize-panes", "on"])
            .unwrap();
        first
            .write_input(&Input::Key {
                event: rustty_vt::KeyEvent::new(rustty_vt::Key::Up),
                options: Default::default(),
            })
            .unwrap();
        wait_text(&connection, &first, "KEY:1b4f41");
        wait_text(&connection, &second, "KEY:1b5b41");
        first.write_input(&Input::Paste(b"abc".to_vec())).unwrap();
        wait_text(&connection, &first, "PASTE:1b5b3230307e6162631b5b3230317e");
        wait_text(&connection, &second, "PASTE:616263");
        let mouse = first.terminal().unwrap().encode_mouse(
            rustty_vt::MouseEvent {
                action: rustty_vt::MouseAction::Press,
                button: Some(rustty_vt::MouseButton::Left),
                x: 20.0,
                y: 10.0,
                modifiers: Default::default(),
            },
            rustty_vt::MouseEncodeOptions {
                screen_size: [800.0, 240.0],
                cell_size: [10, 10],
                padding: [0.0; 4],
                any_button_pressed: true,
                last_cell: None,
            },
        );
        assert_eq!(mouse, b"\x1b[<0;3;2M");
        first.write_input(&Input::Mouse(mouse)).unwrap();
        wait_text(&connection, &first, "MOUSE:1b5b3c303b333b324d");
        first.write_input(&Input::Reply(b"r".to_vec())).unwrap();
        wait_text(&connection, &first, "REPLY:72");
        first.write(b"z").unwrap();
        wait_text(&connection, &first, "TEXT:7a");
        wait_text(&connection, &second, "TEXT:7a");
    }

    #[test]
    fn embedded_and_socket_views_share_panes_but_keep_selection_local() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let endpoint = std::env::temp_dir().join(format!(
            "rustty-tmuxx-test-{}-{}.native",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let local = Connection::embedded(&endpoint, Arc::new(|| {})).unwrap();
        let graph = wait(&local, |g| !g.panes.is_empty());
        let pid = graph.panes[0].id;
        let first = Session::attach_tmuxx(&local, pid, Arc::new(|| {})).unwrap();
        let second = Session::attach_tmuxx(&local, pid, Arc::new(|| {})).unwrap();
        {
            let mut terminal = first.terminal().unwrap();
            let row = terminal.screen().row(0).id;
            terminal.screen_mut().selection = Some(rustty_vt::Selection {
                start: rustty_vt::GridPoint { row, col: 0 },
                end: rustty_vt::GridPoint { row, col: 1 },
                rectangular: false,
            });
        }
        assert!(second.terminal().unwrap().screen().selection.is_none());
        first.write(b"printf 'native-mirror-test\\n'\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let found = {
                let t = second.terminal().unwrap();
                t.screen()
                    .all_rows()
                    .any(|r| t.screen().row_text(r).contains("native-mirror-test"))
            };
            if found {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        let remote = Connection::unix(&endpoint, Arc::new(|| {})).unwrap();
        let graph = wait(&remote, |g| g.panes.iter().any(|p| p.id == pid));
        assert_eq!(graph.panes[0].id, pid);
        let third = Session::attach_tmuxx(&remote, pid, Arc::new(|| {})).unwrap();
        {
            let t = third.terminal().unwrap();
            assert!(
                t.screen()
                    .all_rows()
                    .any(|r| t.screen().row_text(r).contains("native-mirror-test"))
            );
        }
        drop(first);
        drop(second);
        drop(third);
        drop(remote);
        local.command(["split-window", "-h"]).unwrap();
        wait(&local, |g| g.panes.len() == 2);
        drop(local);
        assert!(!endpoint.exists());
    }
}

fn validate_graph(graph: &Graph) -> io::Result<()> {
    use std::collections::BTreeSet;
    let unique = |ids: Vec<u64>| ids.iter().collect::<BTreeSet<_>>().len() == ids.len();
    if graph.panes.len() > 10_000
        || !unique(graph.panes.iter().map(|p| p.id).collect())
        || !unique(graph.windows.iter().map(|w| w.id).collect())
        || !unique(graph.sessions.iter().map(|s| s.id).collect())
    {
        return Err(err("invalid tmuxx object IDs"));
    }
    for window in &graph.windows {
        let layout = Layout::parse(&window.layout).map_err(err)?;
        layout.validate_tiled().map_err(err)?;
        let ids = layout
            .panes()
            .iter()
            .map(|p| p.id.map(u64::from))
            .collect::<Option<BTreeSet<_>>>()
            .ok_or_else(|| err("layout pane has no ID"))?;
        if window.panes.iter().copied().collect::<BTreeSet<_>>() != ids
            || !ids.contains(&window.active)
            || window.panes.iter().any(|id| {
                !graph
                    .panes
                    .iter()
                    .any(|p| p.id == *id && p.window == window.id)
            })
        {
            return Err(err("inconsistent tmuxx pane graph"));
        }
    }
    for session in &graph.sessions {
        if !session.windows.contains_key(&(session.active as u32))
            || session
                .windows
                .values()
                .any(|id| !graph.windows.iter().any(|w| w.id == *id))
        {
            return Err(err("inconsistent tmuxx session graph"));
        }
    }
    Ok(())
}
