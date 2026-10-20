//! OS ownership around the deterministic engine. Dropping a host stops its panes.

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    mpsc::{self, Receiver, SyncSender, TrySendError},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
pub use tmuxx_core::ClientContext;
use tmuxx_core::{
    Effect, Engine, HostAction, HostResponse, Id, Notification as EngineNotification, Outcome,
};
use tmuxx_protocol::native::{Hello, Request, Response, ServerMessage, VERSION};

mod client_commands;
mod control;
mod defaults;
mod files;
mod format_jobs;
mod hooks;
mod lifecycle;
mod messages;
mod names;
mod process;
mod reports;
mod scheduler;
mod sizes;
#[cfg(test)]
mod snapshots;
#[cfg(unix)]
mod unix_ipc;
mod view_state;
#[cfg(target_os = "linux")]
#[path = "linux_watchdog.rs"]
mod watchdog;
#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
mod windows_conpty;
#[cfg(windows)]
mod windows_host;
#[cfg(windows)]
mod windows_job;
#[cfg(windows)]
pub use windows_conpty::{ConptyIdentity, initialize_conpty};
#[cfg(windows)]
pub use windows_job::{OwnedStdioChild, spawn_owned_stdio, spawn_owned_stdio_piped_stderr};
#[cfg(unix)]
type NativeStream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type NativeStream = windows::PipeStream;
#[cfg(target_os = "macos")]
mod watchdog;

pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Options {
    pub endpoint: PathBuf,
    /// Optional tmux-compatible Unix IPC listener, separate from the native endpoint.
    pub tmux_endpoint: Option<PathBuf>,
    pub config: Vec<PathBuf>,
    pub exit_empty: bool,
    /// Total serialized snapshot budget; stream frames remain independently bounded.
    pub snapshot_bytes: u64,
}

impl Options {
    pub fn new(endpoint: impl Into<PathBuf>) -> Self {
        Self {
            endpoint: endpoint.into(),
            tmux_endpoint: None,
            config: Vec::new(),
            exit_empty: false,
            snapshot_bytes: tmuxx_protocol::native::MAX_SNAPSHOT_BYTES,
        }
    }
}

enum Event {
    SnapshotSent(u64),
    FileResult {
        id: u64,
        request_id: u64,
        result: Result<Vec<u8>, String>,
    },
    Stdin {
        id: u64,
        request_id: u64,
        data: Result<Option<Vec<u8>>, String>,
        ack: SyncSender<Result<(), String>>,
    },
    Connect {
        files: bool,
        stdin_files: bool,
        id: u64,
        tx: SyncSender<ServerMessage>,
        context: Box<ClientContext>,
        delivery_bytes: Arc<AtomicUsize>,
    },
    Request {
        id: u64,
        request: Request,
    },
    #[cfg(unix)]
    PrepareControlLine {
        id: u64,
        source: Vec<u8>,
        reply: SyncSender<Result<Vec<tmuxx_command::Command>, String>>,
    },
    #[cfg(unix)]
    ControlRequest {
        id: u64,
        request: Request,
        commands: Option<Result<Vec<tmuxx_command::Command>, String>>,
        flags: u64,
    },
    ControlSource {
        id: u64,
        request: tmuxx_protocol::native::ControlRequest,
    },
    Disconnect(u64),
    Output(Id, u64, Vec<u8>),
    ReadClosed(Id, u64),
    Exited(Id, u64, i32, Option<String>),
    InputFailed(Id, u64, String),
    PipeInput(Id, u64, Vec<u8>),
    PipeClosed(Id, u64),
    PipeExited(Id, u64),
    PipeDrained(Id),
    JobFinished(scheduler::JobResult),
    FormatJobLine(u64, Vec<u8>),
    Shutdown,
}

#[derive(Clone)]
struct Client {
    tx: SyncSender<ServerMessage>,
    context: ClientContext,
    subscribed: bool,
    control: bool,
    delivery_bytes: Arc<AtomicUsize>,
}

struct Process {
    #[cfg(windows)]
    handle: Option<std::os::windows::io::OwnedHandle>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    _watchdog: Option<watchdog::Registration>,
    #[cfg(all(unix, not(target_os = "linux")))]
    pid: Option<u32>,
    generation: u64,
    master: Box<dyn MasterPty + Send>,
    input: SyncSender<Vec<u8>>,
    input_bytes: Arc<AtomicUsize>,
    stop: SyncSender<()>,
    finished: Receiver<()>,
    status: Option<i32>,
    signal: Option<String>,
    eof: bool,
}

struct PendingHostAction {
    client: u64,
    pane: Id,
    action: HostAction,
    deadline: Instant,
}

impl Drop for Process {
    fn drop(&mut self) {
        #[cfg(all(unix, not(target_os = "linux")))]
        let mut groups = process::session_groups(self.pid, self.status.is_some());
        #[cfg(all(unix, not(target_os = "linux")))]
        groups.push(self.master.process_group_leader());
        #[cfg(all(unix, not(target_os = "linux")))]
        process::signal_groups(&groups, libc::SIGHUP);
        #[cfg(target_os = "linux")]
        if let Some(registration) = &self._watchdog {
            let _ = registration.signal(libc::SIGHUP);
        }
        let _ = self.stop.try_send(());
        let _ = self.finished.recv_timeout(Duration::from_secs(2));
        #[cfg(all(unix, not(target_os = "linux")))]
        process::signal_groups(&groups, libc::SIGKILL);
        #[cfg(target_os = "linux")]
        if let Some(registration) = &self._watchdog {
            let _ = registration.signal(libc::SIGKILL);
        }
    }
}

/// A real app-owned host: no global runtime, daemonization, or process ownership leak.
pub struct ServerHandle {
    tx: SyncSender<Event>,
    next_client: Arc<AtomicU64>,
    thread: Option<JoinHandle<io::Result<()>>>,
    pub endpoint: PathBuf,
}

impl ServerHandle {
    /// Connect a typed in-process view to the same owner loop as remote clients.
    pub fn connect(&self) -> io::Result<LocalClient> {
        self.connect_with_context(ClientContext::default())
    }
    /// Supply immutable per-client cwd and environment without changing host defaults.
    pub fn connect_with_context(&self, context: ClientContext) -> io::Result<LocalClient> {
        let id = self.next_client.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::sync_channel(256);
        let delivery_bytes = Arc::new(AtomicUsize::new(0));
        self.tx
            .send(Event::Connect {
                files: false,
                stdin_files: false,
                id,
                tx,
                context: Box::new(context),
                delivery_bytes: delivery_bytes.clone(),
            })
            .map_err(|_| io::Error::other("server stopped"))?;
        Ok(LocalClient {
            id,
            tx: self.tx.clone(),
            rx,
            delivery_bytes,
        })
    }
    pub fn shutdown(mut self) -> io::Result<()> {
        let _ = self.tx.send(Event::Shutdown);
        self.join()
    }
    pub fn wait(mut self) -> io::Result<()> {
        self.join()
    }
    fn join(&mut self) -> io::Result<()> {
        self.thread
            .take()
            .map(|thread| {
                thread
                    .join()
                    .unwrap_or_else(|_| Err(io::Error::other("server thread panicked")))
            })
            .unwrap_or(Ok(()))
    }
}

pub struct LocalClient {
    id: u64,
    tx: SyncSender<Event>,
    rx: Receiver<ServerMessage>,
    delivery_bytes: Arc<AtomicUsize>,
}

impl LocalClient {
    /// Supply one bounded chunk (or EOF) for a ServerMessage::Stdin request.
    pub fn stdin(&self, request_id: u64, data: Option<Vec<u8>>) -> io::Result<()> {
        self.stdin_result(request_id, Ok(data))
    }
    fn stdin_result(
        &self,
        request_id: u64,
        data: Result<Option<Vec<u8>>, String>,
    ) -> io::Result<()> {
        if data
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .is_some_and(|bytes| bytes.len() > 65536)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "stdin chunk exceeds limit",
            ));
        }
        let (ack, done) = mpsc::sync_channel(1);
        self.tx
            .send(Event::Stdin {
                id: self.id,
                request_id,
                data,
                ack,
            })
            .map_err(|_| io::Error::other("server stopped"))?;
        done.recv()
            .map_err(|_| io::Error::other("server stopped"))?
            .map_err(io::Error::other)
    }
    pub fn send(&self, request: Request) -> io::Result<()> {
        self.tx
            .try_send(Event::Request {
                id: self.id,
                request,
            })
            .map_err(|e| {
                io::Error::new(
                    match e {
                        TrySendError::Full(_) => io::ErrorKind::WouldBlock,
                        TrySendError::Disconnected(_) => io::ErrorKind::BrokenPipe,
                    },
                    "server request queue unavailable",
                )
            })
    }
    /// Submit a native control request through the owner's parser and command
    /// queue, retaining per-command control guards and prepared source blocks.
    pub fn send_control(&self, request: tmuxx_protocol::native::ControlRequest) -> io::Result<()> {
        if request.source.is_some() && !request.argv.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "control source and argv are mutually exclusive",
            ));
        }
        self.tx
            .try_send(Event::ControlSource {
                id: self.id,
                request,
            })
            .map_err(|error| {
                io::Error::new(
                    match error {
                        TrySendError::Full(_) => io::ErrorKind::WouldBlock,
                        TrySendError::Disconnected(_) => io::ErrorKind::BrokenPipe,
                    },
                    "server control request queue unavailable",
                )
            })
    }
    #[cfg(unix)]
    fn send_wait(&self, request: Request) -> io::Result<()> {
        // External transport readers can wait; only the owner thread executes requests.
        self.tx
            .send(Event::Request {
                id: self.id,
                request,
            })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "server stopped"))
    }
    #[cfg(unix)]
    fn prepare_control_line(
        &self,
        source: Vec<u8>,
    ) -> io::Result<Result<Vec<tmuxx_command::Command>, String>> {
        let (reply, prepared) = mpsc::sync_channel(1);
        self.tx
            .send(Event::PrepareControlLine {
                id: self.id,
                source,
                reply,
            })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "server stopped"))?;
        prepared
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "server stopped"))
    }
    #[cfg(unix)]
    fn send_control_wait(
        &self,
        request: Request,
        commands: Option<Result<Vec<tmuxx_command::Command>, String>>,
        flags: u64,
    ) -> io::Result<()> {
        self.tx
            .send(Event::ControlRequest {
                id: self.id,
                request,
                commands,
                flags,
            })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "server stopped"))
    }
    pub fn recv(&self) -> io::Result<ServerMessage> {
        self.rx
            .recv()
            .map(|message| self.received(message))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "server stopped"))
    }
    pub fn try_recv(&self) -> Result<ServerMessage, mpsc::TryRecvError> {
        self.rx.try_recv().map(|message| self.received(message))
    }
    fn received(&self, message: ServerMessage) -> ServerMessage {
        let bytes = control::message_bytes(&message);
        let _ = self
            .delivery_bytes
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.saturating_sub(bytes))
            });
        if matches!(message, ServerMessage::Snapshot { .. }) {
            let _ = self.tx.send(Event::SnapshotSent(self.id));
        }
        message
    }
}

impl Drop for LocalClient {
    fn drop(&mut self) {
        let _ = self.tx.send(Event::Disconnect(self.id));
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.tx.send(Event::Shutdown);
            let _ = self.join();
        }
    }
}

#[cfg(unix)]
struct EndpointGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
    _lock: std::fs::File,
}

#[cfg(unix)]
impl EndpointGuard {
    fn bind(path: &Path) -> io::Result<(Self, std::os::unix::net::UnixListener)> {
        use std::os::unix::{
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
            net::{UnixListener, UnixStream},
        };
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(lock_path)?;
        let metadata = lock.metadata()?;
        if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "endpoint lock has a different owner or type",
            ));
        }
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => {
                io::Error::new(io::ErrorKind::WouldBlock, "endpoint is locked")
            }
            std::fs::TryLockError::Error(error) => error,
        })?;
        let listener = match UnixListener::bind(path) {
            Ok(listener) => listener,
            Err(error) => {
                let owned_socket = std::fs::symlink_metadata(path).is_ok_and(|metadata| {
                    metadata.file_type().is_socket() && metadata.uid() == unsafe { libc::geteuid() }
                });
                if !owned_socket
                    || !UnixStream::connect(path)
                        .is_err_and(|error| error.kind() == io::ErrorKind::ConnectionRefused)
                {
                    return Err(error);
                }
                // The lifetime lock serializes stale cleanup with both bind and
                // normal shutdown; a live server is never unlinked.
                std::fs::remove_file(path)?;
                UnixListener::bind(path)?
            }
        };
        let metadata = std::fs::symlink_metadata(path)?;
        Ok((
            Self {
                path: path.to_owned(),
                device: metadata.dev(),
                inode: metadata.ino(),
                _lock: lock,
            },
            listener,
        ))
    }
}

#[cfg(unix)]
impl Drop for EndpointGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        if std::fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
        }) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(unix)]
struct AcceptGuard {
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    connections: Arc<Mutex<BTreeMap<u64, std::os::unix::net::UnixStream>>>,
}

#[cfg(unix)]
impl Drop for AcceptGuard {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        while !self.connections.lock().unwrap().is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        for stream in self.connections.lock().unwrap().values() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Maximum concurrent socket clients, including peers still identifying.
pub const MAX_SOCKET_CLIENTS: usize = 128;

#[cfg(unix)]
struct ConnectionGuard {
    id: u64,
    connections: Arc<Mutex<BTreeMap<u64, std::os::unix::net::UnixStream>>>,
}

#[cfg(unix)]
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.connections.lock().unwrap().remove(&self.id);
    }
}

#[cfg(unix)]
pub fn start(options: Options) -> io::Result<ServerHandle> {
    let environment = host_environment(std::env::vars_os())?;
    use std::os::unix::fs::PermissionsExt;
    let mut listeners = Vec::new();
    let mut paths = Vec::new();
    for (path, ipc) in std::iter::once((&options.endpoint, false))
        .chain(options.tmux_endpoint.iter().map(|path| (path, true)))
    {
        let result = (|| {
            let (guard, listener) = EndpointGuard::bind(path)?;
            paths.push(guard);
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            listener.set_nonblocking(true)?;
            listeners.push((listener, ipc));
            Ok::<_, io::Error>(())
        })();
        result?;
    }
    let endpoint = options.endpoint.clone();
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let watchdog = watchdog::Watchdog::start()?;
    let (tx, rx) = mpsc::sync_channel(1024);
    let next_client = Arc::new(AtomicU64::new(0));
    let accept_ids = next_client.clone();
    let stopped = Arc::new(AtomicBool::new(false));
    let accept_stop = stopped.clone();
    let accept_tx = tx.clone();
    let connections = Arc::new(Mutex::new(BTreeMap::new()));
    let accepted_connections = connections.clone();
    let accept = thread::Builder::new()
        .name("tmuxx-accept".into())
        .spawn(move || {
            while !accept_stop.load(Ordering::Acquire) {
                for (listener, ipc) in &listeners {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            // Darwin accepts inherit the listener's nonblocking flag.
                            if stream.set_nonblocking(false).is_err() || !same_user(&stream) {
                                continue;
                            }
                            let id = accept_ids.fetch_add(1, Ordering::Relaxed);
                            let guard = {
                                let mut connections = accepted_connections.lock().unwrap();
                                if connections.len() >= MAX_SOCKET_CLIENTS {
                                    continue;
                                }
                                let Ok(socket) = stream.try_clone() else {
                                    continue;
                                };
                                connections.insert(id, socket);
                                ConnectionGuard {
                                    id,
                                    connections: accepted_connections.clone(),
                                }
                            };
                            let tx = accept_tx.clone();
                            let ipc = *ipc;
                            let _ = thread::Builder::new()
                                .name(format!("tmuxx-client-{id}"))
                                .spawn(move || {
                                    let _guard = guard;
                                    let result = if ipc {
                                        unix_ipc::serve(stream, id, tx)
                                    } else {
                                        serve_connection(stream, id, tx, None)
                                    };
                                    if let Err(error) = result {
                                        eprintln!("tmuxx: connection {id}: {error}");
                                    }
                                });
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(_) => return,
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
        })?;
    let accept = AcceptGuard {
        stopped: stopped.clone(),
        thread: Some(accept),
        connections,
    };
    let engine_tx = tx.clone();
    let engine = thread::Builder::new()
        .name("tmuxx-owner".into())
        .spawn(move || {
            let _paths = paths;
            let accept = accept;
            let mut host = Host::new(engine_tx, &options, environment);
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                host.watchdog = Some(watchdog);
            }
            let result = host.run(rx, &options);
            stopped.store(true, Ordering::Release);
            host.scheduler.shutdown();
            host.processes.clear();
            for client in host.clients.values() {
                let _ = client.tx.try_send(ServerMessage::Exit {
                    reason: "server exited".into(),
                });
            }
            host.clients.clear();
            drop(accept);
            result
        })?;
    Ok(ServerHandle {
        tx,
        next_client,
        thread: Some(engine),
        endpoint,
    })
}

#[cfg(windows)]
pub fn start(options: Options) -> io::Result<ServerHandle> {
    windows_host::start(options)
}

#[cfg(not(any(unix, windows)))]
pub fn start(_options: Options) -> io::Result<ServerHandle> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native endpoint transport is not implemented on this platform",
    ))
}

#[cfg(unix)]
fn same_user(stream: &std::os::unix::net::UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    unsafe {
        let mut uid = 0;
        let mut gid = 0;
        libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) == 0 && uid == libc::geteuid()
    }
    #[cfg(target_os = "linux")]
    unsafe {
        let mut creds: libc::ucred = std::mem::zeroed();
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut creds as *mut libc::ucred).cast(),
            &mut len,
        ) == 0
            && creds.uid == libc::geteuid()
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "linux"
    )))]
    {
        let _ = stream;
        false
    }
}

/// Whether argv asks to read a buffer or configuration from the client's stdin.
pub fn command_reads_stdin(argv: &[Vec<u8>]) -> bool {
    tmuxx_command::parse_argv(argv).is_ok_and(|commands| {
        commands.iter().any(|command| {
            let Some(entry) = command
                .args
                .first()
                .and_then(|name| tmuxx_command::registry::find(name).ok())
            else {
                return false;
            };
            let template = match entry.name {
                "load-buffer" => "b:t:w",
                "source-file" => "Fnqt:v",
                _ => return false,
            };
            tmuxx_command::Args::parse(&command.args[1..], template)
                .is_ok_and(|args| args.rest.iter().any(|arg| arg == b"-"))
        })
    })
}

/// Preserve tokenized commands when crossing the native argv interface.
pub fn command_argv(commands: &[tmuxx_command::Command]) -> Vec<Vec<u8>> {
    let mut argv = Vec::new();
    for (index, command) in commands.iter().enumerate() {
        if index > 0 {
            argv.push(b";".to_vec());
        }
        for arg in &command.args {
            let mut arg = arg.clone();
            if arg.last() == Some(&b';') {
                arg.insert(arg.len() - 1, b'\\');
            }
            argv.push(arg);
        }
    }
    argv
}

/// Negotiate a native connection before sending any requests.
pub fn native_handshake(stream: &mut (impl Read + Write)) -> io::Result<Hello> {
    native_handshake_with_file_stdin(stream, false)
}

/// Opt into command-driven, bounded stdin transfers for source-file/load-buffer.
pub fn native_handshake_with_file_stdin(
    stream: &mut (impl Read + Write),
    stdin_file_stream: bool,
) -> io::Result<Hello> {
    native_handshake_with_features(stream, stdin_file_stream, None)
}

/// Negotiate native stdin and an optional C-compatible client feature identity.
/// Older servers must explicitly acknowledge identity support before commands.
pub fn native_handshake_with_features(
    stream: &mut (impl Read + Write),
    stdin_file_stream: bool,
    client_features: Option<i32>,
) -> io::Result<Hello> {
    let cwd = std::env::current_dir()?
        .into_os_string()
        .into_string()
        .map_err(|_| io::Error::new(io::ErrorKind::Unsupported, "native cwd must be UTF-8"))?;
    let environment = std::env::vars_os()
        .map(|(key, value)| {
            Ok((
                key.into_string().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        "native environment keys must be UTF-8",
                    )
                })?,
                value.into_string().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        "native environment values must be UTF-8",
                    )
                })?,
            ))
        })
        .collect::<io::Result<_>>()?;
    let mut output = io::BufWriter::new(&mut *stream);
    serde_json::to_writer(
        &mut output,
        &Hello {
            cwd: Some(cwd),
            environment,
            stdin_file_stream,
            client_features,
            ..Hello::default()
        },
    )?;
    output.write_all(b"\n")?;
    output.flush()?;
    drop(output);
    let hello = read_record(&mut BufReader::new(stream), 1024)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "missing native protocol hello",
        )
    })?;
    let hello: Hello = serde_json::from_slice(&hello)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if hello.version != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native protocol version mismatch",
        ));
    }
    if client_features.is_some() && hello.client_features.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "server does not support native client terminal features",
        ));
    }
    Ok(hello)
}

/// Bound allocation before parsing an untrusted native record.
pub fn read_record(reader: &mut impl BufRead, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let mut record = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if record.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated native record",
                ))
            };
        }
        let n = available
            .iter()
            .position(|c| *c == b'\n')
            .map(|n| n + 1)
            .unwrap_or(available.len());
        if record.len().saturating_add(n) > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "native record exceeds limit",
            ));
        }
        let ended = available[n - 1] == b'\n';
        record.extend_from_slice(&available[..n]);
        reader.consume(n);
        if ended {
            return Ok(Some(record));
        }
    }
}

#[cfg(any(unix, windows))]
fn serve_connection(
    stream: NativeStream,
    id: u64,
    tx: SyncSender<Event>,
    instance_id: Option<String>,
) -> io::Result<()> {
    use std::net::Shutdown;
    let mut output = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    output.set_write_timeout(Some(Duration::from_secs(10)))?;
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(10)))?;
    let hello = read_record(&mut reader, MAX_REQUEST_BYTES)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "missing native protocol hello",
        )
    })?;
    let hello: Hello = serde_json::from_slice(&hello)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    #[cfg(windows)]
    let (client_pid, client_sid, client_user) = reader.get_ref().verify_client()?;
    let mut handshake = io::BufWriter::new(&mut output);
    serde_json::to_writer(
        &mut handshake,
        &Hello {
            instance_id: instance_id.clone(),
            stdin_file_stream: true,
            client_features: Some(0),
            ..Hello::default()
        },
    )?;
    handshake.write_all(b"\n")?;
    handshake.flush()?;
    drop(handshake);
    if hello.version != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native protocol version mismatch",
        ));
    }
    if hello
        .instance_id
        .as_ref()
        .is_some_and(|expected| Some(expected) != instance_id.as_ref())
    {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "server incarnation changed; requests were not submitted",
        ));
    }
    reader.get_ref().set_read_timeout(None)?;
    let (out_tx, out_rx) = mpsc::sync_channel(256);
    let context = ClientContext {
        identity: BTreeMap::from([
            ("client_utf8".into(), "1".into()),
            (
                "client_termfeatures".into(),
                tmuxx_protocol::features::feature_names(hello.client_features.unwrap_or(0)),
            ),
        ]),
        cwd: hello.cwd,
        environment: hello.environment,
        host_effects: hello.host_effects,
        ..ClientContext::default()
    };
    #[cfg(windows)]
    let context = {
        let mut context = context;
        context.identity.extend([
            ("client_pid".into(), client_pid.to_string()),
            ("client_uid".into(), client_sid),
            ("client_user".into(), client_user),
            (
                "client_termname".into(),
                context.environment.get("TERM").cloned().unwrap_or_default(),
            ),
        ]);
        context
    };
    let control_commands = hello.control_commands;
    let delivery_bytes = Arc::new(AtomicUsize::new(0));
    tx.send(Event::Connect {
        files: false,
        stdin_files: hello.stdin_file_stream,
        id,
        tx: out_tx,
        context: Box::new(context),
        delivery_bytes: delivery_bytes.clone(),
    })
    .map_err(|_| io::Error::other("server stopped"))?;
    let writer_tx = tx.clone();
    let writer = thread::spawn(move || {
        let mut output = io::BufWriter::new(output);
        for message in out_rx {
            let bytes = control::message_bytes(&message);
            let _ = delivery_bytes.try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.saturating_sub(bytes))
            });
            let result = if let ServerMessage::Snapshot {
                instance_id,
                sequence,
                state,
            } = &message
            {
                if hello.snapshot_stream {
                    tmuxx_protocol::native::write_snapshot(
                        &mut output,
                        instance_id,
                        *sequence,
                        state,
                        tmuxx_protocol::native::MAX_SNAPSHOT_BYTES,
                    )
                } else if tmuxx_protocol::native::snapshot_bytes(state, 64 * 1024 * 1024).is_err() {
                    serde_json::to_writer(
                        &mut output,
                        &ServerMessage::SnapshotAborted {
                            instance_id: instance_id.clone(),
                            sequence: *sequence,
                            error: "large snapshot requires snapshot_stream negotiation".into(),
                        },
                    )
                    .map_err(io::Error::from)
                    .and_then(|_| output.write_all(b"\n"))
                } else {
                    serde_json::to_writer(&mut output, &message)
                        .map_err(io::Error::from)
                        .and_then(|_| output.write_all(b"\n"))
                }
            } else {
                serde_json::to_writer(&mut output, &message)
                    .map_err(io::Error::from)
                    .and_then(|_| output.write_all(b"\n"))
            };
            if result.and_then(|_| output.flush()).is_err() {
                break;
            }
            if matches!(message, ServerMessage::Snapshot { .. }) {
                let _ = writer_tx.send(Event::SnapshotSent(id));
            }
        }
        let _ = output.get_ref().shutdown(Shutdown::Both);
    });
    let result = (|| {
        while let Some(line) = read_record(&mut reader, MAX_REQUEST_BYTES)? {
            let value: serde_json::Value = serde_json::from_slice(&line)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            #[cfg(windows)]
            if value == serde_json::json!({"type": "end_of_input"}) {
                break;
            }
            let event = if value.get("type").is_some() {
                if !control_commands {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "native control requests were not negotiated",
                    ));
                }
                let tmuxx_protocol::native::ClientMessage::Control(request) =
                    serde_json::from_value(value)
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                if request.source.is_some() && !request.argv.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "control source and argv are mutually exclusive",
                    ));
                }
                Event::ControlSource { id, request }
            } else {
                let request = serde_json::from_value(value)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                Event::Request { id, request }
            };
            if tx.send(event).is_err() {
                break;
            }
        }
        Ok(())
    })();
    let _ = tx.send(Event::Disconnect(id));
    let _ = writer.join();
    result
}

struct Host {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    watchdog: Option<Arc<watchdog::Watchdog>>,
    format_jobs: BTreeMap<u64, format_jobs::FormatProcess>,
    latest_clients: BTreeMap<Id, u64>,
    size_activity: BTreeMap<u64, Instant>,
    sizes_pending: bool,
    snapshot_limit: u64,
    snapshots_pending: BTreeSet<u64>,
    snapshots_requested: BTreeSet<u64>,
    checking_sessions: bool,
    wait_pane_target: Option<Id>,
    pane_waiters: BTreeMap<u64, (Id, bool)>,
    stdin_target: Option<Id>,
    stdin_streams: BTreeMap<u64, process::StdinStream>,
    pipes: BTreeMap<Id, process::PipeProcess>,
    retired_pipes: Vec<process::PipeProcess>,
    control: control::Controls,
    view_states: view_state::ViewStates,
    hooks: hooks::Hooks,
    scheduler: scheduler::Scheduler,
    messages: std::collections::VecDeque<(u64, Vec<u8>)>,
    engine: Engine,
    processes: BTreeMap<Id, Process>,
    clients: BTreeMap<u64, Client>,
    file_clients: BTreeSet<u64>,
    tx: SyncSender<Event>,
    instance: String,
    endpoint: PathBuf,
    tmux_endpoint: Option<PathBuf>,
    request_stdin: Option<Vec<u8>>,
    effect_notifications: Vec<EngineNotification>,
    next_host_action: u64,
    host_actions: BTreeMap<u64, PendingHostAction>,
    pane_leases: BTreeMap<Id, BTreeSet<u64>>,
    stopping: bool,
    next_process: u64,
    next_name_check: Instant,
}

fn host_environment(
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> io::Result<BTreeMap<String, String>> {
    vars.into_iter()
        .map(|(name, value)| {
            let name = name.into_string().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "non-UTF8 host environment name is unsupported",
                )
            })?;
            let value = value.into_string().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "non-UTF8 host environment value is unsupported",
                )
            })?;
            Ok((name, value))
        })
        .collect()
}

impl Host {
    fn new(
        tx: SyncSender<Event>,
        options: &Options,
        environment: BTreeMap<String, String>,
    ) -> Self {
        let mut engine = Engine::default();
        engine.server_options.insert(
            "exit-empty".into(),
            if options.exit_empty { "on" } else { "off" }.into(),
        );
        engine.enable_format_jobs();
        engine.environment = environment;
        engine.identity.insert(
            "config_files".into(),
            options
                .config
                .iter()
                .map(|path| path.to_string_lossy())
                .collect::<Vec<_>>()
                .join(","),
        );
        defaults::apply(&mut engine);
        engine.identity.insert(
            "socket_path".into(),
            options
                .tmux_endpoint
                .as_ref()
                .unwrap_or(&options.endpoint)
                .to_string_lossy()
                .into_owned(),
        );
        Self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            watchdog: None,
            latest_clients: BTreeMap::new(),
            size_activity: BTreeMap::new(),
            sizes_pending: false,
            snapshot_limit: options
                .snapshot_bytes
                .min(tmuxx_protocol::native::MAX_SNAPSHOT_BYTES),
            snapshots_pending: BTreeSet::new(),
            snapshots_requested: BTreeSet::new(),
            checking_sessions: false,
            wait_pane_target: None,
            pane_waiters: BTreeMap::new(),
            stdin_target: None,
            stdin_streams: BTreeMap::new(),
            pipes: BTreeMap::new(),
            retired_pipes: Vec::new(),
            control: control::Controls::default(),
            hooks: hooks::Hooks::default(),
            scheduler: scheduler::Scheduler::default(),
            messages: std::collections::VecDeque::new(),
            view_states: view_state::ViewStates::default(),
            engine,
            format_jobs: BTreeMap::new(),
            processes: BTreeMap::new(),
            clients: BTreeMap::new(),
            file_clients: BTreeSet::new(),
            tx,
            instance: format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
            endpoint: options.endpoint.clone(),
            tmux_endpoint: options.tmux_endpoint.clone(),
            request_stdin: None,
            effect_notifications: Vec::new(),
            next_host_action: 0,
            host_actions: BTreeMap::new(),
            pane_leases: BTreeMap::new(),
            stopping: false,
            next_process: 0,
            next_name_check: Instant::now(),
        }
    }
    fn run(&mut self, rx: Receiver<Event>, options: &Options) -> io::Result<()> {
        self.start_configuration(&options.config);
        let mut had_session = false;
        while !self.stopping {
            self.engine.now = tmuxx_core::timestamp();
            had_session |= !self.engine.sessions.is_empty();
            let unattached = self
                .engine
                .server_options
                .get("exit-unattached")
                .is_some_and(|value| value == "on")
                && !self.has_attached_clients();
            if self
                .engine
                .server_options
                .get("exit-empty")
                .is_some_and(|value| value == "on")
                && had_session
                && (self.engine.sessions.is_empty() || unattached)
            {
                self.scheduler.flush_waiters();
                if self.clients.is_empty() && !self.scheduler.has_jobs() {
                    break;
                }
            }
            let alerts = self.engine.alert_tick();
            let _ = self.effects(alerts);
            self.expire_host();
            self.reap_pipes();
            self.control_tick();
            self.hook_monitor_tick();
            self.check_window_names();
            self.start_format_jobs();
            let did_work = self.scheduled_work();
            let event = if did_work {
                match rx.try_recv() {
                    Ok(event) => event,
                    Err(mpsc::TryRecvError::Empty) => continue,
                    Err(mpsc::TryRecvError::Disconnected) => break,
                }
            } else {
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(event) => event,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            };
            match event {
                Event::SnapshotSent(id) => {
                    self.snapshots_pending.remove(&id);
                    if self.snapshots_requested.remove(&id) {
                        self.snapshot(id);
                    }
                }
                Event::FileResult {
                    id,
                    request_id,
                    result,
                } => self.file_complete(id, request_id, result),
                Event::Stdin {
                    id,
                    request_id,
                    data,
                    ack,
                } => {
                    let result = self.stdin_chunk(id, request_id, data);
                    let _ = ack.send(result);
                }
                Event::Connect {
                    files,
                    stdin_files,
                    id,
                    tx,
                    context,
                    delivery_bytes,
                } => {
                    if files {
                        self.file_clients.insert(id);
                    }
                    if stdin_files {
                        self.scheduler.enable_stdin_files(id);
                    }
                    let mut context = *context;
                    if context.cwd.is_none() {
                        context.cwd = std::env::current_dir()
                            .ok()
                            .map(|path| path.to_string_lossy().into_owned());
                    }
                    context.created = self.engine.now;
                    context.activity = self.engine.now;
                    if context.client_name.is_empty() {
                        context.client_name = format!("client-{id}");
                    }
                    self.clients.insert(
                        id,
                        Client {
                            tx,
                            context,
                            subscribed: false,
                            control: false,
                            delivery_bytes,
                        },
                    );
                }
                Event::Disconnect(id) => self.disconnect(id),
                Event::Request { id, request } => self.request(id, request),
                #[cfg(unix)]
                Event::PrepareControlLine { id, source, reply } => {
                    let _ = reply.send(self.prepare_control_commands(id, &source));
                }
                #[cfg(unix)]
                Event::ControlRequest {
                    id,
                    request,
                    commands,
                    flags,
                } => self.request_parsed(id, request, commands, Some(flags)),
                Event::ControlSource { id, request } => {
                    if request.source.as_ref().is_some_and(Vec::is_empty) {
                        self.detach_client(id, false);
                        continue;
                    }
                    let flags = u64::from(request.source.is_some());
                    let commands = request
                        .source
                        .as_ref()
                        .map(|source| self.prepare_control_commands(id, source));
                    let argv = commands
                        .as_ref()
                        .and_then(|commands| commands.as_ref().ok())
                        .map_or(request.argv, |commands| command_argv(commands));
                    self.request_parsed(
                        id,
                        Request {
                            request_id: request.request_id,
                            argv,
                            stdin: None,
                            subscribe: request.subscribe,
                            control: true,
                        },
                        commands,
                        Some(flags),
                    );
                }
                Event::Output(pane, generation, data) => {
                    if !self.is_process(pane, generation) {
                        continue;
                    }
                    if !data.is_empty()
                        && let Some(pane) = self.engine.panes.get_mut(&pane)
                    {
                        pane.last_output_time = Some(self.engine.now);
                    }
                    self.pane_output(pane, &data);
                }
                Event::ReadClosed(pane, generation) => {
                    if !self.is_process(pane, generation) {
                        continue;
                    }
                    if let Some(p) = self.processes.get_mut(&pane) {
                        p.eof = true;
                    }
                    self.finish_process(pane);
                }
                Event::Exited(pane, generation, status, signal) => {
                    if !self.is_process(pane, generation) {
                        continue;
                    }
                    self.pane_wait_complete(pane, status);
                    if let Some(p) = self.processes.get_mut(&pane) {
                        p.status = Some(status);
                        p.signal = signal.clone();
                    }
                    self.engine.record_exit(pane, status, signal);
                    self.finish_process(pane);
                }
                Event::InputFailed(pane, generation, error) => {
                    if self.is_process(pane, generation) {
                        self.input_failed(pane, error);
                    }
                }
                Event::PipeInput(pane, generation, data) => {
                    if self
                        .pipes
                        .get(&pane)
                        .is_some_and(|pipe| pipe.generation == generation)
                        && let Err(error) = self.effects(vec![Effect::Write { pane, data }])
                    {
                        self.close_pipe(pane);
                        eprintln!("tmuxx: pipe input: {error}");
                    }
                }
                Event::PipeClosed(pane, generation) => {
                    if self
                        .pipes
                        .get(&pane)
                        .is_some_and(|pipe| pipe.generation == generation)
                    {
                        self.close_pipe(pane);
                        self.finish_process(pane);
                    }
                }
                Event::PipeExited(pane, generation) => {
                    if self
                        .pipes
                        .get(&pane)
                        .is_some_and(|pipe| pipe.generation == generation && !pipe.input)
                    {
                        self.close_pipe(pane);
                        self.finish_process(pane);
                    }
                }
                Event::PipeDrained(pane) => self.finish_process(pane),
                Event::JobFinished(result) => self.job_finished(result),
                Event::FormatJobLine(id, data) => self.format_job_line(id, data),
                Event::Shutdown => self.stopping = true,
            }
        }
        Ok(())
    }
    fn pane_in_session(&self, pane: Id, session: Option<Id>) -> bool {
        self.engine.panes.get(&pane).is_some_and(|p| {
            session
                .and_then(|s| self.engine.sessions.get(&s))
                .is_some_and(|s| s.windows.values().any(|w| *w == p.window))
        })
    }
    fn is_process(&self, pane: Id, generation: u64) -> bool {
        self.processes
            .get(&pane)
            .is_some_and(|process| process.generation == generation)
    }
    fn send(&mut self, id: u64, message: ServerMessage) {
        if self.clients.get(&id).is_some_and(|client| client.control) {
            self.send_control(id, message);
            return;
        }
        if let Some(c) = self.clients.get(&id) {
            let bytes = control::message_bytes(&message);
            if c.delivery_bytes
                .load(Ordering::Acquire)
                .saturating_add(bytes)
                > 64 * 1024 * 1024
            {
                self.disconnect(id);
                return;
            }
            c.delivery_bytes.fetch_add(bytes, Ordering::AcqRel);
            match c.tx.try_send(message) {
                Ok(()) => {}
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                    c.delivery_bytes.fetch_sub(bytes, Ordering::AcqRel);
                    self.disconnect(id);
                }
            }
        }
    }
    fn broadcast(&mut self, message: ServerMessage) {
        let ids: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, c)| c.subscribed)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.send(id, message.clone());
        }
    }
    fn snapshot(&mut self, id: u64) {
        if !self.clients.contains_key(&id) {
            return;
        }
        if self.snapshots_pending.contains(&id) {
            self.snapshots_requested.insert(id);
            return;
        }
        let state = self.engine.snapshot();
        if let Err(error) = tmuxx_protocol::native::snapshot_bytes(&state, self.snapshot_limit) {
            self.send(
                id,
                ServerMessage::SnapshotAborted {
                    instance_id: self.instance.clone(),
                    sequence: self.engine.sequence,
                    error: error.to_string(),
                },
            );
            return;
        }
        self.snapshots_pending.insert(id);
        self.send(
            id,
            ServerMessage::Snapshot {
                instance_id: self.instance.clone(),
                sequence: self.engine.sequence,
                state,
            },
        );
    }
    fn execute(
        &mut self,
        command: &tmuxx_command::Command,
        context: &mut ClientContext,
    ) -> Result<Outcome, String> {
        self.stdin_target = None;
        self.refresh_process_metadata();
        let name = command.args.first().map(Vec::as_slice).unwrap_or_default();
        // A failed PTY spawn must not leave a half-created graph or consume IDs.
        let checkpoint = matches!(
            name,
            b"new-session"
                | b"new"
                | b"new-window"
                | b"neww"
                | b"split-window"
                | b"splitw"
                | b"new-pane"
                | b"newp"
                | b"respawn-pane"
                | b"respawnp"
                | b"respawn-window"
                | b"respawnw"
        )
        .then(|| (self.engine.clone(), context.clone()));
        let result = self.engine.execute(command, context).and_then(|mut out| {
            // Create the process before resizing existing panes or delivering input.
            let mut effects = out.effects.clone();
            effects.sort_by_key(|effect| !matches!(effect, Effect::Spawn { .. }));
            self.effects(effects)?;
            out.notifications.append(&mut self.effect_notifications);
            Ok(out)
        });
        if result.is_err() {
            self.effect_notifications.clear();
            if let Some((engine, previous_context)) = checkpoint {
                self.processes.retain(|id, _| engine.panes.contains_key(id));
                self.engine = engine;
                *context = previous_context;
            }
        }
        result
    }
    fn input_failed(&mut self, pane: Id, error: String) {
        if self.processes.remove(&pane).is_none() {
            return;
        }
        self.engine.record_exit(pane, 1, None);
        let policies = self.session_policies();
        let before = self.hook_snapshot();
        let message = format!("pane %{pane}: input failed: {error}");
        eprintln!("tmuxx: {message}");
        // Disconnecting invalidates subscriber state and pending input; reconnect
        // requires a new snapshot, never automatic replay of uncertain writes.
        self.broadcast(ServerMessage::Exit { reason: message });
        let clients = self
            .clients
            .iter()
            .filter(|(_, client)| client.subscribed)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in clients {
            self.disconnect(id);
        }
        let out = self.engine.exited(pane, 1);
        self.reconcile_destroyed_sessions(policies, None);
        self.process_exit_hooks(pane, before);
        if let Err(error) = self.effects(out.effects) {
            eprintln!("tmuxx: {error}");
        }
        for data in out.notifications {
            self.broadcast_notification(data);
        }
        self.broadcast(ServerMessage::Changed {
            sequence: self.engine.sequence,
        });
        self.check_unattached_sessions();
    }
    fn finish_process(&mut self, pane: Id) {
        if self
            .pipes
            .get(&pane)
            .is_some_and(|pipe| pipe.pending_bytes() != 0)
        {
            return;
        }
        let status = self.processes.get(&pane).and_then(|p| {
            if p.eof {
                p.status.map(|status| (status, p.signal.clone()))
            } else {
                None
            }
        });
        if let Some((status, signal)) = status {
            let policies = self.session_policies();
            let before = self.hook_snapshot();
            self.processes.remove(&pane);
            self.close_pipe(pane);
            let out = self.engine.exited_with_signal(pane, status, signal);
            self.reconcile_destroyed_sessions(policies, None);
            self.process_exit_hooks(pane, before);
            if let Err(error) = self.effects(out.effects) {
                eprintln!("tmuxx: cleanup: {error}");
            }
            for data in out.notifications {
                self.broadcast_notification(data);
            }
            self.broadcast(ServerMessage::Changed {
                sequence: self.engine.sequence,
            });
            self.check_unattached_sessions();
        }
    }
    fn dispatch_host(&mut self, pane: Id, action: HostAction) -> Result<(), String> {
        let client = self
            .clients
            .iter()
            .filter(|(id, c)| {
                c.subscribed
                    && !c.control
                    && c.context.host_effects
                    && (self.view_states.has_view(**id, pane)
                        || self.pane_in_session(pane, c.context.session))
            })
            .min_by_key(|(id, _)| (!self.view_states.focused(**id, pane), **id))
            .map(|(id, _)| *id);
        self.dispatch_host_to(pane, action, client)
    }
    fn dispatch_host_to(
        &mut self,
        pane: Id,
        action: HostAction,
        client: Option<u64>,
    ) -> Result<(), String> {
        let Some(client) = client else {
            if let Some(response) = action.denied() {
                let effects = self.engine.host_response(pane, action, response)?;
                self.effects(effects)?;
            }
            return Ok(());
        };
        let event_id = self.next_host_action;
        self.next_host_action += 1;
        let value = serde_json::to_value(&action).map_err(|error| error.to_string())?;
        if action.denied().is_some() {
            if self.host_actions.len() >= 1024 {
                let response = action.denied().unwrap();
                let effects = self.engine.host_response(pane, action, response)?;
                self.effects(effects)?;
                return Ok(());
            }
            self.host_actions.insert(
                event_id,
                PendingHostAction {
                    client,
                    pane,
                    action,
                    deadline: Instant::now() + Duration::from_secs(30),
                },
            );
        }
        self.send(
            client,
            ServerMessage::HostEffect {
                sequence: self.engine.sequence,
                pane_id: pane,
                event_id,
                action: value,
            },
        );
        if !self.clients.contains_key(&client) {
            self.disconnect_host(client);
        }
        Ok(())
    }

    fn client_clipboard(&mut self, name: &str, pane: Id, data: Vec<u8>) -> Result<(), String> {
        use tmuxx_terminal::clipboard::{Content, Location, Write};
        let client = self
            .clients
            .iter()
            .find(|(_, client)| {
                client.context.client_name == name
                    && client.subscribed
                    && !client.control
                    && client.context.host_effects
            })
            .map(|(id, _)| *id);
        let action = HostAction::ClipboardWrite(Write::osc52(
            Location::Standard,
            vec![Content {
                mime: b"text/plain".to_vec(),
                data: data.into(),
            }],
        ));
        self.dispatch_host_to(pane, action, client)
    }

    fn respond_host(
        &mut self,
        client: u64,
        args: &[Vec<u8>],
        input: Option<Vec<u8>>,
    ) -> Result<Outcome, String> {
        if args.len() != 2 {
            return Err("host reply requires an event ID".into());
        }
        let event = std::str::from_utf8(&args[1])
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or("invalid host event ID")?;
        let pending = self
            .host_actions
            .get(&event)
            .ok_or("host event expired or already answered")?;
        if pending.client != client {
            return Err("host event belongs to another client".into());
        }
        let response: HostResponse =
            serde_json::from_slice(input.as_deref().ok_or("missing host response")?)
                .map_err(|e| e.to_string())?;
        if !matches!(
            (&pending.action, &response),
            (HostAction::ClipboardRead(_), HostResponse::Read(_))
                | (HostAction::ClipboardWrite(_), HostResponse::Write(_))
        ) {
            return Err("host response type mismatch".into());
        }
        let pending = self.host_actions.remove(&event).unwrap();
        let effects = self
            .engine
            .host_response(pending.pane, pending.action, response)?;
        self.effects(effects)?;
        Ok(Outcome::default())
    }

    fn deny_host_events(&mut self, events: Vec<u64>) {
        for event in events {
            if let Some(pending) = self.host_actions.remove(&event) {
                let response = pending.action.denied().unwrap();
                if let Ok(effects) =
                    self.engine
                        .host_response(pending.pane, pending.action, response)
                    && let Err(error) = self.effects(effects)
                {
                    self.input_failed(pending.pane, error);
                }
            }
        }
    }

    fn disconnect_host(&mut self, client: u64) {
        self.disconnect_view_states(client);
        let events = self
            .host_actions
            .iter()
            .filter(|(_, pending)| pending.client == client)
            .map(|(id, _)| *id)
            .collect();
        self.deny_host_events(events);
        let mut expired = Vec::new();
        for (pane, owners) in &mut self.pane_leases {
            owners.remove(&client);
            if owners.is_empty() {
                expired.push(*pane);
            }
        }
        for pane in expired {
            self.pane_leases.remove(&pane);
            self.release_ephemeral(pane);
        }
    }

    fn pane_retention(
        &mut self,
        client: u64,
        command: &tmuxx_command::Command,
    ) -> Result<Outcome, String> {
        let args = tmuxx_command::Args::parse(&command.args[1..], "t:")?;
        let pane = args
            .get(b't')
            .and_then(|value| value.strip_prefix(b"%"))
            .and_then(|value| std::str::from_utf8(value).ok())
            .and_then(|value| value.parse::<Id>().ok())
            .ok_or("retention commands require a pane ID (-t %ID)")?;
        if !self.engine.panes.contains_key(&pane) && command.args[0] != b"tmuxx-release-pane" {
            return Err("pane not found".into());
        }
        match command.args[0].as_slice() {
            b"tmuxx-retain-pane" => {
                let persistent = match args.rest.as_slice() {
                    [value] if value == b"on" => true,
                    [value] if value == b"off" => false,
                    _ => return Err("retention requires on or off".into()),
                };
                self.pane_leases.entry(pane).or_default().insert(client);
                self.engine.set_persistent(pane, persistent)
            }
            b"tmuxx-claim-pane" => {
                args.no_rest()?;
                self.pane_leases.entry(pane).or_default().insert(client);
                Ok(Outcome::default())
            }
            b"tmuxx-release-pane" => {
                args.no_rest()?;
                self.release_view_state(client, pane);
                if let Some(owners) = self.pane_leases.get_mut(&pane) {
                    owners.remove(&client);
                }
                self.pane_leases.retain(|pane, owners| {
                    self.engine.panes.contains_key(pane) && !owners.is_empty()
                });
                self.release_ephemeral(pane);
                Ok(Outcome::default())
            }
            _ => Err("unknown retention command".into()),
        }
    }

    fn release_ephemeral(&mut self, pane: Id) {
        if self
            .pane_leases
            .get(&pane)
            .is_some_and(|owners| !owners.is_empty())
        {
            return;
        }
        if self
            .engine
            .panes
            .get(&pane)
            .is_none_or(|pane| pane.persistent)
        {
            return;
        }
        let policies = self.session_policies();
        let before = self.hook_snapshot();
        let out = self.engine.close_pane(pane);
        self.reconcile_destroyed_sessions(policies, None);
        if let Some(before) = before {
            self.graph_hooks(before, false);
        }
        if let Err(error) = self.effects(out.effects) {
            eprintln!("tmuxx: ephemeral cleanup: {error}");
        }
        for data in out.notifications {
            self.broadcast_notification(data);
        }
        self.broadcast(ServerMessage::Changed {
            sequence: self.engine.sequence,
        });
        self.check_unattached_sessions();
    }

    fn expire_host(&mut self) {
        let cancelled = self
            .stdin_streams
            .iter()
            .filter(|(_, stream)| !self.engine.panes.contains_key(&stream.pane))
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in cancelled {
            self.stdin_complete(id, Some("pane has been removed".into()));
        }
        self.pane_leases
            .retain(|pane, _| self.engine.panes.contains_key(pane));
        let now = Instant::now();
        let events = self
            .host_actions
            .iter()
            .filter(|(_, pending)| {
                pending.deadline <= now || !self.engine.panes.contains_key(&pending.pane)
            })
            .map(|(id, _)| *id)
            .collect();
        self.deny_host_events(events);
    }

    fn effects(&mut self, effects: Vec<Effect>) -> Result<(), String> {
        for effect in effects {
            match effect {
                Effect::Alert {
                    session,
                    index,
                    window,
                    event,
                } => self.fire_winlink_hook(event, session, index, window),
                Effect::ShellPrompt { pane } => self.fire_pane_hook("pane-shell-prompt", pane),
                Effect::ShellCommand {
                    pane,
                    started,
                    state,
                } => self.fire_shell_hook(pane, started, state),
                Effect::WindowRenamed { window, name } => {
                    let linked_sessions = self
                        .engine
                        .sessions
                        .values()
                        .filter(|session| session.windows.values().any(|id| *id == window))
                        .map(|session| session.id)
                        .collect();
                    self.broadcast_notification(tmuxx_core::Notification::WindowRenamed {
                        window,
                        name,
                        linked_sessions,
                    });
                }
                Effect::ClipboardChanged {
                    pane,
                    buffer,
                    deleted,
                } => {
                    if let Some(pane) = pane {
                        self.fire_pane_hook("pane-set-clipboard", pane);
                    }
                    for name in deleted {
                        self.broadcast(ServerMessage::Control {
                            data: format!("%paste-buffer-deleted {name}").into_bytes(),
                        });
                    }
                    if let Some(name) = buffer {
                        self.broadcast(ServerMessage::Control {
                            data: format!("%paste-buffer-changed {name}").into_bytes(),
                        });
                    }
                }
                Effect::Empty { pane } => {
                    self.processes.remove(&pane);
                    self.reset_control_pane(pane);
                }
                Effect::FeedStdin { pane } => {
                    if let Some(input) = self.request_stdin.take() {
                        self.pane_output(pane, &input);
                    } else {
                        self.stdin_target = Some(pane);
                    }
                }
                Effect::WaitPane { pane } => self.wait_pane_target = Some(pane),
                Effect::Pipe {
                    pane,
                    command,
                    input,
                    output,
                } => self.pipe(pane, command, input, output)?,
                Effect::Host { pane, action } => self.dispatch_host(pane, action)?,
                Effect::ClientClipboard { client, pane, data } => {
                    self.client_clipboard(&client, pane, data)?
                }
                Effect::Spawn {
                    pane,
                    shell,
                    argv,
                    cwd,
                    env,
                    cols,
                    rows,
                } => self.spawn(pane, shell, argv, cwd, env, (cols, rows))?,
                Effect::Write { pane, data } => {
                    if self.engine.panes.get(&pane).is_some_and(|pane| pane.empty) {
                        continue;
                    }
                    let process = self
                        .processes
                        .get(&pane)
                        .ok_or_else(|| format!("pane %{pane} is not running"))?;
                    let size = data.len();
                    let mut pending = process.input_bytes.load(Ordering::Acquire);
                    loop {
                        let total = pending
                            .checked_add(size)
                            .filter(|total| *total <= MAX_REQUEST_BYTES)
                            .ok_or("pane input queue exceeds byte budget")?;
                        match process.input_bytes.compare_exchange_weak(
                            pending,
                            total,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => break,
                            Err(actual) => pending = actual,
                        }
                    }
                    if let Err(error) = process.input.try_send(data) {
                        process.input_bytes.fetch_sub(size, Ordering::AcqRel);
                        return Err(format!("pane input unavailable: {error}"));
                    }
                }
                Effect::Resize { pane, cols, rows } => {
                    let cell = self
                        .engine
                        .panes
                        .get(&pane)
                        .map(|pane| pane.terminal.cell_size())
                        .unwrap_or((0, 0));
                    if let Some(p) = self.processes.get(&pane) {
                        p.master
                            .resize(PtySize {
                                cols,
                                rows,
                                pixel_width: cols.saturating_mul(cell.0),
                                pixel_height: rows.saturating_mul(cell.1),
                            })
                            .map_err(|e| e.to_string())?;
                    }
                }
                Effect::Kill { pane } => {
                    self.pane_wait_complete(
                        pane,
                        self.processes
                            .get(&pane)
                            .and_then(|p| p.status)
                            .unwrap_or(129),
                    );
                    self.close_pipe(pane);
                    self.processes.remove(&pane);
                }
                Effect::Shutdown => self.stopping = true,
                Effect::LoadBuffer {
                    name,
                    path,
                    clipboard,
                } => {
                    let data = if path == b"-" {
                        self.request_stdin.take().ok_or("no stdin data available")?
                    } else {
                        let mut data = Vec::new();
                        files::open(
                            std::fs::OpenOptions::new().read(true),
                            Path::new(&os_string(&path).map_err(|e| e.to_string())?),
                        )
                        .map_err(|e| files::path_error(&e, &path))?
                        .take((MAX_REQUEST_BYTES + 1) as u64)
                        .read_to_end(&mut data)
                        .map_err(|e| files::path_error(&e, &path))?;
                        if data.len() > MAX_REQUEST_BYTES {
                            return Err("buffer exceeds input size limit".into());
                        }
                        data
                    };
                    if data.is_empty() {
                        continue;
                    }
                    let clipboard_data = clipboard.as_ref().map(|_| data.clone());
                    let (name, deleted) = self.engine.store_buffer(name, data)?.unwrap();
                    if let Some((client, pane)) = clipboard {
                        self.client_clipboard(&client, pane, clipboard_data.unwrap())?;
                    }
                    self.effect_notifications.extend(
                        deleted.into_iter().map(|name| {
                            format!("%paste-buffer-deleted {name}").into_bytes().into()
                        }),
                    );
                    self.effect_notifications
                        .push(format!("%paste-buffer-changed {name}").into_bytes().into());
                }
                Effect::SaveBuffer { path, data, append } => {
                    let mut file = files::open(
                        std::fs::OpenOptions::new()
                            .write(true)
                            .create(true)
                            .truncate(!append)
                            .append(append),
                        Path::new(&os_string(&path).map_err(|e| e.to_string())?),
                    )
                    .map_err(|e| files::path_error(&e, &path))?;
                    file.write_all(&data)
                        .map_err(|e| files::path_error(&e, &path))?;
                }
            }
        }
        Ok(())
    }
    fn spawn(
        &mut self,
        pane: Id,
        shell: String,
        argv: Vec<Vec<u8>>,
        cwd: Option<String>,
        env: BTreeMap<String, String>,
        (cols, rows): (u16, u16),
    ) -> Result<(), String> {
        let generation = self.next_process;
        self.next_process += 1;
        let cell = self.engine.panes[&pane].terminal.cell_size();
        let pair = native_pty_system()
            .openpty(PtySize {
                cols,
                rows,
                pixel_width: cols.saturating_mul(cell.0),
                pixel_height: rows.saturating_mul(cell.1),
            })
            .map_err(|e| e.to_string())?;
        let mut command = if argv.len() == 1 {
            let mut c = CommandBuilder::new(&shell);
            #[cfg(unix)]
            {
                c.arg("-c");
                c.arg(os_string(&argv[0]).map_err(|e| e.to_string())?);
            }
            #[cfg(windows)]
            c.windows_raw_arguments(
                windows_job::shell_tail(&shell, &argv[0]).map_err(|e| e.to_string())?,
            );
            c
        } else if argv.is_empty() {
            CommandBuilder::new(&shell)
        } else {
            let mut c = CommandBuilder::new(os_string(&argv[0]).map_err(|e| e.to_string())?);
            for arg in &argv[1..] {
                c.arg(os_string(arg).map_err(|e| e.to_string())?);
            }
            c
        };
        #[cfg(unix)]
        let fallback_cwd = "/".to_owned();
        #[cfg(windows)]
        let fallback_cwd = std::env::current_dir()
            .map_err(|e| e.to_string())?
            .into_os_string()
            .into_string()
            .map_err(|_| "Windows current directory must be Unicode")?;
        let cwd = cwd
            .filter(|path| Path::new(path).is_dir())
            .or_else(|| {
                env.get("HOME")
                    .filter(|path| Path::new(path).is_dir())
                    .cloned()
            })
            .unwrap_or(fallback_cwd);
        command.env_clear();
        for (key, value) in env {
            command.env(key, value);
        }
        command.env("SHELL", &shell);
        command.env("PWD", &cwd);
        command.env("TMUX_PANE", format!("%{pane}"));
        command.env("TMUXX_ENDPOINT", &self.endpoint);
        if let Some(endpoint) = &self.tmux_endpoint {
            let sid = self
                .engine
                .panes
                .get(&pane)
                .and_then(|p| {
                    self.engine
                        .sessions
                        .values()
                        .find(|s| s.windows.values().any(|id| *id == p.window))
                })
                .map(|s| s.id)
                .unwrap_or(0);
            command.env(
                "TMUX",
                format!("{},{},{sid}", endpoint.display(), std::process::id()),
            );
        }
        command.cwd(&cwd);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        #[cfg(target_os = "linux")]
        let (child, registration) = {
            if self.watchdog.is_none() {
                self.watchdog =
                    Some(watchdog::Watchdog::start().map_err(|error| error.to_string())?);
            }
            let master = pair
                .master
                .as_raw_fd()
                .ok_or("PTY master has no descriptor")?;
            let (child, registration) = self
                .watchdog
                .as_ref()
                .unwrap()
                .spawn(command, master)
                .map_err(|error| error.to_string())?;
            (child, Some(registration))
        };
        #[cfg(not(target_os = "linux"))]
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| e.to_string())?;
        #[cfg(target_os = "macos")]
        let mut child = child;
        let pid = child.process_id();
        #[cfg(windows)]
        let handle = child
            .as_raw_handle()
            .map(|handle| {
                unsafe { std::os::windows::io::BorrowedHandle::borrow_raw(handle) }
                    .try_clone_to_owned()
            })
            .transpose()
            .map_err(|e| e.to_string())?;
        // macOS retains its acknowledged registration boundary. Linux's broker
        // owns the child before exec and also covers a lost spawn reply. The
        // Windows portable-pty backend assigns Job ownership before execution.
        #[cfg(target_os = "macos")]
        let registration = match (&self.watchdog, pid) {
            (Some(watchdog), Some(pid)) => match watchdog.register(pid) {
                Ok(registration) => Some(registration),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.to_string());
                }
            },
            _ => None,
        };
        let (input, rx) = mpsc::sync_channel::<Vec<u8>>(64);
        let input_bytes = Arc::new(AtomicUsize::new(0));
        let write_bytes = input_bytes.clone();
        let (stop, stop_rx) = mpsc::sync_channel(1);
        let (done_tx, finished) = mpsc::sync_channel(1);
        let process = Process {
            #[cfg(windows)]
            handle,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            _watchdog: registration,
            #[cfg(all(unix, not(target_os = "linux")))]
            pid,
            generation,
            master: pair.master,
            input,
            input_bytes,
            stop,
            finished,
            status: None,
            signal: None,
            eof: false,
        };
        drop(pair.slave);
        // Start the reaper first; failure of either I/O worker then kills the
        // child through Process::drop and the reaper still collects its status.
        let tx = self.tx.clone();
        let child = Arc::new(Mutex::new(child));
        let waiter = child.clone();
        let reaper = thread::Builder::new()
            .name(format!("tmuxx-wait-{pane}"))
            .spawn(move || {
                let mut child = waiter.lock().unwrap();
                let status = loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break process::exit_status(status),
                        Err(_) => break (1, None),
                        Ok(None) => {}
                    }
                    if !matches!(
                        stop_rx.recv_timeout(Duration::from_millis(10)),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ) {
                        let _ = child.kill();
                        break child.wait().map(process::exit_status).unwrap_or((1, None));
                    }
                };
                let _ = done_tx.send(());
                let _ = tx.send(Event::Exited(pane, generation, status.0, status.1));
            });
        if let Err(error) = reaper {
            let mut child = child.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.to_string());
        }
        let tx = self.tx.clone();
        thread::Builder::new()
            .name(format!("tmuxx-read-{pane}"))
            .spawn(move || {
                let mut buffer = vec![0; 32768];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            if tx
                                .send(Event::Output(pane, generation, buffer[..n].to_vec()))
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                let _ = tx.send(Event::ReadClosed(pane, generation));
            })
            .map_err(|e| e.to_string())?;
        let write_tx = self.tx.clone();
        thread::Builder::new()
            .name(format!("tmuxx-write-{pane}"))
            .spawn(move || {
                for bytes in rx {
                    let result = writer.write_all(&bytes);
                    write_bytes.fetch_sub(bytes.len(), Ordering::AcqRel);
                    if let Err(error) = result {
                        let _ =
                            write_tx.send(Event::InputFailed(pane, generation, error.to_string()));
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        let tty = process
            .master
            .tty_name()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        #[cfg(not(unix))]
        let tty = String::new();
        self.engine.set_process(pane, pid, tty);
        if let Some(pane) = self.engine.panes.get_mut(&pane) {
            pane.cwd = Some(cwd);
        }
        self.processes.insert(pane, process);
        self.reset_control_pane(pane);
        Ok(())
    }
}

#[cfg(unix)]
fn os_string(bytes: &[u8]) -> io::Result<std::ffi::OsString> {
    use std::os::unix::ffi::OsStringExt;
    Ok(std::ffi::OsString::from_vec(bytes.to_vec()))
}
#[cfg(not(unix))]
fn os_string(bytes: &[u8]) -> io::Result<std::ffi::OsString> {
    String::from_utf8(bytes.to_vec())
        .map(Into::into)
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "native Windows arguments and paths must be UTF-8",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(unix)]
    fn host_environment_rejects_non_utf8_without_panicking() {
        use std::os::unix::ffi::OsStringExt;
        let invalid = std::ffi::OsString::from_vec(vec![0xff]);
        for pair in [(invalid.clone(), "value".into()), ("name".into(), invalid)] {
            assert_eq!(
                host_environment([pair]).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        assert_eq!(
            host_environment([("name".into(), "välue".into())]).unwrap()["name"],
            "välue"
        );
    }
    #[test]
    fn native_framing_is_bounded_before_allocation() {
        assert_eq!(
            read_record(&mut &b"abc\nrest"[..], 4).unwrap(),
            Some(b"abc\n".to_vec())
        );
        assert!(read_record(&mut &b"abcde\n"[..], 4).is_err());
        assert!(read_record(&mut &b"abc"[..], 4).is_err());
    }
}
