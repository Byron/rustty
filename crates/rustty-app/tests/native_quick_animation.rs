//! Explicit macOS desktop test; briefly shows a test-owned quick panel:
//! `cargo test -p rustty-app --test native_quick_animation --offline -- --ignored`

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !std::env::args().any(|arg| arg == "--ignored") {
        println!("native_quick_animation: ignored (pass --ignored to run on the macOS desktop)");
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    macos::run()?;
    #[cfg(not(target_os = "macos"))]
    println!("native_quick_animation: macOS only");
    Ok(())
}

#[cfg(target_os = "macos")]
mod macos {
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    use objc2::{MainThreadMarker, rc::Retained};
    use objc2_app_kit::{NSFloatingWindowLevel, NSScreen, NSView, NSWindow};
    use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID};
    use rustty::config::{Config, QuickTerminalPosition};
    use rustty_app::platform::Platform;
    use winit::{
        application::ApplicationHandler,
        dpi::LogicalSize,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
        platform::{
            macos::{ActivationPolicy, EventLoopBuilderExtMacOS, WindowAttributesExtMacOS},
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
        let mut check = Check {
            platform: None,
            window: None,
            native: None,
            config: Config::default(),
            frame: [0.0; 4],
            stage: 0,
            next: Instant::now(),
            deadline: Instant::now() + Duration::from_secs(15),
            finished: false,
        };
        check.config.keybinds.clear();
        event_loop.run_app_on_demand(&mut check)?;
        assert!(check.finished, "native animation test did not complete");
        println!(
            "native_quick_animation: reversal, final frame, zero-duration cancellation and close passed"
        );
        Ok(())
    }

    struct Check {
        platform: Option<Platform>,
        window: Option<Window>,
        native: Option<Retained<NSWindow>>,
        config: Config,
        frame: [f64; 4],
        stage: u8,
        next: Instant,
        deadline: Instant,
        finished: bool,
    }

    impl ApplicationHandler for Check {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            assert!(
                NSScreen::screens(MainThreadMarker::new().unwrap()).count() > 0,
                "native animation test requires a macOS desktop"
            );
            self.platform = Some(Platform::new(Arc::new(|_| {}), &self.config).unwrap());
            let window = event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Rustty quick animation check")
                        .with_nonactivating_panel(true)
                        .with_decorations(false)
                        .with_visible(false)
                        .with_active(false)
                        .with_inner_size(LogicalSize::new(640.0, 320.0)),
                )
                .unwrap();
            let handle = window.window_handle().unwrap();
            let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
                unreachable!()
            };
            // The borrowed handle owns this NSView; retain its native window for
            // assertions after closing the Winit owner at the end of the test.
            let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
            self.native = view.window();
            self.window = Some(window);
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            assert!(
                Instant::now() < self.deadline,
                "animation timed out at stage {}",
                self.stage
            );
            if Instant::now() < self.next {
                event_loop.set_control_flow(ControlFlow::WaitUntil(self.next));
                return;
            }
            let platform = self.platform.as_ref().unwrap();
            let native = self.native.as_ref().unwrap();
            eprintln!(
                "native_quick_animation stage {}: visible={}, key={}, active_space={}, level={}, alpha={}, frame={:?}",
                self.stage,
                native.isVisible(),
                native.isKeyWindow(),
                native.isOnActiveSpace(),
                native.level(),
                native.alphaValue(),
                native.frame()
            );
            if self.stage == 7 {
                assert!(
                    !native.isVisible(),
                    "a closed panel was shown by an old completion"
                );
                assert!(native.delegate().is_none());
                self.finished = true;
                event_loop.exit();
                return;
            }
            let window = self.window.as_ref().unwrap();
            let mut delay = Duration::from_millis(80);
            match self.stage {
                0 => {
                    assert!(
                        !platform.quick_resigned_focus(window, false).unwrap(),
                        "Winit's initial unfocused event must not hide a new panel"
                    );
                    self.config.quick_terminal_animation_duration = Duration::ZERO;
                    platform.show_quick(window, &self.config).unwrap();
                    self.frame = platform.quick_terminal_saved_frame(window).unwrap();
                    assert_settled(native, true, self.frame);
                    platform.hide_quick(window, false, &self.config).unwrap();
                    assert_settled(native, false, self.frame);
                    self.config.quick_terminal_animation_duration = Duration::from_millis(300);
                    platform.show_quick(window, &self.config).unwrap();
                    assert_frame(
                        platform.quick_terminal_saved_frame(window).unwrap(),
                        self.frame,
                    );
                }
                1 => {
                    platform.hide_quick(window, false, &self.config).unwrap();
                    assert_frame(
                        platform.quick_terminal_saved_frame(window).unwrap(),
                        self.frame,
                    );
                }
                2 => {
                    // Reveal while the hide is still in flight. Its old completion
                    // must not order out the panel after this reveal finishes.
                    platform.show_quick(window, &self.config).unwrap();
                    self.frame = platform.quick_terminal_saved_frame(window).unwrap();
                    delay = Duration::from_millis(650);
                }
                3 => {
                    assert_settled(native, true, self.frame);
                    let mut resized = native.frame();
                    resized.size.width = 520.0;
                    resized.size.height = 280.0;
                    native.setFrame_display(resized, false);
                    self.frame = platform.quick_terminal_saved_frame(window).unwrap();
                    assert_eq!(&self.frame[2..], &[520.0, 280.0]);
                    platform.hide_quick(window, false, &self.config).unwrap();
                    delay = Duration::from_millis(650);
                }
                4 => {
                    assert_settled(native, false, self.frame);
                    self.config.quick_terminal_position = QuickTerminalPosition::Center;
                    platform.show_quick(window, &self.config).unwrap();
                    self.frame = platform.quick_terminal_saved_frame(window).unwrap();
                    assert_eq!(&self.frame[2..], &[520.0, 280.0]);
                    // Cancel a live fade with a zero-duration hide, including its
                    // native property animation and its pending completion.
                    self.config.quick_terminal_animation_duration = Duration::ZERO;
                    platform.hide_quick(window, false, &self.config).unwrap();
                    assert_settled(native, false, self.frame);
                    delay = Duration::from_millis(650);
                }
                5 => {
                    assert_settled(native, false, self.frame);
                    self.config.quick_terminal_animation_duration = Duration::from_millis(300);
                    platform.show_quick(window, &self.config).unwrap();
                }
                6 => {
                    platform.forget_window(window);
                    drop(self.window.take());
                    delay = Duration::from_millis(650);
                }
                _ => unreachable!(),
            }
            self.stage += 1;
            self.next = Instant::now() + delay;
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.next));
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }

    fn assert_settled(native: &NSWindow, visible: bool, expected: [f64; 4]) {
        assert_eq!(native.isVisible(), visible);
        assert_eq!(native.level(), NSFloatingWindowLevel);
        assert!((native.alphaValue() - 1.0).abs() < 0.001);
        let frame = native.frame();
        assert_frame(
            [
                frame.origin.x,
                CGDisplayBounds(CGMainDisplayID()).size.height - frame.origin.y - frame.size.height,
                frame.size.width,
                frame.size.height,
            ],
            expected,
        );
    }

    fn assert_frame(actual: [f64; 4], expected: [f64; 4]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() <= 1.0,
                "frame component {actual} != {expected}"
            );
        }
    }
}
