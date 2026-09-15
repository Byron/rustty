//! PTY ownership and background IO. No windowing or GPU dependency.
use crate::config::{
    Command, Config, CursorStyle, GraphemeWidthMethod, ShellIntegration, TerminalColor,
};
use portable_pty::{Child, CommandBuilder, ExitStatus, PtySize, native_pty_system};
use rustty_vt::{CursorShape, Effect, Screen, ScrollbackLimits, Terminal, query};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const MAX_PENDING_INPUT: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct SessionOptions {
    pub cols: u16,
    pub rows: u16,
    pub working_directory: Option<PathBuf>,
    pub command: Option<Command>,
    pub resources: Option<PathBuf>,
    /// Initial host state is installed before the child can issue queries.
    pub color_scheme: Option<query::ColorScheme>,
    pub visible: bool,
    pub focused: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            working_directory: None,
            command: None,
            resources: None,
            color_scheme: None,
            visible: true,
            focused: false,
        }
    }
}

#[derive(Debug)]
pub enum SessionEvent {
    Effect(Effect),
    Exited {
        code: u32,
        signal: Option<String>,
        /// Wall time observed by the child owner, independent of event delivery.
        runtime: Duration,
    },
    OutputClosed,
    Error(String),
}

enum IoCommand {
    Write(Vec<u8>),
    Linefeed(bool),
    HostReport(Vec<u8>),
    Resize { size: PtySize, reply: Vec<u8> },
    Close,
}

/// A viewport snapshot owns only visible rows, so font shaping never holds a
/// terminal lock or copies the scrollback history.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub screen: Screen,
    pub cols: u16,
    pub rows: u16,
    pub generation: u64,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub palette: Vec<[u8; 3]>,
    pub cursor_color: Option<[u8; 3]>,
    pub title: String,
    pub working_directory: String,
}

pub struct Session {
    terminal: Arc<Mutex<Terminal>>,
    input: mpsc::Sender<IoCommand>,
    events: mpsc::Receiver<SessionEvent>,
    pending_input: Arc<PendingInput>,
    exited: Arc<AtomicBool>,
    #[cfg(target_os = "macos")]
    close_child: std::os::unix::net::UnixStream,
    #[cfg(not(target_os = "macos"))]
    close_child: mpsc::Sender<()>,
}

fn error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

impl Session {
    pub fn spawn(
        config: &Config,
        options: SessionOptions,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> io::Result<Self> {
        let cols = options.cols.max(1);
        let rows = options.rows.max(1);
        let pair = native_pty_system()
            .openpty(PtySize {
                cols,
                rows,
                ..PtySize::default()
            })
            .map_err(error)?;
        let command = command(config, &options)?;
        let terminfo_name = command
            .get_env("TERM")
            .map(|name| name.as_encoded_bytes().to_vec());
        // Prepare handles before launching a child: any failure here has no
        // process to kill or reap. Retaining the slave keeps the reader from
        // seeing EOF while the workers are starting.
        let mut reader = pair.master.try_clone_reader().map_err(error)?;
        let mut writer = pair.master.take_writer().map_err(error)?;

        let mut terminal = Terminal::with_limits(cols, rows, scrollback_limits(config));
        terminal.set_scrollback_memory_limit(config.scrollback_limit_bytes);
        // Like Ghostty, this policy applies to new sessions, not config reloads.
        terminal.set_default_mode(
            true,
            2027,
            config.grapheme_width_method == GraphemeWidthMethod::Unicode,
        );
        terminal.terminfo_name = terminfo_name;
        terminal.shell_command_events = true;
        terminal.linefeed_mode_events = true;
        terminal.query_defaults.color_scheme = options.color_scheme;
        terminal.query_defaults.focused = Some(options.focused);
        terminal.query_defaults.size = Some(terminal.query_size());
        terminal.visible = options.visible;
        apply_appearance(&mut terminal, config);
        terminal.working_directory = options
            .working_directory
            .as_ref()
            .or(config.working_directory.as_ref())
            .map_or_else(
                || std::env::var("HOME").unwrap_or_default(),
                |path| path.to_string_lossy().into_owned(),
            );
        let terminal = Arc::new(Mutex::new(terminal));
        let (input, input_rx) = mpsc::channel();
        let (events_tx, events) = mpsc::sync_channel(256);
        let pending_input = Arc::new(PendingInput::default());
        let exited = Arc::new(AtomicBool::new(false));
        #[cfg(target_os = "macos")]
        let (close_child, child_closed) = std::os::unix::net::UnixStream::pair()?;
        #[cfg(not(target_os = "macos"))]
        let (close_child, child_closed) = mpsc::channel();
        let (started_tx, started) = mpsc::sync_channel(1);

        let writer_events = events_tx.clone();
        let writer_wake = wake.clone();
        let writer_pending = pending_input.clone();
        thread::Builder::new()
            .name("rustty-pty-write".into())
            .spawn(move || {
                let master = pair.master;
                let mut linefeed = false;
                while let Ok(command) = input_rx.recv() {
                    let result = match command {
                        IoCommand::Write(bytes) => {
                            let result = write_pty(&mut writer, &bytes, linefeed);
                            writer_pending.release(bytes.len());
                            result
                        }
                        IoCommand::Linefeed(enabled) => {
                            linefeed = enabled;
                            continue;
                        }
                        IoCommand::HostReport(bytes) => write_pty(&mut writer, &bytes, linefeed),
                        IoCommand::Resize { size, reply } => master
                            .resize(size)
                            .map_err(error)
                            .and_then(|()| write_pty(&mut writer, &reply, linefeed)),
                        IoCommand::Close => break,
                    };
                    if let Err(e) = result {
                        writer_wake();
                        let _ = writer_events.send(SessionEvent::Error(e.to_string()));
                        writer_wake();
                        break;
                    }
                    writer_wake();
                }
                writer_pending.close();
            })?;

        let read_terminal = terminal.clone();
        let read_input = input.clone();
        let read_events = events_tx.clone();
        let read_pending = pending_input.clone();
        let read_wake = wake.clone();
        thread::Builder::new()
            .name("rustty-pty-read".into())
            .spawn(move || {
                let mut buffer = [0; 32 * 1024];
                loop {
                    let length = match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(length) => length,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            read_wake();
                            let _ = read_events.send(SessionEvent::Error(e.to_string()));
                            break;
                        }
                    };
                    let effects = match read_terminal.lock() {
                        Ok(mut terminal) => feed_output(&mut terminal, &buffer[..length]),
                        Err(_) => break,
                    };
                    for effect in effects {
                        match effect {
                            Effect::LinefeedMode(enabled) => {
                                if read_input.send(IoCommand::Linefeed(enabled)).is_err() {
                                    return;
                                }
                            }
                            Effect::Write(bytes) => {
                                // Replies use the same bounded byte budget as user
                                // input. Backpressure cannot silently drop a reply.
                                if let Err(e) = enqueue(&read_input, &read_pending, bytes, true) {
                                    read_wake();
                                    let _ = read_events.send(SessionEvent::Error(e.to_string()));
                                }
                            }
                            // The UI needs ordered title changes to distinguish
                            // working/waiting reports across command boundaries.
                            Effect::WorkingDirectory(_) => {}
                            effect => {
                                read_wake();
                                if read_events.send(SessionEvent::Effect(effect)).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    read_wake();
                }
                read_wake();
                let _ = read_events.send(SessionEvent::OutputClosed);
                read_wake();
            })?;

        let wait_exited = exited.clone();
        // Spawn the process only after all three workers exist. Its owner never
        // crosses a fallible thread-spawn boundary and always performs the wait.
        thread::Builder::new()
            .name("rustty-pty-wait".into())
            .spawn(move || {
                let started = Instant::now();
                let child = pair.slave.spawn_command(command).map_err(error);
                drop(pair.slave);
                let child = match child {
                    Ok(child) => child,
                    Err(error) => {
                        let _ = started_tx.send(Err(error));
                        return;
                    }
                };
                // If the caller timed out or unwound, its close channel is also
                // disconnected; the child owner still terminates and reaps it.
                let _ = started_tx.send(Ok(()));
                let event = match wait_for_child(child, child_closed) {
                    Ok(status) => SessionEvent::Exited {
                        code: status.exit_code(),
                        signal: status.signal().map(str::to_owned),
                        runtime: started.elapsed(),
                    },
                    Err(e) => SessionEvent::Error(e.to_string()),
                };
                wait_exited.store(true, Ordering::Release);
                wake();
                let _ = events_tx.send(event);
                wake();
            })?;

        started
            .recv_timeout(Duration::from_secs(5))
            .map_err(|e| match e {
                mpsc::RecvTimeoutError::Timeout => io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out starting terminal process",
                ),
                mpsc::RecvTimeoutError::Disconnected => error("terminal process worker stopped"),
            })??;

        Ok(Self {
            terminal,
            input,
            events,
            pending_input,
            exited,
            close_child,
        })
    }

    pub fn terminal(&self) -> io::Result<MutexGuard<'_, Terminal>> {
        self.terminal
            .lock()
            .map_err(|_| error("terminal worker panicked"))
    }

    /// Queues the entire input or returns an error without sending any prefix.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        if self.exited.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if bytes.len() > MAX_PENDING_INPUT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "input exceeds the terminal queue budget",
            ));
        }
        enqueue(&self.input, &self.pending_input, bytes.to_owned(), false)
    }

    pub fn resize(&self, cols: u16, rows: u16, width_px: u16, height_px: u16) -> io::Result<()> {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let size = query::Size {
            columns: cols,
            rows,
            cell_width: u32::from(width_px) / u32::from(cols),
            cell_height: u32::from(height_px) / u32::from(rows),
        };
        let mut terminal = self.terminal()?;
        // A redraw with unchanged host geometry must not undo DECCOLM.
        if terminal.query_defaults.size == Some(size)
            && terminal.width_px == u32::from(width_px)
            && terminal.height_px == u32::from(height_px)
        {
            return Ok(());
        }
        terminal.query_defaults.size = Some(size);
        let effects =
            terminal.resize_with_cell_size(cols, rows, Some((size.cell_width, size.cell_height)));
        terminal.set_pixel_size(width_px.into(), height_px.into());
        drop(terminal);
        let mut reply = Vec::new();
        for effect in effects {
            if let Effect::Write(bytes) = effect {
                reply.extend(bytes);
            }
        }
        // The owning IO worker writes the small resize report immediately
        // after its resize, without blocking the UI or dropping a protocol
        // reply when the user-input byte budget is full.
        self.input
            .send(IoCommand::Resize {
                size: PtySize {
                    cols,
                    rows,
                    pixel_width: width_px,
                    pixel_height: height_px,
                },
                reply,
            })
            .map_err(error)?;
        Ok(())
    }

    /// Apply actual window state and notify programs subscribed to its changes.
    pub fn set_host_state(
        &self,
        visible: bool,
        focused: bool,
        scheme: query::ColorScheme,
    ) -> io::Result<()> {
        let mut terminal = self.terminal()?;
        let mut reply = Vec::new();
        if terminal.query_defaults.color_scheme != Some(scheme) {
            terminal.query_defaults.color_scheme = Some(scheme);
            if terminal.modes.dec(2031) {
                reply.extend(scheme.encode());
            }
        }
        if terminal.visible != visible {
            terminal.visible = visible;
            if terminal.modes.dec(2033) {
                reply.extend(query::visibility(visible));
            }
        }
        if terminal.query_defaults.focused != Some(focused) {
            terminal.query_defaults.focused = Some(focused);
            reply.extend(terminal.encode_focus(focused));
        }
        drop(terminal);
        if !reply.is_empty() {
            // Host changes produce at most three small reports. Like resize
            // reports, they must not block the UI on the user-input budget.
            self.input
                .send(IoCommand::HostReport(reply))
                .map_err(error)?;
        }
        Ok(())
    }

    pub fn snapshot(&self) -> io::Result<Snapshot> {
        let terminal = self.terminal()?;
        Ok(Snapshot {
            screen: terminal.screen().snapshot_viewport(),
            cols: terminal.cols,
            rows: terminal.rows,
            generation: terminal.generation,
            foreground: terminal.foreground,
            background: terminal.background,
            palette: terminal.palette.clone(),
            cursor_color: terminal.cursor_color,
            title: terminal.title.clone(),
            working_directory: terminal.working_directory.clone(),
        })
    }

    pub fn events(&self) -> impl Iterator<Item = SessionEvent> + '_ {
        self.events.try_iter()
    }
    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }
    /// Request asynchronous termination. The host can close all sessions, then
    /// give their `has_exited` flags a bounded drain period before process exit.
    pub fn close(&self) {
        self.pending_input.close();
        let _ = self.input.send(IoCommand::Close);
        #[cfg(target_os = "macos")]
        let _ = self.close_child.shutdown(std::net::Shutdown::Write);
        #[cfg(not(target_os = "macos"))]
        let _ = self.close_child.send(());
    }
    pub fn apply_config(&self, config: &Config) -> io::Result<()> {
        let mut terminal = self.terminal()?;
        terminal.set_limits(scrollback_limits(config));
        terminal.set_scrollback_memory_limit(config.scrollback_limit_bytes);
        apply_appearance(&mut terminal, config);
        Ok(())
    }
}

fn scrollback_limits(config: &Config) -> ScrollbackLimits {
    ScrollbackLimits {
        bytes: config.scrollback_limit_bytes,
        lines: config.scrollback_limit_lines,
    }
}

fn feed_output(terminal: &mut Terminal, bytes: &[u8]) -> Vec<Effect> {
    let had_title = !terminal.title_bytes().is_empty();
    let mut effects = terminal.feed(bytes);
    // RIS clears the title without a VT host effect. Reconcile at most one
    // missing clear per read, preserving the ordered title/activity effects.
    if terminal.title_bytes().is_empty()
        && effects
            .iter()
            .rev()
            .find_map(|effect| match effect {
                Effect::Title(title) => Some(!title.is_empty()),
                _ => None,
            })
            .unwrap_or(had_title)
    {
        effects.push(Effect::Title(Vec::new()));
    }
    effects
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

fn wait_for_child(
    mut child: Box<dyn Child + Send + Sync>,
    #[cfg(target_os = "macos")] closed: std::os::unix::net::UnixStream,
    #[cfg(not(target_os = "macos"))] closed: mpsc::Receiver<()>,
) -> io::Result<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            // Even a failed status query must not discard an owned child.
            Err(_) => break,
        }
        #[cfg(target_os = "macos")]
        match wait_for_child_event(child.process_id(), &closed) {
            Ok(true) => {}
            Ok(false) | Err(_) => break,
        }
        // ponytail: other platforms still poll; replace with their native
        // process notifications when desktop support reaches them.
        #[cfg(not(target_os = "macos"))]
        match closed.recv_timeout(Duration::from_millis(50)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    // Closing or failing to register may race with a natural exit. Reap that
    // exit before attempting to signal the process.
    if let Ok(Some(status)) = child.try_wait() {
        return Ok(status);
    }
    // Unlike clone_killer(), the owned portable-pty child escalates SIGHUP to
    // SIGKILL on Unix. Both its grace period and wait stay off the UI thread.
    let _ = child.kill();
    loop {
        match child.wait() {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

/// Block until the process exits (true) or its owner requests close (false).
#[cfg(target_os = "macos")]
fn wait_for_child_event(
    pid: Option<u32>,
    closed: &std::os::unix::net::UnixStream,
) -> io::Result<bool> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    let pid = pid.ok_or_else(|| error("PTY child has no process ID"))?;
    // SAFETY: a successful kqueue call returns a new owned descriptor.
    let fd = unsafe { libc::kqueue() };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let queue = unsafe { OwnedFd::from_raw_fd(fd) };
    if unsafe { libc::fcntl(queue.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let event = |ident, filter, fflags| libc::kevent {
        ident,
        filter,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let changes = [
        event(pid as usize, libc::EVFILT_PROC, libc::NOTE_EXIT),
        event(closed.as_raw_fd() as usize, libc::EVFILT_READ, 0),
    ];
    let mut ready = event(0, 0, 0);
    loop {
        // Both filters are installed before blocking, with no timeout. Closing
        // the socket is persistent EOF, including before this registration.
        // SAFETY: the arrays and both descriptors remain live through kevent.
        let count = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                changes.as_ptr(),
                changes.len() as i32,
                &mut ready,
                1,
                std::ptr::null(),
            )
        };
        let failure = if count < 0 {
            io::Error::last_os_error()
        } else if ready.flags & libc::EV_ERROR != 0 && ready.data != 0 {
            io::Error::from_raw_os_error(ready.data as i32)
        } else if count > 0 {
            return Ok(ready.filter == libc::EVFILT_PROC);
        } else {
            continue;
        };
        if failure.raw_os_error() == Some(libc::ESRCH) {
            // The child exited between try_wait and registering NOTE_EXIT.
            return Ok(true);
        }
        if failure.kind() != io::ErrorKind::Interrupted {
            return Err(failure);
        }
    }
}

struct PendingInput {
    used: Mutex<Option<usize>>,
    available: Condvar,
}
impl Default for PendingInput {
    fn default() -> Self {
        Self {
            used: Mutex::new(Some(0)),
            available: Condvar::new(),
        }
    }
}
impl PendingInput {
    fn reserve(&self, length: usize, wait: bool) -> io::Result<()> {
        if length > MAX_PENDING_INPUT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "input exceeds the terminal queue budget",
            ));
        }
        let mut used = self.used.lock().map_err(error)?;
        loop {
            let current = used.ok_or(io::ErrorKind::BrokenPipe)?;
            if current <= MAX_PENDING_INPUT - length {
                *used = Some(current + length);
                return Ok(());
            }
            if !wait {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "terminal input queue is full",
                ));
            }
            used = self.available.wait(used).map_err(error)?;
        }
    }
    fn release(&self, length: usize) {
        if let Ok(mut used) = self.used.lock()
            && let Some(current) = used.as_mut()
        {
            *current -= length;
        }
        self.available.notify_all();
    }
    fn close(&self) {
        if let Ok(mut used) = self.used.lock() {
            *used = None;
        }
        self.available.notify_all();
    }
}

fn write_pty(writer: &mut impl Write, bytes: &[u8], linefeed: bool) -> io::Result<()> {
    if !linefeed || !bytes.contains(&b'\r') {
        return writer.write_all(bytes);
    }
    let mut expanded = [0; 8192];
    for chunk in bytes.chunks(expanded.len() / 2) {
        let mut length = 0;
        for &byte in chunk {
            expanded[length] = byte;
            length += 1;
            if byte == b'\r' {
                expanded[length] = b'\n';
                length += 1;
            }
        }
        writer.write_all(&expanded[..length])?;
    }
    Ok(())
}

fn enqueue(
    input: &mpsc::Sender<IoCommand>,
    pending: &PendingInput,
    bytes: Vec<u8>,
    wait: bool,
) -> io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let length = bytes.len();
    pending.reserve(length, wait)?;
    if let Err(e) = input.send(IoCommand::Write(bytes)) {
        pending.release(length);
        return Err(error(e));
    }
    Ok(())
}

fn apply_appearance(terminal: &mut Terminal, config: &Config) {
    terminal.clipboard_write_limit = config.clipboard_write_limit_bytes.unwrap_or(usize::MAX);
    terminal.title_report = config.title_report;
    let palette: Vec<_> = config
        .palette
        .iter()
        .map(|color| color.to_array())
        .collect();
    let cursor_color = config.cursor_color.and_then(|color| match color {
        TerminalColor::Rgb(color) => Some(color.to_array()),
        _ => None,
    });
    terminal.set_default_colors(
        Some(config.foreground.to_array()),
        Some(config.background.to_array()),
        cursor_color,
        &palette,
    );
    let shape = match config.cursor_style {
        CursorStyle::Bar => CursorShape::Bar,
        CursorStyle::Underline => CursorShape::Underline,
        CursorStyle::BlockHollow => CursorShape::HollowBlock,
        _ => CursorShape::Block,
    };
    terminal.set_default_cursor(shape, config.cursor_style_blink);
}

fn command(config: &Config, options: &SessionOptions) -> io::Result<CommandBuilder> {
    let selected = options.command.as_ref().or(config.command.as_ref());
    let mut cmd = match selected {
        Some(Command::Direct(args)) => {
            let Some(program) = args.first() else {
                return Err(error("command is empty"));
            };
            let mut cmd = CommandBuilder::new(program);
            cmd.args(&args[1..]);
            cmd
        }
        Some(Command::Shell(text)) => {
            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.args(["-c", text]);
            cmd
        }
        None => CommandBuilder::new_default_prog(),
    };
    for (key, value) in &config.env {
        cmd.env(key, value);
    }
    if let Some(path) = options
        .working_directory
        .as_ref()
        .or(config.working_directory.as_ref())
    {
        cmd.cwd(path);
    } else if let Some(home) = std::env::var_os("HOME") {
        cmd.cwd(home);
    }
    // Programs such as Cargo gate OSC progress on the terminal's identity.
    cmd.env("TERM_PROGRAM", "ghostty");
    cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM", "xterm-256color");
    cmd.env_remove("GHOSTTY_SURFACE_ID");
    if let Some(resources) = &options.resources {
        cmd.env("RUSTTY_RESOURCES_DIR", resources);
        cmd.env("GHOSTTY_RESOURCES_DIR", resources);
        let terminfo = resources.join("terminfo");
        if terminfo.is_dir() {
            cmd.env("TERMINFO", terminfo);
            cmd.env("TERM", "xterm-ghostty");
        }
        if config.shell_integration != ShellIntegration::None && selected.is_none() {
            let shell = cmd.get_shell();
            let shell_name = Path::new(&shell)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            cmd.get_argv_mut().push(shell.clone().into());
            cmd.arg("-l");
            let scripts = resources.join("shell-integration");
            cmd.env("GHOSTTY_SHELL_FEATURES", "cursor:steady,path,title");
            if let Ok(exe) = std::env::current_exe()
                && let Some(parent) = exe.parent()
            {
                cmd.env("GHOSTTY_BIN_DIR", parent);
            }
            match shell_name {
                "zsh" if scripts.join("zsh").is_dir() => {
                    if let Some(old) = cmd.get_env("ZDOTDIR").map(ToOwned::to_owned) {
                        cmd.env("GHOSTTY_ZSH_ZDOTDIR", old);
                    }
                    cmd.env("ZDOTDIR", scripts.join("zsh"));
                }
                "fish" | "nu" | "elvish" => {
                    let old = cmd
                        .get_env("XDG_DATA_DIRS")
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
                    cmd.env("XDG_DATA_DIRS", format!("{}:{old}", scripts.display()));
                    cmd.env("GHOSTTY_SHELL_INTEGRATION_XDG_DIR", &scripts);
                    if shell_name == "nu" {
                        cmd.args(["--execute", "use ghostty *"]);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[cfg(unix)]
    #[test]
    fn emoji_presentation_keeps_the_background_of_both_cells() {
        for legacy in [false, true] {
            let mut config = Config::default();
            if legacy {
                config.grapheme_width_method = GraphemeWidthMethod::Legacy;
            }
            let default_wide = !legacy;
            let session = Session::spawn(
                &config,
                SessionOptions {
                    command: Some(Command::Direct(vec!["/bin/sleep".into(), "30".into()])),
                    ..SessionOptions::default()
                },
                Arc::new(|| {}),
            )
            .unwrap();
            for (prefix, wide) in [
                ("", default_wide),
                ("\x1b[?2027h", true),
                ("\x1b[?2027l", false),
                ("\x1bc", default_wide),
            ] {
                let mut terminal = session.terminal().unwrap();
                terminal.feed(b"\x1b[0m\x1b[2J\x1b[H");
                terminal.feed(prefix.as_bytes());
                // A TUI skips the second cell of an emoji when positioning its next run.
                terminal.feed("\x1b[30;107m  ✔️\x1b[5G  > selected row\x1b[0m".as_bytes());
                let cells = &terminal.screen().rows[0].cells;
                assert_eq!(
                    cells[3].style.background,
                    if wide {
                        rustty_vt::Color::Indexed(15)
                    } else {
                        rustty_vt::Color::Default
                    },
                );
                assert_eq!(
                    &*terminal.screen().cell_text(&terminal.screen().rows[0], 2),
                    "✔️"
                );
                assert_eq!(
                    (cells[2].width, cells[3].width),
                    if wide { (2, 0) } else { (1, 1) }
                );
            }
            config.grapheme_width_method = if default_wide {
                GraphemeWidthMethod::Legacy
            } else {
                GraphemeWidthMethod::Unicode
            };
            session.apply_config(&config).unwrap();
            let terminal = session.terminal().unwrap();
            assert_eq!(terminal.modes.get_default(true, 2027), Some(default_wide));
            assert_eq!(terminal.modes.dec(2027), default_wide);
        }
    }

    #[test]
    fn title_reporting_follows_configuration_reload_and_survives_reset() {
        let mut terminal = Terminal::new(10, 2, 0);
        for title_report in [false, true, false] {
            let mut config = Config::default();
            config.title_report = title_report;
            apply_appearance(&mut terminal, &config);
            terminal.feed(b"\x1bc\x1b]2;title\x07");
            let expected = if title_report {
                vec![Effect::Write(b"\x1b]ltitle\x1b\\".to_vec())]
            } else {
                Vec::new()
            };
            assert_eq!(terminal.feed(b"\x1b[21t"), expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn column_mode_survives_redraw_until_host_geometry_changes() {
        let session = Session::spawn(
            &Config::default(),
            SessionOptions {
                command: Some(Command::Direct(vec!["/bin/sleep".into(), "30".into()])),
                ..SessionOptions::default()
            },
            Arc::new(|| {}),
        )
        .unwrap();
        session.resize(100, 10, 900, 180).unwrap();
        session.terminal().unwrap().feed(b"\x1b[?40h\x1b[?3h");
        let generation = session.terminal().unwrap().generation;
        session.resize(100, 10, 900, 180).unwrap();
        {
            let mut terminal = session.terminal().unwrap();
            assert_eq!(terminal.cols, 132);
            assert_eq!(terminal.generation, generation);
            assert_eq!(
                terminal.feed(b"\x1b[14t\x1b[16t\x1b[18t\x1b[?2048h"),
                [
                    Effect::Write(b"\x1b[4;180;900t".to_vec()),
                    Effect::Write(b"\x1b[6;18;9t".to_vec()),
                    Effect::Write(b"\x1b[8;10;100t".to_vec()),
                    Effect::Write(b"\x1b[48;10;100;180;900t".to_vec()),
                ]
            );
            terminal.feed(b"\x1b[?3l");
        }
        session.resize(100, 10, 900, 180).unwrap();
        {
            let mut terminal = session.terminal().unwrap();
            assert_eq!(terminal.cols, 80);
            terminal.feed(b"\x1b[?40l");
            assert_eq!(terminal.cols, 100);
            terminal.feed(b"\x1b[?40h\x1b[?3h\x1b[?40h");
            assert_eq!(terminal.cols, 100);
            terminal.feed(b"\x1b[?3h");
        }
        // Even a pixel-only host resize restores the actual window grid.
        session.resize(100, 10, 901, 180).unwrap();
        assert_eq!(session.terminal().unwrap().cols, 100);
        session.terminal().unwrap().feed(b"\x1b[?3h");
        session.resize(90, 11, 810, 198).unwrap();
        let mut terminal = session.terminal().unwrap();
        assert_eq!((terminal.cols, terminal.rows), (90, 11));
        terminal.feed(b"\x1b[?3h\x1b[?40l");
        assert_eq!((terminal.cols, terminal.rows), (90, 11));
    }

    #[cfg(unix)]
    #[test]
    fn linefeed_mode_translates_pty_input_and_keeps_expansion_bounded() {
        let mut output = Vec::new();
        write_pty(&mut output, &vec![b'\r'; 4097], true).unwrap();
        assert_eq!(output, b"\r\n".repeat(4097));

        let session = Session::spawn(
            &Config::default(),
            SessionOptions {
                command: Some(Command::Direct(vec![
                    "/bin/sh".into(), "-c".into(),
                    r"stty raw -echo; printf '\033[20h\033]2;linefeed-on\007'; dd bs=1 count=6 2>/dev/null | od -An -tx1 | tr -d ' \n'; printf '\r\n\033[20l\033]2;linefeed-off\007'; dd bs=1 count=4 2>/dev/null | od -An -tx1 | tr -d ' \n'; printf '\r\n\033]2;linefeed-done\007'".into(),
                ])),
                ..SessionOptions::default()
            },
            Arc::new(|| {}),
        ).unwrap();
        let wait_for_title = |title: &[u8]| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                for event in session.events() {
                    match event {
                        SessionEvent::Effect(Effect::Title(value)) if value == title => return,
                        SessionEvent::Error(error) => panic!("{error}"),
                        _ => {}
                    }
                }
                assert!(Instant::now() < deadline, "missing title {title:?}");
                thread::sleep(Duration::from_millis(5));
            }
        };
        wait_for_title(b"linefeed-on");
        session.write(b"a\rb\r").unwrap();
        wait_for_title(b"linefeed-off");
        assert!(
            session
                .terminal()
                .unwrap()
                .plain_text()
                .contains("610d0a620d0a")
        );
        session.write(b"a\rb\r").unwrap();
        wait_for_title(b"linefeed-done");
        assert!(
            session
                .terminal()
                .unwrap()
                .plain_text()
                .contains("610d620d")
        );
    }

    #[cfg(unix)]
    #[test]
    fn host_queries_and_subscribed_changes_reach_the_pty_without_input_budget() {
        let session = Session::spawn(
            &Config::default(),
            SessionOptions {
                color_scheme: Some(query::ColorScheme::Light),
                visible: false,
                command: Some(Command::Direct(vec![
                    "/bin/sh".into(), "-c".into(),
                    r"stty raw -echo; printf '\033[?996n\033[?998n\033[?1004h'; dd bs=1 count=21 2>/dev/null | od -An -tx1 | tr -d ' \n'; printf '\r\nstate-ready\r\n'; dd bs=1 count=21 2>/dev/null | od -An -tx1 | tr -d ' \n'; stty min 0 time 1; dd bs=64 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n'; printf '\r\nstate-done\r\n'".into(),
                ])),
                ..SessionOptions::default()
            },
            Arc::new(|| {}),
        ).unwrap();
        let wait_for = |marker: &str| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let text = session.terminal().unwrap().plain_text();
                if text.contains(marker) {
                    return text;
                }
                assert!(Instant::now() < deadline, "missing {marker}: {text:?}");
                thread::sleep(Duration::from_millis(5));
            }
        };
        assert!(wait_for("state-ready").contains("1b5b3f3939373b326e1b5b3f3939393b326e1b5b4f"));
        {
            let mut terminal = session.terminal().unwrap();
            terminal.modes.set(true, 2031, true);
            terminal.modes.set(true, 2033, true);
        }
        // A full user-input queue must neither block nor discard host reports.
        *session.pending_input.used.lock().unwrap() = Some(MAX_PENDING_INPUT);
        session
            .set_host_state(true, true, query::ColorScheme::Dark)
            .unwrap();
        session
            .set_host_state(true, true, query::ColorScheme::Dark)
            .unwrap();
        {
            let mut terminal = session.terminal().unwrap();
            terminal.modes.set(true, 2031, false);
            terminal.modes.set(true, 2033, false);
            terminal.modes.set(true, 1004, false);
        }
        session
            .set_host_state(false, false, query::ColorScheme::Light)
            .unwrap();
        let text = wait_for("state-done");
        assert_eq!(
            text.matches("1b5b3f3939373b316e1b5b3f3939393b316e1b5b49")
                .count(),
            1
        );
        assert_eq!(
            text.matches("1b5b3f3939373b326e1b5b3f3939393b326e1b5b4f")
                .count(),
            1
        );
        assert_eq!(
            *session.pending_input.used.lock().unwrap(),
            Some(MAX_PENDING_INPUT)
        );
    }

    #[cfg(unix)]
    #[test]
    fn activity_title_effects_keep_their_order_around_command_boundaries() {
        let session = Session::spawn(
            &Config::default(),
            SessionOptions {
                command: Some(Command::Direct(vec![
                    "/bin/sh".into(), "-c".into(),
                    r"printf '\033]133;C\007\033]0;working\007\033]9;4;1;23\007\033]133;D;0\007\033]0;ready\007'".into(),
                ])),
                ..SessionOptions::default()
            },
            Arc::new(|| {}),
        ).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut effects = Vec::new();
        let mut closed = false;
        while !closed && Instant::now() < deadline {
            for event in session.events() {
                match event {
                    SessionEvent::Effect(Effect::Title(title)) => {
                        effects.push(format!("title:{}", String::from_utf8_lossy(&title)))
                    }
                    SessionEvent::Effect(Effect::CommandStart) => effects.push("start".into()),
                    SessionEvent::Effect(Effect::CommandEnd { .. }) => effects.push("end".into()),
                    SessionEvent::Effect(Effect::Progress { state, .. }) => {
                        effects.push(format!("progress:{state}"))
                    }
                    SessionEvent::OutputClosed => closed = true,
                    SessionEvent::Error(error) => panic!("{error}"),
                    _ => {}
                }
            }
            if !closed {
                thread::sleep(Duration::from_millis(5));
            }
        }
        assert!(closed, "child did not finish its terminal output");
        assert_eq!(
            effects,
            ["start", "title:working", "progress:1", "end", "title:ready"]
        );
    }

    #[test]
    fn reader_reconciles_reset_titles_across_input_batches_without_extra_events() {
        let stream = b"\x1b]133;C\x07\x1b]2;temporary\x07\x1b]133;D;0\x07\x1bc";
        for split in 0..=stream.len() {
            let mut terminal = Terminal::new(20, 2, 0);
            terminal.shell_command_events = true;
            let mut effects = feed_output(&mut terminal, &stream[..split]);
            effects.extend(feed_output(&mut terminal, &stream[split..]));
            assert_eq!(
                effects,
                [
                    Effect::CommandStart,
                    Effect::Title(b"temporary".to_vec()),
                    Effect::CommandEnd { exit_code: Some(0) },
                    Effect::Progress {
                        state: 0,
                        value: None,
                    },
                    Effect::Title(Vec::new()),
                ],
                "read split at {split}"
            );
            assert!(terminal.title_bytes().is_empty());
            assert!(feed_output(&mut terminal, b"ordinary output").is_empty());
        }

        let mut terminal = Terminal::new(20, 2, 0);
        assert!(feed_output(&mut terminal, b"initial output").is_empty());
        assert_eq!(
            feed_output(&mut terminal, b"\x1b]2;retained\x07"),
            [Effect::Title(b"retained".to_vec())]
        );
        assert!(feed_output(&mut terminal, b"more output").is_empty());
        assert_eq!(terminal.title_bytes(), b"retained");
        // An explicit empty title already provides the reset's final state.
        assert_eq!(
            feed_output(&mut terminal, b"\x1b]2;\x07\x1bc"),
            [
                Effect::Title(Vec::new()),
                Effect::Progress {
                    state: 0,
                    value: None,
                },
            ]
        );
        assert_eq!(
            feed_output(&mut terminal, b"\x1bc"),
            [Effect::Progress {
                state: 0,
                value: None,
            }]
        );
    }

    #[test]
    fn exec_failure_is_reported_by_spawn() {
        let result = Session::spawn(
            &Config::default(),
            SessionOptions {
                command: Some(Command::Direct(vec![
                    "/rustty-test-does-not-exist/executable".into(),
                ])),
                ..SessionOptions::default()
            },
            Arc::new(|| {}),
        );
        assert!(
            result.is_err(),
            "exec failure must not return a live session"
        );
    }

    #[cfg(unix)]
    #[test]
    fn closing_or_dropping_session_reaps_a_child_that_ignores_hangup() {
        for explicit_close in [false, true] {
            let session = Session::spawn(
                &Config::default(),
                SessionOptions {
                    command: Some(Command::Direct(vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "trap '' HUP; printf 'ready:%s:done\\n' \"$$\"; exec /bin/sleep 30".into(),
                    ])),
                    ..SessionOptions::default()
                },
                Arc::new(|| {}),
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let pid = loop {
                let text = session.terminal().unwrap().plain_text();
                if let Some(pid) = text
                    .lines()
                    .find_map(|line| line.strip_prefix("ready:"))
                    .and_then(|pid| pid.trim().strip_suffix(":done"))
                    .and_then(|pid| pid.parse::<u32>().ok())
                {
                    break pid;
                }
                assert!(Instant::now() < deadline, "child did not become ready");
                thread::sleep(Duration::from_millis(5));
            };
            let exited = session.exited.clone();
            let before_drop = Instant::now();
            let retained = if explicit_close {
                session.close();
                Some(session)
            } else {
                drop(session);
                None
            };
            assert!(before_drop.elapsed() < Duration::from_secs(1));
            let deadline = Instant::now() + Duration::from_secs(3);
            while !exited.load(Ordering::Acquire) && Instant::now() < deadline {
                if let Some(session) = &retained {
                    assert!(session.terminal().unwrap().plain_text().contains("ready:"));
                    session.snapshot().unwrap();
                }
                thread::sleep(Duration::from_millis(5));
            }
            let reaped = exited.load(Ordering::Acquire);
            if !reaped {
                // Clean up even if the regression returns; the PID came from this
                // exact child, which is still owned by the session's waiter.
                let _ = std::process::Command::new("/bin/kill")
                    .args(["-KILL", &pid.to_string()])
                    .status();
            }
            assert!(
                reaped,
                "closing the session did not terminate and reap its child"
            );
            if let Some(session) = retained {
                assert!(session.has_exited());
                assert!(session.terminal().unwrap().plain_text().contains("ready:"));
                session.snapshot().unwrap();
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn idle_child_waiter_checks_status_only_when_woken() {
        use portable_pty::ChildKiller;
        use std::sync::atomic::AtomicUsize;

        #[derive(Debug)]
        struct IdleChild(Arc<AtomicUsize>, mpsc::Sender<()>);
        impl ChildKiller for IdleChild {
            fn kill(&mut self) -> io::Result<()> {
                Ok(())
            }
            fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
                unreachable!("the waiter owns and kills its child")
            }
        }
        impl Child for IdleChild {
            fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                let _ = self.1.send(());
                Ok(None)
            }
            fn wait(&mut self) -> io::Result<ExitStatus> {
                Ok(ExitStatus::with_exit_code(0))
            }
            fn process_id(&self) -> Option<u32> {
                // Watch a process that stays alive; the fake kill never signals it.
                Some(std::process::id())
            }
        }

        let checks = Arc::new(AtomicUsize::new(0));
        let (entered, ready) = mpsc::channel();
        let (close, closed) = std::os::unix::net::UnixStream::pair().unwrap();
        let (done, finished) = mpsc::channel();
        let child = IdleChild(checks.clone(), entered);
        thread::spawn(move || done.send(wait_for_child(Box::new(child), closed)).unwrap());
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        thread::sleep(Duration::from_millis(180));
        let idle_checks = checks.load(Ordering::SeqCst);
        drop(close);
        assert!(
            finished
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_ok()
        );
        assert_eq!(idle_checks, 1, "an idle child must not be polled");
    }

    #[test]
    fn configured_scrollback_memory_is_bounded_across_reload_and_reset() {
        let mut config = Config::default();
        config.scrollback_limit_bytes = Some(24 * 1024);
        let session = Session::spawn(
            &config,
            SessionOptions {
                rows: 2,
                command: Some(Command::Direct(vec!["/bin/sleep".into(), "30".into()])),
                ..SessionOptions::default()
            },
            Arc::new(|| {}),
        )
        .unwrap();
        let output = b"line\r\n".repeat(40);
        let before_reload = {
            let mut terminal = session.terminal().unwrap();
            terminal.feed(&output);
            assert!(!terminal.screen().history.is_empty());
            assert!(terminal.screen().history_bytes() <= 24 * 1024);
            terminal.screen().history_bytes()
        };

        config.scrollback_limit_bytes = Some(12 * 1024);
        session.apply_config(&config).unwrap();
        let mut terminal = session.terminal().unwrap();
        assert!(!terminal.screen().history.is_empty());
        assert!(terminal.screen().history_bytes() < before_reload);
        assert!(terminal.screen().history_bytes() <= 12 * 1024);
        terminal.feed(b"\x1bc");
        terminal.feed(&output);
        assert!(!terminal.screen().history.is_empty());
        assert!(terminal.screen().history_bytes() <= 12 * 1024);
    }

    #[test]
    fn short_lived_child_is_reaped_and_final_output_drained() {
        let mut config = Config::default();
        config.scrollback_limit_bytes = None;
        config.scrollback_limit_lines = Some(4);
        let before_spawn = Instant::now();
        let session = Session::spawn(
            &config,
            SessionOptions {
                command: Some(Command::Direct(vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf 'rustty-ready'; exit 7".into(),
                ])),
                ..SessionOptions::default()
            },
            Arc::new(|| {}),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !session.has_exited() {
            assert!(Instant::now() < deadline, "child did not exit");
            thread::sleep(Duration::from_millis(5));
        }
        let maximum_runtime = before_spawn.elapsed();
        // Simulate an app occupied with other work after the process has exited.
        thread::sleep(Duration::from_millis(300));
        let (mut exited, mut eof) = (false, false);
        while !(exited && eof) {
            assert!(
                Instant::now() < deadline,
                "child did not exit and close its output"
            );
            for event in session.events() {
                match event {
                    SessionEvent::Exited { code, runtime, .. } => {
                        assert_eq!(code, 7);
                        assert!(runtime <= maximum_runtime);
                        exited = true;
                    }
                    SessionEvent::OutputClosed => eof = true,
                    SessionEvent::Error(e) => panic!("{e}"),
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            session
                .terminal()
                .unwrap()
                .plain_text()
                .contains("rustty-ready")
        );
        session
            .terminal()
            .unwrap()
            .feed("\r\nline".repeat(40).as_bytes());
        // Nonzero limits retain at least an initial page. These 17 history
        // rows fit in that page and survive both tiny configured line limits.
        assert_eq!(session.terminal().unwrap().limits().lines, Some(4));
        assert_eq!(session.terminal().unwrap().screen().history.len(), 17);
        config.scrollback_limit_lines = Some(1);
        config.clipboard_write_limit_bytes = Some(17);
        session.apply_config(&config).unwrap();
        assert_eq!(session.terminal().unwrap().limits().lines, Some(1));
        assert_eq!(session.terminal().unwrap().screen().history.len(), 17);
        assert_eq!(session.terminal().unwrap().clipboard_write_limit, 17);
        config.scrollback_limit_bytes = Some(0);
        session.apply_config(&config).unwrap();
        assert!(session.terminal().unwrap().screen().history.is_empty());
    }

    #[test]
    fn backpressure_rejects_an_entire_write_without_partial_delivery() {
        let (sender, receiver) = mpsc::channel();
        let pending = PendingInput {
            used: Mutex::new(Some(MAX_PENDING_INPUT - 1)),
            available: Condvar::new(),
        };
        assert_eq!(
            enqueue(&sender, &pending, b"ab".to_vec(), false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(receiver.try_recv().is_err());
        assert_eq!(*pending.used.lock().unwrap(), Some(MAX_PENDING_INPUT - 1));
        enqueue(&sender, &pending, b"x".to_vec(), false).unwrap();
        assert!(matches!(receiver.try_recv(), Ok(IoCommand::Write(bytes)) if bytes == b"x"));
    }

    #[test]
    fn protocol_reply_waits_for_budget_instead_of_being_dropped() {
        let (sender, receiver) = mpsc::channel();
        let pending = Arc::new(PendingInput {
            used: Mutex::new(Some(MAX_PENDING_INPUT)),
            available: Condvar::new(),
        });
        let producer_pending = pending.clone();
        let producer =
            thread::spawn(move || enqueue(&sender, &producer_pending, b"reply".to_vec(), true));
        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        pending.release(5);
        assert!(
            matches!(receiver.recv_timeout(Duration::from_secs(2)), Ok(IoCommand::Write(bytes)) if bytes == b"reply")
        );
        producer.join().unwrap().unwrap();
    }
}
