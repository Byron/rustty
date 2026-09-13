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
    use objc2::{ClassType, MainThreadMarker, rc::Retained};
    use objc2_app_kit::{NSPanel, NSView, NSWindow, NSWindowStyleMask};
    use objc2_foundation::NSObjectProtocol;
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
        println!("native_panel: native classes, key/main eligibility, ownership and style passed");
        Ok(())
    }

    struct Check(bool);

    impl ApplicationHandler for Check {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            for panel in [false, true] {
                let window = event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_title("Rustty native panel check")
                            .with_visible(false)
                            .with_active(false)
                            .with_decorations(false)
                            .with_nonactivating_panel(panel)
                            .with_inner_size(LogicalSize::new(320.0, 180.0)),
                    )
                    .unwrap();
                let id = window.id();
                let native = check_window(&window, panel);
                let view = native.contentView().unwrap();

                // Exercise Winit's temporary zoom mask as well as ordinary edits.
                // The old path replaced the entire borderless style to query zoom.
                let _ = window.is_maximized();
                window.set_decorations(true);
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
            self.0 = true;
            event_loop.exit();
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
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
