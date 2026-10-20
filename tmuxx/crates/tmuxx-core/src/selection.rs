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
        Ok(out)
    }
}
