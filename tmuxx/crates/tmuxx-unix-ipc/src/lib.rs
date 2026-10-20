//! tmux's local Unix protocol, implemented without linking tmux C code.
//!
//! This is a local, native-endian protocol with descriptor passing, not an SSH
//! protocol. Schema selection and tmux protocol-version support are independent.
//! Connections use blocking I/O; set timeouts before supplying a stream.

#![cfg(unix)]

use std::collections::VecDeque;
use std::io;
use std::mem;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;

pub mod client;
mod files;
pub mod server;
pub use client::{
    CommandResult, Identity, identify, run_command, run_command_connection,
    run_command_connection_with_files, run_command_with_input,
};
pub use files::{FileAccess, FileClient};
pub use server::{AcceptedClient, accept_client};

pub const HEADER_SIZE: usize = 16;
pub const MAX_MESSAGE_SIZE: usize = 16_384;
const FD_MARK: u32 = 0x8000_0000;
const MAX_PENDING_FDS: usize = 16;

pub mod msg {
    pub const VERSION: u32 = 12;
    pub const IDENTIFY_FLAGS: u32 = 100;
    pub const IDENTIFY_TERM: u32 = 101;
    pub const IDENTIFY_TTYNAME: u32 = 102;
    pub const IDENTIFY_OLDCWD: u32 = 103;
    pub const IDENTIFY_STDIN: u32 = 104;
    pub const IDENTIFY_ENVIRON: u32 = 105;
    pub const IDENTIFY_DONE: u32 = 106;
    pub const IDENTIFY_CLIENTPID: u32 = 107;
    pub const IDENTIFY_CWD: u32 = 108;
    pub const IDENTIFY_FEATURES: u32 = 109;
    pub const IDENTIFY_STDOUT: u32 = 110;
    pub const IDENTIFY_LONGFLAGS: u32 = 111;
    pub const IDENTIFY_TERMINFO: u32 = 112;
    pub const COMMAND: u32 = 200;
    pub const DETACH: u32 = 201;
    pub const DETACHKILL: u32 = 202;
    pub const EXIT: u32 = 203;
    pub const EXITED: u32 = 204;
    pub const EXITING: u32 = 205;
    pub const LOCK: u32 = 206;
    pub const READY: u32 = 207;
    pub const RESIZE: u32 = 208;
    pub const SHELL: u32 = 209;
    pub const SHUTDOWN: u32 = 210;
    pub const OLDSTDERR: u32 = 211;
    pub const OLDSTDIN: u32 = 212;
    pub const OLDSTDOUT: u32 = 213;
    pub const SUSPEND: u32 = 214;
    pub const UNLOCK: u32 = 215;
    pub const WAKEUP: u32 = 216;
    pub const EXEC: u32 = 217;
    pub const FLAGS: u32 = 218;
    pub const READ_OPEN: u32 = 300;
    pub const READ: u32 = 301;
    pub const READ_DONE: u32 = 302;
    pub const WRITE_OPEN: u32 = 303;
    pub const WRITE: u32 = 304;
    pub const WRITE_READY: u32 = 305;
    pub const WRITE_CLOSE: u32 = 306;
    pub const READ_CANCEL: u32 = 307;
    pub const WRITE_DONE: u32 = 308;
}

pub mod flags {
    pub const CONTROL: u64 = 0x2000;
    pub const CONTROL_CONTROL: u64 = 0x4000;
    pub const UTF8: u64 = 0x10000;
    pub const WRITE_ACK: u64 = 0x4000000000;
    pub const WAIT_EXIT: u64 = 0x200000000;
    const MUTABLE: &[(&str, u64)] = &[
        ("read-only", 0x800),
        ("ignore-size", 0x20000),
        ("no-output", 0x4000000),
        ("pause-after", 0x100000000),
        ("wait-exit", WAIT_EXIT),
        ("new-layouts", 0x800000000),
        ("no-detach-on-destroy", 0x8000000000),
    ];

    /// Replace named mutable flags without losing transport and identity bits.
    pub fn apply(current: u64, names: &str) -> u64 {
        let mut value = current;
        for &(name, bit) in MUTABLE {
            value &= !bit;
            if names
                .split(',')
                .any(|flag| flag.split('=').next() == Some(name))
            {
                value |= bit;
            }
        }
        value
    }

    /// Initial flags supplied during identification, before any server updates.
    pub fn names(value: u64) -> String {
        MUTABLE
            .iter()
            .filter_map(|&(name, bit)| (value & bit != 0).then_some(name))
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Schema {
    /// Current imsg: 32-bit length, high length bit marks an FD.
    Modern { version: u8 },
    /// Older imsg: 16-bit length followed by 16-bit flags (bit 0 marks an FD).
    Legacy { version: u8 },
    /// Server receive-side detection. An FD cannot be sent until resolved.
    Auto { version: u8 },
}

impl Schema {
    pub fn version(self) -> u8 {
        match self {
            Self::Modern { version } | Self::Legacy { version } | Self::Auto { version } => version,
        }
    }
}

#[derive(Debug)]
pub struct Message {
    pub kind: u32,
    pub peer_id: u32,
    pub pid: u32,
    pub payload: Vec<u8>,
    pub fd: Option<OwnedFd>,
}

pub struct Connection {
    stream: UnixStream,
    schema: Schema,
    fds: VecDeque<OwnedFd>,
    failed: bool,
}

impl Connection {
    pub fn new(stream: UnixStream, schema: Schema) -> io::Result<Self> {
        // Suppress SIGPIPE per socket where sendmsg lacks MSG_NOSIGNAL.
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
        {
            let enabled: libc::c_int = 1;
            // SAFETY: valid socket and pointer to an initialized integer.
            if unsafe {
                libc::setsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_NOSIGPIPE,
                    (&enabled as *const libc::c_int).cast(),
                    mem::size_of_val(&enabled) as libc::socklen_t,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self {
            stream,
            schema,
            fds: VecDeque::new(),
            failed: false,
        })
    }

    pub fn schema(&self) -> Schema {
        self.schema
    }
    pub fn stream(&self) -> &UnixStream {
        &self.stream
    }

    /// Send one complete message. The caller retains ownership of a supplied FD.
    pub fn send(
        &mut self,
        kind: u32,
        payload: &[u8],
        fd: Option<BorrowedFd<'_>>,
    ) -> io::Result<()> {
        if self.failed {
            return Err(invalid("imsg connection failed"));
        }
        if payload.len() > MAX_MESSAGE_SIZE - HEADER_SIZE {
            return Err(invalid_input("imsg payload too large"));
        }
        if fd.is_some() && matches!(self.schema, Schema::Auto { .. }) {
            return Err(invalid_input("imsg schema unresolved"));
        }
        let header = encode_header(self.schema, kind, payload.len(), fd.is_some())?;
        let mut bytes = Vec::with_capacity(HEADER_SIZE + payload.len());
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(payload);
        let mut offset = 0;
        while offset < bytes.len() {
            let mut vector = libc::iovec {
                iov_base: bytes[offset..].as_mut_ptr().cast(),
                iov_len: bytes.len() - offset,
            };
            // usize storage gives the control buffer sufficient cmsghdr alignment.
            let mut ancillary = [0_usize; 8];
            // SAFETY: a zeroed msghdr has no invalid scalar representations.
            let mut message: libc::msghdr = unsafe { mem::zeroed() };
            message.msg_iov = &mut vector;
            message.msg_iovlen = 1;
            if let Some(fd) = fd.filter(|_| offset == 0) {
                message.msg_control = ancillary.as_mut_ptr().cast();
                // SAFETY: lengths fit the aligned ancillary buffer above.
                unsafe {
                    message.msg_controllen =
                        libc::CMSG_SPACE(mem::size_of::<libc::c_int>() as _) as _;
                    let cmsg = libc::CMSG_FIRSTHDR(&message);
                    (*cmsg).cmsg_level = libc::SOL_SOCKET;
                    (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                    (*cmsg).cmsg_len = libc::CMSG_LEN(mem::size_of::<libc::c_int>() as _) as _;
                    std::ptr::write_unaligned(
                        libc::CMSG_DATA(cmsg).cast::<libc::c_int>(),
                        fd.as_raw_fd(),
                    );
                }
            }
            #[cfg(any(target_os = "linux", target_os = "android"))]
            let send_flags = libc::MSG_NOSIGNAL;
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            let send_flags = 0;
            // SAFETY: message references live buffers for this synchronous call.
            let sent = unsafe { libc::sendmsg(self.stream.as_raw_fd(), &message, send_flags) };
            if sent < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                self.failed = true;
                return Err(error);
            }
            if sent == 0 {
                self.failed = true;
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "imsg socket wrote zero bytes",
                ));
            }
            offset += sent as usize;
        }
        Ok(())
    }

    /// Read one message, retaining ancillary FDs until their marked frame arrives.
    pub fn recv(&mut self) -> io::Result<Option<Message>> {
        if self.failed {
            return Err(invalid("imsg connection failed"));
        }
        let result = self.recv_inner();
        if result.is_err() {
            self.failed = true;
            self.fds.clear();
        }
        result
    }

    fn recv_inner(&mut self) -> io::Result<Option<Message>> {
        let mut header = [0_u8; HEADER_SIZE];
        if !self.read_exact_message(&mut header, true)? {
            if !self.fds.is_empty() {
                return Err(invalid("unclaimed descriptor at imsg EOF"));
            }
            return Ok(None);
        }
        let (length, has_fd) = decode_length(&header, &mut self.schema)?;
        let mut payload = vec![0; length - HEADER_SIZE];
        self.read_exact_message(&mut payload, false)?;
        let fd = if has_fd {
            Some(
                self.fds
                    .pop_front()
                    .ok_or_else(|| invalid("imsg FD marker without descriptor"))?,
            )
        } else {
            None
        };
        Ok(Some(Message {
            kind: u32::from_ne_bytes(header[0..4].try_into().unwrap()),
            peer_id: u32::from_ne_bytes(header[8..12].try_into().unwrap()),
            pid: u32::from_ne_bytes(header[12..16].try_into().unwrap()),
            payload,
            fd,
        }))
    }

    fn read_exact_message(&mut self, bytes: &mut [u8], allow_eof: bool) -> io::Result<bool> {
        let mut offset = 0;
        while offset < bytes.len() {
            let mut vector = libc::iovec {
                iov_base: bytes[offset..].as_mut_ptr().cast(),
                iov_len: bytes.len() - offset,
            };
            let mut ancillary = [0_usize; 64];
            // SAFETY: a zeroed msghdr is initialized below before use.
            let mut message: libc::msghdr = unsafe { mem::zeroed() };
            message.msg_iov = &mut vector;
            message.msg_iovlen = 1;
            message.msg_control = ancillary.as_mut_ptr().cast();
            message.msg_controllen = mem::size_of_val(&ancillary) as _;
            #[cfg(any(target_os = "linux", target_os = "android"))]
            let recv_flags = libc::MSG_CMSG_CLOEXEC;
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            let recv_flags = 0;
            // SAFETY: message references writable live buffers for this call.
            let received =
                unsafe { libc::recvmsg(self.stream.as_raw_fd(), &mut message, recv_flags) };
            if received < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            // Own every received descriptor before checking errors, including truncation.
            let mut received_fds = Vec::new();
            // SAFETY: libc CMSG helpers traverse kernel-returned ancillary metadata.
            unsafe {
                let mut cmsg = libc::CMSG_FIRSTHDR(&message);
                while !cmsg.is_null() {
                    if (*cmsg).cmsg_level == libc::SOL_SOCKET
                        && (*cmsg).cmsg_type == libc::SCM_RIGHTS
                    {
                        let header_len = libc::CMSG_LEN(0) as usize;
                        let data_len = ((*cmsg).cmsg_len as usize)
                            .checked_sub(header_len)
                            .ok_or_else(|| invalid("invalid ancillary length"))?;
                        if data_len % mem::size_of::<libc::c_int>() != 0 {
                            return Err(invalid("invalid descriptor array"));
                        }
                        for index in 0..data_len / mem::size_of::<libc::c_int>() {
                            let raw = std::ptr::read_unaligned(
                                libc::CMSG_DATA(cmsg).cast::<libc::c_int>().add(index),
                            );
                            if raw < 0 {
                                return Err(invalid("invalid received descriptor"));
                            }
                            received_fds.push(OwnedFd::from_raw_fd(raw));
                        }
                    }
                    cmsg = libc::CMSG_NXTHDR(&message, cmsg);
                }
            }
            if message.msg_flags & libc::MSG_CTRUNC != 0 {
                return Err(invalid("truncated imsg descriptors"));
            }
            for fd in &received_fds {
                // SAFETY: fd is owned and live; FD_CLOEXEC does not change file status flags.
                if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            if self.fds.len() + received_fds.len() > MAX_PENDING_FDS {
                return Err(invalid("too many unclaimed imsg descriptors"));
            }
            self.fds.extend(received_fds);
            if received == 0 {
                if offset == 0 && allow_eof {
                    return Ok(false);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated imsg message",
                ));
            }
            offset += received as usize;
        }
        Ok(true)
    }
}

fn encode_header(
    schema: Schema,
    kind: u32,
    payload_len: usize,
    has_fd: bool,
) -> io::Result<[u8; HEADER_SIZE]> {
    let length = HEADER_SIZE
        .checked_add(payload_len)
        .filter(|&len| len <= MAX_MESSAGE_SIZE)
        .ok_or_else(|| invalid_input("imsg too large"))?;
    let mut bytes = [0; HEADER_SIZE];
    bytes[0..4].copy_from_slice(&kind.to_ne_bytes());
    match schema {
        Schema::Legacy { .. } => {
            bytes[4..6].copy_from_slice(&(length as u16).to_ne_bytes());
            bytes[6..8].copy_from_slice(&u16::from(has_fd).to_ne_bytes());
        }
        Schema::Modern { .. } | Schema::Auto { .. } => {
            bytes[4..8].copy_from_slice(
                &((length as u32) | if has_fd { FD_MARK } else { 0 }).to_ne_bytes(),
            );
        }
    }
    bytes[8..12].copy_from_slice(&u32::from(schema.version()).to_ne_bytes());
    bytes[12..16].copy_from_slice(&u32::MAX.to_ne_bytes());
    Ok(bytes)
}

fn decode_length(bytes: &[u8; HEADER_SIZE], schema: &mut Schema) -> io::Result<(usize, bool)> {
    let raw = u32::from_ne_bytes(bytes[4..8].try_into().unwrap());
    let modern_len = (raw & !FD_MARK) as usize;
    let modern = (HEADER_SIZE..=MAX_MESSAGE_SIZE)
        .contains(&modern_len)
        .then_some((modern_len, raw & FD_MARK != 0));
    let legacy_len = u16::from_ne_bytes(bytes[4..6].try_into().unwrap()) as usize;
    let legacy_flags = u16::from_ne_bytes(bytes[6..8].try_into().unwrap());
    let legacy = ((HEADER_SIZE..=MAX_MESSAGE_SIZE).contains(&legacy_len) && legacy_flags <= 1)
        .then_some((legacy_len, legacy_flags == 1));
    match *schema {
        Schema::Modern { .. } => modern,
        Schema::Legacy { .. } => legacy,
        Schema::Auto { version } => match (modern, legacy) {
            (Some(modern), Some(legacy)) if modern == legacy => Some(modern),
            (Some(modern), None) => {
                *schema = Schema::Modern { version };
                Some(modern)
            }
            (None, Some(legacy)) => {
                *schema = Schema::Legacy { version };
                Some(legacy)
            }
            _ => None,
        },
    }
    .ok_or_else(|| invalid("invalid imsg header length/flags"))
}

pub fn pack_argv(argv: &[Vec<u8>]) -> io::Result<Vec<u8>> {
    let count = i32::try_from(argv.len()).map_err(|_| invalid_input("too many arguments"))?;
    let mut payload = count.to_ne_bytes().to_vec();
    for argument in argv {
        if argument.contains(&0) {
            return Err(invalid_input("NUL in argument"));
        }
        if payload
            .len()
            .saturating_add(argument.len())
            .saturating_add(1)
            > MAX_MESSAGE_SIZE - HEADER_SIZE
        {
            return Err(invalid_input("command too long"));
        }
        payload.extend_from_slice(argument);
        payload.push(0);
    }
    Ok(payload)
}

pub fn unpack_argv(payload: &[u8]) -> io::Result<Vec<Vec<u8>>> {
    let count = read_i32(payload, 0)?;
    if count < 0 || count as usize > payload.len() - 4 {
        return Err(invalid("invalid argument count"));
    }
    let mut rest = &payload[4..];
    let mut argv = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let end = rest
            .iter()
            .position(|&byte| byte == 0)
            .ok_or_else(|| invalid("unterminated argument"))?;
        argv.push(rest[..end].to_vec());
        rest = &rest[end + 1..];
    }
    if !rest.is_empty() {
        return Err(invalid("trailing command bytes"));
    }
    Ok(argv)
}

pub fn read_i32(payload: &[u8], offset: usize) -> io::Result<i32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| invalid("integer offset overflow"))?;
    Ok(i32::from_ne_bytes(
        payload
            .get(offset..end)
            .ok_or_else(|| invalid("truncated integer"))?
            .try_into()
            .unwrap(),
    ))
}

pub fn int_pair(first: i32, second: i32) -> Vec<u8> {
    [first.to_ne_bytes(), second.to_ne_bytes()].concat()
}

/// Peer identity for enforcing the same-user socket policy.
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // SAFETY: credentials and their length are valid output buffers.
        let mut credentials: libc::ucred = unsafe { mem::zeroed() };
        let mut length = mem::size_of_val(&credentials) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(credentials.uid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let mut uid = 0;
        let mut gid = 0;
        // SAFETY: valid socket and initialized output integers.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
