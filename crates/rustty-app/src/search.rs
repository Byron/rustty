//! A pane's transient Find state and its overlay, independent of terminal layout.
use crate::{input, workspace::Id};
use egui::{Color32, Key, Rect, Vec2};
use rustty::{
    config::{Action, Config},
    vt::{
        self,
        search::{Direction, SelectScroll, TerminalSearch},
    },
};
use rustty_render::SearchHighlight;
use std::collections::HashMap;

#[derive(Default)]
pub struct Search {
    pub query: String,
    matches: TerminalSearch,
    generation: Option<u64>,
    highlight: Option<[vt::TrackedPoint; 2]>,
    render_highlights: Vec<SearchHighlight>,
    highlight_viewport: Option<usize>,
}

pub struct OverlayResponse {
    pub response: egui::Response,
    pub changed: bool,
    pub focused: bool,
    pub action: Option<Action>,
}

pub fn field_id(pane: Id) -> egui::Id {
    egui::Id::new(("search", pane))
}

impl Search {
    pub fn refresh(&mut self, terminal: &mut vt::Terminal) {
        let changed = self.matches.set_needle(self.query.as_bytes());
        if !changed && self.generation == Some(terminal.generation) {
            return;
        }
        let highlighted = self.clear_highlight(terminal);
        self.highlight_viewport = None;
        self.matches.run(terminal);
        self.generation = Some(terminal.generation);
        if changed {
            self.matches
                .select(terminal, Direction::Next, SelectScroll::IfNeeded);
        }
        if changed || highlighted {
            self.highlight_selected(terminal);
        }
    }

    pub fn navigate(&mut self, terminal: &mut vt::Terminal, next: bool) -> bool {
        self.refresh(terminal);
        self.clear_highlight(terminal);
        self.highlight_viewport = None;
        let found = self.matches.select(
            terminal,
            if next {
                Direction::Next
            } else {
                Direction::Previous
            },
            SelectScroll::IfNeeded,
        );
        self.highlight_selected(terminal);
        found
    }

    pub fn highlights(&mut self, terminal: &mut vt::Terminal) -> Vec<SearchHighlight> {
        let screen = terminal.screen();
        if self.highlight_viewport == Some(screen.viewport_offset) {
            return self.render_highlights.clone();
        }
        // Navigation can scroll after feeding search, without changing generation.
        self.matches.feed(terminal, false);
        let screen = terminal.screen();
        self.highlight_viewport = Some(screen.viewport_offset);
        self.render_highlights.clear();
        if self.matches.viewport_matches().is_empty() && self.matches.selected_match().is_none() {
            return Vec::new();
        }
        // ponytail: rebuild row indices with highlights; retain an index if
        // very large histories make Find slow.
        let positions: HashMap<_, _> = screen
            .all_rows()
            .enumerate()
            .map(|(index, row)| (row.id, index))
            .collect();
        let top = screen.history_len().saturating_sub(screen.viewport_offset);
        let rows: Vec<_> = screen.viewport().collect();
        for (found, selected) in self
            .matches
            .viewport_matches()
            .iter()
            .copied()
            .map(|found| (found, false))
            .chain(self.matches.selected_match().map(|found| (found, true)))
        {
            let Some((&start, &end)) = positions
                .get(&found.start.row)
                .zip(positions.get(&found.end.row))
            else {
                continue;
            };
            let a = (start, found.start.col);
            let b = (end, found.end.col);
            let (start, end) = (a.min(b), a.max(b));
            for index in start.0.max(top)..=end.0.min(top + rows.len() - 1) {
                let row = rows[index - top];
                self.render_highlights.push(SearchHighlight {
                    row: row.id,
                    columns: (if index == start.0 { start.1 } else { 0 })..=(if index == end.0 {
                        end.1
                    } else {
                        row.cells().len() - 1
                    }),
                    selected,
                });
            }
        }
        self.render_highlights.clone()
    }

    /// Clear only the selection that Find owns; a subsequent mouse selection survives.
    pub fn clear_highlight(&mut self, terminal: &mut vt::Terminal) -> bool {
        let Some([start, end]) = self.highlight.take() else {
            return false;
        };
        let screen = terminal.screen_mut();
        // These pins follow reflow and cannot resolve on another or reset screen.
        let selection = screen
            .resolve(start)
            .zip(screen.resolve(end))
            .map(|(start, end)| vt::Selection {
                start,
                end,
                rectangular: false,
            });
        let highlighted = selection.is_some() && screen.selection == selection;
        if highlighted {
            screen.selection = None;
        }
        terminal.untrack(start);
        terminal.untrack(end);
        highlighted
    }

    fn highlight_selected(&mut self, terminal: &mut vt::Terminal) {
        if let Some(found) = self.matches.selected_match() {
            let selection = vt::Selection {
                start: found.start,
                end: found.end,
                rectangular: false,
            };
            let screen = terminal.screen_mut();
            self.highlight = Some([screen.track(selection.start), screen.track(selection.end)]);
            screen.selection = Some(selection);
        }
    }

    pub fn show(
        &mut self,
        root: &mut egui::Ui,
        pane: Id,
        bounds: Rect,
        selected: bool,
        request_focus: &mut bool,
        config: &Config,
    ) -> OverlayResponse {
        let id = field_id(pane);
        let area_id = egui::Id::new(("search-overlay", pane));
        let layer = egui::LayerId::new(egui::Order::Middle, area_id);
        let owns_focus = |ui: &egui::Ui| {
            ui.memory(|memory| memory.focused())
                .and_then(|id| ui.ctx().read_response(id))
                .is_some_and(|response| response.layer_id == layer)
        };
        let inset = 8.0_f32.min(bounds.width() / 4.0).min(bounds.height() / 4.0);
        let width = (bounds.width() - 2.0 * inset).clamp(1.0, 340.0);
        let mut changed = false;
        let mut focused = false;
        let mut action = None;
        let area = egui::Area::new(area_id)
            .order(egui::Order::Middle)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(bounds.right_top() + Vec2::new(-inset, inset))
            .default_width(width)
            .constrain_to(bounds)
            .enabled(root.is_enabled())
            .fade_in(false)
            .show(root.ctx(), |ui| {
                // Claim focus even during initial sizing so another pane cannot consume
                // queued text, but keep the request for the first visible frame's IME area.
                let focus = ui.is_enabled() && *request_focus;
                if focus && !ui.is_sizing_pass() {
                    *request_focus = false;
                }
                let focused_before = focus || owns_focus(ui);
                let active = selected
                    && ui.input(|input| input.focused)
                    && focused_before
                    && ui.is_enabled();
                ui.multiply_opacity(if active {
                    1.0
                } else {
                    config.search_unfocused_opacity
                });
                let frame = egui::Frame::popup(ui.style())
                    .corner_radius(8)
                    .inner_margin(6.0);
                let margin = frame.total_margin().sum();
                let content_width = (width - margin.x).max(1.0);
                let content_height = (bounds.height() - 2.0 * inset - margin.y).max(1.0);
                frame.show(ui, |ui| {
                    ui.set_width(content_width);
                    egui::ScrollArea::both()
                        .max_width(content_width)
                        .max_height(content_height)
                        .min_scrolled_width(1.0)
                        .min_scrolled_height(1.0)
                        .auto_shrink([false, true])
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                ui.label("Find");
                                ui.spacing_mut().text_edit_width =
                                    (ui.available_size_before_wrap().x - 100.0)
                                        .max(60.0)
                                        .min(ui.available_width());
                                let response = input::text_edit(ui, &mut self.query, id, focus);
                                response.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::TextEdit,
                                        ui.is_enabled(),
                                        "Find in terminal",
                                    )
                                });
                                changed = response.changed();
                                // Plain Enter returns focus to the terminal. Keep Shift+Enter
                                // in Find for previous-match navigation.
                                if ui.is_enabled()
                                    && response.lost_focus()
                                    && ui.input(|input| {
                                        input.key_pressed(Key::Enter) && input.modifiers.shift
                                    })
                                {
                                    action = Some(Action::NavigateSearch { next: false });
                                    response.request_focus();
                                }
                                ui.monospace(format!(
                                    "{}/{}",
                                    self.matches.selected_index().map_or(0, |index| index + 1),
                                    self.matches.total_matches(),
                                ));
                                for (symbol, label, next) in
                                    [("↑", "Next match", true), ("↓", "Previous match", false)]
                                {
                                    let button = ui.add_enabled(
                                        self.matches.total_matches() > 0,
                                        egui::Button::new(symbol),
                                    );
                                    button.widget_info(|| {
                                        egui::WidgetInfo::labeled(
                                            egui::WidgetType::Button,
                                            button.enabled(),
                                            label,
                                        )
                                    });
                                    if button.on_hover_text(label).clicked() {
                                        action = Some(Action::NavigateSearch { next });
                                        response.request_focus();
                                    }
                                }
                                let close = ui.button("×");
                                close.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::Button,
                                        close.enabled(),
                                        "Close Find",
                                    )
                                });
                                if close.on_hover_text("Close Find").clicked() {
                                    action = Some(Action::EndSearch);
                                }
                                focused = owns_focus(ui);
                            });
                        });
                });
                if !selected && config.unfocused_split_opacity < 1.0 {
                    let color = config.unfocused_split_fill.unwrap_or(config.background);
                    let opacity = config.unfocused_split_opacity;
                    // Tint the overlay's colors, preserving alpha: the terminal underneath
                    // already has the same pane dimming and must not be dimmed a second time.
                    ui.ctx().graphics_mut(|graphics| {
                        let shapes = graphics.entry(layer);
                        for index in 0..shapes.next_idx().0 {
                            shapes.mutate_shape(egui::layers::ShapeIdx(index), |shape| {
                                egui::epaint::shape_transform::adjust_colors(
                                    &mut shape.shape,
                                    move |value| {
                                        if *value == Color32::PLACEHOLDER {
                                            return;
                                        }
                                        let alpha = f32::from(value.a()) / 255.0;
                                        let blend = |channel: u8, fill: u8| {
                                            (f32::from(channel) * opacity
                                                + f32::from(fill) * alpha * (1.0 - opacity))
                                                .round()
                                                as u8
                                        };
                                        *value = Color32::from_rgba_premultiplied(
                                            blend(value.r(), color.r),
                                            blend(value.g(), color.g),
                                            blend(value.b(), color.b),
                                            value.a(),
                                        );
                                    },
                                );
                            });
                        }
                    });
                }
                if active != (selected && focused && ui.input(|input| input.focused)) {
                    ui.ctx().request_repaint();
                }
            });
        OverlayResponse {
            response: area.response,
            changed,
            focused,
            action,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Modifiers;

    #[test]
    fn pane_searches_wrap_independently_and_clear_only_their_own_highlights() {
        let mut first = vt::Terminal::new(20, 5, 100);
        first.feed(b"alpha\r\nbeta\r\nalpha\r\nalpha");
        let mut second = vt::Terminal::new(20, 5, 100);
        second.feed(b"beta\r\nother beta");
        let mut a = Search {
            query: "alpha".into(),
            ..Default::default()
        };
        let mut b = Search {
            query: "beta".into(),
            ..Default::default()
        };
        a.refresh(&mut first);
        b.refresh(&mut second);
        assert_eq!(
            (a.matches.total_matches(), b.matches.total_matches()),
            (3, 2)
        );
        assert_eq!(a.matches.selected_index(), Some(0));
        let other_selection = second.screen().selection;
        assert!(a.navigate(&mut first, false));
        assert_eq!(a.matches.selected_index(), Some(2));
        assert!(a.navigate(&mut first, true));
        assert_eq!(a.matches.selected_index(), Some(0));
        assert_eq!(second.screen().selection, other_selection);
        assert_eq!(b.matches.selected_index(), Some(0));

        a.query = "missing".into();
        a.refresh(&mut first);
        assert_eq!(a.matches.total_matches(), 0);
        assert!(first.screen().selection.is_none());
        assert!(!a.navigate(&mut first, false));
        a.query = "alpha".into();
        a.refresh(&mut first);
        a.query.clear();
        a.refresh(&mut first);
        assert_eq!(a.matches.total_matches(), 0);
        assert!(first.screen().selection.is_none());

        a.query = "alpha".into();
        a.refresh(&mut first);
        let point = vt::GridPoint {
            row: first.screen().all_rows().nth(1).unwrap().id,
            col: 0,
        };
        let mouse_selection = Some(vt::Selection {
            start: point,
            end: point,
            rectangular: false,
        });
        first.screen_mut().selection = mouse_selection;
        first.feed(b"!");
        a.refresh(&mut first);
        assert_eq!(first.screen().selection, mouse_selection);
        a.clear_highlight(&mut first);
        assert_eq!(first.screen().selection, mouse_selection);

        a.navigate(&mut first, true);
        first.resize(4, 5);
        a.refresh(&mut first);
        a.clear_highlight(&mut first);
        assert!(first.screen().selection.is_none());

        a.navigate(&mut first, true);
        first.feed(b"\x1b[?1049halt alpha!");
        a.refresh(&mut first);
        a.clear_highlight(&mut first);
        first.feed(b"\x1b[?1049l");
        assert!(first.screen().selection.is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn all_search_matches_render_with_a_distinct_selected_color() {
        use rustty_render::{Color, Paint, Quad, RenderOptions, Renderer};
        use std::ops::Range;

        fn check(
            renderer: &mut Renderer,
            terminal: &mut vt::Terminal,
            search: &mut Search,
            expected: &[(usize, Range<usize>, bool)],
        ) -> rustty_render::Frame {
            search.refresh(terminal);
            let options = RenderOptions {
                cursor_visible: false,
                search_highlights: search.highlights(terminal),
                ..Default::default()
            };
            let frame = renderer
                .prepare(&terminal.screen().snapshot_viewport(), &options)
                .unwrap();
            let colors = [Color::rgb([255, 224, 130]), Color::rgb([242, 165, 126])];
            let actual: Vec<_> = frame
                .quads
                .iter()
                .filter(|quad| quad.paint == Paint::Solid && colors.contains(&quad.color))
                .cloned()
                .collect();
            let metrics = renderer.metrics();
            let expected: Vec<_> = expected
                .iter()
                .flat_map(|(row, columns, selected)| {
                    columns.clone().map(|col| {
                        Quad::solid(
                            [
                                options.padding[0] + col as f32 * metrics.cell_width as f32,
                                options.padding[1] + *row as f32 * metrics.cell_height as f32,
                                metrics.cell_width as f32,
                                metrics.cell_height as f32,
                            ],
                            colors[usize::from(*selected)],
                        )
                    })
                })
                .collect();
            assert_eq!(actual, expected);
            frame
        }

        let mut renderer = Renderer::new(rustty_font::FontConfig::default()).unwrap();
        let mut terminal = vt::Terminal::new(20, 3, 1000);
        terminal.feed("alpha\r\n".repeat(499).as_bytes());
        terminal.feed(b"alpha");
        let mut search = Search {
            query: "alpha".into(),
            ..Default::default()
        };
        let frame = check(
            &mut renderer,
            &mut terminal,
            &mut search,
            &[(0, 0..5, false), (1, 0..5, false), (2, 0..5, true)],
        );
        assert!(
            frame
                .quads
                .iter()
                .filter(|q| q.paint == Paint::Mask)
                .all(|q| q.color == Color::rgb([0; 3]))
        );
        search.navigate(&mut terminal, true);
        check(
            &mut renderer,
            &mut terminal,
            &mut search,
            &[(0, 0..5, false), (1, 0..5, true), (2, 0..5, false)],
        );
        terminal.screen_mut().scroll_viewport(200);
        check(
            &mut renderer,
            &mut terminal,
            &mut search,
            &[(0, 0..5, false), (1, 0..5, false), (2, 0..5, false)],
        );
        terminal.screen_mut().scroll_viewport(-200);
        terminal.feed(b"\r\x1b[2Kother");
        check(
            &mut renderer,
            &mut terminal,
            &mut search,
            &[(0, 0..5, false), (1, 0..5, true)],
        );
        search.query = "missing".into();
        check(&mut renderer, &mut terminal, &mut search, &[]);
        search.query.clear();
        check(&mut renderer, &mut terminal, &mut search, &[]);

        let mut terminal = vt::Terminal::new(4, 2, 100);
        terminal.feed("xx界a\r\nxx界a".as_bytes());
        search.query = "界a".into();
        check(
            &mut renderer,
            &mut terminal,
            &mut search,
            &[(0, 2..4, true), (1, 0..1, true)],
        );
        terminal.screen_mut().scroll_viewport(1);
        check(
            &mut renderer,
            &mut terminal,
            &mut search,
            &[(0, 0..1, false), (1, 2..4, true)],
        );
        search.query = "界".into();
        check(
            &mut renderer,
            &mut terminal,
            &mut search,
            &[(1, 2..4, true)],
        );

        let mut terminal = vt::Terminal::new(6, 1, 0);
        terminal.feed(b"aaaaa");
        search.query = "aaa".into();
        check(
            &mut renderer,
            &mut terminal,
            &mut search,
            &[(0, 0..2, false), (0, 2..5, true)],
        );
        terminal.screen_mut().selection = None;
        check(
            &mut renderer,
            &mut terminal,
            &mut search,
            &[(0, 0..2, false), (0, 2..5, true)],
        );
        search.navigate(&mut terminal, true);
        check(
            &mut renderer,
            &mut terminal,
            &mut search,
            &[(0, 0..1, false), (0, 1..4, true), (0, 4..5, false)],
        );
    }

    struct UiFrame {
        context: egui::Context,
        search: Search,
        config: Config,
        bounds: Rect,
        request_focus: bool,
        selected: bool,
        window_focused: bool,
        time: f64,
    }

    impl Default for UiFrame {
        fn default() -> Self {
            Self {
                context: egui::Context::default(),
                search: Search::default(),
                config: Config::default(),
                bounds: Rect::from_min_size(egui::pos2(20.0, 50.0), Vec2::new(400.0, 300.0)),
                request_focus: true,
                selected: true,
                window_focused: true,
                time: 0.0,
            }
        }
    }

    impl UiFrame {
        fn draw(
            &mut self,
            show: bool,
            mut events: Vec<egui::Event>,
        ) -> (Rect, Option<OverlayResponse>, egui::FullOutput) {
            self.time += 0.1;
            let modifiers = events
                .iter()
                .rev()
                .find_map(|event| match event {
                    egui::Event::Key { modifiers, .. } => Some(*modifiers),
                    _ => None,
                })
                .unwrap_or_default();
            events.insert(0, egui::Event::ModifiersChanged(modifiers));
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(800.0, 600.0),
                )),
                focused: self.window_focused,
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let mut content = Rect::NOTHING;
            let mut overlay = None;
            let mut output = self.context.run_ui(raw, |root| {
                egui::Panel::top("tabs").exact_size(38.0).show(root, |ui| {
                    ui.label("Tabs");
                });
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(root, |ui| {
                        content = ui.max_rect();
                    });
                if show {
                    overlay = Some(self.search.show(
                        root,
                        1,
                        self.bounds,
                        self.selected,
                        &mut self.request_focus,
                        &self.config,
                    ));
                }
            });
            output.textures_delta.clear();
            (content, overlay, output)
        }

        fn open(&mut self) -> OverlayResponse {
            self.draw(true, vec![]);
            self.draw(true, vec![]).1.unwrap()
        }
    }

    fn key(key: Key, shift: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers {
                shift,
                ..Default::default()
            },
        }
    }

    #[test]
    fn find_overlay_keeps_layout_and_clips_interaction_to_its_pane() {
        let mut frame = UiFrame::default();
        let original = frame.draw(false, vec![]).0;
        for size in [
            Vec2::new(400.0, 300.0),
            Vec2::new(180.0, 120.0),
            Vec2::new(45.0, 30.0),
        ] {
            frame.bounds.set_width(size.x);
            frame.bounds.set_height(size.y);
            frame.open();
            let (content, overlay, _) =
                frame.draw(true, vec![egui::Event::Text("long query".repeat(10))]);
            assert_eq!(content, original);
            let overlay = overlay.unwrap();
            assert!(
                frame.bounds.contains_rect(overlay.response.interact_rect),
                "pane {:?}, overlay {:?}",
                frame.bounds,
                overlay.response.interact_rect
            );
            let neighbor = frame.bounds.right_center() + Vec2::new(3.0, 0.0);
            assert!(
                frame
                    .context
                    .layer_id_at(neighbor)
                    .is_none_or(|layer| layer.order == egui::Order::Background)
            );
            assert_eq!(frame.draw(false, vec![]).0, original);
        }
    }

    #[test]
    fn find_controls_fit_and_close_from_normal_and_narrow_panes() {
        for width in [400.0, 180.0] {
            let mut frame = UiFrame::default();
            frame.context.enable_accesskit();
            frame.bounds.set_width(width);
            frame.open();
            let (_, overlay, output) = frame.draw(true, vec![]);
            let bounds = overlay.unwrap().response.interact_rect;
            let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
            let mut close = None;
            for label in ["Next match", "Previous match", "Close Find"] {
                let rect = nodes
                    .iter()
                    .find_map(|(_, node)| (node.label() == Some(label)).then(|| node.bounds()))
                    .flatten()
                    .unwrap_or_else(|| panic!("missing {label}"));
                let rect = Rect::from_min_max(
                    egui::pos2(rect.x0 as f32, rect.y0 as f32),
                    egui::pos2(rect.x1 as f32, rect.y1 as f32),
                );
                assert!(
                    bounds.contains_rect(rect),
                    "{label} {rect:?} outside {bounds:?}"
                );
                close = Some(rect.center());
            }
            let point = close.unwrap();
            let mut action = None;
            for pressed in [true, false] {
                action = frame
                    .draw(
                        true,
                        vec![
                            egui::Event::PointerMoved(point),
                            egui::Event::PointerButton {
                                pos: point,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: Modifiers::default(),
                            },
                        ],
                    )
                    .1
                    .unwrap()
                    .action;
            }
            assert_eq!(action, Some(Action::EndSearch));
        }
    }

    #[test]
    fn find_overlay_routes_composition_and_returns_focus_on_enter() {
        let mut frame = UiFrame::default();
        assert!(frame.open().focused);
        assert!(!frame.request_focus);
        let (_, overlay, output) = frame.draw(true, vec![egui::Event::Text("find ".into())]);
        assert!(overlay.unwrap().changed);
        assert!(output.platform_output.ime.is_some());
        frame.draw(
            true,
            vec![egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "仮".into(),
                active_range_chars: Some(0..1),
            })],
        );
        frame.draw(
            true,
            vec![egui::Event::Ime(egui::ImeEvent::Commit("名".into()))],
        );
        assert_eq!(frame.search.query, "find 名");
        let (_, overlay, _) = frame.draw(true, vec![key(Key::Enter, true)]);
        let overlay = overlay.unwrap();
        assert_eq!(overlay.action, Some(Action::NavigateSearch { next: false }));
        assert!(overlay.focused);

        let (_, overlay, _) = frame.draw(true, vec![key(Key::Enter, false)]);
        let overlay = overlay.unwrap();
        assert_eq!(overlay.action, None);
        assert!(!overlay.focused);
        assert!(!frame.context.text_edit_focused());
        let (_, overlay, _) = frame.draw(true, vec![egui::Event::Text("terminal input".into())]);
        assert!(!overlay.unwrap().focused);
        assert_eq!(frame.search.query, "find 名");
    }

    fn card_fill(output: &egui::FullOutput) -> Color32 {
        fn find(shape: &egui::Shape) -> Option<Color32> {
            match shape {
                egui::Shape::Rect(rect)
                    if rect.corner_radius == egui::CornerRadius::same(8)
                        && rect.blur_width == 0.0
                        && rect.fill != Color32::TRANSPARENT =>
                {
                    Some(rect.fill)
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(find),
                _ => None,
            }
        }
        output
            .shapes
            .iter()
            .find_map(|shape| find(&shape.shape))
            .expect("Find background")
    }

    #[test]
    fn find_opacity_follows_field_and_window_focus_and_preserves_alpha_when_dimming() {
        let mut frame = UiFrame::default();
        frame.open();
        let focused = card_fill(&frame.draw(true, vec![]).2);
        assert_eq!(focused.a(), 255);
        frame.window_focused = false;
        let unfocused = card_fill(&frame.draw(true, vec![]).2);
        assert_eq!(unfocused, focused.gamma_multiply(0.8));
        frame.config.search_unfocused_opacity = 0.4;
        assert_eq!(
            card_fill(&frame.draw(true, vec![]).2),
            focused.gamma_multiply(0.4)
        );
        frame.window_focused = true;
        assert_eq!(card_fill(&frame.draw(true, vec![]).2), focused);
        frame
            .context
            .memory_mut(|memory| memory.request_focus(egui::Id::new("terminal")));
        let blurred = card_fill(&frame.draw(true, vec![]).2);
        assert_eq!(blurred, focused.gamma_multiply(0.4));
        frame.selected = false;
        frame.config.unfocused_split_fill = Some(rustty::config::Rgb::new(100, 80, 60));
        let dimmed = card_fill(&frame.draw(true, vec![]).2);
        assert_eq!(dimmed.a(), blurred.a());
        assert_ne!(dimmed, blurred);
        frame.config.unfocused_split_opacity = 1.0;
        assert_eq!(card_fill(&frame.draw(true, vec![]).2), blurred);
    }
}
