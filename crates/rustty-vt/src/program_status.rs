//! OSC 7501 reports and bounded, per-terminal program records.
//! Hosts decide when real focus, input, or process exit acknowledges records.

use serde::{Deserialize, Serialize};

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    Clear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Permission,
    Question,
    Auth,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
            Self::Clear => "clear",
        }
    }

    /// Reuse the host's existing activity vocabulary without an OSC 777 registration.
    pub fn activity(self) -> Option<crate::agent::State> {
        use crate::agent::State as Activity;
        Some(match self {
            Self::Idle => Activity::Idle,
            Self::Working => Activity::Working,
            Self::Done => Activity::Done,
            Self::Blocked => Activity::NeedsInput,
            Self::Error => Activity::Error,
            Self::Clear => return None,
        })
    }

    fn urgency(self) -> u8 {
        match self {
            Self::Idle | Self::Clear => 0,
            Self::Working => 1,
            Self::Done => 2,
            Self::Error => 3,
            Self::Blocked => 4,
        }
    }
}

/// Each report replaces its record. `None` identifies the root record; `Clear`
/// removes the named record (or all records when no ID is supplied).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub state: State,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub kind: Option<Kind>,
    #[serde(default)]
    pub progress: Option<u8>,
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub message: String,
}

pub(crate) const QUERY_REPLY: &[u8] =
    b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth";

impl Report {
    /// Decode the body after `7501;`, without framing or a terminator.
    pub fn parse(body: &[u8]) -> Option<Self> {
        // 4096 bytes including ESC ], numeric prefix and the longer ST terminator.
        if body.len() > 4087 {
            return None;
        }
        let (mut state, mut id, mut message, mut title) = (None, None, &b""[..], &b""[..]);
        let (mut app, mut kind, mut progress) = (&b""[..], &b""[..], &b""[..]);
        for pair in body.split(|&byte| byte == b':') {
            let Some(eq) = pair.iter().position(|&byte| byte == b'=') else {
                continue;
            };
            let key = pair[..eq].trim_ascii();
            let value = pair[eq + 1..].trim_ascii();
            if key.len() > 16 {
                return None;
            }
            // Validate every raw occurrence before skipping disallowed bytes or
            // replacing duplicates: an invalid ID must never become a root report.
            match key {
                b"id" if !valid_id(value) => return None,
                b"app" if value.len() > 32 => return None,
                b"title" if value.len() > 256 => return None,
                b"msg" if value.len() > 2732 => return None,
                _ => {}
            }
            if !value
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.+-,/=".contains(byte))
            {
                continue;
            }
            match key {
                b"state" => state = Some(value),
                b"id" => id = Some(value),
                b"msg" => message = value,
                b"title" => title = value,
                b"app" => app = value,
                b"kind" => kind = value,
                b"progress" => progress = value,
                _ => {}
            }
        }
        let state = match state? {
            b"idle" => State::Idle,
            b"working" => State::Working,
            b"done" => State::Done,
            b"blocked" => State::Blocked,
            b"error" => State::Error,
            b"clear" => State::Clear,
            _ => return None,
        };
        Some(Self {
            state,
            id: id
                .map(|id| String::from_utf8(id.to_vec()))
                .transpose()
                .ok()?,
            kind: (state == State::Blocked)
                .then(|| match kind {
                    b"permission" => Some(Kind::Permission),
                    b"question" => Some(Kind::Question),
                    b"auth" => Some(Kind::Auth),
                    _ => None,
                })
                .flatten(),
            progress: if matches!(state, State::Working | State::Blocked)
                && !progress.is_empty()
                && progress.iter().all(u8::is_ascii_digit)
            {
                progress
                    .iter()
                    .try_fold(0u8, |n, byte| n.checked_mul(10)?.checked_add(byte - b'0'))
                    .filter(|&n| n <= 100)
            } else {
                None
            },
            app: if valid_name(app) {
                String::from_utf8(app.to_vec()).ok()?
            } else {
                String::new()
            },
            title: decode_text(title, 192)?,
            message: decode_text(message, 2048)?,
        })
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id.as_ref().is_some_and(|id| !valid_id(id.as_bytes())) {
            return Err("invalid program status ID");
        }
        if self.app.len() > 32 || (!self.app.is_empty() && !valid_name(self.app.as_bytes())) {
            return Err("invalid program status app");
        }
        if self.title.len() > 192
            || self.message.len() > 2048
            || self
                .title
                .chars()
                .chain(self.message.chars())
                .any(char::is_control)
        {
            return Err("invalid program status text");
        }
        if self.kind.is_some() && self.state != State::Blocked {
            return Err("program status kind requires blocked state");
        }
        if self.progress.is_some_and(|n| n > 100)
            || (self.progress.is_some() && !matches!(self.state, State::Working | State::Blocked))
        {
            return Err("invalid program status progress");
        }
        Ok(())
    }
}

fn valid_name(value: &[u8]) -> bool {
    !value.is_empty()
        && value
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(b))
}

fn valid_id(value: &[u8]) -> bool {
    value.len() <= 128
        && value.split(|&b| b == b'/').count() <= 8
        && value
            .split(|&b| b == b'/')
            .all(|part| part.len() <= 32 && valid_name(part))
}

fn decode_text(encoded: &[u8], limit: usize) -> Option<String> {
    let decoder = if encoded.ends_with(b"=") {
        &STANDARD
    } else {
        &STANDARD_NO_PAD
    };
    let text = String::from_utf8(decoder.decode(encoded).ok()?).ok()?;
    (text.len() <= limit && !text.chars().any(char::is_control)).then_some(text)
}

pub const MAX_RECORDS: usize = 256;

/// Ordered by last change for urgency; eviction separately uses last report.
/// Private storage ensures all mutations and restored state remain bounded.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "StoredRecords")]
pub struct Records {
    records: Vec<Report>,
    reported: Vec<Option<String>>,
}

#[derive(Deserialize)]
struct StoredRecords {
    records: Vec<Report>,
    reported: Vec<Option<String>>,
}

impl TryFrom<StoredRecords> for Records {
    type Error = &'static str;
    fn try_from(value: StoredRecords) -> Result<Self, Self::Error> {
        if value.records.len() > MAX_RECORDS || value.reported.len() != value.records.len() {
            return Err("invalid program status record count");
        }
        for (index, record) in value.records.iter().enumerate() {
            record.validate()?;
            if record.state == State::Clear
                || value.records[..index].iter().any(|r| r.id == record.id)
                || value.reported.iter().filter(|id| **id == record.id).count() != 1
            {
                return Err("invalid program status record order");
            }
        }
        Ok(Self {
            records: value.records,
            reported: value.reported,
        })
    }
}

/// A query resolves inherited app names without changing the retained records.
#[derive(Debug, Serialize)]
pub struct RecordView<'a> {
    pub id: &'a str,
    pub state: State,
    pub kind: Option<Kind>,
    pub progress: Option<u8>,
    pub app: &'a str,
    pub title: &'a str,
    pub msg: &'a str,
}

impl Records {
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = &Report> {
        self.records.iter()
    }
    pub fn get(&self, id: Option<&str>) -> Option<&Report> {
        self.records.iter().find(|r| r.id.as_deref() == id)
    }

    /// Return whether visible records changed. Identical reports only refresh eviction order.
    pub fn apply(&mut self, report: Report) -> Result<bool, &'static str> {
        report.validate()?;
        if report.state == State::Clear {
            return Ok(self.remove(|r| {
                report.id.as_ref().is_none_or(|ancestor| {
                    r.id.as_ref().is_some_and(|id| {
                        id == ancestor
                            || id
                                .strip_prefix(ancestor)
                                .is_some_and(|tail| tail.starts_with('/'))
                    })
                })
            }));
        }
        self.reported.retain(|id| *id != report.id);
        self.reported.push(report.id.clone());
        if let Some(index) = self.records.iter().position(|r| r.id == report.id) {
            if self.records[index] == report {
                return Ok(false);
            }
            self.records.remove(index);
        } else if self.records.len() == MAX_RECORDS {
            let oldest = self.reported.remove(0);
            self.records.retain(|r| r.id != oldest);
        }
        self.records.push(report);
        Ok(true)
    }

    fn remove(&mut self, predicate: impl Fn(&Report) -> bool) -> bool {
        let old = self.len();
        self.records.retain(|r| !predicate(r));
        self.reported
            .retain(|id| self.records.iter().any(|r| r.id == *id));
        self.len() != old
    }
    pub fn clear(&mut self) -> bool {
        let changed = !self.is_empty();
        self.records.clear();
        self.reported.clear();
        changed
    }
    pub fn on_process_exit_or_prompt(&mut self) -> bool {
        self.remove(|r| matches!(r.state, State::Idle | State::Working | State::Blocked))
    }
    pub fn on_seen_by_user(&mut self) -> bool {
        self.remove(|r| matches!(r.state, State::Done | State::Error))
    }
    pub fn most_urgent(&self) -> Option<&Report> {
        self.records.iter().max_by_key(|r| r.state.urgency())
    }
    pub fn activity(&self) -> Option<crate::agent::State> {
        self.most_urgent()?.state.activity()
    }
    pub fn app_for<'a>(&'a self, record: &'a Report) -> &'a str {
        if !record.app.is_empty() {
            return &record.app;
        }
        let mut id = record.id.as_deref().unwrap_or_default();
        while !id.is_empty() {
            id = id.rsplit_once('/').map_or("", |(parent, _)| parent);
            if let Some(parent) = self.get((!id.is_empty()).then_some(id))
                && !parent.app.is_empty()
            {
                return &parent.app;
            }
        }
        ""
    }
    pub fn query(&self) -> Vec<RecordView<'_>> {
        self.records
            .iter()
            .map(|r| RecordView {
                id: r.id.as_deref().unwrap_or_default(),
                state: r.state,
                kind: r.kind,
                progress: r.progress,
                app: self.app_for(r),
                title: &r.title,
                msg: &r.message,
            })
            .collect()
    }
}

/// Only physical, non-modifier key presses acknowledge unseen records. Hosts
/// handle committed text separately; mouse, paste, replies and releases do not.
pub fn acknowledges_key(event: &crate::KeyEvent) -> bool {
    use crate::{Key, KeyAction};
    event.action != KeyAction::Release
        && !event.composing
        && !matches!(
            event.key,
            Key::Shift
                | Key::Control
                | Key::Alt
                | Key::Super
                | Key::ShiftRight
                | Key::ControlRight
                | Key::AltRight
                | Key::SuperRight
                | Key::CapsLock
                | Key::NumLock
                | Key::ScrollLock
        )
}

impl crate::Terminal {
    pub(crate) fn osc_program_status(
        &mut self,
        data: &[u8],
        bell: bool,
        effects: &mut Vec<crate::Effect>,
    ) {
        if !self.program_status_events {
            return;
        }
        if data == b"?" {
            let mut reply = QUERY_REPLY.to_vec();
            reply.extend_from_slice(if bell { b"\x07" } else { b"\x1b\\" });
            effects.push(crate::Effect::Write(reply));
        } else if let Some(report) = Report::parse(data) {
            let _ = self.program_status.apply(report.clone());
            effects.push(crate::Effect::ProgramStatus(report));
        }
    }
    /// Retained metadata is independent of screen selection and host actions.
    pub fn program_status(&self) -> &Records {
        &self.program_status
    }

    /// Called after real child output has drained, or at a new primary prompt.
    pub fn program_status_exited(&mut self) -> bool {
        self.program_status.on_process_exit_or_prompt()
    }

    /// A real non-modifier key or committed text was accepted by the owner.
    pub fn program_status_seen(&mut self) -> bool {
        self.program_status.on_seen_by_user()
    }

    /// Actual effective focus is independent of application focus-report policy.
    /// Repeated host updates must not acknowledge reports produced while focused.
    pub fn program_status_focus(&mut self, focused: bool) -> bool {
        let gained = focused && !self.program_status_focused;
        self.program_status_focused = focused;
        gained && self.program_status_seen()
    }
}
