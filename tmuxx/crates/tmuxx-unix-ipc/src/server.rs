//! Initial command/control handshake for a Unix server adapter.

use crate::{Connection, Identity, invalid, msg, read_i32, unpack_argv};
use std::io;
use std::os::fd::OwnedFd;

pub struct AcceptedClient {
    pub identity: Identity,
    pub stdin: Option<OwnedFd>,
    pub stdout: Option<OwnedFd>,
    pub argv: Vec<Vec<u8>>,
}

/// Read identification and the initial argv, enforcing a bounded handshake.
/// Peer authorization belongs to the listener before calling this function.
pub fn accept_client(connection: &mut Connection) -> io::Result<AcceptedClient> {
    let mut client = AcceptedClient {
        identity: Identity {
            flags: 0,
            features: 0,
            term: Vec::new(),
            tty_name: Vec::new(),
            cwd: Vec::new(),
            pid: 0,
            environment: Vec::new(),
        },
        stdin: None,
        stdout: None,
        argv: Vec::new(),
    };
    let mut identified = false;
    for _ in 0..1024 {
        let message = connection.recv()?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "client disconnected during identify",
            )
        })?;
        if message.peer_id & 255 != u32::from(connection.schema().version()) {
            connection.send(msg::VERSION, &[], None)?;
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "tmux client protocol version mismatch",
            ));
        }
        let data = &message.payload;
        if message.kind == msg::COMMAND {
            if !identified || message.fd.is_some() {
                return Err(invalid("COMMAND before identification or with descriptor"));
            }
            client.argv = unpack_argv(data)?;
            return Ok(client);
        }
        if identified {
            return Err(invalid("unexpected message after identification"));
        }
        if message.fd.is_some()
            && !matches!(message.kind, msg::IDENTIFY_STDIN | msg::IDENTIFY_STDOUT)
        {
            return Err(invalid("descriptor on non-stream identification"));
        }
        match message.kind {
            msg::IDENTIFY_FLAGS => {
                if data.len() != 4 {
                    return Err(invalid("invalid identify flags"));
                }
                client.identity.flags |= u64::from(read_i32(data, 0)? as u32);
            }
            msg::IDENTIFY_LONGFLAGS => {
                client.identity.flags |= u64::from_ne_bytes(
                    data.as_slice()
                        .try_into()
                        .map_err(|_| invalid("invalid identify longflags"))?,
                );
            }
            msg::IDENTIFY_TERM => client.identity.term = string(data)?,
            msg::IDENTIFY_TTYNAME => client.identity.tty_name = string(data)?,
            msg::IDENTIFY_CWD => client.identity.cwd = string(data)?,
            msg::IDENTIFY_ENVIRON => client.identity.environment.push(string(data)?),
            msg::IDENTIFY_TERMINFO => {
                string(data)?;
            }
            msg::IDENTIFY_FEATURES => {
                if data.len() != 4 {
                    return Err(invalid("invalid identify features"));
                }
                client.identity.features |= read_i32(data, 0)?;
            }
            msg::IDENTIFY_CLIENTPID => {
                if data.len() != 4 {
                    return Err(invalid("invalid identify pid"));
                }
                client.identity.pid = read_i32(data, 0)?;
            }
            msg::IDENTIFY_STDIN => {
                if !data.is_empty() || client.stdin.is_some() {
                    return Err(invalid("invalid or repeated stdin"));
                }
                client.stdin = message.fd;
            }
            msg::IDENTIFY_STDOUT => {
                if !data.is_empty() || client.stdout.is_some() {
                    return Err(invalid("invalid or repeated stdout"));
                }
                client.stdout = message.fd;
            }
            msg::IDENTIFY_DONE => {
                if !data.is_empty() {
                    return Err(invalid("invalid identify done"));
                }
                identified = true;
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("unsupported identify message {}", message.kind),
                ));
            }
        }
    }
    Err(invalid("identify message limit exceeded"))
}

fn string(bytes: &[u8]) -> io::Result<Vec<u8>> {
    if bytes.last() != Some(&0) || bytes[..bytes.len() - 1].contains(&0) {
        return Err(invalid("invalid identify string"));
    }
    Ok(bytes[..bytes.len() - 1].to_vec())
}
