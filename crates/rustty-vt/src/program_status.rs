//! Opt-in OSC 7501 program reports. Record ownership and presentation belong to the host.

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    Clear,
}

/// Each report replaces its record. `None` identifies the root record; `Clear`
/// removes the named record (or all records when no ID is supplied).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub state: State,
    pub id: Option<String>,
    pub message: String,
}

pub(crate) const QUERY_REPLY: &[u8] =
    b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth";

impl Report {
    pub(crate) fn parse(body: &[u8]) -> Option<Self> {
        // 4096 bytes including ESC ], numeric prefix and the longer ST terminator.
        if body.len() > 4087 {
            return None;
        }
        let (mut state, mut id, mut message, mut title) = (None, None, &b""[..], &b""[..]);
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
                b"id" => {
                    if value.len() > 128 || value.split(|&byte| byte == b'/').count() > 8 {
                        return None;
                    }
                    for segment in value.split(|&byte| byte == b'/') {
                        if segment.is_empty()
                            || segment.len() > 32
                            || !segment
                                .iter()
                                .all(|byte| byte.is_ascii_alphanumeric() || b"_.+-".contains(byte))
                        {
                            return None;
                        }
                    }
                }
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
        decode_text(title, 192)?;
        Some(Self {
            state,
            id: id
                .map(|id| String::from_utf8(id.to_vec()))
                .transpose()
                .ok()?,
            message: decode_text(message, 2048)?,
        })
    }
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
