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
}

impl OutlinerHost {
    pub fn new(
        event_loop: &ActiveEventLoop,
        context: &egui::Context,
        painter: &mut painter::Painter,
        platform: &Platform,
        proxy: EventLoopProxy<Event>,
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
        egui.init_accesskit(event_loop, &window, proxy);
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
        self.egui.handle_platform_output_with_event_loop(
            &self.window,
            event_loop,
            output.platform_output,
        );
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
        Ok(selection)
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
