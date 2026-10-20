//! Transport-independent tmux control mode. No input is decoded as UTF-8.
//!
//! Use [`Decoder::feed_into`] to process arbitrarily large responses without
//! retaining a whole command result. The caller chooses its record-size limit.

use std::fmt;

pub mod features;

#[cfg(feature = "native")]
pub mod native;

/// The complete identity of a command's opening and closing guards.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Guard {
    pub time: i64,
    pub number: u64,
    pub flags: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    Begin(Guard),
    /// One command output line, excluding its line terminator.
    CommandOutput(Vec<u8>),
    /// Deferred server diagnostics and job output can arrive after a command's
    /// closing guard. They do not belong to a later command or completion barrier.
    UnframedOutput(Vec<u8>),
    End {
        guard: Guard,
        failed: bool,
    },
    Notification(Notification),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Notification {
    Output {
        pane_id: u64,
        data: Vec<u8>,
        age_ms: Option<u64>,
    },
    LayoutChange {
        window_id: u64,
        layout: Vec<u8>,
        visible_layout: Vec<u8>,
        flags: Vec<u8>,
    },
    SessionChanged {
        session_id: u64,
        name: Vec<u8>,
    },
    /// Older servers can omit the session ID in this notification.
    SessionRenamed {
        session_id: Option<u64>,
        name: Vec<u8>,
    },
    SessionsChanged,
    SessionWindowChanged {
        session_id: u64,
        window_id: u64,
    },
    WindowAdd {
        window_id: u64,
        linked: bool,
    },
    WindowClose {
        window_id: u64,
        linked: bool,
    },
    WindowRenamed {
        window_id: u64,
        name: Vec<u8>,
        linked: bool,
    },
    WindowPaneChanged {
        window_id: u64,
        pane_id: u64,
    },
    PaneModeChanged {
        pane_id: u64,
    },
    Pause {
        pane_id: u64,
    },
    Continue {
        pane_id: u64,
    },
    ClientDetached {
        client: Vec<u8>,
    },
    ClientSessionChanged {
        client: Vec<u8>,
        session_id: u64,
        name: Vec<u8>,
    },
    PasteBufferChanged {
        name: Vec<u8>,
    },
    PasteBufferDeleted {
        name: Vec<u8>,
    },
    SubscriptionChanged {
        fields: Vec<Vec<u8>>,
        value: Vec<u8>,
    },
    ConfigError(Vec<u8>),
    Message(Vec<u8>),
    Exit {
        reason: Option<Vec<u8>>,
    },
    /// The entire original record, including `%`, excluding the newline.
    Unknown(Vec<u8>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    RecordTooLong { limit: usize },
    InvalidGuard,
    UnexpectedGuard,
    InvalidNotification(&'static str),
    InvalidEscape,
    UnexpectedRecord(Vec<u8>),
    TruncatedRecord,
    UnterminatedCommand(Guard),
    InvalidDcs,
    TruncatedDcs,
    Closed,
    Failed,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RecordTooLong { limit } => write!(f, "control record exceeds {limit} bytes"),
            Self::InvalidGuard => f.write_str("invalid control command guard"),
            Self::UnexpectedGuard => f.write_str("control closing guard without an open command"),
            Self::InvalidNotification(name) => write!(f, "invalid %{name} notification"),
            Self::InvalidEscape => f.write_str("invalid octal escape in pane output"),
            Self::UnexpectedRecord(_) => f.write_str("unexpected text outside a control command"),
            Self::TruncatedRecord => f.write_str("control stream ended inside a record"),
            Self::UnterminatedCommand(_) => f.write_str("control stream ended inside a command"),
            Self::InvalidDcs => f.write_str("invalid control-mode DCS framing"),
            Self::TruncatedDcs => f.write_str("control-mode DCS framing was not terminated"),
            Self::Closed => f.write_str("control decoder has already finished"),
            Self::Failed => f.write_str("control decoder has failed; create a new decoder"),
        }
    }
}

impl std::error::Error for DecodeError {}

#[derive(Debug)]
enum Framing {
    Plain,
    DcsStart(usize),
    DcsBody,
    DcsEnd(usize),
    DcsClosed,
}

/// An incremental decoder for `tmux -C` (or, explicitly, `-CC`).
///
/// Errors poison the decoder because dropping part of a record cannot safely
/// restore protocol synchronization. Construct a new decoder for a new stream.
#[derive(Debug)]
pub struct Decoder {
    line: Vec<u8>,
    limit: usize,
    guard: Option<Guard>,
    framing: Framing,
    failed: bool,
    finished: bool,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::with_limit(8 * 1024 * 1024)
    }
}

impl Decoder {
    /// Limit bytes in one record, excluding its LF (and DCS mode's optional CR).
    /// This is a resource policy, not a limit specified by tmux.
    pub fn with_limit(max_record_bytes: usize) -> Self {
        Self {
            line: Vec::new(),
            limit: max_record_bytes,
            guard: None,
            framing: Framing::Plain,
            failed: false,
            finished: false,
        }
    }

    /// Decode a `-CC` stream, including its exact DCS entry/exit wrappers.
    /// The caller must locate the DCS entry in the containing terminal stream.
    pub fn dcs(max_record_bytes: usize) -> Self {
        Self {
            framing: Framing::DcsStart(0),
            ..Self::with_limit(max_record_bytes)
        }
    }

    pub fn active_guard(&self) -> Option<Guard> {
        self.guard
    }

    /// Convenience collector. Use `feed_into` for streaming without this allocation.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Event>, DecodeError> {
        let mut events = Vec::new();
        self.feed_into(bytes, |event| events.push(event))?;
        Ok(events)
    }

    /// Emits complete records as they arrive, including before a later error.
    pub fn feed_into(
        &mut self,
        bytes: &[u8],
        mut emit: impl FnMut(Event),
    ) -> Result<(), DecodeError> {
        if self.failed {
            return Err(DecodeError::Failed);
        }
        if self.finished {
            return Err(DecodeError::Closed);
        }
        let result = self.feed_inner(bytes, &mut emit);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn feed_inner(
        &mut self,
        bytes: &[u8],
        emit: &mut impl FnMut(Event),
    ) -> Result<(), DecodeError> {
        for &byte in bytes {
            match &mut self.framing {
                Framing::DcsStart(index) => {
                    if byte != b"\x1bP1000p"[*index] {
                        return Err(DecodeError::InvalidDcs);
                    }
                    *index += 1;
                    if *index == 7 {
                        self.framing = Framing::DcsBody;
                    }
                    continue;
                }
                Framing::DcsEnd(index) => {
                    if byte != b"\x1b\\"[*index] {
                        return Err(DecodeError::InvalidDcs);
                    }
                    *index += 1;
                    if *index == 2 {
                        self.framing = Framing::DcsClosed;
                    }
                    continue;
                }
                Framing::DcsClosed => return Err(DecodeError::InvalidDcs),
                _ => {}
            }
            if byte == b'\n' {
                if matches!(self.framing, Framing::DcsBody) && self.line.last() == Some(&b'\r') {
                    self.line.pop();
                }
                if self.line.len() > self.limit {
                    return Err(DecodeError::RecordTooLong { limit: self.limit });
                }
                let event = self.record()?;
                self.line.clear();
                if matches!(event, Event::Notification(Notification::Exit { .. }))
                    && matches!(self.framing, Framing::DcsBody)
                {
                    self.framing = Framing::DcsEnd(0);
                }
                emit(event);
            } else {
                let cr_allowance =
                    usize::from(matches!(self.framing, Framing::DcsBody) && byte == b'\r');
                if self.line.len() >= self.limit.saturating_add(cr_allowance) {
                    return Err(DecodeError::RecordTooLong { limit: self.limit });
                }
                self.line.push(byte);
            }
        }
        Ok(())
    }

    fn record(&mut self) -> Result<Event, DecodeError> {
        let (kind, arguments) = split_first(&self.line);
        if let Some(guard) = self.guard {
            // Arbitrary command output may look like any protocol record.
            if (kind == b"%end" || kind == b"%error") && parse_guard(arguments) == Ok(guard) {
                self.guard = None;
                return Ok(Event::End {
                    guard,
                    failed: kind == b"%error",
                });
            }
            return Ok(Event::CommandOutput(self.line.clone()));
        }
        match kind {
            b"%begin" => {
                let guard = parse_guard(arguments)?;
                self.guard = Some(guard);
                Ok(Event::Begin(guard))
            }
            b"%end" | b"%error" => Err(DecodeError::UnexpectedGuard),
            _ if kind.starts_with(b"%") => {
                notification(kind, arguments, &self.line).map(Event::Notification)
            }
            _ => Ok(Event::UnframedOutput(self.line.clone())),
        }
    }

    /// Validate EOF. A `%exit` notification is not required for a clean transport EOF.
    pub fn finish(&mut self) -> Result<(), DecodeError> {
        if self.failed {
            return Err(DecodeError::Failed);
        }
        if self.finished {
            return Err(DecodeError::Closed);
        }
        let result = if !self.line.is_empty() {
            Err(DecodeError::TruncatedRecord)
        } else if let Some(guard) = self.guard {
            Err(DecodeError::UnterminatedCommand(guard))
        } else if !matches!(self.framing, Framing::Plain | Framing::DcsClosed) {
            Err(DecodeError::TruncatedDcs)
        } else {
            Ok(())
        };
        self.finished = true;
        result
    }
}

fn split_first(bytes: &[u8]) -> (&[u8], &[u8]) {
    match bytes.iter().position(|&b| b == b' ') {
        Some(n) => (&bytes[..n], &bytes[n + 1..]),
        None => (bytes, &[]),
    }
}

fn number(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0_u64, |n, b| {
        if b.is_ascii_digit() {
            n.checked_mul(10)?.checked_add(u64::from(b - b'0'))
        } else {
            None
        }
    })
}

fn id(bytes: &[u8], prefix: u8) -> Option<u64> {
    if bytes.first() != Some(&prefix) {
        return None;
    }
    number(&bytes[1..])
}

fn parse_guard(bytes: &[u8]) -> Result<Guard, DecodeError> {
    let mut fields = bytes.split(|&b| b == b' ');
    let time = fields
        .next()
        .and_then(|b| std::str::from_utf8(b).ok())
        .and_then(|s| s.parse::<i64>().ok());
    let command_number = fields.next().and_then(number);
    let flags = fields.next().and_then(number);
    match (time, command_number, flags, fields.next()) {
        (Some(time), Some(number), Some(flags), None) => Ok(Guard {
            time,
            number,
            flags,
        }),
        _ => Err(DecodeError::InvalidGuard),
    }
}

fn notification(kind: &[u8], args: &[u8], raw: &[u8]) -> Result<Notification, DecodeError> {
    use Notification::*;
    let (first, rest) = split_first(args);
    let invalid = || {
        DecodeError::InvalidNotification(match kind {
            b"%output" => "output",
            b"%extended-output" => "extended-output",
            b"%layout-change" => "layout-change",
            b"%subscription-changed" => "subscription-changed",
            _ => "event",
        })
    };
    let pane = || id(args, b'%').ok_or_else(invalid);
    let window = || id(first, b'@').ok_or_else(invalid);
    Ok(match kind {
        b"%output" => Output {
            pane_id: id(first, b'%').ok_or_else(invalid)?,
            data: unescape_output(rest)?,
            age_ms: None,
        },
        b"%extended-output" => {
            let pane_id = id(first, b'%').ok_or_else(invalid)?;
            let (age, extended) = split_first(rest);
            let age_ms = number(age).ok_or_else(invalid)?;
            let (_, value) = fields_until_colon(extended).ok_or_else(invalid)?;
            Output {
                pane_id,
                data: unescape_output(value)?,
                age_ms: Some(age_ms),
            }
        }
        b"%layout-change" => {
            let (layout, rest) = split_first(rest);
            let (visible, flags) = split_first(rest);
            if layout.is_empty() || visible.is_empty() {
                return Err(invalid());
            }
            LayoutChange {
                window_id: window()?,
                layout: layout.to_vec(),
                visible_layout: visible.to_vec(),
                flags: flags.to_vec(),
            }
        }
        b"%session-changed" => SessionChanged {
            session_id: id(first, b'$').ok_or_else(invalid)?,
            name: rest.to_vec(),
        },
        b"%session-renamed" => match id(first, b'$') {
            Some(session_id) => SessionRenamed {
                session_id: Some(session_id),
                name: rest.to_vec(),
            },
            None => SessionRenamed {
                session_id: None,
                name: args.to_vec(),
            },
        },
        b"%sessions-changed" if args.is_empty() => SessionsChanged,
        b"%session-window-changed" => SessionWindowChanged {
            session_id: id(first, b'$').ok_or_else(invalid)?,
            window_id: id(rest, b'@').ok_or_else(invalid)?,
        },
        b"%window-add" | b"%unlinked-window-add" => WindowAdd {
            window_id: id(args, b'@').ok_or_else(invalid)?,
            linked: kind == b"%window-add",
        },
        b"%window-close" | b"%unlinked-window-close" => WindowClose {
            window_id: id(args, b'@').ok_or_else(invalid)?,
            linked: kind == b"%window-close",
        },
        b"%window-renamed" | b"%unlinked-window-renamed" => WindowRenamed {
            window_id: window()?,
            name: rest.to_vec(),
            linked: kind == b"%window-renamed",
        },
        b"%window-pane-changed" => WindowPaneChanged {
            window_id: window()?,
            pane_id: id(rest, b'%').ok_or_else(invalid)?,
        },
        b"%pane-mode-changed" => PaneModeChanged { pane_id: pane()? },
        b"%pause" => Pause { pane_id: pane()? },
        b"%continue" => Continue { pane_id: pane()? },
        b"%client-detached" => ClientDetached {
            client: args.to_vec(),
        },
        b"%client-session-changed" => {
            let (session, name) = split_first(rest);
            ClientSessionChanged {
                client: first.to_vec(),
                session_id: id(session, b'$').ok_or_else(invalid)?,
                name: name.to_vec(),
            }
        }
        b"%paste-buffer-changed" => PasteBufferChanged {
            name: args.to_vec(),
        },
        b"%paste-buffer-deleted" => PasteBufferDeleted {
            name: args.to_vec(),
        },
        b"%subscription-changed" => {
            let (fields, value) = fields_until_colon(args).ok_or_else(invalid)?;
            if fields.len() < 5 {
                return Err(invalid());
            }
            SubscriptionChanged {
                fields,
                value: value.to_vec(),
            }
        }
        b"%config-error" => ConfigError(args.to_vec()),
        b"%message" => Message(args.to_vec()),
        b"%exit" => Exit {
            reason: if args.is_empty() {
                None
            } else {
                Some(args.to_vec())
            },
        },
        _ => Unknown(raw.to_vec()),
    })
}

fn fields_until_colon(mut bytes: &[u8]) -> Option<(Vec<Vec<u8>>, &[u8])> {
    let mut fields = Vec::new();
    loop {
        let (field, rest) = split_first(bytes);
        if field == b":" {
            return Some((fields, rest));
        }
        if field.is_empty() || rest.is_empty() {
            return None;
        }
        fields.push(field.to_vec());
        bytes = rest;
    }
}

/// Escape exactly the bytes escaped by tmux's control output: C0 and backslash.
pub fn escape_output(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    for &byte in bytes {
        if byte < b' ' || byte == b'\\' {
            output.extend_from_slice(&[
                b'\\',
                b'0' + (byte >> 6),
                b'0' + ((byte >> 3) & 7),
                b'0' + (byte & 7),
            ]);
        } else {
            output.push(byte);
        }
    }
    output
}

pub fn unescape_output(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            output.push(bytes[index]);
            index += 1;
        } else {
            let digits = bytes
                .get(index + 1..index + 4)
                .ok_or(DecodeError::InvalidEscape)?;
            if !(b'0'..=b'3').contains(&digits[0])
                || !digits[1..].iter().all(|d| (b'0'..=b'7').contains(d))
            {
                return Err(DecodeError::InvalidEscape);
            }
            output.push(((digits[0] - b'0') << 6) | ((digits[1] - b'0') << 3) | (digits[2] - b'0'));
            index += 4;
        }
    }
    Ok(output)
}

pub fn encode_output(pane_id: u64, bytes: &[u8]) -> Vec<u8> {
    let mut output = format!("%output %{pane_id} ").into_bytes();
    output.extend_from_slice(&escape_output(bytes));
    output.push(b'\n');
    output
}

pub fn encode_guard(guard: Guard, kind: GuardKind) -> Vec<u8> {
    let name = match kind {
        GuardKind::Begin => "begin",
        GuardKind::End => "end",
        GuardKind::Error => "error",
    };
    format!("%{name} {} {} {}\n", guard.time, guard.number, guard.flags).into_bytes()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuardKind {
    Begin,
    End,
    Error,
}

/// Frame textual command output. Notification emission must wait until afterward.
/// As in tmux, arbitrary output that exactly matches the closing guard is ambiguous.
pub fn encode_command_result(guard: Guard, output: &[u8], failed: bool) -> Vec<u8> {
    let mut framed = encode_guard(guard, GuardKind::Begin);
    framed.extend_from_slice(output);
    if !output.is_empty() && !output.ends_with(b"\n") {
        framed.push(b'\n');
    }
    framed.extend_from_slice(&encode_guard(
        guard,
        if failed {
            GuardKind::Error
        } else {
            GuardKind::End
        },
    ));
    framed
}
