//! Clipboard requests leave policy and OS access with the host.

use base64::Engine;
use std::sync::Arc;

pub(crate) mod kitty;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    Standard,
    Selection,
    Primary,
}

impl Location {
    pub(crate) fn from_selector(selector: u8) -> Self {
        match selector {
            b's' => Self::Selection,
            b'p' => Self::Primary,
            _ => Self::Standard,
        }
    }

    fn selector(self) -> u8 {
        match self {
            Self::Standard => b'c',
            Self::Selection => b's',
            Self::Primary => b'p',
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terminator {
    Bell,
    St,
}

impl Terminator {
    pub fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Bell => b"\x07",
            Self::St => b"\x1b\\",
        }
    }
}

/// A binary-safe clipboard representation. An empty representation list clears
/// the clipboard; a representation with empty data retains its MIME type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Content {
    pub mime: Vec<u8>,
    /// Shared by MIME aliases so a bounded transfer cannot multiply its data.
    pub data: Arc<[u8]>,
}

pub fn is_text_mime(mime: &[u8]) -> bool {
    matches!(
        mime,
        b"text/plain" | b"text/plain;charset=utf-8" | b"UTF8_STRING" | b"TEXT" | b"STRING"
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Write {
    pub location: Location,
    pub contents: Vec<Content>,
    pub name: Vec<u8>,
    pub granted: bool,
    pub can_remember: bool,
    pub(crate) protocol: Protocol,
    pub(crate) terminator: Terminator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub location: Location,
    pub terminator: Terminator,
    pub mimes: Vec<Vec<u8>>,
    pub list: bool,
    pub name: Vec<u8>,
    pub granted: bool,
    pub can_remember: bool,
    pub(crate) protocol: Protocol,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Protocol {
    Osc52,
    Kitty { id: Vec<u8>, password: Vec<u8> },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadSuccess {
    pub contents: Vec<Content>,
    pub available: Vec<Vec<u8>>,
    pub remember: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ReadResult {
    Success(ReadSuccess),
    #[default]
    Denied,
    Unsupported,
    Busy,
    IoError,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriteResult {
    Success {
        remember: bool,
    },
    #[default]
    Denied,
    Unsupported,
    Busy,
    InvalidData,
    IoError,
}

impl Write {
    pub fn osc52(location: Location, contents: Vec<Content>) -> Self {
        Self {
            location,
            contents,
            name: Vec::new(),
            granted: false,
            can_remember: false,
            protocol: Protocol::Osc52,
            terminator: Terminator::St,
        }
    }

    /// Encode a write acknowledgement, when the requesting protocol has one.
    /// Use `Terminal::reply_clipboard_write` to also remember a session grant.
    pub fn reply(self, result: WriteResult) -> Option<Vec<u8>> {
        let Protocol::Kitty { id, .. } = &self.protocol else {
            return None;
        };
        let status = match result {
            WriteResult::Success { .. } => "DONE",
            WriteResult::Denied => "EPERM",
            WriteResult::Unsupported => "ENOSYS",
            WriteResult::Busy => "EBUSY",
            WriteResult::InvalidData => "EINVAL",
            WriteResult::IoError => "EIO",
        };
        Some(kitty::response("write", status, id, self.terminator))
    }
}

impl Read {
    pub fn osc52(location: Location, terminator: Terminator) -> Self {
        Self {
            location,
            terminator,
            mimes: vec![b"text/plain".to_vec()],
            list: false,
            name: Vec::new(),
            granted: false,
            can_remember: false,
            protocol: Protocol::Osc52,
        }
    }

    /// Encode a successful reply with these representations. OSC 52 uses the
    /// first text representation. Use `reply_result` for a MIME listing or to
    /// report a failure; Kitty distinguishes denial from an empty success.
    pub fn reply(self, contents: &[Content]) -> Vec<u8> {
        self.reply_result(ReadResult::Success(ReadSuccess {
            contents: contents.to_vec(),
            ..ReadSuccess::default()
        }))
    }

    /// Encode a reply with a protocol-specific failure status. Use
    /// `Terminal::reply_clipboard_read` to also remember a session grant.
    pub fn reply_result(self, result: ReadResult) -> Vec<u8> {
        if let Protocol::Kitty { id, .. } = &self.protocol {
            return kitty::read_reply(&self, id, result);
        }
        let mut reply = b"\x1b]52;".to_vec();
        reply.push(self.location.selector());
        reply.push(b';');
        if let ReadResult::Success(success) = result
            && let Some(content) = success.contents.iter().find(|c| is_text_mime(&c.mime))
        {
            reply.extend_from_slice(
                base64::engine::general_purpose::STANDARD
                    .encode(&content.data)
                    .as_bytes(),
            );
        }
        reply.extend_from_slice(self.terminator.as_bytes());
        reply
    }
}
