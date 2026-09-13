//! Route input to the active editor and keep physical keys separate from composed text.
use rustty::{config, vt};
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey},
    platform::modifier_supplement::KeyEventExtModifierSupplement,
};

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

pub fn terminal_key(
    event: &KeyEvent,
    mods: ModifiersState,
    composing: bool,
) -> Option<vt::KeyEvent> {
    let unmodified = event.key_without_modifiers();
    let key = match &unmodified {
        Key::Character(text) => vt::Key::Char(text.chars().next()?),
        Key::Named(key) => terminal_named_key(*key)?,
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

    #[derive(Clone, Copy, PartialEq)]
    enum Editor {
        Terminal,
        Search,
        TabTitle,
    }

    #[derive(Default)]
    struct InputFrame {
        context: egui::Context,
        text: String,
        popup_open: bool,
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
            let mut raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 400.0),
                )),
                focused: true,
                events,
                ..Default::default()
            };
            filter_egui_events(&mut raw, editor == Editor::Search || self.popup_open);
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
                        editor == Editor::Search || self.popup_open,
                    );
                });
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
