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
    use std::cell::Cell;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use objc2::{
        DeclaredClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
        runtime::ProtocolObject,
    };
    use objc2_app_kit::{
        NSDragOperation, NSFilenamesPboardType, NSPasteboard, NSPasteboardItem,
        NSPasteboardItemDataProvider, NSPasteboardTypeString, NSView, NSWindow,
    };
    use objc2_foundation::{NSArray, NSData, NSObject, NSObjectProtocol, NSPoint, NSString};
    use rustty_app::platform::{DragEvent, DragResult, MAX_DRAG_BYTES, Platform};
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

    type TextDrop = (String, LogicalPosition<f64>);

    #[derive(Default)]
    struct Check {
        window: Option<Window>,
        text: Arc<Mutex<Vec<TextDrop>>>,
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
            check_protocol(&window, &native, view);
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

    fn updated(window: &NSWindow, sender: &DragInfo) -> NSDragOperation {
        unsafe { msg_send![window, draggingUpdated: sender] }
    }

    fn exited(window: &NSWindow, sender: &DragInfo) {
        let _: () = unsafe { msg_send![window, draggingExited: sender] };
    }

    fn check_protocol(window: &Window, native: &NSWindow, view: &NSView) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let policy = Arc::new(AtomicUsize::new(0));
        let policy_read = policy.clone();
        let received = events.clone();
        let offered = Mutex::new(Vec::<Vec<u8>>::new());
        Platform::install_drag_handler(
            window,
            Arc::new(move |event| {
                if let DragEvent::Enter { mimes, .. } = &event {
                    *offered.lock().unwrap() = mimes.clone();
                }
                received.lock().unwrap().push(event);
                match policy_read.load(Ordering::Relaxed) {
                    1 => DragResult::Copy(offered.lock().unwrap().clone()),
                    2 => DragResult::Default,
                    3 => DragResult::Copy(vec![b"text/plain".to_vec()]),
                    _ => DragResult::Reject,
                }
            }),
        );
        Platform::update_drag_registration(window, &[b"application/x-rustty-native-drag".to_vec()]);
        // Reinstalling the existing text fallback must not erase native custom registrations.
        let fallback = Arc::new(AtomicUsize::new(0));
        let fallback_received = fallback.clone();
        window.set_text_drop_handler(Arc::new(move |_, _| {
            fallback_received.fetch_add(1, Ordering::Relaxed);
        }));
        let board = PrivatePasteboard(NSPasteboard::pasteboardWithUniqueName());
        let sender = DragInfo::new(
            board.0.clone(),
            view.convertPoint_toView(NSPoint::new(17.0, 31.0), None),
        );
        let kind = native_type("application/x-rustty-native-drag");
        let reads = Arc::new(AtomicUsize::new(0));
        let provider = Provider::new(reads.clone(), Some(b"private\0payload".to_vec()));
        let item = NSPasteboardItem::new();
        assert!(item.setDataProvider_forTypes(
            ProtocolObject::from_ref(&*provider),
            &NSArray::from_slice(&[&*kind])
        ));
        assert!(
            board
                .0
                .writeObjects(&NSArray::from_slice(&[ProtocolObject::from_ref(&*item)]))
        );
        assert_eq!(entered(native, &sender), NSDragOperation::None);
        assert_eq!(updated(native, &sender), NSDragOperation::None);
        assert_eq!(
            reads.load(Ordering::Relaxed),
            0,
            "hover must not request payload bytes"
        );
        policy.store(1, Ordering::Relaxed);
        assert_eq!(updated(native, &sender), NSDragOperation::Copy);
        assert_eq!(reads.load(Ordering::Relaxed), 0);
        let final_sender = DragInfo::new(
            board.0.clone(),
            view.convertPoint_toView(NSPoint::new(25.0, 42.0), None),
        );
        assert!(perform(native, &final_sender));
        assert_eq!(reads.load(Ordering::Relaxed), 1);
        {
            let events = events.lock().unwrap();
            assert!(
                matches!(&events[0], DragEvent::Enter { serial: 1, position: [17.0, 31.0], mimes, .. } if mimes == &[b"application/x-rustty-native-drag".to_vec()])
            );
            assert!(matches!(
                &events[events.len() - 2],
                DragEvent::Update {
                    position: [25.0, 42.0],
                    ..
                }
            ));
            assert!(
                matches!(&events[events.len()-1], DragEvent::Drop { serial: 1, position: [25.0, 42.0], items, .. } if items.len() == 1 && items[0].data == b"private\0payload")
            );
        }

        // A final-position rejection never asks the provider or falls back to paste.
        board.0.clearContents();
        let provider = Provider::new(reads.clone(), Some(b"blocked".to_vec()));
        let item = NSPasteboardItem::new();
        assert!(item.setDataProvider_forTypes(
            ProtocolObject::from_ref(&*provider),
            &NSArray::from_slice(&[&*kind])
        ));
        assert!(
            board
                .0
                .writeObjects(&NSArray::from_slice(&[ProtocolObject::from_ref(&*item)]))
        );
        assert_eq!(entered(native, &sender), NSDragOperation::Copy);
        policy.store(0, Ordering::Relaxed);
        assert!(!perform(native, &sender));
        assert_eq!(reads.load(Ordering::Relaxed), 1);
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(DragEvent::Leave { serial: 2 })
        ));

        // Provider failure and aggregate overflow reject the complete drop.
        policy.store(1, Ordering::Relaxed);
        board.0.clearContents();
        let provider = Provider::new(reads.clone(), None);
        let item = NSPasteboardItem::new();
        assert!(item.setDataProvider_forTypes(
            ProtocolObject::from_ref(&*provider),
            &NSArray::from_slice(&[&*kind])
        ));
        assert!(
            board
                .0
                .writeObjects(&NSArray::from_slice(&[ProtocolObject::from_ref(&*item)]))
        );
        assert_eq!(entered(native, &sender), NSDragOperation::Copy);
        assert!(!perform(native, &sender));
        board.0.clearContents();
        assert!(board.0.setData_forType(
            Some(&NSData::with_bytes(&vec![1; MAX_DRAG_BYTES / 2 + 1])),
            &kind
        ));
        assert!(board.0.setData_forType(
            Some(&NSData::with_bytes(&vec![2; MAX_DRAG_BYTES / 2 + 1])),
            &native_type("application/x-rustty-second")
        ));
        assert_eq!(entered(native, &sender), NSDragOperation::Copy);
        assert!(!perform(native, &sender));

        // Empty MIME registration still uses the installed protocol handler.
        Platform::update_drag_registration(window, &[]);
        board.0.clearContents();
        assert!(
            board
                .0
                .setString_forType(&NSString::from_str("🦀\ntext"), unsafe {
                    NSPasteboardTypeString
                })
        );
        assert_eq!(entered(native, &sender), NSDragOperation::Copy);
        assert!(perform(native, &sender));
        assert!(
            matches!(events.lock().unwrap().last(), Some(DragEvent::Drop { items, .. }) if items[0].mime == b"text/plain" && items[0].data == "🦀\ntext".as_bytes())
        );

        // Plain text can arrive as UTF-16; the protocol always exposes UTF-8 bytes.
        for kind in [
            "public.utf16-plain-text",
            "public.utf16-external-plain-text",
        ] {
            board.0.clearContents();
            let utf16: Vec<u8> = [0xff, 0xfe]
                .into_iter()
                .chain("🦀\nUTF-16".encode_utf16().flat_map(u16::to_le_bytes))
                .collect();
            assert!(
                board
                    .0
                    .setData_forType(Some(&NSData::with_bytes(&utf16)), &NSString::from_str(kind))
            );
            assert_eq!(entered(native, &sender), NSDragOperation::Copy);
            assert!(perform(native, &sender));
            assert!(
                matches!(events.lock().unwrap().last(), Some(DragEvent::Drop { items, .. }) if items[0].mime == b"text/plain" && items[0].data == "🦀\nUTF-16".as_bytes()),
                "{kind}: {:?}",
                events.lock().unwrap().last()
            );
        }

        // Unaccepted providers are never read, even if they would fail or exceed the limit.
        policy.store(3, Ordering::Relaxed);
        for data in [None, Some(vec![9; MAX_DRAG_BYTES + 1])] {
            board.0.clearContents();
            let image_reads = Arc::new(AtomicUsize::new(0));
            let provider = Provider::new(image_reads.clone(), data);
            let item = NSPasteboardItem::new();
            assert!(item.setDataProvider_forTypes(
                ProtocolObject::from_ref(&*provider),
                &NSArray::from_slice(&[&*NSString::from_str("public.png")])
            ));
            assert!(
                item.setString_forType(&NSString::from_str("accepted text"), unsafe {
                    NSPasteboardTypeString
                })
            );
            assert!(
                board
                    .0
                    .writeObjects(&NSArray::from_slice(&[ProtocolObject::from_ref(&*item)]))
            );
            assert_eq!(entered(native, &sender), NSDragOperation::Copy);
            assert!(perform(native, &sender));
            assert_eq!(image_reads.load(Ordering::Relaxed), 0);
            assert!(
                matches!(events.lock().unwrap().last(), Some(DragEvent::Drop { items, .. }) if items.len() == 1 && items[0].data == b"accepted text")
            );
        }
        policy.store(1, Ordering::Relaxed);
        board.0.clearContents();
        let paths = NSArray::from_retained_slice(&[
            NSString::from_str("/tmp/a space.txt"),
            NSString::from_str("/tmp/🦀.txt"),
        ]);
        assert!(unsafe {
            board
                .0
                .setPropertyList_forType(&paths, NSFilenamesPboardType)
        });
        assert_eq!(entered(native, &sender), NSDragOperation::Copy);
        assert!(perform(native, &sender));
        assert!(
            matches!(events.lock().unwrap().last(), Some(DragEvent::Drop { items, .. }) if items.len() == 1 && items[0].mime == b"text/uri-list" && String::from_utf8_lossy(&items[0].data).contains("a%20space.txt\r\n"))
        );

        // Changed sources and source operations cannot reuse earlier acceptance.
        assert_eq!(entered(native, &sender), NSDragOperation::Copy);
        board.0.clearContents();
        assert!(!perform(native, &sender));
        assert_eq!(entered(native, &sender), NSDragOperation::Copy);
        exited(native, &sender);
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(DragEvent::Leave { .. })
        ));
        sender.ivars().operations.set(NSDragOperation::Move);
        assert_eq!(entered(native, &sender), NSDragOperation::None);
        assert!(!perform(native, &sender));
        assert_eq!(
            fallback.load(Ordering::Relaxed),
            0,
            "registered failures must never paste"
        );
        sender.ivars().operations.set(NSDragOperation::Copy);
        board.0.clearContents();
        for index in 0..17 {
            assert!(board.0.setData_forType(
                Some(&NSData::with_bytes(b"x")),
                &native_type(&format!("application/x-rustty-{index}"))
            ));
        }
        assert_eq!(entered(native, &sender), NSDragOperation::Copy);
        assert!(
            matches!(events.lock().unwrap().last(), Some(DragEvent::Enter { mimes, .. }) if mimes.len() == 16)
        );
        exited(native, &sender);
        policy.store(2, Ordering::Relaxed);
        board.0.clearContents();
        assert!(
            board
                .0
                .setString_forType(&NSString::from_str("ordinary paste"), unsafe {
                    NSPasteboardTypeString
                })
        );
        assert_eq!(entered(native, &sender), NSDragOperation::Copy);
        assert!(perform(native, &sender));
        assert_eq!(fallback.load(Ordering::Relaxed), 1);
        println!(
            "native_drag_drop: OSC72 offers, delayed acceptance, drop preflight, private bytes, limits and lifecycle passed"
        );
    }

    struct ProviderState {
        reads: Arc<AtomicUsize>,
        bytes: Option<Vec<u8>>,
    }

    fn native_type(mime: &str) -> Retained<NSString> {
        let kind: Retained<NSObject> = unsafe {
            msg_send![objc2::class!(UTType), typeWithMIMEType: &*NSString::from_str(mime)]
        };
        unsafe { msg_send![&*kind, identifier] }
    }
    define_class!(
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = ProviderState]
        struct Provider;
        unsafe impl NSObjectProtocol for Provider {}
        unsafe impl NSPasteboardItemDataProvider for Provider {
            #[unsafe(method(pasteboard:item:provideDataForType:))]
            fn provide(&self, _: Option<&NSPasteboard>, item: &NSPasteboardItem, kind: &NSString) {
                self.ivars().reads.fetch_add(1, Ordering::Relaxed);
                if let Some(bytes) = &self.ivars().bytes {
                    assert!(item.setData_forType(&NSData::with_bytes(bytes), kind));
                }
            }
        }
    );
    impl Provider {
        fn new(reads: Arc<AtomicUsize>, bytes: Option<Vec<u8>>) -> Retained<Self> {
            let this = Self::alloc(MainThreadMarker::new().unwrap())
                .set_ivars(ProviderState { reads, bytes });
            unsafe { msg_send![super(this), init] }
        }
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
        operations: Cell<NSDragOperation>,
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

            #[unsafe(method(draggingSourceOperationMask))]
            fn operations(&self) -> NSDragOperation { self.ivars().operations.get() }
        }
    );

    impl DragInfo {
        fn new(board: Retained<NSPasteboard>, point: NSPoint) -> Retained<Self> {
            let this = Self::alloc(MainThreadMarker::new().unwrap()).set_ivars(DragState {
                board,
                point,
                operations: Cell::new(NSDragOperation::Copy),
            });
            unsafe { msg_send![super(this), init] }
        }
    }
}
