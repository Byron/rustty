//! Run explicitly on a macOS desktop:
//! `cargo test -p rustty-app --test native_drag_drop --offline -- --ignored`
//!
//! Exercise native drag callbacks with a hidden window and a private pasteboard.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !std::env::args().any(|arg| arg == "--ignored") {
        println!("native_drag_drop: ignored (pass --ignored to run on the macOS desktop)");
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    macos::run()?;
    #[cfg(not(target_os = "macos"))]
    println!("native_drag_drop: macOS only");
    Ok(())
}

#[cfg(target_os = "macos")]
#[allow(deprecated)] // Cover Winit's existing legacy Finder format.
mod macos {
    use std::sync::{Arc, Mutex};

    use objc2::{
        DeclaredClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
    };
    use objc2_app_kit::{
        NSDragOperation, NSFilenamesPboardType, NSPasteboard, NSPasteboardTypeString, NSView,
        NSWindow,
    };
    use objc2_foundation::{NSArray, NSData, NSObject, NSObjectProtocol, NSPoint, NSString};
    use winit::{
        application::ApplicationHandler,
        dpi::{LogicalPosition, LogicalSize},
        event::WindowEvent,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::{
            macos::{ActivationPolicy, EventLoopBuilderExtMacOS, WindowExtMacOS},
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
        assert!(check.finished, "native drag check did not complete");
        println!("native_drag_drop: emoji, text, file priority and drop coordinates passed");
        Ok(())
    }

    #[derive(Default)]
    struct Check {
        window: Option<Window>,
        text: Arc<Mutex<Vec<(String, LogicalPosition<f64>)>>>,
        files: Vec<WindowEvent>,
        finished: bool,
    }

    impl ApplicationHandler for Check {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let window = event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Rustty native drag check")
                        .with_inner_size(LogicalSize::new(320.0, 180.0))
                        .with_visible(false)
                        .with_active(false),
                )
                .unwrap();
            let handle = window.window_handle().unwrap();
            let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
                unreachable!()
            };
            // Winit owns this view for the lifetime of the borrowed window.
            let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
            let native = view.window().unwrap();
            let board = PrivatePasteboard(NSPasteboard::pasteboardWithUniqueName());
            let sender = DragInfo::new(
                board.0.clone(),
                view.convertPoint_toView(NSPoint::new(17.0, 31.0), None),
            );

            assert!(
                board
                    .0
                    .setString_forType(&NSString::from_str("🦀"), unsafe {
                        NSPasteboardTypeString
                    })
            );
            assert_eq!(entered(&native, &sender), NSDragOperation::None);
            assert!(!perform(&native, &sender));

            let text = self.text.clone();
            window.set_text_drop_handler(Arc::new(move |value, position| {
                text.lock().unwrap().push((value, position));
            }));
            for value in ["🦀", "👩🏽‍💻", "first\nsecond"] {
                board.0.clearContents();
                assert!(
                    board
                        .0
                        .setString_forType(&NSString::from_str(value), unsafe {
                            NSPasteboardTypeString
                        })
                );
                assert_eq!(entered(&native, &sender), NSDragOperation::Copy);
                assert!(perform(&native, &sender));
            }

            board.0.clearContents();
            assert!(board.0.setData_forType(
                Some(&NSData::with_bytes(b"unsupported")),
                &NSString::from_str("app.rustty.native-drag-check"),
            ));
            assert_eq!(entered(&native, &sender), NSDragOperation::None);
            assert!(!perform(&native, &sender));

            board.0.clearContents();
            let files = NSArray::from_retained_slice(&[
                NSString::from_str("/tmp/rustty's dropped file.txt"),
                NSString::from_str("/tmp/second file.txt"),
            ]);
            // The private board owns an ordinary property-list array of file names.
            assert!(unsafe {
                board
                    .0
                    .setPropertyList_forType(&files, NSFilenamesPboardType)
            });
            assert!(
                board
                    .0
                    .setString_forType(&NSString::from_str("file text fallback"), unsafe {
                        NSPasteboardTypeString
                    })
            );
            assert_eq!(entered(&native, &sender), NSDragOperation::Copy);
            assert!(perform(&native, &sender));
            self.window = Some(window);
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            match event {
                WindowEvent::HoveredFile(_) | WindowEvent::DroppedFile(_) => self.files.push(event),
                WindowEvent::Ime(_) => panic!("drops must remain separate from typed input"),
                _ => {}
            }
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            assert_eq!(
                *self.text.lock().unwrap(),
                ["🦀", "👩🏽‍💻", "first\nsecond"]
                    .map(|text| (text.to_owned(), LogicalPosition::new(17.0, 31.0))),
            );
            assert_eq!(
                self.files,
                [
                    WindowEvent::HoveredFile("/tmp/rustty's dropped file.txt".into()),
                    WindowEvent::HoveredFile("/tmp/second file.txt".into()),
                    WindowEvent::DroppedFile("/tmp/rustty's dropped file.txt".into()),
                    WindowEvent::DroppedFile("/tmp/second file.txt".into()),
                ],
            );
            self.finished = true;
            event_loop.exit();
        }
    }

    fn entered(window: &NSWindow, sender: &DragInfo) -> NSDragOperation {
        // NSWindow forwards these native drag callbacks to Winit's delegate.
        unsafe { msg_send![window, draggingEntered: sender] }
    }

    fn perform(window: &NSWindow, sender: &DragInfo) -> bool {
        unsafe { msg_send![window, performDragOperation: sender] }
    }

    struct PrivatePasteboard(Retained<NSPasteboard>);

    impl Drop for PrivatePasteboard {
        fn drop(&mut self) {
            // Release only this test's unique pasteboard, never the user's clipboard.
            let _: () = unsafe { msg_send![&*self.0, releaseGlobally] };
        }
    }

    struct DragState {
        board: Retained<NSPasteboard>,
        point: NSPoint,
    }

    define_class!(
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = DragState]
        struct DragInfo;
        unsafe impl NSObjectProtocol for DragInfo {}
        impl DragInfo {
            #[unsafe(method(draggingPasteboard))]
            fn pasteboard(&self) -> &NSPasteboard {
                &self.ivars().board
            }

            #[unsafe(method(draggingLocation))]
            fn location(&self) -> NSPoint {
                self.ivars().point
            }
        }
    );

    impl DragInfo {
        fn new(board: Retained<NSPasteboard>, point: NSPoint) -> Retained<Self> {
            let this =
                Self::alloc(MainThreadMarker::new().unwrap()).set_ivars(DragState { board, point });
            unsafe { msg_send![super(this), init] }
        }
    }
}
