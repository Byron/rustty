//! Desktop routing uses cached feedback and sends payloads to one Session owner.
use super::*;
use rustty_app::platform::{DragEvent, DragResult};

fn route(app: &mut App, window: WindowId, event: DragEvent) -> Result<DragResult> {
    app.refresh_drag_views();
    let (reply, commands) = app.windows[&window]
        .drag
        .lock()
        .map_err(|_| "poisoned drag controller")?
        .handle(event);
    app.dispatch_drag(window, commands);
    Ok(reply)
}

fn enter(serial: u64, position: [f32; 2]) -> DragEvent {
    DragEvent::Enter {
        serial,
        position,
        source_ops: vt::dnd::Operations {
            copy: true,
            move_: true,
        },
        mimes: vec![b"text/plain".to_vec(), b"image/png".to_vec()],
    }
}

fn update(serial: u64, position: [f32; 2]) -> DragEvent {
    DragEvent::Update {
        serial,
        position,
        source_ops: vt::dnd::Operations {
            copy: true,
            move_: true,
        },
    }
}

pub(super) fn check(app: &mut App, event_loop: &ActiveEventLoop, directory: &Path) -> Result<()> {
    let window = *app.windows.keys().next().ok_or("no drag smoke window")?;
    let mut host = app.windows.remove(&window).unwrap();
    let first = app.focused(host.id).ok_or("no drag smoke pane")?;
    app.action(
        event_loop,
        &mut host,
        Action::NewSplit(Direction::Right),
        true,
    );
    let second = app.focused(host.id).ok_or("drag smoke split failed")?;
    assert_ne!(first, second);
    let mut saved = Vec::new();
    for (index, id) in [first, second].into_iter().enumerate() {
        let mut terminal = app.panes[&id].session.terminal()?;
        let mut fixture = vt::Terminal::new(terminal.cols, terminal.rows, 100);
        fixture.feed(format!("\x1b[1;36mOSC 72 drop target {}\x1b[0m\r\nDrop text or an image here.\r\n\x1b]72;t=a:i={};text/plain image/png\x1b\\", index + 1, index + 10).as_bytes());
        saved.push((id, std::mem::replace(&mut *terminal, fixture)));
    }
    host.focus_hint.dismiss();
    app.draw(event_loop, &mut host)?;
    host.focus_hint.dismiss();
    app.draw(event_loop, &mut host)?;
    let positions = [first, second].map(|id| host.rects[&id].center().into());
    capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("drag-targets.png"),
    )?;
    app.windows.insert(window, host);
    let result = (|| -> Result<()> {
        app.refresh_drag_views();
        assert_eq!(
            app.windows[&window].drag_mimes,
            [b"image/png".to_vec(), b"text/plain".to_vec()]
        );
        assert_eq!(
            route(app, window, enter(1, positions[0]))?,
            DragResult::Reject
        );
        let offer = app.panes[&first].session.drag_view()?.unwrap().offer;
        assert!(offer.is_some());
        assert!(
            app.panes[&second]
                .session
                .drag_view()?
                .unwrap()
                .offer
                .is_none()
        );

        app.panes[&first]
            .session
            .terminal()?
            .feed(b"\x1b]72;t=m:o=2;text/plain\x1b\\");
        assert_eq!(
            route(app, window, update(1, positions[0]))?,
            DragResult::Reject
        );
        app.panes[&first]
            .session
            .terminal()?
            .feed(b"\x1b]72;t=m:o=1;text/plain\x1b\\");
        assert_eq!(
            route(app, window, update(1, positions[0]))?,
            DragResult::Copy(vec![b"text/plain".to_vec()])
        );

        assert_eq!(
            route(app, window, update(1, positions[1]))?,
            DragResult::Reject
        );
        assert!(
            app.panes[&first]
                .session
                .drag_view()?
                .unwrap()
                .offer
                .is_none()
        );
        assert_ne!(
            app.panes[&second].session.drag_view()?.unwrap().offer,
            offer
        );
        app.panes[&second]
            .session
            .terminal()?
            .feed(b"\x1b]72;t=m:o=1;image/png\x1b\\");
        assert_eq!(
            route(app, window, update(1, positions[1]))?,
            DragResult::Copy(vec![b"image/png".to_vec()])
        );
        let drop_event = DragEvent::Drop {
            serial: 1,
            position: positions[1],
            source_ops: vt::dnd::Operations {
                copy: true,
                move_: true,
            },
            items: vec![
                vt::dnd::Item {
                    mime: b"text/plain".to_vec(),
                    data: b"not accepted".to_vec(),
                },
                vt::dnd::Item {
                    mime: b"image/png".to_vec(),
                    data: vec![0, 255, 1],
                },
            ],
        };
        assert_eq!(
            route(app, window, drop_event.clone())?,
            DragResult::Copy(vec![b"image/png".to_vec()])
        );
        assert_eq!(route(app, window, drop_event)?, DragResult::Reject);
        assert!(
            app.panes[&first]
                .session
                .terminal()?
                .kitty_dnd
                .as_ref()
                .unwrap()
                .drop
                .items
                .is_none()
        );
        {
            let terminal = app.panes[&second].session.terminal()?;
            let items = terminal
                .kitty_dnd
                .as_ref()
                .unwrap()
                .drop
                .items
                .as_ref()
                .unwrap();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].mime, b"image/png");
            assert_eq!(items[0].data, [0, 255, 1]);
        }
        route(app, window, DragEvent::Leave { serial: 1 })?;
        {
            let mut terminal = app.panes[&second].session.terminal()?;
            terminal.feed(b"\x1bc");
            assert!(terminal.kitty_dnd.as_ref().unwrap().drop.items.is_some());
            assert_eq!(
                terminal.feed(b"\x1b]72;t=r:x=1\x07"),
                [vt::Effect::Write(
                    b"\x1b]72;t=r:x=1:i=11:m=0;AP8B\x07\x1b]72;t=r:x=1:i=11\x07".to_vec()
                )]
            );
            terminal.feed(b"\x1b]72;t=r:o=1\x07");
            assert!(terminal.kitty_dnd.as_ref().unwrap().drop.items.is_none());
        }

        assert_eq!(
            route(app, window, enter(2, positions[0]))?,
            DragResult::Reject
        );
        app.panes[&first]
            .session
            .terminal()?
            .feed(b"\x1b]72;t=m:o=1;text/plain\x07");
        assert_eq!(
            route(app, window, update(2, positions[0]))?,
            DragResult::Copy(vec![b"text/plain".to_vec()])
        );
        let (reply, queued) = app.windows[&window]
            .drag
            .lock()
            .unwrap()
            .handle(DragEvent::Drop {
                serial: 2,
                position: positions[0],
                source_ops: vt::dnd::Operations {
                    copy: true,
                    move_: false,
                },
                items: vec![vt::dnd::Item {
                    mime: b"text/plain".to_vec(),
                    data: b"stale drop".to_vec(),
                }],
            });
        assert_eq!(reply, DragResult::Copy(vec![b"text/plain".to_vec()]));
        app.panes[&first]
            .session
            .terminal()?
            .feed(b"\x1b]72;t=a:m=1;image/\x07");
        let error_count = app.errors.len();
        app.dispatch_drag(window, queued);
        assert_eq!(
            app.errors.last().map(String::as_str),
            Some("Drop target changed before delivery")
        );
        assert_eq!(app.errors.len(), error_count + 1);
        app.errors.truncate(error_count);
        assert_eq!(
            route(app, window, update(2, positions[0]))?,
            DragResult::Reject
        );
        app.panes[&first]
            .session
            .terminal()?
            .feed(b"\x1b]72;t=a;png\x07");
        assert_eq!(
            route(app, window, update(2, positions[0]))?,
            DragResult::Reject
        );
        app.panes.get_mut(&first).unwrap().exited = true;
        assert_eq!(
            route(app, window, update(2, positions[0]))?,
            DragResult::Reject
        );
        app.panes.get_mut(&first).unwrap().exited = false;
        app.panes[&first]
            .session
            .terminal()?
            .feed(b"\x1b]72;t=A\x07");
        assert_eq!(
            route(app, window, update(2, positions[0]))?,
            DragResult::Reject
        );
        assert_eq!(
            route(app, window, enter(3, positions[0]))?,
            DragResult::Default
        );
        assert_eq!(
            route(app, window, update(2, positions[0]))?,
            DragResult::Reject
        );
        fs::write(
            directory.join("drag-result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true, "panes": 2,
                "checks": ["pending-reject", "copy-only", "cross-pane-leave-and-reoffer", "accepted-mime-filter", "owner-only-binary-payload", "duplicate-drop-reject", "leave-and-RIS-retain-data", "owner-data-request", "queued-stale-drop-reports-error", "chunked-registration-invalidates", "exited-pane-reject", "unregister-sticky-reject", "fresh-unregistered-default", "stale-serial-reject"],
                "captures": ["drag-targets.png"]
            }))?,
        )?;
        eprintln!(
            "Native drag desktop smoke: two-pane routing, cached acceptance, owner data and stale-target checks passed"
        );
        Ok(())
    })();
    for (id, terminal) in saved {
        *app.panes[&id].session.terminal()? = terminal;
    }
    app.panes.get_mut(&first).unwrap().exited = false;
    app.refresh_drag_views();
    result
}
