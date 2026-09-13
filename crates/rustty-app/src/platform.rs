//! Native macOS integration. Window creation, presentation and focus stay in the app.
#![cfg(target_os = "macos")]

use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::c_void,
    path::Path,
    ptr::NonNull,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use block2::{DynBlock, RcBlock};
use muda::{
    AboutMetadata, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::{Key, KeyAccelerator, Modifiers as MenuModifiers},
};
use objc2::{
    DeclaredClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{Bool, ProtocolObject},
    sel,
};
use objc2_app_kit::{
    NSAccessibility, NSApplication, NSColor, NSEvent, NSFloatingWindowLevel,
    NSUserInterfaceItemIdentification, NSView, NSWindow, NSWindowAnimationBehavior,
    NSWindowCollectionBehavior, NSWindowTitleVisibility, NSWorkspace,
};
use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use objc2_foundation::{
    NSArray, NSBundle, NSDictionary, NSError, NSNumber, NSObject, NSObjectProtocol, NSString,
    NSTimer, NSURL,
};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};
use rustty::config::{Action, Config, Direction, KeyBinding, KeyTrigger, Modifiers, OptionAsAlt};
use winit::{
    platform::macos::{OptionAsAlt as WinitOptionAsAlt, WindowExtMacOS},
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::Window,
};

#[derive(Clone, Debug)]
pub enum PlatformEvent {
    Action(Action),
    NotificationClicked(u64),
}

type EventSink = Arc<dyn Fn(PlatformEvent) + Send + Sync>;

/// Own once, on the event-loop thread, after winit initializes NSApplication.
pub struct Platform {
    mtm: MainThreadMarker,
    menu: Menu,
    menu_actions: Vec<(MenuItem, Action)>,
    callback: EventSink,
    global_keys: Option<GlobalKeys>,
    notifications: Option<Retained<UNUserNotificationCenter>>,
    notification_state: Arc<NotificationState>,
    _notification_delegate: Retained<NotificationDelegate>,
}

impl Platform {
    pub fn new(callback: EventSink, config: &Config) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("platform must initialize on the main thread")?;
        let (menu, menu_actions) = make_menu()?;
        let actions: Vec<_> = menu_actions
            .iter()
            .map(|(_, action)| action.clone())
            .collect();
        let sink = callback.clone();
        // Muda installs one process-wide handler. The app owns one Platform for its lifetime.
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some(index) = event
                .id
                .0
                .strip_prefix("rustty-action-")
                .and_then(|id| id.parse::<usize>().ok())
                && let Some(action) = actions.get(index)
            {
                sink(PlatformEvent::Action(action.clone()));
            }
        }));
        menu.init_for_nsapp();

        let delegate = NotificationDelegate::new(callback.clone());
        // UNUserNotificationCenter raises an Objective-C exception for an unbundled
        // command-line executable. The app still works when launched by cargo run.
        let notifications = NSBundle::mainBundle().bundleIdentifier().map(|_| {
            let center = UNUserNotificationCenter::currentNotificationCenter();
            center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            center
        });
        let mut platform = Self {
            mtm,
            menu,
            menu_actions,
            callback,
            global_keys: None,
            notifications,
            notification_state: Arc::default(),
            _notification_delegate: delegate,
        };
        platform.update_config(config)?;
        Ok(platform)
    }

    pub fn update_config(&mut self, config: &Config) -> Result<(), String> {
        for (item, action) in &self.menu_actions {
            // Conditional bindings must reach the app so it can decide whether to
            // perform the action or pass the key to the terminal/editor.
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
                .and_then(|binding| menu_accelerator(&binding.trigger[0]));
            item.set_key_accelerator(accelerator)
                .map_err(|e| e.to_string())?;
        }
        let bindings: Vec<_> = config
            .keybinds
            .iter()
            .filter(|binding| binding.flags.global && binding.table.is_none())
            .cloned()
            .collect();
        if self
            .global_keys
            .as_ref()
            .is_some_and(|keys| keys.context.bindings == bindings)
        {
            return Ok(());
        }
        // Release the old tap before installing a replacement; otherwise a key
        // could briefly be processed twice during config reload.
        self.global_keys = None;
        if !bindings.is_empty() {
            self.global_keys = Some(GlobalKeys::new(self.mtm, bindings, self.callback.clone()));
        }
        Ok(())
    }

    pub fn configure_window(
        &self,
        window: &Window,
        quick: bool,
        config: &Config,
    ) -> Result<(), String> {
        window.set_option_as_alt(match config.macos_option_as_alt {
            OptionAsAlt::False => WinitOptionAsAlt::None,
            OptionAsAlt::True => WinitOptionAsAlt::Both,
            OptionAsAlt::Left => WinitOptionAsAlt::OnlyLeft,
            OptionAsAlt::Right => WinitOptionAsAlt::OnlyRight,
        });
        let native = native_window(window)?;
        native.setTitlebarAppearsTransparent(true);
        native.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        native.setMovableByWindowBackground(false);
        native.setOpaque(config.background_opacity >= 1.0);
        let c = config.background;
        native.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            c.r as f64 / 255.0,
            c.g as f64 / 255.0,
            c.b as f64 / 255.0,
            config.background_opacity as f64,
        )));
        if quick {
            native.setIdentifier(Some(&NSString::from_str("app.rustty.quickTerminal")));
            native.setAccessibilitySubrole(Some(&NSString::from_str("AXFloatingWindow")));
            native.setLevel(NSFloatingWindowLevel);
            native.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::IgnoresCycle
                    | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
            native.setAnimationBehavior(NSWindowAnimationBehavior::None);
            // The host animates and hides on focus loss; native auto-hide would
            // bypass its visibility state. Winit owns an NSWindow, not an NSPanel.
            native.setHidesOnDeactivate(false);
        }
        Ok(())
    }

    /// Replace this pane's outstanding notification. The app decides when a pane
    /// warrants attention; clicking the native notification returns its stable id.
    pub fn notify(&self, pane: u64, title: &str, body: &str) -> Result<(), String> {
        let center = self
            .notifications
            .as_ref()
            .ok_or("notifications require the Rustty app bundle")?;
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        content.setSound(Some(&UNNotificationSound::defaultSound()));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&notification_id(pane)),
            &content,
            None,
        );
        let state = self.notification_state.clone();
        let revision = state.next.fetch_add(1, Ordering::Relaxed);
        state
            .panes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(pane, revision);
        let center_for_request = center.clone();
        let completion = RcBlock::new(move |granted: Bool, error: *mut NSError| {
            if !error.is_null() {
                // SAFETY: Apple's completion handler supplies a valid NSError for
                // the duration of this call, or null when there is no error.
                eprintln!("notification permission: {}", unsafe { &*error });
            }
            if granted.as_bool() {
                let panes = state.panes.lock().unwrap_or_else(|e| e.into_inner());
                if panes.get(&pane) != Some(&revision) {
                    return;
                }
                let report = RcBlock::new(|error: *mut NSError| {
                    if !error.is_null() {
                        eprintln!("posting notification: {}", unsafe { &*error });
                    }
                });
                center_for_request
                    .addNotificationRequest_withCompletionHandler(&request, Some(&report));
            }
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert
                | UNAuthorizationOptions::Sound
                | UNAuthorizationOptions::Badge,
            &completion,
        );
        Ok(())
    }

    pub fn clear_notifications(&self, pane: u64) {
        let mut panes = self
            .notification_state
            .panes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        panes.remove(&pane);
        if let Some(center) = &self.notifications {
            let id = NSString::from_str(&notification_id(pane));
            let ids = NSArray::from_slice(&[&*id]);
            center.removePendingNotificationRequestsWithIdentifiers(&ids);
            center.removeDeliveredNotificationsWithIdentifiers(&ids);
        }
    }

    pub fn set_badge(&self, count: usize) {
        let label = (count != 0).then(|| NSString::from_str(&count.to_string()));
        NSApplication::sharedApplication(self.mtm)
            .dockTile()
            .setBadgeLabel(label.as_deref());
    }

    pub fn open_url(&self, url: &str) -> Result<(), String> {
        if url.chars().any(char::is_control) {
            return Err("URL contains control characters".into());
        }
        let url = NSURL::URLWithString(&NSString::from_str(url)).ok_or("invalid URL")?;
        if url.scheme().is_none() {
            return Err("URL requires a scheme".into());
        }
        open_native_url(&url)
    }

    pub fn open_config(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.to_string()),
        }
        let path = path.to_str().ok_or("config path is not valid UTF-8")?;
        open_native_url(&NSURL::fileURLWithPath(&NSString::from_str(path)))
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        self.notification_state
            .panes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.menu.remove_for_nsapp();
        if let Some(center) = &self.notifications {
            center.setDelegate(None);
            center.removeAllPendingNotificationRequests();
            center.removeAllDeliveredNotifications();
        }
    }
}

#[derive(Default)]
struct NotificationState {
    next: AtomicU64,
    panes: Mutex<HashMap<u64, u64>>,
}

fn native_window(window: &Window) -> Result<Retained<NSWindow>, String> {
    let handle = window.window_handle().map_err(|e| e.to_string())?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("expected an AppKit window".into());
    };
    // SAFETY: winit lends its live NSView through the window handle. Platform is
    // main-thread-only, and the view remains alive while its Window is borrowed.
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    view.window()
        .ok_or_else(|| "native window has no attached view".into())
}

fn open_native_url(url: &NSURL) -> Result<(), String> {
    if NSWorkspace::sharedWorkspace().openURL(url) {
        Ok(())
    } else {
        Err("macOS could not open the URL or file".into())
    }
}

fn notification_id(pane: u64) -> String {
    format!("rustty-pane-{pane}")
}

fn notification_pane(id: &str) -> Option<u64> {
    id.strip_prefix("rustty-pane-")?.parse().ok()
}

define_class!(
    #[unsafe(super = NSObject)]
    #[ivars = EventSink]
    struct NotificationDelegate;

    unsafe impl NSObjectProtocol for NotificationDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn received(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &DynBlock<dyn Fn()>,
        ) {
            // Dismissals must not steal focus from the user's current application.
            if response
                .actionIdentifier()
                .isEqualToString(unsafe { UNNotificationDefaultActionIdentifier })
                && let Some(pane) =
                    notification_pane(&response.notification().request().identifier().to_string())
            {
                (self.ivars())(PlatformEvent::NotificationClicked(pane));
            }
            completion.call(());
        }
    }
);

impl NotificationDelegate {
    fn new(callback: EventSink) -> Retained<Self> {
        use objc2::AnyThread;
        // SAFETY: NSObject's initializer does not impose additional invariants.
        unsafe { msg_send![super(Self::alloc().set_ivars(callback)), init] }
    }
}

fn make_menu() -> Result<(Menu, Vec<(MenuItem, Action)>), String> {
    let menu = Menu::new();
    let mut actions = Vec::new();
    let app = Submenu::new("Rustty", true);
    app.append_items(&[
        &PredefinedMenuItem::about(
            Some("About Rustty"),
            Some(AboutMetadata {
                name: Some("Rustty".into()),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                copyright: Some("Rustty contributors; based on Ghostty (MIT)".into()),
                ..Default::default()
            }),
        ),
        &PredefinedMenuItem::separator(),
    ])
    .map_err(|e| e.to_string())?;
    append_action(&app, &mut actions, "Settings…", Action::OpenConfig)?;
    append_action(
        &app,
        &mut actions,
        "Reload Configuration",
        Action::ReloadConfig,
    )?;
    app.append_items(&[
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::services(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::hide(None),
        &PredefinedMenuItem::hide_others(None),
        &PredefinedMenuItem::show_all(None),
        &PredefinedMenuItem::separator(),
    ])
    .map_err(|e| e.to_string())?;
    append_action(&app, &mut actions, "Quit Rustty", Action::Quit)?;

    let file = Submenu::new("File", true);
    for (name, action) in [
        ("New Window", Action::NewWindow),
        ("New Tab", Action::NewTab),
        ("Split Right", Action::NewSplit(Direction::Right)),
        ("Split Down", Action::NewSplit(Direction::Down)),
        ("Close Surface", Action::CloseSurface),
        ("Close Tab", Action::CloseTab),
        ("Close Window", Action::CloseWindow),
    ] {
        append_action(&file, &mut actions, name, action)?;
    }
    let edit = Submenu::new("Edit", true);
    for (name, action) in [
        ("Undo", Action::Undo),
        ("Redo", Action::Redo),
        ("Copy", Action::CopyToClipboard),
        ("Paste", Action::PasteFromClipboard),
        ("Select All", Action::SelectAll),
        ("Find…", Action::StartSearch),
    ] {
        append_action(&edit, &mut actions, name, action)?;
    }
    let view = Submenu::new("View", true);
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
        append_action(&view, &mut actions, name, action)?;
    }
    let window = Submenu::new("Window", true);
    window
        .append_items(&[
            &PredefinedMenuItem::minimize(None),
            &PredefinedMenuItem::maximize(Some("Zoom")),
            &PredefinedMenuItem::separator(),
        ])
        .map_err(|e| e.to_string())?;
    for (name, action) in [
        ("Previous Tab", Action::PreviousTab),
        ("Next Tab", Action::NextTab),
        ("Previous Split", Action::GotoSplit(Direction::Previous)),
        ("Next Split", Action::GotoSplit(Direction::Next)),
        ("Toggle Quick Terminal", Action::ToggleQuickTerminal),
    ] {
        append_action(&window, &mut actions, name, action)?;
    }
    window
        .append(&PredefinedMenuItem::bring_all_to_front(None))
        .map_err(|e| e.to_string())?;
    window.set_as_windows_menu_for_nsapp();
    menu.append_items(&[&app, &file, &edit, &view, &window])
        .map_err(|e| e.to_string())?;
    Ok((menu, actions))
}

fn append_action(
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
    menu.append(&item).map_err(|e| e.to_string())?;
    actions.push((item, action));
    Ok(())
}

fn menu_accelerator(trigger: &KeyTrigger) -> Option<KeyAccelerator> {
    if trigger.physical {
        return None;
    }
    let mut mods = MenuModifiers::empty();
    if trigger.modifiers.shift {
        mods |= MenuModifiers::SHIFT;
    }
    if trigger.modifiers.control {
        mods |= MenuModifiers::CONTROL;
    }
    if trigger.modifiers.alt {
        mods |= MenuModifiers::ALT;
    }
    if trigger.modifiers.super_key {
        mods |= MenuModifiers::SUPER;
    }
    // Plain keys such as escape must reach the focused terminal/editor.
    if mods.is_empty() {
        return None;
    }
    let key = match trigger.key.as_str() {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "space" => Key::Character(" ".into()),
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
        "backquote" => Key::Character("`".into()),
        key if key.chars().count() == 1 => Key::Character(key.into()),
        _ => return None,
    };
    Some(KeyAccelerator::new(Some(mods), key))
}

struct TapRegistration {
    port: CFRetained<CFMachPort>,
    source: CFRetained<CFRunLoopSource>,
}

impl Drop for TapRegistration {
    fn drop(&mut self) {
        self.source.invalidate();
        self.port.invalidate();
    }
}

struct TapContext {
    mtm: MainThreadMarker,
    callback: EventSink,
    bindings: Vec<KeyBinding>,
    registration: RefCell<Option<TapRegistration>>,
}

struct GlobalKeys {
    context: Rc<TapContext>,
    timer: Option<Retained<NSTimer>>,
}

// These two Accessibility calls avoid repeatedly creating a failed event tap,
// which leaks a Mach port on macOS before Accessibility access is granted.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: *const NSString;
}

impl GlobalKeys {
    fn new(mtm: MainThreadMarker, bindings: Vec<KeyBinding>, callback: EventSink) -> Self {
        let context = Rc::new(TapContext {
            mtm,
            callback,
            bindings,
            registration: RefCell::new(None),
        });
        let timer = if unsafe { AXIsProcessTrusted() } {
            context.enable();
            None
        } else {
            unsafe {
                let options = NSDictionary::from_slices(
                    &[&*kAXTrustedCheckOptionPrompt],
                    &[&*NSNumber::new_bool(true)],
                );
                AXIsProcessTrustedWithOptions(
                    (&*options as *const NSDictionary<NSString, NSNumber>).cast(),
                );
            }
            let poller = PermissionPoller::new(mtm, context.clone());
            // SAFETY: selector signature below accepts one NSTimer. Scheduling is
            // on the main run loop; the timer retains its main-thread-only target.
            Some(unsafe {
                NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                    1.0,
                    &poller,
                    sel!(poll:),
                    None,
                    true,
                )
            })
        };
        Self { context, timer }
    }
}

impl Drop for GlobalKeys {
    fn drop(&mut self) {
        if let Some(timer) = &self.timer {
            timer.invalidate();
        }
        self.context.registration.borrow_mut().take();
    }
}

impl TapContext {
    fn enable(&self) {
        // SAFETY: installed on the main run loop, with a stable Rc allocation
        // held until GlobalKeys invalidates both the source and the Mach port.
        let port = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                1u64 << CGEventType::KeyDown.0,
                Some(global_key_event),
                (self as *const Self).cast_mut().cast(),
            )
        };
        let Some(port) = port else {
            eprintln!("creating global key event tap failed despite Accessibility permission");
            return;
        };
        let Some(source) = CFMachPort::new_run_loop_source(None, Some(&port), 0) else {
            port.invalidate();
            eprintln!("creating global key run-loop source failed");
            return;
        };
        let Some(run_loop) = CFRunLoop::main() else {
            return;
        };
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
        *self.registration.borrow_mut() = Some(TapRegistration { port, source });
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Rc<TapContext>]
    struct PermissionPoller;
    unsafe impl NSObjectProtocol for PermissionPoller {}
    impl PermissionPoller {
        #[unsafe(method(poll:))]
        fn poll(&self, timer: &NSTimer) {
            if unsafe { AXIsProcessTrusted() } {
                timer.invalidate();
                self.ivars().enable();
            }
        }
    }
);

impl PermissionPoller {
    fn new(mtm: MainThreadMarker, context: Rc<TapContext>) -> Retained<Self> {
        unsafe { msg_send![super(Self::alloc(mtm).set_ivars(context)), init] }
    }
}

unsafe extern "C-unwind" fn global_key_event(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: TapContext::enable installs this pointer on the main run loop and
    // GlobalKeys invalidates its tap before releasing the context.
    let context = unsafe { &*user_info.cast::<TapContext>() };
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        if let Some(registration) = &*context.registration.borrow() {
            CGEvent::tap_enable(&registration.port, true);
        }
        return event.as_ptr();
    }
    if kind != CGEventType::KeyDown || NSApplication::sharedApplication(context.mtm).isActive() {
        return event.as_ptr();
    }
    let event_ref = unsafe { event.as_ref() };
    let Some(native) = NSEvent::eventWithCGEvent(event_ref) else {
        return event.as_ptr();
    };
    let flags = CGEvent::flags(Some(event_ref));
    let modifiers = Modifiers {
        shift: flags.contains(CGEventFlags::MaskShift),
        control: flags.contains(CGEventFlags::MaskControl),
        alt: flags.contains(CGEventFlags::MaskAlternate),
        super_key: flags.contains(CGEventFlags::MaskCommand),
    };
    let text = native
        .charactersIgnoringModifiers()
        .map(|s| s.to_string())
        .unwrap_or_default();
    if let Some(binding) = context.bindings.iter().rev().find(|binding| {
        binding
            .trigger
            .first()
            .is_some_and(|trigger| key_matches(trigger, native.keyCode(), &text, modifiers))
    }) {
        for action in &binding.actions {
            (context.callback)(PlatformEvent::Action(action.clone()));
        }
        if binding.flags.consumed {
            return std::ptr::null_mut();
        }
    }
    event.as_ptr()
}

fn key_matches(trigger: &KeyTrigger, code: u16, text: &str, modifiers: Modifiers) -> bool {
    if trigger.modifiers != modifiers {
        return false;
    }
    let key = trigger.key.as_str();
    if key == "catch_all" {
        return true;
    }
    let physical = physical_key(code);
    let named_physical = key.starts_with("key_") || key.starts_with("digit_");
    let key = key
        .strip_prefix("key_")
        .or_else(|| key.strip_prefix("digit_"))
        .unwrap_or(key);
    let key = match key {
        "backquote" => "`",
        " " => "space",
        other => other,
    };
    if trigger.physical || named_physical {
        return physical == Some(key);
    }
    match key {
        "space" => text == " ",
        "backquote" => text == "`",
        key if key.chars().count() == 1 => key.to_lowercase() == text.to_lowercase(),
        _ => physical == Some(key),
    }
}

fn physical_key(code: u16) -> Option<&'static str> {
    Some(match code {
        0 => "a",
        1 => "s",
        2 => "d",
        3 => "f",
        4 => "h",
        5 => "g",
        6 => "z",
        7 => "x",
        8 => "c",
        9 => "v",
        11 => "b",
        12 => "q",
        13 => "w",
        14 => "e",
        15 => "r",
        16 => "y",
        17 => "t",
        18 => "1",
        19 => "2",
        20 => "3",
        21 => "4",
        22 => "6",
        23 => "5",
        24 => "=",
        25 => "9",
        26 => "7",
        27 => "-",
        28 => "8",
        29 => "0",
        30 => "]",
        31 => "o",
        32 => "u",
        33 => "[",
        34 => "i",
        35 => "p",
        36 => "enter",
        37 => "l",
        38 => "j",
        39 => "'",
        40 => "k",
        41 => ";",
        42 => "\\",
        43 => ",",
        44 => "/",
        45 => "n",
        46 => "m",
        47 => ".",
        48 => "tab",
        49 => "space",
        50 => "`",
        51 => "backspace",
        53 => "escape",
        65 => "kp_decimal",
        67 => "kp_multiply",
        69 => "kp_add",
        71 => "num_lock",
        75 => "kp_divide",
        76 => "kp_enter",
        78 => "kp_subtract",
        81 => "kp_equal",
        82 => "kp_0",
        83 => "kp_1",
        84 => "kp_2",
        85 => "kp_3",
        86 => "kp_4",
        87 => "kp_5",
        88 => "kp_6",
        89 => "kp_7",
        91 => "kp_8",
        92 => "kp_9",
        64 => "f17",
        79 => "f18",
        80 => "f19",
        90 => "f20",
        96 => "f5",
        97 => "f6",
        98 => "f7",
        99 => "f3",
        100 => "f8",
        101 => "f9",
        103 => "f11",
        105 => "f13",
        106 => "f16",
        107 => "f14",
        109 => "f10",
        111 => "f12",
        113 => "f15",
        114 => "insert",
        115 => "home",
        116 => "page_up",
        117 => "delete",
        118 => "f4",
        119 => "end",
        120 => "f2",
        121 => "page_down",
        122 => "f1",
        123 => "arrow_left",
        124 => "arrow_right",
        125 => "arrow_down",
        126 => "arrow_up",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_binding_distinguishes_layout_physical_keys_and_modifiers() {
        let logical = KeyTrigger::parse("ctrl+a").unwrap();
        assert!(key_matches(&logical, 12, "a", logical.modifiers));
        let physical = KeyTrigger::parse("physical:ctrl+a").unwrap();
        assert!(!key_matches(&physical, 12, "a", physical.modifiers));
        assert!(key_matches(&physical, 0, "q", physical.modifiers));
        assert!(!key_matches(&logical, 0, "a", Modifiers::default()));
        let arrow = KeyTrigger::parse("super+arrow_left").unwrap();
        assert!(key_matches(&arrow, 123, "\u{f702}", arrow.modifiers));
        let plus = KeyTrigger::parse("super+shift++").unwrap();
        assert!(key_matches(&plus, 24, "+", plus.modifiers));
        assert!(menu_accelerator(&plus).is_some());
        assert!(menu_accelerator(&physical).is_none());
    }

    #[test]
    fn notification_routing_requires_our_full_identifier() {
        assert_eq!(
            notification_pane(&notification_id(u64::MAX)),
            Some(u64::MAX)
        );
        assert_eq!(notification_pane("other-pane-42"), None);
        assert_eq!(notification_pane("rustty-pane-42-extra"), None);
    }
}
