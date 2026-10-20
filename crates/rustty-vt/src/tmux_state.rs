//! tmux-observable state which is distinct from renderer defaults.
use crate::{CursorShape, Effect, Terminal};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TmuxState {
    pub alternate_saved: [u32; 2],
    pub cursor_blinking: bool,
    pub cursor_blinking_set: bool,
    pub cursor_very_visible: bool,
    pub progress_state: u8,
    pub progress: u8,
}
impl Default for TmuxState {
    fn default() -> Self {
        Self {
            alternate_saved: [u32::MAX; 2],
            cursor_blinking: false,
            cursor_blinking_set: false,
            cursor_very_visible: false,
            progress_state: 0,
            progress: 0,
        }
    }
}

impl Terminal {
    pub fn tmux_cursor_shape(&self) -> &'static str {
        if self.metadata.cursor_is_default {
            return "default";
        }
        match self.screen().cursor.shape {
            CursorShape::Underline => "underline",
            CursorShape::Bar => "bar",
            _ => "block",
        }
    }

    pub(crate) fn tmux_progress(&mut self, data: &[u8], effects: &mut Vec<Effect>) {
        let Some(data) = data.strip_prefix(b"4;") else {
            return;
        };
        let Some(&state @ b'0'..=b'4') = data.first() else {
            return;
        };
        let state = state - b'0';
        let value = match &data[1..] {
            [] | [b';'] => None,
            [b';', digits @ ..] if digits.iter().all(u8::is_ascii_digit) => {
                let mut value = 0u8;
                for digit in digits {
                    let Some(next) = value
                        .checked_mul(10)
                        .and_then(|v| v.checked_add(digit - b'0'))
                        .filter(|v| *v <= 100)
                    else {
                        return;
                    };
                    value = next;
                }
                Some(value)
            }
            _ => return,
        };
        self.tmux_state.progress_state = state;
        if state != 3
            && let Some(value) = value
        {
            self.tmux_state.progress = value;
        }
        effects.push(Effect::Progress {
            state,
            value: (state != 3).then_some(self.tmux_state.progress),
        });
        self.generation = self.generation.wrapping_add(1);
    }
}
