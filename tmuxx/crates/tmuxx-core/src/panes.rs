use super::*;

fn remap(layout: &mut Layout, ids: &[(Id, Id)]) {
    match &mut layout.kind {
        tmuxx_layout::Kind::Pane(pane) => {
            if let Some(id) = pane.id
                && let Some((_, replacement)) = ids.iter().find(|(old, _)| *old == u64::from(id))
            {
                pane.id = Some(*replacement as u32);
            }
        }
        tmuxx_layout::Kind::Split { children, .. } => {
            for child in children {
                remap(child, ids);
            }
        }
    }
}
impl Engine {
    pub(super) fn finish_pane_layouts(&mut self, windows: &[Id], out: &mut Outcome) {
        let mut seen = BTreeSet::new();
        for wid in windows.iter().copied().filter(|wid| seen.insert(*wid)) {
            if self.windows.contains_key(&wid) {
                self.sync_layout(wid, out);
                out.notifications.push(self.layout_event(wid));
            }
        }
        out.changed = true;
    }
    pub(super) fn rotate_window(
        &mut self,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(args, "DUZt:")?;
        a.no_rest()?;
        let (_, _, wid) = self.window_target(a.string(b't').as_deref(), ctx)?;
        let zoomed = self.windows[&wid].zoom_target();
        let active = self.windows[&wid].active;
        self.windows.get_mut(&wid).unwrap().zoomed = false;
        let mut out = Outcome::default();
        self.sync_layout(wid, &mut out);
        if zoomed.is_some() {
            out.notifications.push(self.layout_event(wid));
        }
        let window = self.windows.get_mut(&wid).unwrap();
        let old = window.panes.clone();
        let saved: Vec<_> = old.iter().map(|id| self.panes[id].saved_float).collect();
        let active_index = old.iter().position(|id| *id == window.active).unwrap();
        if a.has(b'D') {
            window.panes.rotate_right(1);
        } else {
            window.panes.rotate_left(1);
        }
        remap(
            &mut window.layout,
            &old.into_iter()
                .zip(window.panes.iter().copied())
                .collect::<Vec<_>>(),
        );
        for (id, saved) in window.panes.iter().zip(saved) {
            self.panes.get_mut(id).unwrap().saved_float = saved;
        }
        window.select(window.panes[active_index]);
        if active != window.active {
            out.notifications.push(
                format!("%window-pane-changed @{wid} %{}", window.active)
                    .into_bytes()
                    .into(),
            );
        }
        self.sync_layout(wid, &mut out);
        let window = self.windows.get_mut(&wid).unwrap();
        if a.has(b'Z') && zoomed.is_some() {
            window.zoomed = true;
            window.zoomed_pane = if window.is_over_zoom(window.active) {
                zoomed
            } else {
                Some(window.active)
            };
            window.save_zoom_geometry();
        }
        self.sync_layout(wid, &mut out);
        if a.has(b'Z') && zoomed.is_some() {
            out.notifications.push(self.layout_event(wid));
        }
        out.changed = true;
        Ok(out)
    }
    pub(super) fn swap_pane(
        &mut self,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(args, "dDUs:t:Z")?;
        a.no_rest()?;
        let target = self.pane_target(a.string(b't').as_deref(), ctx)?;
        let tw = self.panes[&target].window;
        let source = if a.has(b'U') || a.has(b'D') {
            if self.windows[&tw].modal == Some(target) {
                return Err("pane is modal".into());
            }
            if self.windows[&tw]
                .layout
                .find(target as u32)
                .unwrap()
                .is_floating()
            {
                return Err(format!(
                    "cannot swap {} on floating pane",
                    if a.has(b'D') { "down" } else { "up" }
                ));
            }
            let panes: Vec<_> = self.windows[&tw]
                .panes
                .iter()
                .copied()
                .filter(|id| {
                    !self.windows[&tw]
                        .layout
                        .find(*id as u32)
                        .unwrap()
                        .is_floating()
                })
                .collect();
            let at = panes.iter().position(|id| *id == target).unwrap();
            panes[(at + if a.has(b'D') { 1 } else { panes.len() - 1 }) % panes.len()]
        } else {
            self.pane_target(
                a.string(b's')
                    .as_deref()
                    .or_else(|| self.marked_target().ok().map(|_| "{marked}")),
                ctx,
            )?
        };
        let sw = self.panes[&source].window;
        if self.windows[&sw].modal == Some(source) || self.windows[&tw].modal == Some(target) {
            return Err("pane is modal".into());
        }
        let mut out = Outcome::default();
        if source == target {
            return Ok(out);
        }
        let zoomed: BTreeMap<_, _> = [sw, tw]
            .into_iter()
            .map(|wid| (wid, self.windows[&wid].zoom_target()))
            .collect();
        let active = zoomed
            .keys()
            .map(|wid| (*wid, self.windows[wid].active))
            .collect::<BTreeMap<_, _>>();
        for wid in std::iter::once(tw).chain((sw != tw).then_some(sw)) {
            self.windows.get_mut(&wid).unwrap().zoomed = false;
            self.sync_layout(wid, &mut out);
            if zoomed[&wid].is_some() {
                out.notifications.push(self.layout_event(wid));
            }
        }
        if sw != tw {
            let source_over = self.windows.get_mut(&sw).unwrap().over_zoom.remove(&source);
            let target_over = self.windows.get_mut(&tw).unwrap().over_zoom.remove(&target);
            if source_over {
                self.windows.get_mut(&tw).unwrap().over_zoom.insert(source);
            }
            if target_over {
                self.windows.get_mut(&sw).unwrap().over_zoom.insert(target);
            }
        }
        let source_saved = self.panes[&source].saved_float;
        self.panes.get_mut(&source).unwrap().saved_float = self.panes[&target].saved_float;
        self.panes.get_mut(&target).unwrap().saved_float = source_saved;
        for wid in [sw, tw].into_iter().collect::<BTreeSet<_>>() {
            let window = self.windows.get_mut(&wid).unwrap();
            for pane in &mut window.panes {
                if *pane == source {
                    *pane = target;
                } else if *pane == target {
                    *pane = source;
                }
            }
            remap(&mut window.layout, &[(source, target), (target, source)]);
            window.zoomed = false;
        }
        self.panes.get_mut(&source).unwrap().window = tw;
        self.panes.get_mut(&target).unwrap().window = sw;
        if !a.has(b'd') {
            self.windows.get_mut(&sw).unwrap().select(target);
            if sw != tw {
                self.windows.get_mut(&tw).unwrap().select(source);
            }
        } else {
            if self.windows[&sw].active == source {
                self.windows.get_mut(&sw).unwrap().select(target);
            }
            if self.windows[&tw].active == target {
                self.windows.get_mut(&tw).unwrap().select(source);
            }
        }
        if sw != tw {
            self.windows
                .get_mut(&sw)
                .unwrap()
                .last
                .retain(|id| *id != source);
            self.windows
                .get_mut(&tw)
                .unwrap()
                .last
                .retain(|id| *id != target);
        }
        for wid in std::iter::once(sw).chain((sw != tw).then_some(tw)) {
            if active[&wid] != self.windows[&wid].active {
                out.notifications.push(
                    format!("%window-pane-changed @{wid} %{}", self.windows[&wid].active)
                        .into_bytes()
                        .into(),
                );
            }
        }
        for wid in zoomed.keys() {
            self.sync_layout(*wid, &mut out);
        }
        for wid in std::iter::once(sw).chain((sw != tw).then_some(tw)) {
            out.notifications.push(self.layout_event(wid));
        }
        if a.has(b'Z') {
            for (wid, zoomed) in &zoomed {
                if zoomed.is_some() {
                    let window = self.windows.get_mut(wid).unwrap();
                    window.zoomed = true;
                    window.zoomed_pane = if window.is_over_zoom(window.active) {
                        *zoomed
                    } else {
                        Some(window.active)
                    };
                    window.save_zoom_geometry();
                }
            }
        }
        for (wid, zoomed) in zoomed {
            self.sync_layout(wid, &mut out);
            if a.has(b'Z') && zoomed.is_some() {
                out.notifications.push(self.layout_event(wid));
            }
        }
        out.changed = true;
        Ok(out)
    }
    fn detach_pane_graph(&mut self, pid: Id, forget: bool) -> Id {
        let wid = self.panes[&pid].window;
        let window = self.windows.get_mut(&wid).unwrap();
        window.zoomed = false;
        window.over_zoom.remove(&pid);
        let at = window.panes.iter().position(|id| *id == pid).unwrap();
        let neighbor = at
            .checked_sub(1)
            .or_else(|| (window.panes.len() > 1).then_some(1))
            .map(|at| window.panes[at]);
        window.panes.retain(|id| *id != pid);
        if forget {
            if self.marked.is_some_and(|m| m.pane == pid) {
                self.marked = None;
            }
            window.last.retain(|id| *id != pid);
            if window.active == pid {
                window.active = window.last.first().copied().or(neighbor).unwrap_or(pid);
                window.last.retain(|id| *id != window.active);
            }
        }
        if !window.panes.is_empty() {
            window.layout.remove(pid as u32);
        }
        wid
    }
    pub(super) fn join_pane(
        &mut self,
        name: &str,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(
            args,
            if matches!(name, "move-pane" | "movep") {
                "bdfhvD::L::MR::U::p:l:s:t:P:X:Y:z:"
            } else {
                "bdfhvp:l:s:t:"
            },
        )?;
        a.no_rest()?;
        if a.has(b'M') {
            self.pane_target(
                a.string(b's')
                    .as_deref()
                    .or_else(|| self.marked_target().ok().map(|_| "{marked}")),
                ctx,
            )?;
            self.pane_target(a.string(b't').as_deref(), ctx)?;
            return Ok(Outcome::default());
        }
        if b"DLRUPXYz".iter().any(|flag| a.has(*flag)) {
            let pane = self.pane_target(a.string(b't').as_deref(), ctx)?;
            return self.move_floating(pane, &a, ctx);
        }
        let source = self.pane_target(
            a.string(b's')
                .as_deref()
                .or_else(|| self.marked_target().ok().map(|_| "{marked}")),
            ctx,
        )?;
        let (sid, tw, target) = self.pane_context(a.string(b't').as_deref(), ctx)?;
        if self.windows[&self.panes[&source].window].modal == Some(source)
            || self.windows[&tw].modal == Some(target)
        {
            return Err("pane is modal".into());
        }
        if source == target
            && self.windows[&tw]
                .layout
                .find(source as u32)
                .unwrap()
                .is_floating()
        {
            return self.tile_pane(source, a.has(b'd'));
        }
        if source == target {
            return Err("source and target panes must be different".into());
        }
        let sw = self.panes[&source].window;
        let over_zoom = self.windows[&sw].over_zoom.contains(&source);
        let horizontal = a.has(b'h');
        let geometry = if a.has(b'f') {
            self.windows[&tw].layout.geometry
        } else {
            self.windows[&tw]
                .layout
                .find(target as u32)
                .unwrap()
                .geometry
        };
        let extent = if horizontal {
            geometry.width
        } else {
            geometry.height
        };
        let size = a
            .string(b'l')
            .or_else(|| a.string(b'p').map(|n| format!("{n}%")))
            .map(|value| {
                if let Some(n) = value.strip_suffix('%') {
                    n.parse::<u32>()
                        .ok()
                        .and_then(|n| extent.checked_mul(n))
                        .map(|n| n / 100)
                } else {
                    value.parse().ok()
                }
                .ok_or("invalid size")
            })
            .transpose()?;
        let placeholder = if sw == tw { u32::MAX } else { source as u32 };
        let mut layout = self.windows[&tw].layout.clone();
        let axis = if horizontal {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        if a.has(b'f') {
            layout.split_full_sized(placeholder, axis, size, a.has(b'b'))
        } else {
            layout.split_sized(target as u32, placeholder, axis, size, a.has(b'b'))
        }
        .map_err(|e| e.to_string())?;
        if sw == tw {
            layout.remove(source as u32);
            remap(&mut layout, &[(u32::MAX as Id, source)]);
        }
        let mut out = Outcome::default();
        for wid in std::iter::once(tw).chain((sw != tw).then_some(sw)) {
            if self.windows[&wid].zoomed {
                self.windows.get_mut(&wid).unwrap().zoomed = false;
                self.sync_layout(wid, &mut out);
                out.notifications.push(self.layout_event(wid));
            }
        }
        let source_active = self.windows[&sw].active;
        if sw != tw && self.windows[&sw].panes.len() > 1 {
            self.windows
                .get_mut(&sw)
                .unwrap()
                .layout
                .remove(source as u32);
            out.notifications.push(self.layout_event(sw));
        }
        self.detach_pane_graph(source, sw != tw);
        if self.windows[&sw].active != source_active && !self.windows[&sw].panes.is_empty() {
            out.notifications.push(
                format!("%window-pane-changed @{sw} %{}", self.windows[&sw].active)
                    .into_bytes()
                    .into(),
            );
        }
        let window = self.windows.get_mut(&tw).unwrap();
        window.layout = layout;
        if over_zoom {
            window.over_zoom.insert(source);
        }
        window.zoomed = false;
        let at =
            window.panes.iter().position(|id| *id == target).unwrap() + usize::from(!a.has(b'b'));
        window.panes.insert(at, source);
        if !a.has(b'd') {
            let old_active = window.active;
            window.select(source);
            if old_active != window.active {
                out.notifications.push(
                    format!("%window-pane-changed @{tw} %{source}")
                        .into_bytes()
                        .into(),
                );
            }
        }
        self.panes.get_mut(&source).unwrap().window = tw;
        if !a.has(b'd') {
            let index = self.window_target(a.string(b't').as_deref(), ctx)?.1;
            self.select_session_window(sid, index, &mut out);
        }
        if self.windows[&sw].panes.is_empty() {
            self.remove_window(sw, &mut out);
        }
        self.finish_pane_layouts(&[sw, tw], &mut out);
        Ok(out)
    }
    pub(super) fn break_pane(
        &mut self,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(args, "abdPF:n:s:t:Wx:X:y:Y:")?;
        a.no_rest()?;
        let (source_session, sw, pane) = self.pane_context(a.string(b's').as_deref(), ctx)?;
        if self.windows[&sw].modal == Some(pane) {
            return Err("pane is modal".into());
        }
        if a.has(b'W') {
            return self.float_pane(pane, &a, ctx);
        }
        let name = a
            .get(b'n')
            .map(|value| {
                std::str::from_utf8(value)
                    .map(|value| clean_name(value, false))
                    .map_err(|_| "invalid window name")
            })
            .transpose()?;
        let (sid, requested) = self.window_destination(a.string(b't').as_deref(), ctx)?;
        if let Some(format) = a.get(b'F') {
            self.expand(format, Some(sid), Some(sw), Some(pane), ctx)?;
        }
        if self.windows[&sw].panes.len() == 1 {
            let source_index = self.window_target(a.string(b's').as_deref(), ctx)?.1;
            let mut argv = vec![
                b"-s".to_vec(),
                format!("${source_session}:{source_index}").into_bytes(),
            ];
            for flag in b"abd" {
                if a.has(*flag) {
                    argv.push(vec![b'-', *flag]);
                }
            }
            if let Some(target) = a.get(b't') {
                argv.extend([b"-t".to_vec(), target.to_vec()]);
            }
            let mut out = self.link_window_named("move-window", &argv, ctx, name)?;
            if a.has(b'P') {
                line(
                    &mut out.output,
                    self.expand(
                        a.get(b'F')
                            .unwrap_or(b"#{session_name}:#{window_index}.#{pane_index}"),
                        Some(sid),
                        Some(sw),
                        Some(pane),
                        ctx,
                    )?,
                );
            }
            return Ok(out);
        }
        let index = if a.has(b'a') || a.has(b'b') {
            let anchor = self.window_target(a.string(b't').as_deref(), ctx)?.1;
            let index = anchor
                .checked_add(u32::from(!a.has(b'b')))
                .filter(|i| *i <= i32::MAX as u32)
                .ok_or("index too high")?;
            self.shuffle_windows(sid, index)?;
            index
        } else {
            requested
                .map(Ok)
                .unwrap_or_else(|| self.free_window_index(sid))?
        };
        if self.sessions[&sid].windows.contains_key(&index) {
            return Err(format!("index in use: {index}"));
        }
        let (cols, rows) = (self.windows[&sw].cols, self.windows[&sw].rows);
        let options = if name.is_some() {
            BTreeMap::from([("automatic-rename".into(), "off".into())])
        } else {
            BTreeMap::new()
        };
        let name = name.unwrap_or_else(|| self.windows[&sw].name.clone());
        let over_zoom = self.windows[&sw].over_zoom.contains(&pane);
        let mut out = Outcome::default();
        let old_active = self.windows[&sw].active;
        if self.windows[&sw].zoomed {
            self.windows.get_mut(&sw).unwrap().zoomed = false;
            self.sync_layout(sw, &mut out);
            out.notifications.push(self.layout_event(sw));
        }
        self.detach_pane_graph(pane, true);
        if self.windows[&sw].active != old_active {
            out.notifications.push(
                format!("%window-pane-changed @{sw} %{}", self.windows[&sw].active)
                    .into_bytes()
                    .into(),
            );
        }
        self.sync_layout(sw, &mut out);
        out.notifications.push(self.layout_event(sw));
        let wid = self.next_window;
        self.next_window += 1;
        self.windows.insert(
            wid,
            Window {
                id: wid,
                silence_deadline: None,
                activity: self.now,
                name,
                panes: vec![pane],
                active: pane,
                last: Vec::new(),
                cols,
                rows,
                zoomed: false,
                manual_size: (cols, rows),
                zoomed_pane: None,
                zoom_geometry: BTreeMap::new(),
                over_zoom: if over_zoom {
                    BTreeSet::from([pane])
                } else {
                    BTreeSet::new()
                },
                layout: Layout::pane(pane as u32, cols as u32, rows as u32),
                last_layout: None,
                old_layout: None,
                last_float: (0, 0),
                modal: None,
                options,
            },
        );
        self.panes.get_mut(&pane).unwrap().window = wid;
        self.reset_silence(wid);
        self.insert_link(sid, index, wid, 0);
        out.notifications.push(self.window_linked(wid));
        let session = self.sessions.get_mut(&sid).unwrap();
        session.selected_windows.push(wid);
        self.sync_group(sid, &mut out);
        if !a.has(b'd') {
            self.select_session_window(sid, index, &mut out);
        }
        self.sync_layout(wid, &mut out);
        out.changed = true;
        if a.has(b'P') {
            line(
                &mut out.output,
                self.expand(
                    a.get(b'F')
                        .unwrap_or(b"#{session_name}:#{window_index}.#{pane_index}"),
                    Some(sid),
                    Some(wid),
                    Some(pane),
                    &self.winlink_context(ctx, sid, index),
                )?,
            );
        }
        Ok(out)
    }
}
