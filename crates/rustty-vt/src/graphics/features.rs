//! Kitty feature discovery is deliberately independent of image decoding.
use super::{Command, Effect, MAX_DIMENSION};

impl Command {
    pub(super) fn feature_reply(&self, enabled: bool, effects: &mut Vec<Effect>) {
        if self.quiet() != 0 {
            return;
        }
        let mut keys = vec![format!("i={}", self.n(b'i'))];
        let action = self.n(b'e');
        if enabled && b"tTqpdfac".iter().any(|&a| u32::from(a) == action) {
            let action = action as u8;
            keys.push(format!("e={}", char::from(action)));
            for (&key, &value) in &self.values {
                if value != 0 && self.supports_feature(action, key, value) {
                    let value = if matches!(key, b't' | b'o' | b'd') {
                        char::from(value as u8).to_string()
                    } else {
                        value.to_string()
                    };
                    keys.push(format!("{}={value}", char::from(key)));
                }
            }
        }
        effects.push(Effect::Write(
            format!("\x1b_G{};OK\x1b\\", keys.join(",")).into_bytes(),
        ));
    }

    fn supports_feature(&self, action: u8, key: u8, value: i64) -> bool {
        if matches!(action, b't' | b'T' | b'q' | b'f') {
            match key {
                b't' => {
                    return value == i64::from(b'd')
                        || (value == i64::from(b's')
                            && cfg!(all(unix, not(target_os = "android"))));
                }
                b'o' => return value == i64::from(b'z'),
                b'f' => {
                    return matches!(value, 24 | 32 | 100)
                        || action != b'f' && matches!(value, 8 | 16);
                }
                b's' | b'v' => return (1..=i64::from(MAX_DIMENSION)).contains(&value),
                b'S' | b'O' => return true,
                _ => {}
            }
        }
        match action {
            b'T' | b'p' => match key {
                b'p' | b'x' | b'y' | b'w' | b'h' | b'X' | b'Y' | b'c' | b'r' | b'z' | b'P'
                | b'Q' | b'H' | b'V' => true,
                b'C' => value == 1,
                // Placeholder rendering and usage hints are not advertised here.
                _ => false,
            },
            b'f' => match key {
                b'x' | b'y' | b'c' | b'r' | b'z' | b'Y' => true,
                b'X' => value == 1,
                _ => false,
            },
            b'a' => match key {
                b's' => (1..=3).contains(&value),
                b'v' | b'r' | b'c' | b'z' => true,
                _ => false,
            },
            b'c' => match key {
                b'r' | b'c' | b'x' | b'y' | b'w' | b'h' | b'X' | b'Y' => true,
                b'C' => value == 1,
                _ => false,
            },
            b'd' => match key {
                b'd' => {
                    if self.tmux {
                        [b'a', b'A', b'i', b'I'].map(i64::from).contains(&value)
                    } else {
                        b"aAiInNcCpPqQrRxXyYzZfF"
                            .iter()
                            .any(|&v| i64::from(v) == value)
                    }
                }
                b'p' => true,
                b'x' | b'y' | b'z' | b'r' => !self.tmux,
                _ => false,
            },
            _ => false,
        }
    }
}
