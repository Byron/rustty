//! Adapt shared desktop state and actions to the retained native zgui window.
use super::zgui_ui::*;
use super::*;

#[derive(Default)]
pub(super) struct State {
    pub outliner_open: bool,
    expansion: outliner::Expansion,
    query: String,
    agents_only: bool,
    edit_tab: Option<(Id, String, String, Option<String>)>,
    panels: BTreeMap<Id, ComposedFrame>,
    model_dirty: std::cell::Cell<bool>,
    config: Option<Arc<Config>>,
    errors: usize,
    model_builds: u64,
    pane_model_builds: u64,
}

impl State {
    pub fn modal_input(&self) -> bool {
        self.edit_tab.is_some()
    }

    pub fn mark_model_dirty(&self) {
        self.model_dirty.set(true);
    }

    fn needs_model_update(&self, errors: usize) -> bool {
        self.model_dirty.get() || self.model_builds == 0 || self.errors != errors
    }

    fn take_model_dirty(&self, errors: usize) -> bool {
        let dirty = self.needs_model_update(errors);
        self.model_dirty.set(false);
        dirty
    }

    pub fn model_build_counts(&self) -> (u64, u64) {
        (self.model_builds, self.pane_model_builds)
    }
}

impl App {
    pub(super) fn initialize_zgui(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
    ) -> Result<()> {
        use std::{cell::RefCell, rc::Rc};
        let mounted = Rc::new(RefCell::new(None::<WindowSignals>));
        let output = Rc::clone(&mounted);
        let proxy = self.proxy.clone();
        let runtime = zgui_runtime::NativeRuntime::open(
            event_loop,
            Arc::clone(&host.window),
            self.background_smoke,
            zgui_ui::STYLESHEET,
            move || {
                let state = output
                    .borrow_mut()
                    .get_or_insert_with(WindowSignals::new)
                    .clone();
                zgui_ui::window_view(state)
            },
            move || {
                let _ = proxy.send_event(Event::ZguiWake);
            },
        )?;
        eprintln!("Rustty renderer: {}", runtime.description());
        host.zgui_ui = mounted.borrow_mut().take();
        host.zgui = Some(runtime);
        Ok(())
    }

    pub(super) fn draw_zgui_frame(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
    ) -> Result<()> {
        self.zgui_frame(event_loop, host, true)
    }

    pub(super) fn settle_zgui_input(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
    ) -> Result<()> {
        host.zgui.as_mut().unwrap().settle()?;
        let commands = host.zgui_ui.as_ref().unwrap().drain_commands();
        if !commands.is_empty() || host.zgui_ui_state.needs_model_update(self.errors.len()) {
            for command in commands {
                self.zgui_command(event_loop, host, command);
            }
            self.zgui_frame(event_loop, host, false)?;
        }
        Ok(())
    }

    fn zgui_frame(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
        present: bool,
    ) -> Result<()> {
        if let Some(runtime) = &mut host.zgui {
            runtime.set_visible(host.visible && !host.occluded);
        }
        if !host.visible || host.occluded {
            self.cancel_selection_drag(host);
            if let Some(runtime) = &mut host.zgui {
                runtime.service_hidden()?;
                host.deadline = runtime.deadline();
            }
            return Ok(());
        }
        let size = host.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let scale = host.window.scale_factor() as f32;
        let now = Instant::now();
        host.accesskit_active = host
            .zgui
            .as_ref()
            .is_some_and(|runtime| runtime.accessibility_active());
        self.zgui_terminal_actions(host);
        let signals = host
            .zgui_ui
            .as_ref()
            .ok_or("zgui view is not mounted")?
            .clone();
        for command in signals.drain_commands() {
            self.zgui_command(event_loop, host, command);
        }
        if self.import_selector {
            if host.zgui_ui_state.take_model_dirty(self.errors.len()) {
                signals.update(self.zgui_model(host));
            }
            if !present {
                host.zgui.as_mut().unwrap().settle()?;
                host.repaint_terminal();
                return Ok(());
            }
            host.zgui.as_mut().unwrap().frame()?;
            host.frames += 1;
            host.last_frame_started = Some(now);
            host.deadline = host.zgui.as_mut().unwrap().deadline();
            if let Some(directory) = handoff_ui::smoke_directory() {
                let path = directory.join("selector.png");
                if !path.exists() {
                    save_native_capture(host, &path)?;
                }
            }
            return Ok(());
        }
        let Some(active) = self.tab(host.id).cloned() else {
            return Ok(());
        };
        self.validate_hints(host);
        if let Some(Confirmation::Paste(paste)) = &host.confirm
            && !self.paste_target_exists(host.id, paste.pane)
        {
            host.confirm = None;
            host.zgui_ui_state.mark_model_dirty();
        }
        if host.search_focus.is_some_and(|id| {
            id != active.focused || self.panes.get(&id).is_none_or(|pane| pane.search.is_none())
        }) {
            host.search_focus = None;
            host.zgui_ui_state.mark_model_dirty();
        }
        if host
            .selection_drag
            .as_ref()
            .is_some_and(|(id, _)| *id != active.focused)
            || host.ui_input()
        {
            self.cancel_selection_drag(host);
        }
        if host.ui_input() {
            host.mouse_button = None;
            host.divider_drag = None;
            host.composing = false;
            host.preedit.clear();
            host.preedit_selection = None;
        }
        self.sync_window_title(host);
        self.activity_flashes.remove(&active.id);
        let model_dirty = host.zgui_ui_state.take_model_dirty(self.errors.len());
        if model_dirty {
            signals.update(self.zgui_model(host));
        }
        if model_dirty || host.zgui_ui_state.config.is_none() {
            host.zgui_ui_state.config = Some(Arc::new(self.window_config(host.id).clone()));
        }
        let config = Arc::clone(host.zgui_ui_state.config.as_ref().unwrap());
        // Settle layout without submitting an intermediate GPU frame.
        host.zgui.as_mut().unwrap().settle()?;
        let bounds = signals
            .workspace_bounds()
            .map(|bounds| logical_bounds(bounds, scale))
            .unwrap_or_else(|| {
                egui::Rect::from_min_size(
                    Pos2::ZERO,
                    Vec2::new(size.width as f32 / scale, size.height as f32 / scale),
                )
            });
        host.content = Rect {
            x: bounds.left(),
            y: bounds.top(),
            width: bounds.width(),
            height: bounds.height(),
        };
        let metrics = host.fonts.metrics();
        let cell_height = metrics.cell_height as f32 / scale;
        let minimum = self.split_minimum(host);
        let required = active
            .visible_tree(host.peek.is_some())
            .minimum_size(minimum);
        let min_inner_size = LogicalSize::new(
            MIN_WINDOW_SIZE.width.max(f64::from(
                required[0] + size.width as f32 / scale - bounds.width(),
            )),
            MIN_WINDOW_SIZE.height.max(f64::from(
                required[1] + size.height as f32 / scale - bounds.height(),
            )),
        );
        if host.min_inner_size != min_inner_size {
            host.min_inner_size = min_inner_size;
            host.window.set_min_inner_size(Some(min_inner_size));
        }
        let animations_enabled = config.window_animations.unwrap_or_else(|| {
            #[cfg(target_os = "macos")]
            {
                !Platform::reduce_motion()
            }
            #[cfg(not(target_os = "macos"))]
            {
                true
            }
        });
        let mut visuals = host.zoom_animation.update(
            &active,
            host.peek.is_some(),
            host.content,
            cell_height,
            minimum,
            scale,
            &config,
            animations_enabled,
            now,
        );
        let runtime = host.zgui.as_mut().unwrap();
        let ids: Vec<_> = visuals.iter().map(|pane| pane.id).collect();
        let pane_rect = |rect: egui::Rect| {
            let outer = rect.expand(1.0);
            [
                (outer.left() - bounds.left()) / bounds.width().max(1.0),
                (outer.top() - bounds.top()) / bounds.height().max(1.0),
                outer.width() / bounds.width().max(1.0),
                outer.height() / bounds.height().max(1.0),
            ]
        };
        for visual in &visuals {
            signals.set_surface(visual.id, runtime.panel_surface(visual.id));
        }
        runtime.retain_panels(&ids);
        if model_dirty || ids != host.pane_order {
            host.zgui_ui_state.pane_model_builds += 1;
            let panes = visuals
                .iter()
                .map(|visual| {
                    let pane = self.panes.get(&visual.id);
                    let label = pane
                        .filter(|pane| !pane.title.is_empty())
                        .map(|pane| pane.title.clone())
                        .unwrap_or_else(|| {
                            self.directory_labels
                                .get(&visual.id)
                                .map(ToString::to_string)
                                .unwrap_or_else(|| "Terminal".into())
                        });
                    let mut menu_items = vec![
                        menu(
                            "Split right",
                            Command::PaneAction(visual.id, Action::NewSplit(Direction::Right)),
                        ),
                        menu(
                            "Split down",
                            Command::PaneAction(visual.id, Action::NewSplit(Direction::Down)),
                        ),
                        menu("Find", Command::PaneAction(visual.id, Action::StartSearch)),
                        menu(
                            "Pin panel",
                            Command::PinTarget(outliner::Target::Pane {
                                tab: active.id,
                                pane: visual.id,
                            }),
                        ),
                        menu(
                            "Close panel",
                            Command::PaneAction(visual.id, Action::CloseSurface),
                        ),
                    ];
                    for item in &mut menu_items {
                        item.disabled = active.modal_pane().is_some_and(|id| id != visual.id);
                    }
                    PaneModel {
                        id: visual.id,
                        rect: pane_rect(visual.rect),
                        label,
                        focused: visual.id == active.focused,
                        opacity: 1.0,
                        badges: [
                            self.workspace
                                .pinned_panes
                                .contains(&visual.id)
                                .then_some("Pinned".to_owned()),
                            (active.zoom == Some(visual.id)).then_some("Zoomed".to_owned()),
                            host.peek.as_ref().and_then(|_| {
                                active
                                    .peek_targets(host.content, minimum)
                                    .iter()
                                    .position(|target| target.target == visual.id)
                                    .map(|index| format!("{}", index + 1))
                            }),
                        ]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" · "),
                        status: self
                            .failed_panes
                            .get(&visual.id)
                            .cloned()
                            .unwrap_or_else(|| {
                                pane.map_or_else(
                                    || "Starting terminal…".into(),
                                    |pane| {
                                        if pane.exited {
                                            "Process exited".into()
                                        } else {
                                            pane.program_tooltip().unwrap_or_default().to_owned()
                                        }
                                    },
                                )
                            }),
                        retry: self.failed_panes.contains_key(&visual.id),
                        hint: host
                            .hints
                            .as_ref()
                            .filter(|hints| hints.pane == visual.id)
                            .map(hints::Hints::prompt),
                        link_hints: host
                            .hints
                            .as_ref()
                            .filter(|hints| hints.pane == visual.id)
                            .map(|hints| {
                                hints
                                    .visible_entries()
                                    .filter_map(|entry| {
                                        let rect = entry.bounds.first()?;
                                        Some((
                                            entry.label.to_uppercase(),
                                            [
                                                rect.left() - hints.rect.left(),
                                                rect.top() - hints.rect.top(),
                                                rect.width(),
                                                rect.height(),
                                            ],
                                        ))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        progress: pane
                            .and_then(Pane::progress)
                            .and_then(Progress::percentage)
                            .map(f64::from),
                        menu: menu_items,
                    }
                })
                .collect();
            signals.update_panes(panes);
        } else {
            for visual in &visuals {
                signals.update_pane_geometry(visual.id, pane_rect(visual.rect));
            }
        }
        runtime.settle()?;
        let native_bounds = signals.pane_bounds();
        let window_bounds = egui::Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(size.width as f32 / scale, size.height as f32 / scale),
        );
        let mut ui_areas = uncovered_rects(window_bounds, bounds);
        for (_, outer, content) in &native_bounds {
            // Keep split borders available to the shared divider drag controller.
            let outer = logical_bounds(*outer, scale).shrink(1.0);
            ui_areas.extend(uncovered_rects(outer, logical_bounds(*content, scale)));
        }
        host.ui_areas.set_rects(ui_areas);
        host.rects.clear();
        host.pane_order = ids;
        host.modal_pane = active.modal_pane();
        host.overlay_panes = active.overlays.iter().map(|overlay| overlay.pane).collect();
        for visual in &mut visuals {
            if let Some((_, outer, content)) =
                native_bounds.iter().find(|(id, ..)| *id == visual.id)
            {
                let outer = logical_bounds(*outer, scale);
                let content = logical_bounds(*content, scale);
                let start = content.min - outer.min;
                let end = outer.max - content.max;
                let inset = |rect: egui::Rect| {
                    egui::Rect::from_min_max(
                        rect.min + start,
                        (rect.max - end).max(rect.min + start),
                    )
                };
                visual.layout = inset(visual.layout.expand(1.0));
                visual.rect = content;
                if let Some(resize) = &mut visual.resize {
                    resize.layout = inset(resize.layout.expand(1.0));
                }
                host.rects.insert(visual.id, content);
            }
        }
        if !host.modal_input() && std::mem::take(&mut host.unfocus_text_input) {
            signals.focus_pane(active.focused);
        }
        if host.focus_text_input && host.search_focus.is_some() && !host.modal_input() {
            signals.focus_search();
            host.focus_text_input = false;
        } else if host.focus_text_input && !host.modal_input() {
            signals.focus_pane(active.focused);
            host.focus_text_input = false;
        }
        if !present {
            host.zgui.as_mut().unwrap().settle()?;
            host.repaint_terminal();
            return Ok(());
        }
        let (blink_on, blink_deadline) = cursor_blink_phase(host.cursor_blink_started, now);
        let prepared =
            self.prepare_panels(host, &active, bounds, &mut visuals, &config, now, blink_on);
        if let Some(error) = prepared.error
            && !self.errors.contains(&error)
        {
            self.errors.push(error);
            host.repaint();
        }
        // Terminal search refreshes during preparation, independently of the window chrome.
        signals.update_search(self.zgui_search(host));
        let runtime = host.zgui.as_mut().unwrap();
        host.zgui_ui_state
            .panels
            .retain(|id, _| host.rects.contains_key(id));
        for pane in &prepared.panels {
            let Some(rect) = host.rects.get(&pane.id) else {
                continue;
            };
            let size = [
                (rect.width() * scale).round().max(1.0) as u32,
                (rect.height() * scale).round().max(1.0) as u32,
            ];
            let frame = native_panel_frame(&mut host.zgui_ui_state.panels, pane, size)?;
            runtime.update_panel(pane.id, frame);
        }
        if host.accesskit_active {
            for (&id, pane) in &mut host.prepared {
                if let Some(parent) = signals.surface_node(id) {
                    let frame = Arc::clone(&pane.frame);
                    runtime.update_accessibility(id, parent, &frame, pane.accessibility(id), scale);
                }
            }
        }
        runtime.frame()?;
        if !runtime.wants_keyboard() && !host.hints_ime {
            host.window
                .set_ime_allowed(prepared.terminal_ime_rect.is_some());
            if let Some(rect) = prepared.terminal_ime_rect {
                host.window.set_ime_cursor_area(
                    winit::dpi::LogicalPosition::new(rect.left() as f64, rect.top() as f64),
                    LogicalSize::new(rect.width() as f64, rect.height() as f64),
                );
            }
        }
        host.deadline = [
            prepared.deadline,
            runtime.deadline(),
            prepared.needs_blink.then_some(blink_deadline),
            host.focus_hint.deadline(now),
            host.navigation_warning.map(|(_, deadline)| deadline),
        ]
        .into_iter()
        .flatten()
        .min();
        self.zgui_terminal_actions(host);
        if host.zoom_animation.is_running() {
            self.cancel_selection_drag(host);
            host.divider_drag = None;
            host.hovered_link = None;
            // Presentation follows the display's vsync; a timer would cap high-refresh displays.
            host.window.request_redraw();
        }
        self.update_mouse_hover(host);
        if !self.background_smoke
            && let Some(cursor) = self.pointer_cursor(host)
        {
            host.window.set_cursor(cursor);
        }
        host.frames += 1;
        host.last_frame_started = Some(now);
        if host.capture
            && let Some(smoke) = &self.smoke
        {
            save_native_capture(host, &smoke.directory.join("window.png"))?;
            host.capture = false;
        }
        if let Some(directory) = &self.tmuxx_smoke
            && !directory.join("result.json").exists()
        {
            if self.started.elapsed() > Duration::from_secs(1) && !host.prepared.is_empty() {
                save_native_capture(host, &directory.join("window.png"))?;
                let result = serde_json::json!({ "panes": host.prepared.len(), "windows": self.workspace.windows.len(), "instance": self.tmuxx.first().map(|entry| &entry.instance), "errors": self.errors, "hidden": !host.window.is_visible().unwrap_or(false), "focused": host.window.has_focus(), "floating_panes": host.overlay_panes.len(), "draw_order": host.pane_order });
                std::fs::write(
                    directory.join("result.json"),
                    serde_json::to_vec_pretty(&result)?,
                )?;
            } else {
                let deadline = now + Duration::from_millis(100);
                host.deadline = Some(host.deadline.map_or(deadline, |old| old.min(deadline)));
            }
        }
        Ok(())
    }

    fn zgui_terminal_actions(&mut self, host: &mut Host) {
        let Some(runtime) = &mut host.zgui else {
            return;
        };
        for action in runtime.take_terminal_actions() {
            use zgui_runtime::TerminalAction;
            match action {
                TerminalAction::Focus { pane } => {
                    self.focus_pane(host.id, pane);
                    if let Some(signals) = &host.zgui_ui {
                        signals.focus_pane(pane);
                    }
                    if !self.background_smoke {
                        host.window.focus_window();
                    }
                }
                TerminalAction::Scroll { pane, up } => {
                    if let Some(pane) = self.panes.get(&pane)
                        && let Ok(mut terminal) = pane.session.terminal()
                    {
                        let amount = (terminal.rows as isize).max(1);
                        terminal
                            .screen_mut()
                            .scroll_viewport(if up { amount } else { -amount });
                    }
                }
                TerminalAction::Select { pane, selection } => {
                    self.cancel_selection_drag(host);
                    if let Some(pane) = self.panes.get(&pane)
                        && let Ok(mut terminal) = pane.session.terminal()
                        && selection.is_none_or(|selection| {
                            terminal.screen().row_by_id(selection.start.row).is_some()
                                && terminal.screen().row_by_id(selection.end.row).is_some()
                        })
                    {
                        terminal.screen_mut().selection = selection;
                    }
                }
            }
            host.repaint();
        }
    }

    pub(super) fn zgui_model(&mut self, host: &mut Host) -> WindowModel {
        host.zgui_ui_state.model_builds += 1;
        host.zgui_ui_state.errors = self.errors.len();
        if self.import_selector {
            return WindowModel {
                dialogs: vec![self.zgui_import_model()],
                background: self.config().background.to_array(),
                foreground: self.config().foreground.to_array(),
                accent: self.config().palette[4].to_array(),
                ..WindowModel::default()
            };
        }
        let Some(window) = self.index(host.id) else {
            return WindowModel::default();
        };
        let state = &self.workspace.windows[window];
        let active = state.tabs.get(state.active_tab);
        let tabs = state
            .tabs
            .iter()
            .map(|tab| {
                let (label, activity) = self.tab_label(tab);
                TabModel {
                    id: tab.id,
                    label: label.to_owned(),
                    activity,
                    color: tab.color,
                    menu: vec![
                        menu(
                            "Tab title and color…",
                            Command::DialogAction {
                                dialog: format!("tab:{}", tab.id),
                                action: "edit".into(),
                            },
                        ),
                        menu("Close tab", Command::CloseTab(tab.id)),
                    ],
                }
            })
            .collect();
        let selected_tab = active.map(|tab| tab.id);
        let breadcrumbs = active
            .map(|tab| outliner::breadcrumb(tab, &self.directory_labels))
            .unwrap_or_default()
            .into_iter()
            .map(|row| BreadcrumbModel {
                target: row.target,
                label: row.label,
            })
            .collect();
        let search = self.zgui_search(host);
        let outliner = if host.zgui_ui_state.outliner_open {
            let rows = self.outline_rows();
            let ui = &mut host.zgui_ui_state;
            ui.expansion.sync(&rows);
            let indices = if ui.query.is_empty() && !ui.agents_only {
                ui.expansion.visible(&rows)
            } else {
                let targets: HashSet<_> = outliner::search(&rows, &ui.query)
                    .into_iter()
                    .filter(|result| {
                        !ui.agents_only || result.path.last().is_some_and(|row| row.has_agent)
                    })
                    .flat_map(|result| result.path.into_iter().map(|row| row.target))
                    .collect();
                rows.iter()
                    .enumerate()
                    .filter_map(|(index, row)| targets.contains(&row.target).then_some(index))
                    .collect()
            };
            Some(OutlinerModel {
                query: ui.query.clone(), agents_only: ui.agents_only,
                rows: indices.into_iter().map(|index| {
                    let row = &rows[index];
                    OutlinerRow {
                        target: row.target, label: row.label.clone(),
                        detail: row.program_status.as_ref().map(|(_, text)| text.clone())
                            .or_else(|| row.tmuxx.clone()).unwrap_or_default(),
                        depth: row.depth, selected: row.focused,
                        expanded: ui.expansion.is_open(row.target),
                        has_children: outliner::has_children(&rows, index),
                        pinned: matches!(row.target, outliner::Target::Pane { pane, .. } if self.workspace.pinned_panes.contains(&pane)),
                        color: row.color,
                    }
                }).collect(),
            })
        } else {
            None
        };
        let palette = host.palette.open.then(|| {
            let themes = host.palette.is_theme_picker();
            PaletteModel {
                title: if themes {
                    "Select a theme"
                } else {
                    "Command palette"
                }
                .into(),
                placeholder: if themes {
                    "Find a theme…"
                } else {
                    "Execute a command…"
                }
                .into(),
                items: host
                    .palette
                    .entries(self.config())
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, shortcut))| PaletteItem {
                        index,
                        label,
                        shortcut,
                    })
                    .collect(),
                preview: themes,
            }
        });
        let mut dialogs = Vec::new();
        if let Some(target) = &host.tmuxx_connect {
            let mut dialog = dialog(
                "connect",
                "Connect to tmuxx",
                format!(
                    "{} · ssh:HOST · system-ssh:HOST\nSSH uses your SSH configuration; it never reconnects automatically.",
                    tmuxx_host::LOCAL_TARGET_HINT
                ),
            );
            dialog.fields.push(field(
                "target",
                "Server",
                target,
                tmuxx_host::LOCAL_TARGET_HINT,
            ));
            dialog.buttons.push(button("connect", "Connect", true));
            dialogs.push(dialog);
        }
        if let Some(confirmation) = &host.confirm {
            dialogs.push(self.zgui_confirmation(host, confirmation));
        }
        if let Some(pending) = host.clipboard_request.front_mut() {
            let mut dialog = dialog("clipboard", "Terminal clipboard access", String::new());
            let (name, can_remember, write) = match &pending.request {
                vt::Effect::ClipboardRead(read) => (&read.name, read.can_remember, false),
                vt::Effect::ClipboardWrite(write) => (&write.name, write.can_remember, true),
                _ => unreachable!(),
            };
            if let Some(pane) = self.panes.get(&pending.pane) {
                dialog
                    .description
                    .push_str(&format!("Terminal: {}\n", pane.title));
            }
            if !name.is_empty() {
                dialog
                    .description
                    .push_str(&format!("Program: {}\n", String::from_utf8_lossy(name)));
            }
            dialog.description.push_str(if write {
                "A terminal program wants to replace the clipboard."
            } else {
                "A terminal program wants to read the clipboard."
            });
            let contents = match &pending.request {
                vt::Effect::ClipboardWrite(write) => {
                    if write.contents.is_empty() {
                        dialog
                            .description
                            .push_str("\nThe clipboard will be cleared.");
                    }
                    Some(&write.contents)
                }
                vt::Effect::ClipboardRead(_) => match &pending.read_result {
                    Some(vt::clipboard::ReadResult::Success(success)) => {
                        for mime in &success.available {
                            dialog
                                .description
                                .push_str(&format!("\n{}", String::from_utf8_lossy(mime)));
                        }
                        Some(&success.contents)
                    }
                    _ => {
                        dialog
                            .description
                            .push_str("\nThe clipboard could not be read.");
                        None
                    }
                },
                _ => unreachable!(),
            };
            if let Some(contents) = contents {
                dialog.preview = pending
                    .preview
                    .native_contents(contents)
                    .iter()
                    .map(|part| PreviewPart {
                        mime: part.mime.clone(),
                        label: part.label.clone(),
                        text: part.text.clone(),
                        image: part.image.clone(),
                    })
                    .collect();
            }
            dialog.buttons = vec![
                button("deny", "Deny", false),
                button("allow", "Allow once", true),
            ];
            if can_remember {
                dialog
                    .buttons
                    .push(button("remember", "Allow for this session", false));
            }
            dialogs.push(dialog);
        }
        if let Some(picker) = &host.layout_picker {
            let mut dialog = dialog(
                "layout",
                "Open saved layout",
                "Open a Rustty or Ghostty saved layout in this instance.".into(),
            );
            dialog.fields.push(field(
                "path",
                "Layout path",
                &picker.path,
                "~/path/to/layout",
            ));
            if let Some(error) = &picker.error {
                dialog.description.push_str(&format!("\n{error}"));
            }
            for (index, choice) in picker.choices.iter().enumerate() {
                let mut choice_button = button(&format!("choice:{index}"), &choice.name, false);
                choice_button.disabled = !choice.available;
                dialog.buttons.push(choice_button);
            }
            dialog.buttons.extend([
                button("browse", "Browse…", false),
                button("open", "Open", true),
            ]);
            dialogs.push(dialog);
        }
        if let Some((id, title, color, error)) = &host.zgui_ui_state.edit_tab {
            let mut dialog = dialog(
                &format!("tab:{id}"),
                "Tab title and color",
                error.clone().unwrap_or_default(),
            );
            dialog
                .fields
                .push(field("title", "Tab title", title, "Automatic title"));
            dialog.fields.push(field(
                "color",
                "Tab color",
                color,
                "#rrggbb, or empty for theme accent",
            ));
            dialog.buttons.push(button("apply", "Apply", true));
            dialogs.push(dialog);
        }
        host.messages_open = !self.errors.is_empty();
        WindowModel {
            background: self.window_config(host.id).background.to_array(),
            foreground: self.window_config(host.id).foreground.to_array(),
            accent: self
                .platform
                .as_ref()
                .and_then(Platform::accent_color)
                .unwrap_or_else(|| self.window_config(host.id).palette[4].to_array()),
            tabs,
            selected_tab,
            breadcrumbs,
            outliner,
            search,
            palette,
            dialogs,
            messages: self.errors.clone(),
            titlebar_inset: if cfg!(target_os = "macos") { 72.0 } else { 0.0 },
        }
    }

    fn zgui_search(&self, host: &Host) -> Option<SearchModel> {
        let pane = self.focused(host.id)?;
        let search = self.panes.get(&pane)?.search.as_ref()?;
        Some(SearchModel {
            pane,
            query: search.query.clone(),
            status: search.status(),
        })
    }

    pub(super) fn zgui_command(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
        command: Command,
    ) {
        match command {
            Command::Action(action) => {
                let focus_terminal = matches!(
                    action,
                    Action::NewTab
                        | Action::NewSplit(_)
                        | Action::ToggleSplitZoom
                        | Action::ToggleQuadrantZoom
                );
                self.action(event_loop, host, action, false);
                host.unfocus_text_input |= focus_terminal;
            }
            Command::PaneAction(pane, action) => {
                if self.tab(host.id).is_some_and(|tab| tab.contains(pane)) {
                    self.focus_pane(host.id, pane);
                    self.action(event_loop, host, action, false);
                    if host.search_focus.is_none() {
                        host.unfocus_text_input = true;
                    }
                }
            }
            Command::RetryPane(pane) => {
                self.failed_panes.remove(&pane);
            }
            Command::SelectTab(id) => {
                self.zgui_select_tab(host, id);
            }
            Command::CloseTab(id) => {
                if self.zgui_select_tab(host, id) {
                    self.action(event_loop, host, Action::CloseTab, false);
                }
            }
            Command::FocusPane(pane) => {
                self.focus_pane(host.id, pane);
            }
            Command::SelectTarget(target) => {
                if outliner::target_window(&self.workspace, target) == Some(host.id) {
                    self.select_outline(host, target, false);
                } else {
                    self.select_outline_target(target);
                }
            }
            Command::PreviewTarget(Some(target)) => {
                if outliner::target_window(&self.workspace, target) == Some(host.id) {
                    self.select_outline(host, target, false);
                } else {
                    self.select_outline_target_with_activation(target, false);
                }
            }
            Command::PreviewTarget(None) => {}
            Command::ExpandTarget(target) => {
                let rows = self.outline_rows();
                if let Some(index) = rows.iter().position(|row| row.target == target) {
                    host.zgui_ui_state.expansion.toggle(&rows, index, false);
                }
            }
            Command::PinTarget(outliner::Target::Pane { pane, .. }) => {
                self.workspace.toggle_pin(pane);
                self.changed();
            }
            Command::PinTarget(_) => {}
            Command::Unpin(pane) => {
                self.workspace.pinned_panes.retain(|id| *id != pane);
                self.changed();
            }
            Command::MovePin(pane, direction) => {
                let pins = &self.workspace.pinned_panes;
                if let Some(index) = pins.iter().position(|id| *id == pane) {
                    let before = if direction < 0 {
                        index.checked_sub(1).map(|index| pins[index])
                    } else {
                        pins.get(index + 2).copied()
                    };
                    if !(direction < 0 && index == 0) && self.workspace.reorder_pin(pane, before) {
                        self.changed();
                    }
                }
            }
            Command::OutlinerQuery(query) => host.zgui_ui_state.query = query,
            Command::OutlinerAgentsOnly(agents_only) => {
                host.zgui_ui_state.agents_only = agents_only
            }
            Command::SearchQuery(id, query) => {
                if let Some(pane) = self.panes.get_mut(&id)
                    && let Some(search) = &mut pane.search
                {
                    search.query = query;
                    if let Ok(mut terminal) = pane.session.terminal() {
                        search.refresh(&mut terminal);
                    }
                }
            }
            Command::SearchFocus(pane) => {
                host.search_focus = pane;
            }
            Command::SearchNext(id, next) => {
                self.search_action(host, id, Action::NavigateSearch { next });
            }
            Command::SearchClose(id) => {
                self.search_action(host, id, Action::EndSearch);
                host.unfocus_text_input = true;
            }
            Command::PaletteChoose(index) => {
                if let Some(event) = host.palette.select(index, true) {
                    self.zgui_palette_event(event_loop, host, event);
                }
            }
            Command::PalettePreview(index) => {
                if let Some(event) = host.palette.select(index, false) {
                    self.zgui_palette_event(event_loop, host, event);
                }
            }
            Command::PaletteClose => {
                host.palette.open = false;
                self.cancel_theme_preview(Some(host));
            }
            Command::DialogField {
                dialog,
                field,
                value,
            } => match dialog.as_str() {
                "connect" if field == "target" => host.tmuxx_connect = Some(value),
                "layout" if field == "path" => {
                    if let Some(picker) = &mut host.layout_picker {
                        picker.path = value;
                    }
                }
                _ if dialog.starts_with("tab:") => {
                    if let Some((_, title, color, _)) = &mut host.zgui_ui_state.edit_tab {
                        match field.as_str() {
                            "title" => *title = value,
                            "color" => *color = value,
                            _ => {}
                        }
                    }
                }
                _ => {}
            },
            Command::DialogCheck {
                dialog,
                check,
                checked,
            } => {
                if dialog == "confirm"
                    && let Some(Confirmation::Quit {
                        save_dump,
                        keep_across_reboots,
                        ..
                    }) = &mut host.confirm
                {
                    match check.as_str() {
                        "save" => *save_dump = checked,
                        "keep" => *keep_across_reboots = checked,
                        _ => {}
                    }
                }
            }
            Command::DialogAction { dialog, action } => {
                self.zgui_dialog_action(event_loop, host, &dialog, &action);
                if !host.modal_input() && host.search_focus.is_none() {
                    host.unfocus_text_input = true;
                }
            }
            Command::DialogClose(dialog) => {
                self.zgui_dialog_action(event_loop, host, &dialog, "cancel");
                if !host.modal_input() && host.search_focus.is_none() {
                    host.unfocus_text_input = true;
                }
            }
            Command::DismissMessages => {
                self.errors.clear();
                host.messages_open = false;
                host.messages_focused = false;
            }
        }
        if !host.palette.open
            && self
                .theme_preview
                .as_ref()
                .is_some_and(|preview| preview.owner == host.id)
        {
            self.cancel_theme_preview(Some(host));
        }
        host.repaint();
    }

    fn zgui_select_tab(&mut self, host: &mut Host, id: Id) -> bool {
        let Some(window) = self.index(host.id) else {
            return false;
        };
        let Some(index) = self.workspace.windows[window]
            .tabs
            .iter()
            .position(|tab| tab.id == id)
        else {
            return false;
        };
        self.workspace.windows[window].active_tab = index;
        let pane = self.workspace.windows[window].tabs[index].focused;
        self.focus_pane(host.id, pane);
        host.search_focus = None;
        host.search_rects.clear();
        host.peek = None;
        host.unfocus_text_input = true;
        true
    }

    fn zgui_palette_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
        event: PaletteEvent,
    ) {
        match event {
            PaletteEvent::Action(action) => {
                self.action(event_loop, host, action, false);
            }
            PaletteEvent::PreviewTheme(theme) => self.preview_theme(host, theme, false),
            PaletteEvent::AcceptTheme(theme) => self.preview_theme(host, theme, true),
        }
    }

    fn zgui_dialog_action(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
        dialog: &str,
        action: &str,
    ) {
        match dialog {
            "import" => self.zgui_import_command(event_loop, action),
            "confirm" => self.zgui_confirm(
                event_loop,
                host,
                action == "approve" || action == "detach",
                action == "detach",
            ),
            "clipboard" => {
                if let Some(pending) = host.clipboard_request.pop_front() {
                    let allow = action == "allow" || action == "remember";
                    self.finish_clipboard(
                        pending.pane,
                        pending.request,
                        allow,
                        action == "remember",
                        pending.read_result,
                    );
                }
            }
            "connect" => {
                if action == "connect" {
                    if let Some(target) = &host.tmuxx_connect {
                        match self.connect_tmuxx(target) {
                            Ok(()) => host.tmuxx_connect = None,
                            Err(error) => self.errors.push(error.to_string()),
                        }
                    }
                } else {
                    host.tmuxx_connect = None;
                }
            }
            "layout" => {
                let command = match action {
                    "browse" => Some(LayoutCommand::Browse),
                    "open" => host
                        .layout_picker
                        .as_ref()
                        .map(|picker| LayoutCommand::Open(PathBuf::from(&picker.path))),
                    "cancel" => Some(LayoutCommand::Close),
                    _ => action
                        .strip_prefix("choice:")
                        .and_then(|index| index.parse::<usize>().ok())
                        .and_then(|index| host.layout_picker.as_ref()?.choices.get(index))
                        .filter(|choice| choice.available)
                        .map(|choice| LayoutCommand::Open(choice.path.clone())),
                };
                if let Some(command) = command {
                    self.layout_command(host, command);
                }
            }
            _ if dialog.starts_with("tab:") => {
                let Some(id) = dialog[4..].parse::<Id>().ok() else {
                    return;
                };
                if action == "edit" {
                    if let Some(tab) = self.tab_by_id(host.id, id) {
                        host.zgui_ui_state.edit_tab = Some((
                            id,
                            tab.title.clone().unwrap_or_default(),
                            tab.color.map_or_else(String::new, |[r, g, b]| {
                                format!("#{r:02x}{g:02x}{b:02x}")
                            }),
                            None,
                        ));
                    }
                } else if action == "apply" {
                    if let Some((id, title, color, _)) = host.zgui_ui_state.edit_tab.take() {
                        match parse_color(&color) {
                            Ok(color) => {
                                if let Some(tab) = self.index(host.id).and_then(|index| {
                                    self.workspace.windows[index]
                                        .tabs
                                        .iter_mut()
                                        .find(|tab| tab.id == id)
                                }) {
                                    tab.title = (!title.is_empty()).then_some(title);
                                    tab.color = color;
                                    self.changed();
                                }
                            }
                            Err(error) => {
                                host.zgui_ui_state.edit_tab =
                                    Some((id, title, color, Some(error.into())))
                            }
                        }
                    }
                } else {
                    host.zgui_ui_state.edit_tab = None;
                }
            }
            _ => {}
        }
    }

    fn tab_by_id(&self, window: Id, id: Id) -> Option<&Tab> {
        self.workspace
            .windows
            .get(self.index(window)?)?
            .tabs
            .iter()
            .find(|tab| tab.id == id)
    }

    fn zgui_confirm(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
        approved: bool,
        detached: bool,
    ) {
        let Some(mut confirmation) = host.confirm.take() else {
            return;
        };
        if !approved {
            return;
        }
        if let Confirmation::Quit {
            save_dump,
            keep_across_reboots,
            error,
            ..
        } = &mut confirmation
        {
            if *save_dump && let Err(failure) = self.dump_session_state(*keep_across_reboots) {
                *error = Some(format!("Could not save session state: {failure}"));
                host.confirm = Some(confirmation);
                return;
            }
            let Confirmation::Quit {
                confirmation: close,
                ..
            } = confirmation
            else {
                unreachable!()
            };
            confirmation = *close;
        }
        if detached && matches!(confirmation, Confirmation::TmuxxClose { .. }) {
            self.close_tmuxx_window(host.id);
            return;
        }
        match confirmation {
            Confirmation::DumpSessions { .. } => {
                if let Err(error) = self.dump_session_state(false) {
                    host.confirm = Some(Confirmation::DumpSessions {
                        error: Some(format!("Could not save session state: {error}")),
                    });
                }
            }
            Confirmation::TmuxxClose { target, session } => {
                self.tmuxx_menu_command(tmuxx_host::MenuCommand::Kill(target, session))
            }
            Confirmation::TmuxxTerminate(command) => self.tmuxx_menu_command(command),
            Confirmation::Close(action) => {
                self.action(event_loop, host, action, true);
            }
            Confirmation::Paste(paste) => self.paste(host, paste, true),
            Confirmation::OpenUrl { uri, cwd, .. } => self.open_url(host, uri, cwd, true),
            Confirmation::Quit { .. } => unreachable!("nested quit confirmation"),
        }
    }

    fn zgui_confirmation(&self, host: &Host, confirmation: &Confirmation) -> DialogModel {
        let (confirmation, save, keep, error) = match confirmation {
            Confirmation::Quit {
                confirmation,
                save_dump,
                keep_across_reboots,
                error,
            } => (
                &**confirmation,
                Some(*save_dump),
                Some(*keep_across_reboots),
                error.as_deref(),
            ),
            confirmation => (
                confirmation,
                None,
                None,
                match confirmation {
                    Confirmation::DumpSessions { error } => error.as_deref(),
                    _ => None,
                },
            ),
        };
        let (title, label, description) = match confirmation {
            Confirmation::DumpSessions { .. } => ("Dump all sessions?", "Dump Sessions", "Save screen contents, scrollback, and working directories from all local sessions in this instance? tmuxx sessions are excluded.\nThis may include personally identifiable information (PII), secrets, and other sensitive data.\nThe dump remains on disk until another Rustty instance successfully imports it. Opening Rustty without importing does not remove it.".into()),
            Confirmation::Close(_) if save.is_some() => ("Save session state before quitting?", "Quit", "Quitting closes local terminals. Persistent tmuxx sessions keep running.".into()),
            Confirmation::Close(_) => ("Close running commands?", "Close", "Closing this terminal will stop its running commands.".into()),
            Confirmation::TmuxxClose { .. } => ("Close tmuxx window?", "Terminate session", if self.tmuxx_window(host.id).is_some_and(|entry| entry.temporary()) { "This temporary session ends when its window closes." } else { "Close the window to keep programs running, or terminate the session." }.into()),
            Confirmation::TmuxxTerminate(command @ (tmuxx_host::MenuCommand::Shutdown(target) | tmuxx_host::MenuCommand::ShutdownIdle(target))) => ("Stop tmuxx server?", "Stop Server", tmuxx_host::shutdown_confirmation(&self.tmuxx, target, matches!(command, tmuxx_host::MenuCommand::ShutdownIdle(_)))),
            Confirmation::TmuxxTerminate(_) => ("Terminate tmuxx programs?", "Terminate", "This stops the selected server programs, including work in other clients.".into()),
            Confirmation::Paste(paste) => ("Paste potentially unsafe text?", "Paste", format!("{}This paste may run commands. Review the text before continuing.", self.panes.get(&paste.pane).map_or_else(String::new, |pane| format!("Terminal: {}\n", pane.title)))),
            Confirmation::OpenUrl { handler, .. } => ("Open link from terminal output?", "Open Link", format!("This link will open in {handler}.\nOnly continue if you recognize and trust the destination.")),
            Confirmation::Quit { .. } => unreachable!("nested quit confirmation"),
        };
        let mut result = dialog("confirm", title, description);
        if let Some(error) = error {
            result.description.push_str(&format!("\n{error}"));
        }
        if let (Some(save), Some(keep)) = (save, keep) {
            result.checks = vec![
                CheckModel {
                    id: "save".into(),
                    label: "Save session state for a future Rustty instance".into(),
                    checked: save,
                },
                CheckModel {
                    id: "keep".into(),
                    label: "Keep session state across reboots".into(),
                    checked: keep,
                },
            ];
        }
        let preview = match confirmation {
            Confirmation::Paste(paste) => {
                let shown = paste.data.len().min(4096);
                Some(format!(
                    "{}{}",
                    String::from_utf8_lossy(&paste.data[..shown]),
                    if shown < paste.data.len() {
                        "\n… preview truncated"
                    } else {
                        ""
                    }
                ))
            }
            Confirmation::OpenUrl { uri, .. } => Some(uri.clone()),
            _ => None,
        };
        if let Some(text) = preview {
            result.preview.push(PreviewPart {
                mime: "text/plain".into(),
                label: String::new(),
                text: Some(text),
                image: None,
            });
        }
        if matches!(confirmation, Confirmation::TmuxxClose { .. })
            && self
                .tmuxx_window(host.id)
                .is_some_and(|entry| !entry.temporary())
        {
            result
                .buttons
                .push(button("detach", "Close window · keep session", false));
        }
        let mut approve = button("approve", label, true);
        approve.destructive = matches!(
            confirmation,
            Confirmation::Close(_)
                | Confirmation::TmuxxClose { .. }
                | Confirmation::TmuxxTerminate(_)
        );
        result.buttons.push(approve);
        result
    }
}

fn menu(label: &str, command: Command) -> MenuItem {
    MenuItem {
        label: label.into(),
        shortcut: String::new(),
        command,
        destructive: label.starts_with("Close"),
        disabled: false,
    }
}
fn logical_bounds(bounds: zgui_ui::Bounds, scale: f32) -> egui::Rect {
    egui::Rect::from_min_size(
        Pos2::new(bounds.origin.x.0 / scale, bounds.origin.y.0 / scale),
        Vec2::new(bounds.size.width.0 / scale, bounds.size.height.0 / scale),
    )
}
fn native_panel_frame(
    cache: &mut BTreeMap<Id, ComposedFrame>,
    pane: &ComposedPane,
    size: [u32; 2],
) -> std::result::Result<Arc<Frame>, rustty_render::ComposeError> {
    if pane.dim.is_none() && pane.transition.is_none() && pane.frame.size == size {
        cache.remove(&pane.id);
        return Ok(Arc::clone(&pane.frame));
    }
    let local = ComposedPane {
        id: pane.id,
        frame: Arc::clone(&pane.frame),
        rect: [0.0, 0.0, pane.rect[2], pane.rect[3]],
        clip: [0.0, 0.0, size[0] as f32, size[1] as f32],
        dim: pane.dim,
        transition: pane.transition,
    };
    if let Some(retained) = cache.get(&pane.id)
        && retained.matches(size, std::slice::from_ref(&local))
    {
        return Ok(Arc::clone(&retained.frame));
    }
    let retained = ComposedFrame::new(size, vec![local])?;
    let frame = Arc::clone(&retained.frame);
    cache.insert(pane.id, retained);
    Ok(frame)
}
fn save_native_capture(host: &Host, path: &Path) -> Result<()> {
    let (size, bytes) = host
        .zgui
        .as_ref()
        .ok_or("zgui is not initialized")?
        .capture()?;
    image::save_buffer(
        path,
        &bytes,
        size[0] as u32,
        size[1] as u32,
        image::ColorType::Rgba8,
    )?;
    Ok(())
}
fn dialog(id: &str, title: &str, description: String) -> DialogModel {
    DialogModel {
        id: id.into(),
        title: title.into(),
        description,
        fields: Vec::new(),
        checks: Vec::new(),
        preview: Vec::new(),
        buttons: vec![button("cancel", "Cancel", false)],
        dismissible: true,
    }
}
fn field(id: &str, label: &str, value: &str, placeholder: &str) -> FieldModel {
    FieldModel {
        id: id.into(),
        label: label.into(),
        value: value.into(),
        placeholder: placeholder.into(),
        multiline: false,
    }
}
fn button(id: &str, label: &str, primary: bool) -> DialogButton {
    DialogButton {
        id: id.into(),
        label: label.into(),
        destructive: false,
        primary,
        disabled: false,
    }
}
fn parse_color(value: &str) -> std::result::Result<Option<[u8; 3]>, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 || !value.is_ascii() {
        return Err("Use a six-digit color such as #6e9bd1, or leave it empty.");
    }
    let number = u32::from_str_radix(value, 16)
        .map_err(|_| "Use a six-digit hexadecimal color such as #6e9bd1.")?;
    Ok(Some([
        (number >> 16) as u8,
        (number >> 8) as u8,
        number as u8,
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tab_colors_accept_theme_defaults_and_reject_invalid_text() {
        assert_eq!(parse_color(""), Ok(None));
        assert_eq!(parse_color(" #6e9bD1 "), Ok(Some([110, 155, 209])));
        for invalid in ["#123", "#GG0000", "🦀12", "1234567"] {
            assert!(parse_color(invalid).is_err());
        }
    }

    #[test]
    fn native_panel_dimming_reuses_unchanged_frames_and_keeps_panels_independent() {
        let frame = Arc::new(Frame::empty([40, 20]));
        let mut cache = BTreeMap::new();
        let mut pane = ComposedPane {
            id: 1,
            frame: Arc::clone(&frame),
            rect: [20.0, 30.0, 40.0, 20.0],
            clip: [20.0, 30.0, 40.0, 20.0],
            dim: None,
            transition: None,
        };
        assert!(Arc::ptr_eq(
            &frame,
            &native_panel_frame(&mut cache, &pane, [40, 20]).unwrap()
        ));
        pane.dim = Some(rustty_render::Color::rgb([20, 30, 40]).opacity(0.2));
        let dimmed = native_panel_frame(&mut cache, &pane, [40, 20]).unwrap();
        assert_eq!(dimmed.quads.len(), 1);
        assert!(Arc::ptr_eq(
            &dimmed,
            &native_panel_frame(&mut cache, &pane, [40, 20]).unwrap()
        ));
        pane.id = 2;
        pane.dim = None;
        assert!(Arc::ptr_eq(
            &frame,
            &native_panel_frame(&mut cache, &pane, [40, 20]).unwrap()
        ));
        assert!(Arc::ptr_eq(&dimmed, &cache[&1].frame));
    }
}
