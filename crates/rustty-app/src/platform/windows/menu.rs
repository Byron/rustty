use super::{EventSink, PlatformEvent, err};
use ::windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::MSG};
use muda::{
    AboutMetadata, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::{Key, KeyAccelerator, Modifiers},
};
use rustty::config::{Action, Config, Direction, KeyTrigger};
use std::ffi::c_void;

pub(super) fn message_hook(message: *const c_void) -> bool {
    if message.is_null() {
        return false;
    }
    // Winit lends MSG for the duration of its synchronous hook.
    let message = unsafe { &*message.cast::<MSG>() };
    // Menu accelerators provide labels; the app router owns keyboard handling,
    // including unconsumed bindings, physical overrides, and key sequences.
    super::notifications::message_hook(message)
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

#[cfg(test)]
mod tests {
    use super::*;
    use ::windows::{
        Win32::{
            Foundation::{LPARAM, WPARAM},
            UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
        },
        core::w,
    };
    use rustty::config::KeyBinding;
    use std::sync::{Arc, Mutex};

    struct HiddenWindow(HWND);
    impl HiddenWindow {
        fn new(style: WINDOW_STYLE) -> Self {
            Self(unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!("Rustty menu regression"),
                    style,
                    0,
                    0,
                    320,
                    200,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap()
            })
        }
    }
    impl Drop for HiddenWindow {
        fn drop(&mut self) {
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }
    struct Fixture {
        menu: NativeMenu,
        normal: HiddenWindow,
        quick: HiddenWindow,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.menu.detach(self.normal.0);
        }
    }
    struct KeyboardState([u8; 256]);
    impl KeyboardState {
        fn control_shift() -> Self {
            let mut saved = [0; 256];
            unsafe { GetKeyboardState(&mut saved) }.unwrap();
            let mut state = [0; 256];
            state[VK_CONTROL.0 as usize] = 0x80;
            state[VK_SHIFT.0 as usize] = 0x80;
            // SetKeyboardState affects this thread's queue, never the user's global input.
            unsafe { SetKeyboardState(&state) }.unwrap();
            Self(saved)
        }
    }
    impl Drop for KeyboardState {
        fn drop(&mut self) {
            let _ = unsafe { SetKeyboardState(&self.0) };
        }
    }

    #[test]
    fn native_menu_labels_do_not_intercept_host_shortcuts() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let fixture = Fixture {
            menu: NativeMenu::new(Arc::new(move |event| {
                if let PlatformEvent::Action(action) = event {
                    sink.lock().unwrap().push(action);
                }
            }))
            .unwrap(),
            normal: HiddenWindow::new(WS_OVERLAPPEDWINDOW),
            quick: HiddenWindow::new(WS_POPUP),
        };
        let mut config = Config::default();
        config.keybinds.clear();
        config.keybinds.extend(
            [
                "unconsumed:ctrl+shift+n=new_window",
                "ctrl+shift+t=new_tab",
                "ctrl+k>ctrl+shift+t=text:sequence",
                "physical:ctrl+shift+t=text:physical",
            ]
            .map(|text| KeyBinding::parse(text).unwrap()),
        );
        fixture.menu.update(&config).unwrap();
        // Muda 0.19 does not populate the root accelerator store when children
        // precede their submenu's attachment. Reinsert these items after attachment
        // so this regression exercises a real table, including after future upgrades.
        let muda::MenuItemKind::Submenu(submenu) = fixture.menu.menu.items().remove(0) else {
            panic!("File submenu");
        };
        for index in 0..2 {
            let item = &fixture.menu.actions[index].0;
            submenu.remove(item).unwrap();
            submenu.insert(item, index).unwrap();
        }
        fixture.menu.attach(fixture.normal.0).unwrap();
        let file = unsafe { GetSubMenu(GetMenu(fixture.normal.0), 0) };
        let mut text = [0u16; 128];
        let length = unsafe { GetMenuStringW(file, 0, Some(&mut text), MF_BYPOSITION) };
        let label = String::from_utf16(&text[..length as usize]).unwrap();
        let (title, shortcut) = label.split_once('\t').expect("native shortcut label");
        assert_eq!(title, "New Window");
        assert!(!shortcut.is_empty());
        assert!(unsafe { GetMenu(fixture.quick.0) }.0.is_null());

        let _keyboard = KeyboardState::control_shift();
        assert_eq!(
            unsafe { CopyAcceleratorTableW(HACCEL(fixture.menu.menu.haccel() as _), None) },
            2
        );
        for (hwnd, name) in [(fixture.normal.0, "ordinary"), (fixture.quick.0, "quick")] {
            for (key, reason) in [
                (b'N', "unconsumed binding"),
                (b'T', "physical override or sequence continuation"),
            ] {
                for kind in [WM_KEYDOWN, WM_KEYUP] {
                    let message = MSG {
                        hwnd,
                        message: kind,
                        wParam: WPARAM(key as usize),
                        ..Default::default()
                    };
                    assert!(
                        !message_hook((&message as *const MSG).cast()),
                        "{name}: {reason} must reach host routing"
                    );
                }
            }
            let alt = MSG {
                hwnd,
                message: WM_SYSKEYDOWN,
                wParam: WPARAM(VK_MENU.0 as usize),
                ..Default::default()
            };
            assert!(
                !message_hook((&alt as *const MSG).cast()),
                "Alt menu navigation must reach Windows"
            );
        }
        assert!(
            events.lock().unwrap().is_empty(),
            "keyboard routing must not emit native menu actions"
        );

        let command = unsafe { GetMenuItemID(file, 0) };
        unsafe {
            SendMessageW(
                fixture.normal.0,
                WM_COMMAND,
                Some(WPARAM(command as usize)),
                Some(LPARAM(0)),
            );
        }
        assert_eq!(
            *events.lock().unwrap(),
            [Action::NewWindow],
            "native menu clicks still dispatch actions"
        );
    }
}
