//! Native macOS integration. Winit owns windows; native helpers handle OS policy.
#![cfg(target_os = "macos")]

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    ffi::c_void,
    io::{self, Read},
    path::{Path, PathBuf},
    ptr::NonNull,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

use block2::{DynBlock, RcBlock};
use muda::{
    AboutMetadata, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::{Key, KeyAccelerator, Modifiers as MenuModifiers},
};
use objc2::{
    DeclaredClass, MainThreadMarker, MainThreadOnly, define_class, extern_class, extern_methods,
    msg_send,
    rc::Retained,
    runtime::{Bool, ProtocolObject},
    sel,
};
use objc2_app_kit::{
    NSAccessibility, NSAnimatablePropertyContainer, NSAnimationContext, NSApplication,
    NSApplicationActivationOptions, NSColor, NSColorSpace, NSEvent, NSEventModifierFlags,
    NSEventType, NSFloatingWindowLevel, NSMenu, NSModalResponseCancel, NSModalResponseOK,
    NSOpenPanel, NSPasteboard, NSPasteboardAccessBehavior, NSPasteboardItem,
    NSPasteboardTypeString, NSPopUpMenuWindowLevel, NSRunningApplication, NSScreen,
    NSUserInterfaceItemIdentification, NSView, NSWindow, NSWindowAnimationBehavior,
    NSWindowCollectionBehavior, NSWindowTabbingMode, NSWindowTitleVisibility, NSWorkspace,
};
use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID};
use objc2_foundation::{
    NSArray, NSBundle, NSCopying, NSData, NSDistributedNotificationCenter, NSError, NSNotification,
    NSNotificationSuspensionBehavior, NSObject, NSObjectProtocol, NSPoint, NSPointInRect, NSRect,
    NSSize, NSString, NSURL,
};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};
use rustty::config::{
    Action, Config, Direction, KeyBinding, KeyTrigger, Modifiers, OptionAsAlt,
    QuickTerminalPosition, QuickTerminalScreen, QuickTerminalSpaceBehavior,
};
use rustty::vt::clipboard::{self, Content, Location, ReadResult, ReadSuccess, WriteResult};
use winit::{
    platform::macos::{OptionAsAlt as WinitOptionAsAlt, WindowExtMacOS},
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::{Theme, Window, WindowId},
};

#[derive(Clone, Debug)]
pub enum PlatformEvent {
    Action(Action),
    NotificationClicked(u64),
    GlobalHotkey { id: u32, key_code: u16 },
    Warning(String),
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
    previous_quick_app: RefCell<Option<Retained<NSRunningApplication>>>,
    quick_animations: RefCell<HashMap<WindowId, Rc<RefCell<QuickAnimation>>>>,
}

impl Platform {
    pub fn double_click_interval() -> Duration {
        Duration::from_secs_f64(NSEvent::doubleClickInterval())
    }

    /// The kernel supplies cryptographic entropy; failures must not mint a paste grant.
    pub fn secure_random(bytes: &mut [u8]) -> io::Result<()> {
        std::fs::File::open("/dev/urandom")?.read_exact(bytes)
    }

    /// Live pointer location in logical content-view coordinates. AppKit file
    /// drags do not emit Winit CursorMoved events, so cached positions are stale.
    pub fn cursor_position(window: &Window) -> Result<[f32; 2], String> {
        let native = native_window(window)?;
        let view = native
            .contentView()
            .ok_or("native window has no content view")?;
        let point = view.convertPoint_fromView(native.mouseLocationOutsideOfEventStream(), None);
        Ok([point.x as f32, point.y as f32])
    }

    /// Native state for an explicitly requested smoke-test failure report.
    pub fn window_diagnostics(window: &Window) -> Result<String, String> {
        let native = native_window(window)?;
        let app = NSApplication::sharedApplication(native.mtm());
        Ok(format!(
            "visible={}, key={}, minimized={}, active_space={}, occlusion={:?}, frame={:?}, app_active={}, app_hidden={}, activation_policy={:?}",
            native.isVisible(),
            native.isKeyWindow(),
            native.isMiniaturized(),
            native.isOnActiveSpace(),
            native.occlusionState(),
            native.frame(),
            app.isActive(),
            app.isHidden(),
            app.activationPolicy(),
        ))
    }

    /// Read application appearance after Winit has initialized NSApplication.
    pub fn system_theme() -> Option<Theme> {
        let app = NSApplication::sharedApplication(MainThreadMarker::new()?);
        let light = NSString::from_str("NSAppearanceNameAqua");
        let dark = NSString::from_str("NSAppearanceNameDarkAqua");
        let appearance = app
            .effectiveAppearance()
            .bestMatchFromAppearancesWithNames(&NSArray::from_slice(&[&*light, &*dark]))?;
        Some(if *appearance == *dark {
            Theme::Dark
        } else {
            Theme::Light
        })
    }

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
            previous_quick_app: RefCell::new(None),
            quick_animations: RefCell::new(HashMap::new()),
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
        if let Some(menu) = NSApplication::sharedApplication(self.mtm).mainMenu() {
            update_native_menu_accelerators(&menu, &config.keybinds);
        }
        let bindings: Vec<_> = config
            .keybinds
            .iter()
            .filter(|binding| binding.flags.global && binding.table.is_none())
            .cloned()
            .collect();
        // Release registrations before replacing them, including on an unchanged
        // reload: another application may have released a conflicting shortcut.
        self.global_keys = None;
        if !bindings.is_empty() {
            match GlobalKeys::new(self.mtm, bindings, self.callback.clone()) {
                Ok(keys) => self.global_keys = Some(keys),
                Err(error) => (self.callback)(PlatformEvent::Warning(error)),
            }
        }
        Ok(())
    }

    /// Resolve at dispatch time so events queued before a reload cannot execute
    /// removed bindings or acquire the actions of a replacement registration.
    pub fn global_keybinding(&self, id: u32) -> Option<KeyBinding> {
        let context = &self.global_keys.as_ref()?.context;
        let index = context.registrations.borrow().get(&id)?.binding;
        context.bindings.get(index).cloned()
    }

    /// Resolve the user's current accent into portable sRGB for tab-owned UI.
    pub fn accent_color(&self) -> Option<[u8; 3]> {
        let color =
            NSColor::controlAccentColor().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
        Some(
            [
                color.redComponent(),
                color.greenComponent(),
                color.blueComponent(),
            ]
            .map(|component| (component.clamp(0.0, 1.0) * 255.0).round() as u8),
        )
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
        // Rustty manages tabs itself; AppKit must keep workspace windows separate.
        native.setTabbingMode(NSWindowTabbingMode::Disallowed);
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
            let animating = self
                .quick_animations
                .borrow()
                .get(&window.id())
                .is_some_and(|state| state.borrow().running);
            native.setLevel(if animating {
                NSPopUpMenuWindowLevel
            } else {
                NSFloatingWindowLevel
            });
            native.setCollectionBehavior(quick_collection_behavior(
                config.quick_terminal_space_behavior,
            ));
            native.setExcludedFromWindowsMenu(true);
            native.setRestorable(false);
            native.setAnimationBehavior(NSWindowAnimationBehavior::None);
            // Native auto-hide would bypass the host's visibility state.
            native.setHidesOnDeactivate(false);
        }
        Ok(())
    }

    /// Anchor a new or restored quick terminal in Winit's global logical coordinates.
    pub fn quick_terminal_frame(
        &self,
        config: &Config,
        saved_size: Option<[f64; 2]>,
    ) -> Option<[f64; 4]> {
        Some(quick_frame(
            self.quick_visible_frame(config.quick_terminal_screen)?,
            config.quick_terminal_position,
            saved_size,
        ))
    }

    fn quick_visible_frame(&self, selection: QuickTerminalScreen) -> Option<[f64; 4]> {
        Some(logical_screen_frame(
            self.quick_screen(selection)?.visibleFrame(),
            CGDisplayBounds(CGMainDisplayID()).size.height,
        ))
    }

    fn quick_screen(&self, selection: QuickTerminalScreen) -> Option<Retained<NSScreen>> {
        let screens = NSScreen::screens(self.mtm);
        match selection {
            QuickTerminalScreen::Main => NSScreen::mainScreen(self.mtm),
            QuickTerminalScreen::Mouse => {
                let mouse = NSEvent::mouseLocation();
                screens
                    .iter()
                    .find(|screen| NSPointInRect(mouse, screen.frame()))
            }
            QuickTerminalScreen::MacosMenuBar => screens.firstObject(),
        }
        .or_else(|| NSScreen::mainScreen(self.mtm))
        .or_else(|| screens.firstObject())
    }

    fn quick_animation(&self, window: &Window, native: &NSWindow) -> Rc<RefCell<QuickAnimation>> {
        self.quick_animations
            .borrow_mut()
            .entry(window.id())
            .or_insert_with(|| Rc::new(RefCell::new(QuickAnimation::new(native.frame()))))
            .clone()
    }

    /// Read the final frame while animating and the live frame after settling.
    /// Event coordinates may describe an earlier animation tick by the time Winit
    /// delivers them; querying here also preserves user resizes after completion.
    pub fn quick_terminal_saved_frame(&self, window: &Window) -> Result<[f64; 4], String> {
        let native = native_window(window)?;
        let frame = self
            .quick_animations
            .borrow()
            .get(&window.id())
            .and_then(|state| {
                let state = state.borrow();
                state.running.then_some(state.frame)
            })
            .unwrap_or_else(|| native.frame());
        Ok(logical_screen_frame(
            frame,
            CGDisplayBounds(CGMainDisplayID()).size.height,
        ))
    }

    /// Invalidate native completions before Winit closes its window and delegate.
    pub fn forget_window(&self, window: &Window) {
        if let Some(state) = self.quick_animations.borrow_mut().remove(&window.id()) {
            state.borrow_mut().cancel();
        }
    }

    /// Select the screen on each reveal, retaining the user's resized dimensions.
    /// Winit created this window as a nonactivating NSPanel, so focusing it leaves
    /// the previously active application in control of the menu bar and its windows.
    pub fn show_quick(&self, window: &Window, config: &Config) -> Result<(), String> {
        let native = native_window(window)?;
        self.configure_window(window, true, config)?;
        let state = self.quick_animation(window, &native);
        let saved = {
            let state = state.borrow();
            if state.running {
                state.frame
            } else {
                native.frame()
            }
        };
        if !state.borrow().visible || !native.isVisible() {
            *self.previous_quick_app.borrow_mut() = NSWorkspace::sharedWorkspace()
                .frontmostApplication()
                .filter(|app| app.processIdentifier() != std::process::id() as i32);
        }
        let screen = self.quick_screen(config.quick_terminal_screen);
        let frame = screen.as_ref().map_or(saved, |screen| {
            let primary_height = CGDisplayBounds(CGMainDisplayID()).size.height;
            let [x, y, width, height] = quick_frame(
                logical_screen_frame(screen.visibleFrame(), primary_height),
                config.quick_terminal_position,
                Some([saved.size.width, saved.size.height]),
            );
            NSRect::new(
                NSPoint::new(x, primary_height - y - height),
                NSSize::new(width, height),
            )
        });
        let duration = if screen.is_some() {
            config.quick_terminal_animation_duration
        } else {
            Duration::ZERO
        };
        let generation = state.borrow_mut().begin(frame, true);
        let fade = config.quick_terminal_position == QuickTerminalPosition::Center;
        if !native.isVisible() {
            let initial = if duration.is_zero() {
                frame
            } else {
                quick_hidden_frame(
                    frame,
                    screen.as_ref().unwrap().frame(),
                    config.quick_terminal_position,
                )
            };
            native.setFrame_display(initial, false);
            native.setAlphaValue(if fade && !duration.is_zero() {
                0.0
            } else {
                1.0
            });
        }
        window.set_visible(true);
        window.focus_window();
        animate_quick(native, state, generation, frame, 1.0, duration);
        Ok(())
    }

    /// `restore_focus` is true only for an explicit toggle/close. Autohide must
    /// leave the app or window the user just selected in control of keyboard focus.
    pub fn hide_quick(
        &self,
        window: &Window,
        restore_focus: bool,
        config: &Config,
    ) -> Result<(), String> {
        let native = native_window(window)?;
        let state = self.quick_animation(window, &native);
        let frame = {
            let state = state.borrow();
            if state.running {
                state.frame
            } else {
                native.frame()
            }
        };
        let screen = native.screen();
        let duration = if native.isVisible() && native.isOnActiveSpace() && screen.is_some() {
            config.quick_terminal_animation_duration
        } else {
            Duration::ZERO
        };
        // Invalidate a previous show before restoring focus, which can synchronously
        // produce focus-loss notifications and another autohide request.
        let generation = state.borrow_mut().begin(frame, false);
        let previous = self.previous_quick_app.borrow_mut().take();
        if may_restore_quick_focus(
            restore_focus,
            native.isKeyWindow(),
            NSApplication::sharedApplication(self.mtm).isActive(),
            native.isOnActiveSpace(),
        ) && let Some(previous) = previous.filter(|app| !app.isTerminated())
        {
            // Restore before ordering out, so macOS does not first raise a normal
            // Rustty window. Never force activation over a newly selected app.
            previous.activateWithOptions(NSApplicationActivationOptions::empty());
        }
        let target = screen.as_ref().map_or(frame, |screen| {
            quick_hidden_frame(frame, screen.frame(), config.quick_terminal_position)
        });
        let alpha = if config.quick_terminal_position == QuickTerminalPosition::Center {
            0.0
        } else {
            1.0
        };
        animate_quick(native, state, generation, target, alpha, duration);
        Ok(())
    }

    /// Ignore Winit's initial unfocused event and stale events overtaken by a reveal.
    /// Forget a real activation target even when autohide is disabled.
    pub fn quick_resigned_focus(&self, window: &Window, was_focused: bool) -> Result<bool, String> {
        let lost = was_focused && !native_window(window)?.isKeyWindow();
        if lost {
            self.previous_quick_app.borrow_mut().take();
            if let Some(state) = self.quick_animations.borrow().get(&window.id()) {
                state.borrow_mut().focus = false;
            }
        }
        Ok(lost)
    }

    /// The host applies its clipboard policy before calling native accessors.
    /// Missing representations are a successful read with no matching content.
    pub fn clipboard_read(&self, request: &clipboard::Read) -> ReadResult {
        let Some(pasteboard) = clipboard_pasteboard(request.location) else {
            return if request.location == Location::Primary {
                ReadResult::Unsupported
            } else {
                ReadResult::IoError
            };
        };
        read_pasteboard(&pasteboard, request)
    }

    /// Publish all representations together after validating their native types.
    pub fn clipboard_write(&self, request: &clipboard::Write) -> WriteResult {
        let Some(pasteboard) = clipboard_pasteboard(request.location) else {
            return if request.location == Location::Primary {
                WriteResult::Unsupported
            } else {
                WriteResult::IoError
            };
        };
        write_pasteboard(&pasteboard, &request.contents)
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

    pub fn open_url(&self, target: &str, cwd: &Path) -> Result<(), String> {
        let url = url_for_opening(target, cwd)?;
        open_native_url(&url)
    }

    pub fn choose_layout_path(&self) -> Result<Option<PathBuf>, String> {
        let panel = NSOpenPanel::openPanel(self.mtm);
        panel.setTitle(Some(&NSString::from_str("Import Saved Layout")));
        panel.setMessage(Some(&NSString::from_str(
            "Choose a Rustty workspace JSON file or a Ghostty .savedState folder.",
        )));
        panel.setPrompt(Some(&NSString::from_str("Import")));
        panel.setCanChooseFiles(true);
        panel.setCanChooseDirectories(true);
        panel.setAllowsMultipleSelection(false);
        panel.setCanCreateDirectories(false);
        panel.setTreatsFilePackagesAsDirectories(false);
        // The string-based API avoids adding a UniformTypeIdentifiers dependency.
        #[allow(deprecated)]
        panel.setAllowedFileTypes(Some(&NSArray::from_slice(&[
            &*NSString::from_str("json"),
            &*NSString::from_str("savedState"),
        ])));
        let response = panel.runModal();
        if response == NSModalResponseCancel {
            return Ok(None);
        }
        if response != NSModalResponseOK {
            return Err("Could not display the saved layout picker".into());
        }
        panel
            .URL()
            .and_then(|url| url.to_file_path())
            .map(Some)
            .ok_or_else(|| "The selected layout has no valid file path".into())
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
        // Legacy Ghostty/Rustty extensions may have no Launch Services handler.
        // `-t` selects the user's default text editor instead of the file type.
        let status = std::process::Command::new("/usr/bin/open")
            .arg("-t")
            .arg(path)
            .status()
            .map_err(|error| format!("Could not open settings in a text editor: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "The text editor could not open {} ({status})",
                path.display()
            ))
        }
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

// These four stable selectors avoid introducing a second objc2 generation just
// for MIME lookup. UniformTypeIdentifiers is available on our minimum macOS 13.
#[link(name = "UniformTypeIdentifiers", kind = "framework")]
unsafe extern "C" {}
extern_class!(
    #[unsafe(super(NSObject))]
    struct UTType;
);
impl UTType {
    extern_methods!(
        #[unsafe(method(typeWithMIMEType:))]
        #[unsafe(method_family = none)]
        fn from_mime(mime: &NSString) -> Option<Retained<Self>>;
        #[unsafe(method(typeWithIdentifier:))]
        #[unsafe(method_family = none)]
        fn from_identifier(identifier: &NSString) -> Option<Retained<Self>>;
        #[unsafe(method(identifier))]
        #[unsafe(method_family = none)]
        fn identifier(&self) -> Retained<NSString>;
        #[unsafe(method(preferredMIMEType))]
        #[unsafe(method_family = none)]
        fn preferred_mime(&self) -> Option<Retained<NSString>>;
    );
}

fn clipboard_pasteboard(location: Location) -> Option<Retained<NSPasteboard>> {
    // AppKit can return nil when its pasteboard service is unavailable, despite
    // the nonnull header annotations. Treat this as an IO failure at the API.
    match location {
        Location::Standard => unsafe { msg_send![objc2::class!(NSPasteboard), generalPasteboard] },
        Location::Selection => unsafe {
            msg_send![objc2::class!(NSPasteboard), pasteboardWithName: &*NSString::from_str("app.rustty.selection")]
        },
        Location::Primary => None,
    }
}

fn clipboard_read_denied(pasteboard: &NSPasteboard) -> bool {
    // macOS 15.4 added explicit per-app pasteboard access policy. Earlier
    // versions have no such selector and continue through normal native access.
    pasteboard.respondsToSelector(sel!(accessBehavior))
        && pasteboard.accessBehavior() == NSPasteboardAccessBehavior::AlwaysDeny
}

fn valid_clipboard_mime(mime: &[u8]) -> Option<&str> {
    if clipboard::is_text_mime(mime) {
        return std::str::from_utf8(mime).ok();
    }
    let mime = std::str::from_utf8(mime).ok()?;
    if mime.len() > 1024 || !mime.is_ascii() || mime.bytes().any(|b| b.is_ascii_control()) {
        return None;
    }
    let base = mime.split(';').next()?.trim();
    let (kind, subtype) = base.split_once('/')?;
    let token = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    };
    (token(kind) && token(subtype)).then_some(mime)
}

fn clipboard_type(mime: &[u8]) -> Option<Retained<NSString>> {
    if clipboard::is_text_mime(mime) {
        return Some(unsafe { NSPasteboardTypeString }.copy());
    }
    let mime = NSString::from_str(valid_clipboard_mime(mime)?);
    Some(UTType::from_mime(&mime).map_or(mime, |kind| kind.identifier()))
}

fn clipboard_mime(kind: &NSString) -> Option<Vec<u8>> {
    if kind.isEqualToString(unsafe { NSPasteboardTypeString }) {
        return Some(b"text/plain".to_vec());
    }
    let raw = kind.to_string();
    let mime = UTType::from_identifier(kind)
        .and_then(|kind| kind.preferred_mime())
        .map(|mime| mime.to_string())
        .or_else(|| valid_clipboard_mime(raw.as_bytes()).map(str::to_owned))?;
    Some(if clipboard::is_text_mime(mime.as_bytes()) {
        b"text/plain".to_vec()
    } else {
        mime.into_bytes()
    })
}

fn read_pasteboard(pasteboard: &NSPasteboard, request: &clipboard::Read) -> ReadResult {
    if clipboard_read_denied(pasteboard) {
        return ReadResult::Denied;
    }
    let generation = pasteboard.changeCount();
    if generation < 0 {
        return ReadResult::IoError;
    }
    let types = pasteboard.types();
    let mut success = ReadSuccess::default();
    if request.list {
        for kind in types.iter().flat_map(|types| types.iter()) {
            if let Some(mime) = clipboard_mime(&kind)
                && !success.available.contains(&mime)
            {
                success.available.push(mime);
            }
        }
    }
    // Different MIME aliases may refer to the same native representation.
    // Read it once, then share its bytes rather than multiplying large images.
    let mut cached = HashMap::<String, Arc<[u8]>>::new();
    let mut total = 0usize;
    for mime in &request.mimes {
        if success.contents.iter().any(|content| &content.mime == mime) {
            continue;
        }
        let Some(kind) = clipboard_type(mime) else {
            continue;
        };
        let key = kind.to_string();
        let data = if let Some(data) = cached.get(&key) {
            data.clone()
        } else if let Some(data) = pasteboard.dataForType(&kind) {
            total = total.saturating_add(data.len());
            if total > 64 * 1024 * 1024 {
                return ReadResult::IoError;
            }
            let data: Arc<[u8]> = data.to_vec().into();
            cached.insert(key, data.clone());
            data
        } else {
            if pasteboard.changeCount() != generation {
                return ReadResult::Busy;
            }
            if clipboard_read_denied(pasteboard) {
                return ReadResult::Denied;
            }
            // A declared representation with no bytes is a failed lazy provider,
            // distinct from a requested type that simply is not available.
            if types
                .as_ref()
                .is_some_and(|types| types.containsObject(&kind))
            {
                return ReadResult::IoError;
            }
            continue;
        };
        success.contents.push(Content {
            mime: mime.clone(),
            data,
        });
    }
    if pasteboard.changeCount() != generation {
        ReadResult::Busy
    } else {
        ReadResult::Success(success)
    }
}

fn write_pasteboard(pasteboard: &NSPasteboard, contents: &[Content]) -> WriteResult {
    let item = NSPasteboardItem::new();
    let mut types = HashMap::<String, Arc<[u8]>>::new();
    let mut data_cache = HashMap::new();
    for content in contents {
        let Some(kind) = clipboard_type(&content.mime) else {
            return WriteResult::InvalidData;
        };
        let key = kind.to_string();
        if let Some(previous) = types.get(&key) {
            if !Arc::ptr_eq(previous, &content.data) && previous != &content.data {
                return WriteResult::InvalidData;
            }
            continue;
        }
        // Alias MIME names share their allocation in the core. Preserve that
        // sharing while staging native representations, even for distinct UTIs.
        let data = data_cache
            .entry((content.data.as_ptr(), content.data.len()))
            .or_insert_with(|| NSData::with_bytes(&content.data));
        if !item.setData_forType(data, &kind) {
            return WriteResult::InvalidData;
        }
        types.insert(key, content.data.clone());
    }
    let generation = pasteboard.clearContents();
    if generation < 0 {
        return WriteResult::IoError;
    }
    if !contents.is_empty()
        && !pasteboard.writeObjects(&NSArray::from_slice(&[ProtocolObject::from_ref(&*item)]))
    {
        return if pasteboard.changeCount() != generation {
            WriteResult::Busy
        } else {
            WriteResult::IoError
        };
    }
    if pasteboard.changeCount() != generation {
        WriteResult::Busy
    } else {
        WriteResult::Success { remember: false }
    }
}

fn quick_collection_behavior(behavior: QuickTerminalSpaceBehavior) -> NSWindowCollectionBehavior {
    let spaces = match behavior {
        QuickTerminalSpaceBehavior::Move => NSWindowCollectionBehavior::CanJoinAllSpaces,
        QuickTerminalSpaceBehavior::Remain => NSWindowCollectionBehavior::MoveToActiveSpace,
    };
    spaces
        | NSWindowCollectionBehavior::IgnoresCycle
        | NSWindowCollectionBehavior::FullScreenAuxiliary
}

#[derive(Debug)]
struct QuickAnimation {
    generation: u64,
    frame: NSRect,
    visible: bool,
    running: bool,
    focus: bool,
}

impl QuickAnimation {
    fn new(frame: NSRect) -> Self {
        Self {
            generation: 0,
            frame,
            visible: false,
            running: false,
            focus: false,
        }
    }

    fn begin(&mut self, frame: NSRect, visible: bool) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.frame = frame;
        self.visible = visible;
        self.focus = visible;
        self.running = true;
        self.generation
    }

    fn current(&self, generation: u64) -> bool {
        self.running && self.generation == generation
    }

    fn finish(&mut self, generation: u64) -> bool {
        if !self.current(generation) {
            return false;
        }
        self.running = false;
        true
    }

    fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.running = false;
        self.focus = false;
    }
}

fn animate_quick(
    native: Retained<NSWindow>,
    state: Rc<RefCell<QuickAnimation>>,
    generation: u64,
    target: NSRect,
    alpha: f64,
    duration: Duration,
) {
    let (target, alpha) = if duration.is_zero() {
        (state.borrow().frame, 1.0)
    } else {
        (target, alpha)
    };
    let window = native.clone();
    let completion = RcBlock::new(move || {
        let (frame, visible, focus) = {
            let state = state.borrow();
            if !state.current(generation) {
                return;
            }
            (state.frame, state.visible, state.focus)
        };
        // Closing a Winit window clears its delegate. A retained animation proxy
        // must never bring that closed window back or try to focus its old view.
        if window.delegate().is_some() {
            if !visible {
                window.orderOut(None);
            }
            window.setFrame_display(frame, false);
            window.setAlphaValue(1.0);
            // IME candidate windows must be able to appear above the terminal.
            window.setLevel(NSFloatingWindowLevel);
            if visible && focus && window.isVisible() {
                window.makeKeyAndOrderFront(None);
            }
        }
        // Settle the native frame before allowing queued move events to read it.
        state.borrow_mut().finish(generation);
    });
    if !duration.is_zero() {
        native.setLevel(NSPopUpMenuWindowLevel);
    }
    let changes = RcBlock::new(move |context: NonNull<NSAnimationContext>| {
        // AppKit supplies the current animation context for this synchronous block.
        unsafe { context.as_ref() }.setDuration(duration.as_secs_f64());
        let animator = native.animator();
        animator.setFrame_display(target, true);
        animator.setAlphaValue(alpha);
    });
    if duration.is_zero() {
        // Retarget the animator as well as settling the window. A direct frame
        // assignment alone would let an interrupted animation keep moving it.
        NSAnimationContext::runAnimationGroup(&changes);
        completion.call(());
    } else {
        NSAnimationContext::runAnimationGroup_completionHandler(&changes, Some(&completion));
    }
}

fn quick_hidden_frame(
    mut frame: NSRect,
    screen: NSRect,
    position: QuickTerminalPosition,
) -> NSRect {
    match position {
        QuickTerminalPosition::Top => frame.origin.y = screen.origin.y + screen.size.height,
        QuickTerminalPosition::Bottom => frame.origin.y = screen.origin.y - frame.size.height,
        QuickTerminalPosition::Left => frame.origin.x = screen.origin.x - frame.size.width,
        QuickTerminalPosition::Right => frame.origin.x = screen.origin.x + screen.size.width,
        QuickTerminalPosition::Center => {}
    }
    frame
}

fn logical_screen_frame(frame: NSRect, primary_height: f64) -> [f64; 4] {
    [
        frame.origin.x,
        primary_height - frame.origin.y - frame.size.height,
        frame.size.width,
        frame.size.height,
    ]
}

fn quick_frame(
    visible: [f64; 4],
    position: QuickTerminalPosition,
    saved_size: Option<[f64; 2]>,
) -> [f64; 4] {
    let [x, y, screen_width, screen_height] = visible;
    let [width, height] = saved_size.unwrap_or(match position {
        QuickTerminalPosition::Top | QuickTerminalPosition::Bottom => {
            [screen_width, screen_height * 0.5]
        }
        QuickTerminalPosition::Left | QuickTerminalPosition::Right => {
            [screen_width * 0.5, screen_height]
        }
        QuickTerminalPosition::Center => [screen_width * 0.8, screen_height * 0.7],
    });
    let width = width.min(screen_width);
    let height = height.min(screen_height);
    let x = x + match position {
        QuickTerminalPosition::Left => 0.0,
        QuickTerminalPosition::Right => screen_width - width,
        _ => (screen_width - width) * 0.5,
    };
    let y = y + match position {
        QuickTerminalPosition::Top => 0.0,
        QuickTerminalPosition::Bottom => screen_height - height,
        _ => (screen_height - height) * 0.5,
    };
    [x.round(), y.round(), width, height]
}

fn may_restore_quick_focus(
    explicit: bool,
    key: bool,
    app_active: bool,
    active_space: bool,
) -> bool {
    explicit && key && app_active && active_space
}

fn url_for_opening(target: &str, cwd: &Path) -> Result<Retained<NSURL>, String> {
    if target.is_empty() || target.chars().any(char::is_control) {
        return Err("URL or file path is empty or contains control characters".into());
    }
    let target = NSString::from_str(target);
    if let Some(url) = NSURL::URLWithString(&target)
        && url.scheme().is_some()
    {
        return Ok(url);
    }
    let path = target.stringByExpandingTildeInPath().to_string();
    NSURL::from_file_path(cwd.join(path)).ok_or_else(|| "invalid file path".into())
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
        ("Open Saved Layout…", Action::OpenLayout),
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

fn update_native_menu_accelerators(menu: &NSMenu, bindings: &[KeyBinding]) {
    for item in menu.itemArray() {
        if let Some(submenu) = item.submenu() {
            update_native_menu_accelerators(&submenu, bindings);
        }
        let Some(action) = item.action() else {
            continue;
        };
        let (key, alt) = if action == sel!(hide:) {
            ("h", false)
        } else if action == sel!(hideOtherApplications:) {
            ("h", true)
        } else if action == sel!(performMiniaturize:) {
            ("m", false)
        } else {
            continue;
        };
        // Muda has no accelerator setter for predefined items. Keep their native
        // actions and modifier masks, but let configured keys reach app routing.
        item.setKeyEquivalent(&NSString::from_str(native_menu_key_equivalent(
            bindings,
            key,
            Modifiers {
                alt,
                super_key: true,
                ..Modifiers::default()
            },
        )));
    }
}

fn native_menu_key_equivalent<'a>(
    bindings: &[KeyBinding],
    key: &'a str,
    modifiers: Modifiers,
) -> &'a str {
    let configured = bindings
        .iter()
        .filter(|binding| binding.table.is_none())
        .flat_map(|binding| &binding.trigger)
        .any(|trigger| {
            trigger.modifiers == modifiers
                && (trigger.physical
                    || trigger.key == "catch_all"
                    || trigger
                        .key
                        .strip_prefix("key_")
                        .unwrap_or(&trigger.key)
                        .eq_ignore_ascii_case(key))
        });
    // Include conditional bindings and every sequence step. Physical bindings
    // stay in app routing since their characters depend on the keyboard layout.
    if configured { "" } else { key }
}

// Carbon exposes registered shortcuts without access to the keyboard event stream.
mod carbon {
    use super::*;

    pub const SIGNATURE: u32 = u32::from_be_bytes(*b"Rsty");
    pub const NOT_HANDLED: i32 = -9874;
    pub const EXCLUSIVE: u32 = 1;

    #[repr(C, packed(2))]
    pub struct HotkeyId {
        pub signature: u32,
        pub id: u32,
    }

    #[repr(C, packed(2))]
    pub struct EventType {
        pub class: u32,
        pub kind: u32,
    }

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        pub fn GetApplicationEventTarget() -> *mut c_void;
        pub fn InstallEventHandler(
            target: *mut c_void,
            handler: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
            count: usize,
            events: *const EventType,
            data: *mut c_void,
            result: *mut *mut c_void,
        ) -> i32;
        pub fn RemoveEventHandler(handler: *mut c_void) -> i32;
        pub fn RegisterEventHotKey(
            code: u32,
            modifiers: u32,
            id: HotkeyId,
            target: *mut c_void,
            options: u32,
            result: *mut *mut c_void,
        ) -> i32;
        pub fn UnregisterEventHotKey(hotkey: *mut c_void) -> i32;
        pub fn GetEventParameter(
            event: *mut c_void,
            name: u32,
            kind: u32,
            actual_kind: *mut u32,
            size: usize,
            actual_size: *mut usize,
            data: *mut c_void,
        ) -> i32;
        pub static kTISNotifySelectedKeyboardInputSourceChanged: *const NSString;
    }
}

struct HotkeyRegistration {
    reference: NonNull<c_void>,
    binding: usize,
    key_code: u16,
}

impl Drop for HotkeyRegistration {
    fn drop(&mut self) {
        // SAFETY: owned successful registration, released on the main thread.
        unsafe { carbon::UnregisterEventHotKey(self.reference.as_ptr()) };
    }
}

struct GlobalKeyContext {
    callback: EventSink,
    bindings: Vec<KeyBinding>,
    registrations: RefCell<HashMap<u32, HotkeyRegistration>>,
}

struct GlobalKeys {
    context: Rc<GlobalKeyContext>,
    handler: NonNull<c_void>,
    observer: Retained<KeyboardLayoutObserver>,
}

impl GlobalKeys {
    fn new(
        mtm: MainThreadMarker,
        bindings: Vec<KeyBinding>,
        callback: EventSink,
    ) -> Result<Self, String> {
        let context = Rc::new(GlobalKeyContext {
            callback,
            bindings,
            registrations: RefCell::default(),
        });
        let event = carbon::EventType {
            class: u32::from_be_bytes(*b"keyb"),
            kind: 5, // kEventHotKeyPressed; releases must not execute actions.
        };
        let mut handler = std::ptr::null_mut();
        // SAFETY: the stable Rc allocation outlives the main-thread handler.
        // The Carbon ABI uses two-byte packing for its event records.
        let status = unsafe {
            carbon::InstallEventHandler(
                carbon::GetApplicationEventTarget(),
                global_key_event,
                1,
                &event,
                Rc::as_ptr(&context).cast_mut().cast(),
                &mut handler,
            )
        };
        if status != 0 {
            return Err(format!(
                "Could not enable global shortcuts (OSStatus {status})"
            ));
        }
        let handler =
            NonNull::new(handler).ok_or("Could not enable global shortcuts: no event handler")?;
        let observer = KeyboardLayoutObserver::new(mtm, context.clone());
        // SAFETY: selector accepts an NSNotification and runs on the main thread.
        // The selected-input-source notification is distributed even when another
        // application owns keyboard focus. The observer is removed before drop.
        unsafe {
            NSDistributedNotificationCenter::defaultCenter()
                .addObserver_selector_name_object_suspensionBehavior(
                    &observer,
                    sel!(keyboardLayoutChanged:),
                    Some(&*carbon::kTISNotifySelectedKeyboardInputSourceChanged),
                    None,
                    NSNotificationSuspensionBehavior::DeliverImmediately,
                );
        }
        let keys = Self {
            context,
            handler,
            observer,
        };
        keys.context.rebuild();
        Ok(keys)
    }
}

impl Drop for GlobalKeys {
    fn drop(&mut self) {
        // SAFETY: unregister the observer and handler while their context still
        // exists. All callbacks and teardown run on the application's main thread.
        unsafe {
            NSDistributedNotificationCenter::defaultCenter().removeObserver_name_object(
                &self.observer,
                Some(&*carbon::kTISNotifySelectedKeyboardInputSourceChanged),
                None,
            );
            self.context.registrations.borrow_mut().clear();
            carbon::RemoveEventHandler(self.handler.as_ptr());
        }
    }
}

// IDs are never reused, even after removing all shortcuts and adding them again.
// This also makes already-queued PlatformEvents harmless after a rebuild.
static NEXT_HOTKEY_ID: AtomicU32 = AtomicU32::new(1);

impl GlobalKeyContext {
    fn rebuild(&self) {
        self.registrations.borrow_mut().clear();
        let (resolved, warnings) = resolve_global_keys(&self.bindings, layout_character);
        for warning in warnings {
            (self.callback)(PlatformEvent::Warning(warning));
        }
        for ((code, modifiers), binding) in resolved {
            let Ok(id) = NEXT_HOTKEY_ID
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            else {
                (self.callback)(PlatformEvent::Warning(
                    "Global shortcut registration IDs exhausted; restart Rustty".into(),
                ));
                break;
            };
            let mut reference = std::ptr::null_mut();
            // SAFETY: valid key/modifier values and an out-pointer, on the main
            // thread. A successful registration is owned by HotkeyRegistration.
            let status = unsafe {
                carbon::RegisterEventHotKey(
                    u32::from(code),
                    modifiers,
                    carbon::HotkeyId {
                        signature: carbon::SIGNATURE,
                        id,
                    },
                    carbon::GetApplicationEventTarget(),
                    carbon::EXCLUSIVE,
                    &mut reference,
                )
            };
            if status == 0
                && let Some(reference) = NonNull::new(reference)
            {
                self.registrations.borrow_mut().insert(
                    id,
                    HotkeyRegistration {
                        reference,
                        binding,
                        key_code: code,
                    },
                );
            } else {
                (self.callback)(PlatformEvent::Warning(format!(
                    "Global shortcut {} could not be registered for key code {code} (OSStatus {status}); it may be reserved or used by another application. Other shortcuts remain available.",
                    global_shortcut_label(&self.bindings[binding].trigger[0]),
                )));
            }
        }
    }
}

fn global_shortcut_label(trigger: &KeyTrigger) -> String {
    let mut label = String::from("global:");
    if trigger.physical {
        label.push_str("physical:");
    }
    for (enabled, name) in [
        (trigger.modifiers.control, "ctrl+"),
        (trigger.modifiers.alt, "alt+"),
        (trigger.modifiers.shift, "shift+"),
        (trigger.modifiers.super_key, "super+"),
    ] {
        if enabled {
            label.push_str(name);
        }
    }
    label.push_str(&trigger.key);
    label
}

fn carbon_modifiers(modifiers: Modifiers) -> u32 {
    u32::from(modifiers.super_key) << 8
        | u32::from(modifiers.shift) << 9
        | u32::from(modifiers.alt) << 11
        | u32::from(modifiers.control) << 12
}

fn resolve_global_keys(
    bindings: &[KeyBinding],
    mut character: impl FnMut(u16, bool) -> Option<String>,
) -> (BTreeMap<(u16, u32), usize>, Vec<String>) {
    let mut resolved = BTreeMap::new();
    let mut warnings = Vec::new();
    for (index, binding) in bindings.iter().enumerate() {
        let [trigger] = binding.trigger.as_slice() else {
            warnings.push("Global shortcuts require a single key combination".into());
            continue;
        };
        let mut matched = false;
        if trigger.key != "catch_all" {
            for code in 0..128 {
                let text = character(code, trigger.modifiers.shift).unwrap_or_default();
                if key_matches(trigger, code, &text, trigger.modifiers) {
                    // Match the existing reverse search: a later binding wins
                    // when logical and physical aliases resolve to the same key.
                    resolved.insert((code, carbon_modifiers(trigger.modifiers)), index);
                    matched = true;
                }
            }
        }
        if !matched {
            warnings.push(format!(
                "Global shortcut {} has no supported key in the current keyboard layout; use an explicit supported key combination",
                global_shortcut_label(trigger),
            ));
        }
    }
    (resolved, warnings)
}

fn layout_character(code: u16, shift: bool) -> Option<String> {
    // AppKit retranslates without changing the input method's dead-key state.
    // Like charactersIgnoringModifiers in the old event path, retain Shift only.
    let empty = NSString::new();
    let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown, NSPoint::ZERO, NSEventModifierFlags::empty(), 0.0, 0,
        None, &empty, &empty, false, code,
    )?;
    event
        .charactersByApplyingModifiers(if shift {
            NSEventModifierFlags::Shift
        } else {
            NSEventModifierFlags::empty()
        })
        .map(|text| text.to_string())
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Rc<GlobalKeyContext>]
    struct KeyboardLayoutObserver;
    unsafe impl NSObjectProtocol for KeyboardLayoutObserver {}
    impl KeyboardLayoutObserver {
        #[unsafe(method(keyboardLayoutChanged:))]
        fn keyboard_layout_changed(&self, _notification: &NSNotification) {
            self.ivars().rebuild();
        }
    }
);

impl KeyboardLayoutObserver {
    fn new(mtm: MainThreadMarker, context: Rc<GlobalKeyContext>) -> Retained<Self> {
        unsafe { msg_send![super(Self::alloc(mtm).set_ivars(context)), init] }
    }
}

unsafe extern "C" fn global_key_event(
    _next_handler: *mut c_void,
    event: *mut c_void,
    user_info: *mut c_void,
) -> i32 {
    let mut hotkey = carbon::HotkeyId {
        signature: 0,
        id: 0,
    };
    // SAFETY: Carbon supplies the event and the stable context installed above.
    // GetEventParameter validates the parameter type and destination buffer size.
    let status = unsafe {
        carbon::GetEventParameter(
            event,
            u32::from_be_bytes(*b"----"),
            u32::from_be_bytes(*b"hkid"),
            std::ptr::null_mut(),
            std::mem::size_of_val(&hotkey),
            std::ptr::null_mut(),
            (&mut hotkey as *mut carbon::HotkeyId).cast(),
        )
    };
    if status != 0 || hotkey.signature != carbon::SIGNATURE {
        return carbon::NOT_HANDLED;
    }
    let id = hotkey.id;
    let context = unsafe { &*user_info.cast::<GlobalKeyContext>() };
    if let Some(registration) = context.registrations.borrow().get(&id) {
        (context.callback)(PlatformEvent::GlobalHotkey {
            id,
            key_code: registration.key_code,
        });
    }
    0
}

fn key_matches(trigger: &KeyTrigger, code: u16, text: &str, modifiers: Modifiers) -> bool {
    if trigger.modifiers != modifiers {
        return false;
    }
    let key = trigger.key.as_str();
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
    fn link_urls_resolve_file_paths_against_the_pane_directory() {
        let cwd = Path::new("/Users/byron/dev/github.com/zed-industries/zed");
        let relative = "target/aarch64-apple-darwin/release-fast/Zed-aarch64.dmg";
        let absolute = cwd.join(relative);
        for (target, expected) in [
            (absolute.to_str().unwrap(), absolute.clone()),
            (relative, absolute.clone()),
            ("./target/Zed.dmg", cwd.join("target/Zed.dmg")),
            ("../Zed.dmg", cwd.join("../Zed.dmg")),
            (
                "target/Zed Dev#1?100%.dmg",
                cwd.join("target/Zed Dev#1?100%.dmg"),
            ),
            (
                "~/Downloads/Zed.dmg",
                PathBuf::from(objc2_foundation::NSHomeDirectory().to_string())
                    .join("Downloads/Zed.dmg"),
            ),
        ] {
            let url = url_for_opening(target, cwd).unwrap();
            assert_eq!(
                url.to_file_path().as_deref(),
                Some(expected.as_path()),
                "{target}"
            );
            assert!(url.query().is_none());
            assert!(url.fragment().is_none());
        }
        for target in [
            "https://example.org/Zed.dmg?download=1#here",
            "file:///tmp/Zed%20Dev.dmg",
        ] {
            assert_eq!(
                url_for_opening(target, cwd)
                    .unwrap()
                    .absoluteString()
                    .unwrap()
                    .to_string(),
                target
            );
        }
        for target in ["", "/tmp/Zed\n.dmg", "file:///tmp/\0Zed.dmg"] {
            assert!(url_for_opening(target, cwd).is_err());
        }
    }

    struct TestPasteboard(Retained<NSPasteboard>);
    impl Drop for TestPasteboard {
        fn drop(&mut self) {
            // This name was allocated by pasteboardWithUniqueName for this test;
            // releaseGlobally never runs on the user's general/selection board.
            unsafe {
                let _: () = msg_send![&*self.0, releaseGlobally];
            }
        }
    }

    #[test]
    #[ignore = "requires macOS pasteboard service; run explicitly outside the sandbox"]
    fn named_pasteboard_roundtrips_raw_mime_data_without_touching_user_clipboard() {
        let unique: Option<Retained<NSPasteboard>> =
            unsafe { msg_send![objc2::class!(NSPasteboard), pasteboardWithUniqueName] };
        let owned = TestPasteboard(unique.expect("macOS pasteboard service is unavailable"));
        let board = &owned.0;
        let content = |mime: &[u8], data: &[u8]| Content {
            mime: mime.to_vec(),
            data: data.into(),
        };
        let original = vec![
            content(b"text/plain", "hello\0日本語\n".as_bytes()),
            content(b"image/png", b"\x89PNG\r\n\x1a\n\0\xffbinary"),
            content(b"application/x-rustty-opaque", b"\0\x01\xff\x80"),
            content(b"application/x-rustty-empty", b""),
        ];
        assert_eq!(
            write_pasteboard(board, &original),
            WriteResult::Success { remember: false }
        );
        let mut request = clipboard::Read::osc52(Location::Standard, clipboard::Terminator::St);
        request.mimes = original.iter().map(|c| c.mime.clone()).collect();
        request.mimes.push(b"UTF8_STRING".to_vec());
        request
            .mimes
            .push(b"application/x-rustty-unavailable".to_vec());
        request.list = true;
        let ReadResult::Success(read) = read_pasteboard(board, &request) else {
            panic!("could not read test-owned pasteboard");
        };
        assert_eq!(read.contents[..4], original);
        assert_eq!(read.contents.len(), 5);
        assert!(Arc::ptr_eq(&read.contents[0].data, &read.contents[4].data));
        for content in &original {
            assert!(
                read.available.contains(&content.mime),
                "{:?}",
                read.available
            );
        }
        assert!(clipboard_pasteboard(Location::Primary).is_none());

        // Invalid or conflicting types must leave existing contents intact.
        assert_eq!(
            write_pasteboard(board, &[content(b"not-a-mime", b"x")]),
            WriteResult::InvalidData
        );
        assert_eq!(
            write_pasteboard(
                board,
                &[content(b"text/plain", b"a"), content(b"UTF8_STRING", b"b")]
            ),
            WriteResult::InvalidData
        );
        let ReadResult::Success(unchanged) = read_pasteboard(board, &request) else {
            panic!("invalid write changed pasteboard ownership");
        };
        assert_eq!(unchanged.contents, read.contents);

        // A declared lazy representation has no data. Listing must never ask
        // its provider for bytes and must still report the advertised MIME.
        let html = NSString::from_str("public.html");
        unsafe {
            board.declareTypes_owner(&NSArray::from_slice(&[&*html]), None);
        }
        request.mimes.clear();
        let ReadResult::Success(listing) = read_pasteboard(board, &request) else {
            panic!("metadata-only listing attempted to read a lazy provider");
        };
        assert_eq!(listing.available, [b"text/html".to_vec()]);
        assert!(listing.contents.is_empty());

        assert_eq!(
            write_pasteboard(board, &[]),
            WriteResult::Success { remember: false }
        );
        request.mimes.push(b"text/plain".to_vec());
        assert_eq!(
            read_pasteboard(board, &request),
            ReadResult::Success(ReadSuccess::default())
        );
    }

    #[test]
    fn quick_terminal_geometry_uses_visible_points_across_displays() {
        let rect = |x, y, w, h| NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
        let main = logical_screen_frame(rect(0.0, 64.0, 1440.0, 812.0), 900.0);
        assert_eq!(main, [0.0, 24.0, 1440.0, 812.0]);
        assert_eq!(
            quick_frame(main, QuickTerminalPosition::Top, None),
            [0.0, 24.0, 1440.0, 406.0]
        );
        assert_eq!(
            quick_frame(main, QuickTerminalPosition::Bottom, None),
            [0.0, 430.0, 1440.0, 406.0]
        );
        // A display above and to the left has negative Winit coordinates. Its
        // native scale factor never enters the global logical-point conversion.
        let above = logical_screen_frame(rect(-1920.0, 940.0, 1920.0, 1040.0), 900.0);
        assert_eq!(above, [-1920.0, -1080.0, 1920.0, 1040.0]);
        assert_eq!(
            quick_frame(above, QuickTerminalPosition::Right, Some([800.0, 600.0])),
            [-800.0, -860.0, 800.0, 600.0]
        );
        let left = quick_frame(main, QuickTerminalPosition::Left, None);
        assert_eq!(left, [0.0, 24.0, 720.0, 812.0]);
        assert_eq!(
            quick_frame(main, QuickTerminalPosition::Center, Some([2000.0, 2000.0])),
            main
        );
        assert_eq!(
            quick_frame(main, QuickTerminalPosition::Center, Some([640.0, 400.0])),
            [400.0, 230.0, 640.0, 400.0]
        );
    }

    #[test]
    fn quick_terminal_restores_dimensions_when_anchoring_to_the_current_display() {
        let main = [0.0, 24.0, 1440.0, 812.0];
        let position = QuickTerminalPosition::Top;
        let fresh = quick_frame(main, position, None);
        assert_eq!(fresh, [0.0, 24.0, 1440.0, 406.0]);
        assert_eq!(
            quick_frame(main, position, Some([fresh[2], fresh[3]])),
            fresh
        );

        // Reopening keeps a custom size, including legal sizes below 320×180.
        assert_eq!(
            quick_frame(main, position, Some([520.0, 280.0])),
            [460.0, 24.0, 520.0, 280.0]
        );
        assert_eq!(
            quick_frame(main, position, Some([280.0, 150.0])),
            [580.0, 24.0, 280.0, 150.0]
        );
        let smaller = [-480.0, -300.0, 480.0, 240.0];
        assert_eq!(
            quick_frame(smaller, position, Some([520.0, 280.0])),
            smaller
        );
    }

    #[test]
    fn quick_terminal_spaces_and_focus_do_not_displace_user_choices() {
        let moving = quick_collection_behavior(QuickTerminalSpaceBehavior::Move);
        let remaining = quick_collection_behavior(QuickTerminalSpaceBehavior::Remain);
        for flags in [moving, remaining] {
            assert!(flags.contains(NSWindowCollectionBehavior::IgnoresCycle));
            assert!(flags.contains(NSWindowCollectionBehavior::FullScreenAuxiliary));
        }
        assert!(moving.contains(NSWindowCollectionBehavior::CanJoinAllSpaces));
        assert!(!moving.contains(NSWindowCollectionBehavior::MoveToActiveSpace));
        assert!(remaining.contains(NSWindowCollectionBehavior::MoveToActiveSpace));
        assert!(!remaining.contains(NSWindowCollectionBehavior::CanJoinAllSpaces));
        for state in 0..16 {
            assert_eq!(
                may_restore_quick_focus(
                    state & 1 != 0,
                    state & 2 != 0,
                    state & 4 != 0,
                    state & 8 != 0,
                ),
                state == 15,
            );
        }
    }

    #[test]
    fn quick_animation_reversals_keep_the_latest_frame_visibility_and_focus() {
        let frame = NSRect::new(NSPoint::new(-900.0, 1100.0), NSSize::new(640.0, 320.0));
        let mut animation = QuickAnimation::new(frame);
        let show = animation.begin(frame, true);
        let hide = animation.begin(frame, false);
        let resized = NSRect::new(frame.origin, NSSize::new(480.0, 240.0));
        let reveal = animation.begin(resized, true);
        assert!(!animation.finish(hide));
        assert!(!animation.finish(show));
        assert_eq!(animation.frame, resized);
        assert!(animation.visible && animation.running && animation.focus);
        // A user switching windows during the slide must not get focus stolen back.
        animation.focus = false;
        assert!(animation.finish(reveal));
        assert!(!animation.focus);
        assert!(!animation.finish(reveal));
        let closing = animation.begin(resized, false);
        animation.cancel();
        assert!(!animation.finish(closing));
        assert!(!animation.running);
    }

    #[test]
    fn quick_animation_edges_clear_the_selected_native_screen_without_resizing() {
        // A secondary display above/left of the primary still uses global points.
        let screen = NSRect::new(NSPoint::new(-1920.0, 900.0), NSSize::new(1920.0, 1080.0));
        let frame = NSRect::new(NSPoint::new(-1200.0, 1200.0), NSSize::new(640.0, 320.0));
        for (position, origin) in [
            (QuickTerminalPosition::Top, NSPoint::new(-1200.0, 1980.0)),
            (QuickTerminalPosition::Bottom, NSPoint::new(-1200.0, 580.0)),
            (QuickTerminalPosition::Left, NSPoint::new(-2560.0, 1200.0)),
            (QuickTerminalPosition::Right, NSPoint::new(0.0, 1200.0)),
            (QuickTerminalPosition::Center, frame.origin),
        ] {
            let hidden = quick_hidden_frame(frame, screen, position);
            assert_eq!(hidden.origin, origin);
            assert_eq!(hidden.size, frame.size);
        }
    }

    #[test]
    fn native_menu_shortcuts_yield_to_configured_bindings_and_return_after_reload() {
        let command = Modifiers {
            super_key: true,
            ..Modifiers::default()
        };
        let command_option = Modifiers {
            alt: true,
            ..command
        };
        let mut bindings = Config::default().keybinds;
        assert_eq!(native_menu_key_equivalent(&bindings, "h", command), "h");
        assert_eq!(
            native_menu_key_equivalent(&bindings, "h", command_option),
            "h"
        );
        assert_eq!(native_menu_key_equivalent(&bindings, "m", command), "m");
        for (binding, key, modifiers, expected) in [
            ("cmd+h=goto_split:left", "h", command, ""),
            ("cmd+key_h=goto_split:left", "h", command, ""),
            ("performable:cmd+h=goto_split:left", "h", command, ""),
            ("unconsumed:cmd+h=goto_split:left", "h", command, ""),
            ("all:cmd+h=goto_split:left", "h", command, ""),
            ("global:cmd+h=goto_split:left", "h", command, ""),
            ("cmd+h>l=goto_split:left", "h", command, ""),
            ("cmd+k>cmd+h=goto_split:left", "h", command, ""),
            ("cmd+catch_all=goto_split:left", "h", command, ""),
            ("physical:cmd+h=goto_split:left", "h", command, ""),
            // A physical Q can produce H on another keyboard layout.
            ("physical:cmd+q=goto_split:left", "h", command, ""),
            ("cmd+ctrl+h=goto_split:left", "h", command, "h"),
            ("cmd+j=goto_split:left", "h", command, "h"),
            ("navigation/cmd+h=goto_split:left", "h", command, "h"),
            ("cmd+h=goto_split:left", "h", command_option, "h"),
            ("cmd+alt+h=goto_split:left", "h", command_option, ""),
            ("cmd+m=goto_split:left", "m", command, ""),
        ] {
            bindings = vec![KeyBinding::parse(binding).unwrap()];
            assert_eq!(
                native_menu_key_equivalent(&bindings, key, modifiers),
                expected,
                "{binding}"
            );
        }
        bindings.clear();
        assert_eq!(native_menu_key_equivalent(&bindings, "m", command), "m");
    }

    #[test]
    fn global_hotkey_resolution_preserves_layout_modifiers_and_precedence() {
        let bindings = [
            "global:ctrl+a=text:logical",
            "global:physical:ctrl+a=text:physical",
            "global:ctrl+key_q=text:last",
            "global:cmd+shift++=toggle_quick_terminal",
            "global:alt+1=new_tab",
            "global:ctrl+super+arrow_left=new_window",
            "global:cmd+f35=new_tab",
        ]
        .map(|binding| KeyBinding::parse(binding).unwrap());
        let (resolved, warnings) = resolve_global_keys(&bindings, |code, shift| {
            Some(
                match (code, shift) {
                    (0, _) => "q",
                    (12, _) => "a",
                    (18 | 82, false) => "1",
                    (24, true) => "+",
                    _ => return None,
                }
                .into(),
            )
        });
        assert_eq!(
            resolved,
            BTreeMap::from([
                ((0, 4096), 1),
                ((12, 4096), 2),
                ((24, 768), 3),
                ((18, 2048), 4),
                ((82, 2048), 4),
                ((123, 4352), 5),
            ])
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("global:super+f35"));

        // A source change moves a character shortcut but keeps physical keys fixed.
        let (changed, warnings) =
            resolve_global_keys(&bindings[..2], |code, _| (code == 1).then(|| "a".into()));
        assert!(warnings.is_empty());
        assert_eq!(changed, BTreeMap::from([((1, 4096), 0), ((0, 4096), 1)]));
    }

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
