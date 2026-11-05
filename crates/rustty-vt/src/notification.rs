//! Bounded OSC 99 assembly. Native notification lifetime and delivery stay with the host.

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use serde::{
    Deserialize, Serialize,
    de::{self, SeqAccess, Visitor},
};
use std::fmt;

pub const MAX_IDENTIFIER_BYTES: usize = 128;
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_BUTTONS: usize = 3;
pub const MAX_BUTTON_BYTES: usize = 1024;
pub const MAX_ALIVE: usize = 128;
const MAX_METADATA_BYTES: usize = 4096;
const MAX_METADATA_FIELDS: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub buttons: bool,
    pub close_events: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    #[serde(default, deserialize_with = "optional_identifier")]
    pub identifier: Option<String>,
    #[serde(deserialize_with = "message_text")]
    pub title: String,
    #[serde(deserialize_with = "message_text")]
    pub body: String,
    #[serde(deserialize_with = "buttons")]
    pub buttons: Vec<String>,
    pub focus: bool,
    pub report: bool,
    pub close_report: bool,
    #[serde(default = "default_sound")]
    pub sound: bool,
}

fn default_sound() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    /// Monotonic for this terminal owner, including across RIS. Hosts additionally
    /// bind callbacks to the owner incarnation and originating pane.
    pub serial: u64,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Show(Message),
    Close {
        #[serde(deserialize_with = "identifier")]
        identifier: String,
    },
    Alive {
        #[serde(deserialize_with = "identifier")]
        identifier: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reply {
    /// Zero activates the notification; 1..=buttons.len() activates a button.
    /// Activation retires the notification, so requested close reports follow it.
    Activated {
        button: u8,
    },
    Closed,
    /// The host retains this notification for activation and alive queries.
    Untracked,
    Failed,
    /// Release a replaced Show or acknowledge Close without a wire response.
    Finished,
    Alive {
        #[serde(deserialize_with = "identifiers")]
        identifiers: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplyError;

impl fmt::Display for ReplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid notification command or reply")
    }
}
impl std::error::Error for ReplyError {}

impl Command {
    /// Check in-memory callers before serialization and decoded commands before use.
    pub fn validate(&self) -> Result<(), ReplyError> {
        if self.serial == 0 {
            return Err(ReplyError);
        }
        let valid = match &self.action {
            Action::Close { identifier } | Action::Alive { identifier } => valid_id(identifier),
            Action::Show(message) => {
                message.identifier.as_deref().is_none_or(valid_id)
                    && !message.title.is_empty()
                    && message.buttons.len() <= MAX_BUTTONS
                    && message
                        .buttons
                        .iter()
                        .all(|s| !s.is_empty() && s.len() <= MAX_BUTTON_BYTES)
                    && [&message.title, &message.body]
                        .into_iter()
                        .chain(&message.buttons)
                        .all(|s| s.len() <= MAX_MESSAGE_BYTES && s.chars().all(display_char))
                    && message
                        .title
                        .len()
                        .saturating_add(message.body.len())
                        .saturating_add(message.buttons.iter().map(String::len).sum::<usize>())
                        <= MAX_MESSAGE_BYTES
            }
        };
        if valid { Ok(()) } else { Err(ReplyError) }
    }

    /// Encode only replies valid for this original command. No caller-supplied
    /// string can become protocol metadata without identifier validation.
    pub fn reply(&self, reply: &Reply) -> Result<Vec<u8>, ReplyError> {
        self.validate()?;
        let mut bytes = Vec::new();
        match (&self.action, reply) {
            (Action::Show(message), Reply::Activated { button }) => {
                if usize::from(*button) > message.buttons.len() {
                    return Err(ReplyError);
                }
                let id = message.identifier.as_deref().unwrap_or("0");
                if message.report {
                    let payload = if *button == 0 {
                        String::new()
                    } else {
                        button.to_string()
                    };
                    encode(&mut bytes, id, "", &payload);
                }
                if message.close_report {
                    encode(&mut bytes, id, ":p=close", "");
                }
            }
            (Action::Show(message), Reply::Closed | Reply::Failed | Reply::Untracked) => {
                if message.close_report {
                    encode(
                        &mut bytes,
                        message.identifier.as_deref().unwrap_or("0"),
                        ":p=close",
                        if *reply == Reply::Untracked {
                            "untracked"
                        } else {
                            ""
                        },
                    );
                }
            }
            (Action::Show(_) | Action::Close { .. }, Reply::Finished)
            | (Action::Close { .. }, Reply::Failed) => {}
            (Action::Alive { identifier }, Reply::Alive { identifiers }) => {
                if identifiers.len() > MAX_ALIVE || !identifiers.iter().all(|id| valid_id(id)) {
                    return Err(ReplyError);
                }
                encode(&mut bytes, identifier, ":p=alive", &identifiers.join(","));
            }
            (Action::Alive { identifier }, Reply::Failed) => {
                encode(&mut bytes, identifier, ":p=alive", "")
            }
            _ => return Err(ReplyError),
        }
        Ok(bytes)
    }
}

fn encode(bytes: &mut Vec<u8>, identifier: &str, metadata: &str, payload: &str) {
    bytes.extend_from_slice(format!("\x1b]99;i={identifier}{metadata};{payload}\x1b\\").as_bytes());
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_IDENTIFIER_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-+.".contains(&b))
}

fn display_char(ch: char) -> bool {
    ch == '\n' || !matches!(ch, '\0'..='\u{1f}' | '\u{7f}'..='\u{9f}')
}
fn safe_char(ch: char) -> bool {
    ch != '\n' && display_char(ch)
}

fn bounded_string<'de, D: de::Deserializer<'de>, const LIMIT: usize>(
    deserializer: D,
) -> Result<String, D::Error> {
    struct Text<const N: usize>;
    impl<'de, const N: usize> Visitor<'de> for Text<N> {
        type Value = String;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "a string of at most {N} bytes")
        }
        fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
            if value.len() > N {
                Err(E::custom("notification string exceeds limit"))
            } else {
                Ok(value.to_owned())
            }
        }
        fn visit_string<E: de::Error>(self, value: String) -> Result<String, E> {
            if value.len() > N {
                Err(E::custom("notification string exceeds limit"))
            } else {
                Ok(value)
            }
        }
    }
    deserializer.deserialize_string(Text::<LIMIT>)
}
fn message_text<'de, D: de::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    bounded_string::<D, MAX_MESSAGE_BYTES>(d)
}
fn identifier<'de, D: de::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let id = bounded_string::<D, MAX_IDENTIFIER_BYTES>(d)?;
    if valid_id(&id) {
        Ok(id)
    } else {
        Err(de::Error::custom("invalid notification identifier"))
    }
}
fn optional_identifier<'de, D: de::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    struct Optional;
    impl<'de> Visitor<'de> for Optional {
        type Value = Option<String>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an optional notification identifier")
        }
        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            identifier(d).map(Some)
        }
    }
    d.deserialize_option(Optional)
}
fn string_list<'de, D: de::Deserializer<'de>, const COUNT: usize, const BYTES: usize>(
    d: D,
) -> Result<Vec<String>, D::Error> {
    struct List<const N: usize, const B: usize>;
    struct Text<const B: usize>(String);
    impl<'de, const B: usize> Deserialize<'de> for Text<B> {
        fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            bounded_string::<D, B>(d).map(Self)
        }
    }
    impl<'de, const N: usize, const B: usize> Visitor<'de> for List<N, B> {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "at most {N} notification strings")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            if seq.size_hint().is_some_and(|n| n > N) {
                return Err(de::Error::custom("too many notification strings"));
            }
            let mut result = Vec::new();
            while let Some(Text(value)) = seq.next_element::<Text<B>>()? {
                if result.len() == N {
                    return Err(de::Error::custom("too many notification strings"));
                }
                result.push(value);
            }
            Ok(result)
        }
    }
    d.deserialize_seq(List::<COUNT, BYTES>)
}
fn buttons<'de, D: de::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    string_list::<D, MAX_BUTTONS, MAX_BUTTON_BYTES>(d)
}
fn identifiers<'de, D: de::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let ids = string_list::<D, MAX_ALIVE, MAX_IDENTIFIER_BYTES>(d)?;
    if ids.iter().all(|id| valid_id(id)) {
        Ok(ids)
    } else {
        Err(de::Error::custom("invalid notification identifier"))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Payload {
    #[default]
    Title,
    Body,
    Buttons,
    Query,
    Close,
    Alive,
    Unknown,
}
impl Payload {
    fn index(self) -> Option<usize> {
        match self {
            Self::Title => Some(0),
            Self::Body => Some(1),
            Self::Buttons => Some(2),
            _ => None,
        }
    }
}

struct Header<'a> {
    id: Option<&'a str>,
    payload: Payload,
    encoded: bool,
    done: bool,
    focus: Option<bool>,
    report: Option<bool>,
    close: Option<bool>,
    sound: Option<bool>,
}
impl<'a> Header<'a> {
    fn parse(metadata: &'a [u8]) -> Option<Self> {
        if metadata.len() > MAX_METADATA_BYTES {
            return None;
        }
        let metadata = std::str::from_utf8(metadata).ok()?;
        let mut header = Self {
            id: None,
            payload: Payload::Title,
            encoded: false,
            done: true,
            focus: None,
            report: None,
            close: None,
            sound: None,
        };
        for (index, field) in metadata.split(':').enumerate() {
            if index == MAX_METADATA_FIELDS {
                return None;
            }
            if field.is_empty() {
                continue;
            }
            let (key, value) = field.split_once('=')?;
            if key.len() != 1
                || !key.as_bytes()[0].is_ascii_alphabetic()
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_/+.,(){}[]*&^%$#@!`~=?".contains(&b))
            {
                return None;
            }
            match key {
                "i" => {
                    if !value.is_empty() && !valid_id(value) {
                        return None;
                    }
                    header.id = (!value.is_empty()).then_some(value);
                }
                "p" => {
                    header.payload = match value {
                        "title" => Payload::Title,
                        "body" => Payload::Body,
                        "buttons" => Payload::Buttons,
                        "?" => Payload::Query,
                        "close" => Payload::Close,
                        "alive" => Payload::Alive,
                        _ => Payload::Unknown,
                    }
                }
                "e" => header.encoded = bit(value)?,
                "d" => header.done = bit(value)?,
                "c" => header.close = Some(bit(value)?),
                "a" => {
                    for action in value.split(',') {
                        match action {
                            "focus" => header.focus = Some(true),
                            "-focus" => header.focus = Some(false),
                            "report" => header.report = Some(true),
                            "-report" => header.report = Some(false),
                            _ => {}
                        }
                    }
                }
                "s" => {
                    if let Ok(value) = STANDARD
                        .decode(value)
                        .or_else(|_| STANDARD_NO_PAD.decode(value))
                    {
                        match value.as_slice() {
                            b"system" => header.sound = Some(true),
                            b"silent" => header.sound = Some(false),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        Some(header)
    }
}
fn bit(value: &str) -> Option<bool> {
    match value {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct State {
    serial: u64,
    pending: Option<Box<Pending>>,
}

#[derive(Clone, Debug)]
struct Pending {
    id: Option<String>,
    data: [Vec<u8>; 3],
    size: usize,
    current: Payload,
    quartet: [u8; 4],
    quartet_len: usize,
    focus: bool,
    report: bool,
    close: bool,
    sound: bool,
    poisoned: bool,
}
impl Pending {
    fn new(id: Option<&str>) -> Self {
        Self {
            id: id.map(str::to_owned),
            data: Default::default(),
            size: 0,
            current: Payload::Title,
            quartet: [0; 4],
            quartet_len: 0,
            focus: true,
            report: false,
            close: false,
            sound: true,
            poisoned: false,
        }
    }
    fn append(&mut self, bytes: &[u8]) -> bool {
        if bytes.len() > MAX_MESSAGE_BYTES.saturating_sub(self.size) {
            return false;
        }
        if let Some(index) = self.current.index() {
            self.data[index].extend_from_slice(bytes);
            self.size += bytes.len();
        }
        true
    }
    fn flush(&mut self) -> bool {
        if self.quartet_len == 0 {
            return true;
        }
        let mut decoded = [0; 3];
        let result = STANDARD_NO_PAD.decode_slice(&self.quartet[..self.quartet_len], &mut decoded);
        self.quartet_len = 0;
        result.is_ok_and(|len| self.append(&decoded[..len]))
    }
    fn payload(&mut self, kind: Payload, encoded: bool, data: &[u8]) -> bool {
        if (self.current != kind || !encoded) && !self.flush() {
            return false;
        }
        self.current = kind;
        if kind.index().is_none() {
            return true;
        }
        if !encoded {
            return std::str::from_utf8(data).is_ok_and(|text| text.chars().all(safe_char))
                && self.append(data);
        }
        for &byte in data {
            self.quartet[self.quartet_len] = byte;
            self.quartet_len += 1;
            if self.quartet_len == 4 {
                let mut decoded = [0; 3];
                let Ok(len) = STANDARD.decode_slice(self.quartet, &mut decoded) else {
                    return false;
                };
                self.quartet_len = 0;
                if !self.append(&decoded[..len]) {
                    return false;
                }
            }
        }
        true
    }
    fn finish(mut self, capabilities: Capabilities) -> Option<Message> {
        if self.poisoned || !self.flush() {
            return None;
        }
        let [title, body, button_data] = self.data;
        let mut title = String::from_utf8(title).ok()?;
        let mut body = String::from_utf8(body).ok()?;
        let mut button_data = String::from_utf8(button_data).ok()?;
        title.retain(display_char);
        body.retain(display_char);
        button_data.retain(display_char);
        if title.is_empty() {
            title = std::mem::take(&mut body);
        }
        if title.is_empty() {
            return None;
        }
        let mut buttons = Vec::new();
        if capabilities.buttons {
            for text in button_data.split('\u{2028}').filter(|s| !s.is_empty()) {
                if buttons.len() == MAX_BUTTONS || text.len() > MAX_BUTTON_BYTES {
                    return None;
                }
                buttons.push(text.to_owned());
            }
        }
        Some(Message {
            identifier: self.id,
            title,
            body,
            buttons,
            focus: self.focus,
            report: self.report,
            close_report: self.close,
            sound: self.sound,
        })
    }
}

impl State {
    pub(crate) fn clear_pending(&mut self) {
        self.pending = None;
    }
    fn command(&mut self, action: Action, effects: &mut Vec<crate::Effect>) {
        let Some(serial) = self.serial.checked_add(1) else {
            return;
        };
        self.serial = serial;
        effects.push(crate::Effect::DesktopNotification(Command {
            serial,
            action,
        }));
    }
    pub(crate) fn handle(
        &mut self,
        data: &[u8],
        capabilities: Option<Capabilities>,
        effects: &mut Vec<crate::Effect>,
    ) {
        let Some(capabilities) = capabilities else {
            self.clear_pending();
            return;
        };
        let Some(split) = data.iter().position(|&b| b == b';') else {
            self.poison();
            return;
        };
        let Some(header) = Header::parse(&data[..split]) else {
            self.poison();
            return;
        };
        let payload = &data[split + 1..];
        let valid_size = payload.len() <= if header.encoded { 4096 } else { 2048 };
        match header.payload {
            Payload::Query if valid_size => {
                let mut bytes = Vec::new();
                let types = if capabilities.buttons {
                    "title,body,buttons,close,alive,?"
                } else {
                    "title,body,close,alive,?"
                };
                encode(
                    &mut bytes,
                    header.id.unwrap_or("0"),
                    ":p=?",
                    &format!(
                        "a=focus,report:o=always:p={types}:s=system,silent{}",
                        if capabilities.close_events {
                            ":c=1"
                        } else {
                            ""
                        }
                    ),
                );
                effects.push(crate::Effect::Write(bytes));
                return;
            }
            Payload::Close | Payload::Alive if valid_size => {
                if let Some(id) = header.id {
                    let identifier = id.to_owned();
                    self.command(
                        if header.payload == Payload::Close {
                            Action::Close { identifier }
                        } else {
                            Action::Alive { identifier }
                        },
                        effects,
                    );
                }
                return;
            }
            Payload::Query | Payload::Close | Payload::Alive => return,
            _ => {}
        }
        let mut pending = self
            .pending
            .take()
            .filter(|p| p.id.as_deref() == header.id)
            .unwrap_or_else(|| Box::new(Pending::new(header.id)));
        if let Some(value) = header.focus {
            pending.focus = value;
        }
        if let Some(value) = header.report {
            pending.report = value;
        }
        if let Some(value) = header.close {
            pending.close = value;
        }
        if let Some(value) = header.sound {
            pending.sound = value;
        }
        if !pending.poisoned
            && (!valid_size || !pending.payload(header.payload, header.encoded, payload))
        {
            pending.poisoned = true;
        }
        if header.done {
            if let Some(message) = pending.finish(capabilities) {
                self.command(Action::Show(message), effects);
            }
        } else {
            self.pending = Some(pending);
        }
    }
    pub(crate) fn poison(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.poisoned = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forged_list_lengths_are_rejected_before_reading_or_allocating_elements() {
        struct Huge;
        impl<'de> SeqAccess<'de> for Huge {
            type Error = de::value::Error;
            fn next_element_seed<T: de::DeserializeSeed<'de>>(
                &mut self,
                _seed: T,
            ) -> Result<Option<T::Value>, Self::Error> {
                panic!("oversized sequence must be rejected before reading an element")
            }
            fn size_hint(&self) -> Option<usize> {
                Some(usize::MAX)
            }
        }
        assert!(buttons(de::value::SeqAccessDeserializer::new(Huge)).is_err());
        assert!(identifiers(de::value::SeqAccessDeserializer::new(Huge)).is_err());
    }
}
