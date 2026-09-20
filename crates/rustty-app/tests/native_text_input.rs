//! Run explicitly on a macOS desktop:
//! `cargo test -p rustty-app --test native_text_input --offline -- --ignored`
//!
//! Exercise the real NSTextInputClient on the main thread, without showing a
//! window or posting keyboard events to other applications.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !std::env::args().any(|arg| arg == "--ignored") {
        println!("native_text_input: ignored (pass --ignored to run on the macOS desktop)");
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    macos::run()?;
    #[cfg(not(target_os = "macos"))]
    println!("native_text_input: macOS only");
    Ok(())
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::msg_send;
    use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType, NSView};
    use objc2_foundation::{NSAttributedString, NSNotFound, NSObject, NSPoint, NSRange, NSString};
    use winit::{
        application::ApplicationHandler,
        event::{ElementState, Ime, WindowEvent},
        event_loop::{ActiveEventLoop, EventLoop},
        keyboard::{KeyCode, PhysicalKey},
        platform::{
            macos::{ActivationPolicy, EventLoopBuilderExtMacOS},
            run_on_demand::EventLoopExtRunOnDemand,
        },
        raw_window_handle::{HasWindowHandle, RawWindowHandle},
        window::{Window, WindowId},
    };

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let mut event_loop = EventLoop::builder()
            .with_activation_policy(ActivationPolicy::Accessory)
            .with_activate_ignoring_other_apps(false)
            .with_default_menu(false)
            .build()?;
        let mut check = Check::default();
        event_loop.run_app_on_demand(&mut check)?;
        assert!(check.finished, "native text input check did not complete");
        println!(
            "native_text_input: picker commits, composition cleanup and subsequent typing passed"
        );
        Ok(())
    }

    #[derive(Default)]
    struct Check {
        window: Option<Window>,
        ime: Vec<Ime>,
        keys: Vec<(ElementState, PhysicalKey)>,
        finished: bool,
    }

    impl ApplicationHandler for Check {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let window = event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Rustty native text input check")
                        .with_visible(false)
                        .with_active(false),
                )
                .unwrap();
            let handle = window.window_handle().unwrap();
            let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
                unreachable!()
            };
            // Winit owns this NSView for the lifetime of the borrowed window.
            let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
            window.set_ime_allowed(true);

            // The Fn picker can insert plain or attributed text without first
            // starting composition, and without a surrounding keyDown call.
            insert(view, &NSString::from_str("🦀"));
            type_a(view);
            insert(
                view,
                &NSAttributedString::from_nsstring(&NSString::from_str("👩🏽‍💻")),
            );
            type_a(view);

            // An out-of-band commit also ends any existing marked text.
            let marked = NSString::from_str("候補");
            let _: () = unsafe {
                msg_send![view, setMarkedText: &*marked,
                    selectedRange: NSRange::new(2, 0),
                    replacementRange: NSRange::new(NSNotFound as usize, 0)]
            };
            insert(view, &NSString::from_str("字"));
            type_a(view);

            window.set_ime_allowed(false);
            insert(view, &NSString::from_str("ignored"));
            type_a(view);
            window.set_ime_allowed(true);
            insert(view, &NSString::from_str("✓"));
            type_a(view);
            insert(view, &NSString::from_str(""));
            insert(view, &NSString::from_str("\r"));

            self.window = Some(window);
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            match event {
                WindowEvent::Ime(ime) => self.ime.push(ime),
                WindowEvent::KeyboardInput { event, .. } => {
                    self.keys.push((event.state, event.physical_key));
                }
                _ => {}
            }
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            assert_eq!(
                self.ime,
                [
                    Ime::Enabled,
                    Ime::Preedit(String::new(), None),
                    Ime::Commit("🦀".into()),
                    Ime::Preedit(String::new(), None),
                    Ime::Commit("👩🏽‍💻".into()),
                    Ime::Preedit("候補".into(), Some((6, 6))),
                    Ime::Preedit(String::new(), None),
                    Ime::Commit("字".into()),
                    Ime::Disabled,
                    Ime::Enabled,
                    Ime::Preedit(String::new(), None),
                    Ime::Commit("✓".into()),
                ],
            );
            assert_eq!(
                self.keys,
                [
                    (ElementState::Pressed, PhysicalKey::Code(KeyCode::KeyA)),
                    (ElementState::Released, PhysicalKey::Code(KeyCode::KeyA)),
                ]
                .repeat(5),
                "commits must neither duplicate typing nor swallow the next key",
            );
            self.finished = true;
            event_loop.exit();
        }
    }

    fn insert(view: &NSView, text: &NSObject) {
        // Winit's NSView implements NSTextInputClient; both NSString and
        // NSAttributedString are valid arguments to this native callback.
        let _: () = unsafe {
            msg_send![view, insertText: text,
                replacementRange: NSRange::new(NSNotFound as usize, 0)]
        };
    }

    fn type_a(view: &NSView) {
        let text = NSString::from_str("a");
        for kind in [NSEventType::KeyDown, NSEventType::KeyUp] {
            let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                kind,
                NSPoint::ZERO,
                NSEventModifierFlags::empty(),
                0.0,
                view.window().unwrap().windowNumber(),
                None,
                &text,
                &text,
                false,
                0, // ANSI A
            )
            .unwrap();
            if kind == NSEventType::KeyDown {
                view.keyDown(&event);
            } else {
                view.keyUp(&event);
            }
        }
    }
}
