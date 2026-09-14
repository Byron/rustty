//! Route input to the active editor and keep physical keys separate from composed text.
use rustty::{config, vt};
use std::collections::HashSet;
use winit::{
    event::{ElementState, KeyEvent, Modifiers},
    keyboard::{
        Key, KeyCode, KeyLocation, ModifiersKeyState, ModifiersState, NamedKey, PhysicalKey,
    },
    platform::modifier_supplement::KeyEventExtModifierSupplement,
};

/// Share the application's Shift override between mouse buttons and scrolling.
pub fn mouse_reporting(
    terminal: &vt::Terminal,
    modifiers: ModifiersState,
    policy: config::MouseShiftCapture,
) -> bool {
    use config::MouseShiftCapture::{Always, False, Never, True};
    let capture = match policy {
        Always => true,
        Never => false,
        False | True => terminal.mouse_shift_capture().unwrap_or(policy == True),
    };
    terminal.mouse_mode != 0 && (!modifiers.shift_key() || capture)
}

/// Selection begins on pointer movement, including movement within one cell.
pub struct SelectionDrag {
    anchor: vt::GridPoint,
    position: egui::Pos2,
}

impl SelectionDrag {
    pub fn new(anchor: vt::GridPoint, position: egui::Pos2) -> Self {
        Self { anchor, position }
    }

    pub fn update(
        &mut self,
        position: egui::Pos2,
        end: vt::GridPoint,
        rectangular: bool,
    ) -> Option<vt::Selection> {
        // AppKit/Winit repeats the pointer position before button release.
        // That update alone must not turn a click into a one-cell selection.
        if self.position == position {
            return None;
        }
        self.position = position;
        Some(vt::Selection {
            start: self.anchor,
            end,
            rectangular,
        })
    }
}

/// Keep egui-winit's native pointer position without waking egui for terminal hover.
pub fn defer_pointer_move(raw: &mut egui::RawInput) -> Option<egui::Pos2> {
    let Some(egui::Event::PointerMoved(position)) = raw.events.last() else {
        return None;
    };
    let position = *position;
    raw.events.pop();
    Some(position)
}

/// Terminal keyboard and IME events are already handled by the native event loop.
pub fn filter_egui_events(raw: &mut egui::RawInput, ui_input: bool) {
    if !ui_input {
        raw.events.retain(|event| {
            !matches!(
                event,
                egui::Event::Key { .. }
                    | egui::Event::Text(_)
                    | egui::Event::Paste(_)
                    | egui::Event::Copy
                    | egui::Event::Cut
                    | egui::Event::Ime(_)
            )
        });
    }
}

/// Keep UI and shortcut presses out of the terminal through their physical release.
pub fn key_is_consumed(
    consumed: &mut HashSet<PhysicalKey>,
    key: PhysicalKey,
    state: ElementState,
    ui_input: bool,
) -> bool {
    if state == ElementState::Released {
        consumed.remove(&key) || ui_input
    } else if ui_input {
        consumed.insert(key);
        true
    } else {
        consumed.contains(&key)
    }
}

/// Native menu shortcuts bypass keyboard events, so feed the owning UI explicitly.
pub fn edit_menu_action(
    raw: &mut egui::RawInput,
    action: &config::Action,
    ui_input: bool,
    clipboard: Option<String>,
) -> bool {
    if !ui_input {
        return false;
    }
    match action {
        config::Action::CopyToClipboard => raw.events.push(egui::Event::Copy),
        config::Action::PasteFromClipboard | config::Action::PasteFromSelection => {
            if let Some(text) = clipboard {
                raw.events.push(egui::Event::Paste(text));
            }
        }
        config::Action::SelectAll | config::Action::Undo | config::Action::Redo => {
            let key = if *action == config::Action::SelectAll {
                egui::Key::A
            } else {
                egui::Key::Z
            };
            // A menu click has no real key release to clear egui's held-key state.
            for pressed in [true, false] {
                raw.events.push(egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers {
                        command: true,
                        mac_cmd: cfg!(target_os = "macos"),
                        ctrl: !cfg!(target_os = "macos"),
                        shift: *action == config::Action::Redo,
                        ..Default::default()
                    },
                });
            }
        }
        _ => return false,
    }
    true
}

/// Focus a newly opened editor before it emits this frame's IME output.
pub fn text_edit(
    ui: &mut egui::Ui,
    text: &mut String,
    id: egui::Id,
    request_focus: bool,
) -> egui::Response {
    if request_focus && !ui.memory(|memory| memory.has_focus(id)) {
        ui.memory_mut(|memory| memory.request_focus(id));
    }
    ui.add(egui::TextEdit::singleline(text).id(id))
}

/// Share egui-winit's IME lifecycle with search fields and popup editors.
pub fn terminal_input(response: &egui::Response, cursor_rect: Option<egui::Rect>, ui_input: bool) {
    if ui_input {
        return;
    }
    // Requesting focus even when already focused interrupts composition in egui.
    if !response.has_focus() {
        response.request_focus();
    }
    if let Some(cursor_rect) = cursor_rect {
        response.ctx.output_mut(|output| {
            output.ime = Some(egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Normal,
                // egui-winit 0.36 uses rect, not cursor_rect, for the native candidate area.
                rect: cursor_rect,
                cursor_rect,
                should_interrupt_composition: false,
            });
        });
    }
}

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

/// IME commits have text but no physical key; use the shared terminal encoder.
pub fn terminal_text(terminal: &vt::Terminal, text: String) -> Vec<u8> {
    let mut event = vt::KeyEvent::new(vt::Key::Unidentified);
    event.text = Some(text);
    terminal.encode_key(&event)
}

pub fn terminal_key(
    event: &KeyEvent,
    modifiers: Modifiers,
    composing: bool,
    options: vt::KeyEncodeOptions,
) -> Option<vt::KeyEvent> {
    let unmodified = event.key_without_modifiers();
    // Num Lock can turn a numpad digit into a navigation key.
    let logical = if event.location == KeyLocation::Numpad {
        &event.logical_key
    } else {
        &unmodified
    };
    let key = terminal_key_code(logical, event.physical_key, event.location);
    let text = terminal_key_text(event.text.as_deref());
    if key == vt::Key::Unidentified && text.is_none() {
        return None;
    }
    let mods = key_modifiers(key, event.state, modifiers);
    Some(vt::KeyEvent {
        key,
        text: text.map(str::to_owned),
        modifiers: terminal_modifiers(mods),
        consumed_modifiers: consumed_modifiers(&unmodified, text, mods, options),
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

fn terminal_key_text(text: Option<&str>) -> Option<&str> {
    // Winit attaches control text to Tab/Enter/Backspace. Let the VT encoder
    // encode these keys with their modifiers, matching Ghostty's keyEventText.
    text.filter(|text| {
        text.as_bytes()
            .first()
            .is_some_and(|b| !b.is_ascii_control())
    })
}

fn key_modifiers(key: vt::Key, state: ElementState, modifiers: Modifiers) -> ModifiersState {
    let mods = modifiers.state();
    if !cfg!(target_os = "macos") {
        return mods;
    }
    // Winit emits macOS modifier key events before ModifiersChanged. Update
    // that key's bit without losing an independently held opposite modifier.
    let (flag, opposite) = match key {
        vt::Key::Shift => (ModifiersState::SHIFT, modifiers.rshift_state()),
        vt::Key::ShiftRight => (ModifiersState::SHIFT, modifiers.lshift_state()),
        vt::Key::Control => (ModifiersState::CONTROL, modifiers.rcontrol_state()),
        vt::Key::ControlRight => (ModifiersState::CONTROL, modifiers.lcontrol_state()),
        vt::Key::Alt => (ModifiersState::ALT, modifiers.ralt_state()),
        vt::Key::AltRight => (ModifiersState::ALT, modifiers.lalt_state()),
        vt::Key::Super => (ModifiersState::SUPER, modifiers.rsuper_state()),
        vt::Key::SuperRight => (ModifiersState::SUPER, modifiers.lsuper_state()),
        _ => return mods,
    };
    let mut mods = mods;
    mods.set(
        flag,
        state == ElementState::Pressed || opposite == ModifiersKeyState::Pressed,
    );
    mods
}

pub fn option_as_alt(
    mode: config::OptionAsAlt,
    left: ModifiersKeyState,
    right: ModifiersKeyState,
) -> bool {
    match mode {
        config::OptionAsAlt::False => false,
        config::OptionAsAlt::True => true,
        config::OptionAsAlt::Left => left == ModifiersKeyState::Pressed,
        config::OptionAsAlt::Right => right == ModifiersKeyState::Pressed,
    }
}

fn consumed_modifiers(
    unmodified: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    options: vt::KeyEncodeOptions,
) -> vt::Modifiers {
    let text = text.filter(|text| !text.is_empty());
    // Winit exposes no consumed-modifier mask. Match AppKit's text translation:
    // Shift and composing Option contribute; Control and Command never do.
    vt::Modifiers {
        shift: mods.shift_key()
            && text.is_some_and(|text| {
                cfg!(target_os = "macos") || Some(text) != unmodified.to_text()
            }),
        alt: mods.alt_key()
            && text.is_some_and(|text| {
                if cfg!(target_os = "macos") {
                    !options.macos_option_as_alt
                } else {
                    !text.is_ascii() && Some(text) != unmodified.to_text()
                }
            }),
        ..Default::default()
    }
}

fn terminal_key_code(logical: &Key, physical: PhysicalKey, location: KeyLocation) -> vt::Key {
    let key = match logical {
        Key::Character(text) => text.chars().next().map(vt::Key::Char),
        Key::Named(key) => terminal_named_key(*key),
        _ => None,
    }
    .unwrap_or(vt::Key::Unidentified);
    match (location, logical, key) {
        (KeyLocation::Right, _, vt::Key::Shift) => return vt::Key::ShiftRight,
        (KeyLocation::Right, _, vt::Key::Control) => return vt::Key::ControlRight,
        (KeyLocation::Right, _, vt::Key::Alt) => return vt::Key::AltRight,
        (KeyLocation::Right, _, vt::Key::Super) => return vt::Key::SuperRight,
        (KeyLocation::Numpad, _, vt::Key::Left) => return vt::Key::KeypadLeft,
        (KeyLocation::Numpad, _, vt::Key::Right) => return vt::Key::KeypadRight,
        (KeyLocation::Numpad, _, vt::Key::Up) => return vt::Key::KeypadUp,
        (KeyLocation::Numpad, _, vt::Key::Down) => return vt::Key::KeypadDown,
        (KeyLocation::Numpad, _, vt::Key::PageUp) => return vt::Key::KeypadPageUp,
        (KeyLocation::Numpad, _, vt::Key::PageDown) => return vt::Key::KeypadPageDown,
        (KeyLocation::Numpad, _, vt::Key::Home) => return vt::Key::KeypadHome,
        (KeyLocation::Numpad, _, vt::Key::End) => return vt::Key::KeypadEnd,
        (KeyLocation::Numpad, _, vt::Key::Insert) => return vt::Key::KeypadInsert,
        (KeyLocation::Numpad, _, vt::Key::Delete) => return vt::Key::KeypadDelete,
        (KeyLocation::Numpad, Key::Named(NamedKey::Clear), _) => return vt::Key::KeypadBegin,
        (KeyLocation::Numpad, _, vt::Key::Enter) => return vt::Key::KeypadEnter,
        (KeyLocation::Numpad, _, vt::Key::Char(n @ '0'..='9')) => {
            return vt::Key::Keypad(n as u8 - b'0');
        }
        _ => {}
    }
    match physical {
        PhysicalKey::Code(KeyCode::ShiftRight) => vt::Key::ShiftRight,
        PhysicalKey::Code(KeyCode::ControlRight) => vt::Key::ControlRight,
        PhysicalKey::Code(KeyCode::AltRight) => vt::Key::AltRight,
        PhysicalKey::Code(KeyCode::SuperRight) => vt::Key::SuperRight,
        PhysicalKey::Code(KeyCode::NumpadEnter) => vt::Key::KeypadEnter,
        PhysicalKey::Code(KeyCode::NumpadDecimal) => vt::Key::KeypadDecimal,
        PhysicalKey::Code(KeyCode::NumpadAdd) => vt::Key::KeypadAdd,
        PhysicalKey::Code(KeyCode::NumpadSubtract) => vt::Key::KeypadSubtract,
        PhysicalKey::Code(KeyCode::NumpadMultiply) => vt::Key::KeypadMultiply,
        PhysicalKey::Code(KeyCode::NumpadDivide) => vt::Key::KeypadDivide,
        PhysicalKey::Code(KeyCode::NumpadEqual) => vt::Key::KeypadEqual,
        PhysicalKey::Code(KeyCode::NumpadComma) => vt::Key::KeypadSeparator,
        PhysicalKey::Code(KeyCode::Numpad0) => vt::Key::Keypad(0),
        PhysicalKey::Code(KeyCode::Numpad1) => vt::Key::Keypad(1),
        PhysicalKey::Code(KeyCode::Numpad2) => vt::Key::Keypad(2),
        PhysicalKey::Code(KeyCode::Numpad3) => vt::Key::Keypad(3),
        PhysicalKey::Code(KeyCode::Numpad4) => vt::Key::Keypad(4),
        PhysicalKey::Code(KeyCode::Numpad5) => vt::Key::Keypad(5),
        PhysicalKey::Code(KeyCode::Numpad6) => vt::Key::Keypad(6),
        PhysicalKey::Code(KeyCode::Numpad7) => vt::Key::Keypad(7),
        PhysicalKey::Code(KeyCode::Numpad8) => vt::Key::Keypad(8),
        PhysicalKey::Code(KeyCode::Numpad9) => vt::Key::Keypad(9),
        _ => key,
    }
}

fn terminal_named_key(key: NamedKey) -> Option<vt::Key> {
    Some(match key {
        NamedKey::Space => vt::Key::Char(' '),
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
        NamedKey::Help => vt::Key::Help,
        NamedKey::ContextMenu => vt::Key::ContextMenu,
        NamedKey::CapsLock => vt::Key::CapsLock,
        NamedKey::NumLock => vt::Key::NumLock,
        NamedKey::ScrollLock => vt::Key::ScrollLock,
        NamedKey::PrintScreen => vt::Key::PrintScreen,
        NamedKey::Pause => vt::Key::Pause,
        NamedKey::Shift => vt::Key::Shift,
        NamedKey::Control => vt::Key::Control,
        NamedKey::Alt => vt::Key::Alt,
        NamedKey::Super => vt::Key::Super,
        key => vt::Key::Function(format!("{key:?}").strip_prefix('F')?.parse().ok()?),
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
    fn shift_mouse_capture_follows_application_requests_and_user_overrides() {
        use config::MouseShiftCapture::{Always, False, Never, True};
        for (policy, expected) in [
            (False, [false, false, true]),
            (True, [true, false, true]),
            (Never, [false, false, false]),
            (Always, [true, true, true]),
        ] {
            for (request, expected) in [b"".as_slice(), b"\x1b[>0s", b"\x1b[>1s"]
                .into_iter()
                .zip(expected)
            {
                let mut terminal = vt::Terminal::new(10, 6, 0);
                terminal.feed(b"\x1b[?1002h\x1b[?1006h");
                terminal.feed(request);
                assert_eq!(
                    mouse_reporting(&terminal, ModifiersState::SHIFT, policy),
                    expected,
                    "{policy:?} {request:?}",
                );
                assert!(mouse_reporting(&terminal, ModifiersState::empty(), policy));
                terminal.feed(b"\x1b[?1002l");
                assert!(!mouse_reporting(&terminal, ModifiersState::SHIFT, policy));
            }
        }
    }

    #[test]
    fn passive_pointer_motion_does_not_restart_egui_repaints_or_discard_clicks() {
        let context = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 400.0));
        let mut time = 0.0;
        let mut draw = |events| {
            time += 0.1;
            let mut clicked = false;
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(rect),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        clicked = ui
                            .interact(
                                rect,
                                egui::Id::new("terminal"),
                                egui::Sense::click_and_drag(),
                            )
                            .clicked();
                    });
                },
            );
            output.textures_delta.clear();
            clicked
        };
        // Entering the terminal is delivered normally, then the UI settles.
        draw(vec![egui::Event::PointerMoved(egui::pos2(10.0, 10.0))]);
        for _ in 0..4 {
            draw(Vec::new());
        }
        assert!(!context.has_requested_repaint());
        let mut raw = egui::RawInput::default();
        let mut latest = None;
        for frame in 0..10 {
            for pixel in 0..100 {
                raw.events.push(egui::Event::PointerMoved(egui::pos2(
                    20.0 + pixel as f32,
                    20.0 + frame as f32,
                )));
                latest = defer_pointer_move(&mut raw);
                assert!(raw.events.is_empty());
            }
            // An unrelated redraw, such as cursor blinking, must stay idle afterward.
            draw(std::mem::take(&mut raw.events));
            assert!(!context.has_requested_repaint());
        }
        // egui-winit's button event carries the latest native coordinates, even
        // when the intervening terminal hover events were deferred.
        let position = latest.unwrap();
        raw.events.push(egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        raw.events.push(egui::Event::PointerMoved(position));
        assert_eq!(defer_pointer_move(&mut raw), Some(position));
        assert_eq!(raw.events.len(), 1);
        draw(std::mem::take(&mut raw.events));
        assert_eq!(
            context.input(|input| input.pointer.latest_pos()),
            Some(position)
        );
        assert!(draw(vec![egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }]));
    }

    #[test]
    fn selection_requires_pointer_movement_and_can_select_one_cell() {
        let anchor = vt::GridPoint { row: 1, col: 2 };
        let press = egui::pos2(20.0, 10.0);
        let mut drag = SelectionDrag::new(anchor, press);
        // Winit's repeated mouseUp position is a click, without a drag.
        assert_eq!(drag.update(press, anchor, false), None);
        let within_cell = egui::pos2(21.0, 10.0);
        assert_eq!(
            drag.update(within_cell, anchor, false),
            Some(vt::Selection {
                start: anchor,
                end: anchor,
                rectangular: false
            })
        );
        assert_eq!(drag.update(within_cell, anchor, false), None);
        let other = vt::GridPoint { row: 1, col: 5 };
        assert_eq!(
            drag.update(egui::pos2(50.0, 10.0), other, true)
                .unwrap()
                .end,
            other
        );
        // Returning to the press position after a drag selects the anchor cell.
        assert_eq!(
            drag.update(press, anchor, true),
            Some(vt::Selection {
                start: anchor,
                end: anchor,
                rectangular: true
            })
        );
    }

    #[test]
    fn dismissing_ui_consumes_held_key_repeats_and_releases_before_terminal_input_resumes() {
        let mut terminal = vt::Terminal::new(20, 2, 0);
        terminal.feed(b"\x1b[>11u");
        for (physical, key) in [
            (KeyCode::Escape, vt::Key::Escape),
            (KeyCode::Enter, vt::Key::Enter),
            (KeyCode::Space, vt::Key::Char(' ')),
        ] {
            let mut consumed = HashSet::new();
            assert!(key_is_consumed(
                &mut consumed,
                physical.into(),
                ElementState::Pressed,
                true
            ));
            // Escape dismisses a dialog; Enter/Space activates its focused button.
            // That frame closes the UI before the next native keyboard event.
            for action in [vt::KeyAction::Repeat, vt::KeyAction::Release] {
                let mut event = vt::KeyEvent::new(key);
                event.action = action;
                let state = if action == vt::KeyAction::Release {
                    ElementState::Released
                } else {
                    ElementState::Pressed
                };
                // Kitty would report these orphan events if they reached the encoder.
                assert!(!terminal.encode_key(&event).is_empty());
                assert!(key_is_consumed(
                    &mut consumed,
                    physical.into(),
                    state,
                    false
                ));
            }
            assert!(consumed.is_empty());
            assert!(!key_is_consumed(
                &mut consumed,
                physical.into(),
                ElementState::Pressed,
                false
            ));
            assert!(!terminal.encode_key(&vt::KeyEvent::new(key)).is_empty());
        }
        // The shortcut that opened an editor stays consumed after the editor closes.
        let mut consumed = HashSet::from([PhysicalKey::Code(KeyCode::KeyV)]);
        assert!(key_is_consumed(
            &mut consumed,
            KeyCode::KeyV.into(),
            ElementState::Pressed,
            false
        ));
        assert!(!key_is_consumed(
            &mut consumed,
            KeyCode::KeyA.into(),
            ElementState::Pressed,
            false
        ));
        assert!(key_is_consumed(
            &mut consumed,
            KeyCode::KeyV.into(),
            ElementState::Released,
            false
        ));
        assert!(consumed.is_empty());
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Editor {
        Terminal,
        Search,
        Palette,
        TabTitle,
    }

    #[derive(Default)]
    struct InputFrame {
        context: egui::Context,
        text: String,
        popup_open: bool,
        time: f64,
    }

    impl InputFrame {
        fn cursor_rect() -> egui::Rect {
            egui::Rect::from_min_size(egui::pos2(60.0, 100.0), egui::vec2(8.0, 16.0))
        }

        fn draw(
            &mut self,
            editor: Editor,
            focus_editor: bool,
            events: Vec<egui::Event>,
        ) -> egui::PlatformOutput {
            self.time += 0.1;
            let mut raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 400.0),
                )),
                focused: true,
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let input_panel = matches!(editor, Editor::Search | Editor::Palette);
            filter_egui_events(&mut raw, input_panel || self.popup_open);
            let mut output = self.context.run_ui(raw, |root| {
                egui::Panel::top("tabs").show(root, |ui| {
                    let tab = ui.button("Terminal");
                    if editor == Editor::TabTitle {
                        egui::Popup::open_id(&self.context, egui::Popup::default_response_id(&tab));
                    } else {
                        egui::Popup::close_all(&self.context);
                    }
                    egui::Popup::context_menu(&tab)
                        .at_position(egui::pos2(10.0, 35.0))
                        .show(|ui| {
                            text_edit(ui, &mut self.text, egui::Id::new("title"), focus_editor);
                        });
                });
                self.popup_open = egui::Popup::is_any_open(&self.context);
                if editor == Editor::Search {
                    egui::Panel::bottom("search").show(root, |ui| {
                        text_edit(ui, &mut self.text, egui::Id::new("search"), focus_editor);
                    });
                }
                egui::CentralPanel::default().show(root, |ui| {
                    let response = ui.interact(
                        ui.max_rect(),
                        egui::Id::new("terminal"),
                        egui::Sense::click_and_drag(),
                    );
                    terminal_input(
                        &response,
                        Some(Self::cursor_rect()),
                        input_panel || self.popup_open,
                    );
                });
                if editor == Editor::Palette {
                    egui::Window::new("Command palette").show(&self.context, |ui| {
                        text_edit(ui, &mut self.text, egui::Id::new("palette"), focus_editor);
                    });
                }
            });
            output.textures_delta.clear();
            output.platform_output
        }
    }

    #[test]
    fn ime_follows_terminal_search_terminal_without_interrupting_idle_composition() {
        let mut frame = InputFrame::default();
        let first = frame.draw(Editor::Terminal, false, vec![]).ime.unwrap();
        assert_eq!(first.rect, InputFrame::cursor_rect());
        assert_eq!(first.cursor_rect, first.rect);
        let idle = frame.draw(Editor::Terminal, false, vec![]).ime.unwrap();
        assert!(!idle.should_interrupt_composition);

        let search = frame.draw(Editor::Search, true, vec![]).ime.unwrap();
        assert_ne!(search.rect, first.rect);
        assert!(frame.context.text_edit_focused());
        frame.draw(
            Editor::Search,
            false,
            vec![egui::Event::Text("find ".into())],
        );
        frame.draw(
            Editor::Search,
            false,
            vec![egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "仮".into(),
                active_range_chars: Some(0..1),
            })],
        );
        frame.draw(
            Editor::Search,
            false,
            vec![egui::Event::Ime(egui::ImeEvent::Commit("名".into()))],
        );
        assert_eq!(frame.text, "find 名");

        let terminal = frame.draw(Editor::Terminal, false, vec![]).ime.unwrap();
        assert_eq!(terminal.rect, first.rect);
        assert!(!frame.context.text_edit_focused());
        let idle = frame
            .draw(
                Editor::Terminal,
                false,
                vec![
                    egui::Event::Text("shell".into()),
                    egui::Event::Ime(egui::ImeEvent::Commit("字".into())),
                ],
            )
            .ime
            .unwrap();
        assert!(!idle.should_interrupt_composition);
        assert!(frame.context.input(|input| input.events.is_empty()));
        assert_eq!(frame.text, "find 名");
    }

    #[test]
    fn tab_title_popup_keeps_keyboard_and_ime_input_until_it_closes() {
        let mut frame = InputFrame::default();
        frame.draw(Editor::Terminal, false, vec![]);
        frame.draw(Editor::TabTitle, false, vec![]);
        frame.draw(Editor::TabTitle, true, vec![]);
        assert!(frame.popup_open);
        let editor = frame
            .draw(
                Editor::TabTitle,
                false,
                vec![egui::Event::Text("work ".into())],
            )
            .ime
            .unwrap();
        assert_ne!(editor.rect, InputFrame::cursor_rect());
        assert!(frame.context.text_edit_focused());
        frame.draw(
            Editor::TabTitle,
            false,
            vec![egui::Event::Ime(egui::ImeEvent::Commit("日誌".into()))],
        );
        assert_eq!(frame.text, "work 日誌");
        let terminal = frame.draw(Editor::Terminal, false, vec![]).ime.unwrap();
        assert!(!frame.popup_open);
        assert_eq!(terminal.rect, InputFrame::cursor_rect());
    }

    #[test]
    fn palette_focus_survives_its_initial_sizing_pass() {
        let mut frame = InputFrame::default();
        frame.draw(Editor::Terminal, false, vec![]);
        let mut focus_pending = true;
        let mut ime = None;
        for _ in 0..3 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 400.0),
                )),
                focused: true,
                ..Default::default()
            };
            let mut output = frame.context.run_ui(raw, |_| {
                egui::Window::new("Command palette").show(&frame.context, |ui| {
                    let focus = !ui.is_sizing_pass() && std::mem::take(&mut focus_pending);
                    text_edit(ui, &mut frame.text, egui::Id::new("palette"), focus);
                });
            });
            ime = output.platform_output.ime;
            output.textures_delta.clear();
        }
        assert!(!focus_pending);
        assert!(frame.context.text_edit_focused());
        assert!(ime.is_some());
    }

    #[test]
    fn native_edit_actions_reach_the_focused_editor_and_leave_terminal_routing_intact() {
        for editor in [Editor::Search, Editor::Palette, Editor::TabTitle] {
            let mut frame = InputFrame::default();
            frame.draw(Editor::Terminal, false, vec![]);
            frame.draw(editor, false, vec![]);
            frame.draw(editor, true, vec![]);
            frame.draw(editor, false, vec![egui::Event::Text("work 日誌".into())]);
            assert_eq!(frame.text, "work 日誌", "{editor:?}");

            let mut raw = egui::RawInput::default();
            assert!(edit_menu_action(
                &mut raw,
                &config::Action::SelectAll,
                true,
                None
            ));
            frame.draw(editor, false, raw.events);
            assert!(!frame.context.input(|input| input.key_down(egui::Key::A)));

            let mut raw = egui::RawInput::default();
            assert!(edit_menu_action(
                &mut raw,
                &config::Action::CopyToClipboard,
                true,
                None
            ));
            let output = frame.draw(editor, false, raw.events);
            assert!(
                output
                    .commands
                    .contains(&egui::OutputCommand::CopyText("work 日誌".into()))
            );

            // Let egui establish an undo point before replacing the selection.
            frame.time += 2.0;
            frame.draw(editor, false, vec![]);

            let mut raw = egui::RawInput::default();
            assert!(edit_menu_action(
                &mut raw,
                &config::Action::PasteFromClipboard,
                true,
                Some("replacement".into()),
            ));
            frame.draw(editor, false, raw.events);
            assert_eq!(frame.text, "replacement", "{editor:?}");

            for (action, expected) in [
                (config::Action::Undo, "work 日誌"),
                (config::Action::Redo, "replacement"),
            ] {
                let mut raw = egui::RawInput::default();
                assert!(edit_menu_action(&mut raw, &action, true, None));
                frame.draw(editor, false, raw.events);
                assert_eq!(frame.text, expected, "{editor:?} {action:?}");
                assert!(!frame.context.input(|input| input.key_down(egui::Key::Z)));
            }

            // Another window's focused editor must not redirect terminal actions.
            assert!(frame.context.text_edit_focused());
            for action in [
                config::Action::CopyToClipboard,
                config::Action::PasteFromClipboard,
                config::Action::SelectAll,
                config::Action::Undo,
                config::Action::Redo,
            ] {
                let mut raw = egui::RawInput::default();
                assert!(!edit_menu_action(
                    &mut raw,
                    &action,
                    false,
                    Some("terminal".into())
                ));
                assert!(raw.events.is_empty());
            }
        }
        let mut raw = egui::RawInput::default();
        assert!(edit_menu_action(
            &mut raw,
            &config::Action::PasteFromClipboard,
            true,
            None
        ));
        assert!(raw.events.is_empty());
        assert!(!edit_menu_action(
            &mut raw,
            &config::Action::NewTab,
            true,
            None
        ));
    }

    #[test]
    fn spacebar_reaches_legacy_and_kitty_terminal_encoders() {
        let key = terminal_named_key(NamedKey::Space).expect("spacebar is printable input");
        let mut event = vt::KeyEvent::new(key);
        let mut terminal = vt::Terminal::new(20, 2, 0);
        assert_eq!(terminal.encode_key(&event), b" ");
        event.action = vt::KeyAction::Repeat;
        assert_eq!(terminal.encode_key(&event), b" ");
        event.action = vt::KeyAction::Release;
        assert!(terminal.encode_key(&event).is_empty());
        event.action = vt::KeyAction::Press;
        event.modifiers.control = true;
        assert_eq!(terminal.encode_key(&event), [0]);
        terminal.feed(b"\x1b[>1u");
        assert_eq!(terminal.encode_key(&event), b"\x1b[32;5u");
    }

    #[test]
    fn composed_text_obeys_keyboard_lock_in_legacy_and_kitty_modes() {
        for mode in [b"".as_slice(), b"\x1b[>31u"] {
            let mut terminal = vt::Terminal::new(20, 2, 0);
            terminal.feed(mode);
            let text = "日本語 e\u{301} ";
            assert_eq!(terminal_text(&terminal, text.into()), text.as_bytes());
            terminal.feed(b"\x1b[2h");
            assert!(terminal_text(&terminal, text.into()).is_empty());
            terminal.feed(b"\x1b[2l");
            assert_eq!(terminal_text(&terminal, text.into()), text.as_bytes());
        }
    }

    #[test]
    fn named_keys_and_right_modifiers_keep_their_terminal_identity() {
        let unknown = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        let mut terminal = vt::Terminal::new(20, 2, 0);
        for (named, expected, encoded) in [
            (NamedKey::Help, vt::Key::Help, b"\x1b[28~".as_slice()),
            (NamedKey::ContextMenu, vt::Key::ContextMenu, b"\x1b[29~"),
        ] {
            let key = terminal_key_code(&Key::Named(named), unknown, KeyLocation::Standard);
            assert_eq!(key, expected);
            assert_eq!(terminal.encode_key(&vt::KeyEvent::new(key)), encoded);
        }
        terminal.feed(b"\x1b[>11u");
        for (named, location, physical, expected, code) in [
            (
                NamedKey::Shift,
                KeyLocation::Right,
                KeyCode::ShiftRight,
                vt::Key::ShiftRight,
                57447,
            ),
            (
                NamedKey::Control,
                KeyLocation::Right,
                KeyCode::ControlRight,
                vt::Key::ControlRight,
                57448,
            ),
            (
                NamedKey::Alt,
                KeyLocation::Right,
                KeyCode::AltRight,
                vt::Key::AltRight,
                57449,
            ),
            (
                NamedKey::Super,
                KeyLocation::Right,
                KeyCode::SuperRight,
                vt::Key::SuperRight,
                57450,
            ),
            (
                NamedKey::CapsLock,
                KeyLocation::Standard,
                KeyCode::CapsLock,
                vt::Key::CapsLock,
                57358,
            ),
            (
                NamedKey::ScrollLock,
                KeyLocation::Standard,
                KeyCode::ScrollLock,
                vt::Key::ScrollLock,
                57359,
            ),
            (
                NamedKey::NumLock,
                KeyLocation::Numpad,
                KeyCode::NumLock,
                vt::Key::NumLock,
                57360,
            ),
            (
                NamedKey::PrintScreen,
                KeyLocation::Standard,
                KeyCode::PrintScreen,
                vt::Key::PrintScreen,
                57361,
            ),
            (
                NamedKey::Pause,
                KeyLocation::Standard,
                KeyCode::Pause,
                vt::Key::Pause,
                57362,
            ),
        ] {
            let key = terminal_key_code(&Key::Named(named), physical.into(), location);
            assert_eq!(key, expected);
            assert_eq!(
                terminal_key_code(&Key::Named(named), unknown, location),
                expected
            );
            let mut event = vt::KeyEvent::new(key);
            assert_eq!(
                terminal.encode_key(&event),
                format!("\x1b[{code}u").as_bytes()
            );
            event.action = vt::KeyAction::Release;
            assert_eq!(
                terminal.encode_key(&event),
                format!("\x1b[{code};1:3u").as_bytes()
            );
        }
    }

    #[test]
    fn numpad_navigation_and_extra_keys_survive_physical_digit_mapping() {
        let mut terminal = vt::Terminal::new(20, 2, 0);
        terminal.feed(b"\x1b[>8u");
        for (named, physical, code) in [
            (NamedKey::ArrowLeft, KeyCode::Numpad4, 57417),
            (NamedKey::ArrowRight, KeyCode::Numpad6, 57418),
            (NamedKey::ArrowUp, KeyCode::Numpad8, 57419),
            (NamedKey::ArrowDown, KeyCode::Numpad2, 57420),
            (NamedKey::PageUp, KeyCode::Numpad9, 57421),
            (NamedKey::PageDown, KeyCode::Numpad3, 57422),
            (NamedKey::Home, KeyCode::Numpad7, 57423),
            (NamedKey::End, KeyCode::Numpad1, 57424),
            (NamedKey::Insert, KeyCode::Numpad0, 57425),
            (NamedKey::Delete, KeyCode::NumpadDecimal, 57426),
            (NamedKey::Clear, KeyCode::Numpad5, 57427),
        ] {
            let key = terminal_key_code(&Key::Named(named), physical.into(), KeyLocation::Numpad);
            assert_eq!(
                terminal.encode_key(&vt::KeyEvent::new(key)),
                format!("\x1b[{code}u").as_bytes()
            );
        }
        for (logical, physical, expected) in [
            (
                Key::Character("5".into()),
                KeyCode::Numpad5,
                vt::Key::Keypad(5),
            ),
            (
                Key::Character("=".into()),
                KeyCode::NumpadEqual,
                vt::Key::KeypadEqual,
            ),
            (
                Key::Character(",".into()),
                KeyCode::NumpadComma,
                vt::Key::KeypadSeparator,
            ),
            (
                Key::Named(NamedKey::Enter),
                KeyCode::NumpadEnter,
                vt::Key::KeypadEnter,
            ),
        ] {
            assert_eq!(
                terminal_key_code(&logical, physical.into(), KeyLocation::Numpad),
                expected
            );
        }
        let unknown = Key::Unidentified(winit::keyboard::NativeKey::Unidentified);
        let physical = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        let mut event =
            vt::KeyEvent::new(terminal_key_code(&unknown, physical, KeyLocation::Standard));
        assert_eq!(event.key, vt::Key::Unidentified);
        event.text = Some("é日誌".into());
        assert_eq!(terminal.encode_key(&event), "é日誌".as_bytes());
    }

    #[test]
    fn option_policy_uses_native_modifier_sides() {
        use ModifiersKeyState::{Pressed, Unknown};
        use config::OptionAsAlt::{False, Left, Right, True};
        for (left, right, expected) in [
            (Unknown, Unknown, [false, true, false, false]),
            (Pressed, Unknown, [false, true, true, false]),
            (Unknown, Pressed, [false, true, false, true]),
            (Pressed, Pressed, [false, true, true, true]),
        ] {
            assert_eq!(
                [False, True, Left, Right].map(|mode| option_as_alt(mode, left, right)),
                expected
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn modifier_key_events_apply_before_winit_updates_modifier_state() {
        for (key, flag) in [
            (vt::Key::Shift, ModifiersState::SHIFT),
            (vt::Key::ShiftRight, ModifiersState::SHIFT),
            (vt::Key::Control, ModifiersState::CONTROL),
            (vt::Key::ControlRight, ModifiersState::CONTROL),
            (vt::Key::Alt, ModifiersState::ALT),
            (vt::Key::AltRight, ModifiersState::ALT),
            (vt::Key::Super, ModifiersState::SUPER),
            (vt::Key::SuperRight, ModifiersState::SUPER),
        ] {
            let other = ModifiersState::all() - flag;
            assert_eq!(
                key_modifiers(key, ElementState::Pressed, other.into()),
                ModifiersState::all()
            );
            assert_eq!(
                key_modifiers(key, ElementState::Released, ModifiersState::all().into()),
                other
            );
        }
        let held = ModifiersState::SHIFT | ModifiersState::ALT;
        assert_eq!(
            key_modifiers(vt::Key::Char('x'), ElementState::Released, held.into()),
            held
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn shifted_control_keys_retain_modifiers_in_kitty_mode() {
        let options = vt::KeyEncodeOptions::default();
        for (named, text, code) in [
            (NamedKey::Tab, "\t", 9),
            (NamedKey::Enter, "\r", 13),
            (NamedKey::Backspace, "\u{8}", 127),
        ] {
            let unmodified = Key::Named(named);
            let text = terminal_key_text(Some(text));
            let mut event = vt::KeyEvent::new(terminal_named_key(named).unwrap());
            event.text = text.map(str::to_owned);
            event.modifiers = terminal_modifiers(ModifiersState::SHIFT);
            event.consumed_modifiers =
                consumed_modifiers(&unmodified, text, ModifiersState::SHIFT, options);
            let mut terminal = vt::Terminal::new(20, 2, 0);
            if named == NamedKey::Tab {
                assert_eq!(terminal.encode_key(&event), b"\x1b[Z");
            }
            terminal.feed(b"\x1b[>1u");
            assert_eq!(
                terminal.encode_key_with_options(&event, options),
                format!("\x1b[{code};2u").as_bytes(),
                "{named:?} must retain Shift instead of producing an unmodified key"
            );
        }
        // Shift still contributes to printable text, including space and Unicode.
        for (base, shifted) in [('a', "A"), (' ', " "), ('é', "É")] {
            let text = terminal_key_text(Some(shifted));
            let mut event = vt::KeyEvent::new(vt::Key::Char(base));
            event.text = text.map(str::to_owned);
            event.modifiers = terminal_modifiers(ModifiersState::SHIFT);
            event.consumed_modifiers = consumed_modifiers(
                &Key::Character(base.to_string().into()),
                text,
                ModifiersState::SHIFT,
                options,
            );
            let mut terminal = vt::Terminal::new(20, 2, 0);
            terminal.feed(b"\x1b[>1u");
            assert_eq!(terminal.encode_key(&event), shifted.as_bytes());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn option_translation_consumes_ascii_and_unicode_without_consuming_terminal_alt() {
        for (base, composed) in [('8', "{"), ('e', "é")] {
            for alt in [false, true] {
                let options = vt::KeyEncodeOptions {
                    macos_option_as_alt: alt,
                };
                let unmodified = Key::Character(base.to_string().into());
                // Winit's OptionAsAlt setting already chooses which text to produce.
                let text = if alt {
                    base.to_string()
                } else {
                    composed.into()
                };
                let mut event = vt::KeyEvent::new(vt::Key::Char(base));
                event.text = Some(text.clone());
                event.modifiers = terminal_modifiers(ModifiersState::ALT);
                event.consumed_modifiers =
                    consumed_modifiers(&unmodified, Some(&text), ModifiersState::ALT, options);
                assert_eq!(event.consumed_modifiers.alt, !alt);
                let mut terminal = vt::Terminal::new(20, 2, 0);
                assert_eq!(
                    terminal.encode_key_with_options(&event, options),
                    if alt {
                        format!("\x1b{base}")
                    } else {
                        text.clone()
                    }
                    .as_bytes()
                );
                terminal.feed(b"\x1b[>24u");
                let expected = if alt {
                    format!("\x1b[{};3u", base as u32)
                } else {
                    format!(
                        "\x1b[{};3;{}u",
                        base as u32,
                        text.chars().next().unwrap() as u32
                    )
                };
                assert_eq!(
                    terminal.encode_key_with_options(&event, options),
                    expected.as_bytes()
                );
            }
        }
        let consumed = consumed_modifiers(
            &Key::Named(NamedKey::Space),
            Some(" "),
            ModifiersState::SHIFT | ModifiersState::CONTROL | ModifiersState::SUPER,
            vt::KeyEncodeOptions::default(),
        );
        assert!(consumed.shift);
        assert!(!consumed.control && !consumed.super_key);
        assert_eq!(
            consumed_modifiers(
                &Key::Named(NamedKey::Space),
                None,
                ModifiersState::SHIFT | ModifiersState::ALT,
                vt::KeyEncodeOptions::default()
            ),
            vt::Modifiers::default()
        );
    }

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
