//! Native tmuxx connections and independent Rustty pane views.
//!
//! The server owns PTYs and application replies. Views own selection, scroll,
//! and rendering. Closing a view never kills its server-side pane.
use crate::config::Config;
use crate::session::{Input, SessionEvent, apply_appearance};
use rustty_vt::{Effect, Terminal};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread;
use std::time::Duration;
pub use tmuxx_layout::{Axis, Kind, Layout};
pub use tmuxx_protocol::native::ServerStatus;
use tmuxx_protocol::native::{Hello, Request, ServerMessage, SnapshotAssembler};
pub use tmuxx_runtime::Options as ServerOptions;
#[cfg(windows)]
pub use tmuxx_runtime::initialize_conpty;
#[cfg(windows)]
pub use tmuxx_runtime::windows::endpoint as windows_endpoint;

mod ssh;
mod system;

const RECORD_LIMIT: usize = 128 * 1024 * 1024;

pub use tmuxx_protocol::native::{
    SnapshotPane as ServerPane, SnapshotSession as ServerSession, SnapshotState as Graph,
    SnapshotWindow as ServerWindow,
};
#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub id: u64,
    pub name: String,
    pub windows: Vec<String>,
}

#[derive(Default)]
struct Model {
    instance: String,
    view_size_ownership: bool,
    selection_updates: bool,
    sequence: u64,
    graph: Option<Graph>,
    server_status: Option<ServerStatus>,
    panes: BTreeMap<u64, PaneState>,
    errors: Vec<String>,
    last_host_event: Option<u64>,
    refresh_pending: bool,
    pending_focus: Vec<PendingFocus>,
    pending_replies: BTreeMap<u64, mpsc::Sender<io::Result<Vec<u8>>>>,
}
struct PendingFocus {
    request_id: u64,
    session: u64,
    index: u32,
    window: u64,
    pane: u64,
    acknowledged: bool,
}
impl PendingFocus {
    fn valid(&self, graph: &Graph) -> bool {
        graph.sessions.iter().any(|session| {
            session.id == self.session && session.windows.get(&self.index) == Some(&self.window)
        }) && graph
            .windows
            .iter()
            .any(|window| window.id == self.window && window.panes.contains(&self.pane))
    }
}
impl Model {
    fn request_refresh(&mut self, shared: &Shared) -> io::Result<()> {
        // Snapshots follow preceding change notices on the same ordered stream.
        if !self.refresh_pending {
            send(shared, Vec::new())?;
            self.refresh_pending = true;
        }
        Ok(())
    }
    fn fail_replies(&mut self, message: &str) {
        for reply in std::mem::take(&mut self.pending_replies).into_values() {
            let _ = reply.send(Err(err(message)));
        }
    }
    fn release_terminals(&mut self) {
        self.panes.clear();
        self.server_status = None;
        if let Some(graph) = &mut self.graph {
            // Keep session identity for disconnected windows and saved attachments.
            for pane in &mut graph.panes {
                pane.terminal = None;
            }
        }
    }
    fn focus_reply(&mut self, request_id: u64, failed: bool) {
        self.pending_focus.retain_mut(|focus| {
            if focus.request_id == request_id {
                focus.acknowledged = true;
                !failed
            } else {
                true
            }
        });
    }
    fn focus_snapshot(&mut self, graph: &Graph) {
        self.pending_focus
            .retain(|focus| !focus.acknowledged && focus.valid(graph));
    }
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
    appearance: Arc<Mutex<Option<Config>>>,
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
    #[cfg(windows)]
    Windows(PathBuf),
    Ssh {
        host: String,
        command: String,
        environment: BTreeMap<String, String>,
    },
    SystemSsh {
        host: String,
        environment: BTreeMap<String, String>,
    },
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
    #[cfg(windows)]
    socket: Option<tmuxx_runtime::windows::PipeStream>,
}

#[derive(Clone)]
pub struct PaneInput {
    shared: Weak<Shared>,
    appearance: Arc<Mutex<Option<Config>>>,
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
    fn connection(&self) -> io::Result<Arc<Shared>> {
        self.shared
            .upgrade()
            .filter(|shared| {
                !self.released.load(Ordering::Acquire) && !shared.closed.load(Ordering::Acquire)
            })
            .ok_or_else(|| io::ErrorKind::BrokenPipe.into())
    }
    pub(crate) fn accepts_input(&self) -> bool {
        self.connection().is_ok()
    }
    pub(crate) fn current_path(&self) -> Option<String> {
        let shared = self.connection().ok()?;
        let model = shared.model.lock().ok()?;
        model
            .graph
            .as_ref()?
            .panes
            .iter()
            .find(|pane| pane.id == self.pane)?
            .current_path
            .clone()
    }
    pub(crate) fn set_appearance(&self, config: &Config) -> io::Result<()> {
        *self.appearance.lock().map_err(err)? = Some(config.clone());
        Ok(())
    }
    pub fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        self.focused.store(false, Ordering::Release);
        self.visible.store(false, Ordering::Release);
        let Some(shared) = self.shared.upgrade() else {
            self.owners.fetch_sub(1, Ordering::AcqRel);
            return;
        };
        let Ok(mut model) = shared.model.lock() else {
            return;
        };
        let last = self.owners.fetch_sub(1, Ordering::AcqRel) == 1;
        let Some(pane) = model.panes.get_mut(&self.pane) else {
            return;
        };
        if !self.native || !Arc::ptr_eq(&pane.owners, &self.owners) {
            return;
        }
        if last {
            // Release is idempotent after kill-pane; a preceding host-state is not.
            pane.host_state = None;
            let _ = send_with_snapshot(
                &shared,
                vec![
                    b"tmuxx-release-pane".to_vec(),
                    b"-t".to_vec(),
                    format!("%{}", self.pane).into_bytes(),
                ],
                false,
            );
        } else {
            drop(model);
            let _ = self.set_host_state(false, false, self.dark.load(Ordering::Acquire));
        }
    }
    pub fn set_persistent(&self, persistent: bool) -> io::Result<()> {
        if !self.native {
            return Err(err("retention leases require a native tmuxx server"));
        }
        let shared = self.connection()?;
        send_with_snapshot(
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
            false,
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
        // Disconnected views keep their snapshots, but have no host to notify.
        let Some(shared) = self.shared.upgrade() else {
            return Ok(());
        };
        if shared.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut model = shared.model.lock().map_err(err)?;
        let Some(pane) = model.panes.get_mut(&self.pane) else {
            return Ok(());
        };
        if !Arc::ptr_eq(&pane.owners, &self.owners) || pane.owners.load(Ordering::Acquire) == 0 {
            return Ok(());
        }
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
            subscribe: false, control: false,
        }).map_err(err)?;
        pane.host_state = Some((focused, visible, dark));
        Ok(())
    }
    pub fn host_reply(&self, effect: Effect, response: HostResponse) -> io::Result<()> {
        let shared = self.connection()?;
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
        let shared = self.connection()?;
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
                // Subscription persists; requesting it again transfers every pane's snapshot.
                subscribe: false,
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
        let shared = self.connection()?;
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
        let shared = self.connection()?;
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
    send_with_snapshot(shared, argv, true)
}
fn send_with_snapshot(shared: &Shared, argv: Vec<Vec<u8>>, snapshot: bool) -> io::Result<u64> {
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
            subscribe: shared.subscribe && snapshot,
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
                #[cfg(any(unix, windows))]
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
        Self::embedded_with_options(ServerOptions::new(endpoint), wake, initial)
    }
    /// Start a private owner, loading its configuration before the first session.
    pub fn embedded_with_options(
        options: ServerOptions,
        wake: Arc<dyn Fn() + Send + Sync>,
        initial: Vec<Vec<u8>>,
    ) -> io::Result<Self> {
        let endpoint = options.endpoint.clone();
        let owner = tmuxx_runtime::start(options)?;
        let client = owner.connect_with_context(tmuxx_runtime::ClientContext {
            host_effects: true,
            selection_updates: true,
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
        #[cfg(windows)]
        {
            connection.source = Source::Windows(endpoint.to_owned());
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
        {
            let mut model = connection.shared.model.lock().map_err(err)?;
            model.view_size_ownership = true;
            model.selection_updates = true;
        }
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
    /// Attach an existing local owner without starting an owner or a pane.
    #[cfg(any(unix, windows))]
    pub fn native(endpoint: &Path, wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        #[cfg(unix)]
        let source = Source::Unix(endpoint.to_owned());
        #[cfg(windows)]
        let source = Source::Windows(endpoint.to_owned());
        Self::connect_source(source, wake, true, true, None)
    }
    /// Send commands to a known owner without retaining or refreshing its pane snapshots.
    #[cfg(any(unix, windows))]
    pub fn native_commands(
        endpoint: &Path,
        expected_instance: &str,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> io::Result<Self> {
        #[cfg(unix)]
        let source = Source::Unix(endpoint.to_owned());
        #[cfg(windows)]
        let source = Source::Windows(endpoint.to_owned());
        Self::connect_source(
            source,
            wake,
            false,
            false,
            Some(expected_instance.to_owned()),
        )
    }
    #[cfg(unix)]
    pub fn unix(endpoint: &Path, wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        Self::native(endpoint, wake)
    }
    #[cfg(windows)]
    pub fn windows(instance: &str, wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        Self::native(&windows_endpoint(instance)?, wake)
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
        Self::ssh_command_with_env(host, remote_command, &BTreeMap::new(), wake)
    }
    /// Apply application environment overrides to SSH, including agent/askpass settings.
    pub fn ssh_command_with_env(
        host: &str,
        remote_command: &str,
        environment: &BTreeMap<String, String>,
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
                environment: environment.clone(),
            },
            wake,
            true,
            true,
            None,
        )
    }
    fn connect_source(
        source: Source,
        wake: Arc<dyn Fn() + Send + Sync>,
        host_effects: bool,
        subscribe: bool,
        expected_instance: Option<String>,
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
                    expected_instance,
                )?;
                connection.socket = Some(stream);
                connection
            }
            #[cfg(windows)]
            Source::Windows(endpoint) => {
                let stream = tmuxx_runtime::windows::connect(endpoint)?;
                let mut connection = Self::stream_inner(
                    stream.try_clone()?,
                    stream.try_clone()?,
                    wake,
                    host_effects,
                    subscribe,
                    expected_instance,
                )?;
                connection.socket = Some(stream);
                connection
            }
            Source::Ssh {
                host,
                command,
                environment,
            } => {
                let mut child = ssh::command(host, environment)
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
                    expected_instance,
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
            Source::SystemSsh { host, environment } => {
                return Self::system_ssh_with_env(host, environment, wake);
            }
            Source::None => return Err(err("stream caller must provide a size client")),
        };
        connection.source = source;
        Ok(connection)
    }
    /// Register this window's size. The first native view owns automatic sizing
    /// until it leaves or another view explicitly fits the session.
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
        self.update_view_size(session, (cols, rows, cell_width, cell_height), false)
    }
    /// Use this window's last reported size for the session and take resize ownership.
    /// With aggressive-resize enabled, inactive tabs adopt the size when selected.
    /// Older native servers apply a fixed size until resized again explicitly.
    pub fn fit_view_size(&self, session: u64) -> io::Result<()> {
        if self.system.is_some() {
            return Err(err("fitting requires a native tmuxx connection"));
        }
        let size = self
            .size_views
            .lock()
            .map_err(err)?
            .get(&session)
            .map(|(_, size)| *size)
            .ok_or_else(|| err("tmuxx window has not reported its size yet"))?;
        self.update_view_size(session, size, true)
    }
    fn update_view_size(
        &self,
        session: u64,
        dimensions: (u16, u16, u16, u16),
        force: bool,
    ) -> io::Result<()> {
        let (cols, rows, cell_width, cell_height) = dimensions;
        if [cols, rows, cell_width, cell_height]
            .iter()
            .any(|value| !(1..=10_000).contains(value))
        {
            return Err(err("invalid tmuxx view size"));
        }
        if self.is_closed() {
            return Err(err("tmuxx connection is closed"));
        }
        let mut views = self.size_views.lock().map_err(err)?;
        if let std::collections::btree_map::Entry::Vacant(entry) = views.entry(session) {
            if force {
                return Err(err("tmuxx window has not reported its size yet"));
            }
            let connection = if let Some(owner) = &self.owner {
                Self::local(
                    owner.connect_with_context(tmuxx_runtime::ClientContext::default())?,
                    self.shared.wake.clone(),
                    false,
                )?
            } else {
                let instance = self.instance();
                if instance.is_empty() {
                    return Err(err("tmuxx size client requires an initial snapshot"));
                }
                Self::connect_source(
                    self.source.clone(),
                    self.shared.wake.clone(),
                    false,
                    false,
                    Some(instance),
                )?
            };
            connection.command([
                "attach-session".into(),
                "-f".into(),
                "no-output".into(),
                "-t".into(),
                format!("${session}"),
            ])?;
            entry.insert((connection, (0, 0, 0, 0)));
        }
        let (connection, size) = views.get_mut(&session).unwrap();
        let errors = connection.errors();
        if !errors.is_empty() {
            return Err(err(errors.join("; ")));
        }
        if connection.is_closed() {
            return Err(err("tmuxx size connection is closed"));
        }
        if force || *size != dimensions {
            if self.shared.model.lock().map_err(err)?.view_size_ownership {
                let mut args = vec![
                    "tmuxx-view-size".to_owned(),
                    "-t".into(),
                    format!("${session}"),
                ];
                if force {
                    args.push("-f".into());
                }
                args.extend([cols, rows, cell_width, cell_height].map(|value| value.to_string()));
                connection.command(args)?;
            } else if force {
                let graph = self
                    .graph()
                    .ok_or_else(|| err("missing tmuxx session graph"))?;
                let session = graph
                    .sessions
                    .iter()
                    .find(|entry| entry.id == session)
                    .ok_or_else(|| err("tmuxx session no longer exists"))?;
                for window in session.windows.values() {
                    connection.command([
                        "resize-window".into(),
                        "-t".into(),
                        format!("${}:@{window}", session.id),
                        "-x".into(),
                        cols.to_string(),
                        "-y".into(),
                        rows.to_string(),
                    ])?;
                }
            } else {
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
            }
            *size = dimensions;
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
        Self::stream_inner(reader, writer, wake, true, true, None)
    }
    fn stream_inner(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        wake: Arc<dyn Fn() + Send + Sync>,
        host_effects: bool,
        subscribe: bool,
        expected_instance: Option<String>,
    ) -> io::Result<Self> {
        let (connection, requests) = Self::base(wake, subscribe);
        let mut writer = io::BufWriter::new(writer);
        write_record(
            &mut writer,
            &Hello {
                host_effects,
                selection_updates: true,
                ..Default::default()
            },
        )?;
        let shared = connection.shared.clone();
        thread::Builder::new()
            .name("rustty-tmuxx-read".into())
            .spawn(move || {
                let result = (|| -> io::Result<()> {
                    let mut reader = BufReader::new(reader);
                    let hello: Hello = tmuxx_protocol::native::decode(
                        &record(&mut reader)?.ok_or_else(|| err("missing tmuxx hello"))?,
                    )
                    .map_err(err)?;
                    if hello.version != tmuxx_protocol::native::VERSION {
                        return Err(err("unsupported tmuxx native version"));
                    }
                    {
                        let mut model = shared.model.lock().map_err(err)?;
                        model.view_size_ownership = hello.view_size_ownership;
                        model.selection_updates = hello.selection_updates;
                    }
                    let mut snapshots = SnapshotAssembler::default();
                    let mut instance = hello.instance_id;
                    if let Some(expected) = expected_instance {
                        let actual = instance
                            .take()
                            .ok_or_else(|| err("missing tmuxx server incarnation"))?;
                        if actual != expected {
                            return Err(err(
                                "tmuxx server incarnation changed; command was not applied",
                            ));
                        }
                        instance = Some(actual);
                    }
                    if let Some(instance) = instance {
                        if instance.is_empty() {
                            return Err(err("empty tmuxx server incarnation"));
                        }
                        let mut model = shared.model.lock().map_err(err)?;
                        if shared.closed.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        model.instance = instance;
                        (shared.wake)();
                    }
                    // No queued command may reach a peer before negotiation succeeds.
                    let shared_writer = shared.clone();
                    thread::Builder::new()
                        .name("rustty-tmuxx-write".into())
                        .spawn(move || {
                            while !shared_writer.closed.load(Ordering::Acquire) {
                                match requests.recv_timeout(Duration::from_millis(50)) {
                                    Ok(request) => {
                                        if let Err(error) = tmuxx_protocol::native::write_request(
                                            &mut writer,
                                            &request,
                                        )
                                        .and_then(|()| writer.flush())
                                        {
                                            fail(&shared_writer, error);
                                            return;
                                        }
                                    }
                                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                                }
                            }
                        })?;
                    while !shared.closed.load(Ordering::Acquire) {
                        let Some(bytes) = record(&mut reader)? else {
                            snapshots.finish()?;
                            return Err(err("tmuxx disconnected"));
                        };
                        if let Some(message) =
                            snapshots.push(tmuxx_protocol::native::decode(&bytes).map_err(err)?)?
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
    /// Receive this native command's output or error without consuming other command errors.
    pub fn command_with_reply(
        &self,
        argv: Vec<Vec<u8>>,
    ) -> io::Result<mpsc::Receiver<io::Result<Vec<u8>>>> {
        if self.system.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "command replies require a native tmuxx connection",
            ));
        }
        let (reply, receiver) = mpsc::channel();
        // Hold the model lock until registration so a fast reply cannot arrive first.
        let mut model = self.shared.model.lock().map_err(err)?;
        let request_id = send(&self.shared, argv)?;
        model.pending_replies.insert(request_id, reply);
        Ok(receiver)
    }
    /// Watch native server connection counts without requesting pane snapshots.
    pub fn watch_server(&self) -> io::Result<()> {
        if self.system.is_some() {
            return Ok(());
        }
        let mut model = self.shared.model.lock().map_err(err)?;
        if self.is_closed() {
            return Err(err("tmuxx connection is closed"));
        }
        let request_id = self.shared.next.fetch_add(1, Ordering::Relaxed);
        self.shared
            .requests
            .try_send(Request {
                request_id,
                argv: vec![b"tmuxx-server-status".to_vec()],
                stdin: None,
                subscribe: false,
                control: false,
            })
            .map_err(err)?;
        // Older owners reject the optional watch; consume only this command's reply.
        let (reply, _) = mpsc::channel();
        model.pending_replies.insert(request_id, reply);
        Ok(())
    }
    pub fn server_status(&self) -> Option<ServerStatus> {
        self.shared.model.lock().ok()?.server_status.clone()
    }
    /// Reload the native owner's startup configuration files in their original order.
    pub fn reload_configuration(&self) -> io::Result<u64> {
        let files = self
            .server_status()
            .and_then(|status| status.config_files)
            .ok_or_else(|| err("The tmuxx server's configuration files are unavailable"))?;
        if files.is_empty() {
            return Err(err("The tmuxx server started without configuration files"));
        }
        if self.is_closed() {
            return Err(err("tmuxx connection is closed"));
        }
        let request_id = self.shared.next.fetch_add(1, Ordering::Relaxed);
        self.shared
            .requests
            .try_send(Request {
                request_id,
                argv: tmuxx_runtime::command_argv(&[tmuxx_command::Command::new(
                    [b"source-file".to_vec(), b"--".to_vec()]
                        .into_iter()
                        .chain(files.into_iter().map(String::into_bytes))
                        .collect(),
                )]),
                stdin: None,
                subscribe: self.shared.subscribe,
                // Native diagnostics must reach Reply even for a management observer.
                control: false,
            })
            .map_err(err)?;
        Ok(request_id)
    }
    /// Live transports used by this view, including its separate sizing clients.
    pub fn view_connection_count(&self) -> Option<usize> {
        let instance = self.shared.model.lock().ok()?.instance.clone();
        if instance.is_empty() {
            return None;
        }
        let mut count = usize::from(!self.is_closed());
        for (connection, _) in self.size_views.lock().ok()?.values() {
            if connection.is_closed() {
                continue;
            }
            // In-process sizing clients are bound to our held owner without a Hello.
            if self.owner.is_none() {
                let child_instance = connection.shared.model.lock().ok()?.instance.clone();
                if child_instance.is_empty() {
                    return None;
                }
                if child_instance != instance {
                    continue;
                }
            }
            count += 1;
        }
        Some(count)
    }
    /// Keep a local focus choice until its command and authoritative selection arrive.
    pub fn focus_pane(&self, session: u64, index: u32, pane: u64) -> io::Result<()> {
        let mut model = self.shared.model.lock().map_err(err)?;
        let graph = model
            .graph
            .as_ref()
            .ok_or_else(|| err("missing tmuxx graph"))?;
        let window = graph
            .sessions
            .iter()
            .find(|s| s.id == session)
            .and_then(|s| s.windows.get(&index))
            .copied()
            .ok_or_else(|| err("tmuxx focus window is unavailable"))?;
        let mut focus = PendingFocus {
            request_id: 0,
            session,
            index,
            window,
            pane,
            acknowledged: false,
        };
        if !focus.valid(graph) {
            return Err(err("tmuxx focus pane is unavailable"));
        }
        focus.request_id = send_with_snapshot(
            &self.shared,
            vec![
                b"select-window".to_vec(),
                b"-t".to_vec(),
                format!("${session}:{index}").into_bytes(),
                b";".to_vec(),
                b"select-pane".to_vec(),
                b"-t".to_vec(),
                format!("%{pane}").into_bytes(),
            ],
            !model.selection_updates,
        )?;
        model.pending_focus.retain(|focus| focus.session != session);
        // Linked windows share pane focus: later intents must be projected last.
        model.pending_focus.push(focus);
        Ok(())
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
        let model = self.shared.model.lock().ok()?;
        let mut graph = model.graph.clone()?;
        for focus in &model.pending_focus {
            if !focus.valid(&graph) {
                continue;
            }
            graph
                .sessions
                .iter_mut()
                .find(|s| s.id == focus.session)?
                .active = u64::from(focus.index);
            graph
                .windows
                .iter_mut()
                .find(|w| w.id == focus.window)?
                .active = focus.pane;
        }
        Some(graph)
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
            send_with_snapshot(
                &self.shared,
                vec![
                    b"tmuxx-claim-pane".to_vec(),
                    b"-t".to_vec(),
                    format!("%{pane}").into_bytes(),
                ],
                false,
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
        let appearance = Arc::new(Mutex::new(None));
        pane_state.views.push(View {
            focused: focused.clone(),
            visible: visible.clone(),
            dark: dark.clone(),
            pending: pending.clone(),
            terminal: Arc::downgrade(&terminal),
            appearance: appearance.clone(),
            events: events_tx,
            exited: Arc::downgrade(&exited),
            wake,
        });
        Ok(Attached {
            terminal,
            input: PaneInput {
                shared: Arc::downgrade(&self.shared),
                appearance,
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
        if let Ok(mut model) = self.shared.model.lock() {
            model.pending_focus.clear();
            model.fail_replies("tmuxx connection is closed");
            model.release_terminals();
        }
        if let Some(system) = &mut self.system {
            system.close();
        }
        #[cfg(any(unix, windows))]
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
    tmuxx_protocol::native::write_record(writer, value)?;
    writer.flush()
}
fn record(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    tmuxx_protocol::native::read_record(reader, RECORD_LIMIT)
}
#[cfg(all(test, unix))]
fn request_record(reader: &mut impl BufRead) -> Request {
    let bytes = record(reader).unwrap().unwrap();
    let tmuxx_protocol::native::ClientMessage::Request(request) =
        tmuxx_protocol::native::decode(&bytes).unwrap()
    else {
        panic!("expected request")
    };
    request
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
        model.pending_focus.clear();
        model.fail_replies(&error.to_string());
        model.errors.push(error.to_string());
        for pane in model.panes.values() {
            for view in &pane.views {
                let _ = view.events.try_send(SessionEvent::Error(error.to_string()));
                (view.wake)();
            }
        }
        model.release_terminals();
    }
    (shared.wake)();
}
fn restore(
    snapshot: &tmuxx_terminal::CheckpointSnapshot,
) -> io::Result<(Terminal, tmuxx_terminal::AgentStatus)> {
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
fn replace_view(view: &mut Terminal, source: &Terminal, appearance: Option<&Config>) {
    let selection = view.screen().selection;
    let offset = view.screen().viewport_offset;
    let same_screen = view.is_alternate_screen() == source.is_alternate_screen();
    let pixels = (view.width_px, view.height_px);
    *view = source.clone();
    if let Some(config) = appearance {
        apply_appearance(view, config);
    }
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

#[derive(Deserialize)]
struct Selection {
    sessions: Vec<Selected>,
    windows: Vec<Selected>,
    panes: Vec<SelectedPane>,
}
#[derive(Deserialize)]
struct Selected {
    id: u64,
    active: u64,
}
#[derive(Deserialize)]
struct SelectedPane {
    id: u64,
    #[serde(default)]
    current_path: Option<String>,
    style: rustty_vt::TmuxPaneStyle,
    control_colors: [Option<[u8; 3]>; 2],
    host_dark: Option<bool>,
}
impl SelectedPane {
    fn apply(&self, terminal: &mut Terminal) {
        terminal.set_tmux_pane_style(self.style);
        terminal.tmux_control_colors = self.control_colors;
        terminal.query_defaults.color_scheme = self.host_dark.map(|dark| {
            if dark {
                rustty_vt::query::ColorScheme::Dark
            } else {
                rustty_vt::query::ColorScheme::Light
            }
        });
    }
}
fn apply_selection(
    model: &mut Model,
    shared: &Shared,
    sequence: u64,
    state: serde_json::Value,
) -> io::Result<()> {
    if sequence < model.sequence {
        return Ok(());
    }
    let selection: Selection = serde_json::from_value(state).map_err(err)?;
    let counts = (
        selection.sessions.len(),
        selection.windows.len(),
        selection.panes.len(),
    );
    let sessions = selection
        .sessions
        .into_iter()
        .map(|selected| (selected.id, selected.active))
        .collect::<BTreeMap<_, _>>();
    let windows = selection
        .windows
        .into_iter()
        .map(|selected| (selected.id, selected.active))
        .collect::<BTreeMap<_, _>>();
    let panes = selection
        .panes
        .into_iter()
        .map(|pane| (pane.id, pane))
        .collect::<BTreeMap<_, _>>();
    let Some(graph) = &model.graph else {
        return model.request_refresh(shared);
    };
    if counts != (sessions.len(), windows.len(), panes.len())
        || counts != (graph.sessions.len(), graph.windows.len(), graph.panes.len())
        || graph.sessions.iter().any(|session| {
            sessions.get(&session.id).is_none_or(|active| {
                u32::try_from(*active)
                    .ok()
                    .is_none_or(|index| !session.windows.contains_key(&index))
            })
        })
        || graph.windows.iter().any(|window| {
            windows
                .get(&window.id)
                .is_none_or(|active| !window.panes.contains(active))
        })
        || graph
            .panes
            .iter()
            .any(|pane| !panes.contains_key(&pane.id) || !model.panes.contains_key(&pane.id))
    {
        return model.request_refresh(shared);
    }
    let mut graph = model.graph.take().unwrap();
    for session in &mut graph.sessions {
        session.active = sessions[&session.id];
    }
    for window in &mut graph.windows {
        window.active = windows[&window.id];
    }
    for pane in &mut graph.panes {
        pane.current_path.clone_from(&panes[&pane.id].current_path);
    }
    for (id, selected) in panes {
        let pane = model.panes.get_mut(&id).unwrap();
        selected.apply(&mut pane.terminal);
        pane.views.retain(|view| {
            let Some(terminal) = view.terminal.upgrade() else {
                return false;
            };
            if let Ok(mut terminal) = terminal.lock() {
                selected.apply(&mut terminal);
            }
            (view.wake)();
            true
        });
    }
    model.sequence = sequence;
    model.focus_snapshot(&graph);
    model.graph = Some(graph);
    // A selection update cannot satisfy a pending structural refresh.
    (shared.wake)();
    Ok(())
}
fn apply(shared: &Shared, message: ServerMessage) -> io::Result<()> {
    if !shared.subscribe
        && matches!(
            message,
            ServerMessage::Snapshot { .. }
                | ServerMessage::Selection { .. }
                | ServerMessage::PaneOutput { .. }
                | ServerMessage::Changed { .. }
        )
    {
        return Ok(());
    }
    let mut model = shared.model.lock().map_err(err)?;
    if shared.closed.load(Ordering::Acquire) {
        return Ok(());
    }
    match message {
        ServerMessage::Selection { sequence, state } => {
            apply_selection(&mut model, shared, sequence, state)?;
        }
        ServerMessage::ServerStatus(status) => {
            if status.instance_id.is_empty()
                || !model.instance.is_empty() && model.instance != status.instance_id
            {
                return Err(err("tmuxx server status incarnation changed"));
            }
            model.instance = status.instance_id.clone();
            model.server_status = Some(status);
            (shared.wake)();
        }
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
                if std::mem::take(&mut model.refresh_pending) {
                    model.request_refresh(shared)?;
                }
                return Ok(());
            }
            let graph = state;
            validate_graph(&graph)?;
            let mut restored = BTreeMap::new();
            for pane in &graph.panes {
                restored.insert(
                    pane.id,
                    restore(
                        pane.terminal
                            .as_deref()
                            .ok_or_else(|| err("missing terminal snapshot"))?,
                    )?,
                );
            }
            if model.instance != instance_id {
                for pane in model.panes.values() {
                    close_views(pane);
                }
                model.panes.clear();
                model.server_status = None;
                model.last_host_event = None;
                model.pending_focus.clear();
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
                    if let Ok(mut terminal) = terminal.lock()
                        && let Ok(appearance) = view.appearance.lock()
                    {
                        if replaced {
                            terminal.screen_mut().selection = None;
                            terminal.screen_mut().viewport_offset = 0;
                        }
                        replace_view(&mut terminal, &pane.terminal, appearance.as_ref());
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
            model.refresh_pending = false;
            model.focus_snapshot(&graph);
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
                let mut events = Vec::new();
                for effect in pane.terminal.feed(&data) {
                    let ended = pane.agent_status.observe(&effect);
                    if let Some(event) = view_event(effect) {
                        events.push(event);
                    }
                    if let Some(event) = ended {
                        events.push(SessionEvent::Effect(Effect::AgentStatus(event)));
                    }
                }
                pane.views.retain(|view| {
                    let Some(terminal) = view.terminal.upgrade() else {
                        return false;
                    };
                    if let Ok(mut terminal) = terminal.lock() {
                        let _ = terminal.feed(&data);
                    }
                    for event in &events {
                        let _ = view.events.try_send(event.clone());
                    }
                    (view.wake)();
                    true
                });
            } else {
                model.request_refresh(shared)?;
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
            let event = match &effect {
                Effect::Notification { title, body } => SessionEvent::NotificationDelivery {
                    title: title.clone(),
                    body: body.clone(),
                },
                effect => SessionEvent::Effect(effect.clone()),
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
                    && view.events.try_send(event).is_ok()
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
            model.request_refresh(shared)?;
        }
        ServerMessage::Reply(reply) => {
            let error = reply.error.or_else(|| {
                reply.status.filter(|status| *status != 0).map(|status| {
                    if reply.output.is_empty() {
                        format!("tmuxx command failed with status {status}")
                    } else {
                        String::from_utf8_lossy(&reply.output).into_owned()
                    }
                })
            });
            model.focus_reply(reply.request_id, error.is_some());
            if let Some(receiver) = model.pending_replies.remove(&reply.request_id) {
                let _ = receiver.send(error.map_or(Ok(reply.output), |error| Err(err(error))));
                (shared.wake)();
            } else if let Some(error) = error {
                model.errors.push(error);
                (shared.wake)();
            }
        }
        ServerMessage::Detached { .. } => {
            shared.closed.store(true, Ordering::Release);
            model.pending_focus.clear();
            model.fail_replies("tmuxx detached");
            for pane in model.panes.values() {
                close_views(pane);
            }
            model.release_terminals();
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
            subscribe: false,
            control: false,
        })
        .map_err(err)
}

fn view_event(effect: Effect) -> Option<SessionEvent> {
    match effect {
        Effect::Notification { title, body } => {
            Some(SessionEvent::NotificationObserved { title, body })
        }
        Effect::Write(_) | Effect::Bell | Effect::ClipboardRead(_) | Effect::ClipboardWrite(_) => {
            None
        }
        effect => Some(SessionEvent::Effect(effect)),
    }
}
fn feed_view(terminal: &mut Terminal, data: &[u8], events: &mpsc::SyncSender<SessionEvent>) {
    for event in terminal.feed(data).into_iter().filter_map(view_event) {
        let _ = events.try_send(event);
    }
}

#[cfg(test)]
fn snapshot_state(value: serde_json::Value) -> Graph {
    serde_json::from_value(value).unwrap()
}

#[cfg(test)]
mod focus_tests {
    use super::*;
    use crate::session::Session;

    fn state() -> serde_json::Value {
        let terminal = tmuxx_terminal::Terminal::new(10, 4, 10).checkpoint_snapshot();
        let layout = r#"{"V":2,"L":{"t":"h","w":21,"h":4,"x":0,"y":0,"c":[{"t":"p","w":10,"h":4,"x":0,"y":0,"i":0,"I":"%7"},{"t":"p","w":10,"h":4,"x":11,"y":0,"i":1,"I":"%8"}]}}"#;
        serde_json::json!({
            "sessions": [
                {"id":0,"name":"one","active":0,"windows":{"0":1,"2":2}},
                {"id":1,"name":"two","active":0,"windows":{"0":1,"2":2}}
            ],
            "windows": [
                {"id":1,"name":"split","active":7,"cols":21,"rows":4,"panes":[7,8],"layout":layout},
                {"id":2,"name":"single","active":9,"cols":10,"rows":4,"panes":[9],"layout":Layout::pane(9,10,4).to_legacy().unwrap()}
            ],
            "panes": [
                {"id":7,"window":1,"cols":10,"rows":4,"status":null,"terminal":terminal},
                {"id":8,"window":1,"cols":10,"rows":4,"status":null,"terminal":terminal},
                {"id":9,"window":2,"cols":10,"rows":4,"status":null,"terminal":terminal}
            ]
        })
    }
    fn snapshot(connection: &Connection, sequence: u64, state: serde_json::Value) {
        apply(
            &connection.shared,
            ServerMessage::Snapshot {
                instance_id: "owner".into(),
                sequence,
                state: snapshot_state(state),
            },
        )
        .unwrap();
    }
    #[test]
    fn native_cwd_survives_attach_live_output_snapshot_and_reopen() {
        {
            let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
            let mut owner = tmuxx_terminal::Terminal::new(10, 4, 10);
            let state_with_cwd = |owner: &tmuxx_terminal::Terminal| {
                let mut state = state();
                state["panes"][0]["terminal"] =
                    serde_json::to_value(owner.checkpoint_snapshot()).unwrap();
                state
            };
            let check = |view: &Session, expected: &str| {
                let report = view.terminal().unwrap().working_directory.clone();
                assert_eq!(
                    view.directory_from_report(&report),
                    Some(std::path::PathBuf::from(expected)),
                    "{report}"
                );
            };
            owner.feed(b"\x1b]7;file://linux/home/one%20two\x07");
            snapshot(&connection, 1, state_with_cwd(&owner));
            let view = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
            check(&view, "/home/one two");
            for (sequence, data) in [
                (2, b"\x1b]7;file://windows/C:/Users/".as_slice()),
                (3, b"one%20two\x07".as_slice()),
            ] {
                owner.feed(data);
                apply(
                    &connection.shared,
                    ServerMessage::PaneOutput {
                        sequence,
                        pane_id: 7,
                        data: data.to_vec(),
                    },
                )
                .unwrap();
            }
            check(&view, "C:/Users/one two");
            owner.feed(b"\x1b]7;file://linux/var/remote\x07");
            snapshot(&connection, 4, state_with_cwd(&owner));
            check(&view, "/var/remote");
            drop(view);
            let reopened = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
            check(&reopened, "/var/remote");
        }
    }

    #[test]
    fn native_process_cwd_survives_reconnect_and_light_updates_without_shell_reports() {
        let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
        let mut initial = state();
        let paths = [r"C:\work\one", r"C:\work\two", r"C:\work\雪 é"];
        for (pane, path) in initial["panes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .zip(paths)
        {
            pane["current_path"] = path.into();
        }
        snapshot(&connection, 1, initial.clone());
        let check = |view: &Session, path: &str| {
            let report = view.terminal().unwrap().working_directory.clone();
            assert_eq!(
                view.directory_from_report(&report),
                Some(PathBuf::from(path.replace('\\', "/")))
            );
        };
        let views = [7, 8, 9]
            .map(|pane| Session::attach_tmuxx(&connection, pane, Arc::new(|| {})).unwrap());
        for (view, path) in views.iter().zip(paths) {
            check(view, path);
        }
        // Opening another view refreshes every existing pane without new shell output.
        snapshot(&connection, 2, initial.clone());
        for (view, path) in views.iter().zip(paths) {
            check(view, path);
        }
        let (reconnected, _requests) = Connection::base(Arc::new(|| {}), true);
        snapshot(&reconnected, 2, initial);
        let reopened = Session::attach_tmuxx(&reconnected, 7, Arc::new(|| {})).unwrap();
        check(&reopened, paths[0]);

        let mut update = selection_state(8);
        for (pane, path) in update["panes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .zip(paths)
        {
            pane["current_path"] = path.into();
        }
        update["panes"][0]["current_path"] = r"C:\work\changed".into();
        apply(
            &reconnected.shared,
            ServerMessage::Selection {
                sequence: 3,
                state: update,
            },
        )
        .unwrap();
        check(&reopened, r"C:\work\changed");
        apply(
            &reconnected.shared,
            ServerMessage::PaneOutput {
                sequence: 4,
                pane_id: 7,
                data: b"\x1b]7;file:///C:/shell%20report\x07".to_vec(),
            },
        )
        .unwrap();
        check(&reopened, "C:/shell report");
        let legacy = Session::attach_tmuxx(&connection, 9, Arc::new(|| {})).unwrap();
        snapshot(&connection, 3, state());
        assert_eq!(legacy.directory_from_report(""), None);
        assert_eq!(reopened.directory_from_report("bad\0path"), None);
    }
    fn reply(connection: &Connection, request_id: u64, error: Option<&str>) {
        apply(
            &connection.shared,
            ServerMessage::Reply(tmuxx_protocol::native::Response {
                request_id,
                status: None,
                output: Vec::new(),
                error: error.map(str::to_owned),
            }),
        )
        .unwrap();
    }
    fn active(connection: &Connection) -> u64 {
        connection.graph().unwrap().windows[0].active
    }
    fn selection_state(active: u64) -> serde_json::Value {
        serde_json::json!({
            "sessions": [{"id":0,"active":0},{"id":1,"active":0}],
            "windows": [{"id":1,"active":active},{"id":2,"active":9}],
            "panes": ([7,8,9].map(|id| serde_json::json!({
                "id":id,
                "style":rustty_vt::TmuxPaneStyle {
                    is_active: id == active || id == 9,
                    ..Default::default()
                },
                "control_colors":[null,null],
                "host_dark":null,
            }))),
        })
    }
    fn selection(connection: &Connection, sequence: u64, active: u64) {
        apply(
            &connection.shared,
            ServerMessage::Selection {
                sequence,
                state: selection_state(active),
            },
        )
        .unwrap();
    }

    #[test]
    fn selection_updates_release_noop_and_rapid_focus_without_snapshot_requests() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        connection.shared.model.lock().unwrap().selection_updates = true;
        snapshot(&connection, 1, state());
        connection.focus_pane(0, 0, 7).unwrap();
        let noop = requests.try_recv().unwrap();
        assert!(!noop.subscribe && !noop.control);
        reply(&connection, noop.request_id, None);
        selection(&connection, 1, 7);
        assert!(
            connection
                .shared
                .model
                .lock()
                .unwrap()
                .pending_focus
                .is_empty()
        );

        connection.focus_pane(1, 0, 8).unwrap();
        let first = requests.try_recv().unwrap();
        selection(&connection, 2, 7);
        assert_eq!(active(&connection), 8, "old selection reversed the click");
        reply(&connection, first.request_id, None);
        assert_eq!(active(&connection), 8, "reply exposed the cached graph");
        connection.focus_pane(0, 0, 7).unwrap();
        let second = requests.try_recv().unwrap();
        assert!(!first.subscribe && !second.subscribe);
        selection(&connection, 3, 8);
        assert_eq!(
            active(&connection),
            7,
            "earlier click replaced the latest one"
        );
        reply(&connection, second.request_id, None);
        selection(&connection, 4, 7);
        assert!(
            connection
                .shared
                .model
                .lock()
                .unwrap()
                .pending_focus
                .is_empty()
        );
        selection(&connection, 5, 8);
        assert_eq!(
            active(&connection),
            8,
            "later external selection was hidden"
        );
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn selection_updates_preserve_live_output_history_selection_and_view_appearance() {
        use crate::session::Session;

        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        let mut owner = tmuxx_terminal::Terminal::new(10, 4, 10);
        owner.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        let mut initial = state();
        initial["panes"][0]["terminal"] =
            serde_json::to_value(owner.checkpoint_snapshot()).unwrap();
        snapshot(&connection, 1, initial);
        let before = connection.graph().unwrap();
        let view = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        let mut config = Config::default();
        config.background = crate::config::Rgb::new(1, 2, 3);
        view.apply_appearance(&config).unwrap();
        let selected = {
            let mut terminal = view.terminal().unwrap();
            terminal.screen_mut().scroll_viewport(1);
            let row = terminal.screen().row(0).id;
            let selected = rustty_vt::Selection {
                start: rustty_vt::GridPoint { row, col: 0 },
                end: rustty_vt::GridPoint { row, col: 2 },
                rectangular: false,
            };
            terminal.screen_mut().selection = Some(selected);
            selected
        };
        requests.try_iter().for_each(drop);
        apply(
            &connection.shared,
            ServerMessage::PaneOutput {
                sequence: 2,
                pane_id: 7,
                data: b" streamed".to_vec(),
            },
        )
        .unwrap();
        let (text, rows, offset) = {
            let terminal = view.terminal().unwrap();
            (
                terminal.plain_text(),
                terminal
                    .screen()
                    .all_rows()
                    .map(|row| row.id)
                    .collect::<Vec<_>>(),
                terminal.screen().viewport_offset,
            )
        };
        let owner_text = connection.shared.model.lock().unwrap().panes[&7]
            .terminal
            .plain_text();
        let mut state = selection_state(8);
        state["panes"][0]["style"]["normal"][1] = serde_json::json!([4, 5, 6]);
        state["panes"][0]["control_colors"] = serde_json::json!([[7, 8, 9], [10, 11, 12]]);
        state["panes"][0]["host_dark"] = true.into();
        apply(
            &connection.shared,
            ServerMessage::Selection { sequence: 3, state },
        )
        .unwrap();
        let after = connection.graph().unwrap();
        for (old, new) in before.panes.iter().zip(&after.panes) {
            assert!(Arc::ptr_eq(
                old.terminal.as_ref().unwrap(),
                new.terminal.as_ref().unwrap()
            ));
        }
        {
            let terminal = view.terminal().unwrap();
            assert_eq!(terminal.plain_text(), text);
            assert_eq!(
                terminal
                    .screen()
                    .all_rows()
                    .map(|row| row.id)
                    .collect::<Vec<_>>(),
                rows
            );
            assert_eq!(terminal.screen().selection, Some(selected));
            assert_eq!(terminal.screen().viewport_offset, offset);
            assert_eq!(terminal.background, [4, 5, 6]);
            assert_eq!(
                terminal.color_default(rustty_vt::color::Target::Background),
                Some([1, 2, 3])
            );
            assert_eq!(
                terminal.tmux_control_colors,
                [Some([7, 8, 9]), Some([10, 11, 12])]
            );
            assert_eq!(
                terminal.query_defaults.color_scheme,
                Some(rustty_vt::query::ColorScheme::Dark)
            );
        }
        let model = connection.shared.model.lock().unwrap();
        assert_eq!(model.panes[&7].terminal.background, [4, 5, 6]);
        assert_eq!(model.panes[&7].terminal.plain_text(), owner_text);
        drop(model);
        selection(&connection, 1, 7);
        assert_eq!(active(&connection), 8, "stale selection was applied");
        apply(
            &connection.shared,
            ServerMessage::PaneOutput {
                sequence: 4,
                pane_id: 7,
                data: b"!".to_vec(),
            },
        )
        .unwrap();
        let terminal = view.terminal().unwrap();
        assert!(
            terminal
                .screen()
                .all_rows()
                .any(|row| terminal.screen().row_text(row).contains('!'))
        );
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn selection_updates_require_matching_graph_and_do_not_satisfy_structural_refreshes() {
        let mut invalid = Vec::new();
        let mut missing = selection_state(8);
        missing["panes"].as_array_mut().unwrap().pop();
        invalid.push(missing);
        for (section, index, field, value) in [
            ("sessions", 1, "id", 0),
            ("sessions", 0, "active", u64::from(u32::MAX) + 1),
            ("windows", 0, "active", 9),
            ("windows", 0, "id", 99),
            ("panes", 0, "id", 99),
            ("panes", 0, "id", 8),
        ] {
            let mut state = selection_state(8);
            state[section][index][field] = value.into();
            invalid.push(state);
        }
        for state in invalid {
            let (connection, requests) = Connection::base(Arc::new(|| {}), true);
            snapshot(&connection, 1, self::state());
            apply(
                &connection.shared,
                ServerMessage::Selection { sequence: 2, state },
            )
            .unwrap();
            assert_eq!(
                active(&connection),
                7,
                "invalid update partly changed the graph"
            );
            let refresh = requests.try_recv().unwrap();
            assert!(refresh.subscribe && refresh.argv.is_empty());
            selection(&connection, 3, 8);
            assert!(connection.shared.model.lock().unwrap().refresh_pending);
            apply(&connection.shared, ServerMessage::Changed { sequence: 4 }).unwrap();
            assert!(requests.try_recv().is_err());
            snapshot(&connection, 4, self::state());
            assert!(!connection.shared.model.lock().unwrap().refresh_pending);
        }
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        selection(&connection, 1, 7);
        assert!(connection.graph().is_none());
        assert!(requests.try_recv().unwrap().subscribe);
    }

    #[test]
    fn legacy_view_fit_targets_cached_session_windows_without_context_size_updates() {
        let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
        snapshot(&connection, 1, state());
        let (sizing, requests) = Connection::base(Arc::new(|| {}), false);
        connection
            .size_views
            .lock()
            .unwrap()
            .insert(0, (sizing, (0, 0, 0, 0)));
        assert!(connection.view_size(0, 0, 20, 8, 16).is_err());
        assert!(requests.try_recv().is_err());
        connection.view_size(0, 70, 20, 8, 16).unwrap();
        let commands = requests
            .try_iter()
            .map(|request| request.argv)
            .collect::<Vec<_>>();
        assert_eq!(
            commands,
            vec![
                ["refresh-client", "-C", "70,20"]
                    .map(|arg| arg.as_bytes().to_vec())
                    .to_vec(),
                ["tmuxx-cell-size", "8", "16"]
                    .map(|arg| arg.as_bytes().to_vec())
                    .to_vec(),
            ]
        );
        connection.fit_view_size(0).unwrap();
        let commands = requests
            .try_iter()
            .map(|request| request.argv)
            .collect::<Vec<_>>();
        assert_eq!(
            commands,
            ["$0:@1", "$0:@2"].map(|target| {
                ["resize-window", "-t", target, "-x", "70", "-y", "20"]
                    .map(|arg| arg.as_bytes().to_vec())
                    .to_vec()
            })
        );
    }

    #[test]
    fn reload_configuration_preserves_paths_and_reports_errors() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), false);
        assert!(connection.reload_configuration().is_err());
        for config_files in [None, Some(vec![])] {
            apply(
                &connection.shared,
                ServerMessage::ServerStatus(ServerStatus {
                    instance_id: "owner".into(),
                    connections: 1,
                    config_files,
                }),
            )
            .unwrap();
            assert!(connection.reload_configuration().is_err());
            assert!(requests.try_recv().is_err());
        }
        let files = vec![
            "C:/config, first.conf".into(),
            "C:/配置;second.conf;".into(),
        ];
        apply(
            &connection.shared,
            ServerMessage::ServerStatus(ServerStatus {
                instance_id: "owner".into(),
                connections: 1,
                config_files: Some(files.clone()),
            }),
        )
        .unwrap();
        let id = connection.reload_configuration().unwrap();
        let request = requests.try_recv().unwrap();
        assert_eq!(request.request_id, id);
        assert!(!request.control && !request.subscribe);
        assert_eq!(
            tmuxx_command::parse_argv(&request.argv).unwrap(),
            vec![tmuxx_command::Command::new(
                ["source-file".to_owned(), "--".to_owned()]
                    .into_iter()
                    .chain(files)
                    .map(String::into_bytes)
                    .collect()
            )]
        );
        reply(&connection, id, Some("cannot read configuration"));
        assert_eq!(connection.errors(), ["cannot read configuration"]);
        let id = connection.reload_configuration().unwrap();
        requests.try_recv().unwrap();
        apply(
            &connection.shared,
            ServerMessage::Reply(tmuxx_protocol::native::Response {
                request_id: id,
                status: Some(1),
                output: b"config.conf:1: unterminated quote".to_vec(),
                error: None,
            }),
        )
        .unwrap();
        assert_eq!(connection.errors(), ["config.conf:1: unterminated quote"]);
        fail(&connection.shared, "disconnected");
        assert!(connection.reload_configuration().is_err());
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn server_watch_keeps_only_status_and_consumes_its_optional_reply() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), false);
        for error in [Some("unknown command: tmuxx-server-status"), None] {
            connection.watch_server().unwrap();
            let request = requests.try_recv().unwrap();
            assert_eq!(request.argv, [b"tmuxx-server-status".to_vec()]);
            assert!(!request.subscribe && !request.control && request.stdin.is_none());
            reply(&connection, request.request_id, error);
            assert!(connection.errors().is_empty());
            assert!(
                connection
                    .shared
                    .model
                    .lock()
                    .unwrap()
                    .pending_replies
                    .is_empty()
            );
        }
        for connections in [3, 0] {
            let status = ServerStatus {
                instance_id: "owner".into(),
                connections,
                config_files: None,
            };
            apply(
                &connection.shared,
                ServerMessage::ServerStatus(status.clone()),
            )
            .unwrap();
            assert_eq!(connection.server_status(), Some(status));
            assert_eq!(connection.instance(), "owner");
        }
        snapshot(&connection, 1, state());
        apply(&connection.shared, ServerMessage::Changed { sequence: 2 }).unwrap();
        assert!(connection.graph().is_none());
        assert!(connection.shared.model.lock().unwrap().panes.is_empty());
        assert!(requests.try_recv().is_err());
        for instance_id in ["", "replacement"] {
            assert!(
                apply(
                    &connection.shared,
                    ServerMessage::ServerStatus(ServerStatus {
                        instance_id: instance_id.into(),
                        connections: 1,
                        config_files: None,
                    }),
                )
                .is_err()
            );
            assert_eq!(connection.instance(), "owner");
        }
        let command = connection.command(["invalid-command"]).unwrap();
        reply(&connection, command, Some("ordinary command failed"));
        assert_eq!(connection.errors(), ["ordinary command failed"]);
        fail(&connection.shared, "connection lost");
        assert!(connection.server_status().is_none());
        apply(
            &connection.shared,
            ServerMessage::ServerStatus(ServerStatus {
                instance_id: "owner".into(),
                connections: 9,
                config_files: None,
            }),
        )
        .unwrap();
        assert!(connection.server_status().is_none());
        assert!(connection.watch_server().is_err());
    }

    #[test]
    fn released_and_disconnected_inputs_reject_writes_before_queueing() {
        for native in [false, true] {
            for disconnect in [false, true] {
                let (connection, requests) = Connection::base(Arc::new(|| {}), true);
                snapshot(&connection, 1, state());
                let mut attached = connection.attach(7, Arc::new(|| {})).unwrap();
                attached.input.native = native;
                assert!(attached.input.accepts_input());
                if disconnect {
                    fail(&connection.shared, "broken pipe");
                } else {
                    attached.input.release();
                }
                requests.try_iter().for_each(drop);
                assert!(!attached.input.accepts_input());
                for result in [
                    attached.input.write(b"text"),
                    attached.input.write_direct(b"mouse report"),
                    attached
                        .input
                        .write_input(&Input::Mouse(b"mouse report".to_vec())),
                ] {
                    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
                }
                assert!(requests.try_recv().is_err());
            }
        }
    }

    #[test]
    fn closed_connections_release_sources_but_keep_view_contents_and_session_metadata() {
        use crate::session::Session;

        for close in ["failure", "detach", "drop"] {
            let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
            let mut owner = tmuxx_terminal::Terminal::new(10, 4, 10);
            owner.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
            let mut initial = state();
            initial["panes"][0]["terminal"] =
                serde_json::to_value(owner.checkpoint_snapshot()).unwrap();
            snapshot(&connection, 1, initial);
            let snapshots = connection
                .graph()
                .unwrap()
                .panes
                .iter()
                .map(|pane| Arc::downgrade(pane.terminal.as_ref().unwrap()))
                .collect::<Vec<_>>();
            let view = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
            let (contents, selection) = {
                assert!(view.accepts_input());
                let mut terminal = view.terminal().unwrap();
                terminal.screen_mut().scroll_viewport(1);
                let row = terminal.screen().row(0).id;
                let selection = rustty_vt::Selection {
                    start: rustty_vt::GridPoint { row, col: 0 },
                    end: rustty_vt::GridPoint { row, col: 2 },
                    rectangular: false,
                };
                terminal.screen_mut().selection = Some(selection);
                (terminal.plain_text(), selection)
            };
            let shared = connection.shared.clone();
            let mut connection = Some(connection);
            match close {
                "failure" => fail(&shared, "connection lost"),
                "detach" => apply(
                    &shared,
                    ServerMessage::Detached {
                        session: "one".into(),
                        hangup: false,
                    },
                )
                .unwrap(),
                "drop" => drop(connection.take()),
                _ => unreachable!(),
            }
            assert!(
                snapshots
                    .iter()
                    .all(|snapshot| snapshot.upgrade().is_none())
            );
            assert_eq!(view.has_exited(), close == "detach");
            assert!(!view.accepts_input());
            {
                let terminal = view.terminal().unwrap();
                assert_eq!(terminal.plain_text(), contents);
                assert_eq!(terminal.screen().selection, Some(selection));
                assert_eq!(terminal.screen().viewport_offset, 1);
            }
            assert_eq!(view.snapshot().unwrap().screen.selection, Some(selection));
            if let Some(connection) = &connection {
                assert!(connection.is_closed());
                assert_eq!(connection.instance(), "owner");
                let sessions = connection.sessions();
                assert_eq!(sessions[0].id, 0);
                assert_eq!(sessions[0].name, "one");
                assert_eq!(sessions[0].windows, ["split", "single"]);
            }
            // A frame already read when another worker closes cannot retain data again.
            apply(
                &shared,
                ServerMessage::Snapshot {
                    instance_id: "late".into(),
                    sequence: 2,
                    state: snapshot_state(state()),
                },
            )
            .unwrap();
            let model = shared.model.lock().unwrap();
            assert!(model.panes.is_empty());
            assert_eq!(model.instance, "owner");
            let graph = model.graph.as_ref().unwrap();
            assert_eq!(graph.sessions[0].name, "one");
            assert_eq!(graph.windows[0].panes, [7, 8]);
            assert!(graph.panes.iter().all(|pane| pane.terminal.is_none()));
        }
    }

    #[test]
    fn view_appearance_survives_native_snapshots_and_application_overrides() {
        use crate::config::{CursorStyle, Rgb, TerminalColor};
        use crate::session::Session;
        use rustty_vt::CursorShape;

        let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
        let mut owner = tmuxx_terminal::Terminal::new(10, 4, 10);
        owner.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        let refresh = |owner: &tmuxx_terminal::Terminal, sequence| {
            let mut state = state();
            state["panes"][0]["terminal"] =
                serde_json::to_value(owner.checkpoint_snapshot()).unwrap();
            snapshot(&connection, sequence, state);
        };
        refresh(&owner, 1);
        let first = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        let second = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        let history = first.terminal().unwrap().screen().history_len();
        assert!(history > 0);
        let mut configs = [Config::default(), Config::default()];
        for (index, (view, config)) in [&first, &second].into_iter().zip(&mut configs).enumerate() {
            config.foreground = Rgb::new(index as u8, 20, 30);
            config.background = Rgb::new(index as u8, 40, 50);
            config.palette[1] = Rgb::new(index as u8, 60, 70);
            config.cursor_color = Some(TerminalColor::Rgb(Rgb::new(index as u8, 80, 90)));
            config.cursor_style = CursorStyle::Underline;
            config.scrollback_limit_lines = Some(0);
            config.scrollback_limit_bytes = Some(0);
            view.apply_appearance(config).unwrap();
            assert_eq!(view.terminal().unwrap().screen().history_len(), history);
            assert_eq!(view.terminal().unwrap().limits().lines, Some(10));
        }
        let defaults = |configs: &[Config; 2]| {
            for (view, config) in [&first, &second].into_iter().zip(configs) {
                let terminal = view.terminal().unwrap();
                assert_eq!(terminal.foreground, config.foreground.to_array());
                assert_eq!(terminal.background, config.background.to_array());
                assert_eq!(terminal.palette[1], config.palette[1].to_array());
                assert_eq!(terminal.screen().cursor.shape, CursorShape::Underline);
                assert_eq!(
                    terminal.cursor_color,
                    Some(
                        config
                            .cursor_color
                            .unwrap()
                            .resolve(config.foreground, config.background)
                            .to_array()
                    ),
                );
            }
        };
        refresh(&owner, 2);
        defaults(&configs);
        assert!(first.terminal().unwrap().screen().history_len() > 0);
        {
            let model = connection.shared.model.lock().unwrap();
            let source = &model.panes[&7].terminal;
            assert_eq!(source.limits().lines, Some(10));
            assert!(source.screen().history_len() > 0);
            assert_ne!(source.background, configs[0].background.to_array());
        }

        let overrides = b"\x1b]10;#abcdef\x07\x1b]11;#123456\x07\x1b]12;#778899\x07\x1b]4;1;#112233\x07\x1b[6 q";
        owner.feed(overrides);
        apply(
            &connection.shared,
            ServerMessage::PaneOutput {
                sequence: 3,
                pane_id: 7,
                data: overrides.to_vec(),
            },
        )
        .unwrap();
        configs[0].foreground = Rgb::new(100, 101, 102);
        first.apply_config(&configs[0]).unwrap();
        for resnapshot in [false, true] {
            if resnapshot {
                refresh(&owner, 4);
            }
            for view in [&first, &second] {
                let terminal = view.terminal().unwrap();
                assert_eq!(terminal.foreground, [0xab, 0xcd, 0xef]);
                assert_eq!(terminal.background, [0x12, 0x34, 0x56]);
                assert_eq!(terminal.cursor_color, Some([0x77, 0x88, 0x99]));
                assert_eq!(terminal.palette[1], [0x11, 0x22, 0x33]);
                assert_eq!(terminal.screen().cursor.shape, CursorShape::Bar);
            }
        }
        let reset = b"\x1b]110\x07\x1b]111\x07\x1b]112\x07\x1b]104;1\x07\x1b[0 q";
        owner.feed(reset);
        apply(
            &connection.shared,
            ServerMessage::PaneOutput {
                sequence: 5,
                pane_id: 7,
                data: reset.to_vec(),
            },
        )
        .unwrap();
        defaults(&configs);
        refresh(&owner, 6);
        defaults(&configs);
    }

    #[test]
    fn graph_copies_share_terminal_snapshots_without_sharing_focus() {
        let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
        snapshot(&connection, 1, state());
        let original = connection.graph().unwrap();
        connection.focus_pane(1, 0, 8).unwrap();
        let projected = connection.graph().unwrap();
        assert_eq!(original.windows[0].active, 7);
        assert_eq!(projected.windows[0].active, 8);
        for (old, new) in original.panes.iter().zip(&projected.panes) {
            assert!(Arc::ptr_eq(
                old.terminal.as_ref().unwrap(),
                new.terminal.as_ref().unwrap()
            ));
            assert!(new.terminal.as_ref().unwrap().checkpoint.is_some());
        }
        snapshot(&connection, 2, state());
        let refreshed = connection.graph().unwrap();
        assert!(!Arc::ptr_eq(
            original.panes[0].terminal.as_ref().unwrap(),
            refreshed.panes[0].terminal.as_ref().unwrap()
        ));
    }

    #[test]
    fn refresh_coalesces_changes_and_unknown_output_until_a_current_snapshot_arrives() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        snapshot(&connection, 1, state());
        for sequence in 2..=4 {
            apply(&connection.shared, ServerMessage::Changed { sequence }).unwrap();
            apply(
                &connection.shared,
                ServerMessage::PaneOutput {
                    sequence,
                    pane_id: 99,
                    data: b"unknown pane".to_vec(),
                },
            )
            .unwrap();
        }
        let request = requests.try_recv().unwrap();
        assert!(request.argv.is_empty() && request.subscribe && !request.control);
        assert!(requests.try_recv().is_err());

        snapshot(&connection, 3, state());
        assert!(
            requests.try_recv().unwrap().subscribe,
            "stale snapshot must retry"
        );
        apply(&connection.shared, ServerMessage::Changed { sequence: 4 }).unwrap();
        assert!(requests.try_recv().is_err());

        snapshot(&connection, 4, state());
        apply(&connection.shared, ServerMessage::Changed { sequence: 5 }).unwrap();
        assert!(requests.try_recv().unwrap().subscribe);
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn failed_refresh_send_does_not_prevent_retry_or_change_command_snapshots() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        for _ in 0..256 {
            connection.command(["display-message", "queued"]).unwrap();
        }
        assert!(apply(&connection.shared, ServerMessage::Changed { sequence: 1 }).is_err());
        assert!(!connection.shared.model.lock().unwrap().refresh_pending);
        assert!(requests.try_iter().all(|request| request.subscribe));
        apply(&connection.shared, ServerMessage::Changed { sequence: 1 }).unwrap();
        assert!(requests.try_recv().unwrap().subscribe);

        let _reply = connection
            .command_with_reply(vec![b"display-message".to_vec()])
            .unwrap();
        assert!(
            requests.try_recv().unwrap().subscribe,
            "command replies keep their snapshot barrier"
        );

        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        drop(requests);
        assert!(apply(&connection.shared, ServerMessage::Changed { sequence: 1 }).is_err());
        assert!(!connection.shared.model.lock().unwrap().refresh_pending);
    }

    #[test]
    fn unchanged_focus_still_requests_a_snapshot_and_releases_its_projection() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        snapshot(&connection, 1, state());
        connection.focus_pane(0, 0, 7).unwrap();
        let request = requests.try_recv().unwrap();
        assert!(request.subscribe);
        reply(&connection, request.request_id, None);
        snapshot(&connection, 1, state());
        assert!(
            connection
                .shared
                .model
                .lock()
                .unwrap()
                .pending_focus
                .is_empty()
        );
    }

    #[test]
    fn focus_waits_for_its_snapshot_and_preserves_newer_clicks_and_external_changes() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        snapshot(&connection, 1, state());
        connection.focus_pane(0, 0, 8).unwrap();
        let first = requests.try_recv().unwrap();
        assert_eq!(tmuxx_command::parse_argv(&first.argv).unwrap().len(), 2);
        assert!(requests.try_recv().is_err());
        assert_eq!(active(&connection), 8);
        snapshot(&connection, 2, state());
        assert_eq!(active(&connection), 8, "old snapshot reversed the click");
        reply(&connection, first.request_id, None);
        assert_eq!(active(&connection), 8, "reply exposed the cached graph");

        connection.focus_pane(0, 0, 7).unwrap();
        let second = requests.try_recv().unwrap();
        let mut selected = state();
        selected["windows"][0]["active"] = 8.into();
        snapshot(&connection, 3, selected.clone());
        assert_eq!(
            active(&connection),
            7,
            "earlier click replaced the latest one"
        );
        reply(&connection, second.request_id, None);
        snapshot(&connection, 4, state());
        assert!(
            connection
                .shared
                .model
                .lock()
                .unwrap()
                .pending_focus
                .is_empty()
        );
        snapshot(&connection, 5, selected);
        assert_eq!(active(&connection), 8, "later external focus was hidden");
    }

    #[test]
    fn focus_projection_orders_linked_sessions_and_protects_active_tabs() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        snapshot(&connection, 1, state());
        connection.focus_pane(1, 0, 8).unwrap();
        connection.focus_pane(0, 0, 7).unwrap();
        assert_eq!(
            active(&connection),
            7,
            "session ID ordering reversed the latest click"
        );
        let first = requests.try_recv().unwrap();
        let second = requests.try_recv().unwrap();
        reply(&connection, first.request_id, None);
        let mut selected = state();
        selected["windows"][0]["active"] = 8.into();
        snapshot(&connection, 2, selected);
        assert_eq!(active(&connection), 7);
        reply(&connection, second.request_id, None);
        snapshot(&connection, 3, state());
        connection.focus_pane(0, 2, 9).unwrap();
        let third = requests.try_recv().unwrap();
        snapshot(&connection, 4, state());
        assert_eq!(connection.graph().unwrap().sessions[0].active, 2);
        reply(&connection, third.request_id, None);
        // A server hook may choose a different tab than the requested one.
        snapshot(&connection, 5, state());
        assert_eq!(connection.graph().unwrap().sessions[0].active, 0);
    }

    #[test]
    fn focus_releases_rejected_removed_replaced_and_disconnected_targets() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        snapshot(&connection, 1, state());
        connection.focus_pane(0, 0, 8).unwrap();
        reply(
            &connection,
            requests.try_recv().unwrap().request_id,
            Some("denied"),
        );
        assert_eq!(active(&connection), 7);
        assert_eq!(connection.errors(), ["denied"]);

        connection.focus_pane(0, 0, 8).unwrap();
        let mut removed = state();
        removed["windows"][0]["panes"] = serde_json::json!([7]);
        removed["windows"][0]["layout"] = Layout::pane(7, 21, 4).to_legacy().unwrap().into();
        removed["panes"].as_array_mut().unwrap().remove(1);
        snapshot(&connection, 2, removed);
        assert_eq!(active(&connection), 7);
        assert!(
            connection
                .shared
                .model
                .lock()
                .unwrap()
                .pending_focus
                .is_empty()
        );

        snapshot(&connection, 3, state());
        connection.focus_pane(0, 0, 8).unwrap();
        apply(
            &connection.shared,
            ServerMessage::Snapshot {
                instance_id: "replacement".into(),
                sequence: 1,
                state: snapshot_state(state()),
            },
        )
        .unwrap();
        assert_eq!(active(&connection), 7);
        assert!(
            connection
                .shared
                .model
                .lock()
                .unwrap()
                .pending_focus
                .is_empty()
        );
        connection.focus_pane(0, 0, 8).unwrap();
        fail(&connection.shared, "disconnected");
        assert_eq!(active(&connection), 7);
        assert!(connection.focus_pane(0, 0, 8).is_err());
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use crate::session::Session;
    use std::time::Instant;

    fn instance() -> String {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        format!(
            "rustty-native-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn wait(connection: &Connection, predicate: impl Fn(&Graph) -> bool) -> Graph {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let errors = connection.errors();
            assert!(errors.is_empty(), "{errors:?}");
            if let Some(graph) = connection.graph()
                && predicate(&graph)
            {
                return graph;
            }
            assert!(
                Instant::now() < deadline,
                "native Windows view did not converge"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn absent_pipe_attachment_does_not_start_an_owner() {
        let name = instance();
        let endpoint = windows_endpoint(&name).unwrap();
        assert!(Connection::native(&endpoint, Arc::new(|| {})).is_err());
        assert!(Connection::windows(&name, Arc::new(|| {})).is_err());
        assert!(tmuxx_runtime::windows::connect(&endpoint).is_err());
    }

    #[test]
    #[ignore = "requires TMUXX_CONPTY_RESOURCES from a verified bundle; run with --ignored"]
    fn embedded_and_pipe_views_share_input_sizes_and_reconnect_without_replacing_the_pane() {
        // Supply TMUXX_CONPTY_RESOURCES from a verified Rustty/tmuxx bundle.
        let name = instance();
        let endpoint = windows_endpoint(&name).unwrap();
        let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
        let owner = Connection::embedded_with_command(
            &endpoint,
            Arc::new(|| {}),
            [shell.as_str(), "/D", "/Q", "/K", "echo RUSTTY_NATIVE_READY"]
                .map(|arg| arg.as_bytes().to_vec())
                .to_vec(),
        )
        .unwrap();
        let graph = wait(&owner, |graph| graph.panes.len() == 1);
        let pane = graph.panes[0].id;
        let pid = graph.panes[0].pid.expect("pane process ID");
        let session = graph.sessions[0].id;
        let embedded_view = Session::attach_tmuxx(&owner, pane, Arc::new(|| {})).unwrap();
        let remote = Connection::windows(&name, Arc::new(|| {})).unwrap();
        let graph = wait(&remote, |graph| graph.panes.len() == 1);
        assert_eq!(graph.panes[0].pid, Some(pid));
        assert!(!owner.persistence_survives_app());
        assert!(remote.persistence_survives_app());
        let pipe_view = Session::attach_tmuxx(&remote, pane, Arc::new(|| {})).unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        while !pipe_view
            .terminal()
            .unwrap()
            .plain_text()
            .contains("RUSTTY_NATIVE_READY")
        {
            assert!(Instant::now() < deadline, "shell did not become ready");
            thread::sleep(Duration::from_millis(5));
        }
        pipe_view.write(b"set /a 137*241\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !embedded_view
            .terminal()
            .unwrap()
            .plain_text()
            .contains("33017")
        {
            assert!(remote.errors().is_empty());
            assert!(
                Instant::now() < deadline,
                "pipe input did not reach the shared shell"
            );
            thread::sleep(Duration::from_millis(5));
        }

        owner
            .command(["set-option", "-w", "window-size", "smallest"])
            .unwrap();
        owner.view_size(session, 100, 40, 8, 16).unwrap();
        wait(&owner, |graph| {
            graph.windows[0].cols == 100 && graph.windows[0].rows == 40
        });
        remote.view_size(session, 60, 20, 8, 16).unwrap();
        wait(&owner, |graph| {
            graph.windows[0].cols == 60 && graph.windows[0].rows == 20
        });
        remote.release_view(session).unwrap();
        wait(&owner, |graph| {
            graph.windows[0].cols == 100 && graph.windows[0].rows == 40
        });

        // The reader holds Shared while blocked on the pipe. Dropping the
        // connection must cancel that read as well as its request writer.
        let workers = Arc::downgrade(&remote.shared);
        drop(pipe_view);
        drop(remote);
        let deadline = Instant::now() + Duration::from_secs(5);
        while workers.upgrade().is_some() {
            assert!(
                Instant::now() < deadline,
                "pipe workers survived connection shutdown"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let reconnected = Connection::native(&endpoint, Arc::new(|| {})).unwrap();
        let graph = wait(&reconnected, |graph| graph.panes.len() == 1);
        assert_eq!(graph.panes[0].pid, Some(pid));
        assert_eq!(graph.panes[0].id, pane);
        drop(reconnected);
        drop(embedded_view);
        drop(owner);
        assert!(tmuxx_runtime::windows::connect(&endpoint).is_err());
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::session::Session;
    use std::time::Instant;

    #[test]
    fn native_handshake_holds_requests_until_accepted_and_preserves_buffered_messages() {
        let (stream, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection =
            Connection::stream(stream.try_clone().unwrap(), stream, Arc::new(|| {})).unwrap();
        let request_id = connection.command(["new-session", "-d"]).unwrap();
        let mut peer = BufReader::new(peer);
        let hello: Hello =
            tmuxx_protocol::native::decode(&record(&mut peer).unwrap().unwrap()).unwrap();
        assert_eq!(hello.version, tmuxx_protocol::native::VERSION);
        peer.get_ref()
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        assert!(matches!(
            record(&mut peer).unwrap_err().kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
        peer.get_ref()
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        let mut response = Vec::new();
        write_record(&mut response, &Hello::default()).unwrap();
        write_record(
            &mut response,
            &ServerMessage::Reply(tmuxx_protocol::native::Response {
                request_id,
                status: None,
                output: Vec::new(),
                error: Some("buffered reply".into()),
            }),
        )
        .unwrap();
        peer.get_mut().write_all(&response).unwrap();
        let subscription = request_record(&mut peer);
        assert!(subscription.argv.is_empty() && subscription.subscribe);
        let request = request_record(&mut peer);
        assert_eq!(request.request_id, request_id);
        assert_eq!(request.argv, [b"new-session".to_vec(), b"-d".to_vec()]);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let errors = connection.errors();
            if !errors.is_empty() {
                assert_eq!(errors, ["buffered reply"]);
                break;
            }
            assert!(Instant::now() < deadline, "buffered reply was lost");
            thread::sleep(Duration::from_millis(5));
        }
        drop(connection);
        peer.get_ref().shutdown(std::net::Shutdown::Both).unwrap();
    }

    #[test]
    fn native_focus_snapshot_fallback_follows_server_negotiation() {
        for supported in [false, true] {
            let (stream, peer) = std::os::unix::net::UnixStream::pair().unwrap();
            let connection =
                Connection::stream(stream.try_clone().unwrap(), stream, Arc::new(|| {})).unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut peer = BufReader::new(peer);
            let hello: Hello =
                tmuxx_protocol::native::decode(&record(&mut peer).unwrap().unwrap()).unwrap();
            assert!(hello.selection_updates);
            write_record(
                peer.get_mut(),
                &Hello {
                    instance_id: Some("owner".into()),
                    selection_updates: supported,
                    ..Default::default()
                },
            )
            .unwrap();
            let initial = request_record(&mut peer);
            assert!(initial.subscribe && initial.argv.is_empty());
            write_record(peer.get_mut(), &ServerMessage::Snapshot {
                instance_id: "owner".into(), sequence: 1,
                state: snapshot_state(serde_json::json!({
                    "sessions": [{"id":1,"name":"focus","active":0,"windows":{"0":2}}],
                    "windows": [{"id":2,"name":"shell","active":7,"cols":20,"rows":3,"panes":[7],"layout":Layout::pane(7,20,3).to_legacy().unwrap()}],
                    "panes": [{"id":7,"window":2,"cols":20,"rows":3,"status":null,"terminal":tmuxx_terminal::Terminal::new(20,3,10).checkpoint_snapshot()}],
                })),
            }).unwrap();
            wait(&connection, |_| true);
            connection.focus_pane(1, 0, 7).unwrap();
            let focus = request_record(&mut peer);
            assert_eq!(focus.subscribe, !supported);
            assert_eq!(tmuxx_command::parse_argv(&focus.argv).unwrap().len(), 2);
            peer.get_ref().shutdown(std::net::Shutdown::Both).unwrap();
        }
    }

    #[test]
    fn native_handshake_rejection_never_submits_queued_requests() {
        for greeting in [
            b"{\"version\":4294967295}\n".as_slice(),
            b"invalid\n",
            b"{\"version\":1}",
            b"",
        ] {
            let (stream, peer) = std::os::unix::net::UnixStream::pair().unwrap();
            let connection =
                Connection::stream(stream.try_clone().unwrap(), stream, Arc::new(|| {})).unwrap();
            connection.command(["new-session", "-d"]).unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut peer = BufReader::new(peer);
            let _: Hello =
                tmuxx_protocol::native::decode(&record(&mut peer).unwrap().unwrap()).unwrap();
            peer.get_mut().write_all(greeting).unwrap();
            peer.get_ref().shutdown(std::net::Shutdown::Write).unwrap();
            let mut requests = Vec::new();
            peer.read_to_end(&mut requests).unwrap();
            assert!(requests.is_empty(), "requests escaped rejected handshake");
            let deadline = Instant::now() + Duration::from_secs(5);
            while !connection.is_closed() {
                assert!(Instant::now() < deadline, "rejected handshake stayed open");
                thread::sleep(Duration::from_millis(5));
            }
            assert!(!connection.errors().is_empty());
        }
    }

    #[test]
    fn native_command_identity_is_required_before_mutations() {
        for advertised in [true, false] {
            for actual in ["owner", "replacement"] {
                let (stream, peer) = std::os::unix::net::UnixStream::pair().unwrap();
                let connection = Connection::stream_inner(
                    stream.try_clone().unwrap(),
                    stream,
                    Arc::new(|| {}),
                    false,
                    false,
                    Some("owner".into()),
                )
                .unwrap();
                connection.watch_server().unwrap();
                let reply = connection
                    .command_with_reply(vec![b"new-session".to_vec(), b"-d".to_vec()])
                    .unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut peer = BufReader::new(peer);
                let hello: Hello =
                    tmuxx_protocol::native::decode(&record(&mut peer).unwrap().unwrap()).unwrap();
                assert!(hello.instance_id.is_none());
                peer.get_ref()
                    .set_read_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                assert!(matches!(
                    record(&mut peer).unwrap_err().kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ));
                peer.get_ref()
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                write_record(
                    peer.get_mut(),
                    &Hello {
                        instance_id: advertised.then(|| actual.into()),
                        ..Default::default()
                    },
                )
                .unwrap();
                if actual == "owner" && advertised {
                    let watch = request_record(&mut peer);
                    assert_eq!(watch.argv, [b"tmuxx-server-status".to_vec()]);
                    assert!(!watch.subscribe && !watch.control);
                    write_record(
                        peer.get_mut(),
                        &ServerMessage::Reply(tmuxx_protocol::native::Response {
                            request_id: watch.request_id,
                            output: Vec::new(),
                            status: None,
                            error: Some("unknown command: tmuxx-server-status".into()),
                        }),
                    )
                    .unwrap();
                    let request = request_record(&mut peer);
                    assert_eq!(request.argv[0], b"new-session");
                    assert!(!request.subscribe);
                    for message in [
                        ServerMessage::Reply(tmuxx_protocol::native::Response {
                            request_id: request.request_id,
                            output: b"$1 @2 %3".to_vec(),
                            status: None,
                            error: None,
                        }),
                        ServerMessage::PaneOutput {
                            sequence: 1,
                            pane_id: 0,
                            data: b"ignored command-only output".to_vec(),
                        },
                        ServerMessage::Changed { sequence: 2 },
                    ] {
                        write_record(peer.get_mut(), &message).unwrap();
                    }
                    assert_eq!(
                        reply.recv_timeout(Duration::from_secs(5)).unwrap().unwrap(),
                        b"$1 @2 %3",
                    );
                    peer.get_ref()
                        .set_read_timeout(Some(Duration::from_millis(100)))
                        .unwrap();
                    assert!(matches!(
                        record(&mut peer).unwrap_err().kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ));
                    assert!(connection.errors().is_empty());
                    assert!(connection.graph().is_none());
                    assert_eq!(connection.instance(), "owner");
                } else {
                    let mut requests = Vec::new();
                    peer.read_to_end(&mut requests).unwrap();
                    assert!(requests.is_empty(), "wrong owner received mutations");
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !connection.is_closed() {
                        assert!(Instant::now() < deadline, "wrong owner was accepted");
                        thread::sleep(Duration::from_millis(5));
                    }
                    assert!(connection.errors().join(";").contains("incarnation"));
                    assert!(
                        reply
                            .recv_timeout(Duration::from_secs(5))
                            .unwrap()
                            .unwrap_err()
                            .to_string()
                            .contains("incarnation")
                    );
                }
                drop(connection);
                let _ = peer.get_ref().shutdown(std::net::Shutdown::Both);
            }
        }
    }

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
    fn native_interaction_keeps_output_subscription_without_requesting_snapshots() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        let terminal = tmuxx_terminal::Terminal::new(20, 3, 20);
        apply(&connection.shared, ServerMessage::Snapshot {
            instance_id: "input".into(), sequence: 1,
            state: snapshot_state(serde_json::json!({
                "sessions": [{"id":1,"name":"input","active":0,"windows":{"0":2}}],
                "windows": [{"id":2,"name":"shell","active":7,"cols":20,"rows":3,"panes":[7],"layout":Layout::pane(7,20,3).to_legacy().unwrap()}],
                "panes": [{"id":7,"window":2,"cols":20,"rows":3,"status":null,"terminal":terminal.checkpoint_snapshot()}],
            })),
        }).unwrap();
        let view = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        let claim = requests.try_recv().unwrap();
        assert_eq!(claim.argv[0], b"tmuxx-claim-pane");
        assert!(!claim.subscribe && !claim.control);
        assert!(requests.try_recv().is_err());
        view.write_input(&Input::Text(b"x".to_vec())).unwrap();
        view.set_host_state(true, true, rustty_vt::query::ColorScheme::Dark)
            .unwrap();
        host_reply(
            &connection.shared,
            1,
            HostResponse::Read(rustty_vt::clipboard::ReadResult::Denied),
        )
        .unwrap();
        view.set_persistent(true).unwrap();
        let interactive = requests.try_iter().collect::<Vec<_>>();
        assert_eq!(interactive.len(), 4);
        assert!(
            interactive
                .iter()
                .all(|request| !request.subscribe && !request.control)
        );
        apply(
            &connection.shared,
            ServerMessage::PaneOutput {
                sequence: 2,
                pane_id: 7,
                data: b"x".to_vec(),
            },
        )
        .unwrap();
        assert!(view.terminal().unwrap().plain_text().contains('x'));
        assert!(requests.try_recv().is_err());
        connection.command(std::iter::empty::<&str>()).unwrap();
        assert!(requests.try_recv().unwrap().subscribe);
        for _ in 0..256 {
            view.write(b"x").unwrap();
        }
        assert!(
            view.set_host_state(false, false, rustty_vt::query::ColorScheme::Light)
                .is_err()
        );
        assert_eq!(requests.try_iter().count(), 256);
        view.set_host_state(false, false, rustty_vt::query::ColorScheme::Light)
            .unwrap();
        let retry = requests.try_recv().unwrap();
        assert_eq!(retry.argv[0], b"tmuxx-host-state");
        assert!(!retry.subscribe && !retry.control);
        assert!(requests.try_recv().is_err());
        view.close();
        let release = requests.try_recv().unwrap();
        assert_eq!(release.argv[0], b"tmuxx-release-pane");
        assert!(!release.subscribe && !release.control);
    }

    #[test]
    fn native_release_updates_other_views_but_only_releases_the_last_view() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        connection.shared.model.lock().unwrap().panes.insert(
            7,
            PaneState {
                host_state: None,
                owners: Arc::new(AtomicUsize::new(0)),
                terminal: tmuxx_terminal::Terminal::new(20, 4, 20).into_backend(),
                agent_status: tmuxx_terminal::AgentStatus::default(),
                views: Vec::new(),
            },
        );
        let first = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        let second = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        assert_eq!(requests.try_recv().unwrap().argv[0], b"tmuxx-claim-pane");
        assert!(requests.try_recv().is_err());
        first
            .set_host_state(true, true, rustty_vt::query::ColorScheme::Dark)
            .unwrap();
        requests.try_recv().unwrap();
        first.close();
        let blur = requests.try_recv().unwrap();
        assert_eq!(blur.argv[0], b"tmuxx-host-state");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&blur.stdin.unwrap()).unwrap(),
            serde_json::json!({"focused":false,"visible":false,"color_scheme":"dark"})
        );
        second
            .set_host_state(true, true, rustty_vt::query::ColorScheme::Dark)
            .unwrap();
        requests.try_recv().unwrap();
        second.close();
        // A partial release can finish its host update after the last owner released.
        first
            .set_host_state(false, false, rustty_vt::query::ColorScheme::Dark)
            .unwrap();
        drop(first);
        drop(second);
        assert_eq!(requests.try_recv().unwrap().argv[0], b"tmuxx-release-pane");
        assert!(requests.try_recv().is_err());

        let reopened = Session::attach_tmuxx(&connection, 7, Arc::new(|| {})).unwrap();
        reopened
            .set_host_state(true, true, rustty_vt::query::ColorScheme::Dark)
            .unwrap();
        assert_eq!(requests.try_recv().unwrap().argv[0], b"tmuxx-claim-pane");
        assert_eq!(requests.try_recv().unwrap().argv[0], b"tmuxx-host-state");
        connection.command(["kill-pane", "-t", "%7"]).unwrap();
        drop(reopened);
        assert_eq!(requests.try_recv().unwrap().argv[0], b"kill-pane");
        assert_eq!(requests.try_recv().unwrap().argv[0], b"tmuxx-release-pane");
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn native_agent_status_attach_live_and_resync_do_not_replay_host_actions() {
        use rustty_vt::agent::{Event, State};
        {
            let codex_frame = |event: &Event| {
                let frame = event.frame().unwrap();
                [
                    b"\x1bPtmux;\x1b".as_slice(),
                    &frame[..frame.len() - 2],
                    b"\x07\x1b\\",
                ]
                .concat()
            };
            let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
            let mut owner = tmuxx_terminal::Terminal::new(20, 4, 20);
            let begin =
                Event::from_json(br#"{"op":"begin","state":"working","thread_id":"thread"}"#)
                    .unwrap();
            owner.feed(b"\x1b]133;C\x07");
            owner.feed(&codex_frame(&begin));
            owner.feed(b"\x07\x1b]52;c;Y2xpcGJvYXJk\x07");
            let snapshot = |owner: &tmuxx_terminal::Terminal, sequence| ServerMessage::Snapshot {
                instance_id: "agent-owner".into(),
                sequence,
                state: snapshot_state(serde_json::json!({
                    "sessions": [{"id":1,"name":"agent","active":0,"windows":{"0":2}}],
                    "windows": [{"id":2,"name":"shell","active":7,"cols":20,"rows":4,"panes":[7],"layout":Layout::pane(7,20,4).to_legacy().unwrap()}],
                    "panes": [{"id":7,"window":2,"cols":20,"rows":4,"pid":123,"status":null,"terminal":(serde_json::to_value(owner.checkpoint_snapshot()).unwrap())}],
                })),
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
            let mut live = codex_frame(&update);
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
    }

    #[test]
    fn notification_observations_keep_command_order_across_split_reads() {
        let bytes = b"\x1b]133;C;cmdline_url=tix%20status\x07\x1b]9;4;3\x07\x1b]9;result\x07\x1b]777;notify;title;body\x07\x1b]133;D;0\x07";
        for split in 0..=bytes.len() {
            let mut terminal = tmuxx_terminal::Terminal::new(10, 4, 0).into_backend();
            let (sender, receiver) = mpsc::sync_channel(16);
            feed_view(&mut terminal, &bytes[..split], &sender);
            feed_view(&mut terminal, &bytes[split..], &sender);
            let events = receiver.try_iter().collect::<Vec<_>>();
            assert!(
                matches!(events.as_slice(), [
                SessionEvent::Effect(Effect::TmuxCommandStart { .. }),
                SessionEvent::Effect(Effect::CommandStart { command_line: Some(command) }),
                SessionEvent::Effect(Effect::Progress { state: 3, .. }),
                SessionEvent::NotificationObserved { title: first_title, body: first_body },
                SessionEvent::NotificationObserved { title, body },
                SessionEvent::Effect(Effect::CommandEnd { exit_code: Some(0) }),
            ] if command == b"tix status" && first_title.is_empty() && first_body == b"result" && title == b"title" && body == b"body"),
                "split {split}: {events:?}"
            );
        }
    }

    #[test]
    fn native_command_replies_are_registered_before_delivery() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        thread::scope(|scope| {
            let shared = &connection.shared;
            scope.spawn(move || {
                for index in 0..32 {
                    let request = requests.recv_timeout(Duration::from_secs(1)).unwrap();
                    apply(
                        shared,
                        ServerMessage::Reply(tmuxx_protocol::native::Response {
                            request_id: request.request_id,
                            output: vec![index],
                            status: None,
                            error: (index % 2 == 1).then(|| "command failed".into()),
                        }),
                    )
                    .unwrap();
                }
            });
            for index in 0..32 {
                let receiver = connection
                    .command_with_reply(vec![b"test".to_vec()])
                    .unwrap();
                let reply = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
                if index % 2 == 0 {
                    assert_eq!(reply.unwrap(), [index]);
                } else {
                    assert_eq!(reply.unwrap_err().to_string(), "command failed");
                }
                assert!(matches!(
                    receiver.recv_timeout(Duration::from_secs(1)),
                    Err(mpsc::RecvTimeoutError::Disconnected)
                ));
            }
        });
        assert!(
            connection
                .shared
                .model
                .lock()
                .unwrap()
                .pending_replies
                .is_empty()
        );
        assert!(connection.errors().is_empty());
    }

    #[test]
    fn native_command_replies_end_on_failure_detach_and_drop() {
        for mode in 0..3 {
            let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
            let receiver = connection
                .command_with_reply(vec![b"test".to_vec()])
                .unwrap();
            let shared = connection.shared.clone();
            match mode {
                0 => fail(&shared, "transport failed"),
                1 => apply(
                    &shared,
                    ServerMessage::Detached {
                        session: "test".into(),
                        hangup: false,
                    },
                )
                .unwrap(),
                _ => drop(connection),
            }
            assert!(
                receiver
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .is_err()
            );
            assert!(shared.model.lock().unwrap().pending_replies.is_empty());
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::TryRecvError::Disconnected)
            ));
        }
    }

    #[test]
    fn native_command_reply_send_failure_leaves_no_pending_reply() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        for _ in 0..256 {
            connection.command(["test"]).unwrap();
        }
        assert!(
            connection
                .command_with_reply(vec![b"test".to_vec()])
                .is_err()
        );
        drop(requests);
        assert!(
            connection
                .command_with_reply(vec![b"test".to_vec()])
                .is_err()
        );
        assert!(
            connection
                .shared
                .model
                .lock()
                .unwrap()
                .pending_replies
                .is_empty()
        );
    }

    #[test]
    fn native_command_errors_keep_connection_and_transport_errors_keep_live_panes() {
        let (connection, requests) = Connection::base(Arc::new(|| {}), true);
        let terminal = tmuxx_terminal::Terminal::new(20, 4, 20);
        apply(&connection.shared, ServerMessage::Snapshot {
            instance_id: "owner".into(), sequence: 1,
            state: snapshot_state(serde_json::json!({
                "sessions": [{"id":1,"name":"test","active":0,"windows":{"0":2}}],
                "windows": [{"id":2,"name":"shell","active":7,"cols":20,"rows":4,"panes":[7],"layout":Layout::pane(7,20,4).to_legacy().unwrap()}],
                "panes": [{"id":7,"window":2,"cols":20,"rows":4,"status":null,"terminal":terminal.checkpoint_snapshot()}],
            })),
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
        requests.try_iter().for_each(drop);
        fail(&connection.shared, "connection lost");
        assert!(connection.is_closed());
        assert!(!view.has_exited());
        for focused in [true, false, true] {
            view.set_host_state(true, focused, rustty_vt::query::ColorScheme::Dark)
                .unwrap();
        }
        assert_eq!(
            view.write(b"x").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        for input in [Input::Text(b"x".to_vec()), Input::Mouse(b"mouse".to_vec())] {
            assert_eq!(
                view.write_input(&input).unwrap_err().kind(),
                io::ErrorKind::BrokenPipe
            );
        }
        assert!(
            requests.try_recv().is_err(),
            "closed transport accepted more input"
        );
        assert!(
            matches!(view.events().next(), Some(SessionEvent::Error(message)) if message == "connection lost")
        );
        fail(&connection.shared, "duplicate worker error");
        assert_eq!(connection.errors(), ["connection lost"]);
        drop(connection);
        view.set_host_state(false, false, rustty_vt::query::ColorScheme::Light)
            .unwrap();
    }

    #[test]
    fn streamed_snapshot_restores_large_image_atomically_and_abort_keeps_live_view() {
        let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
        let mut terminal = tmuxx_terminal::Terminal::new(80, 24, 2000);
        terminal.feed(b"\x1b_Ga=T,i=7,f=32,s=1,v=1;/wAA/w==\x1b\\");
        let mut snapshot = terminal.checkpoint_snapshot();
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
            &snapshot_state(state),
            tmuxx_protocol::native::MAX_SNAPSHOT_BYTES,
        )
        .unwrap();
        assert!(wire.len() < 1024 * 1024, "uniform image should compress");
        let mut reader = BufReader::new(wire.as_slice());
        let mut snapshots = SnapshotAssembler::default();
        let mut completed = 0;
        let mut chunks = 0;
        while let Some(bytes) = record(&mut reader).unwrap() {
            let message = tmuxx_protocol::native::decode(&bytes).unwrap();
            chunks += usize::from(matches!(message, ServerMessage::SnapshotChunk { .. }));
            if let Some(message) = snapshots.push(message).unwrap() {
                apply(&connection.shared, message).unwrap();
                completed += 1;
            } else {
                assert!(connection.graph().is_none());
            }
        }
        snapshots.finish().unwrap();
        assert_eq!(completed, 1);
        assert!(chunks > 16);
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
    fn native_snapshot_restores_capture_lengths_after_non_reflow_shrink() {
        for (input, alternate) in [
            (b"\x1b[?7l01234567890123456789".as_slice(), false),
            (b"\x1b[?1049h01234567890123456789".as_slice(), true),
        ] {
            let mut terminal = tmuxx_terminal::Terminal::new(20, 3, 20);
            terminal.feed(input);
            terminal.resize(10, 3);
            if alternate {
                terminal.feed(b"\x1b[?1049l");
            }
            let (mut restored, _) = restore(&terminal.checkpoint_snapshot()).unwrap();
            let screen = if alternate {
                restored.alternate_screen().unwrap()
            } else {
                restored.primary_screen()
            };
            let row = screen.row(0);
            assert_eq!(screen.row_text(row), "0123456789");
            assert_eq!(screen.tmux_capture_bounds(row.id), Some((10, 20)));
            assert!(!restored.is_alternate_screen());
            restored.feed(b"\x1b[?7h");
            restored.resize(5, 3);
        }
    }

    #[test]
    fn native_snapshot_restores_capture_rows_after_reflow() {
        let mut terminal = tmuxx_terminal::Terminal::new(20, 5, 20);
        terminal.feed(b"abcdefghijklmnopqrstuvwxyz");
        terminal.resize(6, 2);
        terminal.feed(b"\x1b[M");
        terminal.resize(5, 1);
        terminal.feed(b"\x1b[2J");
        terminal.resize(18, 2);
        let (restored, _) = restore(&terminal.checkpoint_snapshot()).unwrap();
        assert_eq!((restored.cols, restored.rows), (18, 2));
    }

    #[test]
    fn native_snapshot_rejects_invalid_capture_metadata() {
        let terminal = tmuxx_terminal::Terminal::new(10, 3, 20);
        let value = serde_json::to_value(terminal.checkpoint_snapshot()).unwrap();
        let id = value["primary"]["rows"][0][0].as_u64().unwrap();
        for capture in [
            serde_json::json!([[id, [20, 19]]]),
            serde_json::json!([[id, [20, 65536]]]),
            serde_json::json!([[id, [-1, 20]]]),
            serde_json::json!([[999999, [20, 20]]]),
            serde_json::json!([[id, [20, 20]], [id, [20, 20]]]),
            serde_json::json!([[id, [20, 20, 20]]]),
        ] {
            let mut invalid = value.clone();
            invalid["primary"]["tmux_capture_rows"] = capture;
            if let Ok(snapshot) =
                serde_json::from_value::<tmuxx_terminal::CheckpointSnapshot>(invalid)
            {
                assert!(restore(&snapshot).is_err());
            }
        }
    }

    #[test]
    fn native_snapshot_preserves_row_ids_and_image_anchors_after_resize() {
        let mut terminal = tmuxx_terminal::Terminal::new(16, 20, 2000);
        terminal.feed(b"abcdefghijklmnopqrstuvwx\r\n\x1b_Ga=T,i=7,f=32,s=1,v=1;/wAA/w==\x1b\\");
        for cols in [8, 16, 8, 16] {
            terminal.resize(cols, 20);
            let expected = terminal.snapshot();
            let (restored, _) = restore(&terminal.checkpoint_snapshot()).unwrap();
            let ids = restored
                .primary_screen()
                .all_rows()
                .map(|r| r.id)
                .collect::<Vec<_>>();
            assert_eq!(
                ids,
                expected
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
        {
            let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
            let mut terminal = tmuxx_terminal::Terminal::new(12, 3, 2000);
            terminal.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07build\r\n\x1b]133;C\x07retained output with wrap\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ");
            terminal.resize(10, 3);
            apply(&connection.shared, ServerMessage::Snapshot { instance_id: "search".into(), sequence: 1, state: snapshot_state(serde_json::json!({
            "sessions": [{"id":1,"name":"test","active":0,"windows":{"0":2}}],
            "windows": [{"id":2,"name":"shell","active":7,"cols":10,"rows":3,"panes":[7],"layout":Layout::pane(7,10,3).to_legacy().unwrap()}],
            "panes": [{"id":7,"window":2,"cols":10,"rows":3,"status":null,"terminal":(serde_json::to_value(terminal.checkpoint_snapshot()).unwrap())}],
        })) }).unwrap();
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
    }

    #[test]
    fn native_views_preserve_wide_cells_viewports_sync_modes_and_stationary_cursor_changes() {
        let (connection, _requests) = Connection::base(Arc::new(|| {}), true);
        let mut owner = tmuxx_terminal::Terminal::new(10, 3, 100);
        owner.feed("old one\r\nold two\r\nold three\r\nwide 你\r\nlast".as_bytes());
        let snapshot = |owner: &mut tmuxx_terminal::Terminal, sequence| ServerMessage::Snapshot {
            instance_id: "damage".into(),
            sequence,
            state: snapshot_state(serde_json::json!({
                "sessions": [{"id":1,"name":"test","active":0,"windows":{"0":2}}],
                "windows": [{"id":2,"name":"shell","active":7,"cols":10,"rows":3,"panes":[7],"layout":Layout::pane(7,10,3).to_legacy().unwrap()}],
                "panes": [{"id":7,"window":2,"cols":10,"rows":3,"status":null,"terminal":owner.checkpoint_snapshot()}],
            })),
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
            apply(&connection.shared, ServerMessage::Snapshot { instance_id: "detach".into(), sequence: 1, state: snapshot_state(serde_json::json!({
                "sessions": [{"id":1,"name":"test","active":0,"windows":{"0":2}}],
                "windows": [{"id":2,"name":"shell","active":7,"cols":80,"rows":24,"panes":[7],"layout":Layout::pane(7,80,24).to_legacy().unwrap()}],
                "panes": [{"id":7,"window":2,"cols":80,"rows":24,"status":null,"terminal":terminal.checkpoint_snapshot()}],
            })) }).unwrap();
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
                state: snapshot_state(serde_json::json!({
                    "sessions": [{"id": 1,"name":"test","active":0,"windows":{"0":2}}],
                    "windows": [{"id":2,"name":"shell","active":7,"cols":80,"rows":24,"panes":[7],"layout":Layout::pane(7,80,24).to_legacy().unwrap()}],
                    "panes": [{"id":7,"window":2,"cols":80,"rows":24,"pid":123,"status":null,"terminal":terminal.checkpoint_snapshot()}],
                })),
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
        let snapshot = terminal.checkpoint_snapshot();
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
                state: snapshot_state(state.clone()),
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
                state: snapshot_state(state),
            },
        )
        .unwrap();
        assert_eq!(first.events().count(), 0);
        assert_eq!(second.events().count(), 0);
    }

    #[test]
    fn embedded_configuration_applies_before_first_session_and_later_remote_panes() {
        let directory =
            std::env::temp_dir().join(format!("rustty-tmuxx-config-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("server settings.conf");
        std::fs::write(&path, "set -g default-shell /bin/sh\nset -g default-command 'printf CONFIGURED; exec /bin/sh'\n").unwrap();
        let endpoint = directory.join("server.native");
        let mut options = ServerOptions::new(&endpoint);
        options.config.push(path.clone());
        let connection =
            Connection::embedded_with_options(options, Arc::new(|| {}), Vec::new()).unwrap();
        let graph = wait(&connection, |g| g.panes.len() == 1);
        let first = graph.panes[0].id;
        let session = Session::attach_tmuxx(&connection, first, Arc::new(|| {})).unwrap();
        wait_text(&connection, &session, "CONFIGURED");

        // Attaching an external client neither rereads the file nor resets defaults.
        std::fs::write(
            &path,
            "set -g default-command 'printf RELOADED; exec /bin/sh'\n",
        )
        .unwrap();
        let remote = Connection::native(&endpoint, Arc::new(|| {})).unwrap();
        remote
            .command(["split-window", "-d", "-t", &format!("%{first}")])
            .unwrap();
        let graph = wait(&remote, |g| g.panes.len() == 2);
        let second = graph.panes.iter().find(|p| p.id != first).unwrap().id;
        let session = Session::attach_tmuxx(&remote, second, Arc::new(|| {})).unwrap();
        wait_text(&remote, &session, "CONFIGURED");
        drop(remote);
        drop(connection);
        std::fs::remove_dir_all(directory).unwrap();
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
    fn native_notifications_are_observed_by_all_views_and_delivered_to_one_host() {
        let connection = test_owner(
            "import os,tty; tty.setraw(0); os.write(1,b'READY'); os.read(0,1); os.write(1,b'\\x1b]133;C;cmdline_url=tix%20status\\x07\\x1b]9;4;3\\x07\\x1b]9;result\\x07\\x1b]777;notify;title;body\\x07\\x1b]133;D;0\\x07FINISHED'); os.read(0,1)",
        );
        let graph = wait(&connection, |graph| !graph.panes.is_empty());
        let pane = graph.panes[0].id;
        let first = Session::attach_tmuxx(&connection, pane, Arc::new(|| {})).unwrap();
        let second = Session::attach_tmuxx(&connection, pane, Arc::new(|| {})).unwrap();
        wait_text(&connection, &first, "READY");
        second
            .set_host_state(true, true, rustty_vt::query::ColorScheme::Dark)
            .unwrap();
        let Source::Unix(endpoint) = &connection.source else {
            panic!("missing endpoint")
        };
        let remote = Connection::native(endpoint, Arc::new(|| {})).unwrap();
        wait(&remote, |graph| graph.panes.iter().any(|p| p.id == pane));
        let third = Session::attach_tmuxx(&remote, pane, Arc::new(|| {})).unwrap();
        third
            .set_host_state(true, false, rustty_vt::query::ColorScheme::Dark)
            .unwrap();
        remote
            .command_with_reply(Vec::new())
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        first.write(b"x").unwrap();
        for (connection, session, delivered) in [
            (&connection, &first, 0),
            (&connection, &second, 2),
            (&remote, &third, 0),
        ] {
            wait_text(connection, session, "FINISHED");
            let events = session.events().collect::<Vec<_>>();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, SessionEvent::NotificationDelivery { .. }))
                    .count(),
                delivered,
                "{events:?}"
            );
            let observations = events
                .into_iter()
                .filter(|event| !matches!(event, SessionEvent::NotificationDelivery { .. }))
                .collect::<Vec<_>>();
            assert!(
                matches!(observations.as_slice(), [
                SessionEvent::Effect(Effect::TmuxCommandStart { .. }),
                SessionEvent::Effect(Effect::CommandStart { command_line: Some(command) }),
                SessionEvent::Effect(Effect::Progress { state: 3, .. }),
                SessionEvent::NotificationObserved { title: first_title, body: first_body },
                SessionEvent::NotificationObserved { title, body },
                SessionEvent::Effect(Effect::CommandEnd { exit_code: Some(0) }),
            ] if command == b"tix status" && first_title.is_empty() && first_body == b"result" && title == b"title" && body == b"body"),
                "{observations:?}"
            );
        }
        let attached = Session::attach_tmuxx(&remote, pane, Arc::new(|| {})).unwrap();
        assert_eq!(attached.events().count(), 0);
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
    fn native_size_views_keep_the_first_owner_until_fit_or_disconnect() {
        let owner = test_owner("import time; time.sleep(30)");
        let graph = wait(&owner, |g| !g.panes.is_empty());
        let session = graph.sessions[0].id;
        let Source::Unix(endpoint) = &owner.source else {
            panic!("missing endpoint");
        };
        let remote = Connection::native(endpoint, Arc::new(|| {})).unwrap();
        wait(&remote, |g| !g.panes.is_empty());
        assert!(remote.shared.model.lock().unwrap().view_size_ownership);
        assert!(remote.fit_view_size(session).is_err());
        let size = |connection: &Connection| {
            connection.size_views.lock().unwrap()[&session]
                .0
                .command_with_reply(
                    ["display-message", "-p", "#{window_width}x#{window_height}"]
                        .map(|arg| arg.as_bytes().to_vec())
                        .to_vec(),
                )
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap()
        };
        owner.view_size(session, 100, 40, 8, 16).unwrap();
        assert_eq!(size(&owner), b"100x40\n");
        remote.view_size(session, 60, 20, 8, 16).unwrap();
        assert_eq!(size(&remote), b"100x40\n");
        let view = Session::attach_tmuxx(&remote, graph.panes[0].id, Arc::new(|| {})).unwrap();
        view.set_host_state(true, true, rustty_vt::query::ColorScheme::Dark)
            .unwrap();
        view.write(b"input").unwrap();
        let flushed = remote.command_with_reply(Vec::new()).unwrap();
        flushed
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(size(&remote), b"100x40\n");
        owner.view_size(session, 90, 35, 8, 16).unwrap();
        assert_eq!(size(&owner), b"90x35\n");

        remote.fit_view_size(session).unwrap();
        assert_eq!(size(&remote), b"60x20\n");
        owner.view_size(session, 110, 45, 8, 16).unwrap();
        assert_eq!(size(&owner), b"60x20\n");
        remote.view_size(session, 70, 25, 8, 16).unwrap();
        assert_eq!(size(&remote), b"70x25\n");
        remote.release_view(session).unwrap();
        wait(&owner, |g| {
            g.windows[0].cols == 110 && g.windows[0].rows == 45
        });
        assert_eq!(size(&owner), b"110x45\n");
        assert!(remote.fit_view_size(session).is_err());
    }

    #[test]
    fn embedded_size_clients_use_the_owner_and_release_their_size_context() {
        let connection = test_owner("import time; time.sleep(30)");
        let graph = wait(&connection, |g| !g.panes.is_empty());
        assert_eq!(connection.view_connection_count(), Some(1));
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
        assert_eq!(connection.view_connection_count(), Some(2));
        connection.release_view(session).unwrap();
        assert_eq!(connection.view_connection_count(), Some(1));
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
    fn native_size_clients_reject_replaced_owners_with_reused_session_ids() {
        let original = test_owner("import time; time.sleep(30)");
        let Source::Unix(endpoint) = &original.source else {
            panic!("missing endpoint");
        };
        let endpoint = endpoint.clone();
        let stale = Connection::native(&endpoint, Arc::new(|| {})).unwrap();
        let graph = wait(&stale, |g| !g.panes.is_empty());
        let session = graph.sessions[0].id;
        let original_instance = stale.instance();
        drop(original);

        let replacement = Connection::embedded(&endpoint, Arc::new(|| {})).unwrap();
        let graph = wait(&replacement, |g| !g.panes.is_empty());
        assert_eq!(graph.sessions[0].id, session);
        assert_ne!(replacement.instance(), original_instance);
        let initial_size = (graph.windows[0].cols, graph.windows[0].rows);
        let deadline = Instant::now() + Duration::from_secs(5);
        let error = loop {
            if let Err(error) = stale.view_size(session, 51, 13, 8, 16) {
                break error;
            }
            assert!(Instant::now() < deadline, "stale size client was accepted");
            thread::sleep(Duration::from_millis(5));
        };
        let error = error.to_string();
        assert!(
            error.contains("incarnation") || error == "tmuxx connection is closed",
            "{error}"
        );
        for _ in 0..3 {
            assert!(stale.view_size(session, 51, 13, 8, 16).is_err());
        }
        let graph = replacement.graph().unwrap();
        assert_eq!((graph.windows[0].cols, graph.windows[0].rows), initial_size);

        let fresh = Connection::native(&endpoint, Arc::new(|| {})).unwrap();
        wait(&fresh, |g| !g.panes.is_empty());
        fresh.view_size(session, 60, 20, 8, 16).unwrap();
        wait(&replacement, |g| {
            g.windows[0].cols == 60 && g.windows[0].rows == 20
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
        let (_, agent_status) = restore(graph.panes[0].terminal.as_deref().unwrap()).unwrap();
        assert!(agent_status.snapshot.is_none());
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
        assert_eq!(graph.panes[0].terminal.as_ref().unwrap().version, 2);
        assert!(
            graph.panes[0]
                .terminal
                .as_ref()
                .unwrap()
                .checkpoint
                .is_some()
        );
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
        assert_eq!(graph.panes[0].terminal.as_ref().unwrap().version, 2);
        assert!(
            graph.panes[0]
                .terminal
                .as_ref()
                .unwrap()
                .checkpoint
                .is_some()
        );
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
