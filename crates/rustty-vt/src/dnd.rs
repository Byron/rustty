//! Kitty OSC 72 drag-and-drop state, independent of an OS drag session.
//!
//! The host advertises MIME types during a drag and captures their data at
//! drop time. Data stays here until the client concludes, unregisters, or a
//! new drag enters. Like Ghostty, clients are treated as local and drag-out
//! and remote file transfer requests are refused. Replies echo BEL or ST;
//! host-initiated events always use ST.

use crate::Effect;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

pub const MAX_MIME_LIST_BYTES: usize = 1024 * 1024;
pub const MAX_ITEMS: usize = 16;
/// Keeps encoded requests and base64 PTY replies below their 16 MiB queues.
pub const MAX_DROP_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_HOST_MIME_BYTES: usize = 255;
/// One complete fetch also fits the owner's bounded PTY reply queue when pipelined.
pub const MAX_DROP_SERVED_BYTES: usize = MAX_DROP_BYTES;
pub const MAX_DROP_REQUESTS: usize = 64;

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    #[serde(
        serialize_with = "serde_bytes::serialize",
        deserialize_with = "deserialize_mime"
    )]
    pub mime: Vec<u8>,
    #[serde(
        serialize_with = "serde_bytes::serialize",
        deserialize_with = "deserialize_drop_data"
    )]
    pub data: Vec<u8>,
}

/// Retained host metadata. Dropped data is never copied into a view or snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub revision: u64,
    pub registration: u64,
    pub client_id: u32,
    /// False while registration chunks arrive. Empty MIME lists still register.
    pub ready: bool,
    #[serde(
        serialize_with = "serde_bytes::serialize",
        deserialize_with = "deserialize_registered_mimes"
    )]
    pub registered_mimes: Vec<u8>,
    pub offer: Option<u64>,
    pub dropped: bool,
    pub accepted: Option<Operation>,
    /// NUL-separated exact names in preference order; empty means all offered.
    #[serde(
        serialize_with = "serde_bytes::serialize",
        deserialize_with = "deserialize_accepted_mimes"
    )]
    pub accepted_mimes: Vec<u8>,
}

impl View {
    pub fn accepts_mime(&self, mime: &[u8]) -> bool {
        accepts_mime(&self.accepted_mimes, mime)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    pub registration: u64,
    pub offer: u64,
    pub action: Action,
}

impl Command {
    /// Validate limits before a host serializes an in-memory command.
    pub fn validate(&self) -> Result<(), HostError> {
        if self.registration == 0 {
            return Err(HostError::StaleRegistration);
        }
        if self.offer == 0 {
            return Err(HostError::StaleOffer);
        }
        let mimes = match &self.action {
            Action::Move { event, mimes } => {
                validate_position(*event)?;
                if mimes.len() > MAX_ITEMS {
                    return Err(HostError::TooLarge);
                }
                mimes.iter().map(Vec::as_slice).collect::<Vec<_>>()
            }
            Action::Drop { event, items } => {
                validate_position(*event)?;
                if items.len() > MAX_ITEMS
                    || items
                        .iter()
                        .try_fold(0usize, |sum, item| sum.checked_add(item.data.len()))
                        .is_none_or(|total| total > MAX_DROP_BYTES)
                {
                    return Err(HostError::TooLarge);
                }
                items
                    .iter()
                    .map(|item| item.mime.as_slice())
                    .collect::<Vec<_>>()
            }
            Action::Leave => return Ok(()),
        };
        validate_mimes(&mimes)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Move {
        event: MoveEvent,
        #[serde(deserialize_with = "deserialize_mimes")]
        mimes: Vec<Vec<u8>>,
    },
    Drop {
        event: MoveEvent,
        #[serde(deserialize_with = "deserialize_items")]
        items: Vec<Item>,
    },
    Leave,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostError {
    StaleRegistration,
    StaleOffer,
    InvalidMime,
    TooLarge,
    NotAccepted,
    InvalidPosition,
}

impl std::fmt::Display for HostError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(match self {
            Self::StaleRegistration => "drag registration changed or is incomplete",
            Self::StaleOffer => "drag offer changed or has ended",
            Self::InvalidMime => "invalid or duplicate drag MIME type",
            Self::TooLarge => "drag data exceeds the host limit",
            Self::NotAccepted => "drop has not been accepted for copying",
            Self::InvalidPosition => "drag position is outside the content area",
        })
    }
}

impl std::error::Error for HostError {}

fn bounded_bytes<'de, D: serde::Deserializer<'de>, const LIMIT: usize>(
    deserializer: D,
) -> Result<Vec<u8>, D::Error> {
    struct Bytes<const LIMIT: usize>;
    impl<'de, const LIMIT: usize> serde::de::Visitor<'de> for Bytes<LIMIT> {
        type Value = Vec<u8>;
        fn expecting(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(out, "at most {LIMIT} bytes")
        }
        fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Vec<u8>, E> {
            if bytes.len() > LIMIT {
                return Err(E::custom("drag byte limit exceeded"));
            }
            Ok(bytes.to_vec())
        }
        fn visit_byte_buf<E: serde::de::Error>(self, bytes: Vec<u8>) -> Result<Vec<u8>, E> {
            if bytes.len() > LIMIT {
                return Err(E::custom("drag byte limit exceeded"));
            }
            Ok(bytes)
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<u8>, A::Error> {
            use serde::de::Error;
            if seq.size_hint().is_some_and(|size| size > LIMIT) {
                return Err(A::Error::custom("drag byte limit exceeded"));
            }
            let mut bytes = Vec::with_capacity(seq.size_hint().unwrap_or(0));
            while let Some(byte) = seq.next_element()? {
                if bytes.len() == LIMIT {
                    return Err(A::Error::custom("drag byte limit exceeded"));
                }
                bytes.push(byte);
            }
            Ok(bytes)
        }
    }
    deserializer.deserialize_bytes(Bytes::<LIMIT>)
}

fn deserialize_mime<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<u8>, D::Error> {
    bounded_bytes::<D, MAX_HOST_MIME_BYTES>(deserializer)
}

fn deserialize_drop_data<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<u8>, D::Error> {
    bounded_bytes::<D, MAX_DROP_BYTES>(deserializer)
}

fn deserialize_registered_mimes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<u8>, D::Error> {
    bounded_bytes::<D, MAX_MIME_LIST_BYTES>(deserializer)
}

fn deserialize_accepted_mimes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<u8>, D::Error> {
    bounded_bytes::<D, { MAX_MIME_LIST_BYTES + 1 }>(deserializer)
}

fn bounded_items<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Items<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Items<T> {
        type Value = Vec<T>;
        fn expecting(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(out, "at most {MAX_ITEMS} drag representations")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
            use serde::de::Error;
            if seq.size_hint().is_some_and(|size| size > MAX_ITEMS) {
                return Err(A::Error::custom("too many drag representations"));
            }
            let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
            while items.len() < MAX_ITEMS {
                let Some(item) = seq.next_element()? else {
                    return Ok(items);
                };
                items.push(item);
            }
            if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                return Err(A::Error::custom("too many drag representations"));
            }
            Ok(items)
        }
    }
    deserializer.deserialize_seq(Items(std::marker::PhantomData))
}

fn deserialize_items<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Item>, D::Error> {
    bounded_items(deserializer)
}

fn deserialize_mimes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Vec<u8>>, D::Error> {
    #[derive(Deserialize)]
    struct Mime(#[serde(deserialize_with = "deserialize_mime")] Vec<u8>);
    Ok(bounded_items::<D, Mime>(deserializer)?
        .into_iter()
        .map(|mime| mime.0)
        .collect())
}

fn revision() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("drag revisions exhausted")
}

fn accepts_mime(accepted: &[u8], mime: &[u8]) -> bool {
    accepted.is_empty() || accepted.split(|&byte| byte == 0).any(|item| item == mime)
}

pub fn valid_host_mime(mime: &[u8]) -> bool {
    !mime.is_empty()
        && mime.len() <= MAX_HOST_MIME_BYTES
        && mime.iter().all(|byte| matches!(byte, 0x21..=0x7e))
}

fn validate_mimes(mimes: &[&[u8]]) -> Result<(), HostError> {
    if mimes.len() > MAX_ITEMS {
        return Err(HostError::TooLarge);
    }
    for (index, mime) in mimes.iter().enumerate() {
        if !valid_host_mime(mime) || mimes[..index].contains(mime) {
            return Err(HostError::InvalidMime);
        }
    }
    Ok(())
}

fn validate_position(event: MoveEvent) -> Result<(), HostError> {
    if event.cell_x > i32::MAX as u32
        || event.cell_y > i32::MAX as u32
        || event.pixel_x < 0
        || event.pixel_y < 0
    {
        Err(HostError::InvalidPosition)
    } else {
        Ok(())
    }
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
    revision: u64,
    registration: u64,
    ready: bool,
    registration_failed: bool,
    offer: Option<u64>,
    operations: Operations,
    acceptance_failed: bool,
    requests: usize,
    served_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn view(&self) -> View {
        View {
            revision: self.revision,
            registration: self.registration,
            client_id: self.drop.client_id,
            ready: self.ready,
            registered_mimes: self.drop.registered_mimes.clone(),
            offer: self.offer,
            dropped: self.drop.dropped,
            accepted: self.client_accepted(),
            accepted_mimes: if self.drop.accept_in_progress {
                Vec::new()
            } else {
                self.drop.accepted_mimes.clone()
            },
        }
    }

    /// The caller also validates pane/connection ownership and grid bounds.
    /// Only this guarded native path accepts Copy; the lower-level API retains
    /// the complete protocol's Move behavior for headless consumers.
    pub fn apply_host(&mut self, command: Command) -> Result<Vec<u8>, HostError> {
        if !self.ready || command.registration != self.registration {
            return Err(HostError::StaleRegistration);
        }
        command.validate()?;
        match command.action {
            Action::Move { event, mimes } => {
                let mimes = mimes.iter().map(Vec::as_slice).collect::<Vec<_>>();
                if self.offer != Some(command.offer) {
                    self.reset_drop();
                    self.offer = Some(command.offer);
                    self.drop.hovered = true;
                } else if self.drop.dropped {
                    return Err(HostError::StaleOffer);
                }
                Ok(self.drag_move(event, &mimes))
            }
            Action::Drop { event, items } => {
                if self.offer != Some(command.offer) || !self.drop.hovered || self.drop.dropped {
                    return Err(HostError::StaleOffer);
                }
                let mimes = items
                    .iter()
                    .map(|item| item.mime.as_slice())
                    .collect::<Vec<_>>();
                if !event.operations.copy
                    || !self.operations.copy
                    || self.client_accepted() != Some(Operation::Copy)
                    || items.is_empty()
                    || !mimes.iter().all(|mime| {
                        self.drop.offered.as_ref().is_some_and(|offered| {
                            offered.mimes.iter().any(|offered| offered == mime)
                        }) && accepts_mime(&self.drop.accepted_mimes, mime)
                    })
                {
                    return Err(HostError::NotAccepted);
                }
                let bytes = self.move_event(event, &mimes, true);
                self.drop.items = Some(items);
                Ok(bytes)
            }
            Action::Leave => {
                if self.offer != Some(command.offer) {
                    return Err(HostError::StaleOffer);
                }
                Ok(self.drag_leave())
            }
        }
    }

    /// Disconnect cancels a hover, while committed data belongs to the owner
    /// until the client concludes. A stale connection cannot cancel a later offer.
    pub fn cancel_offer(&mut self, offer: u64) -> Vec<u8> {
        if self.offer != Some(offer) || self.drop.dropped {
            return Vec::new();
        }
        self.drag_leave()
    }

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
        self.reset_drop();
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
            self.revision = revision();
        }
        let changed_mimes = self.drop.offered.as_ref().is_none_or(|offered| {
            !offered
                .mimes
                .iter()
                .map(Vec::as_slice)
                .eq(mimes.iter().copied())
        });
        if changed_mimes || self.operations != event.operations {
            self.invalidate_acceptance();
            self.operations = event.operations;
        }
        if changed_mimes {
            let mut payload = Vec::new();
            for mime in mimes {
                payload.extend_from_slice(mime);
                payload.push(b' ');
            }
            self.drop.offered = Some(Offered {
                mimes: mimes.iter().map(|mime| mime.to_vec()).collect(),
                payload,
            });
            self.revision = revision();
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
            if changed_mimes || is_drop {
                &self.drop.offered.as_ref().unwrap().payload
            } else {
                b""
            },
            false,
            false,
        )
    }

    fn reset_drop(&mut self) {
        self.invalidate_acceptance();
        self.drop = DropTarget {
            client_id: self.drop.client_id,
            registered_mimes: std::mem::take(&mut self.drop.registered_mimes),
            ..DropTarget::default()
        };
        self.offer = None;
        self.operations = Operations::default();
        self.requests = 0;
        self.served_bytes = 0;
        self.revision = revision();
    }

    fn invalidate_acceptance(&mut self) {
        self.drop.accepted = None;
        self.drop.accept_in_progress = false;
        self.drop.accepted_mimes.clear();
        self.acceptance_failed = false;
        if self.chunking.active && self.chunking.metadata.event_type == Some(EventType::Status) {
            self.chunking = Chunking::default();
        }
        self.revision = revision();
    }

    fn register(&mut self, payload: &[u8], continuation: bool, more: bool) -> Option<Event> {
        if !continuation {
            self.reset_drop();
            self.drop.registered_mimes.clear();
            self.registration = revision();
            self.ready = false;
            self.registration_failed = false;
        }
        let list = &mut self.drop.registered_mimes;
        if payload.len() > MAX_MIME_LIST_BYTES.saturating_sub(list.len()) {
            self.registration_failed = true;
            return None;
        }
        if self.registration_failed {
            return None;
        }
        list.extend_from_slice(payload);
        self.ready = !more;
        self.revision = revision();
        (!more).then_some(Event::Registration)
    }

    fn accept_status(
        &mut self,
        metadata: Metadata,
        payload: &[u8],
        continuation: bool,
    ) -> Option<Event> {
        let drop = &mut self.drop;
        if !continuation {
            drop.accepted_mimes.clear();
            drop.accept_in_progress = true;
            drop.accepted = Some(Operation::from_protocol(metadata.operation));
            self.acceptance_failed = false;
            self.revision = revision();
        }
        // An oversized chunk leaves the acceptance unanswered, matching Kitty.
        if payload.len() > MAX_MIME_LIST_BYTES.saturating_sub(drop.accepted_mimes.len()) {
            self.acceptance_failed = true;
            return None;
        }
        if self.acceptance_failed {
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
        self.revision = revision();
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
            .and_then(|state| state.accept_status(metadata, payload, continuation)),
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
    if metadata.cell_x == 0 && metadata.cell_y == 0 && metadata.pixel_y == 0 {
        if let Some(state) = slot {
            let dropped = state.drop.dropped;
            state.reset_drop();
            return dropped.then_some(match Operation::from_protocol(metadata.operation) {
                Operation::None => Event::ConcludedNone,
                Operation::Copy => Event::ConcludedCopy,
                Operation::Move => Event::ConcludedMove,
            });
        }
        return None;
    }
    let mut keys = String::new();
    if metadata.cell_x != 0 {
        keys.push_str(&format!(":x={}", metadata.cell_x));
    }
    // Directory requests take precedence over URI requests, then MIME requests.
    if metadata.pixel_y != 0 {
        keys.push_str(&format!(":Y={}", metadata.pixel_y));
    } else if metadata.cell_y != 0 {
        keys.push_str(&format!(":y={}", metadata.cell_y));
    }
    let dropped = slot.as_ref().is_some_and(|state| state.drop.dropped);
    let error: &[u8] = if !dropped {
        b"EPERM:drop data can only be requested after a drop"
    } else if metadata.pixel_y != 0 || metadata.cell_y != 0 {
        b"EINVAL:remote drop data is not supported"
    } else {
        let state = slot.as_mut().unwrap();
        let item = (|| {
            let items = state.drop.items.as_ref()?;
            let index = usize::try_from(metadata.cell_x).ok()?.checked_sub(1)?;
            items.get(index)
        })();
        if let Some(item) = item {
            if state.requests >= MAX_DROP_REQUESTS
                || state
                    .served_bytes
                    .checked_add(item.data.len())
                    .is_none_or(|total| total > MAX_DROP_SERVED_BYTES)
            {
                b"EMFILE:drop data request limit exceeded"
            } else {
                state.requests += 1;
                state.served_bytes += item.data.len();
                let header = format!("t=r{keys}");
                let mut response = if item.data.is_empty() {
                    Vec::new()
                } else {
                    encode(&header, client_id, &item.data, true, bell)
                };
                response.extend(encode(&header, client_id, b"", true, bell));
                effects.push(Effect::Write(response));
                return None;
            }
        } else {
            b"ENOENT:drop data request index out of bounds"
        }
    };
    effects.push(Effect::Write(encode(
        &format!("t=R{keys}"),
        client_id,
        error,
        false,
        bell,
    )));
    if dropped {
        slot.as_mut().unwrap().reset_drop();
        Some(Event::ConcludedNone)
    } else {
        None
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_sequence_hints_are_rejected_before_visiting_or_allocating_items() {
        struct Forged;
        impl Iterator for Forged {
            type Item = u8;
            fn next(&mut self) -> Option<u8> {
                panic!("forged length must be rejected first")
            }
            fn size_hint(&self) -> (usize, Option<usize>) {
                (usize::MAX, Some(usize::MAX))
            }
        }
        type Error = serde::de::value::Error;
        assert!(
            deserialize_drop_data(serde::de::value::SeqDeserializer::<_, Error>::new(Forged))
                .is_err()
        );
        assert!(
            deserialize_mime(serde::de::value::SeqDeserializer::<_, Error>::new(Forged)).is_err()
        );
        assert!(
            deserialize_registered_mimes(serde::de::value::SeqDeserializer::<_, Error>::new(
                Forged
            ))
            .is_err()
        );
        assert!(
            deserialize_accepted_mimes(serde::de::value::SeqDeserializer::<_, Error>::new(Forged))
                .is_err()
        );
        assert!(
            bounded_items::<_, u8>(serde::de::value::SeqDeserializer::<_, Error>::new(Forged))
                .is_err()
        );
        let bytes = vec![0; MAX_DROP_BYTES + 1];
        assert!(
            deserialize_drop_data(serde::de::value::BorrowedBytesDeserializer::<Error>::new(
                &bytes
            ))
            .is_err()
        );
    }
}
