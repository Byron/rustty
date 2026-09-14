//! Explicit native Windows checks. Windows stay hidden unless --animate is supplied.
//! cargo test -p rustty-app --test windows_platform -- --ignored [--animate]
//! The user's system clipboard and notification center are never modified.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !std::env::args().any(|arg| arg == "--ignored") {
        println!("windows_platform: ignored (pass --ignored on a Windows desktop)");
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    windows::run()?;
    #[cfg(not(target_os = "windows"))]
    println!("windows_platform: Windows only");
    Ok(())
}

#[cfg(target_os = "windows")]
mod windows {
    use ::windows::Win32::{
        Foundation::*, System::DataExchange::GetClipboardSequenceNumber, UI::WindowsAndMessaging::*,
    };
    use rustty::{
        config::{Action, Config},
        vt::clipboard::{Content, Location, Read, ReadResult, Terminator, Write, WriteResult},
    };
    use rustty_app::platform::{Platform, PlatformEvent, configure_event_loop};
    use std::{
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };
    use winit::{
        application::ApplicationHandler,
        dpi::LogicalSize,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
        platform::{run_on_demand::EventLoopExtRunOnDemand, windows::WindowAttributesExtWindows},
        raw_window_handle::{HasWindowHandle, RawWindowHandle},
        window::{Window, WindowId},
    };
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let mut builder = EventLoop::<()>::builder();
        configure_event_loop(&mut builder);
        let mut event_loop = builder.build()?;
        let mut check = Check {
            platform: None,
            window: None,
            config: Config::default(),
            events: Arc::default(),
            animate: std::env::args().any(|arg| arg == "--animate"),
            stage: 0,
            next: Instant::now(),
            deadline: Instant::now() + Duration::from_secs(10),
            finished: false,
        };
        check.config.keybinds.clear();
        check.config.quick_terminal_animation_duration = Duration::from_millis(100);
        event_loop.run_app_on_demand(&mut check)?;
        assert!(check.finished, "native checks did not complete");
        println!(
            "windows_platform: native menu routing, clipboard isolation, RNG, geometry and lifecycle passed"
        );
        Ok(())
    }
    struct Check {
        platform: Option<Platform>,
        window: Option<Window>,
        config: Config,
        events: Arc<Mutex<Vec<PlatformEvent>>>,
        animate: bool,
        stage: u8,
        next: Instant,
        deadline: Instant,
        finished: bool,
    }
    impl ApplicationHandler for Check {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let events = self.events.clone();
            let platform = Platform::new(
                Arc::new(move |event| events.lock().unwrap().push(event)),
                &self.config,
            )
            .unwrap();
            let window = event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Rustty native checks")
                        .with_visible(false)
                        .with_active(false)
                        .with_skip_taskbar(true)
                        .with_inner_size(LogicalSize::new(640.0, 320.0)),
                )
                .unwrap();
            let client_size = window.inner_size();
            let position = window.outer_position().unwrap();
            platform
                .configure_window(&window, false, &self.config)
                .unwrap();
            assert_eq!(
                window.inner_size(),
                client_size,
                "attaching the menu changed the restored client size"
            );
            assert_eq!(window.outer_position().unwrap(), position);
            platform
                .configure_window(&window, false, &self.config)
                .unwrap();
            assert_eq!(
                window.inner_size(),
                client_size,
                "reconfiguring a window changed its client size"
            );
            let RawWindowHandle::Win32(raw) = window.window_handle().unwrap().as_raw() else {
                unreachable!()
            };
            let hwnd = HWND(raw.hwnd.get() as _);
            unsafe {
                let menu = GetMenu(hwnd);
                assert!(!menu.0.is_null());
                let file = GetSubMenu(menu, 0);
                let command = GetMenuItemID(file, 0);
                assert_ne!(command, u32::MAX);
                SendMessageW(
                    hwnd,
                    WM_COMMAND,
                    Some(WPARAM(command as usize)),
                    Some(LPARAM(0)),
                );
            }
            assert!(
                self.events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|event| matches!(event, PlatformEvent::Action(Action::NewWindow)))
            );
            assert!(
                Platform::window_diagnostics(&window)
                    .unwrap()
                    .contains("visible=false")
            );
            Platform::cursor_position(&window)
                .expect("read live cursor in hidden client coordinates");
            let mut random = [0u8; 32];
            Platform::secure_random(&mut random).unwrap();
            assert_ne!(random, [0; 32]);
            let before = unsafe { GetClipboardSequenceNumber() };
            let write = Write::osc52(
                Location::Selection,
                vec![Content {
                    mime: b"application/x-rustty-test".to_vec(),
                    data: Arc::from(&b"a\0b\xff"[..]),
                }],
            );
            assert!(matches!(
                platform.clipboard_write(&write),
                WriteResult::Success { .. }
            ));
            let mut read = Read::osc52(Location::Selection, Terminator::St);
            read.list = true;
            read.mimes = vec![b"application/x-rustty-test".to_vec()];
            let ReadResult::Success(result) = platform.clipboard_read(&read) else {
                panic!("selection read failed")
            };
            assert_eq!(&*result.contents[0].data, b"a\0b\xff");
            assert_eq!(result.available, [b"application/x-rustty-test".to_vec()]);
            assert_eq!(
                before,
                unsafe { GetClipboardSequenceNumber() },
                "selection modified the system clipboard"
            );
            assert!(matches!(
                platform.clipboard_read(&Read::osc52(Location::Primary, Terminator::St)),
                ReadResult::Unsupported
            ));
            platform.forget_window(&window);
            window.set_decorations(false);
            platform
                .configure_window(&window, true, &self.config)
                .unwrap();
            unsafe {
                assert_ne!(
                    GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & WS_EX_TOOLWINDOW.0 as isize,
                    0
                );
            }
            assert!(
                platform
                    .quick_terminal_frame(&self.config, Some([640.0, 320.0]))
                    .is_some()
            );
            self.platform = Some(platform);
            self.window = Some(window);
        }
        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if self.finished {
                event_loop.exit();
                return;
            }
            assert!(Instant::now() < self.deadline, "native animation timed out");
            let platform = self.platform.as_ref().unwrap();
            let window = self.window.as_ref().unwrap();
            platform.tick();
            if !self.animate {
                platform.forget_window(window);
                self.finished = true;
                event_loop.exit();
                return;
            }
            if Instant::now() >= self.next {
                match self.stage {
                    0 => platform.show_quick(window, &self.config).unwrap(),
                    1 => platform.hide_quick(window, true, &self.config).unwrap(),
                    2 => platform.show_quick(window, &self.config).unwrap(),
                    3 => {
                        self.config.quick_terminal_animation_duration = Duration::ZERO;
                        platform.hide_quick(window, true, &self.config).unwrap();
                        assert!(platform.next_deadline().is_none());
                    }
                    _ => {
                        platform.forget_window(window);
                        self.window.take();
                        self.finished = true;
                        event_loop.exit();
                        return;
                    }
                }
                self.stage += 1;
                self.next = Instant::now() + Duration::from_millis(35);
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                platform.next_deadline().unwrap_or(self.next).min(self.next),
            ));
        }
    }
}
