use super::*;

impl Engine {
    pub(super) fn select_pane(
        &mut self,
        name: &str,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(args, "DdegLlMmP:RT:t:UZ")?;
        a.no_rest()?;
        let (sid, wid, mut pane) = self.pane_context(a.string(b't').as_deref(), ctx)?;
        let last = a.has(b'l') || name == "last-pane";
        let mut out = Outcome::default();
        if last {
            let window = &self.windows[&wid];
            pane = window
                .last
                .first()
                .copied()
                .filter(|pane| self.panes.contains_key(pane))
                .or_else(|| {
                    (window.panes.len() == 2)
                        .then(|| {
                            window
                                .panes
                                .iter()
                                .copied()
                                .find(|pane| *pane != window.active)
                        })
                        .flatten()
                })
                .ok_or("no last pane")?;
        } else if a.has(b'm') || a.has(b'M') {
            self.marked = if a.has(b'M') {
                None
            } else {
                Some(Marked {
                    session: sid,
                    index: self.window_target(a.string(b't').as_deref(), ctx)?.1,
                    window: wid,
                    pane,
                })
            };
            out.changed = true;
            return Ok(out);
        }
        if !last {
            if let Some(style) = a.get(b'P') {
                let style = std::str::from_utf8(style).map_err(|_| "invalid style")?;
                styles::validate(style)?;
                let options = &mut self.panes.get_mut(&pane).unwrap().options;
                options.insert("window-style".into(), style.into());
                options.insert("window-active-style".into(), style.into());
                out.changed = true;
            }
            if a.has(b'g') {
                line(
                    &mut out.output,
                    self.pane_option(pane, "window-style").unwrap_or("default"),
                );
                return Ok(out);
            }
            if let Some(direction) = b"LRUD".iter().find(|flag| a.has(**flag)) {
                // Search the saved layout even while the current pane fills the window.
                let zoom = self.windows[&wid].zoomed;
                self.windows.get_mut(&wid).unwrap().zoomed = false;
                self.sync_layout(wid, &mut out);
                pane = self.adjacent_pane(pane, *direction).unwrap_or(pane);
                self.windows.get_mut(&wid).unwrap().zoomed = zoom;
                self.sync_layout(wid, &mut out);
                // Temporary resizing can reflow contents even if the selection stays put.
                out.changed |= out
                    .effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::Resize { .. }));
            }
        }
        if a.has(b'e') || a.has(b'd') {
            self.panes.get_mut(&pane).unwrap().input_disabled = !a.has(b'e');
            out.changed = true;
            return Ok(out);
        }
        if !last && let Some(title) = a.get(b'T') {
            let title = self.expand(title, Some(sid), Some(wid), Some(pane), ctx)?;
            let title = std::str::from_utf8(&title).map_err(|_| "invalid title")?;
            self.panes
                .get_mut(&pane)
                .unwrap()
                .terminal
                .set_title(clean_name(title, false).as_bytes());
            out.changed = true;
            return Ok(out);
        }
        if self.windows[&wid].modal.is_some_and(|modal| modal != pane) {
            return Ok(out);
        }
        if self.windows[&wid].active != pane {
            let window = self.windows.get_mut(&wid).unwrap();
            let zoom = a.has(b'Z') && window.zoomed && !window.pane_visible(pane);
            if !window.pane_visible(pane) {
                window.zoomed = false;
                self.sync_layout(wid, &mut out);
                out.notifications.push(self.layout_event(wid));
            }
            let window = self.windows.get_mut(&wid).unwrap();
            window.select(pane);
            out.notifications.push(
                format!("%window-pane-changed @{wid} %{pane}")
                    .into_bytes()
                    .into(),
            );
            if zoom {
                window.zoomed = true;
                window.zoomed_pane = Some(window.active);
                window.save_zoom_geometry();
            }
            self.sync_layout(wid, &mut out);
            if zoom {
                out.notifications.push(self.layout_event(wid));
            }
            out.changed = true;
        }
        out.selection_only = !a.has(b'P')
            && !out
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::Resize { .. }))
            && !out
                .notifications
                .iter()
                .any(|event| matches!(event, Notification::LayoutChange { .. }));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(engine: &mut Engine, source: &str) -> Outcome {
        engine
            .execute(
                &tmuxx_command::parse(source.as_bytes()).unwrap()[0],
                &mut ClientContext::default(),
            )
            .unwrap()
    }

    fn engine() -> Engine {
        let mut engine = Engine::default();
        for source in [
            "new-session -d -s selection ''",
            "split-window -h -d -t %0 ''",
            "new-window -d -t selection ''",
        ] {
            run(&mut engine, source);
        }
        engine
    }

    #[test]
    fn pane_and_window_selections_include_noops_without_terminal_refresh() {
        let mut engine = engine();
        for source in [
            "select-pane -t %0",
            "select-pane -t %1",
            "select-pane -L -t %1",
            "last-pane -t %0",
            "select-pane -t %1",
            "select-window -t @0",
            "select-window -t @1",
            "select-window -t @1",
            "last-window -t selection",
            "next-window -t selection",
            "previous-window -t selection",
        ] {
            let out = run(&mut engine, source);
            assert!(out.selection_only, "{source}: {out:?}");
            assert!(
                !out.effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::Resize { .. }))
            );
        }
    }

    #[test]
    fn pane_selection_mutations_and_zoom_keep_full_refresh() {
        let base = engine();
        for source in [
            "select-pane -t %1 -P bg=red",
            "select-pane -t %0 -P bg=red",
            "select-pane -t %1 -T title",
            "select-pane -t %1 -m",
            "select-pane -t %1 -M",
            "select-pane -t %1 -d",
            "select-pane -t %1 -e",
        ] {
            let mut engine = base.clone();
            let out = run(&mut engine, source);
            assert!(out.changed && !out.selection_only, "{source}: {out:?}");
        }
        for source in [
            "select-pane -t %1",
            "select-pane -Z -t %1",
            "select-pane -R -t %0",
            "select-pane -L -t %0",
        ] {
            let mut engine = base.clone();
            run(&mut engine, "resize-pane -Z -t %0");
            let out = run(&mut engine, source);
            assert!(out.changed && !out.selection_only, "{source}: {out:?}");
            assert!(
                out.effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::Resize { .. }))
            );
        }
    }

    #[test]
    fn selection_state_contains_styles_but_never_terminal_history() {
        let mut engine = engine();
        run(&mut engine, "set-option -g window-active-style bg=#010203");
        engine.set_control_colors(1, [Some([4, 5, 6]), Some([7, 8, 9])]);
        engine.set_pane_host_state(1, false, true, Some(false), false);
        run(&mut engine, "select-pane -t %1");
        let state = engine.selection_state();
        assert_eq!(state["windows"][0]["active"], 1);
        assert_eq!(
            state["panes"][1]["style"],
            serde_json::to_value(engine.pane_style(1)).unwrap()
        );
        assert_eq!(
            state["panes"][1]["control_colors"],
            serde_json::json!([[4, 5, 6], [7, 8, 9]])
        );
        assert_eq!(state["panes"][1]["host_dark"], false);
        for _ in 0..1000 {
            engine.feed(0, b"ordinary command output fills retained history\r\n");
        }
        assert!(engine.panes[&0].terminal.history_len() > 900);
        assert_eq!(engine.selection_state(), state);
        assert!(serde_json::to_vec(&state).unwrap().len() < 2048);
    }

    #[test]
    fn snapshots_and_selection_updates_include_process_and_start_paths() {
        let mut engine = engine();
        engine.panes.get_mut(&0).unwrap().cwd = Some("C:\\start".into());
        let pane = engine.panes.get_mut(&1).unwrap();
        pane.cwd = Some("C:\\start".into());
        pane.current_path = Some("C:\\work 雪 é".into());
        engine.feed(1, b"\x1b]7;file://host/C:/shell\x07");

        let snapshot = engine.native_snapshot();
        for state in [
            serde_json::to_value(&snapshot).unwrap(),
            engine.selection_state(),
        ] {
            assert_eq!(state["panes"][0]["current_path"], "C:\\start");
            assert_eq!(state["panes"][1]["current_path"], "C:\\work 雪 é");
            assert_eq!(state["panes"][2]["current_path"], serde_json::Value::Null);
        }
        assert_eq!(
            snapshot.panes[1]
                .terminal
                .as_ref()
                .unwrap()
                .restore()
                .unwrap()
                .cwd(),
            b"file://host/C:/shell"
        );
    }
}
