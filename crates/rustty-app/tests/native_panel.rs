//! Run explicitly on a macOS desktop:
//! `cargo test -p rustty-app --test native_panel --offline -- --ignored`
//!
//! AppKit requires the process's main thread, so this target has no libtest harness.
//! Ordinary Cargo test runs skip native initialization. Windows remain hidden.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !std::env::args().any(|arg| arg == "--ignored") {
        println!("native_panel: ignored (pass --ignored to run on the macOS desktop)");
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    macos::run()?;
    #[cfg(not(target_os = "macos"))]
    println!("native_panel: macOS only");
    Ok(())
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::{Arc, Mutex};

    use objc2::{AnyThread, ClassType, MainThreadMarker, rc::Retained, sel};
    use objc2_app_kit::{
        NSApplication, NSBitmapImageRep, NSColorSpace, NSEventModifierFlags, NSImage, NSPanel,
        NSView, NSWindow, NSWindowButton, NSWindowStyleMask, NSWindowTabbingMode, NSWorkspace,
        NSWorkspaceDidWakeNotification, NSWorkspaceScreensDidSleepNotification,
        NSWorkspaceScreensDidWakeNotification, NSWorkspaceSessionDidResignActiveNotification,
        NSWorkspaceWillSleepNotification,
    };
    use objc2_foundation::{NSData, NSObjectProtocol, NSString};
    use rustty::config::{Action, Config, KeyBinding, StreamDeckAction};
    use rustty_app::platform::{Platform, PlatformEvent};
    use winit::{
        application::ApplicationHandler,
        dpi::{LogicalPosition, LogicalSize},
        event::WindowEvent,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::{
            macos::{ActivationPolicy, EventLoopBuilderExtMacOS, WindowAttributesExtMacOS},
            run_on_demand::EventLoopExtRunOnDemand,
        },
        raw_window_handle::{HasWindowHandle, RawWindowHandle},
        window::{Window, WindowId},
    };

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        assert!(MainThreadMarker::new().is_some());
        let mut event_loop = EventLoop::builder()
            .with_activation_policy(ActivationPolicy::Accessory)
            .with_activate_ignoring_other_apps(false)
            .with_default_menu(false)
            .build()?;
        let mut check = Check(false);
        event_loop.run_app_on_demand(&mut check)?;
        assert!(check.0, "Winit never initialized the native test windows");
        println!(
            "native_panel: app icon, window ownership/style, screen-power lifecycle and Stream Deck menu/shortcuts passed"
        );
        Ok(())
    }

    struct Check(bool);

    impl ApplicationHandler for Check {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
            let stale_icon = NSImage::initWithData(
                NSImage::alloc(),
                &NSData::with_bytes(include_bytes!("../../../images/icons/icon_512.png")),
            )
            .unwrap();
            // SAFETY: seed this test process with a valid old icon on the main thread.
            unsafe { app.setApplicationIconImage(Some(&stale_icon)) };
            let mut config = Config::default();
            config.keybinds.clear();
            let power = Arc::new(Mutex::new(Vec::new()));
            let events = Arc::clone(&power);
            let actions = Arc::new(Mutex::new(Vec::new()));
            let action_events = Arc::clone(&actions);
            let mut platform = Platform::new(
                Arc::new(move |event| match event {
                    PlatformEvent::ScreensAwake(awake) => events.lock().unwrap().push(awake),
                    PlatformEvent::Action(action) => action_events.lock().unwrap().push(action),
                    _ => {}
                }),
                &config,
            )
            .unwrap();
            let expected_icon = NSImage::initWithData(
                NSImage::alloc(),
                &NSData::with_bytes(include_bytes!("../resources/icons/rustty.png")),
            )
            .unwrap();
            let actual_icon = app.applicationIconImage().unwrap();
            assert_eq!(actual_icon.size(), expected_icon.size());
            let actual =
                NSBitmapImageRep::imageRepWithData(&actual_icon.TIFFRepresentation().unwrap())
                    .unwrap();
            let expected =
                NSBitmapImageRep::imageRepWithData(&expected_icon.TIFFRepresentation().unwrap())
                    .unwrap();
            // AppKit renders the live icon at Retina resolution in a different color format.
            for (x, y) in [(512, 512), (400, 400), (600, 600), (0, 0)] {
                let color = |rep: &NSBitmapImageRep| {
                    let c = rep
                        .colorAtX_y(x * rep.pixelsWide() / 1024, y * rep.pixelsHigh() / 1024)
                        .unwrap()
                        .colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())
                        .unwrap();
                    [
                        c.redComponent(),
                        c.greenComponent(),
                        c.blueComponent(),
                        c.alphaComponent(),
                    ]
                };
                let a = color(&actual);
                let e = color(&expected);
                assert!(
                    a.into_iter().zip(e).all(|(a, e)| (a - e).abs() < 0.06),
                    "startup must replace the stale Ghostty Dock/Cmd-Tab icon: {a:?} != {e:?}"
                );
            }
            check_stream_deck_menu(&mut platform, &mut config, &actions);
            assert_eq!(power.lock().unwrap().len(), 1);
            power.lock().unwrap().clear();
            let center = NSWorkspace::sharedWorkspace().notificationCenter();
            // SAFETY: post only to this isolated process's notification center;
            // these synthetic notifications do not sleep, wake or lock the Mac.
            unsafe {
                center.postNotificationName_object(NSWorkspaceScreensDidSleepNotification, None);
                center.postNotificationName_object(NSWorkspaceScreensDidWakeNotification, None);
                center.postNotificationName_object(NSWorkspaceWillSleepNotification, None);
                center.postNotificationName_object(NSWorkspaceDidWakeNotification, None);
                center.postNotificationName_object(
                    NSWorkspaceSessionDidResignActiveNotification,
                    None,
                );
            }
            assert_eq!(&power.lock().unwrap()[..3], [false, true, false]);
            assert_eq!(power.lock().unwrap().len(), 4);
            for panel in [false, true] {
                let window = event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_title("Rustty native panel check")
                            .with_visible(false)
                            .with_active(false)
                            .with_decorations(false)
                            .with_fullsize_content_view(true)
                            .with_nonactivating_panel(panel)
                            .with_inner_size(LogicalSize::new(320.0, 180.0)),
                    )
                    .unwrap();
                platform.configure_window(&window, panel, &config).unwrap();
                let id = window.id();
                let native = check_window(&window, panel);
                assert_eq!(native.tabbingMode(), NSWindowTabbingMode::Disallowed);
                let view = native.contentView().unwrap();
                assert!(Platform::window_controls_rect(&window).is_none());

                // Exercise Winit's temporary zoom mask as well as ordinary edits.
                // The old path replaced the entire borderless style to query zoom.
                let _ = window.is_maximized();
                window.set_decorations(true);
                let controls = Platform::window_controls_rect(&window)
                    .expect("decorated window exposes its traffic lights");
                assert!(
                    controls[0] >= 0.0 && controls[1].is_finite(),
                    "{controls:?}"
                );
                assert!(controls[2] >= 40.0 && controls[3] >= 10.0);
                for kind in [
                    NSWindowButton::CloseButton,
                    NSWindowButton::MiniaturizeButton,
                    NSWindowButton::ZoomButton,
                ] {
                    native.standardWindowButton(kind).unwrap().setHidden(true);
                }
                assert!(Platform::window_controls_rect(&window).is_none());
                window.set_resizable(false);
                check_window(&window, panel);
                window.set_decorations(false);
                window.set_resizable(true);
                let _ = window.is_maximized();
                window.set_ime_allowed(true);
                window.set_ime_cursor_area(
                    LogicalPosition::new(16.0, 20.0),
                    LogicalSize::new(8.0, 16.0),
                );
                let final_native = check_window(&window, panel);
                assert_eq!(window.id(), id);
                assert!(std::ptr::eq(&*native, &*final_native));
                assert!(std::ptr::eq(&*view, &*final_native.contentView().unwrap()));

                // Closing Winit's owner must not over-release a still-retained
                // native window, nor leave the view attached to a replacement.
                drop(window);
                assert!(!native.isVisible());
                assert!(!native.isReleasedWhenClosed());
                assert!(std::ptr::eq(&*native, &*view.window().unwrap()));
            }
            power.lock().unwrap().clear();
            drop(platform);
            // SAFETY: test token removal in the same isolated center after teardown.
            unsafe {
                center.postNotificationName_object(NSWorkspaceScreensDidWakeNotification, None)
            };
            assert!(power.lock().unwrap().is_empty());
            self.0 = true;
            event_loop.exit();
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }

    fn check_stream_deck_menu(
        platform: &mut Platform,
        config: &mut Config,
        actions: &Mutex<Vec<Action>>,
    ) {
        let root = NSApplication::sharedApplication(MainThreadMarker::new().unwrap())
            .mainMenu()
            .unwrap();
        let title = NSString::from_str("Stream Deck");
        let original_count = root.numberOfItems();
        assert!(root.itemWithTitle(&title).is_none());
        platform.set_stream_deck_connected(false).unwrap();
        config.keybinds = vec![KeyBinding::parse("cmd+shift+p=stream_deck:pull_to_first").unwrap()];
        platform.update_config(config).unwrap();
        platform.set_stream_deck_connected(true).unwrap();
        platform.set_stream_deck_connected(true).unwrap();
        assert_eq!(root.numberOfItems(), original_count + 1);
        assert_eq!(
            root.itemAtIndex(4).unwrap().title().to_string(),
            "Stream Deck"
        );
        let deck = root.itemWithTitle(&title).unwrap().submenu().unwrap();
        let expected = [
            ("Next Needing Input", StreamDeckAction::NeedsInput),
            ("Next Working", StreamDeckAction::Working),
            ("Next Done", StreamDeckAction::Done),
            ("Next Idle", StreamDeckAction::Idle),
            ("Next Page", StreamDeckAction::NextPage),
            ("Cycle Brightness", StreamDeckAction::Brightness),
            ("Pull to First Page", StreamDeckAction::PullToFirst),
        ];
        assert_eq!(deck.numberOfItems(), expected.len() as isize);
        for (index, (name, _)) in expected.iter().enumerate() {
            assert_eq!(
                deck.itemAtIndex(index as isize)
                    .unwrap()
                    .title()
                    .to_string(),
                *name
            );
            deck.performActionForItemAtIndex(index as isize);
        }
        assert_eq!(
            *actions.lock().unwrap(),
            expected.map(|(_, action)| Action::StreamDeck(action))
        );
        actions.lock().unwrap().clear();
        let pull = deck.itemAtIndex(6).unwrap();
        assert_eq!(pull.keyEquivalent().to_string(), "p");
        assert_eq!(
            pull.keyEquivalentModifierMask(),
            NSEventModifierFlags::Command | NSEventModifierFlags::Shift
        );

        config.keybinds.clear();
        platform.update_config(config).unwrap();
        assert!(
            pull.keyEquivalent().is_empty(),
            "reload must remove live shortcuts"
        );
        config.keybinds = vec![KeyBinding::parse("cmd+shift+p=stream_deck:pull_to_first").unwrap()];
        platform.update_config(config).unwrap();
        assert_eq!(pull.keyEquivalent().to_string(), "p");
        platform.set_stream_deck_connected(false).unwrap();
        platform.set_stream_deck_connected(false).unwrap();
        assert_eq!(root.numberOfItems(), original_count);
        config.keybinds.clear();
        platform.update_config(config).unwrap();
        platform.set_stream_deck_connected(true).unwrap();
        let deck = root.itemWithTitle(&title).unwrap().submenu().unwrap();
        assert!(deck.itemAtIndex(6).unwrap().keyEquivalent().is_empty());
        deck.performActionForItemAtIndex(6);
        assert_eq!(
            *actions.lock().unwrap(),
            [Action::StreamDeck(StreamDeckAction::PullToFirst)]
        );
        platform.set_stream_deck_connected(false).unwrap();
        assert_eq!(root.numberOfItems(), original_count);

        let app = root.itemAtIndex(0).unwrap().submenu().unwrap();
        let hide = app
            .itemArray()
            .into_iter()
            .find(|item| item.action() == Some(sel!(hide:)))
            .unwrap();
        assert_eq!(
            hide.keyEquivalent().to_string(),
            "h",
            "predefined shortcuts stay intact"
        );
    }

    fn check_window(window: &Window, panel: bool) -> Retained<NSWindow> {
        let handle = window.window_handle().unwrap();
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            panic!("expected an AppKit handle");
        };
        // Winit owns this NSView for the lifetime of the borrowed window handle.
        let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
        let native = view
            .window()
            .expect("Winit's view must remain attached to its original owner");
        assert_eq!(native.isKindOfClass(NSPanel::class()), panel);
        assert!(native.canBecomeKeyWindow());
        assert!(native.canBecomeMainWindow());
        assert!(!native.isReleasedWhenClosed());
        assert!(!native.isVisible());
        assert_eq!(
            native
                .styleMask()
                .contains(NSWindowStyleMask::NonactivatingPanel),
            panel,
        );
        assert!(std::ptr::eq(view, &*native.contentView().unwrap()));
        assert_eq!(
            u64::from(window.id()),
            Retained::as_ptr(&native) as usize as u64
        );
        native
    }
}
