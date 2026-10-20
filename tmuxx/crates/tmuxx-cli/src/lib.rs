use clap::{CommandFactory, FromArgMatches, Parser};
use std::ffi::OsString;
use std::io::{self, BufReader, Write};
use std::path::PathBuf;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;
use tmuxx_protocol::encode_output;
use tmuxx_protocol::native::{Request, ServerMessage};
#[cfg(test)]
mod arguments_tests;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
type NativeStream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type NativeStream = tmuxx_runtime::windows::PipeStream;

const STARTUP_BUSY_EXIT: i32 = 75;

#[derive(Parser, Debug)]
#[command(
    name = "tmuxx",
    about = "Headless tmux-compatible command/control server",
    disable_version_flag = true,
    trailing_var_arg = true,
    args_override_self = true
)]
struct Cli {
    /// Request a UTF-8 client (native clients always use UTF-8).
    #[arg(short = 'u')]
    utf8: bool,
    /// Advertise 256-color support.
    #[arg(short = '2', action=clap::ArgAction::Count)]
    color_256: u8,
    /// Advertise terminal features to the server.
    #[arg(short = 'T')]
    features: Vec<String>,
    /// Retained deprecated tmux no-op.
    #[arg(short = 'q')]
    quiet: bool,
    /// Login tty shell mode is outside the native profile.
    #[arg(short = 'l')]
    login: bool,
    /// Shell replacement is outside the native profile.
    #[arg(short = 'c')]
    shell_command: Option<OsString>,
    #[arg(skip)]
    feature_bits: i32,
    #[arg(short='C',action=clap::ArgAction::Count)]
    control: u8,
    #[arg(short = 'D')]
    foreground: bool,
    #[arg(short = 'N')]
    no_start: bool,
    #[arg(short = 'V')]
    version: bool,
    #[arg(short='v',action=clap::ArgAction::Count)]
    verbose: u8,
    #[arg(short = 'f')]
    config: Vec<PathBuf>,
    #[arg(short = 'L')]
    label: Option<String>,
    #[arg(short = 'S')]
    socket: Option<PathBuf>,
    /// Explicit native socket; never confused with a system tmux socket.
    #[arg(long)]
    native_endpoint: Option<PathBuf>,
    /// Bridge raw native stdio to an existing endpoint; never starts a server.
    #[arg(long)]
    native_stdio: bool,
    /// imsg header profile for -S; protocol version is independent of this schema.
    #[arg(long, value_enum, default_value = "modern")]
    imsg_schema: ImsgSchema,
    #[arg(long)]
    snapshot: bool,
    #[arg(long)]
    exit_empty: bool,
    /// Internal bounded failure channel for the spawning client.
    #[arg(long, hide = true)]
    report_startup_error: bool,
    command: Vec<OsString>,
    #[arg(skip)]
    launcher_prefix: Vec<String>,
    #[arg(skip)]
    native_default: bool,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum ImsgSchema {
    Modern,
    Legacy,
}

/// Run the command driver, including its normal CLI diagnostics and exit status.
///
/// `args` includes argv[0]. An embedded subcommand supplies its launch prefix
/// (for example `["tmux"]`) and defaults to native endpoints rather than C sockets.
pub fn run_from(args: impl IntoIterator<Item = OsString>, launcher_prefix: &[&str]) {
    let matches = Cli::command().get_matches_from(args);
    let mut cli = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    cli.launcher_prefix = launcher_prefix
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect();
    cli.native_default = !launcher_prefix.is_empty();
    {
        let mut features: Vec<_> = matches
            .indices_of("features")
            .into_iter()
            .flatten()
            .zip(cli.features.iter().map(String::as_str))
            .collect();
        if cli.color_256 != 0
            && let Some(index) = matches.index_of("color_256")
        {
            features.push((index, "256"));
        }
        features.sort_by_key(|(index, _)| *index);
        for (_, value) in features {
            cli.feature_bits = tmuxx_protocol::features::apply_features(cli.feature_bits, value);
        }
    }
    let report_startup_error = cli.report_startup_error;
    if let Err(error) = run(cli) {
        if report_startup_error {
            let message = error.to_string();
            let _ = io::stdout().write_all(&message.as_bytes()[..message.len().min(4096)]);
        } else {
            eprintln!("tmuxx: {error}");
        }
        std::process::exit(
            if report_startup_error && error.kind() == io::ErrorKind::WouldBlock {
                STARTUP_BUSY_EXIT
            } else {
                1
            },
        );
    }
}

#[cfg(unix)]
fn socket_path(
    socket: Option<&std::path::Path>,
    label: Option<&str>,
    inherited: Option<OsString>,
    temporary: Option<OsString>,
) -> io::Result<PathBuf> {
    use std::os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{DirBuilderExt, MetadataExt},
    };
    if let Some(path) = socket {
        return Ok(path.to_owned());
    }
    if label.is_none()
        && let Some(value) = inherited
    {
        let value = value
            .as_bytes()
            .split(|b| *b == b',')
            .next()
            .unwrap_or_default();
        if !value.is_empty() {
            return Ok(OsString::from_vec(value.to_vec()).into());
        }
    }
    let base = temporary
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    if !base.is_absolute()
        || base
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket directory must be absolute and contain no ..",
        ));
    }
    let base = base.canonicalize()?;
    let uid = unsafe { libc::getuid() };
    let dir = base.join(format!("tmux-{uid}"));
    if let Err(error) = std::fs::DirBuilder::new().mode(0o700).create(&dir)
        && error.kind() != io::ErrorKind::AlreadyExists
    {
        return Err(error);
    }
    let metadata = std::fs::symlink_metadata(&dir)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o007 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "directory {} has unsafe ownership or permissions",
                dir.display()
            ),
        ));
    }
    let label = label.unwrap_or("default");
    if label.contains('/') || matches!(label, "." | "..") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid socket label",
        ));
    }
    Ok(dir.join(label))
}

#[cfg(unix)]
fn native_path(socket: &std::path::Path) -> PathBuf {
    let mut native = socket.as_os_str().to_os_string();
    native.push(".native");
    native.into()
}

/// The native endpoint shared by the GUI and `rustty tmux`.
/// `TMUXX_ENDPOINT` overrides the per-user default; an inherited `TMUX` never does.
pub fn default_native_endpoint() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("TMUXX_ENDPOINT") {
        return Ok(path.into());
    }
    #[cfg(unix)]
    return socket_path(None, None, None, std::env::var_os("TMUX_TMPDIR"))
        .map(|path| native_path(&path));
    #[cfg(windows)]
    return tmuxx_runtime::windows::endpoint("default");
    #[cfg(not(any(unix, windows)))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native tmux hosting is not implemented on this platform",
    ))
}

fn run(mut cli: Cli) -> io::Result<()> {
    if cli.version {
        println!("tmuxx {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if cli.login || cli.shell_command.is_some() || cli.verbose != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "-l, -c shell replacement and -v log files are outside the native profile",
        ));
    }
    #[cfg(windows)]
    if cli.native_stdio {
        if cli.foreground
            || cli.control != 0
            || !cli.command.is_empty()
            || cli.socket.is_some()
            || cli.label.is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--native-stdio accepts instance selection through its hello only",
            ));
        }
        return windows::native_stdio_bridge(cli.native_endpoint.as_deref());
    }
    #[cfg(windows)]
    if cli.socket.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "-S is Unix tmux IPC; use -L INSTANCE or --native-endpoint on Windows",
        ));
    }
    let explicit_native = if cli.socket.is_none() && cli.label.is_none() {
        match cli.native_endpoint.clone() {
            Some(path) => Some(path),
            None if cli.native_default || cli.native_stdio => Some(default_native_endpoint()?),
            None => std::env::var_os("TMUXX_ENDPOINT").map(PathBuf::from),
        }
    } else {
        cli.native_endpoint.clone()
    };
    #[cfg(unix)]
    let native_only = explicit_native.is_some() && cli.socket.is_none() && cli.label.is_none();
    #[cfg(unix)]
    let socket = if native_only {
        None
    } else {
        Some(socket_path(
            cli.socket.as_deref(),
            cli.label.as_deref(),
            std::env::var_os("TMUX"),
            std::env::var_os("TMUX_TMPDIR"),
        )?)
    };
    #[cfg(not(unix))]
    let socket: Option<PathBuf> = None;
    #[cfg(not(windows))]
    let endpoint = explicit_native
        .or_else(|| socket.as_deref().map(native_path))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "no native endpoint on this platform",
            )
        })?;
    #[cfg(windows)]
    let endpoint = match explicit_native {
        Some(_) if cli.label.is_some() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "-L and --native-endpoint are mutually exclusive",
            ));
        }
        Some(endpoint) => endpoint,
        None => tmuxx_runtime::windows::endpoint(cli.label.as_deref().unwrap_or("default"))?,
    };
    cli.socket = socket;
    #[cfg(unix)]
    if cli.native_stdio {
        if cli.foreground || cli.control != 0 || !cli.command.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--native-stdio does not accept a command or tty mode",
            ));
        }
        return native_stdio_bridge(&endpoint);
    }
    if cli.foreground {
        if !cli.command.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "-D does not accept a command",
            ));
        }
        let mut options = tmuxx_runtime::Options::new(endpoint);
        options.tmux_endpoint = cli.socket;
        options.config = cli.config;
        options.exit_empty = cli.exit_empty;
        return tmuxx_runtime::start(options)?.wait();
    }
    #[cfg(unix)]
    return if cli.socket.is_some() {
        run_tmux(cli, endpoint)
    } else {
        run_unix(cli, endpoint)
    };
    #[cfg(windows)]
    {
        let mut stream = tmuxx_runtime::windows::connect(&endpoint)?;
        let hello = tmuxx_runtime::native_handshake_with_features(
            &mut stream,
            cli.control == 0,
            Some(cli.feature_bits),
        )?;
        if cli.control > 0 && !hello.control_commands {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "server does not support control command framing",
            ));
        }
        native_client(cli, stream, hello.stdin_file_stream)
    }
    #[cfg(not(any(unix, windows)))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native client transport is not implemented on this platform",
    ))
}

#[cfg(unix)]
fn native_stdio_bridge(endpoint: &std::path::Path) -> io::Result<()> {
    use std::net::Shutdown;
    let stream = std::os::unix::net::UnixStream::connect(endpoint)?;
    let mut writer = stream.try_clone()?;
    std::thread::spawn(move || {
        let _ = copy_bridge_bytes(&mut io::stdin().lock(), &mut writer);
        let _ = writer.shutdown(Shutdown::Write);
    });
    let mut reader = stream;
    copy_bridge_bytes(&mut reader, &mut io::stdout().lock())
}

#[cfg(unix)]
fn copy_bridge_bytes(input: &mut impl io::Read, output: &mut impl io::Write) -> io::Result<()> {
    // This is an interactive duplex bridge. Linux's kernel splice specialization
    // of io::copy can leave tiny pipe/socket writes waiting for another transfer;
    // forward each available chunk and flush without waiting for a full record.
    let mut buffer = [0; 32 * 1024];
    loop {
        let length = match input.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(length) => length,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        output.write_all(&buffer[..length])?;
        output.flush()?;
    }
}

#[cfg(unix)]
fn run_unix(cli: Cli, endpoint: PathBuf) -> io::Result<()> {
    let mut stream = connect_or_start(&cli, &endpoint, None)?;
    let hello = tmuxx_runtime::native_handshake_with_features(
        &mut stream,
        cli.control == 0,
        Some(cli.feature_bits),
    )?;
    if cli.control > 0 && !hello.control_commands {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native server does not support control command framing; use its tmux command socket",
        ));
    }
    native_client(cli, stream, hello.stdin_file_stream)
}

#[cfg(unix)]
fn connect_or_start(
    cli: &Cli,
    endpoint: &PathBuf,
    tmux_socket: Option<&PathBuf>,
) -> io::Result<std::os::unix::net::UnixStream> {
    use std::os::unix::{ffi::OsStrExt, net::UnixStream};
    let connect_path = tmux_socket.unwrap_or(endpoint);
    match UnixStream::connect(connect_path) {
        Ok(s) => Ok(s),
        Err(error) => {
            let starts = (cli.control > 0 && cli.command.is_empty())
                || tmuxx_command::parse_argv(cli.command.iter().map(|arg| arg.as_bytes()))
                    .ok()
                    .and_then(|commands| {
                        commands
                            .iter()
                            .map(tmuxx_command::registry::validate)
                            .collect::<Result<Vec<_>, _>>()
                            .ok()
                    })
                    .is_some_and(|entries| entries.iter().any(|entry| entry.starts_server));
            if cli.no_start || !starts {
                return Err(error);
            }
            let mut process = server_command(cli, endpoint, tmux_socket)?;
            let mut child = process.spawn()?;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(s) = UnixStream::connect(connect_path) {
                    break Ok(s);
                }
                if let Some(status) = child.try_wait()? {
                    use std::io::Read;
                    let mut detail = Vec::new();
                    if let Some(stdout) = child.stdout.take() {
                        stdout.take(4096).read_to_end(&mut detail)?;
                    }
                    // A previous owner can close its listener before releasing its lifetime lock.
                    if status.code() == Some(STARTUP_BUSY_EXIT) && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(10));
                        child = process.spawn()?;
                        continue;
                    }
                    return Err(io::Error::other(format!(
                        "server startup failed: {status}: {}",
                        String::from_utf8_lossy(&detail)
                    )));
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "server did not create its endpoint",
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// Prepare an independent native daemon using this executable and its CLI prefix.
/// The caller may append `-f` options before spawning; stdio is disconnected.
#[cfg(any(unix, windows))]
pub fn native_daemon_command(
    endpoint: &std::path::Path,
    launcher_prefix: &[&str],
) -> io::Result<std::process::Command> {
    #[cfg(unix)]
    use std::os::unix::process::CommandExt;
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    let mut process = std::process::Command::new(std::env::current_exe()?);
    process
        .args(launcher_prefix)
        .arg("-D")
        .arg("--native-endpoint")
        .arg(endpoint)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Only the async-signal-safe setsid syscall runs between fork and exec.
    #[cfg(unix)]
    unsafe {
        process.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    // An explicit retained owner must not inherit the GUI's console or its
    // console-control group. Pane Job Objects remain owned by this new process.
    #[cfg(windows)]
    process.creation_flags(
        windows_sys::Win32::System::Threading::DETACHED_PROCESS
            | windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP,
    );
    Ok(process)
}

#[cfg(unix)]
fn server_command(
    cli: &Cli,
    endpoint: &std::path::Path,
    tmux_socket: Option<&PathBuf>,
) -> io::Result<std::process::Command> {
    let prefix: Vec<_> = cli.launcher_prefix.iter().map(String::as_str).collect();
    let mut process = native_daemon_command(endpoint, &prefix)?;
    process
        .arg("--report-startup-error")
        .stdout(std::process::Stdio::piped());
    if let Some(path) = tmux_socket {
        process.arg("-S").arg(path);
    }
    if cli.exit_empty {
        process.arg("--exit-empty");
    }
    for config in &cli.config {
        process.arg("-f").arg(config);
    }
    Ok(process)
}

#[cfg(any(unix, windows))]
fn native_client(cli: Cli, mut stream: NativeStream, stdin_file_stream: bool) -> io::Result<()> {
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;
    #[cfg(unix)]
    let argv: Vec<Vec<u8>> = cli.command.iter().map(|s| s.as_bytes().to_vec()).collect();
    #[cfg(windows)]
    let argv = cli
        .command
        .iter()
        .map(|s| {
            s.to_str().map(|s| s.as_bytes().to_vec()).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Windows command arguments must be Unicode",
                )
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    if cli.control > 1 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "-CC terminal entry is not yet implemented; use -C",
        ));
    }
    let request = Request {
        request_id: 1,
        stdin: if cli.control == 0
            && !stdin_file_stream
            && tmuxx_runtime::command_reads_stdin(&argv)
        {
            use std::io::Read;
            let mut bytes = Vec::new();
            io::stdin()
                .take(tmuxx_runtime::MAX_REQUEST_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > tmuxx_runtime::MAX_REQUEST_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "stdin exceeds native request limit",
                ));
            }
            Some(bytes)
        } else {
            None
        },
        argv,
        subscribe: cli.control > 0 || cli.snapshot,
        control: cli.control > 0,
    };
    let mut output = io::BufWriter::new(&mut stream);
    if cli.control > 0 {
        serde_json::to_writer(
            &mut output,
            &tmuxx_protocol::native::ClientMessage::Control(
                tmuxx_protocol::native::ControlRequest {
                    request_id: request.request_id,
                    argv: request.argv,
                    source: None,
                    subscribe: true,
                },
            ),
        )?;
    } else {
        serde_json::to_writer(&mut output, &request)?;
    }
    output.write_all(b"\n")?;
    output.flush()?;
    drop(output);
    let (completion_tx, completion_rx) = std::sync::mpsc::sync_channel(1);
    if cli.control > 0 {
        let mut input = stream.try_clone()?;
        std::thread::spawn(move || {
            let stdin = io::stdin();
            let mut reader = stdin.lock();
            let mut id = 2;
            loop {
                match tmuxx_runtime::read_record(&mut reader, tmuxx_runtime::MAX_REQUEST_BYTES) {
                    Ok(Some(mut line)) => {
                        if line.last() == Some(&b'\n') {
                            line.pop();
                        }
                        if line.is_empty() {
                            let _ = completion_tx.try_send(());
                        }
                        let request = tmuxx_protocol::native::ClientMessage::Control(
                            tmuxx_protocol::native::ControlRequest {
                                request_id: id,
                                argv: Vec::new(),
                                source: Some(line),
                                subscribe: false,
                            },
                        );
                        id += 1;
                        let mut output = io::BufWriter::new(&mut input);
                        if serde_json::to_writer(&mut output, &request).is_err()
                            || output.write_all(b"\n").is_err()
                            || output.flush().is_err()
                        {
                            return;
                        }
                    }
                    _ => {
                        let _ = completion_tx.try_send(());
                        let _ = input.shutdown(std::net::Shutdown::Write);
                        break;
                    }
                }
            }
        });
    }
    let mut reader = BufReader::new(stream);
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut failed = false;
    let mut job_status = 0;
    let mut wait_exit = false;
    let mut deferred = std::collections::VecDeque::new();
    let mut snapshots = tmuxx_protocol::native::SnapshotAssembler::default();
    loop {
        let message = if let Some(message) = deferred.pop_front() {
            message
        } else if let Some(line) = tmuxx_runtime::read_record(&mut reader, 256 * 1024 * 1024)? {
            serde_json::from_slice(&line)?
        } else {
            snapshots.finish()?;
            // End-of-input drains the native delivery queue and then closes
            // the transport. The CLI still owns the terminal control record,
            // including when a deferred job outlives the input connection.
            if cli.control > 0 {
                writeln!(stdout, "%exit")?;
                stdout.flush()?;
            }
            break;
        };
        let Some(message) = snapshots.push(message)? else {
            continue;
        };
        match message {
            ServerMessage::File {
                request_id,
                path,
                data,
                append,
            } => {
                if !stdin_file_stream
                    || path != b"-"
                    || data.is_some()
                    || append
                    || cli.control != 0
                {
                    return Err(io::Error::other(
                        "unexpected file transfer on native endpoint",
                    ));
                }
                #[cfg(unix)]
                native_stdin(&mut reader, request_id, &mut deferred, &mut snapshots)?;
                #[cfg(windows)]
                windows::native_stdin(&mut reader, request_id, &mut deferred, &mut snapshots)?;
            }
            ServerMessage::Stdin { request_id, .. } => {
                #[cfg(unix)]
                native_stdin(&mut reader, request_id, &mut deferred, &mut snapshots)?;
                #[cfg(windows)]
                windows::native_stdin(&mut reader, request_id, &mut deferred, &mut snapshots)?;
            }
            ServerMessage::Reply(response) => {
                if cli.control == 0 {
                    job_status = response.status.unwrap_or(job_status);
                }
                let reply_failed = response.error.is_some();
                failed |= reply_failed && (cli.control == 0 || response.request_id == 1);
                if cli.control == 0 {
                    stdout.write_all(&response.output)?;
                    if let Some(error) = response.error {
                        writeln!(io::stderr().lock(), "{error}")?;
                        break;
                    }
                    if !cli.snapshot {
                        break;
                    }
                }
            }
            ServerMessage::PaneOutput { pane_id, data, .. } => {
                if cli.control > 0 {
                    stdout.write_all(&encode_output(pane_id, &data))?;
                }
            }
            ServerMessage::Control { data } => {
                if cli.control > 0 {
                    stdout.write_all(&data)?;
                    stdout.write_all(b"\n")?;
                }
            }
            ServerMessage::Snapshot { state, .. } => {
                if cli.snapshot {
                    serde_json::to_writer(&mut stdout, &state)?;
                    stdout.write_all(b"\n")?;
                    break;
                }
            }
            ServerMessage::SnapshotAborted { error, .. } => {
                return Err(io::Error::other(error));
            }
            ServerMessage::SnapshotStart { .. }
            | ServerMessage::SnapshotChunk { .. }
            | ServerMessage::SnapshotEnd { .. } => {
                unreachable!("snapshot assembler consumes fragments")
            }
            ServerMessage::Detached { .. } => {
                if cli.control > 0 {
                    writeln!(stdout, "%exit")?;
                    stdout.flush()?;
                    if wait_exit {
                        let _ = completion_rx.recv();
                    }
                }
                break;
            }
            ServerMessage::Exit { reason } => {
                if cli.control > 0 {
                    writeln!(stdout, "%exit {reason}")?;
                    stdout.flush()?;
                    if wait_exit {
                        let _ = completion_rx.recv();
                    }
                }
                break;
            }
            ServerMessage::ClientFlags { flags } => {
                wait_exit = flags.split(',').any(|flag| flag == "wait-exit");
            }
            ServerMessage::Changed { .. } | ServerMessage::HostEffect { .. } => {}
        }
        stdout.flush()?;
    }
    stdout.flush()?;
    if !failed && job_status != 0 {
        std::process::exit(job_status.clamp(1, 255));
    }
    if failed {
        std::process::exit(1);
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn native_stdin(
    reader: &mut BufReader<std::os::unix::net::UnixStream>,
    original_id: u64,
    deferred: &mut std::collections::VecDeque<ServerMessage>,
    snapshots: &mut tmuxx_protocol::native::SnapshotAssembler,
) -> io::Result<()> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    let input = io::stdin();
    let _flags = DescriptorFlags::nonblocking(input.as_raw_fd())?;
    let mut input = input.lock();
    let mut bytes = [0u8; 65536];
    let mut next_id = u64::MAX;
    let mut pending = None;
    let mut eof = false;
    loop {
        let mut poll = libc::pollfd {
            fd: reader.get_ref().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd for the live native socket.
        let ready = if reader.buffer().is_empty() {
            unsafe { libc::poll(&mut poll, 1, if pending.is_some() { 50 } else { 0 }) }
        } else {
            1
        };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready > 0 {
            let line = tmuxx_runtime::read_record(reader, 256 * 1024 * 1024)?
                .ok_or_else(|| io::Error::other("server stopped during stdin"))?;
            let Some(message) = snapshots.push(serde_json::from_slice(&line)?)? else {
                continue;
            };
            if let ServerMessage::Reply(reply) = &message
                && Some(reply.request_id) == pending
            {
                pending = None;
                if eof || reply.error.is_some() {
                    return Ok(());
                }
            } else {
                let complete = matches!(&message, ServerMessage::Reply(reply) if reply.request_id == original_id)
                    || matches!(
                        message,
                        ServerMessage::Exit { .. } | ServerMessage::Detached { .. }
                    );
                deferred.push_back(message);
                if complete {
                    return Ok(());
                }
                if deferred.len() > 256 {
                    return Err(io::Error::other("too many pending stdin messages"));
                }
            }
        }
        if pending.is_some() {
            continue;
        }
        let n = match input.read(&mut bytes) {
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(error) => return Err(error),
        };
        eof = n == 0;
        let request = Request {
            request_id: next_id,
            argv: vec![
                b"tmuxx-stdin".to_vec(),
                original_id.to_string().into_bytes(),
            ],
            stdin: (!eof).then(|| bytes[..n].to_vec()),
            subscribe: false,
            control: false,
        };
        let mut output = io::BufWriter::new(reader.get_mut());
        serde_json::to_writer(&mut output, &request)?;
        output.write_all(b"\n")?;
        output.flush()?;
        pending = Some(next_id);
        next_id -= 1;
    }
}

#[cfg(unix)]
fn run_tmux(cli: Cli, endpoint: PathBuf) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use tmuxx_unix_ipc::{Connection, Schema};
    if cli.snapshot {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--snapshot requires the native endpoint",
        ));
    }
    let stream = connect_or_start(&cli, &endpoint, cli.socket.as_ref())?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let schema = match cli.imsg_schema {
        ImsgSchema::Modern => Schema::Modern { version: 8 },
        ImsgSchema::Legacy => Schema::Legacy { version: 8 },
    };
    let connection = Connection::new(stream, schema)?;
    let argv = cli
        .command
        .iter()
        .map(|s| s.as_bytes().to_vec())
        .collect::<Vec<_>>();
    if cli.control > 0 {
        return tmux_control(connection, &argv, cli.control > 1, cli.feature_bits);
    }
    let input = io::stdin();
    let _input_flags = DescriptorFlags::nonblocking(input.as_raw_fd())?;
    let mut identity = tmuxx_unix_ipc::Identity::current(false)?;
    identity.features = cli.feature_bits;
    let result = tmuxx_unix_ipc::client::run_command_connection_with_identity(
        connection,
        &argv,
        &mut input.lock(),
        256 * 1024 * 1024,
        tmuxx_unix_ipc::FileAccess::AllowPaths,
        &identity,
    )?;
    io::stdout().write_all(&result.stdout)?;
    io::stderr().write_all(&result.stderr)?;
    if result.status != 0 {
        std::process::exit(result.status.clamp(1, 255));
    }
    Ok(())
}

#[cfg(unix)]
fn tmux_control(
    mut connection: tmuxx_unix_ipc::Connection,
    argv: &[Vec<u8>],
    double: bool,
    feature_bits: i32,
) -> io::Result<()> {
    use std::os::fd::{AsFd, AsRawFd};
    use tmuxx_unix_ipc::{Identity, flags, identify, msg, pack_argv, read_i32};
    let input = io::stdin();
    let output = io::stdout();
    let _terminal = if double {
        Some(ControlTerminal::enter(input.as_raw_fd())?)
    } else {
        None
    };
    let _input_flags = DescriptorFlags::save(input.as_raw_fd());
    let _output_flags = DescriptorFlags::save(output.as_raw_fd());
    let mut identity = Identity::current(true)?;
    identity.features = feature_bits;
    if double {
        identity.flags |= flags::CONTROL_CONTROL;
    }
    identify(
        &mut connection,
        &identity,
        Some(input.as_fd()),
        Some(output.as_fd()),
    )?;
    connection.send(msg::COMMAND, &pack_argv(argv)?, None)?;
    let mut files = tmuxx_unix_ipc::FileClient::control(tmuxx_unix_ipc::FileAccess::AllowPaths);
    let mut status = 0;
    let mut reason = Vec::new();
    let mut client_flags = identity.flags;
    loop {
        let message = files.recv(&mut connection)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "control server disappeared")
        })?;
        if message.fd.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected control descriptor",
            ));
        }
        if files.handle(&mut connection, &message, &mut io::empty())? {
            continue;
        }
        match message.kind {
            msg::FLAGS => {
                client_flags =
                    u64::from_ne_bytes(message.payload.as_slice().try_into().map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "invalid FLAGS")
                    })?);
            }
            msg::VERSION => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "tmux protocol version mismatch: server {}",
                        message.peer_id & 255
                    ),
                ));
            }
            msg::EXIT | msg::SHUTDOWN => {
                if !message.payload.is_empty() {
                    status = read_i32(&message.payload, 0)?;
                    if message.payload.len() > 4 {
                        if message.payload.last() != Some(&0) {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "invalid exit reason",
                            ));
                        }
                        reason.extend_from_slice(&message.payload[4..message.payload.len() - 1]);
                    }
                }
                if message.kind == msg::SHUTDOWN && reason.is_empty() {
                    reason.extend_from_slice(b"server exited");
                }
                break;
            }
            msg::DETACH | msg::DETACHKILL => {
                if message.payload.last() != Some(&0) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid detach session",
                    ));
                }
                connection.send(msg::EXITING, &[], None)?;
            }
            msg::EXITED => break,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("unexpected control IPC message {}", message.kind),
                ));
            }
        }
    }
    // The CLI owns the final exit record and terminal wrapper, just like tmux C.
    drop(_input_flags);
    drop(_output_flags);
    let mut output = output.lock();
    output.write_all(b"%exit")?;
    if !reason.is_empty() {
        output.write_all(b" ")?;
        output.write_all(&reason)?;
    }
    output.write_all(b"\n")?;
    if double {
        output.write_all(b"\x1b\\")?;
    }
    output.flush()?;
    if client_flags & flags::WAIT_EXIT != 0 {
        wait_control_exit(input.lock())?;
    }
    drop(_terminal);
    if status != 0 {
        std::process::exit(status.clamp(1, 255));
    }
    Ok(())
}

#[cfg(unix)]
fn wait_control_exit(mut input: impl io::BufRead) -> io::Result<()> {
    let mut line = Vec::new();
    loop {
        line.clear();
        match input.read_until(b'\n', &mut line) {
            Ok(0) => return Ok(()),
            Ok(_) if line == b"\n" => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
struct DescriptorFlags {
    fd: std::os::fd::RawFd,
    flags: i32,
}
#[cfg(unix)]
impl DescriptorFlags {
    fn nonblocking(fd: std::os::fd::RawFd) -> io::Result<Self> {
        let saved = Self::save(fd);
        // SAFETY: changes flags on the borrowed descriptor and restores them on drop.
        if saved.flags < 0
            || unsafe { libc::fcntl(fd, libc::F_SETFL, saved.flags | libc::O_NONBLOCK) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(saved)
    }
    fn save(fd: std::os::fd::RawFd) -> Self {
        // SAFETY: only reads flags on a borrowed standard descriptor.
        Self {
            fd,
            flags: unsafe { libc::fcntl(fd, libc::F_GETFL) },
        }
    }
}
#[cfg(unix)]
impl Drop for DescriptorFlags {
    fn drop(&mut self) {
        if self.flags >= 0 {
            // SAFETY: restores flags on the same borrowed descriptor.
            unsafe {
                libc::fcntl(self.fd, libc::F_SETFL, self.flags);
            }
        }
    }
}

#[cfg(unix)]
struct ControlTerminal {
    fd: std::os::fd::RawFd,
    saved: libc::termios,
}
#[cfg(unix)]
impl ControlTerminal {
    fn enter(fd: std::os::fd::RawFd) -> io::Result<Self> {
        // SAFETY: valid storage and borrowed terminal descriptor; all calls checked.
        unsafe {
            let mut saved = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut saved) != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut raw = saved;
            libc::cfmakeraw(&mut raw);
            raw.c_iflag = libc::ICRNL | libc::IXANY;
            raw.c_oflag = libc::OPOST | libc::ONLCR;
            if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { fd, saved })
        }
    }
}
#[cfg(unix)]
impl Drop for ControlTerminal {
    fn drop(&mut self) {
        // SAFETY: restores the saved terminal attributes on the borrowed descriptor.
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSAFLUSH, &self.saved);
        }
    }
}
