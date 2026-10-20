use super::*;
use tmuxx_layout::Geometry;

impl Engine {
    pub(super) fn create_pane_command(
        &mut self,
        name: &str,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let split = name == "split-window";
        let a = Args::parse(
            args,
            if split {
                "bB:c:de:EfF:hIkl:m:p:PR:s:S:t:T:vWZ"
            } else {
                "AbB:Cc:Dde:EfF:hIkl:KLMm:Op:PR:s:S:t:T:vWx:X:y:Y:Z"
            },
        )?;
        a.reject(b"mSR")?;
        let (sid, wid, target) = self.pane_context(a.string(b't').as_deref(), ctx)?;
        let window = &self.windows[&wid];
        let floating = if split {
            window.layout.find(target as u32).unwrap().is_floating()
        } else {
            !a.has(b'L')
        };
        if a.has(b'O') {
            if !floating {
                return Err("modal pane must be floating".into());
            }
            if window.modal.is_some() {
                return Err("window already has a modal pane".into());
            }
        }
        if floating && a.has(b'M') {
            return Ok(Outcome::default());
        }
        let empty = Self::empty_requested(&a, true)?;
        let style = a.string(b's');
        if let Some(style) = &style {
            styles::validate(style).map_err(|error| {
                if error.starts_with("invalid style:") {
                    format!("unsupported pane creation style in native-client profile: {style}")
                } else {
                    error
                }
            })?;
        }
        let border = a.string(b'B').unwrap_or_else(|| {
            self.pane_option(target, "pane-border-lines")
                .unwrap_or("single")
                .to_owned()
        });
        if ![
            "single", "double", "heavy", "simple", "number", "spaces", "none", "rounded",
        ]
        .contains(&border.as_str())
        {
            return Err(format!("pane-border-lines unknown value: {border}"));
        }
        if let Some(format) = a.get(b'F') {
            self.expand(format, Some(sid), Some(wid), Some(target), ctx)?;
        }
        let title = a
            .get(b'T')
            .map(|raw| self.expand(raw, Some(sid), Some(wid), Some(target), ctx))
            .transpose()?;
        let id = u32::try_from(self.next_pane).map_err(|_| "pane ID space exhausted")?;
        let axis = if a.has(b'h') {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        let mut layout = window.layout.clone();
        let mut last = window.last_float;
        if floating {
            if split {
                layout
                    .split_floating(
                        target as u32,
                        id,
                        axis,
                        a.has(b'b'),
                        a.has(b'f'),
                        border != "none",
                        (window.cols as u32, window.rows as u32),
                    )
                    .map_err(|e| e.to_string())?;
            } else {
                let (geometry, next) =
                    self.floating_geometry(target, &a, ctx, true).map_err(|e| {
                        e.strip_prefix("failed to float pane: ")
                            .unwrap_or(&e)
                            .to_owned()
                    })?;
                last = next;
                layout
                    .insert_floating(target as u32, id, geometry)
                    .map_err(|e| e.to_string())?;
            }
            if window.modal.is_some() {
                layout.set_floating_z(id, 1).map_err(|e| e.to_string())?;
            }
        } else {
            if layout.find(target as u32).unwrap().is_floating() {
                return Err("can't split a floating pane".into());
            }
            let original = if a.has(b'f') {
                layout.geometry
            } else {
                layout.find(target as u32).unwrap().geometry
            };
            let content = self.pane_content_geometry(wid, original, layout.geometry, false);
            let extent = if axis == Axis::Horizontal {
                original.width
            } else if a.has(b'f') {
                original.height
            } else {
                content.height
            };
            if axis == Axis::Vertical && content.height != original.height && original.height < 4 {
                return Err("no space for a new pane".into());
            }
            let requested = a.get(b'l').or_else(|| a.get(b'p'));
            let size = requested
                .map(|raw| -> Result<u32, String> {
                    let expanded = self.expand(raw, Some(sid), Some(wid), Some(target), ctx)?;
                    let value = text(&expanded);
                    let percent = value.ends_with('%') || !a.has(b'l');
                    let value = value.strip_suffix('%').unwrap_or(&value);
                    let n = value
                        .parse::<i64>()
                        .map_err(|_| "invalid tiled geometry invalid")?;
                    if n < 0 {
                        return Err("invalid tiled geometry too small".into());
                    }
                    if n > if a.has(b'l') { i32::MAX as i64 } else { 100 } {
                        return Err("invalid tiled geometry too large".into());
                    }
                    let n = if percent {
                        i64::from(extent) * n / 100
                    } else {
                        n
                    };
                    u32::try_from(n).map_err(|_| "invalid tiled geometry too large".into())
                })
                .transpose()?;
            if a.has(b'f') {
                layout.split_full_sized(id, axis, size, a.has(b'b'))
            } else {
                layout.split_sized(target as u32, id, axis, size, a.has(b'b'))
            }
            .map_err(|e| e.to_string())?;
        }
        let geometry = self.pane_content_geometry(
            wid,
            layout.find(id).unwrap().geometry,
            layout.geometry,
            floating,
        );
        let environment = self.pane_environment(sid, ctx, process::environment_overrides(&a)?);
        let cwd = a.get(b'c').map(admission::path).transpose()?;
        let mut out = Outcome::default();
        let pane = self.create_pane(
            wid,
            sid,
            geometry.width as u16,
            geometry.height as u16,
            a.rest.clone(),
            cwd,
            environment,
            &mut out,
        );
        if empty {
            self.make_empty(pane, &mut out);
        }
        if a.has(b'I') {
            out.effects.push(Effect::FeedStdin { pane });
        } else if a.has(b'W') {
            out.effects.push(Effect::WaitPane { pane });
        }
        if let Some(title) = title {
            self.panes
                .get_mut(&pane)
                .unwrap()
                .terminal
                .set_title(&title);
        }
        if a.has(b'O') {
            let state = self.panes.get_mut(&pane).unwrap();
            state.modal_capture_keys = a.has(b'K');
            state.modal_close_click = a.has(b'C');
            state.modal_close_cancel = a.has(b'D');
        }
        if a.has(b'B') {
            self.panes
                .get_mut(&pane)
                .unwrap()
                .options
                .insert("pane-border-lines".into(), border);
        }
        if a.has(b'k') {
            self.panes
                .get_mut(&pane)
                .unwrap()
                .options
                .insert("remain-on-exit".into(), "key".into());
        }
        if let Some(style) = style {
            let options = &mut self.panes.get_mut(&pane).unwrap().options;
            options.insert("window-style".into(), style.clone());
            options.insert("window-active-style".into(), style);
        }
        let window = self.windows.get_mut(&wid).unwrap();
        window.layout = layout;
        window.last_float = last;
        let at = window.panes.iter().position(|id| *id == target).unwrap();
        let at = if a.has(b'b') {
            if a.has(b'f') { 0 } else { at }
        } else if floating || a.has(b'f') {
            window.panes.len()
        } else {
            at + 1
        };
        window.panes.insert(at, pane);
        if a.has(b'O') {
            window.modal = Some(pane);
        }
        if floating && (a.has(b'A') || a.has(b'O')) {
            window.over_zoom.insert(pane);
        }
        if !window.is_over_zoom(pane) && !window.is_over_zoom(window.active) {
            window.zoomed = false;
        }
        if a.has(b'O') || !a.has(b'd') {
            window.select(pane);
            out.notifications.push(
                format!("%window-pane-changed @{wid} %{pane}")
                    .into_bytes()
                    .into(),
            );
        }
        if a.has(b'Z') {
            window.zoomed = true;
            window.zoomed_pane = Some(window.active);
        }
        if window.zoomed {
            window.save_zoom_geometry();
        }
        self.finish_pane_layouts(&[wid], &mut out);
        if a.has(b'P') {
            line(
                &mut out.output,
                self.expand(
                    a.get(b'F')
                        .unwrap_or(b"#{session_name}:#{window_index}.#{pane_index}"),
                    Some(sid),
                    Some(wid),
                    Some(pane),
                    ctx,
                )?,
            );
        }
        Ok(out)
    }

    fn floating_geometry(
        &self,
        pane: Id,
        a: &Args,
        ctx: &ClientContext,
        new: bool,
    ) -> Result<(Geometry, (i32, i32)), String> {
        let wid = self.panes[&pane].window;
        let window = &self.windows[&wid];
        let border_lines = a.string(b'B').unwrap_or_else(|| {
            window
                .options
                .get("pane-border-lines")
                .or_else(|| self.global_window_options.get("pane-border-lines"))
                .map(String::as_str)
                .unwrap_or("single")
                .to_owned()
        });
        let border = i32::from(border_lines != "none");
        let old = if new {
            None
        } else {
            self.panes[&pane].saved_float
        };
        let (sid, _, _) = self.window_owner(wid, ctx)?;
        let number = |flag, limit: i32, min: i32, max: i32| -> Result<Option<i32>, String> {
            let Some(raw) = a.get(flag) else {
                return Ok(None);
            };
            let value = self.expand(raw, Some(sid), Some(wid), Some(pane), ctx)?;
            let value = text(&value);
            let parsed = if let Some(value) = value.strip_suffix('%') {
                value
                    .parse::<i64>()
                    .ok()
                    .and_then(|n| n.checked_mul(limit as i64))
                    .map(|n| n / 100)
            } else {
                value.parse::<i64>().ok()
            };
            let n = parsed.ok_or("failed to float pane: position invalid")?;
            if n < min as i64 {
                return Err("failed to float pane: position too small".into());
            }
            if n > max as i64 {
                return Err("failed to float pane: position too large".into());
            }
            Ok(Some(n as i32))
        };
        let width = number(b'x', window.cols as i32, 0, i32::MAX)?
            .map(|n| n - border * 2)
            .unwrap_or_else(|| old.map_or(window.cols as i32 / 2, |g| g.width as i32));
        let height = number(b'y', window.rows as i32, 0, i32::MAX)?
            .map(|n| n - border * 2)
            .unwrap_or_else(|| old.map_or(window.rows as i32 / 4, |g| g.height as i32));
        if width <= 0 || width > tmuxx_layout::MAX_DIMENSION as i32 {
            return Err("failed to float pane: invalid width".into());
        }
        if height <= 0 || height > tmuxx_layout::MAX_DIMENSION as i32 {
            return Err("failed to float pane: invalid height".into());
        }
        let mut last = if window.layout.panes().iter().any(|p| p.z.is_some()) {
            window.last_float
        } else {
            (0, 0)
        };
        let x = number(b'X', window.cols as i32, -width, window.cols as i32)?
            .map(|n| n + border)
            .or_else(|| old.map(|g| g.x))
            .unwrap_or_else(|| {
                last.0 = if last.0 == 0 || last.0 + 4 + width + border > window.cols as i32 {
                    4
                } else {
                    last.0 + 4
                };
                last.0
            });
        let y = number(b'Y', window.rows as i32, -height, window.rows as i32)?
            .map(|n| n + border)
            .or_else(|| old.map(|g| g.y))
            .unwrap_or_else(|| {
                last.1 = if last.1 == 0 || last.1 + 2 + height + border > window.rows as i32 {
                    2
                } else {
                    last.1 + 2
                };
                last.1
            });
        let geometry = Geometry {
            width: width as u32,
            height: height as u32,
            x,
            y,
        };
        Ok((geometry, last))
    }

    pub(super) fn move_floating(
        &mut self,
        pane: Id,
        a: &Args,
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let wid = self.panes[&pane].window;
        let window = &self.windows[&wid];
        let cell = window.layout.find(pane as u32).unwrap();
        if !cell.is_floating() || !window.pane_visible(pane) || window.zoom_target() == Some(pane) {
            return Err("pane is not floating".into());
        }
        let mut geometry = cell.geometry;
        let (wx, wy, px, py) = (
            i32::from(window.cols),
            i32::from(window.rows),
            geometry.width as i32,
            geometry.height as i32,
        );
        let border = i32::from(self.pane_option(pane, "pane-border-lines") != Some("none"));
        let mut order: Vec<_> = window
            .layout
            .panes()
            .iter()
            .filter_map(|p| p.z.zip(p.id))
            .collect();
        order.sort_unstable();
        let at = order
            .iter()
            .position(|(_, id)| u64::from(*id) == pane)
            .unwrap();
        let visible = |index: usize| window.pane_visible(order[index].1 as Id);
        let previous = (0..at).rev().find(|index| visible(*index));
        let next = (at + 1..order.len()).find(|index| visible(*index));
        let mut z = None;
        if let Some(position) = a.string(b'P') {
            let position = position.replace("centre", "center");
            let pos = match position.as_str() {
                "top-left" => Some((border, border)),
                "top-center" => Some(((wx - px) / 2, border)),
                "top-right" => Some((wx - px - border, border)),
                "center-left" => Some((border, (wy - py) / 2)),
                "center" => Some(((wx - px) / 2, (wy - py) / 2)),
                "center-right" => Some((wx - px - border, (wy - py) / 2)),
                "bottom-left" => Some((border, wy - py - border)),
                "bottom-center" => Some(((wx - px) / 2, wy - py - border)),
                "bottom-right" => Some((wx - px - border, wy - py - border)),
                "top-left-center" => Some((wx / 4 - px / 2, wy / 4 - py / 2)),
                "top-right-center" => Some((3 * wx / 4 - px / 2, wy / 4 - py / 2)),
                "bottom-left-center" => Some((wx / 4 - px / 2, 3 * wy / 4 - py / 2)),
                "bottom-right-center" => Some((3 * wx / 4 - px / 2, 3 * wy / 4 - py / 2)),
                "front" => {
                    z = Some(0);
                    None
                }
                "back" => {
                    z = Some(order.len() - 1);
                    None
                }
                "forward" => {
                    z = previous;
                    None
                }
                "backward" => {
                    z = next;
                    None
                }
                "forward-loop" => {
                    z = Some(previous.unwrap_or(order.len() - 1));
                    None
                }
                "backward-loop" => {
                    z = Some(next.unwrap_or(0));
                    None
                }
                _ => return Err(format!("unknown position: {position}")),
            };
            if let Some((x, y)) = pos {
                geometry.x = x;
                geometry.y = y;
            }
        } else if let Some(value) = a.string(b'z') {
            let requested = value.parse::<u32>().map_err(|_| "z-index invalid")? as usize;
            z = Some(
                order
                    .iter()
                    .filter(|(_, id)| *id as Id != pane)
                    .enumerate()
                    .filter(|(_, (_, id))| window.pane_visible(*id as Id))
                    .nth(requested)
                    .map_or(order.len() - 1, |(index, _)| index),
            );
        } else {
            let (sid, _, _) = self.window_owner(wid, ctx)?;
            for (flag, limit, value) in [(b'X', wx, &mut geometry.x), (b'Y', wy, &mut geometry.y)] {
                if let Some(raw) = a.get(flag) {
                    let raw = self.expand(raw, Some(sid), Some(wid), Some(pane), ctx)?;
                    let raw = text(&raw);
                    let parsed = if let Some(p) = raw.strip_suffix('%') {
                        p.parse::<i64>()
                            .ok()
                            .and_then(|n| n.checked_mul(limit as i64))
                            .map(|n| n / 100)
                    } else {
                        raw.parse().ok()
                    }
                    .ok_or("position invalid")?;
                    if parsed < -(limit as i64) {
                        return Err("position too small".into());
                    }
                    if parsed > limit as i64 {
                        return Err("position too large".into());
                    }
                    *value = parsed as i32 + border;
                }
            }
            for (flag, horizontal, sign) in [
                (b'U', false, -1_i64),
                (b'D', false, 1),
                (b'L', true, -1),
                (b'R', true, 1),
            ] {
                if a.has(flag) {
                    let amount = a
                        .string(flag)
                        .unwrap_or_else(|| "1".into())
                        .parse::<i32>()
                        .map_err(|_| "offset invalid")?;
                    let value = if horizontal {
                        &mut geometry.x
                    } else {
                        &mut geometry.y
                    };
                    *value = i32::try_from(i64::from(*value) + i64::from(amount) * sign)
                        .map_err(|_| "offset too large")?;
                }
            }
        }
        let window = self.windows.get_mut(&wid).unwrap();
        window
            .layout
            .position_floating(pane as u32, geometry)
            .map_err(|e| e.to_string())?;
        if let Some(z) = z {
            let item = order.remove(at);
            order.insert(z.min(order.len()), item);
            for (z, (_, id)) in order.into_iter().enumerate() {
                if let tmuxx_layout::Kind::Pane(p) = &mut window.layout.find_mut(id).unwrap().kind {
                    p.z = Some(z as u32);
                }
            }
        }
        let mut out = Outcome::default();
        self.finish_pane_layouts(&[wid], &mut out);
        Ok(out)
    }

    pub(super) fn float_pane(
        &mut self,
        pane: Id,
        a: &Args,
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let wid = self.panes[&pane].window;
        let window = &self.windows[&wid];
        if window.layout.find(pane as u32).unwrap().is_floating() {
            return Err("pane is already floating".into());
        }
        if window.zoomed {
            return Err("can't float a pane while window is zoomed".into());
        }
        if window.modal == Some(pane) {
            return Err("pane is modal".into());
        }
        let (geometry, last) = self.floating_geometry(pane, a, ctx, false)?;
        let window = self.windows.get_mut(&wid).unwrap();
        window
            .layout
            .float_pane(pane as u32, geometry)
            .map_err(|e| e.to_string())?;
        window.last_float = last;
        if !a.has(b'd') {
            window.select(pane);
        }
        self.panes.get_mut(&pane).unwrap().saved_float = Some(geometry);
        let mut out = Outcome::default();
        self.finish_pane_layouts(&[wid], &mut out);
        Ok(out)
    }

    pub(super) fn tile_pane(&mut self, pane: Id, detached: bool) -> Result<Outcome, String> {
        let wid = self.panes[&pane].window;
        let window = self.windows.get_mut(&wid).unwrap();
        if window.zoomed {
            return Err("can't tile a pane while window is zoomed".into());
        }
        let geometry = Geometry {
            width: window.cols as u32,
            height: window.rows as u32,
            x: 0,
            y: 0,
        };
        let saved = window.layout.find(pane as u32).unwrap().geometry;
        window
            .layout
            .tile_pane(pane as u32, geometry)
            .map_err(|e| e.to_string())?;
        self.panes.get_mut(&pane).unwrap().saved_float = Some(saved);
        if !detached {
            window.select(pane);
        }
        let mut out = Outcome::default();
        self.finish_pane_layouts(&[wid], &mut out);
        Ok(out)
    }
}
