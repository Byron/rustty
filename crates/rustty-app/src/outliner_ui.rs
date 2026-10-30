//! A movable hierarchy and its compact breadcrumb.
use crate::{
    outliner::{Expansion, Row, Target, Zoom, has_children, pinned_paths},
    presentation::TabAccent,
    workspace::Id,
};
use egui::{Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use std::ops::Range;

const ROW_HEIGHT: f32 = 30.0;
const ICON_SIZE: f32 = 20.0;
const INDENT: f32 = 18.0;
pub const TMUXX_TAG_WIDTH: f32 = 44.0;

pub fn tmuxx_tag_width(ui: &egui::Ui, label: &str, max_width: f32) -> f32 {
    let text_width = ui
        .painter()
        .layout_no_wrap(
            label.to_owned(),
            FontId::proportional(11.0),
            ui.visuals().text_color(),
        )
        .size()
        .x;
    (text_width + 12.0)
        .clamp(TMUXX_TAG_WIDTH, 220.0)
        .min(max_width.max(0.0))
}

pub fn tmuxx_tag(ui: &mut egui::Ui, label: &str, tooltip: &str, width: f32) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 18.0), Sense::hover());
    paint_tmuxx_tag(ui.painter(), rect, ui.visuals().text_color(), label);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, tooltip));
    response.on_hover_text(tooltip);
}

fn paint_tmuxx_tag(painter: &egui::Painter, rect: Rect, color: Color32, label: &str) {
    painter.rect_stroke(
        rect,
        3,
        Stroke::new(1.0, color.gamma_multiply(0.65)),
        egui::StrokeKind::Inside,
    );
    let mut job = egui::text::LayoutJob::simple_singleline(
        label.to_owned(),
        FontId::proportional(11.0),
        color,
    );
    job.wrap.max_width = (rect.width() - 12.0).max(0.0);
    job.wrap.max_rows = 1;
    let galley = painter.layout_job(job);
    painter
        .with_clip_rect(rect.intersect(painter.clip_rect()))
        .galley(rect.center() - galley.size() / 2.0, galley, color);
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub selected: Option<Target>,
    /// Change the focused panel while keeping keyboard input in the Outliner.
    pub preview: Option<Target>,
    pub reorder: Option<(Id, Option<Id>)>,
    pub unpin: Option<Id>,
}

#[derive(Default)]
pub struct SearchState {
    pub open: bool,
    pub query: String,
    selected_pane: Option<Id>,
    request_focus: bool,
}

impl SearchState {
    pub fn start(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected_pane = None;
        self.request_focus = true;
    }

    pub fn focus(&mut self) {
        self.request_focus = self.open;
    }

    fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.selected_pane = None;
        self.request_focus = false;
    }

    fn footer(&mut self, ui: &mut egui::Ui) -> (bool, bool, Vec<egui::Key>) {
        if !self.open {
            return (false, false, Vec::new());
        }
        let mut navigation = Vec::new();
        let accept = ui.input_mut(|input| {
            input.events.retain(|event| {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                    && matches!(key, egui::Key::N | egui::Key::P)
                    && modifiers.matches_logically(egui::Modifiers::CTRL)
                {
                    navigation.push(*key);
                    false
                } else {
                    true
                }
            });
            input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                | input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
                | input.consume_key(egui::Modifiers::CTRL, egui::Key::OpenBracket)
        });
        let available = ui.available_rect_before_wrap();
        let footer = Rect::from_min_max(
            Pos2::new(
                available.left(),
                (available.bottom() - 34.0).max(available.top()),
            ),
            available.max,
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("outliner-search-footer")
                .max_rect(footer),
        );
        child.painter().hline(
            footer.x_range(),
            footer.top(),
            child.visuals().widgets.noninteractive.bg_stroke,
        );
        let field_id = egui::Id::new("outliner-search-query");
        if self.request_focus || accept || !navigation.is_empty() {
            child.memory_mut(|memory| memory.request_focus(field_id));
            if !child.is_sizing_pass() {
                self.request_focus = false;
            }
        }
        let close_rect = Rect::from_min_max(
            Pos2::new(footer.right() - 28.0, footer.top() + 5.0),
            footer.max,
        );
        let field_rect = Rect::from_min_max(
            footer.min + Vec2::new(0.0, 6.0),
            Pos2::new(close_rect.left() - 4.0, footer.bottom()),
        );
        let editor = child.put(
            field_rect,
            egui::TextEdit::singleline(&mut self.query)
                .id(field_id)
                .hint_text("Find panels…"),
        );
        child
            .ctx()
            .accesskit_node_builder(editor.id, |node| node.set_label("Find panels"));
        let close = child.interact(
            close_rect,
            egui::Id::new("outliner-search-close"),
            Sense::click(),
        );
        if close.hovered() || close.has_focus() {
            child
                .painter()
                .rect_filled(close_rect, 3, child.visuals().widgets.hovered.bg_fill);
        }
        child.painter().text(
            close_rect.center(),
            egui::Align2::CENTER_CENTER,
            "×",
            FontId::proportional(18.0),
            child.visuals().text_color(),
        );
        close.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Close panel search")
        });
        if close.clicked() {
            self.close();
            ui.ctx().request_repaint();
        }
        close.on_hover_text("Close search");
        let body_bottom = (footer.top() - 4.0).max(available.top());
        ui.set_clip_rect(Rect::from_min_max(
            ui.clip_rect().min,
            Pos2::new(ui.clip_rect().right(), body_bottom),
        ));
        (accept && self.open, editor.changed(), navigation)
    }
}

pub fn contents(
    ui: &mut egui::Ui,
    rows: &[Row],
    pinned_panes: &[Id],
    search: &mut SearchState,
    platform_accent: [u8; 3],
) -> Output {
    ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
    let (accept, query_changed, navigation) = search.footer(ui);
    let results = search
        .open
        .then(|| crate::outliner::search(rows, &search.query));
    let searching = search.open && !search.query.trim().is_empty();
    let result_groups = search_groups(results.as_deref().unwrap_or_default());
    let panes: Vec<_> = results
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|result| match result.path.last()?.target {
            target @ Target::Pane { pane, .. } => Some((pane, target)),
            _ => None,
        })
        .collect();
    let panes: Vec<_> = result_groups
        .iter()
        .flatten()
        .map(|index| panes[*index])
        .collect();
    let previous = search.selected_pane;
    if search
        .selected_pane
        .is_some_and(|selected| !panes.iter().any(|(pane, _)| *pane == selected))
    {
        search.selected_pane = None;
    }
    if !panes.is_empty() {
        for key in navigation {
            let current = panes
                .iter()
                .position(|(pane, _)| Some(*pane) == search.selected_pane);
            let next = if key == egui::Key::N {
                current.map_or(0, |index| (index + 1) % panes.len())
            } else {
                current.map_or(panes.len() - 1, |index| {
                    (index + panes.len() - 1) % panes.len()
                })
            };
            search.selected_pane = Some(panes[next].0);
        }
    }
    let selection_changed = search.selected_pane != previous;
    let selected_target = panes
        .iter()
        .find(|(pane, _)| Some(*pane) == search.selected_pane)
        .map(|(_, target)| *target);
    let mut output = pins(
        ui,
        &pinned_paths(rows, pinned_panes),
        pinned_panes,
        platform_accent,
    );
    if selection_changed && !accept {
        output.preview = selected_target;
    }
    let expansion_id = ui.id().with("outliner-expansion");
    let mut expansion = ui
        .data(|data| data.get_temp::<Expansion>(expansion_id))
        .unwrap_or_default();
    if searching {
        let clicked = search_results(
            ui,
            results.as_deref().unwrap_or_default(),
            &result_groups,
            search.selected_pane,
            query_changed || selection_changed,
        );
        if let Some(Target::Pane { pane, .. }) = clicked {
            search.selected_pane = Some(pane);
            ui.ctx().request_repaint();
        }
        output.selected = clicked.or(output.selected);
    } else {
        let changed = expansion.sync(rows);
        let visible = expansion.visible(rows);
        let focus = visible
            .iter()
            .rev()
            .copied()
            .find(|&index| rows[index].focused || rows[index].ancestor);
        egui::ScrollArea::vertical()
        .id_salt("outliner-rows")
        .max_height(ui.available_rect_before_wrap().intersect(ui.clip_rect()).height())
        .min_scrolled_height(0.0)
        .auto_shrink([false, false])
        .animated(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for index in visible {
                let row = &rows[index];
                let (_, rect) = ui.allocate_space(Vec2::new(ui.available_width(), ROW_HEIGHT));
                let left = rect.left() + 7.0 + row.depth as f32 * INDENT;
                let branch = has_children(rows, index);
                let selection_rect = if branch {
                    Rect::from_min_max(Pos2::new(left + INDENT, rect.top()), rect.max)
                } else {
                    rect
                };
                let response = ui.interact(
                    selection_rect,
                    egui::Id::new(("outliner-row", row.target)),
                    Sense::click(),
                );
                let accent = TabAccent::new(row.color, platform_accent);
                let accent_color = rgb(accent.background);
                let foreground = if row.focused {
                    rgb(accent.foreground)
                } else {
                    ui.visuals().text_color()
                };
                let fill = if row.focused {
                    accent_color
                } else if row.ancestor {
                    accent_color.gamma_multiply(0.15)
                } else if response.hovered() {
                    ui.visuals().widgets.hovered.bg_fill
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(rect, 4, fill);
                if response.has_focus() {
                    ui.painter().rect_stroke(
                        rect.shrink(0.5),
                        4,
                        Stroke::new(1.0, foreground),
                        egui::StrokeKind::Inside,
                    );
                }
                if row.ancestor || matches!(row.target, Target::Tab(_)) && row.color.is_some() {
                    ui.painter().line_segment(
                        [
                            Pos2::new(left - 4.0, rect.top() + 6.0),
                            Pos2::new(left - 4.0, rect.bottom() - 6.0),
                        ],
                        Stroke::new(2.0, accent_color),
                    );
                }
                if branch {
                    let open = expansion.is_open(row.target);
                    let bounds = Rect::from_center_size(
                        Pos2::new(left + INDENT / 2.0, rect.center().y),
                        Vec2::new(INDENT, ROW_HEIGHT),
                    );
                    let disclosure = ui.interact(
                        bounds,
                        egui::Id::new(("outliner-disclosure", row.target)),
                        Sense::click(),
                    );
                    let icon = Rect::from_center_size(bounds.center(), Vec2::splat(11.0));
                    if disclosure.hovered() || disclosure.has_focus() {
                        ui.painter().rect_filled(
                            bounds.shrink2(Vec2::new(0.0, 4.0)),
                            2,
                            ui.visuals().widgets.hovered.bg_fill,
                        );
                    }
                    ui.painter().rect_stroke(
                        icon,
                        1,
                        Stroke::new(1.0, foreground),
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().hline(
                        icon.x_range().shrink(3.0),
                        icon.center().y,
                        Stroke::new(1.0, foreground),
                    );
                    if !open {
                        ui.painter().vline(
                            icon.center().x,
                            icon.y_range().shrink(3.0),
                            Stroke::new(1.0, foreground),
                        );
                    }
                    let verb = if open { "Collapse" } else { "Expand" };
                    disclosure.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            ui.is_enabled(),
                            format!("{verb} {}", description(row)),
                        )
                    });
                    ui.ctx()
                        .accesskit_node_builder(disclosure.id, |node| node.set_expanded(open));
                    if disclosure.clicked() {
                        expansion.toggle(rows, index, ui.input(|input| input.modifiers.shift));
                        ui.ctx().request_repaint();
                    }
                    disclosure.on_hover_text(format!(
                        "{verb} this level · Shift-click to {verb} all descendants"
                    ));
                }
                let left = left + INDENT + 3.0;
                let text_left = if let Some(index) = row.tab_index {
                    let shortcut = format!("Cmd+{}", index + 1);
                    let galley = ui.painter().layout_no_wrap(
                        shortcut,
                        FontId::proportional(11.0),
                        if row.ancestor {
                            accent_color
                        } else {
                            ui.visuals().weak_text_color()
                        },
                    );
                    let width = galley.size().x;
                    ui.painter().galley(
                        Pos2::new(left, rect.center().y - galley.size().y / 2.0),
                        galley,
                        foreground,
                    );
                    left + width + 10.0
                } else {
                    paint_icon(
                        ui.painter(),
                        Rect::from_center_size(
                            Pos2::new(left + ICON_SIZE / 2.0, rect.center().y),
                            Vec2::splat(ICON_SIZE),
                        ),
                        row,
                        foreground,
                    );
                    left + ICON_SIZE + 7.0
                };
                let pinned =
                    matches!(row.target, Target::Pane { pane, .. } if pinned_panes.contains(&pane));
                let mut label_rect = rect;
                if row.tmuxx.is_some() {
                    let tag = Rect::from_center_size(Pos2::new(rect.right() - TMUXX_TAG_WIDTH / 2.0 - 4.0, rect.center().y), Vec2::new(TMUXX_TAG_WIDTH, 18.0));
                    paint_tmuxx_tag(ui.painter(), tag, foreground, "tmuxx");
                    label_rect.max.x = tag.left() - 6.0;
                }
                if pinned {
                    label_rect.max.x -= 20.0;
                    paint_pin(
                        ui.painter(),
                        Rect::from_center_size(
                            Pos2::new(rect.right() - 11.0, rect.center().y),
                            Vec2::splat(14.0),
                        ),
                        foreground,
                    );
                }
                if !matches!(row.target, Target::Quad { .. }) || !expansion.is_open(row.target) {
                    paint_label(ui, &row.label, label_rect, text_left, foreground, &[]);
                }
                if (changed || selection_changed) && focus == Some(index) {
                    response.scroll_to_me(None);
                }
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::SelectableLabel,
                        ui.is_enabled(),
                        row.focused,
                        if pinned {
                            format!("{} · Pinned", description(row))
                        } else {
                            description(row)
                        },
                    )
                });
                if response.clicked() {
                    response.surrender_focus();
                    output.selected = Some(row.target);
                    if search.open && let Target::Pane { pane, .. } = row.target {
                        search.selected_pane = Some(pane);
                    }
                }
                response.on_hover_text(row.tmuxx.as_ref().map_or_else(|| description(row), |source| format!("{}\n{source}", description(row))));
            }
        });
    }
    if !ui.is_sizing_pass() {
        ui.data_mut(|data| data.insert_temp(expansion_id, expansion));
    }
    if accept {
        output.selected = selected_target;
        search.close();
        ui.ctx().request_repaint();
    }
    if output.selected.is_some() {
        output.preview = None;
    }
    output
}

fn search_groups(results: &[crate::outliner::SearchResult]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut names = std::collections::HashMap::new();
    let mut order = std::collections::HashMap::new();
    for (index, result) in results.iter().enumerate() {
        for row in &result.path {
            order.entry(row.target).or_insert(index);
        }
        let Some(row) = result.path.last() else {
            continue;
        };
        let Target::Pane { tab, .. } = row.target else {
            continue;
        };
        let parent = result.path.iter().rev().nth(1).map(|row| row.target);
        let group = *names
            .entry((parent, tab, row.label.as_str()))
            .or_insert_with(|| {
                groups.push(Vec::new());
                groups.len() - 1
            });
        groups[group].push(index);
    }
    // Rank branches by their best match, keeping each branch's descendants together.
    groups.sort_by_cached_key(|group| {
        results[group[0]]
            .path
            .iter()
            .map(|row| order[&row.target])
            .collect::<Vec<_>>()
    });
    groups
}

fn search_results(
    ui: &mut egui::Ui,
    results: &[crate::outliner::SearchResult],
    groups: &[Vec<usize>],
    selected_pane: Option<Id>,
    changed: bool,
) -> Option<Target> {
    let scroll = egui::ScrollArea::vertical()
        .id_salt("outliner-search-results")
        .max_height(
            ui.available_rect_before_wrap()
                .intersect(ui.clip_rect())
                .height(),
        )
        .min_scrolled_height(0.0)
        .auto_shrink([false, false])
        .animated(false);
    let mut selected = None;
    scroll.show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        let mut ancestors = Vec::new();
        if results.is_empty() {
            ui.weak("No matching panels");
        }
        for group in groups {
            let result = &results[group[0]];
            let Some(row) = result.path.last() else {
                continue;
            };
            let Target::Pane { pane, .. } = row.target else {
                continue;
            };
            let path = &result.path[..result.path.len() - 1];
            let shared = ancestors
                .iter()
                .zip(path)
                .take_while(|(target, row)| **target == row.target)
                .count();
            for (depth, ancestor) in path.iter().enumerate().skip(shared) {
                let (_, rect) = ui.allocate_space(Vec2::new(ui.available_width(), ROW_HEIGHT));
                let left = rect.left() + 7.0 + depth as f32 * INDENT;
                paint_icon(
                    ui.painter(),
                    Rect::from_center_size(
                        Pos2::new(left + ICON_SIZE / 2.0, rect.center().y),
                        Vec2::splat(ICON_SIZE),
                    ),
                    ancestor,
                    ui.visuals().weak_text_color(),
                );
                paint_label(
                    ui,
                    &ancestor.label,
                    rect,
                    left + ICON_SIZE + 6.0,
                    ui.visuals().weak_text_color(),
                    &[],
                );
                ui.interact(rect, ui.id().with(("search-ancestor", ancestor.target)), Sense::hover())
                    .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, description(ancestor)));
            }
            ancestors = path.iter().map(|row| row.target).collect();
            let (_, rect) = ui.allocate_space(Vec2::new(ui.available_width(), ROW_HEIGHT));
            let rect = Rect::from_min_max(
                Pos2::new((rect.left() + path.len() as f32 * INDENT).min(rect.right()), rect.top()),
                rect.max,
            );
            let response = ui.interact(
                rect,
                if group.len() > 1 { egui::Id::new(("outliner-search-group", pane)) } else { egui::Id::new(("outliner-search-result", pane)) },
                Sense::click(),
            );
            let active = group.iter().any(|index| matches!(results[*index].path.last().unwrap().target, Target::Pane { pane, .. } if selected_pane == Some(pane)));
            if active {
                let color = ui.visuals().text_color();
                ui.painter()
                    .rect_filled(rect, 3, color.gamma_multiply(0.12));
                ui.painter().rect_stroke(
                    rect.shrink(0.5),
                    3,
                    Stroke::new(1.0, color),
                    egui::StrokeKind::Inside,
                );
            } else if response.hovered() || response.has_focus() {
                ui.painter()
                    .rect_filled(rect, 3, ui.visuals().widgets.hovered.bg_fill);
            }
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("search-path", pane))
                    .max_rect(rect.shrink2(Vec2::new(3.0, 2.0))),
            );
            let panels: Vec<_> = group.iter().map(|index| results[*index].path.last().unwrap()).collect();
            let clicked = breadcrumb_with_panels(
                &mut child,
                &result.path[result.path.len() - 1..],
                rect.width() - 6.0,
                Some(&result.highlights),
                &panels,
                selected_pane,
                None,
                None,
            );
            if active && changed {
                response.scroll_to_me(None);
            }
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::SelectableLabel,
                    true,
                    active,
                    format!("Search result: {}", row.label),
                )
            });
            if let Some(target) = clicked {
                selected = Some(target);
            } else if response.clicked() {
                response.surrender_focus();
                selected = Some(row.target);
            }
        }
    });
    selected
}

fn pins(
    ui: &mut egui::Ui,
    paths: &[Vec<Row>],
    pinned_panes: &[Id],
    platform_accent: [u8; 3],
) -> Output {
    let mut output = Output::default();
    if paths.is_empty() {
        return output;
    }
    let panes: Vec<_> = paths
        .iter()
        .filter_map(|path| match path.last()?.target {
            Target::Pane { pane, .. } => Some(pane),
            _ => None,
        })
        .collect();
    ui.label(egui::RichText::new("Pinned").small().weak());
    egui::ScrollArea::vertical()
        .id_salt("outliner-pins")
        .max_height(
            (ui.available_rect_before_wrap()
                .intersect(ui.clip_rect())
                .height()
                * 0.35)
                .clamp(ROW_HEIGHT, 180.0),
        )
        .min_scrolled_height(ROW_HEIGHT)
        .auto_shrink([false, true])
        .animated(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let number_width = ui
                .painter()
                .layout_no_wrap(
                    pinned_panes.len().to_string(),
                    FontId::monospace(11.0),
                    ui.visuals().weak_text_color(),
                )
                .size()
                .x
                + 6.0;
            let mut pin_numbers = pinned_panes.iter().enumerate();
            for (index, (&pane, path)) in panes.iter().zip(paths).enumerate() {
                let (number, _) = pin_numbers
                    .find(|(_, id)| **id == pane)
                    .expect("pinned paths follow pin order");
                let path = match path.first().map(|row| row.target) {
                    Some(Target::Window(_)) => &path[1..],
                    _ => path.as_slice(),
                };
                let (_, rect) = ui.allocate_space(Vec2::new(ui.available_width(), ROW_HEIGHT));
                let row = path.last().unwrap();
                let quad_focused = path
                    .iter()
                    .any(|row| row.ancestor && matches!(row.target, Target::Quad { .. }));
                let accent = TabAccent::new(row.color, platform_accent);
                // Reserve the background behind the controls until breadcrumb layout locates the quad.
                let quad_highlight = (quad_focused && !row.focused)
                    .then(|| (rect, accent, ui.painter().add(egui::Shape::Noop)));
                let foreground = if row.focused {
                    ui.painter().rect_filled(rect, 4, rgb(accent.background));
                    rgb(accent.foreground)
                } else if quad_focused {
                    rgb(accent.foreground)
                } else {
                    ui.visuals().text_color()
                };
                let weak_foreground = if row.focused || quad_focused {
                    foreground
                } else {
                    ui.visuals().weak_text_color()
                };
                let number_rect =
                    Rect::from_min_size(rect.min, Vec2::new(number_width, ROW_HEIGHT));
                let number_handle = ui.interact(
                    number_rect,
                    egui::Id::new(("outliner-pin-number", pane)),
                    Sense::drag(),
                );
                number_handle.dnd_set_drag_payload(pane);
                if number_handle.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                }
                if number_handle.hovered() || number_handle.has_focus() || number_handle.dragged() {
                    ui.painter().rect_stroke(
                        number_rect.shrink(2.0),
                        3,
                        Stroke::new(1.0, weak_foreground),
                        egui::StrokeKind::Inside,
                    );
                }
                ui.painter().text(
                    Pos2::new(rect.left() + number_width - 6.0, rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    (number + 1).to_string(),
                    FontId::monospace(11.0),
                    weak_foreground,
                );
                let handle_rect = Rect::from_min_size(
                    rect.min + Vec2::new(number_width, 0.0),
                    Vec2::new(22.0, ROW_HEIGHT),
                );
                let handle = ui.interact(
                    handle_rect,
                    egui::Id::new(("outliner-pin", pane)),
                    Sense::click(),
                );
                if handle.hovered() || handle.has_focus() {
                    ui.painter().rect_stroke(
                        handle_rect.shrink(2.0),
                        3,
                        Stroke::new(1.0, weak_foreground),
                        egui::StrokeKind::Inside,
                    );
                }
                paint_pin(
                    ui.painter(),
                    Rect::from_center_size(handle_rect.center(), Vec2::splat(16.0)),
                    foreground,
                );
                let label = format!("Unpin panel: {}", path.last().unwrap().label);
                handle.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), &label)
                });
                ui.ctx().accesskit_node_builder(handle.id, |node| {
                    node.set_description(format!("Pin {}", number + 1));
                });
                if handle.clicked() {
                    handle.surrender_focus();
                    output.unpin = Some(pane);
                }
                handle
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(label);
                number_handle.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        ui.is_enabled(),
                        format!("Reorder pinned panel: {}", path.last().unwrap().label),
                    )
                });
                ui.ctx().accesskit_node_builder(number_handle.id, |node| {
                    node.set_description(format!("Pin {}", number + 1));
                    if index > 0 {
                        node.add_action(egui::accesskit::Action::Decrement);
                    }
                    if index + 1 < panes.len() {
                        node.add_action(egui::accesskit::Action::Increment);
                    }
                });
                let has_focus = number_handle.has_focus();
                let (up, down) = ui.input_mut(|input| {
                    (
                        input.has_accesskit_action_request(
                            number_handle.id,
                            egui::accesskit::Action::Decrement,
                        ) || has_focus
                            && input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowUp),
                        input.has_accesskit_action_request(
                            number_handle.id,
                            egui::accesskit::Action::Increment,
                        ) || has_focus
                            && input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowDown),
                    )
                });
                if up && index > 0 {
                    output.reorder = Some((pane, Some(panes[index - 1])));
                } else if down && index + 1 < panes.len() {
                    output.reorder = Some((pane, panes.get(index + 2).copied()));
                }
                number_handle.on_hover_text("Drag to reorder · Alt+↑/↓ to move");

                let crumb_rect = Rect::from_min_max(
                    Pos2::new(handle_rect.right() + 3.0, rect.top() + 2.0),
                    rect.max,
                );
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt(("pinned-path", pane))
                        .max_rect(crumb_rect),
                );
                if row.focused {
                    child.visuals_mut().override_text_color = Some(foreground);
                    child.visuals_mut().weak_text_color = Some(foreground);
                }
                output.selected = breadcrumb_with_pin_highlight(
                    &mut child,
                    path,
                    crumb_rect.width(),
                    quad_highlight,
                )
                .or(output.selected);
                let blank =
                    Rect::from_min_max(Pos2::new(child.min_rect().right(), rect.top()), rect.max);
                if blank.width() > 0.0 {
                    let response =
                        ui.interact(blank, ui.id().with(("pin-tail", pane)), Sense::click());
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            ui.is_enabled(),
                            format!("Pinned panel: {}", path.last().unwrap().label),
                        )
                    });
                    if response.clicked() {
                        output.selected = path.last().map(|row| row.target);
                    }
                }
                let drop = ui.interact(
                    rect,
                    egui::Id::new(("outliner-pin-drop", pane)),
                    Sense::hover(),
                );
                if let Some(dragged) = drop.dnd_hover_payload::<Id>()
                    && *dragged != pane
                {
                    let after = ui.input(|input| {
                        input
                            .pointer
                            .interact_pos()
                            .is_some_and(|point| point.y > rect.center().y)
                    });
                    let before = if after {
                        panes.get(index + 1).copied()
                    } else {
                        Some(pane)
                    };
                    ui.painter().hline(
                        rect.x_range(),
                        if after { rect.bottom() } else { rect.top() },
                        Stroke::new(2.0, ui.visuals().text_color()),
                    );
                    if let Some(dragged) = drop.dnd_release_payload::<Id>() {
                        output.reorder = Some((*dragged, before));
                    }
                }
            }
        });
    ui.separator();
    output
}

/// A neutral pushpin shared by the outliner and terminal directory badges.
pub fn paint_pin(painter: &egui::Painter, rect: Rect, color: Color32) {
    let point = |x: f32, y: f32| rect.min + Vec2::new(x * rect.width(), y * rect.height());
    painter.line_segment(
        [point(0.5, 0.55), point(0.5, 0.95)],
        Stroke::new(1.2, color),
    );
    painter.rect_filled(
        Rect::from_min_max(point(0.3, 0.1), point(0.7, 0.25)),
        0,
        color,
    );
    painter.rect_filled(
        Rect::from_min_max(point(0.4, 0.25), point(0.6, 0.45)),
        0,
        color,
    );
    painter.add(egui::Shape::convex_polygon(
        vec![
            point(0.4, 0.4),
            point(0.6, 0.4),
            point(0.78, 0.6),
            point(0.22, 0.6),
        ],
        color,
        Stroke::NONE,
    ));
}

/// Draw the window header breadcrumb; width is bounded even for long pane labels.
pub fn breadcrumb(ui: &mut egui::Ui, rows: &[Row], max_width: f32) -> Option<Target> {
    ui.scope(|ui| {
        let accent = ui.visuals().selection.bg_fill;
        let background = ui.painter().add(egui::Shape::Noop);
        let selected =
            breadcrumb_with_panels(ui, rows, max_width, None, &[], None, None, Some(accent));
        ui.painter().set(
            background,
            egui::Shape::rect_filled(ui.min_rect(), 4, accent.gamma_multiply(0.15)),
        );
        selected
    })
    .inner
}

fn breadcrumb_with_pin_highlight(
    ui: &mut egui::Ui,
    rows: &[Row],
    max_width: f32,
    pin_highlight: Option<(Rect, TabAccent, egui::layers::ShapeIdx)>,
) -> Option<Target> {
    breadcrumb_with_panels(ui, rows, max_width, None, &[], None, pin_highlight, None)
}

#[allow(clippy::too_many_arguments)]
fn breadcrumb_with_panels(
    ui: &mut egui::Ui,
    rows: &[Row],
    max_width: f32,
    highlights: Option<&[Range<usize>]>,
    panels: &[&Row],
    selected_pane: Option<Id>,
    pin_highlight: Option<(Rect, TabAccent, egui::layers::ShapeIdx)>,
    label_color: Option<Color32>,
) -> Option<Target> {
    if rows.is_empty() || max_width <= 0.0 {
        return None;
    }
    let pin_highlight = pin_highlight.and_then(|(rect, accent, shape)| {
        rows.iter()
            .position(|row| row.ancestor && matches!(row.target, Target::Quad { .. }))
            .map(|index| (index, rect, accent, shape))
    });
    let foreground = ui.visuals().text_color();
    let labels: Vec<_> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let repeated = |label: &str| {
                rows[index + 1..]
                    .iter()
                    .any(|next| !matches!(next.target, Target::Quad { .. }) && next.label == label)
            };
            let label = if matches!(row.target, Target::Quad { .. })
                || row.tab_index.is_none() && repeated(&row.label)
            {
                ""
            } else if matches!(row.target, Target::Window(_)) {
                row.label
                    .split_once(" · ")
                    .filter(|(_, suffix)| repeated(suffix))
                    .map_or(row.label.as_str(), |(window, _)| window)
            } else {
                &row.label
            };
            label.to_owned()
        })
        .collect();
    let mut widths: Vec<f32> = rows
        .iter()
        .zip(&labels)
        .enumerate()
        .map(|(index, (row, label))| {
            ui.painter()
                .layout_no_wrap(label.clone(), FontId::proportional(13.0), foreground)
                .size()
                .x
                + if row.tab_index.is_some() {
                    9.0
                } else if label.is_empty() {
                    ICON_SIZE + 6.0
                } else {
                    ICON_SIZE + 10.0
                }
                + if index + 1 == rows.len() {
                    panels.len().saturating_sub(1) as f32 * (ICON_SIZE + 4.0)
                        + if panels.len() > 1 { 3.0 } else { 0.0 }
                } else {
                    0.0
                }
        })
        .collect();
    let separator = 10.0;
    let width = (widths.iter().sum::<f32>() + separator * (rows.len() - 1) as f32)
        .min(max_width)
        .min(ui.available_width());
    let (_, bounds) = ui.allocate_space(Vec2::new(width, 26.0));
    let separator = separator.min(width / rows.len() as f32);
    let available = (width - separator * (rows.len() - 1) as f32).max(0.0);
    // Give short names their space before sharing the rest among truncated names.
    let mut order: Vec<_> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| widths[a].total_cmp(&widths[b]));
    let mut remaining = available;
    for (slot, index) in order.into_iter().enumerate() {
        widths[index] = widths[index].min(remaining / (rows.len() - slot) as f32);
        remaining -= widths[index];
    }
    let mut x = bounds.left();
    let mut selected = None;
    for (index, row) in rows.iter().enumerate() {
        let foreground = pin_highlight
            .filter(|(quad, ..)| index <= *quad)
            .map_or(foreground, |(_, _, accent, _)| rgb(accent.foreground));
        let weak_foreground = pin_highlight
            .filter(|(quad, ..)| index <= *quad)
            .map_or(ui.visuals().weak_text_color(), |(_, _, accent, _)| {
                rgb(accent.foreground)
            });
        if index > 0 {
            ui.painter()
                .with_clip_rect(bounds.intersect(ui.clip_rect()))
                .text(
                    Pos2::new(x + separator / 2.0, bounds.center().y),
                    egui::Align2::CENTER_CENTER,
                    "›",
                    FontId::proportional(16.0),
                    weak_foreground,
                );
            x += separator;
        }
        let width = widths[index];
        let rect = Rect::from_min_size(
            Pos2::new(x, bounds.top()),
            Vec2::new(width, bounds.height()),
        );
        if let Some((quad, mut highlight_rect, accent, shape)) = pin_highlight
            && index == quad
        {
            highlight_rect.max.x = rect.right();
            ui.painter().set(
                shape,
                egui::Shape::rect_filled(highlight_rect, 4, rgb(accent.background)),
            );
        }
        let response = ui.interact(
            rect,
            ui.id().with(("breadcrumb", row.target)),
            if highlights.is_some() {
                Sense::hover()
            } else {
                Sense::click()
            },
        );
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        if response.hovered() || response.has_focus() {
            painter.rect_stroke(
                rect.shrink(0.5),
                3,
                Stroke::new(1.0, weak_foreground),
                egui::StrokeKind::Inside,
            );
        }
        let text_left = if index + 1 == rows.len() && panels.len() > 1 {
            for (slot, panel) in panels.iter().enumerate() {
                let Target::Pane { pane, .. } = panel.target else {
                    continue;
                };
                let icon = Rect::from_min_size(
                    Pos2::new(x + 3.0 + slot as f32 * (ICON_SIZE + 4.0), rect.top()),
                    Vec2::new(ICON_SIZE + 4.0, rect.height()),
                );
                let response = ui.interact(
                    icon,
                    egui::Id::new(("outliner-search-result", pane)),
                    Sense::click(),
                );
                paint_icon(
                    &painter,
                    Rect::from_center_size(icon.center(), Vec2::splat(ICON_SIZE)),
                    panel,
                    foreground,
                );
                if panel.focused || selected_pane == Some(pane) {
                    painter.rect_stroke(
                        icon.shrink(0.5),
                        3,
                        Stroke::new(1.0, foreground),
                        egui::StrokeKind::Inside,
                    );
                }
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::SelectableLabel,
                        ui.is_enabled(),
                        selected_pane == Some(pane),
                        description(panel),
                    )
                });
                if response.clicked() {
                    response.surrender_focus();
                    selected = Some(panel.target);
                }
                response.on_hover_text(description(panel));
            }
            x + 3.0 + panels.len() as f32 * (ICON_SIZE + 4.0)
        } else if row.tab_index.is_some() {
            x + 3.0
        } else {
            paint_icon(
                &painter,
                Rect::from_center_size(
                    Pos2::new(x + ICON_SIZE / 2.0 + 3.0, rect.center().y),
                    Vec2::splat(ICON_SIZE),
                ),
                row,
                foreground,
            );
            x + ICON_SIZE + 4.0
        };
        paint_label(
            ui,
            &labels[index],
            rect,
            text_left,
            label_color.unwrap_or(foreground),
            if index + 1 == rows.len() {
                highlights.unwrap_or_default()
            } else {
                &[]
            },
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                if highlights.is_some() {
                    egui::WidgetType::Label
                } else {
                    egui::WidgetType::Button
                },
                ui.is_enabled(),
                description(row),
            )
        });
        if response.clicked() {
            response.surrender_focus();
            selected = Some(row.target);
        }
        response.on_hover_text(description(row));
        x += width;
    }
    selected
}

fn paint_label(
    ui: &egui::Ui,
    label: &str,
    rect: Rect,
    left: f32,
    foreground: Color32,
    highlights: &[Range<usize>],
) {
    let width = (rect.right() - left - 6.0).max(0.0);
    if width == 0.0 {
        return;
    }
    let mut job = egui::text::LayoutJob::simple_singleline(
        label.to_owned(),
        FontId::proportional(13.0),
        foreground,
    );
    if !highlights.is_empty() {
        let format = job.sections[0].format.clone();
        job.sections.clear();
        let mut start = 0;
        for highlight in highlights {
            if start < highlight.start {
                job.sections.push(egui::text::LayoutSection {
                    leading_space: 0.0,
                    byte_range: egui::text::ByteIndex(start)
                        ..egui::text::ByteIndex(highlight.start),
                    format: format.clone(),
                });
            }
            job.sections.push(egui::text::LayoutSection {
                leading_space: 0.0,
                byte_range: egui::text::ByteIndex(highlight.start)
                    ..egui::text::ByteIndex(highlight.end),
                format: egui::TextFormat {
                    background: foreground.gamma_multiply(0.15),
                    underline: Stroke::new(1.0, foreground),
                    ..format.clone()
                },
            });
            start = highlight.end;
        }
        if start < label.len() {
            job.sections.push(egui::text::LayoutSection {
                leading_space: 0.0,
                byte_range: egui::text::ByteIndex(start)..egui::text::ByteIndex(label.len()),
                format,
            });
        }
    }
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    let galley = ui.painter().layout_job(job);
    ui.painter()
        .with_clip_rect(rect.intersect(ui.clip_rect()))
        .galley(
            Pos2::new(left, rect.center().y - galley.size().y / 2.0),
            galley,
            foreground,
        );
}

fn paint_icon(painter: &egui::Painter, rect: Rect, row: &Row, color: Color32) {
    if matches!(row.target, Target::Window(_)) {
        let bounds = rect.shrink2(Vec2::new(1.5, 3.0));
        painter.rect_stroke(bounds, 1, Stroke::new(1.0, color), egui::StrokeKind::Inside);
        painter.rect_filled(
            Rect::from_min_size(bounds.min, Vec2::new(bounds.width(), 4.0)),
            1,
            color,
        );
        return;
    }
    let zoomed = row.zoom != Zoom::None;
    let outer = rect.shrink(3.0);
    let quad = matches!(row.target, Target::Quad { .. });
    painter.rect_stroke(outer, 1, Stroke::new(1.0, color), egui::StrokeKind::Inside);
    let inner = outer.shrink(1.5);
    let position = Rect::from_min_size(
        inner.min + Vec2::new(row.rect.x * inner.width(), row.rect.y * inner.height()),
        Vec2::new(
            row.rect.width * inner.width(),
            row.rect.height * inner.height(),
        ),
    );
    if quad {
        painter.rect_stroke(
            position,
            0,
            Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
    } else {
        painter.rect_filled(position, 0, color);
    }
    if zoomed {
        // Corner brackets mark zoom without hiding the pane's original position.
        for (corner, direction) in [
            (rect.left_top(), Vec2::new(1.0, 1.0)),
            (rect.right_top(), Vec2::new(-1.0, 1.0)),
            (rect.left_bottom(), Vec2::new(1.0, -1.0)),
            (rect.right_bottom(), Vec2::new(-1.0, -1.0)),
        ] {
            painter.line_segment(
                [corner, corner + Vec2::new(direction.x * 4.0, 0.0)],
                Stroke::new(1.5, color),
            );
            painter.line_segment(
                [corner, corner + Vec2::new(0.0, direction.y * 4.0)],
                Stroke::new(1.5, color),
            );
        }
    }
}

fn description(row: &Row) -> String {
    let kind = match row.target {
        Target::Window(_) => "Window",
        Target::Tab(_) => "Tab",
        Target::Quad { .. } => "Quad",
        Target::Pane { .. } => "Panel",
    };
    let mut description = format!("{kind}: {}", row.label);
    if row.tmuxx.is_some() {
        description.push_str(" · tmuxx");
    }
    if let Some(index) = row.tab_index {
        description.push_str(&format!(" · Cmd+{}", index + 1));
    }
    match row.zoom {
        Zoom::None => {}
        Zoom::Quad => description.push_str(" · Quad-zoom"),
        Zoom::Pane => description.push_str(" · Zoom"),
        Zoom::PaneInQuad => description.push_str(" · Zoom within quad-zoom"),
    }
    if row.focused {
        description.push_str(" · Focused");
    } else if row.ancestor {
        description.push_str(" · Contains focused panel");
    }
    description
}

fn rgb([r, g, b]: [u8; 3]) -> Color32 {
    Color32::from_rgb(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_icons_match_panel_geometry_without_fills_or_an_extra_square() {
        let context = egui::Context::default();
        let icon = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::splat(ICON_SIZE));
        let mut row = rows()[2].clone();
        for position in [
            crate::workspace::Rect::UNIT,
            crate::workspace::Rect {
                x: 0.5,
                width: 0.5,
                ..crate::workspace::Rect::UNIT
            },
            crate::workspace::Rect {
                x: 0.2,
                y: 0.5,
                width: 0.3,
                height: 0.5,
            },
        ] {
            row.rect = position;
            for zoom in [Zoom::None, Zoom::Quad] {
                row.zoom = zoom;
                let draw = |target| {
                    let mut row = row.clone();
                    row.target = target;
                    let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                        paint_icon(ui.painter(), icon, &row, Color32::WHITE);
                    });
                    output.textures_delta.clear();
                    output
                };
                let quad = draw(Target::Quad { tab: 1, node: 2 });
                let panel = draw(Target::Pane { tab: 1, pane: 3 });
                let rectangles = |output: &egui::FullOutput| {
                    output
                        .shapes
                        .iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::Shape::Rect(rect) => Some(rect.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                };
                let quad_rects = rectangles(&quad);
                let panel_rects = rectangles(&panel);
                assert_eq!(quad_rects.len(), 2);
                assert_eq!(panel_rects.len(), 2);
                for (quad, panel) in quad_rects.iter().zip(&panel_rects) {
                    assert_eq!(quad.rect, panel.rect);
                    assert_eq!(quad.fill, Color32::TRANSPARENT);
                    assert_eq!(quad.stroke.color, Color32::WHITE);
                    assert_eq!(quad.stroke.width, 1.0);
                }
                assert_eq!(panel_rects[1].fill, Color32::WHITE);
                let brackets = |output: &egui::FullOutput| {
                    output
                        .shapes
                        .iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::Shape::LineSegment { points, stroke } => Some((*points, *stroke)),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                };
                assert_eq!(brackets(&quad), brackets(&panel));
                assert_eq!(
                    brackets(&quad).len(),
                    if zoom == Zoom::None { 0 } else { 8 }
                );
            }
        }
    }

    fn rows() -> Vec<Row> {
        vec![
            Row {
                target: Target::Window(10),
                depth: 0,
                label: "Window 1 · Work".into(),
                tab_index: None,
                rect: crate::workspace::Rect::UNIT,
                focused: false,
                ancestor: true,
                color: Some([210, 110, 70]),
                zoom: Zoom::None,
                tmuxx: None,
            },
            Row {
                target: Target::Tab(1),
                depth: 1,
                label: "Work".into(),
                tab_index: Some(0),
                rect: crate::workspace::Rect::UNIT,
                focused: false,
                ancestor: true,
                color: Some([210, 110, 70]),
                zoom: Zoom::PaneInQuad,
                tmuxx: None,
            },
            Row {
                target: Target::Quad { tab: 1, node: 2 },
                depth: 2,
                label: "Rustty".into(),
                tab_index: None,
                rect: crate::workspace::Rect {
                    width: 0.5,
                    height: 0.5,
                    ..crate::workspace::Rect::UNIT
                },
                focused: false,
                ancestor: true,
                color: Some([210, 110, 70]),
                zoom: Zoom::Quad,
                tmuxx: None,
            },
            Row {
                target: Target::Pane { tab: 1, pane: 3 },
                depth: 3,
                label: "Tests".into(),
                tab_index: None,
                rect: crate::workspace::Rect {
                    x: 0.5,
                    y: 0.5,
                    width: 0.5,
                    height: 0.5,
                },
                focused: true,
                ancestor: false,
                color: Some([210, 110, 70]),
                zoom: Zoom::PaneInQuad,
                tmuxx: None,
            },
        ]
    }

    fn frame(
        context: &egui::Context,
        rows: &[Row],
        events: Vec<egui::Event>,
        crumbs: bool,
    ) -> (egui::FullOutput, Option<Target>) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 240.0))),
            focused: true,
            events,
            ..Default::default()
        };
        let mut target = None;
        let mut output = context.run_ui(input, |root| {
            egui::CentralPanel::default().show(root, |ui| {
                target = if crumbs {
                    breadcrumb(ui, rows, 140.0)
                } else {
                    contents(ui, rows, &[], &mut SearchState::default(), [210, 110, 70]).selected
                };
            });
        });
        output.textures_delta.clear();
        (output, target)
    }

    #[test]
    fn tmuxx_tag_names_the_toolbar_session_but_keeps_window_rows_compact() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut rows = rows();
        let has_tag = |output: &egui::FullOutput| {
            output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "tmuxx" && !text.galley.elided))
        };
        frame(&context, &rows, Vec::new(), false);
        assert!(!has_tag(&frame(&context, &rows, Vec::new(), false).0));
        rows[0].tmuxx = Some("tmuxx · local".into());
        assert!(has_tag(&frame(&context, &rows, Vec::new(), false).0));
        assert!(description(&rows[0]).contains("tmuxx"));
        let label = "tmuxx · work · build";
        let tooltip = "Session: work\ntmuxx · SSH · me@build.example.com";
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            let width = tmuxx_tag_width(ui, label, ui.available_width());
            assert!((TMUXX_TAG_WIDTH..220.0).contains(&width));
            tmuxx_tag(ui, label, tooltip, width);
        });
        output.textures_delta.clear();
        assert!(output.shapes.iter().any(|shape| matches!(
            &shape.shape,
            egui::Shape::Text(text) if text.galley.text() == label && !text.galley.elided
        )));
        let tag = output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, node)| node.value() == Some(tooltip))
            .map(|(_, node)| node)
            .unwrap();
        assert_eq!(tag.role(), egui::accesskit::Role::Label);
        assert!(!tag.supports_action(egui::accesskit::Action::Click));
    }

    #[test]
    fn tmuxx_tag_elides_long_names_within_the_available_width() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let label = format!("tmuxx · {} · remote", "開発 🦀 café ".repeat(20));
        let tooltip = format!("Session: {label}\ntmuxx · SSH · me@remote.example.com");
        for max_width in [1000.0_f32, 132.0, 30.0] {
            let width = max_width.min(220.0);
            let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                assert_eq!(tmuxx_tag_width(ui, &label, max_width), width);
                tmuxx_tag(ui, &label, &tooltip, width);
            });
            output.textures_delta.clear();
            let tag = output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .find(|(_, node)| node.value() == Some(tooltip.as_str()))
                .map(|(_, node)| node)
                .unwrap();
            assert_eq!(tag.bounds().unwrap().width(), f64::from(width));
            let text = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == label => Some(text),
                    _ => None,
                })
                .unwrap();
            assert!(text.galley.elided);
            assert_eq!(text.galley.rows.len(), 1);
            assert!(text.galley.size().x <= width - 12.0);
        }
    }

    fn node<'a>(output: &'a egui::FullOutput, label: &str) -> Option<&'a egui::accesskit::Node> {
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some(label))
            .map(|(_, node)| node)
    }

    fn bounds(output: &egui::FullOutput, row: &Row) -> Rect {
        let bounds = node(output, &description(row))
            .expect("accessible row")
            .bounds()
            .unwrap();
        Rect::from_min_max(
            Pos2::new(bounds.x0 as f32, bounds.y0 as f32),
            Pos2::new(bounds.x1 as f32, bounds.y1 as f32),
        )
    }

    fn click(context: &egui::Context, rows: &[Row], pos: Pos2, crumbs: bool) -> Option<Target> {
        click_with_modifiers(context, rows, pos, crumbs, egui::Modifiers::NONE)
    }

    fn click_with_modifiers(
        context: &egui::Context,
        rows: &[Row],
        pos: Pos2,
        crumbs: bool,
        modifiers: egui::Modifiers,
    ) -> Option<Target> {
        frame(
            context,
            rows,
            vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::PointerMoved(pos),
            ],
            crumbs,
        );
        let mut target = None;
        for pressed in [true, false] {
            target = frame(
                context,
                rows,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers,
                }],
                crumbs,
            )
            .1
            .or(target);
        }
        target
    }

    fn pin_frame(
        context: &egui::Context,
        rows: &[Row],
        pins: &[Id],
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Output) {
        let mut result = Output::default();
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 420.0))),
                focused: true,
                events,
                ..Default::default()
            },
            |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    result = contents(ui, rows, pins, &mut SearchState::default(), [210, 110, 70]);
                });
            },
        );
        output.textures_delta.clear();
        (output, result)
    }

    fn search_frame(
        context: &egui::Context,
        rows: &[Row],
        pins: &[Id],
        search: &mut SearchState,
        mut input: egui::RawInput,
    ) -> (egui::FullOutput, Output, egui::Id) {
        input
            .screen_rect
            .get_or_insert(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 420.0)));
        let mut result = Output::default();
        let mut scroll_id = egui::Id::NULL;
        let mut output = context.run_ui(input, |root| {
            egui::CentralPanel::default().show(root, |ui| {
                result = contents(ui, rows, pins, search, [210, 110, 70]);
                scroll_id = ui.make_persistent_id(egui::IdSalt::new(
                    if search.open && !search.query.trim().is_empty() {
                        "outliner-search-results"
                    } else {
                        "outliner-rows"
                    },
                ));
            });
        });
        output.textures_delta.clear();
        (output, result, scroll_id)
    }

    fn search_click(
        context: &egui::Context,
        rows: &[Row],
        search: &mut SearchState,
        pos: Pos2,
    ) -> Output {
        let mut result = Output::default();
        for event in [
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ] {
            result = search_frame(
                context,
                rows,
                &[],
                search,
                egui::RawInput {
                    events: vec![event],
                    ..Default::default()
                },
            )
            .1;
        }
        result
    }

    fn navigation_rows() -> Vec<Row> {
        let mut rows = rows();
        for (pane, label) in [(4, "Tasks"), (5, "Tracks")] {
            rows.push(Row {
                target: Target::Pane { tab: 1, pane },
                label: label.into(),
                focused: false,
                ..rows[3].clone()
            });
        }
        rows
    }

    fn control_key(key: egui::Key, repeat: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers: egui::Modifiers::CTRL,
        }
    }

    #[test]
    fn search_groups_shared_ancestors_once_and_navigates_in_display_order() {
        let context = egui::Context::default();
        let mut rows = navigation_rows();
        rows[3].label = "Panel".into();
        rows[4].label = "xPanel".into();
        rows[5].label = "xxPanel".into();
        let other: Vec<_> = rows[..4]
            .iter()
            .map(|row| Row {
                target: match row.target {
                    Target::Window(_) => Target::Window(200),
                    Target::Tab(_) => Target::Tab(21),
                    Target::Quad { .. } => Target::Quad { tab: 21, node: 22 },
                    Target::Pane { .. } => Target::Pane { tab: 21, pane: 23 },
                },
                label: if matches!(row.target, Target::Pane { .. }) {
                    "Panel".into()
                } else {
                    row.label.clone()
                },
                focused: false,
                ancestor: false,
                ..row.clone()
            })
            .collect();
        rows.extend(other);
        let results = crate::outliner::search(&rows, "panel");
        assert_eq!(
            search_groups(&results),
            [vec![0], vec![2], vec![3], vec![1]]
        );
        let mut search = SearchState::default();
        search.start();
        search.query = "panel".into();
        for _ in 0..2 {
            search_frame(&context, &rows, &[], &mut search, Default::default());
        }
        let (output, _, _) = search_frame(&context, &rows, &[], &mut search, Default::default());
        assert_eq!(output.shapes.iter().filter(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Work")).count(), 2);
        let result = |pane| {
            context
                .read_response(egui::Id::new(("outliner-search-result", pane)))
                .unwrap()
                .rect
        };
        let rects = [3_u64, 4, 5, 23].map(result);
        assert!(
            rects
                .windows(2)
                .all(|pair| pair[0].bottom() < pair[1].top())
        );
        for pane in [3, 4, 5, 23, 3] {
            let (_, output, _) = search_frame(
                &context,
                &rows,
                &[],
                &mut search,
                egui::RawInput {
                    events: vec![control_key(egui::Key::N, false)],
                    ..Default::default()
                },
            );
            assert_eq!(
                output.preview,
                Some(Target::Pane {
                    tab: if pane == 23 { 21 } else { 1 },
                    pane
                })
            );
        }
    }

    #[test]
    fn search_navigation_wraps_repeats_and_leaves_the_editor_focused() {
        let context = egui::Context::default();
        let rows = navigation_rows();
        let mut search = SearchState::default();
        search.start();
        assert_eq!(
            search_frame(&context, &rows, &[], &mut search, Default::default()).1,
            Output::default()
        );
        for (key, repeat, pane) in [
            (egui::Key::N, false, 3),
            (egui::Key::N, true, 4),
            (egui::Key::N, true, 5),
            (egui::Key::N, true, 3),
            (egui::Key::P, false, 5),
        ] {
            let (_, output, _) = search_frame(
                &context,
                &rows,
                &[],
                &mut search,
                egui::RawInput {
                    events: vec![control_key(key, repeat)],
                    ..Default::default()
                },
            );
            assert_eq!(
                output,
                Output {
                    preview: Some(Target::Pane { tab: 1, pane }),
                    ..Output::default()
                }
            );
            assert_eq!(search.selected_pane, Some(pane));
            assert!(search.query.is_empty());
            assert!(
                context.memory(|memory| memory.has_focus(egui::Id::new("outliner-search-query")))
            );
            assert_eq!(
                search_frame(&context, &rows, &[], &mut search, Default::default()).1,
                Output::default()
            );
        }
        search.close();
        search.start();
        let (_, output, _) = search_frame(
            &context,
            &rows,
            &[],
            &mut search,
            egui::RawInput {
                events: vec![control_key(egui::Key::P, false)],
                ..Default::default()
            },
        );
        assert_eq!(
            output.preview,
            Some(rows[5].target),
            "previous from no selection starts at the end"
        );
    }

    #[test]
    fn search_acceptance_uses_same_frame_typing_and_navigation() {
        for (key, modifiers) in [
            (egui::Key::Enter, egui::Modifiers::NONE),
            (egui::Key::Escape, egui::Modifiers::NONE),
            (egui::Key::OpenBracket, egui::Modifiers::CTRL),
        ] {
            let context = egui::Context::default();
            let rows = navigation_rows();
            let mut search = SearchState::default();
            search.start();
            search_frame(&context, &rows, &[], &mut search, Default::default());
            let (_, output, _) = search_frame(
                &context,
                &rows,
                &[],
                &mut search,
                egui::RawInput {
                    events: vec![
                        egui::Event::Text("ts".into()),
                        control_key(egui::Key::N, false),
                        egui::Event::Key {
                            key,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers,
                        },
                    ],
                    ..Default::default()
                },
            );
            assert_eq!(
                output,
                Output {
                    selected: Some(rows[3].target),
                    ..Output::default()
                }
            );
            assert!(!search.open);
        }
    }

    #[test]
    fn search_selection_survives_edits_and_reordering_but_clears_on_removal() {
        let context = egui::Context::default();
        let mut rows = navigation_rows();
        let mut search = SearchState::default();
        search.start();
        search_frame(&context, &rows, &[], &mut search, Default::default());
        let (_, output, _) = search_frame(
            &context,
            &rows,
            &[],
            &mut search,
            egui::RawInput {
                events: vec![
                    egui::Event::Text("t".into()),
                    control_key(egui::Key::N, false),
                    control_key(egui::Key::N, false),
                ],
                ..Default::default()
            },
        );
        assert_eq!(output.preview, Some(rows[4].target));
        rows.swap(3, 4);
        assert_eq!(
            search_frame(&context, &rows, &[], &mut search, Default::default()).1,
            Output::default()
        );
        assert_eq!(search.selected_pane, Some(4));
        rows.swap(3, 4);
        let (_, output, _) = search_frame(
            &context,
            &rows,
            &[],
            &mut search,
            egui::RawInput {
                events: vec![egui::Event::Text("s".into())],
                ..Default::default()
            },
        );
        assert_eq!(search.query, "ts");
        assert_eq!(output, Output::default());
        assert_eq!(search.selected_pane, Some(4));
        rows.remove(4);
        assert_eq!(
            search_frame(&context, &rows, &[], &mut search, Default::default()).1,
            Output::default()
        );
        assert_eq!(search.selected_pane, None);
        let (_, output, _) = search_frame(
            &context,
            &rows,
            &[],
            &mut search,
            egui::RawInput {
                events: vec![control_key(egui::Key::P, false)],
                ..Default::default()
            },
        );
        assert_eq!(output.preview, Some(rows[4].target));
        let (_, output, _) = search_frame(
            &context,
            &rows,
            &[],
            &mut search,
            egui::RawInput {
                events: vec![
                    egui::Event::Text("zzz".into()),
                    control_key(egui::Key::N, false),
                ],
                ..Default::default()
            },
        );
        assert_eq!(output, Output::default());
        assert_eq!(search.selected_pane, None);
        let (_, output, _) = search_frame(
            &context,
            &rows,
            &[],
            &mut search,
            egui::RawInput {
                events: (0..5)
                    .map(|_| egui::Event::Key {
                        key: egui::Key::Backspace,
                        physical_key: None,
                        pressed: true,
                        repeat: true,
                        modifiers: egui::Modifiers::NONE,
                    })
                    .collect(),
                ..Default::default()
            },
        );
        assert!(search.query.is_empty());
        assert_eq!(output, Output::default());
        assert_eq!(search.selected_pane, None);
    }

    #[test]
    fn search_accepts_the_clicked_result_without_resetting_to_first() {
        let context = egui::Context::default();
        let rows = navigation_rows();
        let mut search = SearchState::default();
        search.start();
        search.query = "ts".into();
        for _ in 0..2 {
            search_frame(&context, &rows, &[], &mut search, Default::default());
        }
        let result = context
            .read_response(egui::Id::new(("outliner-search-result", 4_u64)))
            .unwrap()
            .rect;
        assert_eq!(
            search_click(&context, &rows, &mut search, result.center()).selected,
            Some(rows[4].target)
        );
        assert_eq!(search.selected_pane, Some(4));
        let (_, output, _) = search_frame(
            &context,
            &rows,
            &[],
            &mut search,
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
        );
        assert_eq!(
            output,
            Output {
                selected: Some(rows[4].target),
                ..Output::default()
            }
        );
        assert!(!search.open);
    }

    #[test]
    fn search_navigation_reveals_selected_result_without_resetting_manual_scroll() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let template = rows().pop().unwrap();
        let rows: Vec<_> = (0..30)
            .map(|pane| Row {
                target: Target::Pane { tab: 1, pane },
                label: format!("Panel {pane}"),
                depth: 0,
                focused: pane == 0,
                ..template.clone()
            })
            .collect();
        let mut search = SearchState::default();
        search.start();
        search.query = "p".into();
        let (_, _, scroll_id) = search_frame(&context, &rows, &[], &mut search, Default::default());
        search_frame(
            &context,
            &rows,
            &[],
            &mut search,
            egui::RawInput {
                events: vec![control_key(egui::Key::N, false)],
                ..Default::default()
            },
        );
        let mut state = egui::scroll_area::State::load(&context, scroll_id).unwrap();
        state.offset.y = 96.0;
        state.store(&context, scroll_id);
        assert_eq!(
            search_frame(&context, &rows, &[], &mut search, Default::default()).1,
            Output::default()
        );
        assert_eq!(
            egui::scroll_area::State::load(&context, scroll_id)
                .unwrap()
                .offset
                .y,
            96.0
        );
        let (_, output, _) = search_frame(
            &context,
            &rows,
            &[],
            &mut search,
            egui::RawInput {
                events: vec![egui::Event::Text("a".into())],
                ..Default::default()
            },
        );
        assert_eq!(
            output,
            Output::default(),
            "refining the same selected panel does not preview again"
        );
        assert_eq!(
            egui::scroll_area::State::load(&context, scroll_id)
                .unwrap()
                .offset
                .y,
            0.0,
            "an explicit query edit reveals the selected result even when its ID is unchanged"
        );
        let mut state = egui::scroll_area::State::load(&context, scroll_id).unwrap();
        state.offset.y = 96.0;
        state.store(&context, scroll_id);
        search_frame(
            &context,
            &rows,
            &[],
            &mut search,
            egui::RawInput {
                events: vec![control_key(egui::Key::N, false)],
                ..Default::default()
            },
        );
        let offset = egui::scroll_area::State::load(&context, scroll_id)
            .unwrap()
            .offset
            .y;
        assert!(
            offset > 0.0 && offset < 96.0,
            "only reveal the selected row above the viewport"
        );
        let (_, output, _) = search_frame(
            &context,
            &rows,
            &[],
            &mut search,
            egui::RawInput {
                events: (0..20).map(|_| control_key(egui::Key::N, true)).collect(),
                ..Default::default()
            },
        );
        assert_eq!(output.preview, Some(rows[21].target));
        let (output, result, _) =
            search_frame(&context, &rows, &[], &mut search, Default::default());
        assert_eq!(result, Output::default());
        let selected = node(&output, "Search result: Panel 21").unwrap();
        assert_eq!(selected.toggled(), Some(egui::accesskit::Toggled::True));
        let bounds = selected.bounds().unwrap();
        assert!(bounds.y0 >= 8.0 && bounds.y1 <= 374.0, "{bounds:?}");
        assert!(
            egui::scroll_area::State::load(&context, scroll_id)
                .unwrap()
                .offset
                .y
                > 96.0
        );
        assert_eq!(search.query, "pa");
    }

    #[test]
    fn search_typing_highlights_only_matching_panel_characters_in_neutral_colors() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut rows = rows();
        rows[2].label = rows[3].label.clone();
        let mut search = SearchState::default();
        search.start();
        search_frame(&context, &rows, &[3], &mut search, Default::default());
        let (output, result, _) = search_frame(
            &context,
            &rows,
            &[3],
            &mut search,
            egui::RawInput {
                events: vec![egui::Event::Text("tst".into())],
                ..Default::default()
            },
        );
        assert_eq!(search.query, "tst");
        assert_eq!(result, Output::default());
        assert_eq!(search.selected_pane, None);
        assert!(node(&output, "Search result: Tests").is_some());
        assert!(node(&output, "Unpin panel: Tests").is_some());
        assert_eq!(
            output
                .shapes
                .iter()
                .filter(|shape| {
                    matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Tests")
                })
                .count(),
            3,
            "pin, quad heading, and search result each draw the name once"
        );
        let mut matched = Vec::new();
        for shape in &output.shapes {
            let egui::Shape::Text(text) = &shape.shape else {
                continue;
            };
            for section in &text.galley.job.sections {
                if section.format.underline != Stroke::NONE {
                    assert_eq!(text.galley.job.text, "Tests");
                    assert_eq!(section.format.underline.color, section.format.color);
                    assert_ne!(section.format.color, rgb([210, 110, 70]));
                    assert_eq!(
                        section.format.background,
                        section.format.color.gamma_multiply(0.15)
                    );
                    matched.push(
                        text.galley.job.text[section.byte_range.start.0..section.byte_range.end.0]
                            .to_owned(),
                    );
                }
            }
        }
        assert_eq!(matched, ["T", "st"]);
    }

    #[test]
    fn search_accept_keys_without_navigation_close_without_selecting() {
        for (key, modifiers) in [
            (egui::Key::Enter, egui::Modifiers::NONE),
            (egui::Key::Escape, egui::Modifiers::NONE),
            (egui::Key::OpenBracket, egui::Modifiers::CTRL),
        ] {
            for query in ["", "bld", "missing"] {
                let context = egui::Context::default();
                let mut rows = rows();
                rows.push(Row {
                    target: Target::Pane { tab: 1, pane: 4 },
                    label: "Build artifacts".into(),
                    focused: false,
                    ..rows.last().unwrap().clone()
                });
                rows.push(Row {
                    target: Target::Pane { tab: 1, pane: 5 },
                    label: "Build".into(),
                    ..rows.last().unwrap().clone()
                });
                let mut search = SearchState::default();
                search.start();
                search_frame(&context, &rows, &[], &mut search, Default::default());
                let (_, result, _) = search_frame(
                    &context,
                    &rows,
                    &[],
                    &mut search,
                    egui::RawInput {
                        events: vec![
                            egui::Event::Text(query.into()),
                            egui::Event::ModifiersChanged(modifiers),
                            egui::Event::Key {
                                key,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers,
                            },
                        ],
                        ..Default::default()
                    },
                );
                assert_eq!(
                    result.selected, None,
                    "{key:?} must close without selecting for {query:?}"
                );
                assert!(!search.open);
                assert!(search.query.is_empty());
            }
        }
    }

    #[test]
    fn search_result_ancestor_click_selects_panel_and_can_resume_typing() {
        let context = egui::Context::default();
        let rows = rows();
        let mut search = SearchState::default();
        search.start();
        search.query = "ts".into();
        for _ in 0..2 {
            search_frame(&context, &rows, &[], &mut search, Default::default());
        }
        let result = context
            .read_response(egui::Id::new(("outliner-search-result", 3_u64)))
            .unwrap()
            .rect;
        let selected = search_click(
            &context,
            &rows,
            &mut search,
            result.left_center() + Vec2::new(10.0, 0.0),
        );
        assert_eq!(selected.selected, Some(rows[3].target));
        assert!(search.open);
        assert_eq!(search.query, "ts");
        search_frame(
            &context,
            &rows,
            &[],
            &mut search,
            egui::RawInput {
                focused: false,
                events: vec![egui::Event::WindowFocused(false)],
                ..Default::default()
            },
        );
        search.focus();
        search_frame(
            &context,
            &rows,
            &[],
            &mut search,
            egui::RawInput {
                events: vec![
                    egui::Event::WindowFocused(true),
                    egui::Event::Text("t".into()),
                ],
                ..Default::default()
            },
        );
        assert_eq!(search.query, "tst");
        let close = context
            .read_response(egui::Id::new("outliner-search-close"))
            .unwrap()
            .rect;
        assert_eq!(
            search_click(&context, &rows, &mut search, close.center()),
            Output::default()
        );
        assert!(!search.open);
    }

    #[test]
    fn search_footer_stays_below_pins_and_results_in_a_narrow_panel() {
        for size in [Vec2::new(400.0, 420.0), Vec2::new(180.0, 240.0)] {
            let context = egui::Context::default();
            context.enable_accesskit();
            let rows = rows();
            let mut search = SearchState::default();
            search.start();
            search.query = "tst".into();
            for _ in 0..2 {
                search_frame(
                    &context,
                    &rows,
                    &[3],
                    &mut search,
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                        ..Default::default()
                    },
                );
            }
            let response = |id| context.read_response(id).unwrap().rect;
            let field = response(egui::Id::new("outliner-search-query"));
            let close = response(egui::Id::new("outliner-search-close"));
            let result = response(egui::Id::new(("outliner-search-result", 3_u64)));
            let pin = response(egui::Id::new(("outliner-pin", 3_u64)));
            assert!(field.left() >= 0.0 && field.right() < close.left());
            assert!(close.right() <= size.x && close.bottom() <= size.y);
            assert!(
                size.y - close.bottom() <= 10.0,
                "footer stays at the bottom"
            );
            assert!(pin.bottom() < result.top() && result.bottom() < field.top());
            assert!(result.right() <= size.x);
        }
    }

    #[test]
    fn search_preserves_tree_scroll_until_focus_changes_then_reveals_the_new_panel() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let template = rows().pop().unwrap();
        let mut rows: Vec<_> = (0..30)
            .map(|index| Row {
                target: Target::Pane {
                    tab: 1,
                    pane: index,
                },
                label: format!("Panel {index}"),
                focused: index == 0,
                depth: 0,
                ..template.clone()
            })
            .collect();
        let mut search = SearchState::default();
        let (_, _, scroll_id) = search_frame(&context, &rows, &[], &mut search, Default::default());
        let mut scroll = egui::scroll_area::State::load(&context, scroll_id).unwrap();
        scroll.offset.y = 96.0;
        scroll.store(&context, scroll_id);
        search.start();
        search.query = "panel 29".into();
        search_frame(&context, &rows, &[], &mut search, Default::default());
        search.close();
        search_frame(&context, &rows, &[], &mut search, Default::default());
        assert_eq!(
            egui::scroll_area::State::load(&context, scroll_id)
                .unwrap()
                .offset
                .y,
            96.0
        );
        search.start();
        search.query = "panel 29".into();
        rows[0].focused = false;
        rows[29].focused = true;
        search_frame(&context, &rows, &[], &mut search, Default::default());
        search.close();
        search_frame(&context, &rows, &[], &mut search, Default::default());
        let (output, _, _) = search_frame(&context, &rows, &[], &mut search, Default::default());
        let pane = bounds(&output, &rows[29]);
        assert!(pane.top() >= 8.0 && pane.bottom() <= 412.0, "{pane:?}");
    }

    #[test]
    fn search_footer_clips_long_trees_and_results_even_at_minimum_window_height() {
        let template = rows().pop().unwrap();
        let rows: Vec<_> = (0..30)
            .map(|index| Row {
                target: Target::Pane {
                    tab: 1,
                    pane: index,
                },
                label: format!("Panel {index}"),
                depth: 0,
                focused: index == 0,
                ..template.clone()
            })
            .collect();
        for size in [Vec2::new(400.0, 420.0), Vec2::new(220.0, 120.0)] {
            for query in ["", "panel"] {
                let context = egui::Context::default();
                let mut search = SearchState::default();
                search.start();
                search.query = query.into();
                for _ in 0..3 {
                    search_frame(
                        &context,
                        &rows,
                        &[0],
                        &mut search,
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                            ..Default::default()
                        },
                    );
                }
                let field = context
                    .read_response(egui::Id::new("outliner-search-query"))
                    .unwrap()
                    .rect;
                for row in &rows {
                    let id = if query.is_empty() {
                        egui::Id::new(("outliner-row", row.target))
                    } else {
                        let Target::Pane { pane, .. } = row.target else {
                            unreachable!()
                        };
                        egui::Id::new(("outliner-search-result", pane))
                    };
                    let rect = context.read_response(id).unwrap().interact_rect;
                    if rect.is_positive() {
                        assert!(
                            rect.bottom() < field.top(),
                            "{size:?}: {rect:?} overlaps {field:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn pinned_highlight_follows_focus_with_readable_accent_colors() {
        let mut rows = rows();
        rows[3].color = Some([40, 70, 110]);
        for (pane, label) in [(4, "Build"), (5, "Unpinned")] {
            rows.push(Row {
                target: Target::Pane { tab: 1, pane },
                label: label.into(),
                color: None,
                ..rows[3].clone()
            });
        }
        for visuals in [egui::Visuals::dark(), egui::Visuals::light()] {
            let context = egui::Context::default();
            context.set_visuals(visuals);
            context.enable_accesskit();
            for focused in [Some(3), Some(4), Some(5), None] {
                for row in &mut rows {
                    row.focused =
                        matches!(row.target, Target::Pane { pane, .. } if Some(pane) == focused);
                }
                pin_frame(&context, &rows, &[3, 4], Vec::new());
                let (output, _) = pin_frame(&context, &rows, &[3, 4], Vec::new());
                for (index, pane) in [(3, 3), (4, 4)] {
                    let handle = context
                        .read_response(egui::Id::new(("outliner-pin", pane as Id)))
                        .unwrap()
                        .rect;
                    let row = &rows[index];
                    let accent = TabAccent::new(row.color, [210, 110, 70]);
                    let pin_rect = context
                        .read_response(egui::Id::new(("outliner-pin-drop", pane as Id)))
                        .unwrap()
                        .rect;
                    let highlighted = output.shapes.iter().any(|shape| {
                        matches!(&shape.shape, egui::Shape::Rect(rect)
                            if rect.rect.contains_rect(pin_rect) && rect.fill == rgb(accent.background))
                    });
                    assert_eq!(highlighted, row.focused, "pin {pane}, focus {focused:?}");
                    if row.focused {
                        let text = output
                            .shapes
                            .iter()
                            .find_map(|shape| match &shape.shape {
                                egui::Shape::Text(text)
                                    if text.galley.text() == row.label
                                        && (text.pos.y - handle.center().y).abs()
                                            < ROW_HEIGHT / 2.0 =>
                                {
                                    Some(text)
                                }
                                _ => None,
                            })
                            .unwrap();
                        assert!(
                            text.galley
                                .job
                                .sections
                                .iter()
                                .all(|section| { section.format.color == rgb(accent.foreground) })
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn pinned_quad_highlight_stops_before_the_panel_and_follows_its_current_quad() {
        use crate::workspace::{Tab, WindowState, Workspace};
        use rustty::config::Direction;
        let mut tab = Tab::new(1, 2, "/tmp".into());
        tab.split(3, 4, Direction::Right, "/tmp".into());
        tab.split(5, 6, Direction::Down, "/tmp".into());
        tab.focus(2);
        tab.split(7, 8, Direction::Down, "/tmp".into());
        tab.focus(2);
        tab.split(9, 10, Direction::Right, "/tmp".into());
        let mut changed = tab.clone();
        changed.close(9);
        let mut flat = Tab::new(1, 2, "/tmp".into());
        flat.split(3, 4, Direction::Right, "/tmp".into());
        let labels = [(2, "Pinned".into())].into();
        for visuals in [egui::Visuals::dark(), egui::Visuals::light()] {
            let context = egui::Context::default();
            let neutral = visuals.text_color();
            context.set_visuals(visuals);
            context.enable_accesskit();
            for tab in [tab.clone(), changed.clone(), flat.clone()] {
                let focuses: Vec<_> = [2, 9, 7, 3, 2]
                    .into_iter()
                    .filter(|pane| tab.contains(*pane))
                    .collect();
                let mut workspace = Workspace::default();
                workspace.windows.push(WindowState {
                    id: 100,
                    tabs: vec![tab],
                    active_tab: 0,
                    frame: [0.0; 4],
                    quick: false,
                });
                for focused in focuses {
                    workspace.windows[0].tabs[0].focus(focused);
                    let rows = crate::outliner::workspace_rows(
                        &workspace,
                        Some(100),
                        &labels,
                        &Default::default(),
                        &Default::default(),
                        &Default::default(),
                    );
                    let path = &pinned_paths(&rows, &[2])[0];
                    let quad = path
                        .iter()
                        .find(|row| matches!(row.target, Target::Quad { .. }));
                    pin_frame(&context, &rows, &[2], Vec::new());
                    let (output, _) = pin_frame(&context, &rows, &[2], Vec::new());
                    let handle = context
                        .read_response(egui::Id::new(("outliner-pin", 2_u64)))
                        .unwrap()
                        .rect;
                    let highlight = output.shapes.iter().find_map(|shape| match &shape.shape {
                        egui::Shape::Rect(rect)
                            if rect.rect.contains_rect(handle)
                                && rect.fill == rgb([210, 110, 70]) =>
                        {
                            Some(rect)
                        }
                        _ => None,
                    });
                    let panel = path.last().unwrap();
                    let pin_bounds = |row: &Row| {
                        let bounds = output
                            .platform_output
                            .accesskit_update
                            .as_ref()
                            .unwrap()
                            .nodes
                            .iter()
                            .filter_map(|(_, node)| {
                                (node.label() == Some(description(row).as_str()))
                                    .then(|| node.bounds())
                                    .flatten()
                            })
                            .find(|bounds| {
                                bounds.y0 < handle.bottom() as f64
                                    && bounds.y1 > handle.top() as f64
                            })
                            .unwrap();
                        Rect::from_min_max(
                            Pos2::new(bounds.x0 as f32, bounds.y0 as f32),
                            Pos2::new(bounds.x1 as f32, bounds.y1 as f32),
                        )
                    };
                    assert_eq!(
                        highlight.is_some(),
                        panel.focused || quad.is_some_and(|quad| quad.ancestor)
                    );
                    if let Some(quad) = quad.filter(|quad| quad.ancestor && !panel.focused) {
                        let highlight = highlight.unwrap();
                        assert_eq!(highlight.rect.right(), pin_bounds(quad).right());
                        assert!(highlight.rect.right() < pin_bounds(panel).left());
                        let text = output
                            .shapes
                            .iter()
                            .find_map(|shape| match &shape.shape {
                                egui::Shape::Text(text)
                                    if text.galley.text() == "Pinned"
                                        && (text.pos.y - handle.center().y).abs()
                                            < ROW_HEIGHT / 2.0 =>
                                {
                                    Some(text)
                                }
                                _ => None,
                            })
                            .unwrap();
                        assert!(
                            text.galley
                                .job
                                .sections
                                .iter()
                                .all(|section| section.format.color == neutral)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn pinned_labels_skip_windows_and_stay_stable_when_switching_tabs() {
        use crate::workspace::{Tab, WindowState, Workspace};
        let mut workspace = Workspace::default();
        let mut window = WindowState {
            id: 100,
            tabs: vec![Tab::new(1, 2, "/tmp".into()), Tab::new(3, 4, "/tmp".into())],
            active_tab: 0,
            frame: [0.0; 4],
            quick: false,
        };
        window.tabs[0].title = Some("agents".into());
        window.tabs[1].title = Some("misc".into());
        workspace.windows = vec![
            window.clone(),
            WindowState {
                id: 200,
                tabs: vec![Tab::new(21, 22, "/tmp".into())],
                ..window
            },
        ];
        let labels = [
            (2, "gitoxide.check".into()),
            (4, "ghostty".into()),
            (22, "remote".into()),
        ]
        .into();
        let context = egui::Context::default();
        context.enable_accesskit();
        for active_tab in [0, 1, 0] {
            workspace.windows[0].active_tab = active_tab;
            let rows = crate::outliner::workspace_rows(
                &workspace,
                Some(100),
                &labels,
                &Default::default(),
                &Default::default(),
                &Default::default(),
            );
            let paths = pinned_paths(&rows, &[2, 4, 22]);
            assert!(
                paths
                    .iter()
                    .all(|path| matches!(path[0].target, Target::Window(_)))
            );
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(500.0, 500.0))),
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        pins(ui, &paths, &[2, 4, 22], [210, 110, 70]);
                    });
                },
            );
            output.textures_delta.clear();
            let text: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() != "›" => {
                        Some(text.galley.text())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                text,
                [
                    "Pinned",
                    "1",
                    "agents",
                    "gitoxide.check",
                    "2",
                    "misc",
                    "ghostty",
                    "3",
                    "remote"
                ]
            );
            let nodes = &output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes;
            assert!(!nodes.iter().any(|(_, node)| {
                node.label()
                    .is_some_and(|label| label.starts_with("Window:"))
            }));
            for path in &paths {
                for row in &path[1..] {
                    assert!(
                        node(&output, &description(row))
                            .unwrap()
                            .supports_action(egui::accesskit::Action::Click)
                    );
                }
            }
        }
    }

    #[test]
    fn pin_numbers_follow_pin_order_after_reordering_unpinning_and_hidden_paths() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut rows = rows();
        rows.push(Row {
            target: Target::Pane { tab: 1, pane: 4 },
            label: "Build".into(),
            focused: false,
            ..rows.last().unwrap().clone()
        });
        for pins in [
            &[3, 4][..],
            &[4, 3],
            &[3],
            &[99, 3, 4],
            &[10, 11, 12, 13, 14, 15, 16, 17, 18, 3, 4],
        ] {
            pin_frame(&context, &rows, pins, Vec::new());
            let (output, _) = pin_frame(&context, &rows, pins, Vec::new());
            for (index, pane) in pins
                .iter()
                .enumerate()
                .filter(|(_, pane)| matches!(pane, 3 | 4))
            {
                let number = (index + 1).to_string();
                let handle = context
                    .read_response(egui::Id::new(("outliner-pin", pane)))
                    .unwrap()
                    .rect;
                let text = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.text() == number => Some(text),
                        _ => None,
                    })
                    .unwrap();
                assert!(text.pos.x + text.galley.size().x < handle.left());
                assert!((text.pos.y + text.galley.size().y / 2.0 - handle.center().y).abs() < 1.0);
                let label = if *pane == 3 { "Tests" } else { "Build" };
                assert_eq!(
                    node(&output, &format!("Unpin panel: {label}"))
                        .unwrap()
                        .description(),
                    Some(format!("Pin {number}").as_str()),
                );
            }
        }
    }

    #[test]
    fn pinned_breadcrumbs_navigate_and_drag_reorders_without_selecting_a_panel() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut rows = rows();
        rows.push(Row {
            target: Target::Pane { tab: 1, pane: 4 },
            label: "Build".into(),
            focused: false,
            ..rows.last().unwrap().clone()
        });
        let pins = [3, 4];
        pin_frame(&context, &rows, &pins, Vec::new());
        let (output, _) = pin_frame(&context, &rows, &pins, Vec::new());
        let ancestors: Vec<_> = output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .filter(|(_, node)| node.label() == Some(description(&rows[1]).as_str()))
            .collect();
        assert_eq!(
            ancestors.len(),
            3,
            "each pinned path and the tree expose their own ancestor control"
        );
        assert_ne!(ancestors[0].0, ancestors[1].0);
        let ancestor = ancestors[0].1.bounds().unwrap();
        let point = Pos2::new(
            (ancestor.x0 + ancestor.x1) as f32 / 2.0,
            (ancestor.y0 + ancestor.y1) as f32 / 2.0,
        );
        pin_frame(
            &context,
            &rows,
            &pins,
            vec![egui::Event::PointerMoved(point)],
        );
        let mut selected = None;
        for pressed in [true, false] {
            let (_, output) = pin_frame(
                &context,
                &rows,
                &pins,
                vec![egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            assert_eq!(output.reorder, None);
            selected = output.selected.or(selected);
        }
        assert_eq!(selected, Some(rows[1].target));

        let pin = context
            .read_response(egui::Id::new(("outliner-pin", 4_u64)))
            .unwrap()
            .rect
            .center();
        let to = context
            .read_response(egui::Id::new(("outliner-pin-drop", 3_u64)))
            .unwrap()
            .rect
            .center_top()
            + Vec2::new(0.0, 2.0);
        let (output, _) = pin_frame(&context, &rows, &pins, vec![egui::Event::PointerMoved(pin)]);
        assert_eq!(
            output.platform_output.cursor_icon,
            egui::CursorIcon::PointingHand
        );
        for pressed in [true, false] {
            let (_, output) = pin_frame(
                &context,
                &rows,
                &pins,
                vec![egui::Event::PointerButton {
                    pos: pin,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            assert_eq!(
                output,
                Output {
                    unpin: (!pressed).then_some(4),
                    ..Output::default()
                }
            );
        }
        for event in [
            egui::Event::PointerButton {
                pos: pin,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerMoved(to),
            egui::Event::PointerMoved(to),
            egui::Event::PointerButton {
                pos: to,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ] {
            assert_eq!(
                pin_frame(&context, &rows, &pins, vec![event]).1,
                Output::default()
            );
            assert!(!egui::DragAndDrop::has_payload_of_type::<Id>(&context));
        }
        let from = context
            .read_response(egui::Id::new(("outliner-pin-number", 4_u64)))
            .unwrap()
            .rect
            .center();
        assert!(from.x < pin.x);
        let (output, _) = pin_frame(
            &context,
            &rows,
            &pins,
            vec![egui::Event::PointerMoved(from)],
        );
        assert_eq!(output.platform_output.cursor_icon, egui::CursorIcon::Grab);
        for pressed in [true, false] {
            assert_eq!(
                pin_frame(
                    &context,
                    &rows,
                    &pins,
                    vec![egui::Event::PointerButton {
                        pos: from,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    }],
                )
                .1,
                Output::default()
            );
        }
        for event in [
            egui::Event::PointerMoved(from),
            egui::Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerMoved(to),
            egui::Event::PointerMoved(to),
        ] {
            assert_eq!(
                pin_frame(&context, &rows, &pins, vec![event]).1,
                Output::default()
            );
        }
        assert!(egui::DragAndDrop::has_payload_of_type::<Id>(&context));
        let (output, _) = pin_frame(&context, &rows, &pins, Vec::new());
        assert_eq!(
            output.platform_output.cursor_icon,
            egui::CursorIcon::Grabbing
        );
        let (_, output) = pin_frame(
            &context,
            &rows,
            &pins,
            vec![egui::Event::PointerButton {
                pos: to,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(
            output,
            Output {
                selected: None,
                preview: None,
                reorder: Some((4, Some(3))),
                unpin: None,
            }
        );

        let id = egui::Id::new(("outliner-pin-number", 3_u64));
        let (output, _) = pin_frame(&context, &rows, &pins, Vec::new());
        let node_id = output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Reorder pinned panel: Tests"))
            .map(|(id, node)| {
                assert!(node.supports_action(egui::accesskit::Action::Increment));
                *id
            })
            .unwrap();
        let (_, output) = pin_frame(
            &context,
            &rows,
            &pins,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Increment,
                    target_tree: egui::accesskit::TreeId::ROOT,
                    target_node: node_id,
                    data: None,
                },
            )],
        );
        assert_eq!(
            output,
            Output {
                selected: None,
                preview: None,
                reorder: Some((3, None)),
                unpin: None,
            }
        );
        context.memory_mut(|memory| memory.request_focus(id));
        let (_, output) = pin_frame(
            &context,
            &rows,
            &pins,
            vec![egui::Event::Key {
                key: egui::Key::ArrowDown,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::ALT,
            }],
        );
        assert_eq!(
            output,
            Output {
                selected: None,
                preview: None,
                reorder: Some((3, None)),
                unpin: None,
            }
        );
        let (output, _) = pin_frame(&context, &rows, &pins, Vec::new());
        let node_id = output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Unpin panel: Tests"))
            .map(|(id, node)| {
                assert!(!node.supports_action(egui::accesskit::Action::Increment));
                *id
            })
            .unwrap();
        let (_, output) = pin_frame(
            &context,
            &rows,
            &pins,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Click,
                    target_tree: egui::accesskit::TreeId::ROOT,
                    target_node: node_id,
                    data: None,
                },
            )],
        );
        assert_eq!(
            output,
            Output {
                unpin: Some(3),
                ..Output::default()
            }
        );
    }

    #[test]
    fn sibling_panels_have_separate_tree_rows_and_share_only_search_breadcrumbs() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut rows = rows();
        rows.push(Row {
            target: Target::Pane { tab: 1, pane: 4 },
            focused: false,
            ..rows[3].clone()
        });
        let mut flat = rows.clone();
        flat.remove(2);
        for row in &mut flat[2..] {
            row.depth -= 1;
        }
        frame(&context, &rows, Vec::new(), false);
        let disclosure = context
            .read_response(egui::Id::new(("outliner-disclosure", rows[2].target)))
            .unwrap()
            .rect;
        assert_eq!(click(&context, &rows, disclosure.center(), false), None);
        for tree in [&rows, &flat] {
            for _ in 0..2 {
                frame(&context, tree, Vec::new(), false);
            }
            let (output, _) = frame(&context, tree, Vec::new(), false);
            assert_eq!(output.shapes.iter().filter(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Tests")).count(), 2);
            let panels = &tree[tree.len() - 2..];
            let first = bounds(&output, &panels[0]);
            let second = bounds(&output, &panels[1]);
            assert!(first.bottom() < second.top());
            assert_eq!(first.left(), second.left());
            for row in panels {
                let rect = bounds(&output, row);
                assert_eq!(
                    click(
                        &context,
                        tree,
                        rect.right_center() - Vec2::new(10.0, 0.0),
                        false
                    ),
                    Some(row.target)
                );
            }
        }
        let mut search = SearchState::default();
        search.start();
        search.query = "tst".into();
        for _ in 0..2 {
            search_frame(&context, &rows, &[], &mut search, Default::default());
        }
        let (output, _, _) = search_frame(&context, &rows, &[], &mut search, Default::default());
        assert_eq!(output.shapes.iter().filter(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Tests")).count(), 1);
        let second = context
            .read_response(egui::Id::new(("outliner-search-result", 4_u64)))
            .unwrap()
            .rect;
        assert_eq!(
            search_click(&context, &rows, &mut search, second.center()).selected,
            Some(rows[4].target)
        );
        assert!(search.open);
        let group = context
            .read_response(egui::Id::new(("outliner-search-group", 3_u64)))
            .unwrap()
            .rect;
        assert_eq!(
            search_click(
                &context,
                &rows,
                &mut search,
                group.right_center() - Vec2::new(10.0, 0.0)
            )
            .selected,
            Some(rows[3].target)
        );
        let (_, output, _) = search_frame(
            &context,
            &rows,
            &[],
            &mut search,
            egui::RawInput {
                events: vec![control_key(egui::Key::N, false)],
                ..Default::default()
            },
        );
        assert_eq!(output.preview, Some(rows[4].target));
    }

    #[test]
    fn rows_expose_focus_zoom_and_click_targets_at_each_level() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let rows = rows();
        frame(&context, &rows, Vec::new(), false);
        let disclosure = context
            .read_response(egui::Id::new(("outliner-disclosure", rows[2].target)))
            .unwrap()
            .rect;
        assert_eq!(click(&context, &rows, disclosure.center(), false), None);
        let (output, _) = frame(&context, &rows, Vec::new(), false);
        for row in &rows {
            let rect = bounds(&output, row);
            assert_eq!(
                click(&context, &rows, rect.center(), false),
                Some(row.target)
            );
        }
        assert!(description(&rows[0]).starts_with("Window:"));
        assert!(description(&rows[1]).contains("Cmd+1"));
        assert!(description(&rows[2]).contains("Quad-zoom"));
        assert!(description(&rows[3]).contains("Zoom within quad-zoom · Focused"));
    }

    #[test]
    fn disclosure_clicks_only_change_visibility_and_shift_applies_to_descendants() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut rows = rows();
        let sibling: Vec<_> = rows[1..]
            .iter()
            .zip([
                Target::Tab(11),
                Target::Quad { tab: 11, node: 12 },
                Target::Pane { tab: 11, pane: 13 },
            ])
            .map(|(row, target)| Row {
                target,
                label: format!("Other {}", row.label),
                focused: false,
                ancestor: false,
                ..row.clone()
            })
            .collect();
        rows.extend(sibling);
        frame(&context, &rows, Vec::new(), false);
        let disclosure = context
            .read_response(egui::Id::new(("outliner-disclosure", rows[4].target)))
            .unwrap()
            .rect;
        assert_eq!(click(&context, &rows, disclosure.center(), false), None);
        let (output, _) = frame(&context, &rows, Vec::new(), false);
        assert!(
            node(&output, &description(&rows[2])).is_some(),
            "quads start visible"
        );
        assert!(
            node(&output, &description(&rows[3])).is_none(),
            "panels start collapsed"
        );
        assert!(
            node(&output, &description(&rows[5])).is_none(),
            "manually collapsed branches stay closed"
        );
        let tab = &rows[4];
        let disclosure = context
            .read_response(egui::Id::new(("outliner-disclosure", tab.target)))
            .unwrap()
            .rect;
        assert!(disclosure.right() <= bounds(&output, tab).left());
        let button = node(&output, &format!("Expand {}", description(tab))).unwrap();
        assert_eq!(button.is_expanded(), Some(false));
        assert!(button.supports_action(egui::accesskit::Action::Click));

        assert_eq!(click(&context, &rows, disclosure.center(), false), None);
        let (output, _) = frame(&context, &rows, Vec::new(), false);
        assert_eq!(
            node(&output, &format!("Collapse {}", description(tab)))
                .unwrap()
                .is_expanded(),
            Some(true)
        );
        assert!(
            node(&output, &description(&rows[5])).is_some(),
            "ordinary expand reveals the next level"
        );
        assert!(node(&output, &description(&rows[6])).is_none());

        for open in [false, true] {
            assert_eq!(
                click_with_modifiers(
                    &context,
                    &rows,
                    disclosure.center(),
                    false,
                    egui::Modifiers::SHIFT
                ),
                None
            );
            let (output, _) = frame(&context, &rows, Vec::new(), false);
            for row in &rows[5..] {
                assert_eq!(
                    node(&output, &description(row)).is_some(),
                    open,
                    "Shift-click changes every descendant level"
                );
            }
        }

        let id = egui::Id::new(("outliner-disclosure", tab.target));
        context.memory_mut(|memory| memory.request_focus(id));
        for open in [false, true] {
            for pressed in [true, false] {
                let (_, target) = frame(
                    &context,
                    &rows,
                    vec![
                        egui::Event::ModifiersChanged(egui::Modifiers::NONE),
                        egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: None,
                            pressed,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    false,
                );
                assert_eq!(target, None);
            }
            let (output, _) = frame(&context, &rows, Vec::new(), false);
            assert_eq!(node(&output, &description(&rows[5])).is_some(), open);
            assert!(
                context.memory(|memory| memory.has_focus(id)),
                "disclosure keeps focus for repeated keyboard toggles"
            );
        }
    }

    #[test]
    fn focus_only_scrolls_enough_to_reveal_an_offscreen_row() {
        fn draw(context: &egui::Context, rows: &[Row]) -> (egui::FullOutput, egui::Id, Rect) {
            let mut scroll_id = egui::Id::NULL;
            let mut viewport = Rect::NOTHING;
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 240.0))),
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        scroll_id = ui.make_persistent_id(egui::IdSalt::new("outliner-rows"));
                        viewport = ui.available_rect_before_wrap();
                        contents(ui, rows, &[], &mut SearchState::default(), [210, 110, 70]);
                    });
                },
            );
            output.textures_delta.clear();
            (output, scroll_id, viewport)
        }
        let context = egui::Context::default();
        context.enable_accesskit();
        let template = rows().pop().unwrap();
        let mut rows: Vec<_> = (0..20)
            .map(|index| Row {
                target: Target::Pane {
                    tab: 1,
                    pane: index,
                },
                label: format!("Panel {index}"),
                focused: index == 0,
                ..template.clone()
            })
            .collect();
        let (_, scroll_id, _) = draw(&context, &rows);
        let mut state = egui::scroll_area::State::load(&context, scroll_id).unwrap();
        state.offset.y = 96.0;
        state.store(&context, scroll_id);
        draw(&context, &rows);

        rows[0].focused = false;
        rows[4].focused = true;
        let (output, _, viewport) = draw(&context, &rows);
        assert!(viewport.contains_rect(bounds(&output, &rows[4])));
        assert_eq!(
            egui::scroll_area::State::load(&context, scroll_id)
                .unwrap()
                .offset
                .y,
            96.0,
            "changing focus within the visible rows must preserve the viewport"
        );

        let previous = bounds(&output, &rows[10]);
        rows[4].focused = false;
        rows[10].focused = true;
        draw(&context, &rows);
        let (output, _, _) = draw(&context, &rows);
        let offset = egui::scroll_area::State::load(&context, scroll_id)
            .unwrap()
            .offset
            .y;
        let minimum = previous.bottom() - viewport.bottom() + 2.0;
        assert!(
            (offset - 96.0 - minimum).abs() <= 1.0,
            "only reveal the clipped row, allowing pixel rounding"
        );
        assert!(viewport.contains_rect(bounds(&output, &rows[10])));

        rows.splice(10..10, self::rows().into_iter().take(3));
        draw(&context, &rows);
        let (output, _, _) = draw(&context, &rows);
        assert!(
            viewport.contains_rect(bounds(&output, &rows[10])),
            "a new collapsed window is revealed after the same focused panel gains ancestors"
        );
        assert!(node(&output, &description(&rows[13])).is_none());
        assert!(node(&output, &description(&rows[12])).is_none());
    }

    #[test]
    fn focus_scrolls_to_the_visible_quad_tab_or_previously_expanded_panel() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let template = rows();
        let mut rows = Vec::new();
        for index in 0..15 {
            let tab = 100 + index * 3;
            for (level, target) in [
                Target::Tab(tab),
                Target::Quad { tab, node: tab + 1 },
                Target::Pane { tab, pane: tab + 2 },
            ]
            .into_iter()
            .enumerate()
            {
                rows.push(Row {
                    target,
                    depth: level,
                    label: format!("Entry {tab} {level}"),
                    focused: index == 0 && level == 2,
                    ancestor: index == 0 && level < 2,
                    ..template[level + 1].clone()
                });
            }
        }
        frame(&context, &rows, Vec::new(), false);
        let last = rows.len() - 1;
        for row in &mut rows[..3] {
            row.focused = false;
            row.ancestor = false;
        }
        rows[last].focused = true;
        rows[last - 1].ancestor = true;
        rows[last - 2].ancestor = true;
        for _ in 0..2 {
            frame(&context, &rows, Vec::new(), false);
        }
        let (output, _) = frame(&context, &rows, Vec::new(), false);
        let viewport = Rect::from_min_max(Pos2::new(8.0, 8.0), Pos2::new(312.0, 232.0));
        assert!(viewport.contains_rect(bounds(&output, &rows[last - 1])));
        assert!(node(&output, &description(&rows[last])).is_none());
        let disclosure = |target| {
            context
                .read_response(egui::Id::new(("outliner-disclosure", target)))
                .unwrap()
                .rect
                .center()
        };
        assert_eq!(
            click(&context, &rows, disclosure(rows[last - 1].target), false),
            None
        );
        let (output, _) = frame(&context, &rows, Vec::new(), false);
        assert!(node(&output, &description(&rows[last])).is_some());
        assert_eq!(
            click(&context, &rows, disclosure(rows[last - 2].target), false),
            None
        );
        let (output, _) = frame(&context, &rows, Vec::new(), false);
        assert!(viewport.contains_rect(bounds(&output, &rows[last - 2])));
        assert!(node(&output, &description(&rows[last - 1])).is_none());

        for focused in [0, 14] {
            for (index, row) in rows.iter_mut().enumerate() {
                row.focused = index / 3 == focused && index % 3 == 2;
                row.ancestor = index / 3 == focused && index % 3 < 2;
            }
            for _ in 0..2 {
                frame(&context, &rows, Vec::new(), false);
            }
        }
        let (output, _) = frame(&context, &rows, Vec::new(), false);
        assert!(viewport.contains_rect(bounds(&output, &rows[last - 2])));
        assert!(node(&output, &description(&rows[last - 1])).is_none());
        assert_eq!(
            click(&context, &rows, disclosure(rows[last - 2].target), false),
            None
        );
        for focused in [0, 14] {
            for (index, row) in rows.iter_mut().enumerate() {
                row.focused = index / 3 == focused && index % 3 == 2;
                row.ancestor = index / 3 == focused && index % 3 < 2;
            }
            for _ in 0..2 {
                frame(&context, &rows, Vec::new(), false);
            }
        }
        let (output, _) = frame(&context, &rows, Vec::new(), false);
        assert!(viewport.contains_rect(bounds(&output, &rows[last])));
    }

    #[test]
    fn narrow_pinned_path_keeps_every_level_and_tab_name() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut rows = rows();
        rows[0].label = "Window with a very long name".repeat(5);
        rows[2].label = "Quad with a very long name".repeat(5);
        frame(&context, &rows, Vec::new(), true);
        let (output, _) = frame(&context, &rows, Vec::new(), true);
        let first = bounds(&output, &rows[0]);
        let last = bounds(&output, rows.last().unwrap());
        assert!((last.right() - first.left() - 140.0).abs() < 0.1);
        for row in &rows {
            let rect = bounds(&output, row);
            assert!(
                rect.width() > 0.0 && rect.left() >= first.left() && rect.right() <= last.right()
            );
            assert_eq!(
                click(&context, &rows, rect.center(), true),
                Some(row.target)
            );
        }
        assert!(
            output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Work")
            }),
            "tab breadcrumb paints its name even when the visible text is elided"
        );
    }

    #[test]
    fn breadcrumb_compact_names_keep_icons_and_every_click_target() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let accent = Color32::from_rgb(255, 200, 0);
        let foreground = Color32::from_rgb(120, 130, 140);
        let weak_foreground = Color32::from_rgb(90, 100, 110);
        let mut rows = rows();
        rows[2].label = "Work".into();
        rows[3].label = "Work".into();
        let draw = |rows: &[Row], events| {
            let mut selected = None;
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 240.0))),
                    events,
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        ui.visuals_mut().selection.bg_fill = accent;
                        ui.visuals_mut().override_text_color = Some(foreground);
                        ui.visuals_mut().weak_text_color = Some(weak_foreground);
                        selected = breadcrumb(ui, rows, 580.0);
                    });
                },
            );
            output.textures_delta.clear();
            (output, selected)
        };
        draw(&rows, Vec::new());
        let (output, _) = draw(&rows, Vec::new());
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text)
                    if text.galley.text() != "›" && !text.galley.text().is_empty() =>
                {
                    Some(text.galley.text())
                }
                _ => None,
            })
            .collect();
        assert_eq!(labels, ["Window 1", "Work", "Work"]);
        let background = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect) if rect.fill == accent.gamma_multiply(0.15) => Some(rect),
                _ => None,
            })
            .unwrap();
        for shape in &output.shapes {
            if let egui::Shape::Rect(rect) = &shape.shape
                && rect.stroke.width > 0.0
            {
                assert_eq!(rect.stroke.color, foreground);
            }
            if let egui::Shape::Text(text) = &shape.shape
                && text.galley.text() == "›"
            {
                assert_eq!(text.galley.job.sections[0].format.color, weak_foreground);
            }
            if let egui::Shape::Text(text) = &shape.shape
                && text.galley.text() != "›"
            {
                assert_eq!(text.galley.job.sections[0].format.color, accent);
                assert_eq!(
                    text.galley.job.sections[0].format.font_id,
                    FontId::proportional(13.0)
                );
            }
        }
        for row in &rows {
            let rect = bounds(&output, row);
            assert!(background.rect.contains_rect(rect));
            assert!(rect.width() >= ICON_SIZE);
            let pos = rect.center();
            draw(&rows, vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                let (_, selected) = draw(
                    &rows,
                    vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    }],
                );
                assert_eq!(selected, (!pressed).then_some(row.target));
            }
        }
        rows[3].label = "Tests".into();
        let (output, _) = draw(&rows, Vec::new());
        assert!(
            output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Work")
            }),
            "a hidden quad name must not suppress the tab name"
        );
        rows[1].label = "Other".into();
        let (output, _) = draw(&rows, Vec::new());
        assert!(output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Window 1 · Work")
        }), "a hidden quad name must not suppress the window's tab suffix");
    }

    #[test]
    fn narrow_breadcrumb_keeps_both_targets_inside_its_budget() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut rows = rows().split_off(2);
        rows[0].label = "Long shared name of this quadrant".repeat(5);
        rows[1].label = "Tests".into();
        frame(&context, &rows, Vec::new(), true);
        let (output, _) = frame(&context, &rows, Vec::new(), true);
        let first = bounds(&output, &rows[0]);
        let second = bounds(&output, &rows[1]);
        assert!(first.right() < second.left());
        assert!(second.width() >= ICON_SIZE + 20.0);
        assert!(second.right() - first.left() <= 140.0);
        assert!((first.width() - ICON_SIZE - 6.0).abs() < 0.1);
        assert!((second.left() - first.right() - 10.0).abs() < 0.1);
        assert!(!output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == rows[0].label)
        }));
        for row in &rows {
            assert_eq!(
                click(&context, &rows, bounds(&output, row).center(), true),
                Some(row.target)
            );
        }
    }

    #[test]
    fn expanded_quads_show_panel_labels_and_collapsed_quads_show_their_own_label() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let rows = rows();
        frame(&context, &rows, Vec::new(), false);
        for open in [false, true, false] {
            let (output, _) = frame(&context, &rows, Vec::new(), false);
            let has_text = |label: &str| {
                output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == label)
            })
            };
            assert_eq!(has_text(&rows[2].label), !open);
            assert_eq!(has_text(&rows[3].label), open);
            assert!(has_text(&rows[1].label));
            assert_eq!(
                click(&context, &rows, bounds(&output, &rows[2]).center(), false),
                Some(rows[2].target)
            );
            let disclosure = context
                .read_response(egui::Id::new(("outliner-disclosure", rows[2].target)))
                .unwrap()
                .rect;
            assert_eq!(click(&context, &rows, disclosure.center(), false), None);
        }
    }
}
