//! An interactive tab preview that shares the window's normal popup input routing.

use crate::outliner::Target;
use egui::{Popup, Response};

const OPEN_DELAY: f64 = 0.25;
const LEAVE_DELAY: f64 = 0.18;

#[derive(Clone, Default)]
struct Hover {
    entered: Option<f64>,
    left: Option<f64>,
    suppressed: bool,
    was_open: bool,
}

#[derive(Clone)]
struct Active {
    popup: egui::Id,
    pass: u64,
    dismissed: bool,
}

fn active_id(viewport: egui::ViewportId) -> egui::Id {
    egui::Id::new(("active-tab-outline", viewport))
}

/// Dismiss only this viewport's tab preview before routing native input.
///
/// egui can close popup memory only inside its current viewport. Queue that
/// update until this viewport next renders, without touching another window.
pub fn dismiss(ctx: &egui::Context, viewport: egui::ViewportId) -> bool {
    let id = active_id(viewport);
    let Some(mut active) = ctx.data(|data| data.get_temp::<Active>(id)) else {
        return false;
    };
    if active.dismissed {
        return false;
    }
    active.dismissed = true;
    ctx.data_mut(|data| {
        let hover_id = active.popup.with("hover");
        let mut hover = data.get_temp::<Hover>(hover_id).unwrap_or_default();
        hover.suppressed = true;
        hover.entered = None;
        hover.was_open = false;
        data.insert_temp(hover_id, hover);
        data.insert_temp(id, active);
    });
    true
}

/// Call after drawing the tabs, before caching this viewport's popup state.
pub fn finish_frame(ctx: &egui::Context) {
    let id = active_id(ctx.viewport_id());
    if let Some(active) = ctx.data(|data| data.get_temp::<Active>(id)) {
        if active.dismissed || active.pass != ctx.cumulative_pass_nr() {
            Popup::close_id(ctx, active.popup);
        }
        if !Popup::is_id_open(ctx, active.popup) {
            ctx.data_mut(|data| data.remove::<Active>(id));
        }
    }
}

/// Show a tab's children without opening another native window.
///
/// The response must have a stable tab ID. Contents can use `ui.available_height()`
/// to constrain their scroll area and return the target selected by the user.
pub fn show(
    response: &Response,
    contents: impl FnOnce(&mut egui::Ui) -> Option<Target>,
) -> Option<Target> {
    let ctx = &response.ctx;
    let id = response.id.with(("tab-outline", ctx.viewport_id()));
    let active_id = active_id(ctx.viewport_id());
    if let Some(active) = ctx.data(|data| data.get_temp::<Active>(active_id))
        && active.dismissed
    {
        Popup::close_id(ctx, active.popup);
        ctx.data_mut(|data| data.remove::<Active>(active_id));
    }
    let state_id = id.with("hover");
    let mut state = ctx
        .data(|data| data.get_temp::<Hover>(state_id))
        .unwrap_or_default();
    let (now, pointer, pressed, held, escape, focused) = ctx.input(|input| {
        (
            input.time,
            input.pointer.hover_pos(),
            input.pointer.any_pressed(),
            input.pointer.any_down(),
            input.key_pressed(egui::Key::Escape),
            input.focused,
        )
    });
    let on_tab = pointer.is_some_and(|point| response.rect.contains(point));
    let over_tab = response.contains_pointer();
    let mut open = Popup::is_id_open(ctx, id);
    if state.was_open && !open {
        state.suppressed = true;
        state.entered = None;
    }
    if !on_tab {
        state.suppressed = false;
        state.entered = None;
    }
    if !focused || pointer.is_none() || !response.enabled() {
        Popup::close_id(ctx, id);
        ctx.data_mut(|data| data.remove::<Hover>(state_id));
        return None;
    }
    if escape || pressed && over_tab {
        Popup::close_id(ctx, id);
        state.suppressed = on_tab;
        state.entered = None;
        open = false;
    }
    if !open {
        state.left = None;
        if response.hovered() && !held && !state.suppressed && !Popup::is_any_open(ctx) {
            let entered = *state.entered.get_or_insert(now);
            let remaining = OPEN_DELAY - (now - entered);
            if remaining <= 0.0 {
                Popup::open_id(ctx, id);
                open = true;
            } else {
                ctx.request_repaint_after_secs(remaining as f32);
            }
        } else {
            state.entered = None;
        }
    }
    if open {
        let over_popup = pointer.is_some_and(|point| {
            ctx.read_response(id)
                .is_some_and(|popup| popup.rect.contains(point))
        });
        if over_tab || over_popup {
            state.left = None;
        } else {
            // Start at the actual departure, even if the pointer rested here for hours.
            let left = *state.left.get_or_insert(now);
            let remaining = LEAVE_DELAY - (now - left);
            if remaining <= 0.0 {
                Popup::close_id(ctx, id);
                open = false;
            } else {
                ctx.request_repaint_after_secs(remaining as f32);
            }
        }
    }
    let selected = if open {
        let frame = egui::Frame::popup(&ctx.global_style());
        let margin = frame.total_margin().sum();
        let bounds = ctx.content_rect().shrink(4.0);
        let width = (bounds.width().min(360.0) - margin.x).max(1.0);
        let height = (bounds.height().min(400.0) - margin.y).max(1.0);
        let left = response.rect.left().clamp(
            bounds.left(),
            (bounds.right() - width - margin.x).max(bounds.left()),
        );
        let anchor = egui::Rect::from_min_max(
            egui::pos2(left, response.rect.top()),
            egui::pos2(left, response.rect.bottom()),
        );
        Popup::from_response(response)
            .id(id)
            .anchor(anchor)
            .align_alternatives(&[egui::RectAlign::TOP_START])
            .open_memory(None)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .gap(2.0)
            .width(width)
            .frame(frame)
            .show(|ui| {
                ui.set_width(width);
                ui.set_max_height(height);
                let selected = contents(ui);
                if selected.is_some() {
                    ui.close();
                }
                selected
            })
            .and_then(|shown| shown.inner)
    } else {
        None
    };
    state.was_open = Popup::is_id_open(ctx, id);
    if open && !state.was_open {
        state.suppressed = true;
        state.entered = None;
    }
    if state.was_open {
        let pass = ctx.cumulative_pass_nr();
        ctx.data_mut(|data| {
            data.insert_temp(
                active_id,
                Active {
                    popup: id,
                    pass,
                    dismissed: false,
                },
            );
        });
    }
    ctx.data_mut(|data| data.insert_temp(state_id, state));
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Id, Modifiers, PointerButton, Pos2, Rect, Vec2};

    struct Preview {
        context: egui::Context,
        time: f64,
        screen: Vec2,
        tab: Rect,
        first_row: Option<Rect>,
        rows: usize,
        focused: bool,
        tab_visible: bool,
        viewport: egui::ViewportId,
        other_viewport: Option<egui::ViewportId>,
    }

    impl Default for Preview {
        fn default() -> Self {
            Self {
                context: egui::Context::default(),
                time: 0.0,
                screen: egui::vec2(640.0, 480.0),
                tab: Rect::from_min_size(egui::pos2(20.0, 10.0), egui::vec2(80.0, 24.0)),
                first_row: None,
                rows: 3,
                focused: true,
                tab_visible: true,
                viewport: egui::ViewportId::ROOT,
                other_viewport: None,
            }
        }
    }

    impl Preview {
        fn id(&self) -> Id {
            Id::new("tab").with(("tab-outline", self.viewport))
        }

        fn open(&self) -> bool {
            Popup::is_id_open(&self.context, self.id())
        }

        fn frame(&mut self, elapsed: f64, events: Vec<Event>) -> Option<Target> {
            self.time += elapsed;
            self.first_row = None;
            let mut selected = None;
            let mut raw = egui::RawInput {
                viewport_id: self.viewport,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.screen)),
                time: Some(self.time),
                events,
                focused: self.focused,
                ..Default::default()
            };
            raw.viewports.entry(self.viewport).or_default();
            if let Some(viewport) = self.other_viewport {
                raw.viewports.entry(viewport).or_default();
            }
            let mut output = self.context.run_ui(raw, |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    if !self.tab_visible {
                        return;
                    }
                    let response = ui.interact(self.tab, Id::new("tab"), egui::Sense::click());
                    Popup::context_menu(&response).show(|ui| {
                        ui.label("Tab settings");
                    });
                    selected = show(&response, |ui| {
                        let mut selected = None;
                        egui::ScrollArea::vertical()
                            .max_height(ui.available_height())
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for index in 0..self.rows {
                                    let row = ui.add_sized(
                                        [ui.available_width(), 30.0],
                                        egui::Button::new(format!("Panel {index}")),
                                    );
                                    if index == 0 {
                                        self.first_row = Some(row.rect);
                                    }
                                    if row.clicked() {
                                        selected = Some(Target::Pane {
                                            tab: 1,
                                            pane: index as u64,
                                        });
                                    }
                                }
                            });
                        selected
                    });
                });
                finish_frame(&self.context);
            });
            output.textures_delta.clear();
            selected
        }

        fn hover(&mut self) {
            self.frame(0.01, vec![]);
            self.frame(0.01, vec![Event::PointerMoved(self.tab.center())]);
            self.frame(OPEN_DELAY - 0.01, vec![]);
            assert!(!self.open(), "the preview should wait for its hover delay");
            self.frame(0.02, vec![]);
            self.frame(0.01, vec![]); // Finish the popup's sizing pass.
            assert!(self.open());
        }

        fn button(
            &mut self,
            position: Pos2,
            button: PointerButton,
            pressed: bool,
        ) -> Option<Target> {
            self.frame(
                0.01,
                vec![Event::PointerButton {
                    pos: position,
                    button,
                    pressed,
                    modifiers: Modifiers::NONE,
                }],
            )
        }
    }

    #[test]
    fn hover_transfers_into_the_popup_and_selects_a_panel() {
        let mut preview = Preview::default();
        preview.hover();
        let row = preview.first_row.unwrap().center();
        preview.frame(5.0, vec![]);
        preview.frame(
            0.01,
            vec![Event::PointerMoved(egui::pos2(
                preview.tab.center().x,
                preview.tab.bottom() + 1.0,
            ))],
        );
        assert!(preview.open());
        preview.frame(LEAVE_DELAY / 2.0, vec![Event::PointerMoved(row)]);
        preview.frame(1.0, vec![]);
        assert!(preview.open());
        preview.button(row, PointerButton::Primary, true);
        assert_eq!(
            preview.button(row, PointerButton::Primary, false),
            Some(Target::Pane { tab: 1, pane: 0 }),
        );
        assert!(!preview.open());
    }

    #[test]
    fn escape_suppresses_reopening_until_the_pointer_leaves_the_tab() {
        let mut preview = Preview::default();
        preview.hover();
        preview.frame(
            0.01,
            vec![Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert!(!preview.open());
        preview.frame(1.0, vec![]);
        assert!(!preview.open());
        preview.frame(0.01, vec![Event::PointerMoved(egui::pos2(600.0, 450.0))]);
        preview.hover();
        preview.frame(0.01, vec![Event::PointerMoved(egui::pos2(600.0, 450.0))]);
        preview.frame(LEAVE_DELAY / 2.0, vec![]);
        assert!(preview.open());
        preview.frame(LEAVE_DELAY, vec![]);
        assert!(!preview.open());
        preview.hover();
        preview.frame(0.01, vec![Event::PointerGone]);
        assert!(!preview.open());
        preview.hover();
        preview.focused = false;
        preview.frame(0.01, vec![]);
        assert!(!preview.open());
    }

    #[test]
    fn tab_clicks_and_drags_do_not_open_or_replace_context_menus() {
        let mut preview = Preview::default();
        preview.frame(0.01, vec![]);
        let tab = preview.tab.center();
        preview.frame(0.01, vec![Event::PointerMoved(tab)]);
        preview.button(tab, PointerButton::Primary, true);
        preview.frame(1.0, vec![]);
        assert!(!preview.open());
        preview.button(tab, PointerButton::Primary, false);
        preview.frame(1.0, vec![]);
        assert!(!preview.open());
        preview.frame(0.01, vec![Event::PointerMoved(egui::pos2(600.0, 450.0))]);
        preview.hover();
        preview.button(tab, PointerButton::Secondary, true);
        assert!(!preview.open());
        preview.button(tab, PointerButton::Secondary, false);
        let menu = Id::new("tab").with("popup");
        assert!(Popup::is_id_open(&preview.context, menu));
        preview.frame(1.0, vec![]);
        assert!(Popup::is_id_open(&preview.context, menu));
        assert!(!preview.open());
    }

    #[test]
    fn the_popup_fits_narrow_windows_and_stays_open_when_scrolled() {
        let mut preview = Preview {
            screen: egui::vec2(240.0, 180.0),
            tab: Rect::from_min_size(egui::pos2(190.0, 10.0), egui::vec2(40.0, 24.0)),
            rows: 30,
            ..Default::default()
        };
        preview.hover();
        let popup = preview.context.read_response(preview.id()).unwrap().rect;
        assert!(
            Rect::from_min_size(Pos2::ZERO, preview.screen).contains_rect(popup),
            "popup {popup:?} exceeds window {:?}",
            preview.screen,
        );
        assert!(popup.width() <= 360.0 && popup.height() <= 400.0);
        let top = preview.first_row.unwrap().top();
        preview.frame(0.01, vec![Event::PointerMoved(popup.center())]);
        preview.frame(
            0.01,
            vec![Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -60.0),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::NONE,
            }],
        );
        preview.frame(0.1, vec![]);
        assert!(preview.open());
        assert!(preview.first_row.unwrap().top() < top);
    }

    #[test]
    fn native_dismissal_does_not_close_another_viewports_context_menu() {
        let first = egui::ViewportId::from_hash_of("first");
        let other = egui::ViewportId::from_hash_of("other");
        let mut preview = Preview {
            viewport: first,
            other_viewport: Some(other),
            ..Default::default()
        };
        let menu = Id::new("other-context-menu");
        let other_frame = |context: &egui::Context, open: bool| {
            let mut raw = egui::RawInput {
                viewport_id: other,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(640.0, 480.0))),
                ..Default::default()
            };
            raw.viewports.entry(first).or_default();
            raw.viewports.entry(other).or_default();
            let mut output = context.run_ui(raw, |root| {
                if open {
                    Popup::open_id(context, menu);
                }
                Popup::new(
                    menu,
                    context.clone(),
                    egui::pos2(20.0, 20.0),
                    root.layer_id(),
                )
                .open_memory(None)
                .show(|ui| {
                    ui.label("Other window's settings");
                });
                finish_frame(context);
                assert!(
                    Popup::is_id_open(context, menu),
                    "other frame, opening={open}"
                );
            });
            output.textures_delta.clear();
        };
        preview.hover();
        other_frame(&preview.context, true);
        assert!(dismiss(&preview.context, first));
        assert!(!dismiss(&preview.context, first));
        assert!(Popup::is_id_open(&preview.context, menu));
        preview.frame(0.01, vec![]);
        assert!(!preview.open());
        preview.frame(1.0, vec![]);
        assert!(!preview.open());
        other_frame(&preview.context, false);
    }

    #[test]
    fn removing_the_tab_closes_its_preview_before_input_state_is_cached() {
        let mut preview = Preview::default();
        preview.hover();
        preview.tab_visible = false;
        preview.frame(0.01, vec![]);
        assert!(!preview.open());
        assert!(!Popup::is_any_open(&preview.context));
        assert!(!dismiss(&preview.context, egui::ViewportId::ROOT));
    }
}
