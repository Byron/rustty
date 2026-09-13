//! OSC 5522 parsing, bounded transfers, and session grants.

use super::{Content, Location, Protocol, Read, ReadResult, Terminator, Write, WriteResult};
use crate::Effect;
use base64::Engine;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use std::sync::Arc;

const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true),
);

#[derive(Clone, Debug, Default)]
pub(crate) struct State {
    write: Option<Transaction>,
    grants: Vec<Grant>,
}

impl State {
    pub fn clear_grants(&mut self) {
        self.grants.clear();
    }

    pub fn grant(&mut self, password: &[u8], read: bool, one_time: bool) {
        if password.is_empty() || password.len() > 128 {
            return;
        }
        let entry = if let Some(index) = self.grants.iter().position(|g| g.password == password) {
            let entry = &mut self.grants[index];
            entry.one_time &= one_time;
            entry
        } else {
            if self.grants.len() == 32 {
                self.grants.remove(0);
            }
            self.grants.push(Grant {
                password: password.to_vec(),
                read: false,
                write: false,
                one_time,
            });
            self.grants.last_mut().unwrap()
        };
        if read {
            entry.read = true;
        } else {
            entry.write = true;
        }
    }

    pub(crate) fn use_grant(&mut self, password: &[u8], read: bool) -> bool {
        let Some(index) = self.grants.iter().position(|g| g.password == password) else {
            return false;
        };
        let grant = &self.grants[index];
        let allowed = if read { grant.read } else { grant.write };
        if grant.one_time {
            self.grants.swap_remove(index);
        }
        allowed
    }

    pub fn remember_read(&mut self, request: &Read, result: &ReadResult) {
        if let Protocol::Kitty { password, .. } = &request.protocol
            && let ReadResult::Success(success) = result
            && success.remember
        {
            self.grant(password, true, false);
        }
    }

    pub fn remember_write(&mut self, request: &Write, result: WriteResult) {
        if let Protocol::Kitty { password, .. } = &request.protocol
            && let WriteResult::Success { remember: true } = result
        {
            self.grant(password, false, false);
        }
    }

    pub fn handle(
        &mut self,
        data: &[u8],
        terminator: Terminator,
        max_size: usize,
        write_enabled: bool,
        read_enabled: bool,
        effects: &mut Vec<Effect>,
    ) {
        let split = data.iter().position(|&b| b == b';').unwrap_or(data.len());
        let Some(raw) = Raw::parse(&data[..split]) else {
            return;
        };
        let op = raw.op;
        let Ok(meta) = Metadata::decode(raw) else {
            if matches!(op, Operation::Data | Operation::Alias) {
                self.finish_write("EINVAL", terminator, effects);
            }
            return;
        };
        let payload = data.get(split + 1..).unwrap_or_default();
        match op {
            Operation::Read => {
                let Ok(payload) = BASE64.decode(payload) else {
                    return;
                };
                if std::str::from_utf8(&payload).is_err() {
                    return;
                }
                if !read_enabled {
                    effects.push(Effect::Write(response(
                        "read", "EPERM", &meta.id, terminator,
                    )));
                    return;
                }
                let mut list = false;
                let mut mimes = Vec::new();
                for mime in mime_list(&payload) {
                    if mime == b"." {
                        list = true;
                    } else if mimes.len() < 4 {
                        mimes.push(mime.to_vec());
                    }
                }
                let password = meta.effective_password().to_vec();
                let granted = !mimes.is_empty() && self.use_grant(&password, true);
                effects.push(Effect::ClipboardRead(Read {
                    location: meta.location,
                    terminator,
                    mimes,
                    list,
                    name: meta.name,
                    granted,
                    can_remember: !password.is_empty(),
                    protocol: Protocol::Kitty {
                        id: meta.id,
                        password,
                    },
                }));
            }
            Operation::Write => {
                self.write = None;
                if !write_enabled {
                    effects.push(Effect::Write(response(
                        "write", "ENOSYS", &meta.id, terminator,
                    )));
                    return;
                }
                self.write = Some(Transaction::new(meta, max_size));
            }
            Operation::Data if meta.mime.is_empty() => {
                let Some(mut transfer) = self.write.take() else {
                    return;
                };
                let contents = match transfer.commit() {
                    Ok(contents) => contents,
                    Err(status) => {
                        effects.push(Effect::Write(response(
                            "write",
                            status,
                            &transfer.meta.id,
                            terminator,
                        )));
                        return;
                    }
                };
                if !write_enabled {
                    effects.push(Effect::Write(response(
                        "write",
                        "ENOSYS",
                        &transfer.meta.id,
                        terminator,
                    )));
                    return;
                }
                let password = transfer.meta.effective_password().to_vec();
                let granted = self.use_grant(&password, false);
                effects.push(Effect::ClipboardWrite(Write {
                    location: transfer.meta.location,
                    contents,
                    name: transfer.meta.name,
                    granted,
                    can_remember: !password.is_empty(),
                    protocol: Protocol::Kitty {
                        id: transfer.meta.id,
                        password,
                    },
                    terminator,
                }));
            }
            Operation::Data | Operation::Alias => {
                let Some(transfer) = &mut self.write else {
                    return;
                };
                let result = if op == Operation::Data {
                    transfer.data(&meta.mime, payload)
                } else {
                    transfer.alias(&meta.mime, payload)
                };
                if let Err(status) = result {
                    self.finish_write(status, terminator, effects);
                }
            }
        }
    }

    fn finish_write(&mut self, status: &str, terminator: Terminator, effects: &mut Vec<Effect>) {
        if let Some(transfer) = self.write.take() {
            effects.push(Effect::Write(response(
                "write",
                status,
                &transfer.meta.id,
                terminator,
            )));
        }
    }
}

#[derive(Clone, Debug)]
struct Grant {
    password: Vec<u8>,
    read: bool,
    write: bool,
    one_time: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Read,
    Write,
    Data,
    Alias,
}

struct Raw<'a> {
    op: Operation,
    location: &'a [u8],
    id: &'a [u8],
    mime: &'a [u8],
    password: &'a [u8],
    name: &'a [u8],
}

impl<'a> Raw<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut op = None;
        let (mut location, mut id, mut mime, mut password, mut name) =
            (&b""[..], &b""[..], &b""[..], &b""[..], &b""[..]);
        for field in data.split(|&b| b == b':') {
            let split = field.iter().position(|&b| b == b'=')?;
            let value = &field[split + 1..];
            match &field[..split] {
                b"type" => op = Some(value),
                b"loc" => location = value,
                b"id" => id = value,
                b"mime" => mime = value,
                b"pw" => password = value,
                b"name" => name = value,
                _ => {}
            }
        }
        let op = match op? {
            b"read" => Operation::Read,
            b"write" => Operation::Write,
            b"wdata" => Operation::Data,
            b"walias" => Operation::Alias,
            _ => return None,
        };
        Some(Self {
            op,
            location,
            id,
            mime,
            password,
            name,
        })
    }
}

#[derive(Clone, Debug)]
struct Metadata {
    location: Location,
    id: Vec<u8>,
    mime: Vec<u8>,
    password: Vec<u8>,
    name: Vec<u8>,
}

impl Metadata {
    fn decode(raw: Raw<'_>) -> Result<Self, ()> {
        fn value(raw: &[u8], limit: usize, ignore_overflow: bool) -> Result<Vec<u8>, ()> {
            if raw.len() > limit.div_ceil(3) * 4 {
                return if ignore_overflow {
                    Ok(Vec::new())
                } else {
                    Err(())
                };
            }
            let decoded = BASE64.decode(raw).map_err(|_| ())?;
            if std::str::from_utf8(&decoded).is_err() {
                return Err(());
            }
            if decoded.len() > limit {
                return if ignore_overflow {
                    Ok(Vec::new())
                } else {
                    Err(())
                };
            }
            Ok(decoded)
        }
        Ok(Self {
            location: if raw.location == b"primary" {
                Location::Primary
            } else {
                Location::Standard
            },
            id: raw
                .id
                .iter()
                .copied()
                .filter(|b| b.is_ascii_alphanumeric() || b"-_+.".contains(b))
                .take(512)
                .collect(),
            mime: value(raw.mime, 256, false)?,
            password: value(raw.password, 128, true)?,
            name: value(raw.name, 256, false)?,
        })
    }

    fn effective_password(&self) -> &[u8] {
        if self.name.is_empty() {
            b""
        } else {
            &self.password
        }
    }
}

fn mime_list(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    data.split(|b| *b == b' ' || (b'\t'..=b'\r').contains(b))
        .filter(|mime| !mime.is_empty())
}

#[derive(Clone, Debug)]
struct Transaction {
    meta: Metadata,
    entries: Vec<(Vec<u8>, Vec<u8>)>,
    aliases: Vec<(Vec<u8>, Vec<u8>)>,
    current: Option<usize>,
    decoder: Streaming,
    decoded_bytes: usize,
    max_size: usize,
}

impl Transaction {
    fn new(meta: Metadata, max_size: usize) -> Self {
        Self {
            meta,
            entries: Vec::new(),
            aliases: Vec::new(),
            current: None,
            decoder: Streaming::default(),
            decoded_bytes: 0,
            max_size,
        }
    }

    fn data(&mut self, mime: &[u8], payload: &[u8]) -> Result<(), &'static str> {
        if !self
            .current
            .is_some_and(|index| self.entries[index].0 == mime)
        {
            self.decoder.finish()?;
            self.current =
                if let Some(index) = self.entries.iter().position(|(name, _)| name == mime) {
                    self.entries[index].1.clear();
                    Some(index)
                } else if self.entries.len() < 64 {
                    self.entries.push((mime.to_vec(), Vec::new()));
                    Some(self.entries.len() - 1)
                } else {
                    None
                };
        }
        let Some(index) = self.current else {
            return Ok(());
        };
        let decoded = self.decoder.feed(payload)?;
        if decoded.len() > self.max_size.saturating_sub(self.decoded_bytes) {
            return Err("EFBIG");
        }
        self.decoded_bytes += decoded.len();
        self.entries[index].1.extend_from_slice(&decoded);
        Ok(())
    }

    fn alias(&mut self, target: &[u8], payload: &[u8]) -> Result<(), &'static str> {
        if target.is_empty() {
            return Err("EINVAL");
        }
        let decoded = BASE64.decode(payload).map_err(|_| "EINVAL")?;
        if std::str::from_utf8(&decoded).is_err() {
            return Err("EINVAL");
        }
        for name in mime_list(&decoded).filter(|name| name.len() <= 256) {
            if let Some((_, old)) = self.aliases.iter_mut().find(|(alias, _)| alias == name) {
                *old = target.to_vec();
            } else if self.aliases.len() < 64 {
                self.aliases.push((name.to_vec(), target.to_vec()));
            } else {
                break;
            }
        }
        Ok(())
    }

    fn commit(&mut self) -> Result<Vec<Content>, &'static str> {
        self.decoder.finish()?;
        let mut contents: Vec<_> = self
            .entries
            .drain(..)
            .map(|(mime, data)| Content {
                mime,
                data: Arc::from(data),
            })
            .collect();
        for (alias, target) in &self.aliases {
            let Some(data) = contents
                .iter()
                .find(|c| &c.mime == target)
                .map(|c| c.data.clone())
            else {
                continue;
            };
            if let Some(content) = contents.iter_mut().find(|c| &c.mime == alias) {
                content.data = data;
            } else {
                contents.push(Content {
                    mime: alias.clone(),
                    data,
                });
            }
        }
        Ok(contents)
    }
}

#[derive(Clone, Debug, Default)]
struct Streaming {
    carry: Vec<u8>,
}

impl Streaming {
    fn feed(&mut self, input: &[u8]) -> Result<Vec<u8>, &'static str> {
        let mut rem = input;
        let mut result = Vec::new();
        if !self.carry.is_empty() {
            let take = (4 - self.carry.len()).min(rem.len());
            self.carry.extend_from_slice(&rem[..take]);
            rem = &rem[take..];
            if self.carry.len() < 4 {
                self.validate_carry()?;
                return Ok(result);
            }
            result = BASE64.decode(&self.carry).map_err(|_| "EINVAL")?;
            let padded = self.carry.ends_with(b"=");
            self.carry.clear();
            if padded {
                return if rem.is_empty() {
                    Ok(result)
                } else {
                    Err("EINVAL")
                };
            }
        }
        let bulk_len = rem.len() / 4 * 4;
        result.extend(BASE64.decode(&rem[..bulk_len]).map_err(|_| "EINVAL")?);
        if rem[..bulk_len].ends_with(b"=") && bulk_len != rem.len() {
            return Err("EINVAL");
        }
        self.carry.extend_from_slice(&rem[bulk_len..]);
        self.validate_carry()?;
        Ok(result)
    }

    fn validate_carry(&self) -> Result<(), &'static str> {
        if self.carry.iter().enumerate().any(|(i, &b)| {
            !(b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || i >= 2 && b == b'=')
        }) {
            Err("EINVAL")
        } else {
            Ok(())
        }
    }

    fn finish(&mut self) -> Result<(), &'static str> {
        let valid = self.carry.is_empty();
        self.carry.clear();
        if valid { Ok(()) } else { Err("EINVAL") }
    }
}

pub(crate) fn response(op: &str, status: &str, id: &[u8], terminator: Terminator) -> Vec<u8> {
    let mut output = packet(op, status, false, id, None, None, &[]);
    output.extend_from_slice(terminator.as_bytes());
    output
}

fn packet(
    op: &str,
    status: &str,
    primary: bool,
    id: &[u8],
    mime: Option<&[u8]>,
    password: Option<&[u8]>,
    payload: &[u8],
) -> Vec<u8> {
    let mut output = format!("\x1b]5522;type={op}:status={status}").into_bytes();
    if primary {
        output.extend_from_slice(b":loc=primary");
    }
    if !id.is_empty() {
        output.extend_from_slice(b":id=");
        output.extend_from_slice(id);
    }
    if let Some(mime) = mime {
        output.extend_from_slice(b":mime=");
        output.extend_from_slice(BASE64.encode(mime).as_bytes());
    }
    if let Some(password) = password {
        output.extend_from_slice(b":pw=");
        output.extend_from_slice(BASE64.encode(password).as_bytes());
    }
    if !payload.is_empty() {
        output.push(b';');
        output.extend_from_slice(BASE64.encode(payload).as_bytes());
    }
    output
}

pub(crate) fn read_reply(request: &Read, id: &[u8], result: ReadResult) -> Vec<u8> {
    let success = match result {
        ReadResult::Success(success) => success,
        result => {
            let status = match result {
                ReadResult::Denied => "EPERM",
                ReadResult::Unsupported => "ENOSYS",
                ReadResult::Busy => "EBUSY",
                ReadResult::IoError => "EIO",
                ReadResult::Success(_) => unreachable!(),
            };
            return response("read", status, id, request.terminator);
        }
    };
    let mut output = packet(
        "read",
        "OK",
        request.location == Location::Primary,
        id,
        None,
        None,
        &[],
    );
    output.extend_from_slice(request.terminator.as_bytes());
    if request.list {
        let listing = listing(success.available.iter().map(Vec::as_slice));
        output.extend(packet(
            "read",
            "DATA",
            false,
            id,
            Some(b"."),
            None,
            &listing,
        ));
        output.extend_from_slice(request.terminator.as_bytes());
    }
    for mime in &request.mimes {
        if let Some(content) = success.contents.iter().find(|c| &c.mime == mime) {
            for chunk in content.data.chunks(4096) {
                output.extend(packet("read", "DATA", false, id, Some(mime), None, chunk));
                output.extend_from_slice(request.terminator.as_bytes());
            }
        }
    }
    output.extend(response("read", "DONE", id, request.terminator));
    output
}

fn listing<'a>(available: impl Iterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut listing = Vec::new();
    let mut any = false;
    for (index, mime) in available.enumerate() {
        any = true;
        if listing.len() + usize::from(index > 0) + mime.len() + 1 > 4096 {
            break;
        }
        if index > 0 {
            listing.push(b' ');
        }
        listing.extend_from_slice(mime);
    }
    if any {
        listing.push(b'\n');
    }
    listing
}

pub(crate) fn paste_event(location: Location, password: &[u8], available: &[&[u8]]) -> Vec<u8> {
    let mut output = packet(
        "read",
        "OK",
        location != Location::Standard,
        &[],
        None,
        Some(password),
        &[],
    );
    output.extend_from_slice(Terminator::St.as_bytes());
    output.extend(packet(
        "read",
        "DATA",
        false,
        &[],
        Some(b"."),
        Some(password),
        &listing(available.iter().copied()),
    ));
    output.extend_from_slice(Terminator::St.as_bytes());
    output.extend(packet(
        "read",
        "DONE",
        false,
        &[],
        None,
        Some(password),
        &[],
    ));
    output.extend_from_slice(Terminator::St.as_bytes());
    output
}
