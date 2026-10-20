use super::*;

impl ClientContext {
    pub fn set_session(&mut self, session: Option<Id>) {
        if session.is_none() {
            self.last_session = None;
            self.attached = false;
        } else if self.session.is_some() && self.session != session {
            self.last_session = self.session;
        }
        self.session = session;
    }
}

impl Window {
    pub(super) fn save_zoom_geometry(&mut self) {
        self.zoom_geometry = self
            .layout
            .panes()
            .iter()
            .filter_map(|p| p.id.map(|id| (id, self.layout.find(id).unwrap().geometry)))
            .collect();
    }
    pub(super) fn zoom_target(&self) -> Option<Id> {
        self.zoomed.then(|| self.zoomed_pane.unwrap_or(self.active))
    }
    pub(super) fn is_over_zoom(&self, pane: Id) -> bool {
        self.over_zoom.contains(&pane)
            && self
                .layout
                .find(pane as u32)
                .is_some_and(Layout::is_floating)
    }
    pub(super) fn pane_visible(&self, pane: Id) -> bool {
        self.zoom_target()
            .is_none_or(|target| target == pane || self.is_over_zoom(pane))
    }
    pub(super) fn pane_z(&self, pane: Id) -> usize {
        let mut order: Vec<_> = self
            .layout
            .panes()
            .iter()
            .filter_map(|p| p.z.zip(p.id))
            .collect();
        order.sort_unstable();
        let floating =
            |id: u32| self.pane_visible(id as Id) && self.zoom_target() != Some(id as Id);
        let mut z = 0;
        for (_, id) in order {
            if id as Id == pane {
                return z + usize::from(!floating(id));
            }
            if floating(id) {
                z += 1;
            }
        }
        z + 1
    }
    pub(super) fn visible_layout(&self) -> Layout {
        let Some(target) = self.zoom_target() else {
            return self.layout.clone();
        };
        let mut layout = Layout::pane(target as u32, self.cols as u32, self.rows as u32);
        let floating = self
            .panes
            .iter()
            .rev()
            .filter(|id| **id != target && self.over_zoom.contains(id))
            .filter_map(|id| {
                let pane = self.layout.find(*id as u32)?;
                pane.is_floating().then(|| pane.clone())
            })
            .collect();
        layout
            .append_floating(floating, false)
            .expect("visible floating cells are valid");
        layout.normalize_z();
        layout
    }
    pub(super) fn select(&mut self, pane: Id) {
        if self.modal.is_some_and(|modal| modal != pane) {
            return;
        }
        if self.active != pane {
            if !self.pane_visible(pane) {
                self.zoomed = false;
            }
            if self
                .layout
                .find(pane as u32)
                .is_some_and(Layout::is_floating)
            {
                self.layout
                    .set_floating_z(pane as u32, 0)
                    .expect("floating pane exists");
            }
            self.last
                .retain(|old| *old != pane && *old != self.active && self.panes.contains(old));
            self.last.insert(0, self.active);
            self.active = pane;
        }
    }
}
impl Session {
    pub(super) fn select(&mut self, index: u32) {
        if index != self.active {
            self.last.retain(|old| {
                *old != index && *old != self.active && self.windows.contains_key(old)
            });
            if self.windows.contains_key(&self.active) {
                self.last.insert(0, self.active);
            }
            self.active = index;
            if let Some(window) = self.windows.get(&index) {
                self.selected_windows.push(*window);
            }
        }
    }
    pub(super) fn repair_current(&mut self) {
        self.links
            .retain(|index, _| self.windows.contains_key(index));
        self.last.retain(|index| self.windows.contains_key(index));
        if !self.windows.contains_key(&self.active) {
            self.active = self
                .last
                .first()
                .copied()
                .or_else(|| {
                    self.windows
                        .range(..self.active)
                        .next_back()
                        .map(|(i, _)| *i)
                })
                .or_else(|| self.windows.keys().next_back().copied())
                .unwrap_or(0);
            self.last.retain(|index| *index != self.active);
            if let Some(window) = self.windows.get(&self.active) {
                self.selected_windows.push(*window);
            }
        }
    }
}
impl Engine {
    pub(super) fn select_session_window(&mut self, sid: Id, index: u32, out: &mut Outcome) {
        let session = self.sessions.get_mut(&sid).unwrap();
        if session.active != index {
            session.select(index);
            let window = session.windows[&index];
            out.notifications.push(
                format!("%session-window-changed ${sid} @{window}")
                    .into_bytes()
                    .into(),
            );
        }
    }
    pub(super) fn select_before_unlink(&mut self, sid: Id, index: u32, out: &mut Outcome) {
        let session = &self.sessions[&sid];
        if session.active == index && session.windows.len() > 1 {
            let next = session
                .last
                .iter()
                .copied()
                .find(|previous| *previous != index && session.windows.contains_key(previous))
                .or_else(|| {
                    session
                        .windows
                        .range(..index)
                        .next_back()
                        .map(|(index, _)| *index)
                })
                .or_else(|| session.windows.last_key_value().map(|(index, _)| *index))
                .unwrap();
            self.select_session_window(sid, next, out);
        }
    }
    pub(super) fn insert_link(&mut self, sid: Id, index: u32, wid: Id, alerts: u8) {
        let session = self.sessions.get_mut(&sid).unwrap();
        session.windows.insert(index, wid);
        session.links.insert(
            index,
            Winlink {
                order: self.next_link,
                alerts,
            },
        );
        self.next_link += 1;
    }

    pub(super) fn group_target(&self, target: &str, ctx: &ClientContext) -> (String, Option<Id>) {
        if let Ok(id) = self.session_id(Some(target), ctx) {
            let session = &self.sessions[&id];
            if let Some(group) = &session.group {
                return (group.clone(), Some(id));
            }
            let mut name = session.name.clone();
            let mut suffix = 1;
            while self
                .sessions
                .values()
                .any(|s| s.group.as_ref() == Some(&name))
            {
                name = format!("{}-{suffix}", session.name);
                suffix += 1;
            }
            return (name, Some(id));
        }
        (
            target.into(),
            self.sessions
                .values()
                .find(|s| s.group.as_deref() == Some(target))
                .map(|s| s.id),
        )
    }
    pub(super) fn sync_group(&mut self, sid: Id, out: &mut Outcome) {
        let Some(source) = self.sessions.get(&sid) else {
            return;
        };
        let Some(group) = source.group.clone() else {
            return;
        };
        let windows = source.windows.clone();
        if windows.is_empty() {
            return;
        }
        let links = source.links.clone();
        let targets = self
            .sessions
            .values()
            .filter(|s| s.id != sid && s.group.as_ref() == Some(&group))
            .map(|s| s.id)
            .collect::<Vec<_>>();
        for target in targets {
            let old_windows = self.sessions[&target].windows.clone();
            let session = self.sessions.get_mut(&target).unwrap();
            if !windows.contains_key(&session.active) {
                let replacement = session
                    .last
                    .first()
                    .copied()
                    .or_else(|| {
                        session
                            .windows
                            .range(..session.active)
                            .next_back()
                            .map(|(i, _)| *i)
                    })
                    .or_else(|| session.windows.last_key_value().map(|(i, _)| *i));
                if let Some(index) = replacement {
                    session.select(index);
                }
            }
            session.windows.clear();
            session.links.clear();
            for (&index, &wid) in &windows {
                self.insert_link(
                    target,
                    index,
                    wid,
                    links.get(&index).map_or(0, |link| link.alerts),
                );
                out.notifications.push(self.window_linked(wid));
            }
            let session = self.sessions.get_mut(&target).unwrap();
            if !session.windows.contains_key(&session.active) {
                session.active = session.windows.first_key_value().map_or(0, |(i, _)| *i);
            }
            session
                .last
                .retain(|index| session.windows.contains_key(index));
            for wid in old_windows.values() {
                if !windows.values().any(|id| id == wid) {
                    out.notifications.push(self.window_unlinked(*wid));
                }
            }
        }
        self.sessions.retain(|_, s| !s.windows.is_empty());
    }
    pub(super) fn free_window_index(&self, sid: Id) -> Result<u32, String> {
        let mut index = self
            .session_option(sid, "base-index")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        for _ in 0..=self.sessions[&sid].windows.len() {
            if !self.sessions[&sid].windows.contains_key(&index) {
                return Ok(index);
            }
            index = if index == i32::MAX as u32 {
                0
            } else {
                index + 1
            };
        }
        Err("no free window indexes".into())
    }
    pub(super) fn destination_index(
        &self,
        sid: Id,
        target: Option<&str>,
    ) -> Result<Option<u32>, String> {
        let Some(raw) = target.and_then(|s| s.split_once(':').map(|(_, index)| index)) else {
            return Ok(None);
        };
        if raw.is_empty() {
            return Ok(None);
        }
        let current = self.sessions[&sid].active;
        let missing = || format!("can't find window: {raw}");
        let offset = |raw: &str| {
            if raw.is_empty() {
                Ok(1)
            } else {
                raw.parse::<u32>()
                    .ok()
                    .filter(|n| (1..=i32::MAX as u32).contains(n))
                    .ok_or_else(missing)
            }
        };
        let index = if let Some(amount) = raw.strip_prefix('+') {
            current
                .checked_add(offset(amount)?)
                .filter(|index| *index <= i32::MAX as u32)
                .ok_or_else(missing)?
        } else if let Some(amount) = raw.strip_prefix('-') {
            current.checked_sub(offset(amount)?).ok_or_else(missing)?
        } else {
            raw.strip_prefix('=')
                .unwrap_or(raw)
                .parse::<u32>()
                .ok()
                .filter(|index| *index <= i32::MAX as u32)
                .map_or_else(|| self.window_index(sid, raw), Ok)?
        };
        if index > i32::MAX as u32 {
            return Err("index too high".into());
        }
        Ok(Some(index))
    }
    pub(super) fn shuffle_windows(&mut self, sid: Id, index: u32) -> Result<(), String> {
        let session = &self.sessions[&sid];
        let mut end = index;
        while session.windows.contains_key(&end) {
            end = end
                .checked_add(1)
                .filter(|i| *i <= i32::MAX as u32)
                .ok_or("no free window indexes")?;
        }
        let session = self.sessions.get_mut(&sid).unwrap();
        for from in (index..end).rev() {
            let wid = session.windows.remove(&from).unwrap();
            session.windows.insert(from + 1, wid);
            if let Some(link) = session.links.remove(&from) {
                session.links.insert(from + 1, link);
            }
        }
        if (index..end).contains(&session.active) {
            session.active += 1;
        }
        for old in &mut session.last {
            if (index..end).contains(old) {
                *old += 1;
            }
        }
        if let Some(marked) = &mut self.marked
            && marked.session == sid
            && (index..end).contains(&marked.index)
        {
            marked.index += 1;
        }
        Ok(())
    }
    fn release_unlinked(&mut self, wid: Id, out: &mut Outcome) {
        if !self
            .sessions
            .values()
            .any(|s| s.windows.values().any(|id| *id == wid))
        {
            self.remove_window(wid, out);
        }
    }
    pub(super) fn new_window(
        &mut self,
        args: &[Vec<u8>],
        ctx: &mut ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(args, "abdEkPSF:c:e:n:t:")?;
        let environment = process::environment_overrides(&a)?;
        let empty = Self::empty_requested(&a, false)?;
        let target = a.string(b't');
        let (sid, requested) = self.window_destination(target.as_deref(), ctx)?;
        let name = a
            .get(b'n')
            .map(|name| {
                self.expand(name, Some(sid), None, None, ctx).and_then(|v| {
                    String::from_utf8(v)
                        .map(|name| clean_name(&name, false))
                        .map_err(|_| "invalid window name".into())
                })
            })
            .transpose()?;
        let cwd = a
            .get(b'c')
            .map(admission::path)
            .transpose()?
            .or_else(|| ctx.cwd.clone());
        let mut out = Outcome::default();
        if a.has(b'S') {
            let index = if let Some(index) = requested {
                self.sessions[&sid]
                    .windows
                    .contains_key(&index)
                    .then_some(index)
            } else if let Some(name) = &name {
                let matches: Vec<_> = self.sessions[&sid]
                    .windows
                    .iter()
                    .filter(|(_, id)| &self.windows[id].name == name)
                    .map(|(index, _)| *index)
                    .collect();
                if matches.len() > 1 {
                    return Err(format!("multiple windows named {name}"));
                }
                matches.first().copied()
            } else {
                None
            };
            if let Some(index) = index {
                if !a.has(b'd') {
                    self.sessions.get_mut(&sid).unwrap().select(index);
                    out.changed = true;
                }
                return Ok(out);
            }
        }
        let index = if a.has(b'a') || a.has(b'b') {
            if let Some(index) =
                requested.filter(|index| !self.sessions[&sid].windows.contains_key(index))
            {
                index
            } else {
                let (_, anchor, _) = self.window_target(target.as_deref(), ctx)?;
                let index = anchor.saturating_add(u32::from(!a.has(b'b')));
                if anchor == i32::MAX as u32 || self.shuffle_windows(sid, index).is_err() {
                    requested.unwrap_or(anchor)
                } else {
                    index
                }
            }
        } else {
            requested
                .map(Ok)
                .unwrap_or_else(|| self.free_window_index(sid))?
        };
        let old = self.sessions[&sid].windows.get(&index).copied();
        if old.is_some() && !a.has(b'k') {
            return Err(format!("create window failed: index {index} in use"));
        }
        let (cols, rows, cell_width, cell_height) = self.new_window_size(sid, ctx);
        if let Some(old) = old {
            out.notifications.push(self.window_unlinked(old));
            // History belongs to the removed winlink, not its reusable index.
            self.sessions
                .get_mut(&sid)
                .unwrap()
                .last
                .retain(|last| *last != index);
        }
        let (_, wid, pane) = self.create_window(
            sid,
            name,
            cols,
            rows,
            a.rest.clone(),
            cwd,
            a.has(b'd'),
            index,
            self.pane_environment(sid, ctx, environment),
            &mut out,
        );
        self.panes
            .get_mut(&pane)
            .unwrap()
            .terminal
            .set_cell_size(cell_width, cell_height);
        if empty {
            self.make_empty(pane, &mut out);
        }
        self.sync_group(sid, &mut out);
        if let Some(old) = old {
            self.release_unlinked(old, &mut out);
        }
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
        out.changed = true;
        Ok(out)
    }
    pub(super) fn swap_window(
        &mut self,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(args, "ds:t:")?;
        a.no_rest()?;
        let (source, si, sw) = self.window_target(a.string(b's').as_deref(), ctx)?;
        let (target, ti, tw) = self.window_target(a.string(b't').as_deref(), ctx)?;
        if source != target
            && self.sessions[&source].group.is_some()
            && self.sessions[&source].group == self.sessions[&target].group
        {
            return Err("can't move window, sessions are grouped".into());
        }
        let sa = self.sessions[&source]
            .links
            .get(&si)
            .map_or(0, |link| link.alerts);
        let ta = self.sessions[&target]
            .links
            .get(&ti)
            .map_or(0, |link| link.alerts);
        self.insert_link(source, si, tw, sa);
        self.insert_link(target, ti, sw, ta);
        let mut out = Outcome {
            changed: true,
            ..Outcome::default()
        };
        if a.has(b'd') {
            self.select_session_window(target, ti, &mut out);
            if source != target {
                self.select_session_window(source, si, &mut out);
            }
        }
        self.sync_group(source, &mut out);
        if source != target {
            self.sync_group(target, &mut out);
        }
        Ok(out)
    }
}

impl Engine {
    pub(super) fn select_window(
        &mut self,
        name: &str,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let args = Args::parse(args, "alnpTt:")?;
        args.no_rest()?;
        let target = args.string(b't');
        let direct = matches!(name, "select-window" | "selectw");
        let (sid, requested) = if direct {
            let (sid, index, _) = self.window_target(target.as_deref(), ctx)?;
            (sid, index)
        } else {
            let sid = self.session_id(target.as_deref(), ctx)?;
            (sid, self.sessions[&sid].active)
        };
        let session = &self.sessions[&sid];
        let next = args.has(b'n') || matches!(name, "next-window" | "next");
        let previous = args.has(b'p') || matches!(name, "previous-window" | "prev");
        let last = args.has(b'l')
            || matches!(name, "last-window" | "last")
            || (args.has(b'T') && requested == session.active);
        let index = if next || previous {
            let mut indices = session
                .windows
                .keys()
                .copied()
                .filter(|index| !args.has(b'a') || session.links[index].alerts != 0)
                .collect::<Vec<_>>();
            if !next {
                indices.reverse();
            }
            indices
                .iter()
                .find(|index| {
                    if next {
                        **index > session.active
                    } else {
                        **index < session.active
                    }
                })
                .or_else(|| indices.first())
                .copied()
                .filter(|index| *index != session.active)
                .ok_or(if next {
                    "no next window"
                } else {
                    "no previous window"
                })?
        } else if last {
            session
                .last
                .first()
                .copied()
                .filter(|index| session.windows.contains_key(index))
                .ok_or("no last window")?
        } else {
            requested
        };
        let mut out = Outcome::default();
        if index != session.active {
            let wid = session.windows[&index];
            self.sessions.get_mut(&sid).unwrap().select(index);
            out.notifications.push(
                format!("%session-window-changed ${sid} @{wid}")
                    .into_bytes()
                    .into(),
            );
            out.changed = true;
        }
        Ok(out)
    }

    pub(super) fn kill_session(
        &mut self,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let args = Args::parse(args, "aCgf:t:")?;
        args.no_rest()?;
        let sid = self.session_id(args.string(b't').as_deref(), ctx)?;
        if args.has(b'f') && (!args.has(b'a') || args.has(b'C')) {
            return Err("-f only valid with -a".into());
        }
        let mut out = Outcome {
            changed: true,
            ..Outcome::default()
        };
        if args.has(b'C') {
            for link in self.sessions.get_mut(&sid).unwrap().links.values_mut() {
                link.alerts = 0;
            }
            return Ok(out);
        }
        let group = self.sessions[&sid]
            .group
            .as_ref()
            .filter(|_| args.has(b'g'));
        let mut sessions = self
            .sessions
            .values()
            .filter(|session| {
                if args.has(b'a') {
                    session.id != sid
                } else if let Some(group) = group {
                    session.group.as_ref() == Some(group)
                } else {
                    session.id == sid
                }
            })
            .map(|s| (s.name.clone(), s.id))
            .collect::<Vec<_>>();
        if args.has(b'a') {
            sessions.sort();
        } else {
            sessions.sort_by_key(|(_, id)| *id);
        }
        for (_, sid) in sessions {
            if let Some(filter) = args.get(b'f')
                && !tmuxx_format::truthy(&self.expand(filter, Some(sid), None, None, ctx)?)
            {
                continue;
            }
            self.remove_session(sid, &mut out);
        }
        Ok(out)
    }

    pub(super) fn kill_other_windows(
        &mut self,
        sid: Id,
        wid: Id,
        filter: Option<&[u8]>,
        ctx: &ClientContext,
        out: &mut Outcome,
    ) -> Result<(), String> {
        if self.sessions[&sid].windows.len() == 1 {
            return Ok(());
        }
        let matches = |engine: &Self, index, window| -> Result<bool, String> {
            let Some(filter) = filter else {
                return Ok(true);
            };
            let context = engine.winlink_context(ctx, sid, index);
            let value = engine.expand(filter, Some(sid), Some(window), None, &context)?;
            Ok(tmuxx_format::truthy(&value))
        };
        loop {
            let mut found = None;
            for (&index, &window) in &self.sessions[&sid].windows {
                if window != wid && matches(self, index, window)? {
                    found = Some(window);
                    break;
                }
            }
            let Some(window) = found else {
                break;
            };
            self.remove_window_with_renumber(window, out, false);
        }
        let mut count = 0;
        let mut kill_current = false;
        for (&index, &window) in &self.sessions[&sid].windows {
            if window == wid {
                count += 1;
                kill_current |= matches(self, index, window)?;
            }
        }
        if count > 1 && kill_current {
            self.remove_window_with_renumber(wid, out, false);
        }
        let mut sessions = self
            .sessions
            .values()
            .map(|s| (s.name.clone(), s.id))
            .collect::<Vec<_>>();
        sessions.sort();
        for (_, session) in sessions {
            self.renumber_session(session);
        }
        Ok(())
    }

    pub(super) fn winlink_context(
        &self,
        ctx: &ClientContext,
        sid: Id,
        index: u32,
    ) -> ClientContext {
        let mut context = ctx.clone();
        let session = &self.sessions[&sid];
        context
            .formats
            .insert(b"window_index".to_vec(), index.to_string().into_bytes());
        context.formats.insert(
            b"window_active".to_vec(),
            u8::from(session.active == index).to_string().into_bytes(),
        );
        context.formats.insert(
            b"window_last_flag".to_vec(),
            u8::from(session.last.first() == Some(&index))
                .to_string()
                .into_bytes(),
        );
        context
    }
    pub(super) fn renumber_windows(&mut self, sid: Id) {
        let base = self
            .session_option(sid, "base-index")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        let Some(session) = self.sessions.get_mut(&sid) else {
            return;
        };
        if session.windows.len() as u64 + u64::from(base) > i32::MAX as u64 + 1 {
            return;
        }
        let old = std::mem::take(&mut session.windows);
        let indices: BTreeMap<_, _> = old
            .keys()
            .enumerate()
            .map(|(i, index)| (*index, base + i as u32))
            .collect();
        session.windows = old
            .into_iter()
            .map(|(index, wid)| (indices[&index], wid))
            .collect();
        let old_links = std::mem::take(&mut session.links);
        for (old_index, index) in &indices {
            let alerts = old_links.get(old_index).map_or(0, |link| link.alerts);
            session.links.insert(
                *index,
                Winlink {
                    order: self.next_link,
                    alerts,
                },
            );
            self.next_link += 1;
        }
        session.active = indices.get(&session.active).copied().unwrap_or(base);
        session.last = session
            .last
            .iter()
            .filter_map(|index| indices.get(index).copied())
            .collect();
        if let Some(marked) = &mut self.marked
            && marked.session == sid
        {
            if let Some(index) = indices.get(&marked.index) {
                marked.index = *index;
            } else {
                self.marked = None;
            }
        }
    }
    pub(super) fn renumber_session(&mut self, sid: Id) {
        if self.session_option(sid, "renumber-windows") != Some("on") {
            return;
        }
        let Some(session) = self.sessions.get(&sid) else {
            return;
        };
        let targets = if let Some(group) = &session.group {
            self.sessions
                .values()
                .filter(|s| s.group.as_ref() == Some(group))
                .map(|s| s.id)
                .collect()
        } else {
            vec![sid]
        };
        for target in targets {
            self.renumber_windows(target);
        }
    }
    pub(super) fn window_destination(
        &self,
        target: Option<&str>,
        ctx: &ClientContext,
    ) -> Result<(Id, Option<u32>), String> {
        if let Some(raw) = target {
            if raw
                .split_once(':')
                .map_or(raw, |(_, window)| window)
                .split_once('.')
                .is_some_and(|(_, pane)| !pane.is_empty())
            {
                return Err("can't specify pane here".into());
            }
            if let Some((session, _)) = raw.split_once(':') {
                let sid = self.session_name(Some(session), ctx)?;
                return Ok((sid, self.destination_index(sid, Some(raw))?));
            }
            if raw.parse::<u32>().is_ok() || raw.starts_with(['+', '-']) {
                let sid = self.session_id(None, ctx)?;
                return Ok((sid, self.destination_index(sid, Some(&format!(":{raw}")))?));
            }
            if let Ok(sid) = self.session_id(Some(raw), ctx) {
                return Ok((sid, None));
            }
            let (sid, index, _) = self.window_target(Some(raw), ctx)?;
            return Ok((sid, Some(index)));
        }
        Ok((self.session_id(None, ctx)?, None))
    }
    pub(super) fn link_window(
        &mut self,
        name: &str,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        self.link_window_named(name, args, ctx, None)
    }
    pub(super) fn link_window_named(
        &mut self,
        name: &str,
        args: &[Vec<u8>],
        ctx: &ClientContext,
        renamed: Option<String>,
    ) -> Result<Outcome, String> {
        let moving = matches!(name, "move-window" | "movew");
        let a = Args::parse(args, if moving { "abdkrs:t:" } else { "abdks:t:" })?;
        a.no_rest()?;
        if a.has(b'r') {
            let sid = self.session_id(a.string(b't').as_deref(), ctx)?;
            self.renumber_windows(sid);
            return Ok(Outcome {
                changed: true,
                ..Outcome::default()
            });
        }
        let (source, mut source_index, wid) = self.window_target(a.string(b's').as_deref(), ctx)?;
        let target = a.string(b't');
        let (sid, requested) = self.window_destination(target.as_deref(), ctx)?;
        if sid != source
            && self.sessions[&sid].group.is_some()
            && self.sessions[&sid].group == self.sessions[&source].group
        {
            return Err("sessions are grouped".into());
        }
        let index = if a.has(b'a') || a.has(b'b') {
            let anchor = self.window_target(target.as_deref(), ctx)?.1;
            let index = anchor
                .checked_add(u32::from(!a.has(b'b')))
                .filter(|i| *i <= i32::MAX as u32)
                .ok_or("index too high")?;
            // Remember the source winlink across insertion shifting.
            let shifted = if source == sid && source_index >= index {
                self.sessions[&sid]
                    .windows
                    .range(index..=source_index)
                    .map(|(i, _)| *i)
                    .eq(index..=source_index)
            } else {
                false
            };
            self.shuffle_windows(sid, index)?;
            if shifted {
                source_index += 1;
            }
            index
        } else {
            requested
                .map(Ok)
                .unwrap_or_else(|| self.free_window_index(sid))?
        };
        let old = self.sessions[&sid].windows.get(&index).copied();
        if old == Some(wid) {
            return Err(format!("same index: {index}"));
        }
        if old.is_some() && !a.has(b'k') {
            return Err(format!("index in use: {index}"));
        }
        let mut out = Outcome {
            changed: true,
            ..Outcome::default()
        };
        if let Some(old) = old {
            out.notifications.push(self.window_unlinked(old));
            self.sessions
                .get_mut(&sid)
                .unwrap()
                .last
                .retain(|last| *last != index);
        }
        self.insert_link(sid, index, wid, 0);
        out.notifications.push(self.window_linked(wid));
        self.sync_group(sid, &mut out);
        if !a.has(b'd') {
            self.select_session_window(sid, index, &mut out);
        }
        if let Some(name) = renamed {
            let window = self.windows.get_mut(&wid).unwrap();
            window.name = name;
            window
                .options
                .insert("automatic-rename".into(), "off".into());
            out.notifications.push(self.window_renamed(wid));
        }
        if moving {
            if self.sessions[&source].windows.len() == 1 {
                self.remove_session_group(source, &mut out);
            } else {
                self.select_before_unlink(source, source_index, &mut out);
                out.notifications.push(self.window_unlinked(wid));
                let session = self.sessions.get_mut(&source).unwrap();
                session.windows.remove(&source_index);
                session.links.remove(&source_index);
                session.repair_current();
                self.sync_group(source, &mut out);
            }
            if !a.has(b's') && self.session_option(source, "renumber-windows") == Some("on") {
                self.renumber_windows(source);
            }
        }
        if let Some(marked) = &mut self.marked
            && marked.session == source
            && marked.index == source_index
        {
            if source == sid {
                marked.index = index;
            } else {
                self.marked = None;
            }
        }
        if let Some(old) = old {
            self.release_unlinked(old, &mut out);
        }
        Ok(out)
    }
    pub(super) fn unlink_window(
        &mut self,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(args, "kt:")?;
        a.no_rest()?;
        let (sid, index, wid) = self.window_target(a.string(b't').as_deref(), ctx)?;
        let link_groups = self
            .sessions
            .values()
            .filter(|s| s.windows.values().any(|id| *id == wid))
            .map(|s| (s.group.clone(), if s.group.is_some() { 0 } else { s.id }))
            .collect::<BTreeSet<_>>()
            .len();
        let local_links = self.sessions[&sid]
            .windows
            .values()
            .filter(|id| **id == wid)
            .count();
        if link_groups == 1 && local_links == 1 && !a.has(b'k') {
            return Err("window only linked to one session".into());
        }
        let mut out = Outcome {
            changed: true,
            ..Outcome::default()
        };
        if self.sessions[&sid].windows.len() == 1 {
            self.remove_session_group(sid, &mut out);
            return Ok(out);
        }
        self.select_before_unlink(sid, index, &mut out);
        out.notifications.push(self.window_unlinked(wid));
        let session = self.sessions.get_mut(&sid).unwrap();
        session.windows.remove(&index);
        session.links.remove(&index);
        session.repair_current();
        self.sync_group(sid, &mut out);
        self.sessions.retain(|_, s| !s.windows.is_empty());
        self.release_unlinked(wid, &mut out);
        Ok(out)
    }
}
