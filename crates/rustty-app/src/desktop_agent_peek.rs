//! Native presentation of the workspace-wide agent picker.
use super::*;
use rustty_app::{
    agent_peek::{self, Filter},
    deck::{PaneView, Target},
};
use winit::keyboard::{Key, KeyCode, NamedKey};

pub(super) const RENDERER_BIT: u64 = 1 << 63;
pub(super) const BACKGROUND_RENDERER_BIT: u64 = 1 << 62;

#[derive(Default)]
pub(super) struct Overlay {
    pub state: agent_peek::State,
    pub(super) prepared: BTreeMap<Id, PreparedPane>,
    pub(super) composed: Option<ComposedFrame>,
    pub footer: Vec<(egui::Rect, Filter)>,
    pub landing: Option<Landing>,
}

pub(super) struct Landing {
    pub target: Target,
    pub from: (egui::Rect, egui::Rect),
    pub destination: egui::Rect,
    pub phase: f32,
    background: Option<Arc<Frame>>,
    tab: Tab,
}

impl Landing {
    fn geometry(&self) -> (egui::Rect, egui::Rect) {
        let mix = smooth(self.phase);
        let interpolate = |from: egui::Rect| {
            egui::Rect::from_min_max(
                from.min.lerp(self.destination.min, mix),
                from.max.lerp(self.destination.max, mix),
            )
        };
        (interpolate(self.from.0), interpolate(self.from.1))
    }
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

impl Overlay {
    pub fn blocks_mouse(&self, position: Pos2) -> bool {
        self.state.active() || self.footer.iter().any(|(rect, _)| rect.contains(position))
    }

    pub fn geometry(&self, card: &agent_peek::Card) -> Option<(egui::Rect, egui::Rect)> {
        if let Some(landing) = &self.landing
            && landing.target == card.target
        {
            return Some(landing.geometry());
        }
        preview_rects(card.rect, self.prepared.get(&card.target.pane)?.frame.size)
    }

    fn opacity(&self, card: &agent_peek::Card) -> f32 {
        self.landing.as_ref().map_or(card.opacity, |landing| {
            if landing.target == card.target {
                1.0
            } else {
                card.opacity * (1.0 - smooth(landing.phase))
            }
        })
    }

    fn chrome_opacity(&self, card: &agent_peek::Card) -> f32 {
        self.opacity(card)
            * self
                .landing
                .as_ref()
                .filter(|landing| landing.target == card.target)
                // Retire the label before its header space closes over the terminal.
                .map_or(1.0, |landing| 1.0 - smooth((landing.phase * 4.0).min(1.0)))
    }

    fn clips(&self, card: &agent_peek::Card, clip: egui::Rect) -> Vec<egui::Rect> {
        if let Some(landing) = &self.landing
            && landing.target != card.target
        {
            return uncovered_rects(clip, landing.geometry().0.expand(3.0));
        }
        vec![clip]
    }

    pub fn hit(&self, mouse: Pos2) -> Option<Target> {
        self.state
            .cards
            .iter()
            .rev()
            .find(|card| {
                card.present
                    && card.opacity > 0.0
                    && self
                        .geometry(card)
                        .is_some_and(|(rect, _)| rect.contains(mouse))
            })
            .map(|card| card.target)
    }
}

// The terminal and its chrome share one fit, including during flyout.
fn preview_rects(slot: egui::Rect, source: [u32; 2]) -> Option<(egui::Rect, egui::Rect)> {
    let available = slot.size() - Vec2::new(8.0, 30.0);
    if source.contains(&0) || available.x <= 0.0 || available.y <= 0.0 {
        return None;
    }
    let source = Vec2::new(source[0] as f32, source[1] as f32);
    let scale = (available.x / source.x).min(available.y / source.y);
    let size = source * scale;
    let outer = egui::Rect::from_center_size(slot.center(), size + Vec2::new(8.0, 30.0));
    let content = egui::Rect::from_min_size(outer.min + Vec2::new(4.0, 26.0), size);
    Some((outer, content))
}

fn bounds(host: &Host) -> egui::Rect {
    egui::Rect::from_min_size(
        Pos2::new(host.content.x, host.content.y),
        Vec2::new(host.content.width, host.content.height - 48.0),
    )
    .shrink(8.0)
}

impl App {
    pub(super) fn agent_peek_view(&self, host: &Host) -> Option<Tab> {
        host.agent_peek
            .landing
            .as_ref()
            .map(|landing| landing.tab.clone())
    }

    pub(super) fn refresh_agent_peek_geometry(&self, host: &mut Host) {
        if let Some(target) = host
            .agent_peek
            .landing
            .as_ref()
            .map(|landing| landing.target)
            && let Some((_, destination)) = self.agent_peek_landing(host, target)
            && let Some(landing) = &mut host.agent_peek.landing
        {
            landing.destination = destination;
        }
    }

    fn agent_peek_landing(&self, host: &Host, target: Target) -> Option<(Tab, egui::Rect)> {
        let window = &self.workspace.windows[self.index(host.id)?];
        let mut tab = window
            .tabs
            .iter()
            .find(|tab| tab.panes.contains_key(&target.pane))?
            .clone();
        set_destination_zoom(&mut tab, target.pane);
        tab.focus(target.pane);
        let scale = host.window.scale_factor() as f32;
        let metrics = host.fonts.metrics();
        let (_, rect) = tab
            .layout_with_minimum(
                host.content,
                false,
                metrics.cell_height as f32 / scale,
                self.split_minimum(host),
            )
            .into_iter()
            .find(|(id, _)| *id == target.pane)?;
        let rect = egui::Rect::from_min_size(
            Pos2::new(rect.x, rect.y),
            Vec2::new(rect.width, rect.height),
        )
        .shrink(1.0);
        let config = self.window_config(host.id);
        let (cols, rows) = pane_grid_size(
            rect.size(),
            scale,
            [metrics.cell_width, metrics.cell_height],
            config,
        );
        let content = egui::Rect::from_min_size(
            rect.min + Vec2::new(config.window_padding_x.start, config.window_padding_y.start),
            Vec2::new(
                f32::from(cols) * metrics.cell_width as f32,
                f32::from(rows) * metrics.cell_height as f32,
            ) / scale,
        );
        Some((tab, content))
    }

    pub(super) fn agent_peek_reports(&self) -> Vec<PaneView> {
        self.workspace
            .windows
            .iter()
            .flat_map(|window| &window.tabs)
            .flat_map(Tab::pane_ids)
            .filter_map(|id| {
                self.panes.get(&id)?.deck_view(
                    id,
                    self.directory_labels.get(&id).map_or("", String::as_str),
                )
            })
            .collect()
    }

    fn toggle_agent_peek(&mut self, host: &mut Host, filter: Filter) {
        let reports = self.agent_peek_reports();
        let bounds = bounds(host);
        host.agent_peek
            .state
            .toggle(filter, &reports, bounds, Instant::now());
        host.search_focus = None;
        host.hovered_link = None;
        self.cancel_selection_drag(host);
        host.repaint();
    }

    pub(super) fn agent_peek_key(&mut self, host: &mut Host, key: &winit::event::KeyEvent) -> bool {
        if input::global_key_is_consumed(
            &mut self.consumed_global_keys,
            key.physical_key,
            key.state,
            key.repeat,
        ) {
            return true;
        }
        if key.state == ElementState::Released {
            return false;
        }
        if !host.peek.is_some_and(|peek| {
            input::chord_held(peek.chord, input::modifiers(host.modifiers.state()))
        }) || host.modal_input()
            || host.composing
        {
            return false;
        }
        let filter = match &key.logical_key {
            Key::Character(text) => agent_peek::FILTERS
                .iter()
                .find(|(_, letter, _)| text.eq_ignore_ascii_case(&letter.to_string()))
                .map(|(filter, _, _)| *filter),
            _ => None,
        };
        let digit = input::peek_digit(key.physical_key)
            .map(|index| index + 1)
            .or_else(|| {
                matches!(
                    key.physical_key,
                    PhysicalKey::Code(KeyCode::Digit0 | KeyCode::Numpad0)
                )
                .then_some(0)
            });
        let escape = key.logical_key == Key::Named(NamedKey::Escape);
        if filter.is_none() && !host.agent_peek.state.active() {
            return false;
        }
        if !key.repeat {
            self.consumed_global_keys.insert(key.physical_key);
            host.sequence.clear();
            host.sequence_len = 0;
            if let Some(filter) = filter {
                self.toggle_agent_peek(host, filter);
            } else if escape {
                if let Some(filter) = host.agent_peek.state.filter {
                    self.toggle_agent_peek(host, filter);
                }
            } else if let Some(digit) = digit {
                host.agent_peek.state.digit(digit, Instant::now());
                host.repaint();
            } else if key.logical_key == Key::Named(NamedKey::Enter) {
                host.agent_peek.state.accept_number(Instant::now());
                host.repaint();
            } else if key.logical_key == Key::Named(NamedKey::Backspace) {
                host.agent_peek.state.number /= 10;
                host.repaint();
            }
        }
        true
    }

    pub(super) fn agent_peek_mouse(
        &mut self,
        host: &mut Host,
        action: vt::MouseAction,
        button: Option<vt::MouseButton>,
    ) -> bool {
        if host.peek.is_none() && !host.agent_peek.state.active() {
            return false;
        }
        if !host.agent_peek.blocks_mouse(host.mouse) {
            return false;
        }
        if action == vt::MouseAction::Press && button == Some(vt::MouseButton::Left) {
            if let Some(filter) = host
                .agent_peek
                .footer
                .iter()
                .find(|(rect, _)| rect.contains(host.mouse))
                .map(|(_, filter)| *filter)
            {
                self.toggle_agent_peek(host, filter);
            } else if let Some(target) = host.agent_peek.hit(host.mouse) {
                host.agent_peek.state.select(target, Instant::now());
            }
        }
        host.repaint();
        true
    }

    pub(super) fn update_agent_peek(
        &mut self,
        host: &mut Host,
        enabled: bool,
        config: &Config,
        now: Instant,
    ) -> bool {
        if host.peek.is_none() {
            host.agent_peek = Overlay::default();
            return false;
        }
        if host.agent_peek.state.active()
            && host.peek.is_some_and(|peek| {
                !input::chord_held(peek.chord, input::modifiers(host.modifiers.state()))
            })
        {
            self.release_peek(host);
            return false;
        }
        let reports = self.agent_peek_reports();
        let duration = |value| if enabled { value } else { Duration::ZERO };
        if let Some((target, started)) = host.agent_peek.state.exiting
            && reports.iter().any(|report| report.target == target)
            && let Some((tab, destination)) = self.agent_peek_landing(host, target)
        {
            if host.agent_peek.landing.is_none()
                && let Some(from) = host
                    .agent_peek
                    .state
                    .cards
                    .iter()
                    .find(|card| card.target == target)
                    .and_then(|card| host.agent_peek.geometry(card))
            {
                host.agent_peek.landing = Some(Landing {
                    target,
                    from,
                    destination,
                    phase: 0.0,
                    background: host
                        .composed
                        .as_ref()
                        .map(|composed| Arc::clone(&composed.frame)),
                    tab: tab.clone(),
                });
            }
            if let Some(landing) = &mut host.agent_peek.landing {
                landing.tab = tab;
                landing.destination = destination;
                let duration = duration(config.agent_peek_select_animation_duration);
                landing.phase = if duration.is_zero() {
                    1.0
                } else {
                    (now.saturating_duration_since(started).as_secs_f32() / duration.as_secs_f32())
                        .min(1.0)
                };
            }
        } else {
            host.agent_peek.landing = None;
        }
        let bounds = bounds(host);
        let (moving, selected) = host.agent_peek.state.update(
            &reports,
            bounds,
            [
                duration(config.agent_peek_animation_duration),
                duration(config.agent_peek_select_animation_duration),
            ],
            now,
        );
        if let Some(target) = selected {
            self.finish_agent_peek(host, target);
        }
        moving
    }

    fn finish_agent_peek(&mut self, host: &mut Host, target: Target) {
        let valid = self
            .agent_peek_reports()
            .iter()
            .any(|report| report.target == target);
        if valid {
            // Restore the source view before revealing the selected pane's parent context.
            if let Some(peek) = host.peek.take()
                && let Some(tab) = self.tab_mut(host.id)
            {
                tab.focused = peek.target;
                tab.zoom = peek.full_zoom.or(peek.quadrant);
                tab.quadrant_zoom = peek.quadrant;
            }
            for window in &mut self.workspace.windows {
                if let Some(tab) = window
                    .tabs
                    .iter_mut()
                    .find(|tab| tab.panes.contains_key(&target.pane))
                {
                    set_destination_zoom(tab, target.pane);
                    break;
                }
            }
            self.reveal_pane_with_host(target.pane, Some(host));
        }
        host.agent_peek = Overlay::default();
        host.repaint();
    }

    /// Prepare at the current PTY grid, regardless of preview geometry.
    pub(super) fn prepare_agent_peek(
        &mut self,
        host: &mut Host,
        now: Instant,
        animation_deadline: &mut Option<Instant>,
    ) -> Result<()> {
        let cards = host.agent_peek.state.cards.clone();
        host.agent_peek
            .prepared
            .retain(|id, _| cards.iter().any(|card| card.target.pane == *id));
        for card in &cards {
            let id = card.target.pane;
            // Keep the floating texture stable when the destination adopts its final PTY grid.
            if host
                .agent_peek
                .landing
                .as_ref()
                .is_some_and(|landing| landing.target == card.target)
                && let Some(prepared) = host.agent_peek.prepared.get_mut(&id)
            {
                if prepared.frame.generation != host.fonts.generation() {
                    prepared.frame = Arc::new(host.fonts.prepare_cached(
                        &prepared.screen,
                        prepared.key.view,
                        &prepared.key.options,
                        &mut prepared.rows,
                    )?);
                    host.pane_prepares += 1;
                }
                continue;
            }
            let Some(pane) = self.panes.get_mut(&id) else {
                continue;
            };
            let mut terminal = pane.session.terminal()?;
            let synchronized = pane.sync_output.update(&mut terminal, now).is_some();
            let now_ms = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            if !synchronized && let Some(next) = terminal.tick_graphics(now_ms) {
                let deadline = now + Duration::from_millis(next.saturating_sub(now_ms));
                *animation_deadline =
                    Some(animation_deadline.map_or(deadline, |old| old.min(deadline)));
            }
            let metrics = host.fonts.metrics();
            let size = [
                u32::from(terminal.cols) * metrics.cell_width,
                u32::from(terminal.rows) * metrics.cell_height,
            ];
            let (foreground, background) = if terminal.modes.dec(5) {
                (terminal.background, terminal.foreground)
            } else {
                (terminal.foreground, terminal.background)
            };
            let options = RenderOptions {
                size,
                padding: [0.0; 2],
                foreground,
                background,
                cursor_color: terminal.cursor_color.unwrap_or(foreground),
                palette: terminal
                    .palette
                    .as_slice()
                    .try_into()
                    .unwrap_or([[0; 3]; 256]),
                focused: false,
                cursor_visible: !pane.exited,
                extra_cursor_visible: !pane.exited,
                blink_visible: true,
                background_opacity: 1.0,
                ..Default::default()
            };
            let rect =
                egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(size[0] as f32, size[1] as f32));
            let key = PaneRenderKey::new(&terminal, options, rect, 1.0);
            if host
                .agent_peek
                .prepared
                .get_mut(&id)
                .is_some_and(|prepared| prepared.matches(&key, &host.fonts))
            {
                continue;
            }
            if synchronized
                && host
                    .agent_peek
                    .prepared
                    .get(&id)
                    .is_some_and(|prepared| prepared.frame.generation == host.fonts.generation())
            {
                continue;
            }
            let screen = pane.session.display_screen(&terminal)?;
            drop(terminal);
            let rows = host
                .agent_peek
                .prepared
                .get_mut(&id)
                .map(|prepared| std::mem::take(&mut prepared.rows))
                .unwrap_or_default();
            host.agent_peek
                .prepared
                .insert(id, PreparedPane::new(key, screen, &mut host.fonts, rows)?);
            host.pane_prepares += 1;
        }
        Ok(())
    }

    pub(super) fn compose_agent_peek(
        &mut self,
        host: &mut Host,
        size: [u32; 2],
        scale: f32,
    ) -> std::result::Result<(), rustty_render::ComposeError> {
        let clip = egui::Rect::from_min_size(
            Pos2::new(host.content.x, host.content.y),
            Vec2::new(host.content.width, host.content.height),
        );
        let mut panes = Vec::new();
        for card in &host.agent_peek.state.cards {
            let Some(prepared) = host.agent_peek.prepared.get(&card.target.pane) else {
                continue;
            };
            let Some((_, rect)) = host.agent_peek.geometry(card) else {
                continue;
            };
            let landing = host
                .agent_peek
                .landing
                .as_ref()
                .filter(|landing| landing.target == card.target);
            let (frame, factor) = if let Some(landing) = landing
                && let Some(native) = host.prepared.get(&card.target.pane)
            {
                let size = landing.destination.size() * scale;
                let mut target = Frame::empty([size.x.round() as u32, size.y.round() as u32]);
                target.append_clipped(
                    &native.frame,
                    native.key.options.padding.map(|value| -value),
                    [0.0, 0.0, size.x, size.y],
                )?;
                let mix = smooth(landing.phase);
                let frame = resize_animation::blend(&prepared.frame, &target, mix, None)?;
                let from_scale =
                    landing.from.1.width() * scale / prepared.frame.size[0].max(1) as f32;
                (Arc::new(frame), from_scale + (1.0 - from_scale) * mix)
            } else {
                (
                    Arc::clone(&prepared.frame),
                    rect.width() * scale / prepared.frame.size[0].max(1) as f32,
                )
            };
            for clip in host.agent_peek.clips(card, rect.intersect(clip)) {
                panes.push(ComposedPane {
                    rect: [
                        rect.left() * scale,
                        rect.top() * scale,
                        frame.size[0] as f32 * factor,
                        frame.size[1] as f32 * factor,
                    ],
                    frame: Arc::clone(&frame),
                    clip: [
                        clip.left() * scale,
                        clip.top() * scale,
                        clip.width().max(0.0) * scale,
                        clip.height().max(0.0) * scale,
                    ],
                    dim: None,
                    transition: Some(ZoomPaint {
                        background: rustty_render::Color::rgb(prepared.key.options.background),
                        frame_background: rustty_render::Color::rgb(
                            prepared.key.options.background,
                        ),
                        opacity: host.agent_peek.opacity(card),
                        background_opacity: host.agent_peek.opacity(card),
                        moving: landing.is_some(),
                    }),
                });
            }
        }
        if host
            .agent_peek
            .composed
            .as_ref()
            .is_some_and(|frame| frame.matches(size, &panes))
        {
            return Ok(());
        }
        let mut frame = Frame::empty(size);
        for pane in &panes {
            let mut scaled = (*pane.frame).clone();
            let factor = pane.rect[2] / scaled.size[0].max(1) as f32;
            for quad in &mut scaled.quads {
                quad.rect = quad.rect.map(|value| value * factor);
                quad.color = quad.color.opacity(pane.transition.unwrap().opacity);
            }
            if let Some(background) = scaled.quads.first_mut() {
                background.rect = [
                    pane.clip[0] - pane.rect[0],
                    pane.clip[1] - pane.rect[1],
                    pane.clip[2],
                    pane.clip[3],
                ];
            }
            frame.append_clipped(&scaled, [pane.rect[0], pane.rect[1]], pane.clip)?;
        }
        host.agent_peek.composed = Some(ComposedFrame {
            panes,
            frame: Arc::new(frame),
        });
        Ok(())
    }

    pub(super) fn paint_agent_peek(
        &mut self,
        ui: &mut egui::Ui,
        host: &mut Host,
        config: &Config,
        accent: Color32,
    ) {
        host.agent_peek.footer.clear();
        if host.peek.is_none() && !host.agent_peek.state.active() {
            return;
        }
        let viewport = egui::Rect::from_min_size(
            Pos2::new(host.content.x, host.content.y),
            Vec2::new(host.content.width, host.content.height),
        );
        let painter = ui.painter().with_clip_rect(viewport);
        if let Some(landing) = &host.agent_peek.landing {
            // Hide the stationary copy until the opaque floating pane reaches it.
            let background = host
                .prepared
                .get(&landing.target.pane)
                .map_or(config.background.to_array(), |prepared| {
                    prepared.key.options.background
                });
            painter.rect_filled(
                landing.destination,
                0.0,
                Color32::from_rgb(background[0], background[1], background[2]),
            );
            if let Some(background) = &landing.background {
                let mut frame = (**background).clone();
                for quad in &mut frame.quads {
                    quad.color = quad.color.opacity(1.0 - smooth(landing.phase));
                }
                let size = host.window.inner_size();
                let scale = host.window.scale_factor() as f32;
                // This retained scene has its own renderer/atlas while the destination prepares new glyphs.
                painter.add(self.painter.terminal_callback(
                    egui::Rect::from_min_size(
                        Pos2::ZERO,
                        Vec2::new(size.width as f32 / scale, size.height as f32 / scale),
                    ),
                    host.id | BACKGROUND_RENDERER_BIT,
                    Arc::new(frame),
                ));
            }
        }
        if host.agent_peek.state.number > 0 {
            painter.text(
                viewport.right_bottom() - Vec2::new(10.0, 38.0),
                egui::Align2::RIGHT_BOTTOM,
                format!("{} ↵", host.agent_peek.state.number),
                egui::FontId::proportional(15.0),
                rgb(config.foreground),
            );
        }
        let dim = config.agent_peek_background_dim
            * host.agent_peek.state.dim
            * host
                .agent_peek
                .landing
                .as_ref()
                .map_or(1.0, |landing| 1.0 - smooth(landing.phase));
        painter.rect_filled(
            viewport,
            0.0,
            Color32::from_black_alpha((255.0 * dim) as u8),
        );
        for card in &host.agent_peek.state.cards {
            let Some((rect, _)) = host.agent_peek.geometry(card) else {
                continue;
            };
            for clip in host.agent_peek.clips(card, viewport) {
                let painter = painter.with_clip_rect(clip);
                painter.rect_filled(
                    rect.expand(3.0),
                    6.0,
                    Color32::from_black_alpha((100.0 * host.agent_peek.chrome_opacity(card)) as u8),
                );
                painter.rect_filled(
                    rect,
                    4.0,
                    rgb(config.background).gamma_multiply(host.agent_peek.opacity(card)),
                );
            }
        }
        if host.agent_peek.state.active()
            && let Some(composed) = &host.agent_peek.composed
        {
            let size = host.window.inner_size();
            let scale = host.window.scale_factor() as f32;
            painter.add(self.painter.terminal_callback(
                egui::Rect::from_min_size(
                    Pos2::ZERO,
                    Vec2::new(size.width as f32 / scale, size.height as f32 / scale),
                ),
                host.id | RENDERER_BIT,
                Arc::clone(&composed.frame),
            ));
        }
        for card in &host.agent_peek.state.cards {
            let Some((rect, _)) = host.agent_peek.geometry(card) else {
                continue;
            };
            for clip in host.agent_peek.clips(card, rect.intersect(viewport)) {
                let painter = painter.with_clip_rect(clip);
                let opacity = host.agent_peek.chrome_opacity(card);
                let color = rgb(config.foreground).gamma_multiply(opacity);
                painter.rect_stroke(
                    rect,
                    4.0,
                    egui::Stroke::new(1.0, accent.gamma_multiply(opacity)),
                    egui::StrokeKind::Inside,
                );
                let number =
                    egui::Rect::from_min_size(rect.min + Vec2::splat(4.0), Vec2::new(22.0, 18.0));
                painter.rect_filled(number, 3.0, accent.gamma_multiply(opacity));
                painter.text(
                    number.center(),
                    egui::Align2::CENTER_CENTER,
                    card.number.to_string(),
                    egui::FontId::proportional(12.0),
                    Color32::WHITE.gamma_multiply(opacity),
                );
                let show_state = host.agent_peek.landing.is_none()
                    && !host
                        .agent_peek
                        .state
                        .filter
                        .is_some_and(|filter| filter == Filter::State(card.state));
                let label_x = if show_state {
                    rustty_app::deck::paint_state(
                        &painter,
                        egui::Rect::from_min_size(
                            number.right_top() + Vec2::new(5.0, 1.0),
                            Vec2::splat(16.0),
                        ),
                        card.state,
                    );
                    number.right() + 26.0
                } else {
                    number.right() + 7.0
                };
                let label = self
                    .directory_labels
                    .get(&card.target.pane)
                    .cloned()
                    .unwrap_or_default();
                let galley = painter.layout(
                    label,
                    egui::FontId::proportional(12.0),
                    color,
                    (rect.right() - label_x - 4.0).max(1.0),
                );
                painter.galley(Pos2::new(label_x, number.top() + 2.0), galley, color);
            }
            ui.interact(
                rect.intersect(viewport),
                egui::Id::new(("agent-peek-card", host.id, card.target.pane)),
                Sense::hover(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        }
        if host.agent_peek.landing.is_some() {
            return;
        }
        let reports = self.agent_peek_reports();
        let filters: Vec<_> = agent_peek::FILTERS
            .into_iter()
            .filter_map(|(filter, key, label)| {
                let count = host.agent_peek.state.count(filter, &reports);
                (count > 0).then_some((filter, key, label, count))
            })
            .collect();
        let width = (filters.len() as f32 * 62.0).min(viewport.width() - 16.0);
        let cell = width / filters.len().max(1) as f32;
        let left = viewport.center().x - width / 2.0;
        for (index, (filter, key, label, count)) in filters.into_iter().enumerate() {
            let rect = egui::Rect::from_min_size(
                Pos2::new(left + index as f32 * cell, viewport.bottom() - 36.0),
                Vec2::new(cell - 4.0, 28.0),
            );
            let selected = host.agent_peek.state.filter == Some(filter);
            painter.rect_filled(
                rect,
                4.0,
                if selected {
                    accent
                } else {
                    rgb(config.background)
                },
            );
            if selected {
                painter.hline(
                    rect.left() + 4.0..=rect.right() - 4.0,
                    rect.bottom() - 2.0,
                    egui::Stroke::new(2.0, Color32::WHITE),
                );
            }
            let icon = egui::Rect::from_min_size(rect.min + Vec2::new(5.0, 6.0), Vec2::splat(16.0));
            match filter {
                Filter::State(state) => rustty_app::deck::paint_state(&painter, icon, state),
                Filter::All => {
                    for offset in [
                        Vec2::ZERO,
                        Vec2::new(9.0, 0.0),
                        Vec2::new(0.0, 9.0),
                        Vec2::splat(9.0),
                    ] {
                        painter.rect_filled(
                            egui::Rect::from_min_size(icon.min + offset, Vec2::splat(6.0)),
                            1.0,
                            rgb(config.foreground),
                        );
                    }
                }
            }
            painter.text(
                Pos2::new(icon.right() + 4.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                count.to_string(),
                egui::FontId::proportional(12.0),
                rgb(config.foreground),
            );
            painter.text(
                Pos2::new(rect.right() - 3.0, rect.top() - 3.0),
                egui::Align2::RIGHT_BOTTOM,
                key.to_string(),
                egui::FontId::proportional(13.0),
                rgb(config.foreground),
            );
            ui.interact(
                rect,
                egui::Id::new(("agent-peek-filter", host.id, index)),
                Sense::hover(),
            )
            .on_hover_text(format!("{key}: {label} · {count} panels"))
            .on_hover_cursor(egui::CursorIcon::PointingHand);
            host.agent_peek.footer.push((rect, filter));
        }
    }
}

fn set_destination_zoom(tab: &mut Tab, pane: Id) {
    let quad = tab
        .quad_path(pane)
        .into_iter()
        .rev()
        .find(|quad| *quad != pane);
    tab.zoom = quad;
    tab.quadrant_zoom = quad;
}
