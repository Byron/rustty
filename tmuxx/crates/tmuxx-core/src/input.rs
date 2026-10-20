use super::*;
use tmuxx_terminal::{Input, Key, KeyAction, KeyEncodeOptions, KeyEvent};

impl Engine {
    pub(super) fn encode_named_key(&self, pane: Id, bytes: &[u8]) -> Vec<u8> {
        if bytes.eq_ignore_ascii_case(b"Any") {
            return Vec::new();
        }
        if let Some(event) = parse_key(bytes) {
            let mut data = self.encode_pane_key(
                pane,
                &event,
                KeyEncodeOptions {
                    macos_option_as_alt: true,
                },
            );
            if self.panes[&pane].terminal.extended_keys_mode() == 0 && event.modifiers.control {
                let cp = match event.key {
                    Key::Char(c) => Some(c),
                    Key::Escape => Some('\x1b'),
                    _ => None,
                };
                if cp.is_some_and(|c| c.is_ascii() && !matches!(c, '\r'|'\n'|'\t'|'1'..='9'|'0'|'!'|'('|')'|'='|'+'|';'|':'|'\''|'"'|','|'<'|'.'|'>'|'/'|'-'|'?'|' '|'@'..='~')) { data.extend(bytes); }
            }
            if self.panes[&pane].terminal.extended_keys_mode() != 0
                && !event.modifiers.control
                && !event.modifiers.alt
                && !event.modifiers.shift
                && matches!(event.key, Key::Char(c) if c.is_ascii_control() && !matches!(c,'\r'|'\t'|'\x1b'|'\x7f'))
            {
                data.extend(bytes);
            }
            data
        } else {
            bytes.to_vec()
        }
    }
    pub(super) fn encode_pane_input(&self, pane: Id, input: &Input) -> Vec<u8> {
        match input {
            Input::Key { event, options } => self.encode_pane_key(pane, event, *options),
            _ => self.panes[&pane].terminal.encode_input(input),
        }
    }
    pub(super) fn encode_pane_key(
        &self,
        pane: Id,
        event: &KeyEvent,
        options: KeyEncodeOptions,
    ) -> Vec<u8> {
        let terminal = &self.panes[&pane].terminal;
        if terminal.kitty_keyboard_active() || event.modifiers.super_key {
            return terminal.encode_input(&Input::Key {
                event: event.clone(),
                options,
            });
        }
        if event.action == KeyAction::Release || event.composing {
            return Vec::new();
        }
        let mut event = event.clone();
        if cfg!(target_os = "macos") && !options.macos_option_as_alt {
            event.modifiers.alt = false;
        }
        if event.key == Key::Backspace {
            let name = self.global_option("backspace").unwrap_or("C-?");
            if name == "None" {
                return if event.modifiers.control || event.modifiers.alt || event.modifiers.shift {
                    Vec::new()
                } else {
                    vec![0]
                };
            }
            let Some(mapped) = parse_key(name.as_bytes()) else {
                return Vec::new();
            };
            if !event.modifiers.shift && !event.modifiers.alt && !event.modifiers.control {
                return plain_backspace(&mapped);
            }
            event.key = mapped.key;
            event.text = mapped.text;
            event.modifiers.shift |= mapped.modifiers.shift;
            event.modifiers.alt |= mapped.modifiers.alt;
            event.modifiers.control |= mapped.modifiers.control;
        }
        if let Some((plain, app)) = match event.key {
            Key::Keypad(n @ 0..=9) => Some((b'0' + n, b'p' + n)),
            Key::KeypadEnter => Some((b'\n', b'M')),
            Key::KeypadDivide => Some((b'/', b'o')),
            Key::KeypadMultiply => Some((b'*', b'j')),
            Key::KeypadSubtract => Some((b'-', b'm')),
            Key::KeypadAdd => Some((b'+', b'k')),
            Key::KeypadDecimal => Some((b'.', b'n')),
            _ => None,
        } {
            if event.modifiers.control || event.modifiers.shift {
                return Vec::new();
            }
            let mut bytes = if event.modifiers.alt {
                vec![27]
            } else {
                Vec::new()
            };
            if terminal.mode(true, 66) {
                bytes.extend([27, b'O', app]);
            } else {
                bytes.push(plain);
            }
            return bytes;
        }
        let mode = terminal.extended_keys_mode();
        if event.key == Key::Tab && event.modifiers.shift && mode != 2 {
            return b"\x1b[Z".to_vec();
        }
        if matches!(event.key, Key::Home | Key::End)
            && !event.modifiers.shift
            && !event.modifiers.control
            && !event.modifiers.alt
        {
            return if event.key == Key::Home {
                b"\x1b[1~".to_vec()
            } else {
                b"\x1b[4~".to_vec()
            };
        }
        let cp = match event.key {
            Key::Char(cp) => event
                .text
                .as_deref()
                .filter(|s| s.chars().count() == 1)
                .and_then(|s| s.chars().next())
                .filter(|c| !c.is_control())
                .unwrap_or(cp),
            Key::Enter => '\r',
            Key::Tab => '\t',
            Key::Escape => '\x1b',
            _ => return terminal.encode_input(&Input::Key { event, options }),
        };
        let modifiers = event.modifiers;
        let number = 1
            + u8::from(modifiers.shift)
            + 2 * u8::from(modifiers.alt)
            + 4 * u8::from(modifiers.control);
        let standard = mode == 0
            || mode == 1
                && ((modifiers.alt && !modifiers.control)
                    || modifiers.control
                        && (matches!(cp, ' ' | '/' | '@' | '^' | '2'..='8')
                            || ('@'..='~').contains(&cp)));
        if number == 1
            && mode != 0
            && cp.is_ascii_control()
            && !matches!(cp, '\r' | '\t' | '\x1b' | '\x7f')
        {
            return Vec::new();
        }
        if number > 1 && !standard {
            return if self.global_option("extended-keys-format") == Some("xterm") {
                format!("\x1b[27;{number};{}~", cp as u32)
            } else {
                format!("\x1b[{};{number}u", cp as u32)
            }
            .into_bytes();
        }
        standard_key(cp, modifiers.control, modifiers.alt)
    }
}
fn plain_backspace(event: &KeyEvent) -> Vec<u8> {
    if event.modifiers.alt || event.modifiers.shift {
        return Vec::new();
    }
    let Key::Char(cp) = event.key else {
        return Vec::new();
    };
    if !event.modifiers.control {
        return vec![cp as u8];
    }
    match cp {
        '?' => vec![127],
        '@'..='_' => vec![cp as u8 - 64],
        'a'..='z' => vec![cp as u8 - 96],
        _ => Vec::new(),
    }
}
fn standard_key(cp: char, control: bool, meta: bool) -> Vec<u8> {
    let mut output = if meta { vec![27] } else { Vec::new() };
    if !cp.is_ascii() {
        output.extend(cp.to_string().bytes());
        return output;
    }
    let byte = if control && !matches!(cp, '\r' | '\n' | '\t') {
        let table = b"1!9(0)=+;:'\",<.>/-8? 2";
        let mapping = b"119900=+;;'',,..\x1f\x1f\x7f\x7f\0\0";
        if let Some(index) = table.iter().position(|b| *b == cp as u8) {
            mapping[index]
        } else if ('3'..='7').contains(&cp) {
            cp as u8 - 24
        } else if ('@'..='~').contains(&cp) {
            cp as u8 & 31
        } else {
            return output;
        }
    } else {
        cp as u8 & 127
    };
    output.push(byte);
    output
}

pub(super) const CONTROL_NAMES: [&str; 32] = [
    "[NUL]", "[SOH]", "[STX]", "[ETX]", "[EOT]", "[ENQ]", "[ASC]", "[BEL]", "[BS]", "Tab", "[LF]",
    "[VT]", "[FF]", "Enter", "[SO]", "[SI]", "[DLE]", "[DC1]", "[DC2]", "[DC3]", "[DC4]", "[NAK]",
    "[SYN]", "[ETB]", "[CAN]", "[EM]", "[SUB]", "Escape", "[FS]", "[GS]", "[RS]", "[US]",
];

pub(super) fn canonical_key(value: &str) -> Option<String> {
    for name in ["None", "Any"] {
        if value.eq_ignore_ascii_case(name) {
            return Some(name.into());
        }
    }
    let event = parse_key(value.as_bytes())?;
    // tmux stores hexadecimal values >=32 as Unicode, even Space and DEL.
    if value.starts_with("0x")
        && let Key::Char(c) = event.key
        && c >= ' '
    {
        return Some(c.to_string());
    }
    let btab = value.to_ascii_lowercase().ends_with("btab");
    let base = match event.key {
        Key::Char(' ') => "Space".into(),
        Key::Char('\x7f') => "C-?".into(),
        Key::Char(c) if c.is_ascii_control() => CONTROL_NAMES[c as usize].into(),
        Key::Char(c) => c.to_string(),
        Key::Enter => "Enter".into(),
        Key::Tab if btab => "BTab".into(),
        Key::Tab => "Tab".into(),
        Key::Escape => "Escape".into(),
        Key::Backspace => "BSpace".into(),
        Key::Up => "Up".into(),
        Key::Down => "Down".into(),
        Key::Left => "Left".into(),
        Key::Right => "Right".into(),
        Key::Home => "Home".into(),
        Key::End => "End".into(),
        Key::PageUp => "PPage".into(),
        Key::PageDown => "NPage".into(),
        Key::Insert => "IC".into(),
        Key::Delete => "DC".into(),
        Key::Function(n) => format!("F{n}"),
        Key::Keypad(n) => format!("KP{n}"),
        Key::KeypadEnter => "KPEnter".into(),
        Key::KeypadDivide => "KP/".into(),
        Key::KeypadMultiply => "KP*".into(),
        Key::KeypadSubtract => "KP-".into(),
        Key::KeypadAdd => "KP+".into(),
        Key::KeypadDecimal => "KP.".into(),
        _ => return None,
    };
    Some(format!(
        "{}{}{}{base}",
        if event.modifiers.control { "C-" } else { "" },
        if event.modifiers.alt { "M-" } else { "" },
        if event.modifiers.shift && (!btab || value.to_ascii_lowercase().contains("s-")) {
            "S-"
        } else {
            ""
        }
    ))
}
