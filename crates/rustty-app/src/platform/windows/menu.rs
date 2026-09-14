use super::{EventSink, PlatformEvent, err};
use ::windows::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{HACCEL, MSG, TranslateAcceleratorW},
};
use muda::{
    AboutMetadata, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::{Key, KeyAccelerator, Modifiers},
};
use rustty::config::{Action, Config, Direction, KeyTrigger};
use std::{cell::RefCell, ffi::c_void};

thread_local! { static ACTIVE_MENU: RefCell<Option<Menu>> = const { RefCell::new(None) }; }

pub(super) fn message_hook(message: *const c_void) -> bool {
    if message.is_null() {
        return false;
    }
    let message = unsafe { &*message.cast::<MSG>() };
    if super::notifications::message_hook(message) {
        return true;
    }
    ACTIVE_MENU.with(|slot| {
        let slot = slot.borrow();
        let Some(menu) = slot.as_ref() else {
            return false;
        };
        // Winit lends MSG for the duration of its synchronous hook.
        unsafe { TranslateAcceleratorW(message.hwnd, HACCEL(menu.haccel() as _), message) != 0 }
    })
}

pub(super) struct NativeMenu {
    menu: Menu,
    actions: Vec<(MenuItem, Action)>,
}
impl NativeMenu {
    pub fn new(callback: EventSink) -> Result<Self, String> {
        let menu = Menu::new();
        let mut actions = Vec::new();
        let file = Submenu::new("&File", true);
        for (name, action) in [
            ("New Window", Action::NewWindow),
            ("New Tab", Action::NewTab),
            ("Open Saved Layout…", Action::OpenLayout),
            ("Split Right", Action::NewSplit(Direction::Right)),
            ("Split Down", Action::NewSplit(Direction::Down)),
            ("Close Surface", Action::CloseSurface),
            ("Close Tab", Action::CloseTab),
            ("Close Window", Action::CloseWindow),
            ("Settings…", Action::OpenConfig),
            ("Reload Configuration", Action::ReloadConfig),
            ("Exit", Action::Quit),
        ] {
            append(&file, &mut actions, name, action)?;
        }
        let edit = Submenu::new("&Edit", true);
        for (name, action) in [
            ("Undo", Action::Undo),
            ("Redo", Action::Redo),
            ("Copy", Action::CopyToClipboard),
            ("Paste", Action::PasteFromClipboard),
            ("Select All", Action::SelectAll),
            ("Find…", Action::StartSearch),
        ] {
            append(&edit, &mut actions, name, action)?;
        }
        let view = Submenu::new("&View", true);
        for (name, action) in [
            ("Command Palette…", Action::ToggleCommandPalette),
            ("Toggle Full Screen", Action::ToggleFullscreen),
            ("Toggle Split Zoom", Action::ToggleSplitZoom),
            ("Toggle Quadrant Zoom", Action::ToggleQuadrantZoom),
            ("Equalize Splits", Action::EqualizeSplits),
            ("Increase Font Size", Action::IncreaseFontSize(1.0)),
            ("Decrease Font Size", Action::DecreaseFontSize(1.0)),
            ("Reset Font Size", Action::ResetFontSize),
        ] {
            append(&view, &mut actions, name, action)?;
        }
        let window = Submenu::new("&Window", true);
        window
            .append_items(&[
                &PredefinedMenuItem::minimize(None),
                &PredefinedMenuItem::maximize(None),
            ])
            .map_err(err)?;
        for (name, action) in [
            ("Previous Tab", Action::PreviousTab),
            ("Next Tab", Action::NextTab),
            ("Previous Split", Action::GotoSplit(Direction::Previous)),
            ("Next Split", Action::GotoSplit(Direction::Next)),
            ("Toggle Quick Terminal", Action::ToggleQuickTerminal),
        ] {
            append(&window, &mut actions, name, action)?;
        }
        let help = Submenu::new("&Help", true);
        help.append(&PredefinedMenuItem::about(
            Some("About Rustty"),
            Some(AboutMetadata {
                name: Some("Rustty".into()),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                copyright: Some("Rustty contributors; based on Ghostty (MIT)".into()),
                ..Default::default()
            }),
        ))
        .map_err(err)?;
        menu.append_items(&[&file, &edit, &view, &window, &help])
            .map_err(err)?;
        let routed = actions
            .iter()
            .map(|(_, action)| action.clone())
            .collect::<Vec<_>>();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some(action) = event
                .id
                .0
                .strip_prefix("rustty-action-")
                .and_then(|s| s.parse::<usize>().ok())
                .and_then(|i| routed.get(i))
            {
                callback(PlatformEvent::Action(action.clone()));
            }
        }));
        ACTIVE_MENU.with(|slot| *slot.borrow_mut() = Some(menu.clone()));
        Ok(Self { menu, actions })
    }
    pub fn attach(&self, hwnd: HWND) -> Result<(), String> {
        unsafe { self.menu.init_for_hwnd(hwnd.0 as isize) }.map_err(err)
    }
    pub fn detach(&self, hwnd: HWND) {
        let _ = unsafe { self.menu.remove_for_hwnd(hwnd.0 as isize) };
    }
    pub fn update(&self, config: &Config) -> Result<(), String> {
        for (item, action) in &self.actions {
            let accelerator = config
                .keybinds
                .iter()
                .rev()
                .find(|binding| {
                    binding.table.is_none()
                        && !binding.flags.performable
                        && !binding.flags.all
                        && binding.trigger.len() == 1
                        && binding.actions == [action.clone()]
                })
                .and_then(|binding| accelerator(&binding.trigger[0]));
            item.set_key_accelerator(accelerator).map_err(err)?;
        }
        Ok(())
    }
}
impl Drop for NativeMenu {
    fn drop(&mut self) {
        ACTIVE_MENU.with(|slot| slot.borrow_mut().take());
    }
}
fn append(
    menu: &Submenu,
    actions: &mut Vec<(MenuItem, Action)>,
    label: &str,
    action: Action,
) -> Result<(), String> {
    let item = MenuItem::with_id(
        format!("rustty-action-{}", actions.len()),
        label,
        true,
        None,
    );
    menu.append(&item).map_err(err)?;
    actions.push((item, action));
    Ok(())
}
fn accelerator(trigger: &KeyTrigger) -> Option<KeyAccelerator> {
    // Win32 accelerator tables do not represent the Windows key or scan-code bindings.
    if trigger.physical || trigger.modifiers.super_key {
        return None;
    }
    let mut modifiers = Modifiers::empty();
    if trigger.modifiers.control {
        modifiers |= Modifiers::CONTROL;
    }
    if trigger.modifiers.shift {
        modifiers |= Modifiers::SHIFT;
    }
    if trigger.modifiers.alt {
        modifiers |= Modifiers::ALT;
    }
    if modifiers.is_empty() {
        return None;
    }
    let key = match trigger.key.as_str() {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "escape" => Key::Escape,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "arrow_left" => Key::ArrowLeft,
        "arrow_right" => Key::ArrowRight,
        "arrow_up" => Key::ArrowUp,
        "arrow_down" => Key::ArrowDown,
        "home" => Key::Home,
        "end" => Key::End,
        "page_up" => Key::PageUp,
        "page_down" => Key::PageDown,
        "space" => Key::Character(" ".into()),
        "backquote" => Key::Character("`".into()),
        key if key.chars().count() == 1 => Key::Character(key.into()),
        _ => return None,
    };
    Some(KeyAccelerator::new(Some(modifiers), key))
}
