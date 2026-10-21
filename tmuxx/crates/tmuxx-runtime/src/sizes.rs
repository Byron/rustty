use super::*;

impl Host {
    pub(super) fn size_view_matches_session(&self, client: u64, context: &ClientContext) -> bool {
        self.size_views
            .iter()
            .find(|(id, _)| *id == client)
            .is_none_or(|(_, session)| {
                context.session == Some(*session) && self.engine.sessions.contains_key(session)
            })
    }

    pub(super) fn clear_stale_view_size(&self, client: u64, context: &mut ClientContext) {
        if !self.size_view_matches_session(client, context) {
            context.size_requested = false;
            context.window_size_requested = false;
            context.window_sizes.clear();
            context.ignore_size = true;
        }
    }

    pub(super) fn view_size(
        &mut self,
        client: u64,
        command: &tmuxx_command::Command,
        context: &mut ClientContext,
    ) -> Result<Outcome, String> {
        let args = tmuxx_command::Args::parse(&command.args[1..], "ft:")?;
        if args.rest.len() != 4 {
            return Err("expected columns, rows, cell width and cell height".into());
        }
        let values = args
            .rest
            .iter()
            .map(|value| {
                std::str::from_utf8(value)
                    .ok()
                    .and_then(|value| value.parse::<u16>().ok())
                    .filter(|value| (1..=10_000).contains(value))
            })
            .collect::<Option<Vec<_>>>()
            .ok_or("invalid view size")?;
        let session = args
            .get(b't')
            .and_then(|value| value.strip_prefix(b"$"))
            .and_then(|value| std::str::from_utf8(value).ok())
            .and_then(|value| value.parse::<Id>().ok())
            .filter(|session| self.engine.sessions.contains_key(session))
            .ok_or("view size requires an existing session ID (-t $ID)")?;
        if context.session != Some(session) {
            return Err("view size client is attached to a different session".into());
        }
        if !self.size_view_matches_session(client, context) {
            return Err("view size client belongs to a different session".into());
        }
        if args.has(b'f') {
            self.size_views.retain(|(id, _)| *id != client);
            self.size_views.insert(0, (client, session));
        } else if !self.size_views.iter().any(|(id, _)| *id == client) {
            self.size_views.push((client, session));
        }
        context.cols = values[0];
        context.rows = values[1];
        context.cell_width = values[2];
        context.cell_height = values[3];
        context.size_requested = true;
        context.resize_order = self.engine.sequence + 1;
        let windows = self.engine.sessions[&session]
            .windows
            .values()
            .copied()
            .collect::<Vec<_>>();
        for window in windows {
            if args.has(b'f') {
                self.engine
                    .windows
                    .get_mut(&window)
                    .unwrap()
                    .options
                    .insert("window-size".into(), "latest".into());
            }
            let owner =
                self.size_views.iter().find(|(id, session)| {
                    self.clients.get(id).is_some_and(|client| {
                        client.context.session == Some(*session)
                            && self.engine.sessions.get(session).is_some_and(|session| {
                                session.windows.values().any(|id| *id == window)
                            })
                    })
                });
            if owner.is_some_and(|(id, _)| *id == client) {
                self.client_became_latest(client, window);
            }
        }
        self.sizes_pending = true;
        Ok(Outcome {
            changed: true,
            ..Outcome::default()
        })
    }

    pub(super) fn command_sizes(&mut self, command: &tmuxx_command::Command) -> bool {
        if command
            .args
            .first()
            .is_some_and(|name| name == b"tmuxx-view-size")
        {
            return true;
        }
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
        self.size_views
            .retain(|(id, _)| self.clients.contains_key(id));
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
        let unflagged = self.clients.iter().any(|(id, client)| {
            self.size_view_matches_session(*id, &client.context)
                && client.context.session.is_some()
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
                .filter(|(id, client)| {
                    let c = &client.context;
                    self.size_view_matches_session(**id, c)
                        && (c.size_requested || c.window_size_requested)
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
            // GUI input and size reports arrive on different connections.
            let owner = self
                .size_views
                .iter()
                .find(|(view, _)| candidates.iter().any(|(id, _)| *id == view))
                .map(|(id, _)| *id);
            let latest = self
                .latest_clients
                .get(&window.id)
                .copied()
                .filter(|latest| {
                    candidates.iter().any(|(id, _)| {
                        **id == *latest
                            && (!self.size_views.iter().any(|(view, _)| view == *id)
                                || Some(**id) == owner)
                    })
                })
                .or(owner);
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
