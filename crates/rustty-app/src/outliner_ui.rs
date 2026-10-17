//! A movable hierarchy and its compact, neutral breadcrumb.
use crate::{
    outliner::{Expansion, Row, Target, Zoom, has_children},
    presentation::TabAccent,
};
use egui::{Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};

const ROW_HEIGHT: f32 = 30.0;
const ICON_SIZE: f32 = 20.0;
const INDENT: f32 = 18.0;

pub fn contents(ui: &mut egui::Ui, rows: &[Row], platform_accent: [u8; 3]) -> Option<Target> {
    ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
    let expansion_id = ui.id().with("outliner-expansion");
    let mut expansion = ui
        .data(|data| data.get_temp::<Expansion>(expansion_id))
        .unwrap_or_default();
    let changed = expansion.sync(rows);
    let mut selected = None;
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
                let response =
                    ui.interact(selection_rect, ui.id().with(row.target), Sense::click());
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
                paint_label(ui, row, rect, text_left, foreground);
                if row.focused && changed {
                    response.scroll_to_me(None);
                }
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::SelectableLabel,
                        ui.is_enabled(),
                        row.focused,
                        description(row),
                    )
                });
                if response.clicked() {
                    response.surrender_focus();
                    selected = Some(row.target);
                }
                response.on_hover_text(description(row));
            }
        });
    if !ui.is_sizing_pass() {
        ui.data_mut(|data| data.insert_temp(expansion_id, expansion));
    }
    selected
}

/// Call in a right-aligned header; width is bounded even for long pane labels.
pub fn breadcrumb(ui: &mut egui::Ui, rows: &[Row], max_width: f32) -> Option<Target> {
    if rows.is_empty() || max_width <= 0.0 {
        return None;
    }
    let foreground = ui.visuals().text_color();
    let mut widths: Vec<f32> = rows
        .iter()
        .map(|row| {
            ui.painter()
                .layout_no_wrap(row.label.clone(), FontId::proportional(13.0), foreground)
                .size()
                .x
                + ICON_SIZE
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
        paint_icon(
            &painter,
            Rect::from_center_size(
                Pos2::new(x + ICON_SIZE / 2.0 + 3.0, rect.center().y),
                Vec2::splat(ICON_SIZE),
            ),
            row,
            foreground,
        );
        paint_label(ui, row, rect, x + ICON_SIZE + 8.0, foreground);
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

fn paint_label(ui: &egui::Ui, row: &Row, rect: Rect, left: f32, foreground: Color32) {
    let width = (rect.right() - left - 6.0).max(0.0);
    if width == 0.0 {
        return;
    }
    let mut job = egui::text::LayoutJob::simple_singleline(
        row.label.clone(),
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
                    contents(ui, rows, [210, 110, 70])
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
                        contents(ui, rows, [210, 110, 70]);
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
