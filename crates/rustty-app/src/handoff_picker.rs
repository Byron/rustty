//! Visual state import controls; discovery, file ownership, and approval live in the host.

#[derive(Clone, Debug)]
pub(super) struct Row {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub status: Status,
}

#[derive(Clone, Debug)]
pub(super) enum Status {
    Available,
    Waiting,
    Ready { file: String, bytes: u64 },
    Declined,
    Ignored,
    Unavailable,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Command {
    Ask(String),
    Import(String),
    Refresh,
    Cancel,
}

pub(super) fn show(
    root: &mut egui::Ui,
    rows: &[Row],
    selected: &mut Option<String>,
    error: Option<&str>,
) -> Option<Command> {
    if root.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        return Some(Command::Cancel);
    }
    if selected
        .as_ref()
        .is_some_and(|selected| !rows.iter().any(|row| &row.id == selected))
    {
        *selected = None;
    }
    let mut command = None;
    egui::CentralPanel::default().show(root, |ui| {
        ui.heading("Import Terminal State");
        ui.label("Choose a running Rustty instance to copy its windows and terminal contents.");
        ui.label("Imported panels start new shells. The original instance keeps running.");
        ui.add_space(12.0);
        egui::ScrollArea::vertical()
            .max_height((ui.available_height() - 120.0).max(80.0))
            .show(ui, |ui| {
                if rows.is_empty() {
                    ui.weak("No other Rustty instances are available.");
                }
                for row in rows {
                    ui.push_id(&row.id, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .selectable_label(selected.as_ref() == Some(&row.id), &row.label)
                                .clicked()
                            {
                                *selected = Some(row.id.clone());
                            }
                            match &row.status {
                                Status::Available => {}
                                Status::Waiting => {
                                    ui.weak("Waiting for approval…");
                                }
                                Status::Ready { file, bytes } => {
                                    ui.weak(format!("Ready · {file} · {bytes} bytes"));
                                }
                                Status::Declined => {
                                    ui.weak("Request declined");
                                }
                                Status::Ignored => {
                                    ui.weak("Ignoring requests for this session");
                                }
                                Status::Unavailable => {
                                    ui.weak("State export unavailable");
                                }
                            }
                        });
                        if !row.detail.is_empty() {
                            ui.weak(&row.detail);
                        }
                        ui.add_space(8.0);
                    });
                }
            });
        ui.separator();
        if let Some(error) = error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                command = Some(Command::Cancel);
            }
            if ui.button("Refresh").clicked() {
                command = Some(Command::Refresh);
            }
            let source = rows.iter().find(|row| selected.as_ref() == Some(&row.id));
            let (label, enabled) = match source.map(|row| &row.status) {
                Some(Status::Ready { .. }) => ("Import", true),
                Some(Status::Available | Status::Declined) => ("Ask", true),
                Some(Status::Waiting) => ("Waiting…", false),
                _ => ("Ask", false),
            };
            if ui.add_enabled(enabled, egui::Button::new(label)).clicked()
                && let Some(source) = source
            {
                command = Some(if matches!(source.status, Status::Ready { .. }) {
                    Command::Import(source.id.clone())
                } else {
                    Command::Ask(source.id.clone())
                });
            }
        });
    });
    command
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Approval {
    pub export: bool,
    pub ignore_further: bool,
}

/// Call only when the host has no other modal prompt to show.
pub(super) fn show_approval(
    context: &egui::Context,
    requester: &str,
    ignore_further: &mut bool,
) -> Option<Approval> {
    let mut approved = None;
    egui::Window::new("Export Terminal State?")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(context, |ui| {
            ui.label(format!("{requester} is asking to copy this instance's terminal state."));
            ui.label("Export all windows and panel contents, including scrollback and alternate screens.");
            ui.label("Your shells and this instance will keep running.");
            ui.checkbox(ignore_further, "Ignore further requests for this session");
            ui.horizontal(|ui| {
                if ui.button("Decline").clicked() {
                    approved = Some(false);
                }
                if ui.button("Export").clicked() {
                    approved = Some(true);
                }
            });
        });
    if context.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        approved = Some(false);
    }
    approved.map(|export| Approval {
        export,
        ignore_further: *ignore_further,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton, Pos2, Rect, Vec2};

    fn frame<T>(
        context: &egui::Context,
        events: Vec<Event>,
        mut show: impl FnMut(&mut egui::Ui) -> Option<T>,
    ) -> (Option<T>, egui::PlatformOutput) {
        let mut command = None;
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0))),
                focused: true,
                events,
                ..Default::default()
            },
            |root| command = show(root).or(command.take()),
        );
        output.textures_delta.clear();
        (command, output.platform_output)
    }

    fn center(output: &egui::PlatformOutput, label: &str) -> Pos2 {
        output
            .accesskit_update
            .as_ref()
            .and_then(|update| {
                update.nodes.iter().find_map(|(_, node)| {
                    if node.label() != Some(label) && node.value() != Some(label) {
                        return None;
                    }
                    let rect = node.bounds()?;
                    Some(Pos2::new(
                        ((rect.x0 + rect.x1) / 2.0) as f32,
                        ((rect.y0 + rect.y1) / 2.0) as f32,
                    ))
                })
            })
            .unwrap_or_else(|| panic!("accessible control {label:?}"))
    }

    fn click<T>(
        context: &egui::Context,
        label: &str,
        mut show: impl FnMut(&mut egui::Ui) -> Option<T>,
    ) -> Option<T> {
        frame(context, vec![], &mut show);
        let point = center(&frame(context, vec![], &mut show).1, label);
        let mut command = None;
        for pressed in [true, false] {
            command = frame(
                context,
                vec![
                    Event::PointerMoved(point),
                    Event::PointerButton {
                        pos: point,
                        button: PointerButton::Primary,
                        pressed,
                        modifiers: Modifiers::default(),
                    },
                ],
                &mut show,
            )
            .0
            .or(command);
        }
        command
    }

    fn escape() -> Event {
        Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        }
    }

    fn rows() -> Vec<Row> {
        ["Rustty old", "Rustty previous"]
            .into_iter()
            .enumerate()
            .map(|(index, label)| Row {
                id: index.to_string(),
                label: label.into(),
                detail: format!("PID {} · /Applications/{label}.app", index + 100),
                status: Status::Available,
            })
            .collect()
    }

    #[test]
    fn picker_asks_selected_instance_then_imports_its_ready_dump() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut rows = rows();
        let mut selected = None;
        let select = click(&context, "Rustty previous", |root| {
            show(root, &rows, &mut selected, None)
        });
        assert_eq!(select, None);
        assert_eq!(selected.as_deref(), Some("1"));
        assert_eq!(
            click(&context, "Ask", |root| show(
                root,
                &rows,
                &mut selected,
                None
            )),
            Some(Command::Ask("1".into()))
        );
        rows[1].status = Status::Waiting;
        assert_eq!(
            click(&context, "Waiting…", |root| show(
                root,
                &rows,
                &mut selected,
                None
            )),
            None
        );
        rows[1].status = Status::Ready {
            file: "request-1.rustty-state.lz4".into(),
            bytes: 2048,
        };
        let output = frame(&context, vec![], |root| {
            show(root, &rows, &mut selected, None)
        })
        .1;
        center(&output, "Ready · request-1.rustty-state.lz4 · 2048 bytes");
        assert_eq!(
            click(&context, "Import", |root| show(
                root,
                &rows,
                &mut selected,
                None
            )),
            Some(Command::Import("1".into()))
        );
    }

    #[test]
    fn picker_refreshes_cancels_and_clears_a_disappeared_selection() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut selected = Some("gone".into());
        assert_eq!(
            click(&context, "Refresh", |root| show(
                root,
                &[],
                &mut selected,
                None
            )),
            Some(Command::Refresh)
        );
        assert_eq!(selected, None);
        assert_eq!(
            click(&context, "Cancel", |root| show(
                root,
                &[],
                &mut selected,
                None
            )),
            Some(Command::Cancel)
        );
        assert_eq!(
            frame(&context, vec![escape()], |root| show(
                root,
                &[],
                &mut selected,
                None
            ))
            .0,
            Some(Command::Cancel)
        );
    }

    #[test]
    fn picker_cannot_request_ignored_or_unavailable_instances() {
        for status in [Status::Ignored, Status::Unavailable] {
            let context = egui::Context::default();
            context.enable_accesskit();
            let mut rows = rows();
            rows[0].status = status;
            let mut selected = Some("0".into());
            assert_eq!(
                click(&context, "Ask", |root| show(
                    root,
                    &rows,
                    &mut selected,
                    None
                )),
                None
            );
            rows[0].status = Status::Declined;
            assert_eq!(
                click(&context, "Ask", |root| show(
                    root,
                    &rows,
                    &mut selected,
                    None
                )),
                Some(Command::Ask("0".into()))
            );
        }
    }

    #[test]
    fn approval_can_ignore_future_requests_while_exporting_or_declining() {
        for (label, export) in [("Export", true), ("Decline", false)] {
            let context = egui::Context::default();
            context.enable_accesskit();
            let mut ignore = false;
            assert_eq!(
                click(&context, "Ignore further requests for this session", |_| {
                    show_approval(&context, "Rustty new · PID 42", &mut ignore)
                }),
                None
            );
            assert!(ignore);
            assert_eq!(
                click(&context, label, |_| {
                    show_approval(&context, "Rustty new · PID 42", &mut ignore)
                }),
                Some(Approval {
                    export,
                    ignore_further: true,
                })
            );
        }
    }

    #[test]
    fn approval_escape_declines_without_enabling_ignore() {
        let context = egui::Context::default();
        let mut ignore = false;
        assert_eq!(
            frame(&context, vec![escape()], |_| {
                show_approval(&context, "Rustty new", &mut ignore)
            })
            .0,
            Some(Approval {
                export: false,
                ignore_further: false,
            })
        );
    }
}
