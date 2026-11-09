//! Native zgui integration checks driven through real retained input and layout.

use super::*;
use zgui::{
    geom::{CssPx, Point},
    platform::SurfaceEvent,
    vocab::{
        KeyEvent, KeyState, Modifiers, NamedKey, PhysicalKey, PointerAction, PointerButton,
        PointerEvent, Timestamp,
    },
};

/// Runs after the ordinary smoke fixture has opened its first native window.
pub(super) fn check(app: &mut App, event_loop: &ActiveEventLoop, directory: &Path) -> Result<bool> {
    let Some(key) = app
        .windows
        .iter()
        .find_map(|(&key, host)| (host.zgui.is_some() && host.zgui_ui.is_some()).then_some(key))
    else {
        return Ok(false);
    };
    let mut host = app.windows.remove(&key).expect("native smoke host");
    let result = check_window(app, event_loop, &mut host, directory);
    app.windows.insert(key, host);
    result?;
    root_input_boundary(app, event_loop, key, directory)?;
    Ok(true)
}

fn check_window(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    host: &mut Host,
    directory: &Path,
) -> Result<()> {
    let egui_passes = host.egui.egui_ctx().cumulative_pass_nr();
    draw(app, event_loop, host)?;
    capture(host, &directory.join("native-initial.png"))?;
    let signals = host.zgui_ui.as_ref().unwrap().clone();
    let initial_panes = app
        .tab(host.id)
        .ok_or("native smoke has no tab")?
        .pane_ids()
        .len();
    let initial_tabs = app.workspace.windows[app.index(host.id).unwrap()]
        .tabs
        .len();

    click_control(host, "new-tab")?;
    draw(app, event_loop, host)?;
    ensure(
        app.workspace.windows[app.index(host.id).unwrap()]
            .tabs
            .len()
            == initial_tabs + 1,
        "native New Tab button did not create a tab",
    )?;
    ensure(
        !host.zgui.as_ref().unwrap().wants_widget_keys(),
        "native New Tab left keyboard focus on its button",
    )?;
    app.action(event_loop, host, Action::CloseTab, false);
    draw(app, event_loop, host)?;

    click_control(host, "command-palette")?;
    draw(app, event_loop, host)?;
    ensure(
        host.palette.open,
        "native palette button did not open the palette",
    )?;
    ensure(
        host.zgui.as_ref().unwrap().wants_keyboard(),
        "native palette did not focus its editable input",
    )?;
    text(host, "Split Right")?;
    key(host, NamedKey::Enter)?;
    draw(app, event_loop, host)?;
    ensure(
        !host.palette.open,
        "native palette stayed open after choosing a command",
    )?;
    ensure(
        app.tab(host.id).unwrap().pane_ids().len() == initial_panes + 1,
        "typing and accepting Split Right did not split the terminal",
    )?;

    click_control(host, "command-palette")?;
    draw(app, event_loop, host)?;
    text(host, "Find")?;
    key(host, NamedKey::Enter)?;
    draw(app, event_loop, host)?;
    let pane = app.tab(host.id).unwrap().focused;
    ensure(
        signals.control_bounds("search").is_some(),
        "native search has no layout",
    )?;
    ensure(
        host.zgui.as_ref().unwrap().wants_keyboard(),
        "native search did not take keyboard focus",
    )?;
    text(host, "Rustty")?;
    draw(app, event_loop, host)?;
    ensure(
        app.panes[&pane]
            .search
            .as_ref()
            .is_some_and(|search| search.query == "Rustty"),
        "native text input did not update terminal search",
    )?;
    capture(host, &directory.join("find-focused.png"))?;
    click_control(host, "command-palette")?;
    draw(app, event_loop, host)?;
    ensure(
        host.palette.open && host.zgui.as_ref().unwrap().wants_keyboard(),
        "opening the palette over Find did not focus its input",
    )?;
    text(host, "Split Right")?;
    draw(app, event_loop, host)?;
    ensure(
        app.panes[&pane]
            .search
            .as_ref()
            .is_some_and(|search| search.query == "Rustty"),
        "palette input leaked into terminal search",
    )?;
    capture(host, &directory.join("native-palette.png"))?;
    key(host, NamedKey::Escape)?;
    draw(app, event_loop, host)?;
    click_control(host, "search")?;
    draw(app, event_loop, host)?;
    character(host, "a", Modifiers::PRIMARY)?;
    key(host, NamedKey::Backspace)?;
    text(host, "native")?;
    draw(app, event_loop, host)?;
    ensure(
        app.panes[&pane]
            .search
            .as_ref()
            .is_some_and(|search| search.query == "native"),
        "native Select All and editing did not replace the search query",
    )?;
    key(host, NamedKey::Escape)?;
    draw(app, event_loop, host)?;
    ensure(
        app.panes[&pane].search.is_none(),
        "native Escape did not close search",
    )?;
    ensure(
        !host.zgui.as_ref().unwrap().wants_widget_keys(),
        "native search close did not return keyboard focus to the terminal",
    )?;

    let (_, outer, content) = signals
        .pane_bounds()
        .into_iter()
        .find(|(id, ..)| *id == pane)
        .ok_or("native pane has no surface layout")?;
    click(host, center(content), PointerButton::Primary)?;
    draw(app, event_loop, host)?;
    ensure(
        !host.zgui.as_ref().unwrap().wants_widget_keys(),
        "clicking a native terminal left keyboard focus in a widget",
    )?;
    let header = [
        outer.origin.x.0 + outer.size.width.0 / 2.0,
        (outer.origin.y.0 + content.origin.y.0) / 2.0,
    ];
    click(host, header, PointerButton::Secondary)?;
    draw(app, event_loop, host)?;
    ensure(
        signals.popup_open(),
        "native pane context menu did not open",
    )?;
    ensure(
        host.zgui.as_ref().unwrap().wants_widget_keys(),
        "native context menu did not claim keyboard focus",
    )?;
    capture(host, &directory.join("native-context-menu.png"))?;
    key(host, NamedKey::Escape)?;
    draw(app, event_loop, host)?;
    ensure(
        !signals.popup_open(),
        "native context menu did not dismiss with Escape",
    )?;

    click_control(host, "command-palette")?;
    draw(app, event_loop, host)?;
    text(host, "Themes")?;
    key(host, NamedKey::Enter)?;
    draw(app, event_loop, host)?;
    ensure(
        host.palette.open && host.palette.is_theme_picker(),
        "native palette did not switch to its theme picker",
    )?;
    ensure(
        host.zgui.as_ref().unwrap().wants_keyboard(),
        "native theme picker lost keyboard focus",
    )?;
    key(host, NamedKey::ArrowDown)?;
    draw(app, event_loop, host)?;
    capture(host, &directory.join("theme-picker.png"))?;
    key(host, NamedKey::Escape)?;
    draw(app, event_loop, host)?;
    ensure(!host.palette.open, "native theme picker did not dismiss")?;

    let width = signals
        .workspace_bounds()
        .ok_or("native workspace has no layout")?
        .size
        .width
        .0;
    click_control(host, "outliner")?;
    draw(app, event_loop, host)?;
    settle(app, event_loop, host)?;

    let tab = app.tab(host.id).unwrap().id;
    app.zgui_command(
        event_loop,
        host,
        zgui_ui::Command::DialogAction {
            dialog: format!("tab:{tab}"),
            action: "edit".into(),
        },
    );
    draw(app, event_loop, host)?;
    ensure(
        host.zgui.as_ref().unwrap().wants_keyboard(),
        "native tab dialog did not focus its input",
    )?;
    text(host, "Native zgui")?;
    capture(host, &directory.join("native-tab-dialog.png"))?;
    key(host, NamedKey::Enter)?;
    draw(app, event_loop, host)?;
    ensure(
        app.tab(host.id).unwrap().title.as_deref() == Some("Native zgui"),
        "native dialog text and Enter did not apply the tab title",
    )?;
    ensure(
        signals.workspace_bounds().unwrap().size.width.0 < width,
        "native Sidebar did not participate in workspace layout",
    )?;
    capture(host, &directory.join("outliner.png"))?;
    click_control(host, "outliner")?;
    draw(app, event_loop, host)?;
    settle(app, event_loop, host)?;

    // Hide the cursor and settle the fixture so only a deliberately edited panel becomes dirty.
    let panes = app.tab(host.id).unwrap().visible_panes(false);
    for id in &panes {
        app.panes[id].session.terminal()?.feed(b"\x1b[?25l");
    }
    settle(app, event_loop, host)?;
    let baseline = host.zgui.as_ref().unwrap().panel_paint_counts();
    let model_baseline = host.zgui_ui_state.model_build_counts();
    draw(app, event_loop, host)?;
    ensure(
        host.zgui.as_ref().unwrap().panel_paint_counts() == baseline,
        "unchanged native window repainted terminal panels",
    )?;
    ensure(
        host.zgui_ui_state.model_build_counts() == model_baseline,
        "unchanged native window rebuilt its chrome or pane metadata",
    )?;
    app.panes[&pane]
        .session
        .terminal()?
        .feed(b"\x1b7\x1b[Hnative dirty tracking\x1b8");
    draw(app, event_loop, host)?;
    let after = host.zgui.as_ref().unwrap().panel_paint_counts();
    ensure(
        host.zgui_ui_state.model_build_counts() == model_baseline,
        "terminal output rebuilt native chrome or pane metadata",
    )?;
    ensure(
        host.composed.is_none(),
        "native panes also allocated a whole-window terminal composition",
    )?;
    ensure(
        baseline.iter().all(|(id, before)| {
            after
                .iter()
                .find(|(other, _)| other == id)
                .is_some_and(|(_, paints)| *paints == before + u64::from(*id == pane))
        }),
        "one changed terminal repainted another native panel",
    )?;

    host.visible = false;
    app.panes[&pane]
        .session
        .terminal()?
        .feed(b"\x1b7\x1b[Hhidden retained update\x1b8");
    draw(app, event_loop, host)?;
    ensure(
        host.zgui.as_ref().unwrap().panel_paint_counts() == after,
        "hidden native window rasterized terminal panels",
    )?;
    host.visible = true;
    draw(app, event_loop, host)?;
    let revealed = host.zgui.as_ref().unwrap().panel_paint_counts();
    ensure(
        after.iter().all(|(id, before)| {
            revealed
                .iter()
                .find(|(other, _)| other == id)
                .is_some_and(|(_, paints)| *paints == before + u64::from(*id == pane))
        }),
        "revealing a native window lost its update or repainted unchanged panels",
    )?;

    app.search_action(host, pane, Action::StartSearch);
    app.zgui_command(
        event_loop,
        host,
        zgui_ui::Command::SearchQuery(pane, "native_lazy_find".into()),
    );
    draw(app, event_loop, host)?;
    let search_before = signals.search_model().ok_or("native Find is missing")?;
    let search_models = host.zgui_ui_state.model_build_counts();
    app.panes[&pane]
        .session
        .terminal()?
        .feed(b"\x1b7\x1b[Hnative_lazy_find\x1b8");
    app.draw_zgui_frame(event_loop, host)?;
    let search_after = signals.search_model().ok_or("native Find disappeared")?;
    ensure(
        search_before.status != search_after.status && search_after.status.ends_with("/1"),
        "native Find did not publish refreshed output matches in the same frame",
    )?;
    ensure(
        host.zgui_ui_state.model_build_counts() == search_models,
        "Find output rebuilt unrelated native chrome or pane metadata",
    )?;
    app.zgui_command(event_loop, host, zgui_ui::Command::SearchClose(pane));
    draw(app, event_loop, host)?;

    let previous_title = std::mem::replace(
        &mut app.panes.get_mut(&pane).unwrap().title,
        "Updated native panel title".into(),
    );
    let before_title = host.zgui_ui_state.model_build_counts();
    host.repaint();
    draw(app, event_loop, host)?;
    ensure(
        host.zgui_ui_state.model_build_counts().0 > before_title.0
            && signals
                .pane_model(pane)
                .is_some_and(|model| model.label == "Updated native panel title"),
        "native metadata invalidation left the retained pane title stale",
    )?;
    app.panes.get_mut(&pane).unwrap().title = previous_title;
    host.repaint();

    let opacity = app.loaded.config.background_opacity;
    app.loaded.config.background_opacity = 0.5;
    host.repaint();
    draw(app, event_loop, host)?;
    let (_, _, content) = signals
        .pane_bounds()
        .into_iter()
        .find(|(id, ..)| *id == pane)
        .ok_or("native opacity check has no terminal bounds")?;
    let point = center(content);
    let (size, bytes) = host.zgui.as_ref().unwrap().capture()?;
    let alpha = bytes[((point[1] as usize).min(size[1] - 1) * size[0]
        + (point[0] as usize).min(size[0] - 1))
        * 4
        + 3];
    ensure(
        (120..=136).contains(&alpha),
        &format!("native terminal opacity was flattened: expected alpha≈128, got {alpha}"),
    )?;
    capture(host, &directory.join("native-transparency.png"))?;
    app.loaded.config.background_opacity = opacity;
    host.repaint();
    draw(app, event_loop, host)?;
    ensure(
        host.egui.egui_ctx().cumulative_pass_nr() == egui_passes,
        "native window executed an egui frame",
    )?;
    ensure(
        signals.pane_bounds().len() == app.tab(host.id).unwrap().visible_panes(false).len(),
        "native pane geometry diverged from the active workspace",
    )?;
    ensure(
        host.zgui.as_ref().unwrap().panel_paints() > 0,
        "native terminal surfaces were never painted",
    )?;
    capture(host, &directory.join("window.png"))?;
    let report = serde_json::json!({
        "passed": true,
        "renderer": host.zgui.as_ref().unwrap().description(),
        "egui_passes": host.egui.egui_ctx().cumulative_pass_nr() - egui_passes,
        "panels": signals.pane_bounds().len(),
        "panel_paints": host.zgui.as_ref().unwrap().panel_paints(),
        "panel_paint_counts": host.zgui.as_ref().unwrap().panel_paint_counts(),
        "model_build_counts": host.zgui_ui_state.model_build_counts(),
        "checks": ["native-window", "native-tab-button", "native-command-input", "native-command-keyboard-selection", "native-split-layout", "native-find-input", "palette-over-find", "native-select-all-editing", "native-find-escape", "native-pane-focus", "native-context-menu", "native-theme-picker", "native-sidebar-layout", "native-dialog-input-submit", "native-terminal-paint", "unchanged-panels-stay-clean", "only-changed-panel-repaints", "native-window-transparency", "hidden-native-window-retention", "no-egui-frames", "retained-native-chrome", "native-find-output-refresh", "native-metadata-invalidation", "no-native-window-composition"]
    });
    std::fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    eprintln!(
        "Native zgui smoke: real widgets, input, layout, terminal surfaces, and zero egui frames passed"
    );
    Ok(())
}

fn ensure(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned().into())
    }
}

fn root_input_boundary(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    window: WindowId,
    directory: &Path,
) -> Result<()> {
    use winit::event::{DeviceId, ElementState, Ime, MouseButton, WindowEvent};
    let before: usize = app.panes.values().map(|pane| pane.input_bytes).sum();
    let frames_before = app.windows[&window].zgui.as_ref().unwrap().frame_attempts();
    let paints_before = app.windows[&window].zgui.as_ref().unwrap().panel_paints();
    let host_frames_before = app.windows[&window].frames;
    let position = center(
        app.windows[&window]
            .zgui_ui
            .as_ref()
            .unwrap()
            .control_bounds("command-palette")
            .ok_or("native palette button disappeared")?,
    );
    app.window_event(
        event_loop,
        window,
        WindowEvent::CursorEntered {
            device_id: DeviceId::dummy(),
        },
    );
    app.window_event(
        event_loop,
        window,
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(
                f64::from(position[0]),
                f64::from(position[1]),
            ),
        },
    );
    app.window_event(
        event_loop,
        window,
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
    );
    app.windows.get_mut(&window).unwrap().mouse_button = Some(vt::MouseButton::Left);
    app.window_event(
        event_loop,
        window,
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    );
    ensure(
        app.windows[&window].mouse_button.is_none(),
        "native UI release retained terminal mouse capture",
    )?;
    // No render separates the pointer release from input: the app must settle its queued click.
    app.window_event(
        event_loop,
        window,
        WindowEvent::Ime(Ime::Commit("Find".into())),
    );
    ensure(
        app.windows[&window].palette.open,
        "root input dispatch did not settle the preceding native button click",
    )?;
    ensure(
        app.panes
            .values()
            .map(|pane| pane.input_bytes)
            .sum::<usize>()
            == before,
        "native palette IME text leaked into the PTY",
    )?;
    ensure(
        app.windows[&window].zgui.as_ref().unwrap().frame_attempts() == frames_before
            && app.windows[&window].zgui.as_ref().unwrap().panel_paints() == paints_before
            && app.windows[&window].frames == host_frames_before,
        "settling native pointer-to-IME input entered the GPU presentation path",
    )?;
    let mut host = app.windows.remove(&window).unwrap();
    key(&mut host, NamedKey::Enter)?;
    draw(app, event_loop, &mut host)?;
    let pane = app.tab(host.id).unwrap().focused;
    let found = app.panes[&pane].search.is_some();
    key(&mut host, NamedKey::Escape)?;
    draw(app, event_loop, &mut host)?;
    app.windows.insert(window, host);
    ensure(
        found,
        "IME committed through root dispatch did not filter the native command palette",
    )?;
    let path = directory.join("result.json");
    let mut report: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    report["checks"].as_array_mut().unwrap().extend([
        "native-root-pointer-ime-boundary".into(),
        "native-ui-release-clears-terminal-capture".into(),
        "native-input-without-gpu-presentation".into(),
    ]);
    std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}

fn draw(app: &mut App, event_loop: &ActiveEventLoop, host: &mut Host) -> Result<()> {
    // First pass consumes native widget commands; second observes any mount/focus callbacks.
    app.draw_zgui_frame(event_loop, host)?;
    app.draw_zgui_frame(event_loop, host)
}

fn settle(app: &mut App, event_loop: &ActiveEventLoop, host: &mut Host) -> Result<()> {
    for _ in 0..16 {
        std::thread::sleep(Duration::from_millis(16));
        host.zgui.as_mut().unwrap().deadline_reached();
        draw(app, event_loop, host)?;
    }
    Ok(())
}

fn center(bounds: zgui_ui::Bounds) -> [f32; 2] {
    [
        bounds.origin.x.0 + bounds.size.width.0 / 2.0,
        bounds.origin.y.0 + bounds.size.height.0 / 2.0,
    ]
}

fn click_control(host: &mut Host, name: &str) -> Result<()> {
    let bounds = host
        .zgui_ui
        .as_ref()
        .unwrap()
        .control_bounds(name)
        .ok_or_else(|| format!("native control {name} has no layout"))?;
    click(host, center(bounds), PointerButton::Primary)
}

fn click(host: &mut Host, position: [f32; 2], button: PointerButton) -> Result<()> {
    let scale = host.window.scale_factor() as f32;
    let position = Point::new(CssPx(position[0] / scale), CssPx(position[1] / scale));
    let runtime = host.zgui.as_mut().unwrap();
    for action in [
        PointerAction::Entered,
        PointerAction::Moved,
        PointerAction::Pressed,
        PointerAction::Released,
    ] {
        let mut event = PointerEvent::mouse(position);
        if matches!(action, PointerAction::Pressed | PointerAction::Released) {
            event = event.with_button(button);
        }
        runtime.queue(SurfaceEvent::Pointer {
            action,
            event,
            modifiers: Modifiers::NONE,
            timestamp: Timestamp::ORIGIN,
        });
    }
    runtime.frame()
}

fn key(host: &mut Host, key: NamedKey) -> Result<()> {
    keys(
        host,
        KeyEvent::named(key, PhysicalKey::Unidentified(0)),
        Modifiers::NONE,
    )
}

fn character(host: &mut Host, text: &str, modifiers: Modifiers) -> Result<()> {
    keys(host, KeyEvent::character(text), modifiers)
}

fn keys(host: &mut Host, event: KeyEvent, modifiers: Modifiers) -> Result<()> {
    let runtime = host.zgui.as_mut().unwrap();
    for state in [KeyState::Pressed, KeyState::Released] {
        runtime.queue(SurfaceEvent::Key {
            state,
            event: event.clone(),
            modifiers,
            timestamp: Timestamp::ORIGIN,
        });
    }
    runtime.frame()
}

fn text(host: &mut Host, text: &str) -> Result<()> {
    for character in text.chars() {
        self::character(host, &character.to_string(), Modifiers::NONE)?;
    }
    Ok(())
}

fn capture(host: &Host, path: &Path) -> Result<()> {
    let (size, bytes) = host.zgui.as_ref().unwrap().capture()?;
    image::save_buffer(
        path,
        &bytes,
        size[0] as u32,
        size[1] as u32,
        image::ColorType::Rgba8,
    )?;
    Ok(())
}
