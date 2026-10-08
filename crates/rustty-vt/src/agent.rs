//! Private, pane-local OSC 777 agent status. This carries metadata, never actions.

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Deserializer, Serialize};

pub const MAX_JSON_BYTES: usize = 1024;
pub const MAX_METADATA_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Idle,
    Working,
    NeedsInput,
    Done,
    Error,
    Paused,
    Unknown,
}

impl std::str::FromStr for State {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "idle" => Ok(Self::Idle),
            "working" => Ok(Self::Working),
            "needs_input" => Ok(Self::NeedsInput),
            "done" => Ok(Self::Done),
            "error" => Ok(Self::Error),
            "paused" => Ok(Self::Paused),
            "unknown" => Ok(Self::Unknown),
            _ => Err("invalid agent state"),
        }
    }
}

/// Every update replaces all metadata; an omitted field uses the pane default.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub state: State,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_string"
    )]
    pub label: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_string"
    )]
    pub thread_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_string"
    )]
    pub turn_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Event {
    Begin(Snapshot),
    Update(Snapshot),
    End,
}

// Separate strict shapes also reject metadata on `end`, duplicate keys, and null.
#[derive(Deserialize)]
#[serde(untagged)]
enum WireEvent {
    Snapshot(WireSnapshot),
    End(WireEnd),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSnapshot {
    op: SnapshotOp,
    state: State,
    #[serde(default, deserialize_with = "optional_string")]
    label: Option<String>,
    #[serde(default, deserialize_with = "optional_string")]
    thread_id: Option<String>,
    #[serde(default, deserialize_with = "optional_string")]
    turn_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum SnapshotOp {
    Begin,
    Update,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEnd {
    #[serde(rename = "op")]
    _op: EndOp,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum EndOp {
    End,
}

fn optional_string<'de, D: Deserializer<'de>>(value: D) -> Result<Option<String>, D::Error> {
    String::deserialize(value).map(Some)
}

impl Snapshot {
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(label) = &self.label
            && (label.len() > MAX_METADATA_BYTES || label.chars().any(char::is_control))
        {
            return Err("label must be at most 128 UTF-8 bytes without control characters");
        }
        for id in [&self.thread_id, &self.turn_id].into_iter().flatten() {
            if id.is_empty()
                || id.len() > MAX_METADATA_BYTES
                || !id.is_ascii()
                || id.bytes().any(|byte| byte.is_ascii_control())
            {
                return Err("thread/turn IDs must be 1–128 ASCII bytes without control characters");
            }
        }
        if self.state == State::Done && self.turn_id.is_none() {
            return Err("done requires a turn_id");
        }
        Ok(())
    }
}

impl Event {
    /// Decode the version and base64 after `777;rustty-agent;`.
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let encoded = payload.strip_prefix(b"1;")?;
        if encoded.len() > MAX_JSON_BYTES.div_ceil(3) * 4 {
            return None;
        }
        let json = STANDARD.decode(encoded).ok()?;
        Self::from_json(&json).ok()
    }

    pub fn from_json(json: &[u8]) -> Result<Self, &'static str> {
        if json.len() > MAX_JSON_BYTES {
            return Err("agent JSON exceeds 1024 bytes");
        }
        let wire: WireEvent =
            serde_json::from_slice(json).map_err(|_| "invalid agent JSON shape")?;
        match wire {
            WireEvent::End(_) => Ok(Self::End),
            WireEvent::Snapshot(wire) => {
                let snapshot = Snapshot {
                    state: wire.state,
                    label: wire.label,
                    thread_id: wire.thread_id,
                    turn_id: wire.turn_id,
                };
                snapshot.validate()?;
                Ok(match wire.op {
                    SnapshotOp::Begin => Self::Begin(snapshot),
                    SnapshotOp::Update => Self::Update(snapshot),
                })
            }
        }
    }

    /// Build a whole ordinary-ST frame for a reporter's serialized output path.
    pub fn frame(&self) -> Result<Vec<u8>, &'static str> {
        if let Self::Begin(snapshot) | Self::Update(snapshot) = self {
            snapshot.validate()?;
        }
        let json = serde_json::to_vec(self).map_err(|_| "cannot serialize agent status")?;
        if json.len() > MAX_JSON_BYTES {
            return Err("agent JSON exceeds 1024 bytes");
        }
        Ok(format!("\x1b]777;rustty-agent;1;{}\x1b\\", STANDARD.encode(json)).into_bytes())
    }
}
