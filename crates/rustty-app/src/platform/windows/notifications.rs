//! WinRT toasts use a desktop COM activator, including clicks from Notification Center.
use super::{APP_USER_MODEL_ID, EventSink, PlatformEvent, err, wide};
use ::windows::{
    Data::Xml::Dom::XmlDocument,
    UI::Notifications::{ToastNotification, ToastNotificationManager, ToastNotifier},
    Win32::{
        Foundation::*,
        System::Com::*,
        UI::{
            Notifications::*,
            Shell::{ITaskbarList3, TaskbarList},
            WindowsAndMessaging::*,
        },
    },
    core::{BOOL, GUID, HSTRING, IUnknown, Interface, PCWSTR, Ref, implement, w},
};
use std::{
    cell::RefCell,
    collections::HashSet,
    ffi::{OsStr, c_void},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const ACTIVATOR: GUID = GUID::from_u128(0x9cb270bb_cc8f_4c71_acf2_4d632a9a7e26);
type Panes = Arc<Mutex<HashSet<u64>>>;
struct Bridge {
    callback: EventSink,
    panes: Panes,
    token: u64,
}
thread_local! { static BRIDGE:RefCell<Option<Bridge>>=const {RefCell::new(None)}; }
fn activation_message() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(w!("app.rustty.toast-activated.v1")) })
}
pub(super) fn message_hook(message: &MSG) -> bool {
    if message.message != activation_message() || message.message == 0 {
        return false;
    }
    BRIDGE.with(|slot| {
        if let Some(bridge) = slot.borrow().as_ref()
            && message.lParam.0 as u64 == bridge.token
            && bridge
                .panes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&(message.wParam.0 as u64))
        {
            (bridge.callback)(PlatformEvent::NotificationClicked(message.wParam.0 as u64));
        }
    });
    true
}

#[implement(INotificationActivationCallback)]
struct Activation {
    callback: EventSink,
    panes: Panes,
    token: u64,
    completed: Option<Arc<AtomicBool>>,
}
impl INotificationActivationCallback_Impl for Activation_Impl {
    fn Activate(
        &self,
        app: &PCWSTR,
        arguments: &PCWSTR,
        _data: *const NOTIFICATION_USER_INPUT_DATA,
        _count: u32,
    ) -> ::windows::core::Result<()> {
        struct Complete(Option<Arc<AtomicBool>>);
        impl Drop for Complete {
            fn drop(&mut self) {
                if let Some(done) = &self.0 {
                    done.store(true, Ordering::Relaxed);
                }
            }
        }
        let _complete = Complete(self.completed.clone());
        if app.is_null() || arguments.is_null() {
            return Err(E_POINTER.into());
        }
        let app = unsafe { app.to_string() }?;
        let arguments = unsafe { arguments.to_string() }?;
        if app == APP_USER_MODEL_ID
            && let Some(target) = notification_target(&arguments)
        {
            if target.process == std::process::id() {
                if target.token == self.token
                    && self
                        .panes
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .contains(&target.pane)
                {
                    (self.callback)(PlatformEvent::NotificationClicked(target.pane));
                }
            } else {
                // Windows may choose any registered factory for our shared CLSID.
                // Route to the originating HWND and verify its process before posting.
                let hwnd = HWND(target.window as usize as _);
                let mut process = 0;
                unsafe {
                    GetWindowThreadProcessId(hwnd, Some(&mut process));
                }
                if process == target.process && activation_message() != 0 {
                    unsafe {
                        PostMessageW(
                            Some(hwnd),
                            activation_message(),
                            WPARAM(target.pane as usize),
                            LPARAM(target.token as isize),
                        )
                    }?;
                }
            }
        }
        Ok(())
    }
}
#[implement(IClassFactory)]
struct Factory {
    callback: EventSink,
    panes: Panes,
    token: u64,
    completed: Option<Arc<AtomicBool>>,
}
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        object: *mut *mut c_void,
    ) -> ::windows::core::Result<()> {
        if iid.is_null() || object.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe {
            *object = std::ptr::null_mut();
        }
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let activation: INotificationActivationCallback = Activation {
            callback: self.callback.clone(),
            panes: self.panes.clone(),
            token: self.token,
            completed: self.completed.clone(),
        }
        .into();
        unsafe { activation.query(iid, object).ok() }
    }
    fn LockServer(&self, _lock: BOOL) -> ::windows::core::Result<()> {
        Ok(())
    }
}

pub(super) struct Notifications {
    notifier: Option<ToastNotifier>,
    error: Option<String>,
    registration: u32,
    panes: Panes,
    group: HSTRING,
    taskbar: Option<ITaskbarList3>,
    routing_window: MessageWindow,
    token: u64,
}
// A message-only HWND survives individual workspace windows and is never visible.
struct MessageWindow(HWND);
impl Drop for MessageWindow {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.0) };
    }
}
impl Notifications {
    pub fn new(callback: EventSink) -> Result<Self, String> {
        let panes = Panes::default();
        let mut entropy = [0u8; 8];
        super::Platform::secure_random(&mut entropy).map_err(err)?;
        let token = u64::from_ne_bytes(entropy);
        if activation_message() == 0 {
            return Err("Could not register toast activation message".into());
        }
        let routing_window = MessageWindow(
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!("Rustty notification routing"),
                    WINDOW_STYLE::default(),
                    0,
                    0,
                    0,
                    0,
                    Some(HWND_MESSAGE),
                    None,
                    None,
                    None,
                )
            }
            .map_err(err)?,
        );
        let factory: IClassFactory = Factory {
            callback: callback.clone(),
            panes: panes.clone(),
            token,
            completed: None,
        }
        .into();
        let registration = unsafe {
            CoRegisterClassObject(
                &ACTIVATOR,
                &factory,
                CLSCTX_LOCAL_SERVER,
                REGCLS_MULTIPLEUSE,
            )
        }
        .map_err(err)?;
        BRIDGE.with(|slot| {
            *slot.borrow_mut() = Some(Bridge {
                callback,
                panes: panes.clone(),
                token,
            })
        });
        let (notifier, error) = match ToastNotificationManager::CreateToastNotifierWithId(
            &HSTRING::from(APP_USER_MODEL_ID),
        ) {
            Ok(notifier) => (Some(notifier), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let taskbar = unsafe {
            CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER).and_then(
                |taskbar| {
                    taskbar.HrInit()?;
                    Ok(taskbar)
                },
            )
        }
        .map_err(|error| eprintln!("initializing taskbar badges: {error}"))
        .ok();
        Ok(Self {
            notifier,
            error,
            registration,
            panes,
            group: format!("rustty-{}", std::process::id()).into(),
            taskbar,
            routing_window,
            token,
        })
    }
    pub fn notify(&self, pane: u64, title: &str, body: &str) -> Result<(), String> {
        let notifier = self.notifier.as_ref().ok_or_else(|| {
            self.error
                .clone()
                .unwrap_or_else(|| "Windows toast service is unavailable".into())
        })?;
        let xml = XmlDocument::new().map_err(err)?;
        let owner = self.routing_window.0;
        let args = format!(
            "rustty:{}:{:x}:{pane:x}:{:x}",
            std::process::id(),
            owner.0 as usize,
            self.token
        );
        xml.LoadXml(&HSTRING::from(format!("<toast launch=\"{args}\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual><audio src=\"ms-winsoundevent:Notification.Default\"/></toast>",xml_text(title),xml_text(body)))).map_err(err)?;
        let toast = ToastNotification::CreateToastNotification(&xml).map_err(err)?;
        toast
            .SetTag(&HSTRING::from(format!("{pane:x}")))
            .map_err(err)?;
        toast.SetGroup(&self.group).map_err(err)?;
        toast.SetExpiresOnReboot(true).map_err(err)?;
        self.panes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(pane);
        if let Err(error) = notifier.Show(&toast) {
            self.panes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&pane);
            return Err(format!(
                "Could not display Windows notification (register the portable app's Start Menu shortcut): {error}"
            ));
        }
        Ok(())
    }
    pub fn clear(&self, pane: u64) {
        self.panes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&pane);
        if let Ok(history) = ToastNotificationManager::History() {
            let _ = history.RemoveGroupedTagWithId(
                &HSTRING::from(format!("{pane:x}")),
                &self.group,
                &HSTRING::from(APP_USER_MODEL_ID),
            );
        }
    }
    pub fn set_badge(&self, hwnd: HWND, count: usize) {
        let Some(taskbar) = &self.taskbar else {
            return;
        };
        let icon = if count == 0 {
            HICON::default()
        } else {
            match badge_icon(count) {
                Ok(icon) => icon,
                Err(error) => {
                    eprintln!("creating taskbar badge: {error}");
                    return;
                }
            }
        };
        let description = wide(OsStr::new(&format!("{count} terminals need attention")));
        if let Err(error) =
            unsafe { taskbar.SetOverlayIcon(hwnd, icon, PCWSTR(description.as_ptr())) }
        {
            eprintln!("updating taskbar badge: {error}");
        }
        if !icon.0.is_null() {
            let _ = unsafe { DestroyIcon(icon) };
        }
    }
}
impl Drop for Notifications {
    fn drop(&mut self) {
        let panes = self
            .panes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect::<Vec<_>>();
        for pane in panes {
            self.clear(pane);
        }
        let _ = unsafe { CoRevokeClassObject(self.registration) };
        BRIDGE.with(|slot| slot.borrow_mut().take());
    }
}
#[derive(Debug, PartialEq)]
struct Target {
    process: u32,
    window: u64,
    pane: u64,
    token: u64,
}
fn notification_target(arguments: &str) -> Option<Target> {
    let rest = arguments.strip_prefix("rustty:")?;
    let mut parts = rest.split(':');
    let process = parts.next()?.parse::<u32>().ok()?;
    let mut next_hex = || {
        let value = parts.next()?;
        if value.is_empty() || value.len() > 16 {
            return None;
        }
        u64::from_str_radix(value, 16).ok()
    };
    let window = next_hex()?;
    let pane = next_hex()?;
    let token = next_hex()?;
    if process == 0 || parts.next().is_some() {
        return None;
    }
    Some(Target {
        process,
        window,
        pane,
        token,
    })
}

/// COM can cold-launch the executable for an old toast. Dispatch/reject that activation
/// without restoring sessions or showing a window, then leave no background process.
pub(super) fn run_toast_activator() -> Result<(), String> {
    use ::windows::Win32::System::WinRT::{RO_INIT_SINGLETHREADED, RoInitialize};
    let _runtime = super::Runtime(unsafe { RoInitialize(RO_INIT_SINGLETHREADED) }.is_ok());
    let completed = Arc::new(AtomicBool::new(false));
    let factory: IClassFactory = Factory {
        callback: Arc::new(|_| {}),
        panes: Panes::default(),
        token: 0,
        completed: Some(completed.clone()),
    }
    .into();
    let registration = unsafe {
        CoRegisterClassObject(
            &ACTIVATOR,
            &factory,
            CLSCTX_LOCAL_SERVER,
            REGCLS_MULTIPLEUSE,
        )
    }
    .map_err(err)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut message = MSG::default();
    while !completed.load(Ordering::Relaxed) && Instant::now() < deadline {
        unsafe {
            let _ = MsgWaitForMultipleObjectsEx(None, 100, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                if message.message == WM_QUIT {
                    completed.store(true, Ordering::Relaxed);
                    break;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    unsafe { CoRevokeClassObject(registration) }.map_err(err)
}
fn xml_text(text: &str) -> String {
    let mut result = String::new();
    for c in text.chars() {
        match c {
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '"' => result.push_str("&quot;"),
            '\'' => result.push_str("&apos;"),
            c if c == '\n'
                || c == '\r'
                || c == '\t'
                || (!c.is_control() && c != '\u{fffe}' && c != '\u{ffff}') =>
            {
                result.push(c)
            }
            _ => (),
        }
    }
    result
}
fn badge_icon(count: usize) -> ::windows::core::Result<HICON> {
    // Tiny bitmap digits avoid a font/DC dependency for taskbar overlays.
    const DIGITS: [[u8; 5]; 11] = [
        [7, 5, 5, 5, 7],
        [2, 6, 2, 2, 7],
        [7, 1, 7, 4, 7],
        [7, 1, 7, 1, 7],
        [5, 5, 7, 1, 1],
        [7, 4, 7, 1, 7],
        [7, 4, 7, 5, 7],
        [7, 1, 1, 1, 1],
        [7, 5, 7, 5, 7],
        [7, 5, 7, 1, 7],
        [0, 2, 7, 2, 0],
    ];
    let text = if count > 99 {
        "99+".into()
    } else {
        count.to_string()
    };
    let mut pixels = vec![0u8; 32 * 32 * 4];
    let mut mask = [255u8; 32 * 4];
    for y in 0..32usize {
        for x in 0..32usize {
            let dx = x as i32 - 16;
            let dy = y as i32 - 16;
            if dx * dx + dy * dy <= 225 {
                let p = (y * 32 + x) * 4;
                pixels[p..p + 4].copy_from_slice(&[44, 44, 210, 255]);
                mask[y * 4 + x / 8] &= !(0x80 >> (x % 8));
            }
        }
    }
    let left = (32 - (text.len() * 8 - 2)) / 2;
    for (index, c) in text.bytes().enumerate() {
        let glyph = if c == b'+' { 10 } else { (c - b'0') as usize };
        for (y, row) in DIGITS[glyph].iter().enumerate() {
            for x in 0..3 {
                if row & (1 << (2 - x)) != 0 {
                    for yy in 0..2 {
                        for xx in 0..2 {
                            let p = ((11 + y * 2 + yy) * 32 + left + index * 8 + x * 2 + xx) * 4;
                            pixels[p..p + 4].fill(255);
                        }
                    }
                }
            }
        }
    }
    unsafe { CreateIcon(None, 32, 32, 1, 32, mask.as_ptr(), pixels.as_ptr()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activation_requires_our_full_process_identifier() {
        assert_eq!(
            notification_target(&format!("rustty:{}:abc:a:123", std::process::id()))
                .map(|target| target.pane),
            Some(10)
        );
        assert_eq!(notification_target("other:1:a"), None);
        assert_eq!(notification_target("rustty:0:abc:a:123"), None);
        assert_eq!(notification_target("rustty:1:abc:a:123:extra"), None);
    }
    #[test]
    fn terminal_text_cannot_inject_toast_xml() {
        assert_eq!(xml_text("<&>\x07"), "&lt;&amp;&gt;");
    }
    #[test]
    fn native_com_callback_routes_live_panes_and_ignores_cleared_ones() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let clicked = Arc::new(AtomicU64::new(0));
        let result = clicked.clone();
        let panes = Panes::default();
        panes.lock().unwrap().insert(42);
        let callback: INotificationActivationCallback = Activation {
            panes: panes.clone(),
            token: 123,
            completed: None,
            callback: Arc::new(move |event| {
                if let PlatformEvent::NotificationClicked(pane) = event {
                    result.store(pane, Ordering::Relaxed);
                }
            }),
        }
        .into();
        let app = wide(OsStr::new(APP_USER_MODEL_ID));
        let arguments = wide(OsStr::new(&format!(
            "rustty:{}:0:2a:7b",
            std::process::id()
        )));
        unsafe { callback.Activate(PCWSTR(app.as_ptr()), PCWSTR(arguments.as_ptr()), &[]) }
            .unwrap();
        assert_eq!(clicked.swap(0, Ordering::Relaxed), 42);
        panes.lock().unwrap().clear();
        unsafe { callback.Activate(PCWSTR(app.as_ptr()), PCWSTR(arguments.as_ptr()), &[]) }
            .unwrap();
        assert_eq!(clicked.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn forwarded_activation_requires_instance_token_and_live_pane() {
        use std::sync::atomic::AtomicU64;
        let clicked = Arc::new(AtomicU64::new(0));
        let result = clicked.clone();
        let panes = Panes::default();
        panes.lock().unwrap().insert(42);
        BRIDGE.with(|slot| {
            *slot.borrow_mut() = Some(Bridge {
                panes: panes.clone(),
                token: 123,
                callback: Arc::new(move |event| {
                    if let PlatformEvent::NotificationClicked(pane) = event {
                        result.store(pane, Ordering::Relaxed);
                    }
                }),
            })
        });
        let mut message = MSG {
            message: activation_message(),
            wParam: WPARAM(42),
            lParam: LPARAM(124),
            ..Default::default()
        };
        assert!(message_hook(&message));
        assert_eq!(clicked.load(Ordering::Relaxed), 0);
        message.lParam = LPARAM(123);
        assert!(message_hook(&message));
        assert_eq!(clicked.swap(0, Ordering::Relaxed), 42);
        panes.lock().unwrap().clear();
        assert!(message_hook(&message));
        assert_eq!(clicked.load(Ordering::Relaxed), 0);
        BRIDGE.with(|slot| slot.borrow_mut().take());
    }
}
