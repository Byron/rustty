//! A low-level keyboard hook preserves unconsumed and scan-code global bindings.
//! It only translates configured actions; processing/rendering stays on the UI thread.
use super::{EventSink, PlatformEvent, err};
use ::windows::Win32::{
    Foundation::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
use rustty::config::{Config, KeyBinding, KeyTrigger, Modifiers};
use std::{
    cell::RefCell,
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
};

struct Context {
    callback: EventSink,
    bindings: Arc<Mutex<Vec<KeyBinding>>>,
    consumed: [bool; 256],
}
impl Context {
    fn dispatch(
        &mut self,
        key: usize,
        physical: Option<&str>,
        text: &str,
        modifiers: Modifiers,
    ) -> bool {
        let bindings = self.bindings.lock().unwrap_or_else(|e| e.into_inner());
        let binding = bindings.iter().rev().find(|binding| {
            binding
                .trigger
                .first()
                .is_some_and(|trigger| matches(trigger, physical, text, modifiers))
        });
        let Some(binding) = binding else {
            return false;
        };
        // Preserve broadcast flags and dispatch the entire chain on the UI thread.
        (self.callback)(PlatformEvent::GlobalHotkey(binding.clone()));
        self.consumed[key] = binding.flags.consumed;
        binding.flags.consumed
    }
}
thread_local! { static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) }; }
pub(super) struct GlobalKeys {
    bindings: Arc<Mutex<Vec<KeyBinding>>>,
    thread: Option<JoinHandle<()>>,
    id: u32,
}
impl GlobalKeys {
    pub fn new(callback: EventSink) -> Result<Self, String> {
        let bindings = Arc::new(Mutex::new(Vec::new()));
        let thread_bindings = bindings.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("rustty-global-keys".into())
            .spawn(move || {
                CONTEXT.with(|context| {
                    *context.borrow_mut() = Some(Context {
                        callback,
                        bindings: thread_bindings,
                        consumed: [false; 256],
                    })
                });
                // Creating the message queue before publishing its id makes shutdown reliable.
                let mut message = MSG::default();
                unsafe {
                    let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
                }
                let hook = unsafe {
                    SetWindowsHookExW(
                        WH_KEYBOARD_LL,
                        Some(key_event),
                        GetModuleHandleW(None)
                            .ok()
                            .map(|module| HINSTANCE(module.0)),
                        0,
                    )
                };
                let hook = match hook {
                    Ok(hook) => hook,
                    Err(error) => {
                        let _ = sender.send(Err(error.to_string()));
                        return;
                    }
                };
                let _ = sender.send(Ok(unsafe { GetCurrentThreadId() }));
                unsafe {
                    while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                    let _ = UnhookWindowsHookEx(hook);
                }
                CONTEXT.with(|context| context.borrow_mut().take());
            })
            .map_err(err)?;
        let id = receiver.recv().map_err(err)??;
        Ok(Self {
            bindings,
            thread: Some(thread),
            id,
        })
    }
    pub fn update(&self, config: &Config) {
        *self.bindings.lock().unwrap_or_else(|e| e.into_inner()) = config
            .keybinds
            .iter()
            .filter(|binding| binding.flags.global && binding.table.is_none())
            .cloned()
            .collect();
    }
}
impl Drop for GlobalKeys {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
unsafe extern "system" fn key_event(code: i32, kind: WPARAM, data: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, kind, data) };
    }
    let event = unsafe { &*(data.0 as *const KBDLLHOOKSTRUCT) };
    let consumed = CONTEXT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(context) = slot.as_mut() else {
            return false;
        };
        let key = event.vkCode as usize;
        if key >= 256 {
            return false;
        }
        if matches!(kind.0 as u32, WM_KEYUP | WM_SYSKEYUP) {
            return std::mem::take(&mut context.consumed[key]);
        }
        if !matches!(kind.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
            return false;
        }
        // The action may focus Rustty or reload bindings before this key is released.
        // Keep repeats with their consumed press instead of leaking them to either app.
        if context.consumed[key] {
            return true;
        }
        let foreground = unsafe { GetForegroundWindow() };
        let mut process = 0;
        let thread = unsafe { GetWindowThreadProcessId(foreground, Some(&mut process)) };
        if process == std::process::id() {
            return false;
        }
        if context
            .bindings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
        {
            return false;
        }
        let down = |key: VIRTUAL_KEY| unsafe { GetAsyncKeyState(key.0 as i32) } < 0;
        let modifiers = Modifiers {
            shift: down(VK_SHIFT),
            control: down(VK_CONTROL),
            alt: down(VK_MENU),
            super_key: down(VK_LWIN) || down(VK_RWIN),
        };
        let physical = physical(event.scanCode, event.flags.contains(LLKHF_EXTENDED));
        let layout = unsafe { GetKeyboardLayout(thread) };
        let mut state = [0u8; 256];
        // Like charactersIgnoringModifiers on macOS, retain Shift for punctuation,
        // while removing Control/Alt so they cannot turn text into control bytes.
        if modifiers.shift {
            state[VK_SHIFT.0 as usize] = 0x80;
        }
        let mut text = [0u16; 16];
        // Bit 2 avoids altering the foreground application's dead-key state (Windows 10+).
        let count = unsafe {
            ToUnicodeEx(
                event.vkCode,
                event.scanCode,
                &state,
                &mut text,
                4,
                Some(layout),
            )
        };
        let text = if count > 0 {
            String::from_utf16_lossy(&text[..(count as usize).min(text.len())])
        } else {
            String::new()
        };
        context.dispatch(key, physical, &text, modifiers)
    });
    if consumed {
        LRESULT(1)
    } else {
        unsafe { CallNextHookEx(None, code, kind, data) }
    }
}
fn matches(trigger: &KeyTrigger, physical: Option<&str>, text: &str, modifiers: Modifiers) -> bool {
    if trigger.modifiers != modifiers {
        return false;
    }
    if trigger.key == "catch_all" {
        return true;
    }
    let named = trigger.key.starts_with("key_") || trigger.key.starts_with("digit_");
    let key = trigger
        .key
        .strip_prefix("key_")
        .or_else(|| trigger.key.strip_prefix("digit_"))
        .unwrap_or(&trigger.key);
    let key = if key == "backquote" { "`" } else { key };
    if trigger.physical || named {
        return physical == Some(key);
    }
    if key == "space" {
        text == " "
    } else if key.chars().count() == 1 {
        key.to_lowercase() == text.to_lowercase()
    } else {
        physical == Some(key)
    }
}
fn physical(scan: u32, extended: bool) -> Option<&'static str> {
    Some(match (scan, extended) {
        (0x1c, true) => "kp_enter",
        (0x35, true) => "kp_divide",
        (0x37, false) => "kp_multiply",
        (0x37, true) => "print_screen",
        (0x45, true) => "num_lock",
        (0x45, false) => "pause",
        (0x46, _) => "scroll_lock",
        (0x4a, _) => "kp_subtract",
        (0x4e, _) => "kp_add",
        (0x47, false) => "kp_7",
        (0x48, false) => "kp_8",
        (0x49, false) => "kp_9",
        (0x4b, false) => "kp_4",
        (0x4c, false) => "kp_5",
        (0x4d, false) => "kp_6",
        (0x4f, false) => "kp_1",
        (0x50, false) => "kp_2",
        (0x51, false) => "kp_3",
        (0x52, false) => "kp_0",
        (0x53, false) => "kp_decimal",
        (0x64, _) => "f13",
        (0x65, _) => "f14",
        (0x66, _) => "f15",
        (0x67, _) => "f16",
        (0x68, _) => "f17",
        (0x69, _) => "f18",
        (0x6a, _) => "f19",
        (0x6b, _) => "f20",
        (0x6c, _) => "f21",
        (0x6d, _) => "f22",
        (0x6e, _) => "f23",
        (0x76, _) => "f24",
        (0x01, _) => "escape",
        (0x02, _) => "1",
        (0x03, _) => "2",
        (0x04, _) => "3",
        (0x05, _) => "4",
        (0x06, _) => "5",
        (0x07, _) => "6",
        (0x08, _) => "7",
        (0x09, _) => "8",
        (0x0a, _) => "9",
        (0x0b, _) => "0",
        (0x0c, _) => "-",
        (0x0d, _) => "=",
        (0x0e, _) => "backspace",
        (0x0f, _) => "tab",
        (0x10, _) => "q",
        (0x11, _) => "w",
        (0x12, _) => "e",
        (0x13, _) => "r",
        (0x14, _) => "t",
        (0x15, _) => "y",
        (0x16, _) => "u",
        (0x17, _) => "i",
        (0x18, _) => "o",
        (0x19, _) => "p",
        (0x1a, _) => "[",
        (0x1b, _) => "]",
        (0x1c, _) => "enter",
        (0x1e, _) => "a",
        (0x1f, _) => "s",
        (0x20, _) => "d",
        (0x21, _) => "f",
        (0x22, _) => "g",
        (0x23, _) => "h",
        (0x24, _) => "j",
        (0x25, _) => "k",
        (0x26, _) => "l",
        (0x27, _) => ";",
        (0x28, _) => "'",
        (0x29, _) => "`",
        (0x2b, _) => "\\",
        (0x2c, _) => "z",
        (0x2d, _) => "x",
        (0x2e, _) => "c",
        (0x2f, _) => "v",
        (0x30, _) => "b",
        (0x31, _) => "n",
        (0x32, _) => "m",
        (0x33, _) => ",",
        (0x34, _) => ".",
        (0x35, _) => "/",
        (0x39, _) => "space",
        (0x3b, _) => "f1",
        (0x3c, _) => "f2",
        (0x3d, _) => "f3",
        (0x3e, _) => "f4",
        (0x3f, _) => "f5",
        (0x40, _) => "f6",
        (0x41, _) => "f7",
        (0x42, _) => "f8",
        (0x43, _) => "f9",
        (0x44, _) => "f10",
        (0x57, _) => "f11",
        (0x58, _) => "f12",
        (0x47, true) => "home",
        (0x48, true) => "arrow_up",
        (0x49, true) => "page_up",
        (0x4b, true) => "arrow_left",
        (0x4d, true) => "arrow_right",
        (0x4f, true) => "end",
        (0x50, true) => "arrow_down",
        (0x51, true) => "page_down",
        (0x52, true) => "insert",
        (0x53, true) => "delete",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustty::config::Action;

    #[test]
    fn global_bindings_keep_action_chains_and_broadcast_flags() {
        let mut binding = KeyBinding::parse("global:ctrl+a=text:first").unwrap();
        binding.actions.push(Action::Text(b"second".to_vec()));
        let (sender, receiver) = mpsc::channel();
        let mut context = Context {
            callback: Arc::new(move |event| sender.send(event).unwrap()),
            bindings: Arc::new(Mutex::new(vec![binding.clone()])),
            consumed: [false; 256],
        };
        let key = VK_A.0 as usize;
        assert!(!context.dispatch(key, Some("a"), "a", Modifiers::default()));
        assert!(receiver.try_recv().is_err());
        assert!(!context.consumed[key]);
        assert!(context.dispatch(key, Some("a"), "a", binding.trigger[0].modifiers));
        let events = receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(events.len(), 1, "a global action chain is one UI event");
        let PlatformEvent::GlobalHotkey(actual) = &events[0] else {
            panic!("global shortcuts must retain their complete binding");
        };
        assert_eq!(actual, &binding);
        assert!(actual.flags.all);
        assert!(context.consumed[key]);
    }

    #[test]
    fn consumed_global_keys_remain_consumed_until_release() {
        for (down, up) in [(WM_KEYDOWN, WM_KEYUP), (WM_SYSKEYDOWN, WM_SYSKEYUP)] {
            let mut context = Context {
                callback: Arc::new(|_| {}),
                bindings: Arc::new(Mutex::new(Vec::new())),
                consumed: [false; 256],
            };
            context.consumed[VK_A.0 as usize] = true;
            CONTEXT.with(|slot| *slot.borrow_mut() = Some(context));
            let event = KBDLLHOOKSTRUCT {
                vkCode: u32::from(VK_A.0),
                scanCode: 0x1e,
                ..Default::default()
            };
            let data = LPARAM((&event as *const KBDLLHOOKSTRUCT) as isize);
            // Call the hook directly; no key is injected into the desktop.
            let repeated = unsafe { key_event(HC_ACTION as i32, WPARAM(down as usize), data) };
            let released = unsafe { key_event(HC_ACTION as i32, WPARAM(up as usize), data) };
            let context = CONTEXT.with(|slot| slot.borrow_mut().take().unwrap());
            assert_eq!(repeated, LRESULT(1));
            assert_eq!(released, LRESULT(1));
            assert!(!context.consumed[VK_A.0 as usize]);
        }
    }

    #[test]
    fn scan_codes_distinguish_navigation_from_keypad() {
        assert_eq!(physical(0x48, true), Some("arrow_up"));
        assert_ne!(physical(0x48, false), Some("arrow_up"));
        assert_eq!(physical(0x29, false), Some("`"));
    }
    #[test]
    fn layout_keys_and_physical_keys_have_distinct_global_matches() {
        let logical = KeyTrigger::parse("ctrl+a").unwrap();
        let scan = KeyTrigger::parse("physical:ctrl+a").unwrap();
        assert!(matches(&logical, Some("q"), "a", logical.modifiers));
        assert!(!matches(&scan, Some("q"), "a", scan.modifiers));
        assert!(matches(&scan, Some("a"), "q", scan.modifiers));
        assert!(!matches(&logical, Some("a"), "a", Modifiers::default()));
        let plus = KeyTrigger::parse("ctrl+shift++").unwrap();
        assert!(matches(&plus, Some("="), "+", plus.modifiers));
    }
}
