//! The workspace outliner owns a native utility window, without terminal sessions.
use super::*;

pub(super) struct OutlinerHost {
    pub window: Arc<Window>,
    pub viewport: ViewportId,
    pub egui: egui_winit::State,
    pub visible: bool,
    pub focused: bool,
    pub modifiers: Modifiers,
    pub held_keys: HashSet<PhysicalKey>,
    pub search: outliner_ui::SearchState,
    pub deadline: Option<Instant>,
    pub accesskit_active: bool,
    pub frames: u64,
    pub hovered: Option<outliner::Target>,
    pub check_hover_focus: bool,
    hover_target: Option<outliner::Target>,
    preview: Option<HoverPreview>,
    background: bool,
}

struct HoverPreview {
    active: Option<Id>,
    windows: BTreeMap<Id, PreviewWindow>,
}

struct PreviewWindow {
    active_tab: Id,
    quick_visible: Option<bool>,
    minimized: bool,
    tabs: BTreeMap<Id, PreviewTab>,
}

struct PreviewTab {
    focused: Id,
    zoom: Option<Id>,
    quadrant_zoom: Option<Id>,
    remembered: BTreeMap<Id, Id>,
}

impl PreviewTab {
    fn new(tab: &Tab) -> Self {
        Self {
            focused: tab.focused,
            zoom: tab.zoom,
            quadrant_zoom: tab.quadrant_zoom,
            remembered: tab.remembered.clone(),
        }
    }

    fn restore(&self, tab: &mut Tab) {
        if !tab.contains(self.focused) {
            return;
        }
        tab.focus(self.focused);
        if tab.focused != self.focused {
            return;
        }
        tab.zoom = self.zoom.filter(|id| {
            tab.root
                .node(*id)
                .is_some_and(|node| node.contains(self.focused))
                || *id == self.focused
        });
        tab.quadrant_zoom = self
            .quadrant_zoom
            .filter(|id| tab.quad_path(self.focused).contains(id));
        tab.remembered = self.remembered.clone();
        tab.remembered.retain(|quad, pane| {
            tab.root
                .node(*quad)
                .is_some_and(|node| node.contains(*pane))
        });
    }
}

impl HoverPreview {
    fn capture(&mut self, app: &App, id: Id, target: Option<outliner::Target>) {
        let Some(window) = app.index(id).map(|index| &app.workspace.windows[index]) else {
            return;
        };
        let Some(active) = window.tabs.get(window.active_tab) else {
            return;
        };
        let saved = self.windows.entry(id).or_insert_with(|| {
            let host = app.windows.values().find(|host| host.id == id);
            PreviewWindow {
                active_tab: active.id,
                quick_visible: window.quick.then(|| host.is_some_and(|host| host.visible)),
                minimized: host.is_some_and(|host| host.window.is_minimized() == Some(true)),
                tabs: BTreeMap::new(),
            }
        });
        saved
            .tabs
            .entry(active.id)
            .or_insert_with(|| PreviewTab::new(active));
        let tab = match target {
            Some(outliner::Target::Window(_)) => window.tabs.first(),
            Some(
                outliner::Target::Tab(tab)
                | outliner::Target::Quad { tab, .. }
                | outliner::Target::Pane { tab, .. },
            ) => window.tabs.iter().find(|value| value.id == tab),
            None => None,
        };
        if let Some(tab) = tab {
            saved
                .tabs
                .entry(tab.id)
                .or_insert_with(|| PreviewTab::new(tab));
        }
    }

    fn restore_navigation(&self, workspace: &mut Workspace) {
        for window in &mut workspace.windows {
            let Some(saved) = self.windows.get(&window.id) else {
                continue;
            };
            for tab in &mut window.tabs {
                if let Some(saved) = saved.tabs.get(&tab.id) {
                    saved.restore(tab);
                }
            }
            if let Some(index) = window
                .tabs
                .iter()
                .position(|tab| tab.id == saved.active_tab)
            {
                window.active_tab = index;
            }
        }
    }
}

impl App {
    pub(super) fn commit_outliner_hover(&mut self) -> bool {
        self.outliner
            .as_mut()
            .is_some_and(|host| host.preview.take().is_some())
    }

    pub(super) fn outliner_hover_active(&self) -> bool {
        self.outliner
            .as_ref()
            .is_some_and(|host| host.preview.is_some())
    }

    pub(super) fn saved_outliner_navigation(&self, workspace: &mut Workspace) {
        if let Some(preview) = self
            .outliner
            .as_ref()
            .and_then(|host| host.preview.as_ref())
        {
            preview.restore_navigation(workspace);
        }
    }

    pub(super) fn check_outliner_hover_focus(&mut self) {
        if !self
            .outliner
            .as_mut()
            .is_some_and(|host| std::mem::take(&mut host.check_hover_focus))
        {
            return;
        }
        let focused = self.windows.values().any(|host| {
            if self.background_smoke {
                host.focused
            } else {
                host.window.has_focus()
            }
        }) || self.outliner.as_ref().is_some_and(|host| {
            if self.background_smoke {
                host.focused
            } else {
                host.window.has_focus()
            }
        });
        if !focused {
            self.finish_outliner_hover(false);
            if let Some(host) = &mut self.outliner {
                host.modifiers = Modifiers::default();
                host.egui
                    .egui_input_mut()
                    .events
                    .push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
                host.repaint();
            }
        }
    }

    pub(super) fn update_outliner_hover(
        &mut self,
        hovered: Option<outliner::Target>,
        command: bool,
    ) {
        let Some(mut host) = self.outliner.take() else {
            return;
        };
        host.hovered = hovered;
        if !command {
            self.outliner = Some(host);
            self.finish_outliner_hover(true);
            return;
        }
        if !host.visible || hovered == host.hover_target {
            self.outliner = Some(host);
            return;
        }
        host.hover_target = hovered;
        if let Some(target) = hovered
            && let Some(id) = outliner::target_window(&self.workspace, target)
            && self.windows.values().any(|host| host.id == id)
        {
            let preview = host.preview.get_or_insert_with(|| HoverPreview {
                active: self.active,
                windows: BTreeMap::new(),
            });
            if let Some(previous) = self.active {
                preview.capture(self, previous, None);
            }
            preview.capture(self, id, Some(target));
            // A detached Outliner distinguishes temporary focus from ordinary focus commands.
            self.select_outline_target_with_activation(target, false);
            host.repaint();
        }
        self.outliner = Some(host);
    }

    pub(super) fn finish_outliner_hover(&mut self, reveal: bool) {
        let Some(mut outliner) = self.outliner.take() else {
            return;
        };
        outliner.hover_target = None;
        let Some(preview) = outliner.preview.take() else {
            self.outliner = Some(outliner);
            return;
        };
        preview.restore_navigation(&mut self.workspace);
        self.active = preview
            .active
            .filter(|id| self.index(*id).is_some())
            .or_else(|| self.active.filter(|id| self.index(*id).is_some()));
        for (&id, saved) in &preview.windows {
            let Some(key) = self
                .windows
                .iter()
                .find_map(|(&key, host)| (host.id == id).then_some(key))
            else {
                continue;
            };
            let mut host = self.windows.remove(&key).unwrap();
            if let Some(pane) = self.focused(id) {
                self.focus_pane(id, pane);
            }
            if saved.quick_visible == Some(false) && host.visible {
                self.quick_visible(&mut host, false, false);
            } else if (saved.quick_visible == Some(true) && !host.visible)
                || reveal
                    && self.active == Some(id)
                    && !saved.minimized
                    && saved.quick_visible != Some(false)
            {
                let result = if self.background_smoke {
                    Ok(())
                } else {
                    self.platform.as_ref().map_or(Ok(()), |platform| {
                        platform.preview_window(
                            &host.window,
                            saved.quick_visible.is_some(),
                            self.config(),
                        )
                    })
                };
                if let Err(error) = result {
                    self.errors.push(error);
                } else {
                    host.visible = true;
                    host.occluded = false;
                }
            }
            if saved.minimized && !self.background_smoke {
                host.window.set_minimized(true);
            }
            host.repaint();
            self.windows.insert(key, host);
        }
        // Host focus synchronization must not rewrite the restored per-quad memories.
        preview.restore_navigation(&mut self.workspace);
        self.outliner = Some(outliner);
        self.repaint_outliner();
        self.sync_host_state();
    }
}

impl OutlinerHost {
    pub fn new(
        event_loop: &ActiveEventLoop,
        context: &egui::Context,
        painter: &mut painter::Painter,
        platform: &Platform,
        proxy: EventLoopProxy<Event>,
        background: bool,
    ) -> Result<Self> {
        let attributes = Window::default_attributes()
            .with_title("Outliner")
            .with_visible(false)
            .with_active(false)
            .with_inner_size(LogicalSize::new(340.0, 520.0))
            .with_min_inner_size(LogicalSize::new(220.0, 120.0))
            .with_enabled_buttons(winit::window::WindowButtons::CLOSE)
            .with_window_level(winit::window::WindowLevel::AlwaysOnTop);
        #[cfg(target_os = "macos")]
        let attributes = attributes
            .with_nonactivating_panel(true)
            .with_accepts_first_mouse(true);
        #[cfg(target_os = "windows")]
        let attributes = attributes.with_skip_taskbar(true);
        let window = Arc::new(event_loop.create_window(attributes)?);
        platform.configure_outliner(&window)?;
        let viewport = ViewportId::from_hash_of("rustty-outliner");
        pollster::block_on(painter.set_window(viewport, window.clone()))?;
        let mut egui = egui_winit::State::new(
            context.clone(),
            viewport,
            &*window,
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(painter.max_texture_side()),
        );
        if !background {
            egui.init_accesskit(event_loop, &window, proxy);
        }
        Ok(Self {
            window,
            viewport,
            egui,
            visible: false,
            focused: false,
            modifiers: Modifiers::default(),
            held_keys: HashSet::new(),
            search: outliner_ui::SearchState::default(),
            deadline: None,
            accesskit_active: false,
            frames: 0,
            hovered: None,
            check_hover_focus: false,
            hover_target: None,
            preview: None,
            background,
        })
    }

    pub fn repaint(&self) {
        if self.visible {
            self.window.request_redraw();
        }
    }

    pub fn draw(
        &mut self,
        event_loop: &ActiveEventLoop,
        painter: &mut painter::Painter,
        rows: &[outliner::Row],
        pinned_panes: &[Id],
        accent: [u8; 3],
        capture: Option<&Path>,
    ) -> Result<outliner_ui::Output> {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(outliner_ui::Output::default());
        }
        let mut input = self.egui.take_egui_input(&self.window);
        input.viewport_id = self.viewport;
        input::filter_egui_events(&mut input, true);
        let context = self.egui.egui_ctx().clone();
        if self.accesskit_active {
            context.enable_accesskit();
        } else {
            context.disable_accesskit();
        }
        let mut selection = outliner_ui::Output::default();
        let mut output = context.run_ui(input, |root| {
            egui::CentralPanel::default().show(root, |ui| {
                selection = outliner_ui::contents(ui, rows, pinned_panes, &mut self.search, accent);
            });
        });
        if !self.background {
            self.egui.handle_platform_output_with_event_loop(
                &self.window,
                event_loop,
                output.platform_output,
            );
        }
        let primitives = context.tessellate(output.shapes, output.pixels_per_point);
        if let Some(path) = capture
            && let Some(state) = painter.render_state()
        {
            let result = smoke::capture(
                &state,
                &primitives,
                &output.textures_delta,
                [size.width, size.height],
                output.pixels_per_point,
                path,
            );
            output.textures_delta.clear();
            result?;
        } else if self.background
            && let Some(state) = painter.render_state()
        {
            smoke::submit_offscreen(
                &state,
                &primitives,
                &output.textures_delta,
                [size.width, size.height],
                output.pixels_per_point,
                None,
            );
            output.textures_delta.clear();
        } else {
            painter.paint(
                self.viewport,
                output.pixels_per_point,
                &primitives,
                &mut output.textures_delta,
                capture.is_some(),
                &self.window,
            )?;
            if let Some(path) = capture {
                let mut events = Vec::new();
                painter.handle_screenshots(&mut events);
                let image = events
                    .into_iter()
                    .find_map(|event| match event {
                        egui::Event::Screenshot {
                            viewport_id, image, ..
                        } if viewport_id == self.viewport => Some(image),
                        _ => None,
                    })
                    .ok_or("missing outliner screenshot")?;
                let mut encoder = png::Encoder::new(
                    std::fs::File::create(path)?,
                    image.width() as u32,
                    image.height() as u32,
                );
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                encoder.write_header()?.write_image_data(
                    &image
                        .pixels
                        .iter()
                        .flat_map(|color| color.to_array())
                        .collect::<Vec<_>>(),
                )?;
            }
        }
        self.frames += 1;
        Ok(selection)
    }
}

pub(super) fn hover_modifier(modifiers: winit::keyboard::ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.super_key()
    } else {
        modifiers.control_key()
    }
}

pub(super) fn opens_search(
    key: &winit::keyboard::Key,
    modifiers: winit::keyboard::ModifiersState,
) -> bool {
    matches!(key, winit::keyboard::Key::Character(text) if text == "/")
        && !modifiers.control_key()
        && !modifiers.alt_key()
        && !modifiers.super_key()
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    #[test]
    fn hover_uses_command_on_macos_and_control_on_windows_without_option() {
        assert!(!hover_modifier(ModifiersState::empty()));
        assert!(!hover_modifier(ModifiersState::ALT));
        assert_eq!(
            hover_modifier(ModifiersState::SUPER),
            cfg!(target_os = "macos")
        );
        assert_eq!(
            hover_modifier(ModifiersState::CONTROL),
            !cfg!(target_os = "macos")
        );
    }

    #[test]
    fn hover_snapshot_restores_navigation_without_reverting_content_changes() {
        let mut tab = Tab::new(1, 2, PathBuf::from("/tmp"));
        tab.split(3, 4, Direction::Right, PathBuf::from("/tmp"));
        tab.quad_roots.insert(4);
        tab.focus(2);
        tab.quadrant_zoom = Some(4);
        tab.zoom = Some(2);
        let saved = PreviewTab::new(&tab);
        let remembered = tab.remembered.clone();
        tab.focus(3);
        tab.title = Some("Renamed while previewing".into());
        saved.restore(&mut tab);
        assert_eq!(
            (tab.focused, tab.zoom, tab.quadrant_zoom),
            (2, Some(2), Some(4))
        );
        assert_eq!(tab.remembered, remembered);
        assert_eq!(tab.title.as_deref(), Some("Renamed while previewing"));
    }

    #[test]
    fn hover_snapshot_does_not_restore_a_closed_panel() {
        let mut tab = Tab::new(1, 2, PathBuf::from("/tmp"));
        let saved = PreviewTab::new(&tab);
        tab.split(3, 4, Direction::Right, PathBuf::from("/tmp"));
        tab.focus(3);
        assert!(tab.close(2));
        saved.restore(&mut tab);
        assert_eq!(tab.focused, 3);
        assert!(!tab.contains(2));
    }

    #[test]
    fn search_slash_follows_the_keyboard_layout_without_taking_modified_shortcuts() {
        let slash = Key::Character("/".into());
        for modifiers in [ModifiersState::empty(), ModifiersState::SHIFT] {
            assert!(opens_search(&slash, modifiers));
            for shortcut in [
                ModifiersState::CONTROL,
                ModifiersState::ALT,
                ModifiersState::SUPER,
            ] {
                assert!(!opens_search(&slash, modifiers | shortcut));
            }
        }
        for key in [Key::Character("?".into()), Key::Named(NamedKey::Escape)] {
            assert!(!opens_search(&key, ModifiersState::empty()));
        }
    }
}
