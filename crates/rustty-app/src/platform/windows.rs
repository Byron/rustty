//! Windows 11 desktop integration. All HWND/COM ownership stays on the event thread.
use ::windows::{
    UI::ViewManagement::{UIColorType, UISettings},
    Win32::{
        Foundation::*,
        Graphics::Gdi::ScreenToClient,
        Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom},
        System::{
            Registry::*,
            WinRT::{RO_INIT_SINGLETHREADED, RoInitialize, RoUninitialize},
        },
        UI::{Input::KeyboardAndMouse::GetDoubleClickTime, Shell::*, WindowsAndMessaging::*},
    },
    core::{HRESULT, PCWSTR, w},
};
use rustty::config::{Action, Config, KeyBinding};
use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::{OsStr, OsString},
    io,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    event_loop::EventLoopBuilder,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::{Theme, Window, WindowId},
};

mod clipboard;
mod keys;
mod menu;
mod notifications;
mod quick;

pub const APP_USER_MODEL_ID: &str = "app.rustty";
pub const TOAST_ACTIVATOR_CLSID: &str = "{9CB270BB-CC8F-4C71-ACF2-4D632A9A7E26}";

#[derive(Clone, Debug)]
pub enum PlatformEvent {
    Action(Action),
    GlobalHotkey(KeyBinding),
    NotificationClicked(u64),
}
type EventSink = Arc<dyn Fn(PlatformEvent) + Send + Sync>;

/// Install before building Winit's event loop. The handler only consumes menu accelerators.
pub fn configure_event_loop<T>(builder: &mut EventLoopBuilder<T>) {
    use winit::platform::windows::EventLoopBuilderExtWindows;
    builder.with_msg_hook(menu::message_hook);
}

/// Handle Windows COM's -Embedding launch without starting a terminal session.
pub fn run_toast_activator() -> Result<(), String> {
    notifications::run_toast_activator()
}

pub struct Platform {
    menu: menu::NativeMenu,
    keys: keys::GlobalKeys,
    notifications: notifications::Notifications,
    quick: RefCell<HashMap<WindowId, quick::QuickWindow>>,
    windows: RefCell<HashMap<WindowId, (isize, bool)>>,
    selection: RefCell<Vec<clipboard::Content>>,
    badge: RefCell<usize>,
    _runtime: Runtime,
}

// This field is declared last so COM-dependent fields are dropped first.
struct Runtime(bool);
impl Drop for Runtime {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                RoUninitialize();
            }
        }
    }
}

impl Platform {
    pub fn double_click_interval() -> Duration {
        Duration::from_millis(u64::from(unsafe { GetDoubleClickTime() }))
    }

    pub fn new(callback: EventSink, config: &Config) -> Result<Self, String> {
        // Winit's OLE drag/drop uses STA too. Balance only our successful initialization.
        let runtime = Runtime(unsafe { RoInitialize(RO_INIT_SINGLETHREADED) }.is_ok());
        unsafe { SetCurrentProcessExplicitAppUserModelID(w!("app.rustty")) }.map_err(err)?;
        let mut platform = Self {
            menu: menu::NativeMenu::new(callback.clone())?,
            keys: keys::GlobalKeys::new(callback.clone())?,
            notifications: notifications::Notifications::new(callback)?,
            quick: RefCell::default(),
            windows: RefCell::default(),
            selection: RefCell::default(),
            badge: RefCell::new(0),
            _runtime: runtime,
        };
        platform.update_config(config)?;
        Ok(platform)
    }

    pub fn update_config(&mut self, config: &Config) -> Result<(), String> {
        self.menu.update(config)?;
        self.keys.update(config);
        Ok(())
    }

    pub fn secure_random(bytes: &mut [u8]) -> io::Result<()> {
        unsafe { BCryptGenRandom(None, bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }
            .ok()
            .map_err(io::Error::other)
    }

    pub fn cursor_position(window: &Window) -> Result<[f32; 2], String> {
        let mut point = POINT::default();
        unsafe {
            GetCursorPos(&mut point).map_err(|error| format!("GetCursorPos: {error}"))?;
            ScreenToClient(hwnd(window)?, &mut point)
                .ok()
                .map_err(|error| format!("ScreenToClient: {error}"))?;
        }
        let scale = window.scale_factor() as f32;
        Ok([point.x as f32 / scale, point.y as f32 / scale])
    }

    pub fn window_diagnostics(window: &Window) -> Result<String, String> {
        let hwnd = hwnd(window)?;
        let mut rect = RECT::default();
        unsafe {
            GetWindowRect(hwnd, &mut rect).map_err(err)?;
            Ok(format!(
                "visible={}, foreground={}, minimized={}, rect={rect:?}, scale={}",
                IsWindowVisible(hwnd).as_bool(),
                GetForegroundWindow() == hwnd,
                IsIconic(hwnd).as_bool(),
                window.scale_factor()
            ))
        }
    }

    pub fn system_theme() -> Option<Theme> {
        let mut light = 1u32;
        let mut len = 4u32;
        let result = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
                w!("AppsUseLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some((&mut light as *mut u32).cast()),
                Some(&mut len),
            )
        };
        (result == ERROR_SUCCESS).then_some(if light == 0 {
            Theme::Dark
        } else {
            Theme::Light
        })
    }

    pub fn accent_color(&self) -> Option<[u8; 3]> {
        let color = UISettings::new()
            .ok()?
            .GetColorValue(UIColorType::Accent)
            .ok()?;
        Some([color.R, color.G, color.B])
    }

    pub fn configure_window(
        &self,
        window: &Window,
        quick: bool,
        config: &Config,
    ) -> Result<(), String> {
        let native = hwnd(window)?;
        window.set_transparent(config.background_opacity < 1.0);
        let new = self
            .windows
            .borrow_mut()
            .insert(window.id(), (native.0 as isize, quick))
            .is_none();
        if quick {
            unsafe {
                let style = GetWindowLongPtrW(native, GWL_EXSTYLE) as u32;
                SetWindowLongPtrW(
                    native,
                    GWL_EXSTYLE,
                    ((style | WS_EX_TOOLWINDOW.0) & !WS_EX_APPWINDOW.0) as isize,
                );
                SetWindowPos(
                    native,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                )
                .map_err(err)?;
            }
        } else if new {
            self.menu.attach(native)?;
            self.notifications.set_badge(native, *self.badge.borrow());
        }
        Ok(())
    }

    pub fn quick_terminal_frame(
        &self,
        config: &Config,
        saved_size: Option<[f64; 2]>,
    ) -> Option<[f64; 4]> {
        quick::frame(config, saved_size)
    }
    pub fn quick_terminal_saved_frame(&self, window: &Window) -> Result<[f64; 4], String> {
        if let Some(state) = self.quick.borrow().get(&window.id()) {
            state.saved_frame()
        } else {
            quick::saved_frame(hwnd(window)?)
        }
    }
    pub fn show_quick(&self, window: &Window, config: &Config) -> Result<(), String> {
        let native = hwnd(window)?;
        let mut states = self.quick.borrow_mut();
        states
            .entry(window.id())
            .or_insert_with(|| quick::QuickWindow::new(native))
            .show(config)
    }
    pub fn hide_quick(
        &self,
        window: &Window,
        restore_focus: bool,
        config: &Config,
    ) -> Result<(), String> {
        let native = hwnd(window)?;
        self.quick
            .borrow_mut()
            .entry(window.id())
            .or_insert_with(|| quick::QuickWindow::new(native))
            .hide(restore_focus, config)
    }
    pub fn quick_resigned_focus(&self, window: &Window, was_focused: bool) -> Result<bool, String> {
        let lost = was_focused && unsafe { GetForegroundWindow() } != hwnd(window)?;
        if lost && let Some(state) = self.quick.borrow_mut().get_mut(&window.id()) {
            state.resigned_focus();
        }
        Ok(lost)
    }
    pub fn tick(&self) {
        for state in self.quick.borrow_mut().values_mut() {
            state.tick();
        }
    }
    pub fn next_deadline(&self) -> Option<Instant> {
        self.quick
            .borrow()
            .values()
            .filter_map(quick::QuickWindow::next_deadline)
            .min()
    }
    pub fn forget_window(&self, window: &Window) {
        self.quick.borrow_mut().remove(&window.id());
        if let Some((native, quick)) = self.windows.borrow_mut().remove(&window.id())
            && !quick
        {
            self.menu.detach(HWND(native as _));
        }
    }

    pub fn clipboard_read(&self, request: &clipboard::Read) -> clipboard::ReadResult {
        clipboard::read(request, &self.selection)
    }
    pub fn clipboard_write(&self, request: &clipboard::Write) -> clipboard::WriteResult {
        // A live owner is required before EmptyClipboard/SetClipboardData.
        let owner = self
            .windows
            .borrow()
            .values()
            .next()
            .map(|(handle, _)| HWND(*handle as _));
        clipboard::write(request, &self.selection, owner)
    }
    pub fn notify(&self, pane: u64, title: &str, body: &str) -> Result<(), String> {
        self.notifications.notify(pane, title, body)
    }
    pub fn clear_notifications(&self, pane: u64) {
        self.notifications.clear(pane);
    }
    pub fn set_badge(&self, count: usize) {
        *self.badge.borrow_mut() = count;
        for &(native, quick) in self.windows.borrow().values() {
            if !quick {
                self.notifications.set_badge(HWND(native as _), count);
            }
        }
    }
    pub fn open_url(&self, target: &str, cwd: &Path) -> Result<(), String> {
        shell_open(&link_target(target, cwd)?, w!("open"))
    }
    pub fn open_config(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(err)?;
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.to_string()),
        }
        shell_open(path.as_os_str(), w!("edit")).or_else(|_| {
            std::process::Command::new("notepad.exe")
                .arg(path)
                .spawn()
                .map(|_| ())
                .map_err(err)
        })
    }
    pub fn choose_layout_path(&self) -> Result<Option<PathBuf>, String> {
        use ::windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree,
        };
        unsafe {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(err)?;
            dialog.SetTitle(w!("Import Saved Layout")).map_err(err)?;
            dialog
                .SetFileTypes(&[Common::COMDLG_FILTERSPEC {
                    pszName: w!("Rustty workspace (*.json)"),
                    pszSpec: w!("*.json"),
                }])
                .map_err(err)?;
            dialog
                .SetOptions(FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST | FOS_FORCEFILESYSTEM)
                .map_err(err)?;
            match dialog.Show(Some(GetForegroundWindow())) {
                Err(error) if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => {
                    return Ok(None);
                }
                result => result.map_err(err)?,
            }
            let path = dialog
                .GetResult()
                .map_err(err)?
                .GetDisplayName(SIGDN_FILESYSPATH)
                .map_err(err)?;
            let result = PathBuf::from(std::ffi::OsString::from_wide(path.as_wide()));
            CoTaskMemFree(Some(path.0.cast()));
            Ok(Some(result))
        }
    }
}
impl Drop for Platform {
    fn drop(&mut self) {
        for &(native, quick) in self.windows.get_mut().values() {
            if !quick {
                self.menu.detach(HWND(native as _));
            }
        }
    }
}

fn hwnd(window: &Window) -> Result<HWND, String> {
    match window.window_handle().map_err(err)?.as_raw() {
        RawWindowHandle::Win32(handle) => Ok(HWND(handle.hwnd.get() as _)),
        _ => Err("expected a Windows HWND".into()),
    }
}
fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}
fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn link_target(target: &str, cwd: &Path) -> Result<OsString, String> {
    if target.is_empty() || target.chars().any(char::is_control) {
        return Err("URL or file path is empty or contains control characters".into());
    }
    let path = Path::new(target);
    let valid_scheme = target.split_once(':').is_some_and(|(scheme, _)| {
        !scheme.is_empty()
            && scheme.bytes().enumerate().all(|(i, b)| {
                b.is_ascii_alphabetic() || (i != 0 && (b.is_ascii_digit() || b"+-.".contains(&b)))
            })
    });
    if path.is_absolute() || valid_scheme {
        return Ok(target.into());
    }
    if let Some(relative) = target
        .strip_prefix("~/")
        .or_else(|| target.strip_prefix("~\\"))
    {
        return Ok(rustty::app_paths::home_dir()
            .map_err(err)?
            .join(relative)
            .into_os_string());
    }
    Ok(cwd.join(path).into_os_string())
}

fn shell_open(value: &OsStr, verb: PCWSTR) -> Result<(), String> {
    let value = wide(value);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as _,
        fMask: SEE_MASK_FLAG_NO_UI,
        lpVerb: verb,
        lpFile: PCWSTR(value.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(err)
}

#[cfg(test)]
mod link_tests {
    use super::*;

    #[test]
    fn links_keep_urls_and_resolve_paths_against_the_pane_directory() {
        let cwd = Path::new(r"C:\work");
        for target in [
            "https://example.org",
            "file:///C:/work/file.rs",
            r"D:\file.rs",
        ] {
            assert_eq!(link_target(target, cwd).unwrap(), OsString::from(target));
        }
        assert_eq!(
            link_target("src/file.rs", cwd).unwrap(),
            cwd.join("src/file.rs")
        );
        assert_eq!(
            link_target("~/file.rs", cwd).unwrap(),
            rustty::app_paths::home_dir().unwrap().join("file.rs")
        );
        for target in ["", "file\nname", "https://example.org/\0"] {
            assert!(link_target(target, cwd).is_err());
        }
    }
}
