//! Client file messages. Path access is an explicit caller policy.
use crate::{Connection, HEADER_SIZE, MAX_MESSAGE_SIZE, Message, int_pair, invalid, msg, read_i32};
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileAccess {
    StdioOnly,
    AllowPaths,
}

enum Target {
    Stdout,
    Stderr,
    File(File),
}
struct Stream {
    target: Target,
    error: i32,
}

pub struct FileClient {
    streams: BTreeMap<i32, Stream>,
    access: FileAccess,
    allow_streams: bool,
    stdin_available: bool,
    limit: usize,
    pending: VecDeque<Message>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
impl FileClient {
    pub fn command(access: FileAccess, output_limit: usize) -> Self {
        Self {
            streams: BTreeMap::new(),
            access,
            allow_streams: true,
            stdin_available: true,
            limit: output_limit,
            pending: VecDeque::new(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }
    pub fn control(access: FileAccess) -> Self {
        Self {
            allow_streams: false,
            ..Self::command(access, 0)
        }
    }
    /// Receive the next message, including messages received during stdin input.
    pub fn recv(&mut self, connection: &mut Connection) -> io::Result<Option<Message>> {
        self.pending
            .pop_front()
            .map_or_else(|| connection.recv(), |message| Ok(Some(message)))
    }
    /// Serve one file message, returning false for messages outside this family.
    pub fn handle(
        &mut self,
        connection: &mut Connection,
        message: &Message,
        input: &mut impl Read,
    ) -> io::Result<bool> {
        if !(msg::READ_OPEN..=msg::WRITE_DONE).contains(&message.kind) {
            return Ok(false);
        }
        if message.fd.is_some() {
            return Err(invalid("unexpected descriptor in file message"));
        }
        let data = &message.payload;
        let stream = read_i32(data, 0)?;
        match message.kind {
            msg::WRITE_OPEN => {
                if data.len() < 12 {
                    return Err(invalid("short WRITE_OPEN"));
                }
                let fd = read_i32(data, 4)?;
                let flags = read_i32(data, 8)?;
                let error = if self.streams.contains_key(&stream) {
                    libc::EBADF
                } else {
                    let target = match fd {
                        1 if self.allow_streams => Ok(Target::Stdout),
                        2 if self.allow_streams => Ok(Target::Stderr),
                        -1 if self.access == FileAccess::AllowPaths => open(
                            &data[12..],
                            flags | libc::O_WRONLY | libc::O_CREAT | libc::O_NONBLOCK,
                        )
                        .map(Target::File),
                        _ => Err(io::Error::from_raw_os_error(libc::EACCES)),
                    };
                    match target {
                        Ok(target) => {
                            self.streams.insert(stream, Stream { target, error: 0 });
                            0
                        }
                        Err(error) => error.raw_os_error().unwrap_or(libc::EINVAL),
                    }
                };
                connection.send(msg::WRITE_READY, &int_pair(stream, error), None)?;
            }
            msg::WRITE => {
                let target = self
                    .streams
                    .get_mut(&stream)
                    .ok_or_else(|| invalid("WRITE to unknown stream"))?;
                let bytes = &data[4..];
                match &mut target.target {
                    Target::File(file) => {
                        if target.error == 0
                            && let Err(error) = file.write_all(bytes)
                        {
                            target.error = error.raw_os_error().unwrap_or(libc::EIO);
                        }
                    }
                    target => {
                        if self
                            .stdout
                            .len()
                            .saturating_add(self.stderr.len())
                            .saturating_add(bytes.len())
                            > self.limit
                        {
                            return Err(io::Error::new(
                                io::ErrorKind::OutOfMemory,
                                "command output limit exceeded",
                            ));
                        }
                        if matches!(target, Target::Stdout) {
                            self.stdout.extend_from_slice(bytes);
                        } else {
                            self.stderr.extend_from_slice(bytes);
                        }
                    }
                }
            }
            msg::WRITE_CLOSE => {
                if data.len() != 4 {
                    return Err(invalid("invalid WRITE_CLOSE"));
                }
                let error = self
                    .streams
                    .remove(&stream)
                    .map_or(libc::EBADF, |stream| stream.error);
                connection.send(msg::WRITE_DONE, &int_pair(stream, error), None)?;
            }
            msg::READ_OPEN => {
                if data.len() < 8 {
                    return Err(invalid("short READ_OPEN"));
                }
                let fd = read_i32(data, 4)?;
                let result = if fd == 0 && self.allow_streams && self.stdin_available {
                    self.stdin_available = false;
                    send_input(connection, stream, input, &mut self.pending)
                } else if fd != -1 {
                    Err(io::Error::from_raw_os_error(libc::EBADF))
                } else if fd == -1 && self.access == FileAccess::AllowPaths {
                    open(&data[8..], libc::O_RDONLY | libc::O_NONBLOCK).and_then(|mut file| {
                        send_input(connection, stream, &mut file, &mut self.pending)
                    })
                } else {
                    Err(io::Error::from_raw_os_error(libc::EACCES))
                };
                connection.send(
                    msg::READ_DONE,
                    &int_pair(
                        stream,
                        result
                            .err()
                            .map_or(0, |e| e.raw_os_error().unwrap_or(libc::EIO)),
                    ),
                    None,
                )?;
            }
            msg::READ_CANCEL => {
                if data.len() != 4 {
                    return Err(invalid("invalid READ_CANCEL"));
                }
            }
            _ => return Err(invalid("unexpected client-side file message")),
        }
        Ok(true)
    }
}

fn send_input(
    connection: &mut Connection,
    stream: i32,
    input: &mut impl Read,
    pending: &mut VecDeque<Message>,
) -> io::Result<()> {
    let mut bytes = [0; MAX_MESSAGE_SIZE - HEADER_SIZE - 4];
    let mut pending_bytes = pending
        .iter()
        .map(|message| HEADER_SIZE + message.payload.len())
        .sum::<usize>();
    let mut timeout = 0;
    loop {
        let mut poll = libc::pollfd {
            fd: connection.stream().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd for a live, borrowed socket.
        let ready = unsafe { libc::poll(&mut poll, 1, timeout) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready > 0 {
            let message = connection
                .recv()?
                .ok_or_else(|| invalid("server closed during input"))?;
            if message.kind == msg::READ_CANCEL {
                if message.fd.is_some() || message.payload != stream.to_ne_bytes() {
                    return Err(invalid("invalid READ_CANCEL"));
                }
                return Ok(());
            }
            let stopped = matches!(message.kind, msg::EXIT | msg::SHUTDOWN);
            pending_bytes = pending_bytes.saturating_add(HEADER_SIZE + message.payload.len());
            if pending_bytes > 16 * 1024 * 1024 {
                return Err(invalid("pending input messages exceed limit"));
            }
            pending.push_back(message);
            if stopped {
                return Ok(());
            }
        }
        timeout = 0;
        let n = match input.read(&mut bytes) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                timeout = 50;
                continue;
            }
            Err(e) => return Err(e),
        };
        connection.send(
            msg::READ,
            &[stream.to_ne_bytes().as_slice(), &bytes[..n]].concat(),
            None,
        )?;
    }
}
fn open(path: &[u8], flags: i32) -> io::Result<File> {
    let path = std::ffi::CStr::from_bytes_with_nul(path)
        .map_err(|_| invalid("invalid client file path"))?;
    // SAFETY: a validated NUL-terminated path and native flags; new descriptor is owned.
    let fd = unsafe { libc::open(path.as_ptr(), flags | libc::O_CLOEXEC, 0o644) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
