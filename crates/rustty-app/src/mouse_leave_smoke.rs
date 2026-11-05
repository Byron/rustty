//! Exercise native pointer dispatch, capturing each pane's ordered input queue.
use super::*;
use winit::{event::DeviceId, keyboard::ModifiersState};

fn with_host(
    app: &mut App,
    window: WindowId,
    action: impl FnOnce(&mut App, &mut Host) -> Result<()>,
) -> Result<()> {
    let mut host = app.windows.remove(&window).unwrap();
    let result = action(app, &mut host);
    app.windows.insert(window, host);
    result
}

fn reports(app: &mut App, pane: Id) -> Result<Vec<Vec<u8>>> {
    let pane = app.panes.get_mut(&pane).unwrap();
    let bytes = queued_input(pane)?.into_iter().skip(1).collect();
    pane.input.truncate(1);
    pane.input_bytes = 0;
    Ok(bytes)
}

fn expect(app: &mut App, pane: Id, expected: &[&[u8]]) -> Result<()> {
    let actual = reports(app, pane)?;
    if actual.iter().map(Vec::as_slice).collect::<Vec<_>>() != expected {
        return Err(
            format!("mouse leave pane {pane}: expected {expected:?}, got {actual:?}").into(),
        );
    }
    Ok(())
}

pub(super) fn check(app: &mut App, event_loop: &ActiveEventLoop, directory: &Path) -> Result<()> {
    let window = *app.windows.keys().next().ok_or("no mouse smoke window")?;
    let mut host = app.windows.remove(&window).unwrap();
    let first = app.focused(host.id).ok_or("no mouse smoke pane")?;
    app.action(
        event_loop,
        &mut host,
        Action::NewSplit(Direction::Right),
        true,
    );
    let second = app.focused(host.id).ok_or("mouse smoke split failed")?;
    assert_ne!(first, second);
    app.focus_pane(host.id, first);
    host.visible = true;
    host.occluded = false;
    host.focus_hint.dismiss();
    app.draw(event_loop, &mut host)?;
    let positions = [first, second].map(|id| host.rects[&id].center());
    let host_id = host.id;
    let scale = host.window.scale_factor();
    app.windows.insert(window, host);
    let original_workspace = app.workspace.clone();
    let mut saved = Vec::new();
    for (index, id) in [first, second].into_iter().enumerate() {
        let pane = app.panes.get_mut(&id).unwrap();
        let mut terminal = pane.session.terminal()?;
        let mut fixture = vt::Terminal::new(terminal.cols, terminal.rows, 0);
        fixture.feed(format!("\x1b[1;36mMouse leave target {}\x1b[0m\r\nSGR pixel reports stay with this pane.\x1b[?1016h", index + 1).as_bytes());
        saved.push((
            id,
            std::mem::replace(&mut *terminal, fixture),
            std::mem::take(&mut pane.input),
            std::mem::take(&mut pane.input_bytes),
            pane.mouse_cell.take(),
            pane.exited,
            std::mem::take(&mut pane.selection_gesture),
        ));
        // Keep the real Session and production ordered queue while withholding
        // simulated input from the disposable child, as other pointer smokes do.
        pane.input.push_back(SessionInput::Text(Vec::new()));
    }
    let motion = |position: Pos2| WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: LogicalPosition::new(position.x, position.y).to_physical(scale),
    };
    let left = || WindowEvent::CursorLeft {
        device_id: DeviceId::dummy(),
    };
    let button = |state| WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    };
    let result = (|| -> Result<()> {
        let errors = app.errors.len();
        // Mode 1016 alone opts into Leave; ordinary tracking remains disabled.
        app.window_event(event_loop, window, motion(positions[0]));
        let (selection, viewport) = {
            let mut terminal = app.panes[&first].session.terminal()?;
            let screen = terminal.screen_mut();
            let row = screen.row(0).id;
            screen.selection = Some(vt::Selection {
                start: vt::GridPoint { row, col: 0 },
                end: vt::GridPoint { row, col: 3 },
                rectangular: false,
            });
            (screen.selection, screen.viewport_offset)
        };
        app.panes.get_mut(&first).unwrap().mouse_cell = Some([2, 3]);
        app.window_event(event_loop, window, motion(positions[1]));
        expect(app, first, &[b"\x1b[<288;0;0M"])?;
        expect(app, second, &[])?;
        assert_eq!(app.focused(host_id), Some(first));
        assert_eq!(app.panes[&first].mouse_cell, None);
        {
            let terminal = app.panes[&first].session.terminal()?;
            assert_eq!(terminal.screen().selection, selection);
            assert_eq!(terminal.screen().viewport_offset, viewport);
        }
        app.window_event(event_loop, window, motion(positions[1]));
        expect(app, second, &[])?;

        // A stationary pointer becomes app UI when a modal opens.
        with_host(app, window, |app, host| {
            host.palette.open = true;
            app.update_mouse_hover(host);
            app.update_mouse_hover(host);
            host.palette.open = false;
            Ok(())
        })?;
        expect(app, second, &[b"\x1b[<288;0;0M"])?;
        app.window_event(event_loop, window, motion(positions[1]));
        app.windows.get_mut(&window).unwrap().modifiers =
            (ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL).into();
        app.window_event(event_loop, window, left());
        app.window_event(event_loop, window, left());
        expect(app, second, &[b"\x1b[<316;0;0M"])?;
        app.windows.get_mut(&window).unwrap().modifiers = Modifiers::default();

        // Real motion follows the hovered inactive pane without changing focus.
        for id in [first, second] {
            app.panes[&id].session.terminal()?.feed(b"\x1b[?1003h");
        }
        app.window_event(event_loop, window, motion(positions[1]));
        let moved = reports(app, second)?;
        assert_eq!(moved.len(), 1);
        assert!(moved[0].starts_with(b"\x1b[<35;") && moved[0].ends_with(b"M"));
        expect(app, first, &[])?;
        assert_eq!(app.focused(host_id), Some(first));
        app.window_event(event_loop, window, left());
        expect(app, second, &[b"\x1b[<288;0;0M"])?;

        // Pressed motion/release keep the captured pane even after crossing.
        app.window_event(event_loop, window, motion(positions[0]));
        reports(app, first)?;
        app.window_event(event_loop, window, button(ElementState::Pressed));
        let pressed = reports(app, first)?;
        assert_eq!(pressed.len(), 1);
        assert!(pressed[0].starts_with(b"\x1b[<0;"));
        app.window_event(event_loop, window, motion(positions[1]));
        let dragged = reports(app, first)?;
        assert_eq!(dragged.len(), 2);
        assert_eq!(dragged[0], b"\x1b[<288;0;0M");
        assert!(dragged[1].starts_with(b"\x1b[<32;") && dragged[1].ends_with(b"M"));
        expect(app, second, &[])?;
        assert_eq!(
            app.windows[&window].mouse_button,
            Some(vt::MouseButton::Left)
        );
        app.window_event(event_loop, window, button(ElementState::Released));
        let released = reports(app, first)?;
        assert_eq!(released.len(), 1);
        assert!(released[0].starts_with(b"\x1b[<0;") && released[0].ends_with(b"m"));
        assert_eq!(app.focused(host_id), Some(first));
        assert_eq!(app.windows[&window].mouse_button, None);
        expect(app, second, &[])?;
        app.window_event(event_loop, window, left());
        expect(app, second, &[b"\x1b[<288;0;0M"])?;
        for id in [first, second] {
            app.panes[&id].session.terminal()?.feed(b"\x1b[?1003l");
        }

        // Layout replacement and tab switches invalidate stale hit geometry.
        app.window_event(event_loop, window, motion(positions[0]));
        with_host(app, window, |app, host| {
            let a = host.rects[&first];
            let b = host.rects[&second];
            host.rects.insert(first, b);
            host.rects.insert(second, a);
            app.update_mouse_hover(host);
            Ok(())
        })?;
        expect(app, first, &[b"\x1b[<288;0;0M"])?;
        expect(app, second, &[])?;
        let tab = app.workspace.id();
        let pane = app.workspace.id();
        let index = app.index(host_id).unwrap();
        app.workspace.windows[index]
            .tabs
            .push(Tab::new(tab, pane, fixture_directory()));
        app.workspace.windows[index].active_tab = 1;
        with_host(app, window, |app, host| {
            app.update_mouse_hover(host);
            app.update_mouse_hover(host);
            Ok(())
        })?;
        expect(app, second, &[b"\x1b[<288;0;0M"])?;
        app.workspace = original_workspace.clone();
        with_host(app, window, |app, host| app.draw(event_loop, host))?;

        app.window_event(event_loop, window, motion(positions[0]));
        with_host(app, window, |app, host| {
            app.quick_visible(host, false, false);
            app.update_mouse_hover(host);
            app.quick_visible(host, true, false);
            Ok(())
        })?;
        expect(app, first, &[b"\x1b[<288;0;0M"])?;
        app.window_event(event_loop, window, motion(positions[0]));
        app.panes.get_mut(&first).unwrap().mouse_cell = Some([1, 1]);
        app.panes.get_mut(&first).unwrap().exited = true;
        with_host(app, window, |app, host| {
            app.update_mouse_hover(host);
            Ok(())
        })?;
        expect(app, first, &[])?;
        assert_eq!(app.panes[&first].mouse_cell, None);
        assert_eq!(app.windows[&window].mouse_hover, None);
        app.panes.get_mut(&first).unwrap().exited = false;

        // A pane removed from the current layout still receives its final Leave
        // before reconciliation hides the retained Session.
        app.window_event(event_loop, window, motion(positions[0]));
        app.remember();
        assert!(app.tab_mut(host_id).unwrap().close(first));
        app.reconcile(event_loop);
        expect(app, first, &[b"\x1b[<288;0;0M"])?;
        app.workspace = original_workspace.clone();
        app.reconcile(event_loop);
        with_host(app, window, |app, host| {
            app.draw(event_loop, host)?;
            app.clear_mouse_hover(host);
            capture_window(
                app,
                event_loop,
                host,
                &directory.join("mouse-leave-targets.png"),
            )
        })?;
        assert_eq!(
            app.errors.len(),
            errors,
            "mouse leave generated host errors"
        );
        fs::write(
            directory.join("mouse-leave-result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true,
                "panes": 2,
                "capture": "ordered per-pane input queues",
                "checks": ["mode-1016-without-tracking", "pane-crossing-without-focus", "same-pane-dedup", "app-ui-capture", "cursor-left-modifiers-dedup", "inactive-hover-motion", "drag-capture-and-release", "layout-and-tab-invalidation", "hidden-and-retained-dead-pane", "removed-pane-final-leave"]
            }))?,
        )?;
        Ok(())
    })();
    app.workspace = original_workspace;
    for (id, terminal, input, bytes, mouse_cell, exited, selection_gesture) in saved {
        let pane = app.panes.get_mut(&id).unwrap();
        *pane.session.terminal()? = terminal;
        pane.input = input;
        pane.input_bytes = bytes;
        pane.mouse_cell = mouse_cell;
        pane.exited = exited;
        pane.selection_gesture = selection_gesture;
    }
    result?;
    eprintln!("Native mouse leave smoke: ten routing/lifecycle checks passed across two panes");
    Ok(())
}
