//! Compatibility adapter. All mutations go through the same LocalClient owner.

use super::{Event, LocalClient, MAX_REQUEST_BYTES, command_argv};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, SyncSender, TryRecvError},
};
use std::time::{Duration, Instant};
use tmuxx_protocol::encode_output;
use tmuxx_protocol::native::{Request, ServerMessage};
use tmuxx_unix_ipc::{
    Connection, HEADER_SIZE, MAX_MESSAGE_SIZE, Schema, accept_client, flags, int_pair, msg,
    read_i32,
};

pub(super) fn serve(stream: UnixStream, id: u64, tx: SyncSender<Event>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let uid = tmuxx_unix_ipc::peer_uid(&stream)?;
    let mut connection = Connection::new(stream, Schema::Auto { version: 8 })?;
    let accepted = accept_client(&mut connection)?;
    let (out_tx, rx) = mpsc::sync_channel(256);
    let delivery_bytes = Arc::new(super::AtomicUsize::new(0));
    let mut context = tmuxx_core::ClientContext {
        identity: std::collections::BTreeMap::from([
            ("client_pid".into(), accepted.identity.pid.to_string()),
            ("client_uid".into(), uid.to_string()),
            (
                "client_termname".into(),
                String::from_utf8_lossy(&accepted.identity.term).into_owned(),
            ),
            ("client_termtype".into(), String::new()),
            (
                "client_termfeatures".into(),
                tmuxx_unix_ipc::client::feature_names(accepted.identity.features),
            ),
            (
                "client_tty".into(),
                String::from_utf8_lossy(&accepted.identity.tty_name).into_owned(),
            ),
            (
                "client_utf8".into(),
                u8::from(accepted.identity.flags & flags::UTF8 != 0).to_string(),
            ),
        ]),
        ..tmuxx_core::ClientContext::default()
    };
    if uid == unsafe { libc::getuid() }
        && let Some(user) = super::defaults::user_name()
    {
        context.identity.insert("client_user".into(), user);
    }
    context.client_flags = flags::names(accepted.identity.flags);
    context.ignore_size = context
        .client_flags
        .split(',')
        .any(|flag| flag == "ignore-size");
    context.client_name = if accepted.identity.tty_name.is_empty() {
        format!("client-{}", accepted.identity.pid)
    } else {
        String::from_utf8(accepted.identity.tty_name.clone())
            .map_err(|_| invalid("client tty name must be UTF-8"))?
    };
    if !accepted.identity.cwd.is_empty() {
        context.cwd = Some(
            String::from_utf8(accepted.identity.cwd.clone())
                .map_err(|_| invalid("client cwd must be UTF-8"))?,
        );
    }
    for variable in &accepted.identity.environment {
        let variable = std::str::from_utf8(variable)
            .map_err(|_| invalid("client environment must be UTF-8"))?;
        if let Some((key, value)) = variable.split_once('=')
            && !key.is_empty()
        {
            context.environment.insert(key.to_owned(), value.to_owned());
        }
    }
    tx.send(Event::Connect {
        stdin_files: false,
        files: true,
        id,
        tx: out_tx,
        context: Box::new(context),
        delivery_bytes: delivery_bytes.clone(),
    })
    .map_err(|_| io::Error::other("server stopped"))?;
    let local = LocalClient {
        id,
        tx,
        rx,
        delivery_bytes,
    };
    let argv = if accepted.argv.is_empty() {
        vec![b"new-session".to_vec()]
    } else {
        accepted.argv
    };
    if accepted.identity.flags & flags::CONTROL != 0 {
        let input = accepted
            .stdin
            .ok_or_else(|| invalid("control mode requires stdin"))?;
        let double = accepted.identity.flags & flags::CONTROL_CONTROL != 0;
        let output = if double {
            input.try_clone()?
        } else {
            accepted
                .stdout
                .ok_or_else(|| invalid("control mode requires stdout"))?
        };
        serve_control(
            &mut connection,
            local,
            argv,
            input,
            output,
            accepted.identity.flags,
        )
    } else {
        let mut job_status = 0;
        let mut opened = [false; 2];
        local.send_wait(Request {
            request_id: 1,
            argv,
            stdin: None,
            subscribe: false,
            control: false,
        })?;
        let mut deferred = VecDeque::new();
        loop {
            match deferred.pop_front().map_or_else(|| local.recv(), Ok)? {
                ServerMessage::File {
                    request_id,
                    path,
                    data,
                    append,
                } => {
                    transfer_file(
                        &mut connection,
                        &local,
                        request_id,
                        &path,
                        data.as_deref(),
                        append,
                        accepted.identity.flags,
                    )?;
                }
                ServerMessage::Stdin { request_id, .. } => {
                    read_stdin(&mut connection, &local, request_id, &mut deferred)?;
                }
                ServerMessage::Reply(reply) => {
                    if let Some(status) = reply.status {
                        job_status = status;
                    }
                    write_stream(&mut connection, 1, &reply.output, &mut opened)?;
                    if let Some(error) = &reply.error {
                        write_stream(
                            &mut connection,
                            2,
                            format!("{error}\n").as_bytes(),
                            &mut opened,
                        )?;
                        finish_stdio(&mut connection, &opened, accepted.identity.flags)?;
                        return exit(&mut connection, 1, None);
                    }
                    break;
                }
                ServerMessage::Exit { reason } => {
                    finish_stdio(&mut connection, &opened, accepted.identity.flags)?;
                    if reason == "server exited" {
                        return connection.send(msg::SHUTDOWN, &[], None);
                    }
                    return exit(&mut connection, 1, Some(&reason));
                }
                _ => {}
            }
        }

        finish_stdio(&mut connection, &opened, accepted.identity.flags)?;
        exit(&mut connection, job_status, None)
    }
}

fn transfer_file(
    connection: &mut Connection,
    local: &LocalClient,
    request_id: u64,
    path: &[u8],
    data: Option<&[u8]>,
    append: bool,
    flags: u64,
) -> io::Result<()> {
    let result = if path == b"-" && flags & tmuxx_unix_ipc::flags::CONTROL != 0 {
        Err(io::Error::from_raw_os_error(libc::EBADF))
    } else if let Some(data) = data {
        write_file(connection, path, append, data, flags).map(|_| Vec::new())
    } else {
        read_file(connection, path)
    }
    .map_err(|error| {
        let error = error
            .raw_os_error()
            .map(|code| {
                unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) }
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| error.to_string());
        format!("{error}: {}", String::from_utf8_lossy(path))
    });
    local
        .tx
        .send(Event::FileResult {
            id: local.id,
            request_id,
            result,
        })
        .map_err(|_| io::Error::other("server stopped"))
}

fn read_file(connection: &mut Connection, path: &[u8]) -> io::Result<Vec<u8>> {
    let stream = 3i32;
    let mut payload = int_pair(stream, if path == b"-" { 0 } else { -1 });
    payload.extend_from_slice(path);
    payload.push(0);
    connection.send(msg::READ_OPEN, &payload, None)?;
    let mut input = Vec::new();
    let mut overflow_deadline = None;
    loop {
        if let Some(deadline) = overflow_deadline {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "oversized file input did not finish",
                ));
            }
            if !ready(connection.stream().as_raw_fd(), libc::POLLIN, 50)? {
                continue;
            }
        }
        let message = connection
            .recv()?
            .ok_or_else(|| invalid("client closed during file input"))?;
        if message.fd.is_some() || read_i32(&message.payload, 0)? != stream {
            return Err(invalid("invalid file input stream"));
        }
        match message.kind {
            msg::READ => {
                if overflow_deadline.is_some() {
                    continue;
                }
                if input.len().saturating_add(message.payload.len() - 4) > MAX_REQUEST_BYTES {
                    // C treats a cancel racing an already-sent READ_DONE as fatal.
                    overflow_deadline = Some(Instant::now() + Duration::from_secs(10));
                    continue;
                }
                input.extend_from_slice(&message.payload[4..]);
            }
            msg::READ_DONE if message.payload.len() == 8 => {
                if overflow_deadline.is_some() {
                    return Err(invalid("file input exceeds limit"));
                }
                let error = read_i32(&message.payload, 4)?;
                return if error == 0 {
                    Ok(input)
                } else {
                    Err(io::Error::from_raw_os_error(error))
                };
            }
            _ => return Err(invalid("unexpected file input message")),
        }
    }
}

// The command queue and its after-hooks stay blocked until READ_DONE. Only one
// bounded chunk is held here while the owner acknowledges the terminal update.
fn read_stdin(
    connection: &mut Connection,
    local: &LocalClient,
    request_id: u64,
    deferred: &mut VecDeque<ServerMessage>,
) -> io::Result<()> {
    let stream = 3i32;
    let mut payload = int_pair(stream, 0);
    payload.extend_from_slice(b"-\0");
    connection.send(msg::READ_OPEN, &payload, None)?;
    let mut cancelled = false;
    let mut cancel_deadline = None;
    let mut deferred_bytes = 0usize;
    loop {
        loop {
            let message = match local.try_recv() {
                Ok(message) => message,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) if cancelled => break,
                Err(TryRecvError::Disconnected) => ServerMessage::Exit {
                    reason: "server stopped".into(),
                },
            };
            let complete = matches!(&message, ServerMessage::Reply(reply) if reply.request_id == request_id)
                || matches!(
                    message,
                    ServerMessage::Exit { .. } | ServerMessage::Detached { .. }
                );
            if complete && !cancelled {
                connection.send(msg::READ_CANCEL, &stream.to_ne_bytes(), None)?;
                cancelled = true;
                cancel_deadline = Some(std::time::Instant::now() + Duration::from_secs(10));
            }
            deferred_bytes = deferred_bytes.saturating_add(super::control::message_bytes(&message));
            if deferred_bytes > MAX_REQUEST_BYTES || deferred.len() >= 256 {
                return Err(invalid("pending stdin messages exceed limit"));
            }
            deferred.push_back(message);
        }
        if cancel_deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "client did not acknowledge stdin cancellation",
            ));
        }
        // Stdin may legitimately remain idle indefinitely; socket timeouts only
        // bound incomplete imsg frames, not the lifetime of the input stream.
        if !ready(connection.stream().as_raw_fd(), libc::POLLIN, 50)? {
            continue;
        }
        let message = connection
            .recv()?
            .ok_or_else(|| invalid("client closed during stdin"))?;
        if message.fd.is_some() || read_i32(&message.payload, 0)? != stream {
            return Err(invalid("invalid stdin stream"));
        }
        match message.kind {
            msg::READ => {
                if !cancelled
                    && local
                        .stdin(request_id, Some(message.payload[4..].to_vec()))
                        .is_err()
                {
                    connection.send(msg::READ_CANCEL, &stream.to_ne_bytes(), None)?;
                    cancelled = true;
                    cancel_deadline = Some(std::time::Instant::now() + Duration::from_secs(10));
                }
            }
            msg::READ_DONE if message.payload.len() == 8 => {
                if !cancelled {
                    let error = read_i32(&message.payload, 4)?;
                    local.stdin_result(
                        request_id,
                        if error == 0 {
                            Ok(None)
                        } else {
                            Err(io::Error::from_raw_os_error(error).to_string())
                        },
                    )?;
                }
                return Ok(());
            }
            _ => return Err(invalid("unexpected stdin message")),
        }
    }
}

fn write_stream(
    connection: &mut Connection,
    fd: i32,
    bytes: &[u8],
    opened: &mut [bool; 2],
) -> io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    if !opened[(fd - 1) as usize] {
        open_destination(connection, fd, fd, 0, None)?;
        opened[(fd - 1) as usize] = true;
    }
    write_data(connection, fd, bytes)
}

fn finish_stdio(connection: &mut Connection, opened: &[bool; 2], flags: u64) -> io::Result<()> {
    for (index, open) in opened.iter().enumerate() {
        if *open {
            close_destination(connection, index as i32 + 1, flags)?;
        }
    }
    Ok(())
}

fn write_file(
    connection: &mut Connection,
    path: &[u8],
    append: bool,
    bytes: &[u8],
    client_flags: u64,
) -> io::Result<()> {
    write_destination(
        connection,
        4,
        -1,
        if append {
            libc::O_APPEND
        } else {
            libc::O_TRUNC
        },
        Some(path),
        bytes,
        client_flags,
    )
}

fn write_destination(
    connection: &mut Connection,
    stream: i32,
    fd: i32,
    flags: i32,
    path: Option<&[u8]>,
    bytes: &[u8],
    client_flags: u64,
) -> io::Result<()> {
    open_destination(connection, stream, fd, flags, path)?;
    write_data(connection, stream, bytes)?;
    close_destination(connection, stream, client_flags)
}

fn open_destination(
    connection: &mut Connection,
    stream: i32,
    fd: i32,
    flags: i32,
    path: Option<&[u8]>,
) -> io::Result<()> {
    let mut payload = [stream.to_ne_bytes(), fd.to_ne_bytes(), flags.to_ne_bytes()].concat();
    if let Some(path) = path {
        payload.extend_from_slice(path);
        payload.push(0);
    }
    connection.send(msg::WRITE_OPEN, &payload, None)?;
    let ready = connection
        .recv()?
        .ok_or_else(|| invalid("client closed before WRITE_READY"))?;
    if ready.kind != msg::WRITE_READY
        || ready.fd.is_some()
        || ready.payload.len() != 8
        || read_i32(&ready.payload, 0)? != stream
    {
        return Err(invalid("client refused output stream"));
    }
    let error = read_i32(&ready.payload, 4)?;
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    Ok(())
}

fn write_data(connection: &mut Connection, stream: i32, bytes: &[u8]) -> io::Result<()> {
    for bytes in bytes.chunks(MAX_MESSAGE_SIZE - HEADER_SIZE - 4) {
        connection.send(
            msg::WRITE,
            &[stream.to_ne_bytes().as_slice(), bytes].concat(),
            None,
        )?;
    }
    Ok(())
}

fn close_destination(
    connection: &mut Connection,
    stream: i32,
    client_flags: u64,
) -> io::Result<()> {
    if client_flags & flags::WRITE_ACK != 0 {
        connection.send(msg::WRITE_CLOSE, &stream.to_ne_bytes(), None)?;
        let done = connection
            .recv()?
            .ok_or_else(|| invalid("client closed before WRITE_DONE"))?;
        if done.kind != msg::WRITE_DONE || done.fd.is_some() || done.payload != int_pair(stream, 0)
        {
            return Err(invalid("client failed output stream"));
        }
    }
    Ok(())
}

fn exit(connection: &mut Connection, status: i32, reason: Option<&str>) -> io::Result<()> {
    let mut data = status.to_ne_bytes().to_vec();
    if let Some(reason) = reason {
        data.extend_from_slice(reason.as_bytes());
        data.push(0);
    }
    connection.send(msg::EXIT, &data, None)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn ready(fd: RawFd, events: i16, timeout: i32) -> io::Result<bool> {
    let mut poll = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    loop {
        // SAFETY: the single pollfd is initialized and remains valid for this call.
        let n = unsafe { libc::poll(&mut poll, 1, timeout) };
        if n >= 0 {
            return Ok(n > 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

struct Output(File);
impl Output {
    fn new(fd: OwnedFd) -> io::Result<Self> {
        // Like C control_start, use nonblocking writes on the passed descriptor.
        let raw = fd.as_raw_fd();
        // SAFETY: fcntl only reads and changes flags on this live descriptor.
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(File::from(fd)))
    }
    fn write(&mut self, mut data: &[u8]) -> io::Result<()> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !data.is_empty() {
            match self.0.write(data) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "control output closed",
                    ));
                }
                Ok(n) => data = &data[n..],
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "control client too far behind",
                        ));
                    }
                    ready(self.0.as_raw_fd(), libc::POLLOUT, 50)?;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

// A control input line is one command group. Its error cancels only that group.
fn serve_control(
    connection: &mut Connection,
    local: LocalClient,
    argv: Vec<Vec<u8>>,
    input: OwnedFd,
    output: OwnedFd,
    mut client_flags: u64,
) -> io::Result<()> {
    let double = client_flags & flags::CONTROL_CONTROL != 0;
    let mut output = Output::new(output)?;
    if double {
        output.write(b"\x1bP1000p")?;
    }
    let (input_tx, input_rx) = mpsc::sync_channel(128);
    let stop = Arc::new(AtomicBool::new(false));
    let input_stop = stop.clone();
    let input_thread = std::thread::spawn(move || {
        let result = read_lines(File::from(input), &input_stop, &input_tx);
        if !input_stop.load(Ordering::Acquire) {
            let _ = input_tx.try_send(result.map(|_| None));
        }
    });
    let mut detached = None;
    let result = (|| {
        let mut number = 1u64;
        let mut outstanding = true;
        let mut group_flags = 0;
        let mut status = 0;
        local.send_control_wait(
            Request {
                request_id: number,
                argv,
                stdin: None,
                subscribe: true,
                control: true,
            },
            None,
            group_flags,
        )?;
        loop {
            if !outstanding {
                match input_rx.try_recv() {
                    Ok(Ok(Some(line))) => {
                        if line.is_empty() {
                            return Ok(Some((status, None)));
                        }
                        group_flags = 1;
                        let commands = local.prepare_control_line(line)?;
                        number += 1;
                        let argv = commands
                            .as_ref()
                            .map_or_else(|_| Vec::new(), |commands| command_argv(commands));
                        local.send_control_wait(
                            Request {
                                request_id: number,
                                argv,
                                stdin: None,
                                subscribe: false,
                                control: true,
                            },
                            Some(commands),
                            group_flags,
                        )?;
                        outstanding = true;
                    }
                    Ok(Ok(None)) | Err(TryRecvError::Disconnected) => {
                        return Ok(Some((status, None)));
                    }
                    Ok(Err(error)) => return Err(error),
                    Err(TryRecvError::Empty) => {}
                }
            }
            match local.try_recv() {
                Ok(ServerMessage::ClientFlags { flags: names }) => {
                    client_flags = flags::apply(client_flags, &names);
                    connection.send(msg::FLAGS, &client_flags.to_ne_bytes(), None)?;
                }
                Ok(ServerMessage::File {
                    request_id,
                    path,
                    data,
                    append,
                }) => {
                    transfer_file(
                        connection,
                        &local,
                        request_id,
                        &path,
                        data.as_deref(),
                        append,
                        client_flags,
                    )?;
                }
                Ok(ServerMessage::Stdin { request_id, .. }) => {
                    local.stdin_result(
                        request_id,
                        Err("control client cannot supply stdin".into()),
                    )?;
                }
                Ok(ServerMessage::Reply(reply)) => {
                    let failed = reply.error.is_some();
                    outstanding = false;
                    if failed && group_flags == 0 {
                        status = 1;
                        return Ok(Some((status, None)));
                    }
                }
                Ok(ServerMessage::PaneOutput { pane_id, data, .. }) => {
                    output.write(&encode_output(pane_id, &data))?
                }
                Ok(ServerMessage::Control { data }) => {
                    output.write(&data)?;
                    output.write(b"\n")?;
                }
                Ok(ServerMessage::Detached { session, hangup }) => {
                    detached = Some((session, hangup));
                    return Ok(None);
                }
                Ok(ServerMessage::Exit { reason }) => {
                    return Ok(Some((status, Some(reason))));
                }
                Ok(
                    ServerMessage::Snapshot { .. }
                    | ServerMessage::SnapshotStart { .. }
                    | ServerMessage::SnapshotChunk { .. }
                    | ServerMessage::SnapshotEnd { .. }
                    | ServerMessage::SnapshotAborted { .. }
                    | ServerMessage::Changed { .. }
                    | ServerMessage::HostEffect { .. },
                ) => {}
                Err(TryRecvError::Disconnected) => {
                    return Ok(Some((status, Some("server exited".into()))));
                }
                Err(TryRecvError::Empty) => {}
            }
            if ready(connection.stream().as_raw_fd(), libc::POLLIN, 0)? {
                let Some(message) = connection.recv()? else {
                    return Ok(None);
                };
                if message.fd.is_some() {
                    return Err(invalid("unexpected control descriptor"));
                }
                match message.kind {
                    msg::EXITING => {
                        connection.send(msg::EXITED, &[], None)?;
                        return Ok(None);
                    }
                    msg::RESIZE | msg::WAKEUP => {}
                    _ => return Err(invalid("unexpected control client message")),
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    })();
    stop.store(true, Ordering::Release);
    drop(input_rx);
    let _ = input_thread.join();
    // Release the shared stdin descriptor before the client waits for its exit acknowledgement.
    if let Some((session, hangup)) = detached {
        result?;
        let mut payload = session.into_bytes();
        payload.push(0);
        connection.send(
            if hangup { msg::DETACHKILL } else { msg::DETACH },
            &payload,
            None,
        )?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if !ready(connection.stream().as_raw_fd(), libc::POLLIN, 50)? {
                continue;
            }
            let Some(message) = connection.recv()? else {
                return Ok(());
            };
            if message.fd.is_some() {
                return Err(invalid("unexpected detach descriptor"));
            }
            if message.kind == msg::EXITING {
                return connection.send(msg::EXITED, &[], None);
            }
            if !matches!(message.kind, msg::RESIZE | msg::WAKEUP) {
                return Err(invalid("unexpected detach acknowledgement"));
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "detach acknowledgement timed out",
        ))
    } else if let Some((status, reason)) = result? {
        exit(connection, status, reason.as_deref())
    } else {
        Ok(())
    }
}

fn read_lines(
    mut input: File,
    stop: &AtomicBool,
    tx: &SyncSender<io::Result<Option<Vec<u8>>>>,
) -> io::Result<()> {
    struct RestoreFlags {
        fd: RawFd,
        flags: i32,
    }
    impl Drop for RestoreFlags {
        fn drop(&mut self) {
            // SAFETY: input remains alive until this guard has been dropped.
            unsafe {
                libc::fcntl(self.fd, libc::F_SETFL, self.flags);
            }
        }
    }
    // SAFETY: input owns a live descriptor; F_GETFL/F_SETFL take integer flags.
    let flags = unsafe { libc::fcntl(input.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(input.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let _restore = RestoreFlags {
        fd: input.as_raw_fd(),
        flags,
    };
    let mut line = Vec::new();
    let mut bytes = [0; 8192];
    while !stop.load(Ordering::Acquire) {
        // Darwin poll misses EOF for a FIFO whose writer sent no data.
        ready(input.as_raw_fd(), libc::POLLIN, 50)?;
        let n = match input.read(&mut bytes) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e),
        };
        for &byte in &bytes[..n] {
            if byte == b'\n' {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.is_empty() {
                    return Ok(());
                }
                if tx.send(Ok(Some(std::mem::take(&mut line)))).is_err() {
                    return Ok(());
                }
            } else {
                if line.len() >= MAX_REQUEST_BYTES {
                    return Err(invalid("control input exceeds limit"));
                }
                line.push(byte);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::OpenOptionsExt;

    #[test]
    fn silent_fifo_eof_closes_control_reader_and_restores_shared_flags() {
        let path = std::env::temp_dir().join(format!("tmuxx-fifo-{}", std::process::id()));
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: name is a valid terminated path; only this test owns it.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = Cleanup(path.clone());
        let input = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .unwrap();
        let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let shared = input.try_clone().unwrap();
        // Start with blocking flags, as an ordinary client would provide.
        assert_eq!(
            unsafe { libc::fcntl(shared.as_raw_fd(), libc::F_SETFL, 0) },
            0
        );
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            done_tx.send(read_lines(input, &stopped, &tx)).unwrap();
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        drop(writer);
        let result = done_rx.recv_timeout(Duration::from_secs(2));
        stop.store(true, Ordering::Release);
        thread.join().unwrap();
        result
            .expect("control EOF must be observed without POLLIN")
            .unwrap();
        assert!(rx.try_recv().is_err());
        assert_eq!(
            unsafe { libc::fcntl(shared.as_raw_fd(), libc::F_GETFL) } & libc::O_NONBLOCK,
            0
        );
    }
}
