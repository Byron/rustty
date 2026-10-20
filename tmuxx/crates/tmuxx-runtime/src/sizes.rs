use super::*;

impl Host {
    pub(super) fn command_sizes(&mut self, command: &tmuxx_command::Command) -> bool {
        let Some(name) = command
            .args
            .first()
            .and_then(|name| tmuxx_command::registry::find(name).ok())
            .map(|entry| entry.name)
        else {
            return false;
        };
        let immediate = name == "refresh-client"
            && tmuxx_command::Args::parse(&command.args[1..], "A:B:cC:Df:r:F:lLRSt:U")
                .is_ok_and(|args| args.has(b'C') && !args.has(b'A') && !args.has(b'B'));
        self.sizes_pending |= match name {
            "refresh-client" => immediate,
            "new-session" | "attach-session" | "switch-client" | "new-window" | "break-pane"
            | "join-pane" | "move-pane" | "move-window" | "link-window" | "unlink-window"
            | "kill-window" | "kill-pane" | "kill-session" | "select-window" | "next-window"
            | "previous-window" | "last-window" | "swap-window" | "select-layout"
            | "next-layout" | "previous-layout" | "resize-window" | "set-option"
            | "set-window-option" => true,
            _ => false,
        };
        immediate
    }

    pub(super) fn recalculate_sizes(&mut self) {
        self.sizes_pending = false;
        self.latest_clients
            .retain(|window, _| self.engine.windows.contains_key(window));
        self.size_activity
            .retain(|client, _| self.clients.contains_key(client));
        for (window, latest) in &mut self.latest_clients {
            if !self.clients.contains_key(latest)
                && let Some((id, _)) = self
                    .clients
                    .iter()
                    .filter(|(_, client)| {
                        client
                            .context
                            .session
                            .and_then(|sid| self.engine.sessions.get(&sid))
                            .is_some_and(|session| {
                                session.windows.get(&session.active) == Some(window)
                            })
                    })
                    .max_by_key(|(id, _)| self.size_activity.get(id))
            {
                *latest = *id;
            }
        }
        let unflagged = self.clients.values().any(|client| {
            client.context.session.is_some()
                && (client.context.control
                    || client.context.size_requested
                    || client.context.window_size_requested)
                && !client.context.ignore_size
        });
        let mut sizes = Vec::new();
        for window in self.engine.windows.values() {
            let clients: Vec<_> = self
                .clients
                .iter()
                .filter(|(_, client)| {
                    let c = &client.context;
                    (c.size_requested || c.window_size_requested)
                        && (!c.ignore_size || !unflagged)
                        && c.session
                            .and_then(|sid| self.engine.sessions.get(&sid))
                            .is_some_and(|session| {
                                session.windows.values().any(|id| *id == window.id)
                            })
                })
                .collect();
            let policy = self
                .engine
                .window_option(window.id, "window-size")
                .unwrap_or("latest");
            let aggressive =
                self.engine.window_option(window.id, "aggressive-resize") == Some("on");
            let candidates: Vec<_> = clients
                .iter()
                .copied()
                .filter(|(_, client)| {
                    !aggressive
                        || client
                            .context
                            .session
                            .and_then(|sid| self.engine.sessions.get(&sid))
                            .is_some_and(|session| {
                                session.windows.get(&session.active) == Some(&window.id)
                            })
                })
                .collect();
            let latest = self.latest_clients.get(&window.id).copied();
            let selected: Vec<_> = candidates
                .iter()
                .copied()
                .filter(|(id, _)| policy != "latest" || clients.len() <= 1 || Some(**id) == latest)
                .collect();
            let mut size = (policy == "manual").then_some(window.manual_size);
            let mut cell = if policy == "manual" {
                self.engine.panes[&window.active].terminal.cell_size()
            } else {
                (0, 0)
            };
            if policy != "manual" {
                for (_, client) in selected {
                    let c = &client.context;
                    let (cols, rows) = c
                        .window_sizes
                        .get(&window.id)
                        .copied()
                        .unwrap_or((c.cols, c.rows));
                    size = Some(match size {
                        None => (cols, rows),
                        Some((x, y)) if policy == "largest" => (x.max(cols), y.max(rows)),
                        Some((x, y)) => (x.min(cols), y.min(rows)),
                    });
                    if c.cell_width > cell.0 && c.cell_height > cell.1 {
                        cell = (c.cell_width, c.cell_height);
                    }
                }
            }
            // A cap can supply the size even when the latest client is not eligible.
            for (_, client) in candidates {
                if let Some(&(cols, rows)) = client.context.window_sizes.get(&window.id) {
                    size = Some(size.map_or((cols, rows), |(x, y)| (x.min(cols), y.min(rows))));
                }
            }
            let Some((cols, rows)) = size else {
                continue;
            };
            let (cols, rows) = (cols.clamp(1, 10000), rows.clamp(1, 10000));
            if cell.0 == 0 {
                cell.0 = 16;
            }
            if cell.1 == 0 {
                cell.1 = 32;
            }
            let pixels_changed = window
                .panes
                .iter()
                .any(|pane| self.engine.panes[pane].terminal.cell_size() != cell);
            if (cols, rows) != (window.cols, window.rows) || pixels_changed {
                sizes.push((window.id, cols, rows, cell));
            }
        }
        for (window, cols, rows, cell) in sizes {
            let mut pixel_resizes = Vec::new();
            for pane in self.engine.windows[&window].panes.clone() {
                let pane = self.engine.panes.get_mut(&pane).unwrap();
                if pane.terminal.cell_size() != cell {
                    pane.terminal.set_cell_size(cell.0, cell.1);
                    pixel_resizes.push(pane.id);
                }
            }
            if let Ok(mut out) = self.engine.resize_window(window, cols, rows) {
                out.effects.extend(pixel_resizes.into_iter().map(|id| {
                    let pane = &self.engine.panes[&id];
                    Effect::Resize {
                        pane: id,
                        cols: pane.cols,
                        rows: pane.rows,
                    }
                }));
                self.engine.sequence += 1;
                if let Err(error) = self.effects(out.effects) {
                    eprintln!("tmuxx: resize: {error}");
                }
                for data in out.notifications {
                    self.broadcast_notification(data);
                }
                self.broadcast(ServerMessage::Changed {
                    sequence: self.engine.sequence,
                });
            }
        }
    }

    pub(super) fn client_became_latest(&mut self, client: u64, window: Id) {
        self.size_activity.insert(client, Instant::now());
        self.sizes_pending |= self.latest_clients.insert(window, client) != Some(client);
    }
}
