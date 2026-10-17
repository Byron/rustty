//! A movable hierarchy and its compact, neutral breadcrumb.
use crate::{
    outliner::{Expansion, Row, Target, Zoom, has_children, pinned_paths},
    presentation::TabAccent,
    workspace::Id,
};
use egui::{Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};

const ROW_HEIGHT: f32 = 30.0;
const ICON_SIZE: f32 = 20.0;
const INDENT: f32 = 18.0;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub selected: Option<Target>,
    pub reorder: Option<(Id, Option<Id>)>,
    pub unpin: Option<Id>,
}

pub fn contents(
    ui: &mut egui::Ui,
    rows: &[Row],
    pinned_panes: &[Id],
    platform_accent: [u8; 3],
) -> Output {
    ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
    let mut output = pins(ui, &pinned_paths(rows, pinned_panes));
    let expansion_id = ui.id().with("outliner-expansion");
    let mut expansion = ui
        .data(|data| data.get_temp::<Expansion>(expansion_id))
        .unwrap_or_default();
    let changed = expansion.sync(rows);
    egui::ScrollArea::vertical()
        .id_salt("outliner-rows")
        .auto_shrink([false, false])
        .animated(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for index in expansion.visible(rows) {
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
                paint_label(ui, &row.label, label_rect, text_left, foreground);
                if row.focused && changed {
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
                }
                response.on_hover_text(description(row));
            }
        });
    if !ui.is_sizing_pass() {
        ui.data_mut(|data| data.insert_temp(expansion_id, expansion));
    }
    output
}

fn pins(ui: &mut egui::Ui, paths: &[Vec<Row>]) -> Output {
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
        .max_height((ui.available_height() * 0.35).clamp(ROW_HEIGHT, 180.0))
        .min_scrolled_height(ROW_HEIGHT)
        .auto_shrink([false, true])
        .animated(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (index, (&pane, path)) in panes.iter().zip(paths).enumerate() {
                let (_, rect) = ui.allocate_space(Vec2::new(ui.available_width(), ROW_HEIGHT));
                let handle_rect = Rect::from_min_size(rect.min, Vec2::new(22.0, ROW_HEIGHT));
                let handle = ui.interact(
                    handle_rect,
                    egui::Id::new(("outliner-pin", pane)),
                    Sense::click_and_drag(),
                );
                handle.dnd_set_drag_payload(pane);
                if handle.hovered() || handle.has_focus() || handle.dragged() {
                    ui.painter().rect_stroke(
                        handle_rect.shrink(2.0),
                        3,
                        Stroke::new(1.0, ui.visuals().weak_text_color()),
                        egui::StrokeKind::Inside,
                    );
                }
                paint_pin(
                    ui.painter(),
                    Rect::from_center_size(handle_rect.center(), Vec2::splat(16.0)),
                    ui.visuals().text_color(),
                );
                let label = format!("Unpin panel: {}", path.last().unwrap().label);
                handle.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), &label)
                });
                ui.ctx().accesskit_node_builder(handle.id, |node| {
                    if index > 0 {
                        node.add_action(egui::accesskit::Action::Decrement);
                    }
                    if index + 1 < panes.len() {
                        node.add_action(egui::accesskit::Action::Increment);
                    }
                });
                if handle.clicked() {
                    handle.surrender_focus();
                    output.unpin = Some(pane);
                }
                let has_focus = handle.has_focus();
                let (up, down) = ui.input_mut(|input| {
                    (
                        input.has_accesskit_action_request(
                            handle.id,
                            egui::accesskit::Action::Decrement,
                        ) || has_focus
                            && input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowUp),
                        input.has_accesskit_action_request(
                            handle.id,
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
                handle
                    .on_hover_cursor(egui::CursorIcon::Grab)
                    .on_hover_text(format!("{label}\nDrag to reorder · Alt+↑/↓ to move"));

                let crumb_rect = Rect::from_min_max(
                    Pos2::new(handle_rect.right() + 3.0, rect.top() + 2.0),
                    rect.max,
                );
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt(("pinned-path", pane))
                        .max_rect(crumb_rect),
                );
                output.selected =
                    breadcrumb(&mut child, path, crumb_rect.width()).or(output.selected);
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

/// Call in a right-aligned header; width is bounded even for long pane labels.
pub fn breadcrumb(ui: &mut egui::Ui, rows: &[Row], max_width: f32) -> Option<Target> {
    if rows.is_empty() || max_width <= 0.0 {
        return None;
    }
    let foreground = ui.visuals().text_color();
    let labels: Vec<_> = rows
        .iter()
        .map(|row| {
            row.tab_index.map_or_else(
                || row.label.clone(),
                |index| format!("Cmd+{} {}", index + 1, row.label),
            )
        })
        .collect();
    let mut widths: Vec<f32> = rows
        .iter()
        .zip(&labels)
        .map(|(row, label)| {
            ui.painter()
                .layout_no_wrap(label.clone(), FontId::proportional(13.0), foreground)
                .size()
                .x
                + if row.tab_index.is_some() {
                    0.0
                } else {
                    ICON_SIZE
                }
                + 15.0
        })
        .collect();
    let separator = 18.0;
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
        if index > 0 {
            ui.painter()
                .with_clip_rect(bounds.intersect(ui.clip_rect()))
                .text(
                    Pos2::new(x + separator / 2.0, bounds.center().y),
                    egui::Align2::CENTER_CENTER,
                    "›",
                    FontId::proportional(16.0),
                    ui.visuals().weak_text_color(),
                );
            x += separator;
        }
        let width = widths[index];
        let rect = Rect::from_min_size(
            Pos2::new(x, bounds.top()),
            Vec2::new(width, bounds.height()),
        );
        let response = ui.interact(
            rect,
            ui.id().with(("breadcrumb", row.target)),
            Sense::click(),
        );
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        if response.hovered() || response.has_focus() {
            painter.rect_stroke(
                rect.shrink(0.5),
                3,
                Stroke::new(1.0, ui.visuals().weak_text_color()),
                egui::StrokeKind::Inside,
            );
        }
        let text_left = if row.tab_index.is_some() {
            x + 5.0
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
            x + ICON_SIZE + 8.0
        };
        paint_label(ui, &labels[index], rect, text_left, foreground);
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), description(row))
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

fn paint_label(ui: &egui::Ui, label: &str, rect: Rect, left: f32, foreground: Color32) {
    let width = (rect.right() - left - 6.0).max(0.0);
    if width == 0.0 {
        return;
    }
    let mut job = egui::text::LayoutJob::simple_singleline(
        label.to_owned(),
        FontId::proportional(13.0),
        foreground,
    );
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
    let inner = outer.shrink(if quad { 3.0 } else { 1.5 });
    if quad {
        painter.rect_stroke(
            outer.shrink(2.0),
            0,
            Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
    }
    let position = Rect::from_min_size(
        inner.min + Vec2::new(row.rect.x * inner.width(), row.rect.y * inner.height()),
        Vec2::new(
            row.rect.width * inner.width(),
            row.rect.height * inner.height(),
        ),
    );
    painter.rect_filled(position, 0, color);
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
                    contents(ui, rows, &[], [210, 110, 70]).selected
                };
            });
        });
        output.textures_delta.clear();
        (output, target)
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
                    result = contents(ui, rows, pins, [210, 110, 70]);
                });
            },
        );
        output.textures_delta.clear();
        (output, result)
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
            .filter(|(_, node)| node.label() == Some(description(&rows[0]).as_str()))
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
        assert_eq!(selected, Some(rows[0].target));

        let from = context
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
        pin_frame(
            &context,
            &rows,
            &pins,
            vec![egui::Event::PointerMoved(from)],
        );
        for pressed in [true, false] {
            let (_, output) = pin_frame(
                &context,
                &rows,
                &pins,
                vec![egui::Event::PointerButton {
                    pos: from,
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
                reorder: Some((4, Some(3))),
                unpin: None,
            }
        );

        let id = egui::Id::new(("outliner-pin", 3_u64));
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
                reorder: Some((3, None)),
                unpin: None,
            }
        );
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
    fn rows_expose_focus_zoom_and_click_targets_at_each_level() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let rows = rows();
        frame(&context, &rows, Vec::new(), false);
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
        let (output, _) = frame(&context, &rows, Vec::new(), false);
        assert!(
            node(&output, &description(&rows[3])).is_some(),
            "focused path starts open"
        );
        assert!(
            node(&output, &description(&rows[5])).is_none(),
            "unrelated branches start closed"
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
                        contents(ui, rows, &[], [210, 110, 70]).selected;
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
            viewport.contains_rect(bounds(&output, &rows[13])),
            "the same focused panel is revealed after gaining ancestors"
        );
        assert!(
            egui::scroll_area::State::load(&context, scroll_id)
                .unwrap()
                .offset
                .y
                > offset
        );
    }

    #[test]
    fn narrow_pinned_path_keeps_every_level_and_tab_shortcut() {
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
        assert!(output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().starts_with("Cmd+1 Work"))
        }), "tab breadcrumb paints its shortcut even when the visible text is elided");
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
        assert!((second.right() - first.left() - 140.0).abs() < 0.1);
        for row in &rows {
            assert_eq!(
                click(&context, &rows, bounds(&output, row).center(), true),
                Some(row.target)
            );
        }
    }
}
