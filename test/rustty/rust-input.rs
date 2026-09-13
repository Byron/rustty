//! Input protocol adapter. Physical key names match the Zig public API.
use rustty_vt::{
    Key, KeyAction, KeyEncodeOptions, KeyEvent, Modifiers, MouseAction, MouseButton, MouseEvent,
    Terminal,
};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Event {
    kind: String,
    key: String,
    data: String,
    action: String,
    modifiers: u16,
    consumed_modifiers: u16,
    unshifted: u32,
    composing: bool,
    macos_option_as_alt: String,
    conservative: bool,
    focused: bool,
    button: Option<String>,
    x: f32,
    y: f32,
}

impl Default for Event {
    fn default() -> Self {
        Self {
            kind: String::new(),
            key: "unidentified".into(),
            data: String::new(),
            action: "press".into(),
            modifiers: 0,
            consumed_modifiers: 0,
            unshifted: 0,
            composing: false,
            macos_option_as_alt: "true".into(),
            conservative: false,
            focused: true,
            button: None,
            x: 0.,
            y: 0.,
        }
    }
}

pub fn encode(terminal: &Terminal, event: &Event) -> Result<Vec<u8>, &'static str> {
    let mods = modifiers(event.modifiers);
    match event.kind.as_str() {
        "key" => {
            let text = String::from_utf8(super::unhex(&event.data)?).map_err(|_| "InvalidText")?;
            let key = key(&event.key)?;
            let action = match event.action.as_str() {
                "press" => KeyAction::Press,
                "repeat" => KeyAction::Repeat,
                "release" => KeyAction::Release,
                _ => return Err("InvalidAction"),
            };
            let options = KeyEncodeOptions {
                macos_option_as_alt: match event.macos_option_as_alt.as_str() {
                    "true" => true,
                    "false" => false,
                    "left" => event.modifiers & 256 == 0,
                    "right" => event.modifiers & 256 != 0,
                    _ => return Err("InvalidOptionAsAlt"),
                },
            };
            Ok(terminal.encode_key_with_options(
                &KeyEvent {
                    key,
                    text: (!text.is_empty()).then_some(text),
                    modifiers: mods,
                    consumed_modifiers: modifiers(event.consumed_modifiers),
                    action,
                    unshifted: (event.unshifted != 0)
                        .then(|| char::from_u32(event.unshifted))
                        .flatten(),
                    composing: event.composing,
                },
                options,
            ))
        }
        "mouse" => {
            if !event.x.is_finite() || !event.y.is_finite() {
                return Err("UnsupportedCoordinates");
            }
            let button = match event.button.as_deref() {
                None => None,
                Some("left") => Some(MouseButton::Left),
                Some("middle") => Some(MouseButton::Middle),
                Some("right") => Some(MouseButton::Right),
                Some("four") => Some(MouseButton::WheelUp),
                Some("five") => Some(MouseButton::WheelDown),
                Some("six") => Some(MouseButton::WheelLeft),
                Some("seven") => Some(MouseButton::WheelRight),
                Some("eight") => Some(MouseButton::Extra(0)),
                Some("nine") => Some(MouseButton::Extra(1)),
                Some("ten") => Some(MouseButton::Extra(2)),
                Some("eleven") => Some(MouseButton::Extra(3)),
                _ => return Err("InvalidButton"),
            };
            let action = match event.action.as_str() {
                "press" => MouseAction::Press,
                "release" => MouseAction::Release,
                "motion" => MouseAction::Move,
                _ => return Err("InvalidAction"),
            };
            Ok(terminal.encode_mouse(MouseEvent {
                action,
                button,
                modifiers: mods,
                col: (event.x / 8.).floor() as usize,
                row: (event.y / 16.).floor() as usize,
                x: f64::from(event.x),
                y: f64::from(event.y),
            }))
        }
        "focus" => Ok(terminal.encode_focus(event.focused)),
        "paste" => {
            let bytes = super::unhex(&event.data)?;
            Ok(terminal.encode_paste(&bytes))
        }
        "paste_safe" => {
            let bytes = super::unhex(&event.data)?;
            Ok(vec![u8::from(rustty_vt::input::paste_is_safe(
                &bytes,
                !event.conservative && terminal.modes.dec(2004),
            ))])
        }
        _ => Err("UnsupportedInput"),
    }
}

fn modifiers(bits: u16) -> Modifiers {
    Modifiers {
        shift: bits & 1 != 0,
        control: bits & 2 != 0,
        alt: bits & 4 != 0,
        super_key: bits & 8 != 0,
        caps_lock: bits & 16 != 0,
        num_lock: bits & 32 != 0,
    }
}

fn key(name: &str) -> Result<Key, &'static str> {
    Ok(match name {
        "unidentified" => Key::Unidentified,
        "enter" => Key::Enter,
        "backspace" => Key::Backspace,
        "tab" => Key::Tab,
        "escape" => Key::Escape,
        "arrow_up" => Key::Up,
        "arrow_down" => Key::Down,
        "arrow_left" => Key::Left,
        "arrow_right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "page_up" => Key::PageUp,
        "page_down" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        "help" => Key::Help,
        "context_menu" => Key::ContextMenu,
        "caps_lock" => Key::CapsLock,
        "num_lock" => Key::NumLock,
        "scroll_lock" => Key::ScrollLock,
        "print_screen" => Key::PrintScreen,
        "pause" => Key::Pause,
        "numpad_enter" => Key::KeypadEnter,
        "numpad_decimal" => Key::KeypadDecimal,
        "numpad_add" => Key::KeypadAdd,
        "numpad_subtract" => Key::KeypadSubtract,
        "numpad_multiply" => Key::KeypadMultiply,
        "numpad_divide" => Key::KeypadDivide,
        "numpad_equal" => Key::KeypadEqual,
        "numpad_separator" => Key::KeypadSeparator,
        "numpad_left" => Key::KeypadLeft,
        "numpad_right" => Key::KeypadRight,
        "numpad_up" => Key::KeypadUp,
        "numpad_down" => Key::KeypadDown,
        "numpad_page_up" => Key::KeypadPageUp,
        "numpad_page_down" => Key::KeypadPageDown,
        "numpad_home" => Key::KeypadHome,
        "numpad_end" => Key::KeypadEnd,
        "numpad_insert" => Key::KeypadInsert,
        "numpad_delete" => Key::KeypadDelete,
        "numpad_begin" => Key::KeypadBegin,
        "shift_left" => Key::Shift,
        "shift_right" => Key::ShiftRight,
        "control_left" => Key::Control,
        "control_right" => Key::ControlRight,
        "alt_left" => Key::Alt,
        "alt_right" => Key::AltRight,
        "meta_left" => Key::Super,
        "meta_right" => Key::SuperRight,
        "space" => Key::Char(' '),
        "backquote" => Key::Char('`'),
        "minus" => Key::Char('-'),
        "equal" => Key::Char('='),
        "bracket_left" => Key::Char('['),
        "bracket_right" => Key::Char(']'),
        "backslash" => Key::Char('\\'),
        "semicolon" => Key::Char(';'),
        "quote" => Key::Char('\''),
        "comma" => Key::Char(','),
        "period" => Key::Char('.'),
        "slash" => Key::Char('/'),
        name if name.starts_with("key_") && name.len() == 5 => {
            Key::Char(name.chars().last().unwrap())
        }
        name if name.starts_with("digit_") && name.len() == 7 => {
            Key::Char(name.chars().last().unwrap())
        }
        name if name.starts_with("numpad_") && name.len() == 8 => {
            Key::Keypad(name[7..].parse().map_err(|_| "InvalidKey")?)
        }
        name if name.starts_with('f') => {
            Key::Function(name[1..].parse().map_err(|_| "InvalidKey")?)
        }
        _ => return Err("InvalidKey"),
    })
}
