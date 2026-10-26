//! Saved session import controls; discovery and file ownership live in the host.

#[derive(Clone, Debug)]
pub(super) struct Row {
    pub id: String,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Command {
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
        ui.heading("Import Saved Session State");
        ui.label("Choose saved session state to restore its windows and terminal contents.");
        ui.label(
            "Imported panels restore display and scrollback. Local shells start when their tab is opened.",
        );
        ui.add_space(12.0);
        egui::ScrollArea::vertical()
            .max_height((ui.available_height() - 120.0).max(80.0))
            .show(ui, |ui| {
                if rows.is_empty() {
                    ui.weak("No saved sessions are available.");
                }
                for row in rows {
                    ui.push_id(&row.id, |ui| {
                        if ui
                            .selectable_label(selected.as_ref() == Some(&row.id), &row.label)
                            .clicked()
                        {
                            *selected = Some(row.id.clone());
                        }
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
            if ui
                .add_enabled(source.is_some(), egui::Button::new("Import"))
                .clicked()
                && let Some(source) = source
            {
                command = Some(Command::Import(source.id.clone()));
            }
        });
    });
    command
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
        ["24 Oct 2026 at 12:30", "23 Oct 2026 at 21:44"]
            .into_iter()
            .enumerate()
            .map(|(index, label)| Row {
                id: index.to_string(),
                label: label.into(),
                detail: format!("Rustty 0.1.0 · PID {}", index + 100),
            })
            .collect()
    }

    #[test]
    fn picker_imports_the_selected_saved_session() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let rows = rows();
        let mut selected = Some("0".into());
        assert_eq!(
            click(&context, "23 Oct 2026 at 21:44", |root| {
                show(root, &rows, &mut selected, None)
            }),
            None
        );
        assert_eq!(selected.as_deref(), Some("1"));
        let output = frame(&context, vec![], |root| {
            show(root, &rows, &mut selected, None)
        })
        .1;
        center(&output, "Rustty 0.1.0 · PID 101");
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
    fn picker_requires_a_selection_and_exposes_import_errors() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let rows = rows();
        let mut selected = None;
        assert_eq!(
            click(&context, "Import", |root| show(
                root,
                &rows,
                &mut selected,
                None
            )),
            None
        );
        let output = frame(&context, vec![], |root| {
            show(
                root,
                &rows,
                &mut selected,
                Some("Saved session could not be read."),
            )
        })
        .1;
        center(&output, "Saved session could not be read.");
    }
}
