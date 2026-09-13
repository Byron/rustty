//! Kitty OSC 72 drag-and-drop state, independent of an OS drag session.
//!
//! The host advertises MIME types during a drag and captures their data at
//! drop time. Data stays here until the client concludes, unregisters, or a
//! new drag enters. Like Ghostty, clients are treated as local and drag-out
//! and remote file transfer requests are refused. Replies echo BEL or ST;
//! host-initiated events always use ST.

use crate::Effect;
use base64::{Engine, engine::general_purpose::STANDARD};

pub const MAX_MIME_LIST_BYTES: usize = 1024 * 1024;
pub const MAX_ITEMS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventType {
    Register,
    Unregister,
    Status,
    Drop,
    Request,
    RequestError,
    Offer,
    Present,
    StartDrag,
    DragEvent,
    DragError,
    RemoteData,
    Query,
}

/// Metadata of one command; coordinates preserve Kitty's wrapping i32 cast.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub event_type: Option<EventType>,
    pub more: bool,
    pub client_id: u32,
    pub operation: u32,
    pub cell_x: i32,
    pub cell_y: i32,
    pub pixel_x: i32,
    pub pixel_y: i32,
}

impl Metadata {
    /// Reject the entire command on malformed or unknown metadata.
    pub fn parse(raw: &[u8]) -> Option<Self> {
        let mut metadata = Self::default();
        if raw.is_empty() {
            return Some(metadata);
        }
        // A trailing separator is accepted, but empty interior fields are not.
        for field in raw.strip_suffix(b":").unwrap_or(raw).split(|&b| b == b':') {
            if field.len() < 3 || field[1] != b'=' {
                return None;
            }
            let value = &field[2..];
            match field[0] {
                b't' => {
                    metadata.event_type = Some(match value {
                        b"a" => EventType::Register,
                        b"A" => EventType::Unregister,
                        b"m" => EventType::Status,
                        b"M" => EventType::Drop,
                        b"r" => EventType::Request,
                        b"R" => EventType::RequestError,
                        b"o" => EventType::Offer,
                        b"p" => EventType::Present,
                        b"P" => EventType::StartDrag,
                        b"e" => EventType::DragEvent,
                        b"E" => EventType::DragError,
                        b"k" => EventType::RemoteData,
                        b"q" => EventType::Query,
                        _ => return None,
                    });
                }
                b'm' => metadata.more = unsigned(value)? != 0,
                b'i' => metadata.client_id = unsigned(value)?,
                b'o' => metadata.operation = unsigned(value)?,
                b'x' | b'y' | b'X' | b'Y' => {
                    let negative = value.starts_with(b"-");
                    let magnitude = unsigned(if negative { &value[1..] } else { value })? as i32;
                    let value = if negative {
                        magnitude.wrapping_neg()
                    } else {
                        magnitude
                    };
                    match field[0] {
                        b'x' => metadata.cell_x = value,
                        b'y' => metadata.cell_y = value,
                        b'X' => metadata.pixel_x = value,
                        b'Y' => metadata.pixel_y = value,
                        _ => unreachable!(),
                    }
                }
                _ => return None,
            }
        }
        Some(metadata)
    }
}

fn unsigned(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || bytes.len() > 10 {
        return None;
    }
    bytes.iter().try_fold(0_u32, |number, &byte| {
        byte.is_ascii_digit()
            .then_some(u32::from(byte.wrapping_sub(b'0')))
            .and_then(|digit| number.checked_mul(10)?.checked_add(digit))
    })
}

/// Only the first chunk supplies metadata; continuations update `more`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Chunking {
    pub active: bool,
    pub metadata: Metadata,
}

impl Chunking {
    fn apply(&mut self, metadata: Metadata) -> Metadata {
        if self.active {
            self.active = metadata.more;
            return Metadata {
                more: metadata.more,
                ..self.metadata
            };
        }
        if metadata.more {
            self.active = true;
            self.metadata = metadata;
        }
        metadata
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Operation {
    #[default]
    None = 0,
    Copy = 1,
    Move = 2,
}

impl Operation {
    fn from_protocol(value: u32) -> Self {
        match value {
            1 => Self::Copy,
            2 => Self::Move,
            _ => Self::None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Operations {
    pub copy: bool,
    pub move_: bool,
}

impl Operations {
    pub fn protocol_value(self) -> u8 {
        u8::from(self.copy) | (u8::from(self.move_) << 1)
    }
}

/// The host reads registration and acceptance details from `Terminal::kitty_dnd`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Registration,
    Acceptance,
    ConcludedNone,
    ConcludedCopy,
    ConcludedMove,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub mime: Vec<u8>,
    pub data: Vec<u8>,
}

/// MIME indices follow this order; every advertised entry has a trailing space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offered {
    pub mimes: Vec<Vec<u8>>,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DropTarget {
    pub client_id: u32,
    /// Space-separated bytes, retained for OS type registration.
    pub registered_mimes: Vec<u8>,
    pub hovered: bool,
    pub dropped: bool,
    pub accepted: Option<Operation>,
    pub accept_in_progress: bool,
    /// Space-separated during accumulation, NUL-separated once complete.
    pub accepted_mimes: Vec<u8>,
    pub offered: Option<Offered>,
    pub items: Option<Vec<Item>>,
}

/// Host methods require the same terminal lock used when feeding client output.
/// Write their returned protocol bytes to the PTY.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub chunking: Chunking,
    pub drop: DropTarget,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MoveEvent {
    /// Zero-based grid coordinates.
    pub cell_x: u32,
    pub cell_y: u32,
    /// Pixels relative to the terminal content area's top-left corner.
    pub pixel_x: i32,
    pub pixel_y: i32,
    pub operations: Operations,
}

impl State {
    pub fn registered_mimes(&self) -> impl Iterator<Item = &[u8]> {
        self.drop
            .registered_mimes
            .split(|&byte| byte == b' ')
            .filter(|mime| !mime.is_empty())
    }

    /// Unanswered while status chunks arrive; `Some(Operation::None)` is rejection.
    pub fn client_accepted(&self) -> Option<Operation> {
        if self.drop.accept_in_progress {
            None
        } else {
            self.drop.accepted
        }
    }

    pub fn drag_move(&mut self, event: MoveEvent, mimes: &[&[u8]]) -> Vec<u8> {
        self.move_event(event, mimes, false)
    }

    /// Capture at most 16 representations, keeping advertised and held data aligned.
    pub fn drag_drop(&mut self, event: MoveEvent, items: &[Item]) -> Vec<u8> {
        let copies = items[..items.len().min(MAX_ITEMS)].to_vec();
        let mimes: Vec<_> = copies.iter().map(|item| item.mime.as_slice()).collect();
        let response = self.move_event(event, &mimes, true);
        self.drop.items = Some(copies);
        response
    }

    /// A leave after dropping is ignored so toolkits cannot discard held data.
    pub fn drag_leave(&mut self) -> Vec<u8> {
        if self.drop.dropped {
            return Vec::new();
        }
        let hovered = self.drop.hovered;
        self.drop.hovered = false;
        self.drop.offered = None;
        if hovered {
            encode("t=m:x=-1:y=-1", self.drop.client_id, b"", false, false)
        } else {
            Vec::new()
        }
    }

    fn move_event(&mut self, event: MoveEvent, mimes: &[&[u8]], is_drop: bool) -> Vec<u8> {
        if !self.drop.hovered {
            self.reset_drop();
            self.drop.hovered = true;
        }
        if is_drop {
            self.drop.dropped = true;
            self.drop.hovered = false;
        }
        if self.drop.offered.as_ref().is_none_or(|offered| {
            !offered
                .mimes
                .iter()
                .map(Vec::as_slice)
                .eq(mimes.iter().copied())
        }) {
            let mut payload = Vec::new();
            for mime in mimes {
                payload.extend_from_slice(mime);
                payload.push(b' ');
            }
            self.drop.offered = Some(Offered {
                mimes: mimes.iter().map(|mime| mime.to_vec()).collect(),
                payload,
            });
        }
        encode(
            &format!(
                "t={}:x={}:y={}:X={}:Y={}:o={}",
                if is_drop { 'M' } else { 'm' },
                event.cell_x,
                event.cell_y,
                event.pixel_x,
                event.pixel_y,
                event.operations.protocol_value(),
            ),
            self.drop.client_id,
            &self.drop.offered.as_ref().unwrap().payload,
            false,
            false,
        )
    }

    fn reset_drop(&mut self) {
        self.drop = DropTarget {
            client_id: self.drop.client_id,
            registered_mimes: std::mem::take(&mut self.drop.registered_mimes),
            ..DropTarget::default()
        };
    }

    fn register(&mut self, payload: &[u8], continuation: bool, more: bool) -> Option<Event> {
        let list = &mut self.drop.registered_mimes;
        if !continuation {
            list.clear();
        }
        if payload.len() > MAX_MIME_LIST_BYTES.saturating_sub(list.len()) {
            return None;
        }
        list.extend_from_slice(payload);
        (!more).then_some(Event::Registration)
    }

    fn accept_status(&mut self, metadata: Metadata, payload: &[u8]) -> Option<Event> {
        let drop = &mut self.drop;
        if !drop.accept_in_progress {
            drop.accepted_mimes.clear();
            drop.accept_in_progress = true;
            drop.accepted = Some(Operation::from_protocol(metadata.operation));
        }
        // An oversized chunk leaves the acceptance unanswered, matching Kitty.
        if payload.len() > MAX_MIME_LIST_BYTES.saturating_sub(drop.accepted_mimes.len()) {
            return None;
        }
        drop.accepted_mimes.extend_from_slice(payload);
        if metadata.more {
            return None;
        }
        drop.accept_in_progress = false;
        if !drop.accepted_mimes.is_empty() {
            for byte in &mut drop.accepted_mimes {
                if *byte == b' ' {
                    *byte = 0;
                }
            }
            drop.accepted_mimes.push(0);
        }
        Some(Event::Acceptance)
    }
}

pub(crate) fn handle(slot: &mut Option<State>, data: &[u8], bell: bool, effects: &mut Vec<Effect>) {
    let split = data
        .iter()
        .position(|&byte| byte == b';')
        .unwrap_or(data.len());
    let Some(raw) = Metadata::parse(&data[..split]) else {
        return;
    };
    let continuation = slot.as_ref().is_some_and(|state| state.chunking.active);
    let metadata = slot.as_mut().map_or(raw, |state| state.chunking.apply(raw));
    let payload = data.get(split + 1..).unwrap_or_default();
    let Some(event_type) = metadata.event_type else {
        return;
    };
    let event = match event_type {
        EventType::Register => {
            // Machine IDs are accepted and ignored; no remote marker is sent.
            if metadata.cell_x == 1 {
                return;
            }
            let state = slot.get_or_insert_with(|| {
                let mut state = State::default();
                state.chunking.apply(raw);
                state
            });
            state.drop.client_id = metadata.client_id;
            state.register(payload, continuation, metadata.more)
        }
        EventType::Unregister => slot.take().map(|_| Event::Registration),
        EventType::Status => slot
            .as_mut()
            .and_then(|state| state.accept_status(metadata, payload)),
        EventType::Request => data_request(slot, metadata, bell, effects),
        EventType::Query => {
            effects.push(Effect::Write(encode(
                "t=q",
                metadata.client_id,
                b"",
                false,
                bell,
            )));
            None
        }
        EventType::Offer if metadata.cell_x != 0 => None,
        EventType::Offer | EventType::Present | EventType::StartDrag => {
            effects.push(Effect::Write(encode(
                "t=E",
                metadata.client_id,
                b"EPERM:drag out is not supported by this terminal",
                false,
                bell,
            )));
            None
        }
        EventType::Drop
        | EventType::RequestError
        | EventType::DragEvent
        | EventType::DragError
        | EventType::RemoteData => None,
    };
    if let Some(event) = event {
        effects.push(Effect::DragAndDrop(event));
    }
}

fn data_request(
    slot: &mut Option<State>,
    metadata: Metadata,
    bell: bool,
    effects: &mut Vec<Effect>,
) -> Option<Event> {
    let client_id = slot.as_ref().map_or(0, |state| state.drop.client_id);
    let mut keys = String::new();
    if metadata.cell_x != 0 {
        keys.push_str(&format!(":x={}", metadata.cell_x));
    }
    // Directory requests take precedence over URI requests, then MIME requests.
    if metadata.pixel_y != 0 || metadata.cell_y != 0 {
        if metadata.pixel_y != 0 {
            keys.push_str(&format!(":Y={}", metadata.pixel_y));
        } else {
            keys.push_str(&format!(":y={}", metadata.cell_y));
        }
        effects.push(Effect::Write(encode(
            &format!("t=R{keys}"),
            client_id,
            b"EINVAL:remote drop data is not supported",
            false,
            bell,
        )));
    } else if metadata.cell_x != 0 {
        let items = slot.as_ref().and_then(|state| state.drop.items.as_ref());
        let item = items.and_then(|items| {
            usize::try_from(metadata.cell_x)
                .ok()
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| items.get(index))
        });
        let response = if let Some(item) = item {
            let header = format!("t=r{keys}");
            let mut response = if item.data.is_empty() {
                Vec::new()
            } else {
                encode(&header, client_id, &item.data, true, bell)
            };
            response.extend(encode(&header, client_id, b"", true, bell));
            response
        } else {
            encode(
                &format!("t=R{keys}"),
                client_id,
                if items.is_some() {
                    b"ENOENT:drop data request index out of bounds"
                } else {
                    b"ENOENT:no drop data available"
                },
                false,
                bell,
            )
        };
        effects.push(Effect::Write(response));
    } else if let Some(state) = slot {
        let dropped = state.drop.dropped;
        state.reset_drop();
        return dropped.then_some(match Operation::from_protocol(metadata.operation) {
            Operation::None => Event::ConcludedNone,
            Operation::Copy => Event::ConcludedCopy,
            Operation::Move => Event::ConcludedMove,
        });
    }
    None
}

fn encode(header: &str, client_id: u32, data: &[u8], base64: bool, bell: bool) -> Vec<u8> {
    let mut prefix = format!("\x1b]72;{header}");
    if client_id != 0 {
        prefix.push_str(&format!(":i={client_id}"));
    }
    let terminator: &[u8] = if bell { b"\x07" } else { b"\x1b\\" };
    let mut response = Vec::new();
    if data.is_empty() {
        response.extend_from_slice(prefix.as_bytes());
        response.extend_from_slice(terminator);
        return response;
    }
    let limit = if base64 { 3072 } else { 4096 };
    let mut chunks = data.chunks(limit).peekable();
    while let Some(chunk) = chunks.next() {
        response.extend_from_slice(prefix.as_bytes());
        response.extend_from_slice(if chunks.peek().is_some() {
            b":m=1;"
        } else {
            b":m=0;"
        });
        if base64 {
            let mut buffer = [0; 4096];
            let length = STANDARD.encode_slice(chunk, &mut buffer).unwrap();
            response.extend_from_slice(&buffer[..length]);
        } else {
            response.extend_from_slice(chunk);
        }
        response.extend_from_slice(terminator);
    }
    response
}
