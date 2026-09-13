//! Keep physical keys and composed text separate until terminal encoding.
use rustty::{config, vt};
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey},
    platform::modifier_supplement::KeyEventExtModifierSupplement,
};

pub fn modifiers(m: ModifiersState) -> config::Modifiers {
    config::Modifiers {
        shift: m.shift_key(),
        control: m.control_key(),
        alt: m.alt_key(),
        super_key: m.super_key(),
    }
}

pub fn terminal_modifiers(m: ModifiersState) -> vt::Modifiers {
    vt::Modifiers {
        shift: m.shift_key(),
        control: m.control_key(),
        alt: m.alt_key(),
        super_key: m.super_key(),
        ..Default::default()
    }
}

pub fn terminal_key(
    event: &KeyEvent,
    mods: ModifiersState,
    composing: bool,
) -> Option<vt::KeyEvent> {
    let unmodified = event.key_without_modifiers();
    let key = match &unmodified {
        Key::Character(text) => vt::Key::Char(text.chars().next()?),
        Key::Named(key) => match key {
            NamedKey::Enter => vt::Key::Enter,
            NamedKey::Tab => vt::Key::Tab,
            NamedKey::Backspace => vt::Key::Backspace,
            NamedKey::Escape => vt::Key::Escape,
            NamedKey::ArrowUp => vt::Key::Up,
            NamedKey::ArrowDown => vt::Key::Down,
            NamedKey::ArrowLeft => vt::Key::Left,
            NamedKey::ArrowRight => vt::Key::Right,
            NamedKey::Home => vt::Key::Home,
            NamedKey::End => vt::Key::End,
            NamedKey::PageUp => vt::Key::PageUp,
            NamedKey::PageDown => vt::Key::PageDown,
            NamedKey::Insert => vt::Key::Insert,
            NamedKey::Delete => vt::Key::Delete,
            NamedKey::Shift => vt::Key::Shift,
            NamedKey::Control => vt::Key::Control,
            NamedKey::Alt => vt::Key::Alt,
            NamedKey::Super => vt::Key::Super,
            key => vt::Key::Function(format!("{key:?}").strip_prefix('F')?.parse().ok()?),
        },
        _ => return None,
    };
    let key = match event.physical_key {
        PhysicalKey::Code(KeyCode::NumpadEnter) => vt::Key::KeypadEnter,
        PhysicalKey::Code(KeyCode::NumpadDecimal) => vt::Key::KeypadDecimal,
        PhysicalKey::Code(KeyCode::NumpadAdd) => vt::Key::KeypadAdd,
        PhysicalKey::Code(KeyCode::NumpadSubtract) => vt::Key::KeypadSubtract,
        PhysicalKey::Code(KeyCode::NumpadMultiply) => vt::Key::KeypadMultiply,
        PhysicalKey::Code(KeyCode::NumpadDivide) => vt::Key::KeypadDivide,
        PhysicalKey::Code(code)
            if format!("{code:?}")
                .strip_prefix("Numpad")
                .is_some_and(|n| n.len() == 1 && n.as_bytes()[0].is_ascii_digit()) =>
        {
            vt::Key::Keypad(format!("{code:?}").as_bytes()[6] - b'0')
        }
        _ => key,
    };
    Some(vt::KeyEvent {
        key,
        text: event.text.as_ref().map(|text| text.to_string()),
        modifiers: terminal_modifiers(mods),
        consumed_modifiers: vt::Modifiers {
            shift: mods.shift_key()
                && event
                    .text
                    .as_ref()
                    .is_some_and(|text| Some(text.as_str()) != unmodified.to_text()),
            alt: mods.alt_key() && event.text.as_ref().is_some_and(|text| !text.is_ascii()),
            ..Default::default()
        },
        action: if event.state == ElementState::Released {
            vt::KeyAction::Release
        } else if event.repeat {
            vt::KeyAction::Repeat
        } else {
            vt::KeyAction::Press
        },
        unshifted: unmodified.to_text().and_then(|text| text.chars().next()),
        composing,
    })
}

fn physical_name(code: PhysicalKey) -> String {
    let PhysicalKey::Code(code) = code else {
        return String::new();
    };
    match code {
        KeyCode::Backquote => "`".into(),
        KeyCode::Space => " ".into(),
        KeyCode::Minus => "-".into(),
        KeyCode::Equal => "=".into(),
        KeyCode::BracketLeft => "[".into(),
        KeyCode::BracketRight => "]".into(),
        KeyCode::Backslash => "\\".into(),
        KeyCode::Semicolon => ";".into(),
        KeyCode::Quote => "'".into(),
        KeyCode::Comma => ",".into(),
        KeyCode::Period => ".".into(),
        KeyCode::Slash => "/".into(),
        _ => normalize_name(&format!("{code:?}")),
    }
}

fn normalize_name(name: &str) -> String {
    match name {
        "space" | "Space" => " ".into(),
        "backquote" | "Backquote" => "`".into(),
        "ArrowLeft" => "arrow_left".into(),
        "ArrowRight" => "arrow_right".into(),
        "ArrowUp" => "arrow_up".into(),
        "ArrowDown" => "arrow_down".into(),
        "PageUp" => "page_up".into(),
        "PageDown" => "page_down".into(),
        _ => name
            .strip_prefix("Key")
            .or_else(|| name.strip_prefix("key_"))
            .or_else(|| name.strip_prefix("Digit"))
            .or_else(|| name.strip_prefix("digit_"))
            .unwrap_or(name)
            .to_lowercase(),
    }
}

pub fn matches(trigger: &config::KeyTrigger, event: &KeyEvent, mods: ModifiersState) -> bool {
    if trigger.modifiers != modifiers(mods) {
        return false;
    }
    if trigger.key == "catch_all" {
        return true;
    }
    let wanted = normalize_name(&trigger.key);
    if trigger.physical {
        return wanted == physical_name(event.physical_key);
    }
    let key = event.key_without_modifiers();
    let actual = match key {
        Key::Character(text) => normalize_name(&text),
        Key::Named(name) => normalize_name(&format!("{name:?}")),
        _ => return false,
    };
    wanted == actual
}

/// A modifier-only peek ends as soon as any initiating modifier is released.
pub fn chord_held(chord: config::Modifiers, current: config::Modifiers) -> bool {
    (!chord.shift || current.shift)
        && (!chord.control || current.control)
        && (!chord.alt || current.alt)
        && (!chord.super_key || current.super_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_and_modifier_release_preserve_peek_chord() {
        assert_eq!(normalize_name("key_a"), "a");
        assert_eq!(physical_name(PhysicalKey::Code(KeyCode::KeyA)), "a");
        assert_eq!(normalize_name("backquote"), "`");
        let chord = config::Modifiers {
            control: true,
            super_key: true,
            ..Default::default()
        };
        assert!(chord_held(
            chord,
            config::Modifiers {
                shift: true,
                ..chord
            }
        ));
        assert!(!chord_held(
            chord,
            config::Modifiers {
                control: false,
                ..chord
            }
        ));
    }
}
