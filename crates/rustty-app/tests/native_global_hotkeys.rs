//! Run explicitly on a macOS desktop:
//! `cargo test -p rustty-app --test native_global_hotkeys --offline -- --ignored`
//!
//! AppKit requires the process's main thread, so this target has no libtest harness.
//! Synthetic Carbon events exercise routing without posting keyboard input or
//! requiring Accessibility. Ordinary Cargo test runs skip native initialization.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !std::env::args().any(|arg| arg == "--ignored") {
        println!("native_global_hotkeys: ignored (pass --ignored on the macOS desktop)");
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    macos::run()?;
    Ok(())
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2_foundation::{NSDistributedNotificationCenter, NSString};
    use rustty::config::{Config, KeyBinding};
    use rustty_app::platform::{Platform, PlatformEvent};
    use std::{
        ffi::c_void,
        sync::Arc,
        time::{Duration, Instant},
    };
    use winit::{
        application::ApplicationHandler,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
        platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS},
        window::WindowId,
    };

    #[repr(C, packed(2))]
    struct HotkeyId {
        signature: u32,
        id: u32,
    }
    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn GetApplicationEventTarget() -> *mut c_void;
        fn RegisterEventHotKey(
            code: u32,
            modifiers: u32,
            id: HotkeyId,
            target: *mut c_void,
            options: u32,
            result: *mut *mut c_void,
        ) -> i32;
        fn UnregisterEventHotKey(hotkey: *mut c_void) -> i32;
        fn CreateEvent(
            allocator: *mut c_void,
            class: u32,
            kind: u32,
            time: f64,
            attributes: u32,
            event: *mut *mut c_void,
        ) -> i32;
        fn SetEventParameter(
            event: *mut c_void,
            name: u32,
            kind: u32,
            size: usize,
            data: *const c_void,
        ) -> i32;
        fn GetMainEventQueue() -> *mut c_void;
        fn PostEventToQueue(queue: *mut c_void, event: *mut c_void, priority: i16) -> i32;
        fn ReleaseEvent(event: *mut c_void);
        static kTISNotifySelectedKeyboardInputSourceChanged: *const NSString;
    }

    fn config(keys: &[&str]) -> Config {
        let mut config = Config::default();
        config.keybinds = keys
            .iter()
            .map(|key| {
                KeyBinding::parse(&format!(
                    "global:physical:ctrl+alt+super+shift+{key}=ignore"
                ))
                .unwrap()
            })
            .collect();
        config
    }

    fn register(code: u32) -> *mut c_void {
        let mut reference = std::ptr::null_mut();
        // SAFETY: run on the native main thread with a valid out-pointer.
        assert_eq!(
            unsafe {
                RegisterEventHotKey(
                    code,
                    256 | 512 | 2048 | 4096,
                    HotkeyId {
                        signature: u32::from_be_bytes(*b"Test"),
                        id: 1,
                    },
                    GetApplicationEventTarget(),
                    1,
                    &mut reference,
                )
            },
            0
        );
        reference
    }

    fn live_id(platform: &Platform, key: &str) -> Option<u32> {
        // This isolated process creates only a handful of registrations. Probe
        // their opaque IDs through the public lookup without assuming an exact ID.
        (1..64).find(|id| {
            platform
                .global_keybinding(*id)
                .is_some_and(|binding| binding.trigger[0].key == key)
        })
    }

    fn post_hotkey(id: u32) {
        let mut event = std::ptr::null_mut();
        let hotkey = HotkeyId {
            signature: u32::from_be_bytes(*b"Rsty"),
            id,
        };
        // SAFETY: Carbon validates the owned event and copies the parameter. The
        // queue retains the event until NSApplication processes it.
        unsafe {
            assert_eq!(
                CreateEvent(
                    std::ptr::null_mut(),
                    u32::from_be_bytes(*b"keyb"),
                    5,
                    0.0,
                    0,
                    &mut event
                ),
                0
            );
            assert_eq!(
                SetEventParameter(
                    event,
                    u32::from_be_bytes(*b"----"),
                    u32::from_be_bytes(*b"hkid"),
                    std::mem::size_of_val(&hotkey),
                    (&hotkey as *const HotkeyId).cast()
                ),
                0
            );
            assert_eq!(PostEventToQueue(GetMainEventQueue(), event, 1), 0);
            ReleaseEvent(event);
        }
    }

    fn notify_layout_change() {
        // SAFETY: the framework's constant is a valid NSString. This exercises
        // the distributed observer without changing the user's selected layout.
        unsafe {
            NSDistributedNotificationCenter::defaultCenter()
                .postNotificationName_object_userInfo_deliverImmediately(
                    &*kTISNotifySelectedKeyboardInputSourceChanged,
                    None,
                    None,
                    true,
                );
        }
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let event_loop = EventLoop::<PlatformEvent>::with_user_event()
            .with_activation_policy(ActivationPolicy::Accessory)
            .with_activate_ignoring_other_apps(false)
            .with_default_menu(false)
            .build()?;
        let mut check = Check {
            proxy: event_loop.create_proxy(),
            platform: None,
            stage: 0,
            stale_id: 0,
            warnings: 0,
            deadline: Instant::now() + Duration::from_secs(5),
            finish_at: None,
        };
        event_loop.run_app(&mut check)?;
        assert_eq!(check.stage, 4, "native hotkey check did not finish");
        println!(
            "native_global_hotkeys: conflict recovery, Carbon/Winit delivery, stale IDs, layout observer and cleanup passed"
        );
        Ok(())
    }

    struct Check {
        proxy: EventLoopProxy<PlatformEvent>,
        platform: Option<Platform>,
        stage: u8,
        stale_id: u32,
        warnings: usize,
        deadline: Instant,
        finish_at: Option<Instant>,
    }

    impl ApplicationHandler<PlatformEvent> for Check {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.platform.is_some() {
                return;
            }
            let conflict = register(0); // ANSI A
            let proxy = self.proxy.clone();
            let mut platform = Platform::new(
                Arc::new(move |event| {
                    let _ = proxy.send_event(event);
                }),
                &config(&["a", "b"]),
            )
            .unwrap();
            assert!(live_id(&platform, "a").is_none());
            let old = live_id(&platform, "b").expect("unaffected shortcut must register");
            assert_eq!(unsafe { UnregisterEventHotKey(conflict) }, 0);
            platform.update_config(&config(&["a", "b"])).unwrap();
            assert!(platform.global_keybinding(old).is_none());
            let live = live_id(&platform, "a").expect("unchanged reload must retry conflicts");
            self.platform = Some(platform);
            post_hotkey(old);
            post_hotkey(live);
            event_loop.set_control_flow(ControlFlow::Poll);
        }

        fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: PlatformEvent) {
            match event {
                PlatformEvent::Warning(warning) => {
                    assert!(warning.contains("global:physical:ctrl+alt+shift+super+a"));
                    self.warnings += 1;
                }
                PlatformEvent::GlobalHotkey { id, .. } => {
                    let platform = self.platform.as_mut().expect("callback after teardown");
                    let binding = platform.global_keybinding(id).expect("stale ID dispatched");
                    match self.stage {
                        0 => {
                            assert_eq!(binding.trigger[0].key, "a");
                            assert_eq!(self.warnings, 1);
                            platform.update_config(&config(&["c"])).unwrap();
                            assert!(platform.global_keybinding(id).is_none());
                            self.stage = 1;
                            post_hotkey(id);
                            post_hotkey(live_id(platform, "c").unwrap());
                        }
                        1 => {
                            assert_eq!(binding.trigger[0].key, "c");
                            self.stale_id = id;
                            self.stage = 2;
                            notify_layout_change();
                        }
                        3 => {
                            assert_ne!(id, self.stale_id);
                            platform.update_config(&config(&[])).unwrap();
                            assert!(platform.global_keybinding(id).is_none());
                            let freed = register(8); // ANSI C
                            assert_eq!(unsafe { UnregisterEventHotKey(freed) }, 0);
                            self.platform = None;
                            self.stage = 4;
                            post_hotkey(id);
                            notify_layout_change();
                            self.finish_at = Some(Instant::now() + Duration::from_millis(100));
                        }
                        _ => panic!("duplicate or stale callback"),
                    }
                }
                other => panic!("unexpected event {other:?}"),
            }
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            assert!(
                Instant::now() < self.deadline,
                "native hotkey check timed out"
            );
            if self.stage == 2 {
                let platform = self.platform.as_ref().unwrap();
                if platform.global_keybinding(self.stale_id).is_none() {
                    self.stage = 3;
                    post_hotkey(self.stale_id);
                    post_hotkey(live_id(platform, "c").unwrap());
                }
            }
            if self.finish_at.is_some_and(|end| Instant::now() >= end) {
                event_loop.exit();
            }
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }
}
