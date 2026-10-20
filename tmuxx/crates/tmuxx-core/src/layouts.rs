use super::*;
use tmuxx_layout::PRESETS;

fn percentage(value: &str, available: u32) -> Option<u32> {
    match value.strip_suffix('%') {
        Some(value) => value
            .parse::<u32>()
            .ok()?
            .checked_mul(available)
            .map(|v| v / 100),
        None => value.parse().ok(),
    }
    .filter(|v| *v <= available)
}

impl Engine {
    pub(super) fn pane_content_geometry(
        &self,
        wid: Id,
        mut geometry: tmuxx_layout::Geometry,
        root: tmuxx_layout::Geometry,
        floating: bool,
    ) -> tmuxx_layout::Geometry {
        if floating {
            return geometry;
        }
        let window = &self.windows[&wid];
        let status = window
            .options
            .get("pane-border-status")
            .or_else(|| self.global_window_options.get("pane-border-status"))
            .map(String::as_str)
            .unwrap_or("off");
        let top = status == "top" && geometry.y == root.y;
        let bottom = status == "bottom"
            && geometry.y + geometry.height as i32 == root.y + root.height as i32;
        if top {
            geometry.y += 1;
        }
        if (top || bottom) && geometry.height > 1 {
            geometry.height -= 1;
        }
        geometry
    }
    pub(super) fn clamp_floating(&self, layout: &mut Layout, cols: u16, rows: u16) {
        let ids: Vec<_> = layout
            .panes()
            .iter()
            .filter(|p| p.z.is_some())
            .filter_map(|p| p.id)
            .collect();
        for id in ids {
            let pad = u32::from(self.pane_option(id as Id, "pane-border-lines") != Some("none"));
            let geometry = &mut layout.find_mut(id).unwrap().geometry;
            for (size, offset, limit) in [
                (&mut geometry.width, &mut geometry.x, cols as u32),
                (&mut geometry.height, &mut geometry.y, rows as u32),
            ] {
                *size = (*size).min(limit.saturating_sub(2 * pad).max(1));
                if i64::from(*offset) + i64::from(*size + pad) > i64::from(limit) {
                    *offset = if *size + 2 * pad >= limit {
                        pad
                    } else {
                        limit - *size - pad
                    } as i32;
                }
            }
        }
    }
    fn assign_layout(&self, wid: Id, layout: Layout) -> Result<Layout, String> {
        let window = &self.windows[&wid];
        let legacy = layout.panes().iter().all(|p| p.index.is_none());
        let ids: Vec<_> = window
            .panes
            .iter()
            .map(|id| *id as u32)
            .filter(|id| !legacy || !window.layout.find(*id).unwrap().is_floating())
            .collect();
        if ids.is_empty() {
            return Err(format!("window @{wid} has no panes"));
        }
        let mut layout = layout.for_panes(&ids).map_err(|e| e.to_string())?;
        if legacy {
            let floating = window
                .panes
                .iter()
                .filter_map(|id| {
                    let pane = window.layout.find(*id as u32).unwrap();
                    pane.is_floating().then(|| pane.clone())
                })
                .collect();
            layout
                .append_floating(floating, false)
                .map_err(|e| e.to_string())?;
        }
        Ok(layout)
    }
    pub(super) fn resize_pane(
        &mut self,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(args, "D::L::MR::Tt:U::x:y:Z")?;
        if a.rest.len() > 1 {
            return Err("too many arguments".into());
        }
        let pane = self.pane_target(a.string(b't').as_deref(), ctx)?;
        if a.has(b'T') {
            self.panes
                .get_mut(&pane)
                .unwrap()
                .terminal
                .trim_history_below_cursor();
            return Ok(Outcome {
                changed: true,
                ..Outcome::default()
            });
        }
        if a.has(b'M') {
            return Ok(Outcome::default());
        }
        let wid = self.panes[&pane].window;
        let border = self.pane_option(pane, "pane-border-lines") != Some("none");
        let window = self.windows.get_mut(&wid).unwrap();
        if a.has(b'Z') {
            if window.zoomed {
                window.zoomed = false;
            } else if window.panes.len() > 1 {
                if !window.over_zoom.contains(&window.active) {
                    window.select(pane);
                }
                window.zoomed = true;
                window.zoomed_pane = Some(pane);
                window.save_zoom_geometry();
            }
        } else {
            let mut layout = window.layout.clone();
            let floating = layout.find(pane as u32).unwrap().is_floating();
            let mut geometry = layout.find(pane as u32).unwrap().geometry;
            for (flag, axis, extent, label) in [
                (b'x', Axis::Horizontal, window.cols, "width"),
                (b'y', Axis::Vertical, window.rows, "height"),
            ] {
                if let Some(value) = a.string(flag) {
                    let size = if let Some(p) = value.strip_suffix('%') {
                        p.parse::<u32>()
                            .ok()
                            .and_then(|p| p.checked_mul(extent as u32))
                            .map(|p| p / 100)
                    } else {
                        value.parse::<u32>().ok()
                    }
                    .filter(|v| *v <= i32::MAX as u32)
                    .ok_or_else(|| format!("{label} invalid"))?;
                    if floating {
                        let size = if border && size >= 3 { size - 2 } else { size };
                        if size == 0 || size > tmuxx_layout::MAX_DIMENSION {
                            return Err("size size is too big or too small".into());
                        }
                        if axis == Axis::Horizontal {
                            geometry.width = size;
                        } else {
                            geometry.height = size;
                        }
                    } else {
                        layout
                            .resize_pane_to(
                                pane as u32,
                                axis,
                                size.clamp(1, tmuxx_layout::MAX_DIMENSION),
                            )
                            .map_err(|e| e.to_string())?;
                    }
                }
            }
            let mut opposite = false;
            for (flag, axis, negative) in [
                (b'U', Axis::Vertical, true),
                (b'D', Axis::Vertical, false),
                (b'L', Axis::Horizontal, true),
                (b'R', Axis::Horizontal, false),
            ] {
                if !a.has(flag) {
                    continue;
                }
                let value = a
                    .get(flag)
                    .or_else(|| a.rest.first().map(Vec::as_slice))
                    .unwrap_or(b"1");
                let value = text(value);
                let amount = value.parse::<i32>().map_err(|_| {
                    if value.parse::<i64>().is_ok() {
                        if value.starts_with('-') {
                            "adjustment too small"
                        } else {
                            "adjustment too large"
                        }
                    } else {
                        "adjustment invalid"
                    }
                })?;
                if floating {
                    opposite |= negative;
                    let (size, offset) = if axis == Axis::Horizontal {
                        (&mut geometry.width, &mut geometry.x)
                    } else {
                        (&mut geometry.height, &mut geometry.y)
                    };
                    let next = i64::from(*size) + i64::from(amount);
                    if !(1..=i64::from(tmuxx_layout::MAX_DIMENSION)).contains(&next) {
                        return Err("adjustment change is too big or too small".into());
                    }
                    *size = next as u32;
                    if opposite {
                        *offset = offset.checked_sub(amount).ok_or("offset too large")?;
                    }
                } else {
                    layout
                        .resize_border(
                            pane as u32,
                            axis,
                            if negative {
                                amount.saturating_neg()
                            } else {
                                amount
                            },
                        )
                        .map_err(|e| e.to_string())?;
                }
            }
            if floating {
                layout
                    .position_floating(pane as u32, geometry)
                    .map_err(|e| e.to_string())?;
            }
            window.layout = layout;
            if !floating || !window.over_zoom.contains(&pane) || window.zoom_target() == Some(pane)
            {
                window.zoomed = false;
            }
        }
        let mut out = Outcome {
            changed: true,
            ..Outcome::default()
        };
        self.sync_layout(wid, &mut out);
        out.notifications.push(self.layout_event(wid));
        Ok(out)
    }

    pub(super) fn select_layout(
        &mut self,
        name: &str,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(
            args,
            if matches!(name, "select-layout" | "selectl") {
                "Enopt:"
            } else {
                "t:"
            },
        )?;
        if a.rest.len() > usize::from(matches!(name, "select-layout" | "selectl")) {
            return Err("too many arguments".into());
        }
        let (_, wid, pane) = self.pane_context(a.string(b't').as_deref(), ctx)?;
        let window = &self.windows[&wid];
        let old =
            Layout::parse(&self.formatted_layout(wid, false, ctx)).expect("engine layout is valid");
        let next = a.has(b'n') || matches!(name, "next-layout" | "nextl");
        let previous = a.has(b'p') || matches!(name, "previous-layout" | "prevl");
        let selected = a.rest.first().map(|v| text(v));
        let preset = if next {
            Some(window.last_layout.map_or(0, |n| (n + 1) % PRESETS.len()))
        } else if previous {
            Some(window.last_layout.map_or(PRESETS.len() - 1, |n| {
                (n + PRESETS.len() - 1) % PRESETS.len()
            }))
        } else if a.has(b'E') || a.has(b'o') {
            None
        } else if let Some(name) = &selected {
            PRESETS.iter().position(|p| *p == name).or_else(|| {
                let mut matches = PRESETS
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.starts_with(name));
                let (index, _) = matches.next()?;
                matches.next().is_none().then_some(index)
            })
        } else {
            window.last_layout
        };
        let ids: Vec<_> = window.panes.iter().map(|id| *id as u32).collect();
        let mut restore_selection = false;
        let layout = if let Some(preset) = preset {
            let tiled: Vec<_> = ids
                .iter()
                .copied()
                .filter(|id| !window.layout.find(*id).unwrap().is_floating())
                .collect();
            let floating: Vec<_> = ids
                .iter()
                .filter_map(|id| {
                    let pane = window.layout.find(*id).unwrap();
                    pane.is_floating().then(|| pane.clone())
                })
                .collect();
            let vertical = preset >= 4;
            let available =
                if vertical { window.cols } else { window.rows }.saturating_sub(1) as u32;
            let (main_key, other_key, fallback) = if vertical {
                ("main-pane-width", "other-pane-width", 80)
            } else {
                ("main-pane-height", "other-pane-height", 24)
            };
            let main = self
                .pane_option(pane, main_key)
                .and_then(|v| percentage(v, available))
                .unwrap_or(fallback);
            let other = self
                .pane_option(pane, other_key)
                .and_then(|v| percentage(v, available))
                .unwrap_or(0);
            let columns = self
                .pane_option(pane, "tiled-layout-max-columns")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if tiled.len() < 2 {
                window.layout.clone()
            } else {
                let geometry = tmuxx_layout::Geometry {
                    width: window.cols as u32,
                    height: window.rows as u32,
                    x: 0,
                    y: 0,
                };
                let mut layout = Layout::preset(&tiled, geometry, preset, main, other, columns)
                    .map_err(|e| e.to_string())?;
                layout
                    .append_floating(floating, (2..=5).contains(&preset) && tiled.len() > 2)
                    .map_err(|e| e.to_string())?;
                layout
            }
        } else if a.has(b'E') {
            let mut layout = window.layout.clone();
            layout.spread(pane as u32).map_err(|e| e.to_string())?;
            layout
        } else if a.has(b'o') && selected.is_none() {
            if let Some(layout) = &window.old_layout {
                restore_selection = layout.panes().iter().any(|p| p.index.is_some());
                self.assign_layout(wid, layout.clone())?
            } else {
                window.layout.clone()
            }
        } else if let Some(name) = selected {
            if name.starts_with('{') {
                tmuxx_command::json::canonicalize(name.as_bytes())
                    .map_err(|e| format!("{e}: {name}"))?;
            }
            let layout = Layout::parse_command(&name).map_err(|e| format!("{e}: {name}"))?;
            restore_selection = layout.panes().iter().any(|p| p.index.is_some());
            self.assign_layout(wid, layout)
                .map_err(|e| format!("{e}: {name}"))?
        } else {
            window.layout.clone()
        };
        let window = self.windows.get_mut(&wid).unwrap();
        window.old_layout = Some(old);
        window.layout = layout;
        if let Some(preset) = preset {
            window.last_layout = Some(preset);
        }
        if restore_selection {
            let panes = window.layout.panes();
            if let Some(pane) = panes.iter().find(|p| p.active) {
                window.active = pane.id.unwrap() as Id;
            }
            let mut last: Vec<_> = panes
                .iter()
                .filter(|p| !p.active)
                .filter_map(|p| p.last.map(|last| (last, p.id.unwrap() as Id)))
                .collect();
            last.sort_unstable();
            window.last = last.into_iter().map(|(_, id)| id).collect();
        }
        if window.layout.has_tiled() {
            window.cols = window.layout.geometry.width as u16;
            window.rows = window.layout.geometry.height as u16;
        }
        window.zoomed = false;
        let mut out = Outcome {
            changed: true,
            ..Outcome::default()
        };
        self.sync_layout(wid, &mut out);
        out.notifications.push(self.layout_event(wid));
        Ok(out)
    }
}
