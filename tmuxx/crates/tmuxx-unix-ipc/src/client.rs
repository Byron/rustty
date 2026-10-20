//! A command client plus identification helpers for control-stream bridges.

use crate::{
    Connection, FileAccess, FileClient, HEADER_SIZE, MAX_MESSAGE_SIZE, Schema, flags, invalid,
    invalid_input, msg, pack_argv, read_i32,
};
use std::io::{self, Read};
use std::os::fd::BorrowedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Identity {
    pub flags: u64,
    pub features: i32,
    pub term: Vec<u8>,
    pub tty_name: Vec<u8>,
    pub cwd: Vec<u8>,
    pub pid: i32,
    pub environment: Vec<Vec<u8>>,
}

impl Identity {
    /// Explicitly capture this process's environment for the remote Unix peer.
    pub fn current(control: bool) -> io::Result<Self> {
        Ok(Self {
            features: 0,
            flags: flags::UTF8 | flags::WRITE_ACK | if control { flags::CONTROL } else { 0 },
            term: std::env::var_os("TERM")
                .map(|v| v.as_os_str().as_bytes().to_vec())
                .unwrap_or_default(),
            tty_name: Vec::new(),
            cwd: std::env::current_dir()?.as_os_str().as_bytes().to_vec(),
            pid: std::process::id() as i32,
            environment: std::env::vars_os()
                .map(|(key, value)| {
                    let mut bytes = key.as_os_str().as_bytes().to_vec();
                    bytes.push(b'=');
                    bytes.extend_from_slice(value.as_os_str().as_bytes());
                    bytes
                })
                .collect(),
        })
    }
}

pub use tmuxx_protocol::features::{apply_features, feature_names};

pub fn identify(
    connection: &mut Connection,
    identity: &Identity,
    stdin: Option<BorrowedFd<'_>>,
    stdout: Option<BorrowedFd<'_>>,
) -> io::Result<()> {
    connection.send(msg::IDENTIFY_LONGFLAGS, &identity.flags.to_ne_bytes(), None)?;
    send_string(connection, msg::IDENTIFY_TERM, &identity.term)?;
    connection.send(
        msg::IDENTIFY_FEATURES,
        &identity.features.to_ne_bytes(),
        None,
    )?;
    send_string(connection, msg::IDENTIFY_TTYNAME, &identity.tty_name)?;
    send_string(connection, msg::IDENTIFY_CWD, &identity.cwd)?;
    connection.send(msg::IDENTIFY_STDIN, &[], stdin)?;
    connection.send(msg::IDENTIFY_STDOUT, &[], stdout)?;
    connection.send(msg::IDENTIFY_CLIENTPID, &identity.pid.to_ne_bytes(), None)?;
    for value in &identity.environment {
        if value.len() < MAX_MESSAGE_SIZE - HEADER_SIZE {
            send_string(connection, msg::IDENTIFY_ENVIRON, value)?;
        }
    }
    connection.send(msg::IDENTIFY_DONE, &[], None)
}

fn send_string(connection: &mut Connection, kind: u32, bytes: &[u8]) -> io::Result<()> {
    if bytes.contains(&0) {
        return Err(invalid_input("NUL in identify string"));
    }
    let mut terminated = bytes.to_vec();
    terminated.push(0);
    connection.send(kind, &terminated, None)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommandResult {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: i32,
    pub exit_reason: Option<Vec<u8>>,
}

/// Run a command on an already-existing C/tmuxx socket; never auto-start a server.
///
/// Only stdin/stdout/stderr are served. Requests to open client filesystem paths
/// receive EACCES. Callers needing those operations can use the low-level codec.
/// The timeout covers each blocking socket operation; output_limit bounds the
/// combined collected stdout/stderr. Neither is a tmux protocol restriction.
pub fn run_command(
    path: &Path,
    schema: Schema,
    argv: &[Vec<u8>],
    input: &[u8],
    timeout: Duration,
    output_limit: usize,
) -> io::Result<CommandResult> {
    run_command_with_input(path, schema, argv, &mut &*input, timeout, output_limit)
}

/// As `run_command`, but read stdin only if the server requests it.
/// Supply a nonblocking reader for live stdin so server cancellation can be
/// observed while input is idle. Finite in-memory readers need no special setup.
pub fn run_command_with_input(
    path: &Path,
    schema: Schema,
    argv: &[Vec<u8>],
    input: &mut impl Read,
    timeout: Duration,
    output_limit: usize,
) -> io::Result<CommandResult> {
    if matches!(schema, Schema::Auto { .. }) {
        return Err(invalid_input(
            "command clients must select an explicit imsg schema",
        ));
    }
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    run_command_connection(Connection::new(stream, schema)?, argv, input, output_limit)
}

/// Run a command over a caller-established, explicitly selected connection.
pub fn run_command_connection(
    connection: Connection,
    argv: &[Vec<u8>],
    input: &mut impl Read,
    output_limit: usize,
) -> io::Result<CommandResult> {
    run_command_connection_with_files(connection, argv, input, output_limit, FileAccess::StdioOnly)
}

/// A command client whose file-open policy is chosen explicitly by its caller.
pub fn run_command_connection_with_files(
    connection: Connection,
    argv: &[Vec<u8>],
    input: &mut impl Read,
    output_limit: usize,
    access: FileAccess,
) -> io::Result<CommandResult> {
    run_command_connection_with_identity(
        connection,
        argv,
        input,
        output_limit,
        access,
        &Identity::current(false)?,
    )
}

/// Like the file-policy client, with caller-selected terminal identification.
pub fn run_command_connection_with_identity(
    mut connection: Connection,
    argv: &[Vec<u8>],
    input: &mut impl Read,
    output_limit: usize,
    access: FileAccess,
    identity: &Identity,
) -> io::Result<CommandResult> {
    let schema = connection.schema();
    if matches!(schema, Schema::Auto { .. }) {
        return Err(invalid_input(
            "command clients must select an explicit imsg schema",
        ));
    }
    let payload = pack_argv(argv)?;
    identify(&mut connection, identity, None, None)?;
    connection.send(msg::COMMAND, &payload, None)?;
    let mut result = CommandResult::default();
    let mut files = FileClient::command(access, output_limit);
    loop {
        let message = files.recv(&mut connection)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "tmux disconnected without command exit",
            )
        })?;
        if message.kind == msg::VERSION {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "tmux protocol version mismatch: client {}, server {}",
                    schema.version(),
                    message.peer_id & 255
                ),
            ));
        }
        if message.peer_id & 255 != u32::from(schema.version()) {
            return Err(invalid("unexpected tmux protocol version"));
        }
        if message.fd.is_some() {
            return Err(invalid("unexpected descriptor in command reply"));
        }
        if files.handle(&mut connection, &message, input)? {
            continue;
        }
        let data = &message.payload;
        match message.kind {
            msg::FLAGS => {
                if data.len() != 8 {
                    return Err(invalid("invalid FLAGS"));
                }
            }
            msg::EXIT | msg::SHUTDOWN => {
                if !data.is_empty() {
                    result.status = read_i32(data, 0)?;
                    if data.len() > 4 {
                        if data.last() != Some(&0) {
                            return Err(invalid("unterminated exit reason"));
                        }
                        result.exit_reason = Some(data[4..data.len() - 1].to_vec());
                    }
                }
                result.stdout = files.stdout;
                result.stderr = files.stderr;
                return Ok(result);
            }
            msg::READY => {
                connection.send(msg::EXITING, &[], None)?;
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "ordinary terminal attachment is unsupported",
                ));
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("unsupported tmux command message {}", message.kind),
                ));
            }
        }
    }
}
