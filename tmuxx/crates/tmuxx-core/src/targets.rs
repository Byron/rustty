use super::*;

fn unique<'a>(
    names: impl Iterator<Item = (Id, &'a str)>,
    pattern: &str,
    kind: &str,
) -> Result<Id, String> {
    let exact = pattern.starts_with('=');
    let raw = pattern.strip_prefix('=').unwrap_or(pattern);
    let names: Vec<_> = names.collect();
    for mode in 0..if exact { 1 } else { 3 } {
        let mut matches = Vec::new();
        for (id, name) in &names {
            if match mode {
                0 => *name == raw,
                1 => name.starts_with(raw),
                _ => tmuxx_format::glob_match(raw.as_bytes(), name.as_bytes())?,
            } {
                matches.push(*id);
            }
        }
        if matches.len() == 1 {
            return Ok(matches[0]);
        }
        if matches.len() > 1 {
            return Err(format!("can't find {kind}: {raw}"));
        }
    }
    Err(format!("can't find {kind}: {raw}"))
}
fn offset(raw: &str) -> Option<(bool, usize)> {
    if !raw.starts_with(['+', '-']) {
        return None;
    }
    let n = if raw.len() == 1 {
        1
    } else {
        raw[1..].parse().ok()?
    };
    (n > 0 && n <= i32::MAX as usize).then_some((raw.starts_with('+'), n))
}
fn alias(raw: &str) -> &str {
    match raw {
        "{last}" => "!",
        "{start}" => "^",
        "{end}" => "$",
        "{next}" => "+",
        "{previous}" => "-",
        _ => raw,
    }
}
impl Engine {
    fn context_pane(&self, ctx: &ClientContext) -> Option<Id> {
        ctx.current_pane
            .filter(|id| self.panes.contains_key(id))
            .or_else(|| {
                if ctx.attached {
                    return None;
                }
                self.panes
                    .values()
                    .find(|pane| {
                        !pane.tty.is_empty() && ctx.identity.get("client_tty") == Some(&pane.tty)
                    })
                    .map(|pane| pane.id)
                    .or_else(|| {
                        ctx.environment
                            .get("TMUX_PANE")
                            .and_then(|value| value.strip_prefix('%'))
                            .and_then(|value| value.parse().ok())
                            .filter(|id| self.panes.contains_key(id))
                    })
            })
    }
    fn whole_target(
        &self,
        target: &str,
        ctx: &ClientContext,
    ) -> Result<Option<(Id, u32, Id, Id)>, String> {
        match target {
            "~" | "{marked}" => self.marked_target().map(Some),
            "=" | "{mouse}" => Err("no mouse target".into()),
            "@" | "{active}" | "{current}" => {
                let client = self
                    .clients
                    .iter()
                    .find(|client| {
                        !ctx.client_name.is_empty() && client.client_name == ctx.client_name
                    })
                    .unwrap_or(ctx);
                let session = client
                    .attached
                    .then_some(client.session)
                    .flatten()
                    .and_then(|sid| self.sessions.get(&sid))
                    .ok_or("no current client")?;
                let wid = session.windows[&session.active];
                Ok(Some((
                    session.id,
                    session.active,
                    wid,
                    self.windows[&wid].active,
                )))
            }
            _ => Ok(None),
        }
    }
    pub(super) fn marked_target(&self) -> Result<(Id, u32, Id, Id), String> {
        let Marked {
            session: sid,
            index,
            window: wid,
            pane: pid,
        } = self.marked.ok_or("no marked target")?;
        let session = self.sessions.get(&sid).ok_or("no marked target")?;
        if session.windows.get(&index) != Some(&wid)
            || self.panes.get(&pid).is_none_or(|p| p.window != wid)
        {
            return Err("no marked target".into());
        }
        Ok((sid, index, wid, pid))
    }
    pub(super) fn session_id(
        &self,
        target: Option<&str>,
        ctx: &ClientContext,
    ) -> Result<Id, String> {
        if let Some(target) = target.filter(|t| !t.is_empty()) {
            if let Some(target) = self.whole_target(target, ctx)? {
                return Ok(target.0);
            }
            if target.contains([':', '.']) || target.starts_with(['@', '%']) {
                return self.pane_context(Some(target), ctx).map(|target| target.0);
            }
        }
        self.session_name(target, ctx)
    }
    pub(super) fn session_name(
        &self,
        target: Option<&str>,
        ctx: &ClientContext,
    ) -> Result<Id, String> {
        if let Some(raw) = target.filter(|target| !target.is_empty()) {
            if !raw.starts_with('=')
                && let Some(id) = raw.strip_prefix('$')
            {
                return id
                    .parse::<Id>()
                    .ok()
                    .filter(|id| self.sessions.contains_key(id))
                    .ok_or_else(|| format!("can't find session: {raw}"));
            }
            return unique(
                self.sessions.values().map(|s| (s.id, s.name.as_str())),
                raw,
                "session",
            );
        }
        ctx.session
            .filter(|id| self.sessions.contains_key(id))
            .or_else(|| {
                self.context_pane(ctx)
                    .and_then(|pane| self.window_owner(self.panes[&pane].window, ctx).ok())
                    .map(|target| target.0)
            })
            .or_else(|| {
                self.sessions
                    .values()
                    .max_by(|a, b| {
                        a.activity
                            .cmp(&b.activity)
                            .then_with(|| b.name.cmp(&a.name))
                    })
                    .map(|s| s.id)
            })
            .ok_or_else(|| "no server running".into())
    }
    pub(super) fn window_owner(
        &self,
        wid: Id,
        ctx: &ClientContext,
    ) -> Result<(Id, u32, Id), String> {
        let linked = |session: &Session| {
            if session.windows.get(&session.active) == Some(&wid) {
                return Some((session.id, session.active, wid));
            }
            session
                .windows
                .iter()
                .find(|(_, id)| **id == wid)
                .map(|(index, _)| (session.id, *index, wid))
        };
        ctx.session
            .and_then(|id| self.sessions.get(&id))
            .and_then(linked)
            .or_else(|| {
                self.sessions
                    .values()
                    .filter(|session| session.windows.values().any(|id| *id == wid))
                    .max_by(|a, b| {
                        a.activity
                            .cmp(&b.activity)
                            .then_with(|| b.name.cmp(&a.name))
                    })
                    .and_then(linked)
            })
            .ok_or_else(|| format!("can't find window: @{wid}"))
    }
    pub(super) fn window_index(&self, sid: Id, raw: &str) -> Result<u32, String> {
        let session = &self.sessions[&sid];
        let exact = raw.starts_with('=');
        let raw = if exact { &raw[1..] } else { alias(raw) };
        if !exact && raw.starts_with(['+', '-']) && offset(raw).is_none() {
            return Err(format!("can't find window: {raw}"));
        }
        let special = if raw.is_empty() {
            Some(session.active)
        } else if !exact && raw == "!" {
            session.last.first().copied()
        } else if !exact && raw == "^" {
            session.windows.keys().next().copied()
        } else if !exact && raw == "$" {
            session.windows.keys().next_back().copied()
        } else if let Some(wid) = raw.strip_prefix('@').and_then(|v| v.parse::<Id>().ok()) {
            if session.windows.get(&session.active) == Some(&wid) {
                Some(session.active)
            } else {
                session
                    .windows
                    .iter()
                    .find(|(_, id)| **id == wid)
                    .map(|(i, _)| *i)
            }
        } else if !exact && let Some((forward, n)) = offset(raw) {
            let indices: Vec<_> = session.windows.keys().copied().collect();
            let current = indices
                .iter()
                .position(|i| *i == session.active)
                .unwrap_or(0);
            let n = n % indices.len();
            Some(indices[(current + if forward { n } else { indices.len() - n }) % indices.len()])
        } else {
            (!raw.starts_with(['+', '-']))
                .then(|| raw.parse::<u32>().ok())
                .flatten()
                .filter(|index| session.windows.contains_key(index))
        };
        if let Some(index) = special {
            return Ok(index);
        }
        unique(
            session
                .windows
                .iter()
                .map(|(i, id)| (u64::from(*i), self.windows[id].name.as_str())),
            &format!("{}{raw}", if exact { "=" } else { "" }),
            "window",
        )
        .map(|index| index as u32)
    }
    pub(super) fn window_target(
        &self,
        target: Option<&str>,
        ctx: &ClientContext,
    ) -> Result<(Id, u32, Id), String> {
        let Some(target) = target.filter(|t| !t.is_empty()) else {
            if let Some(pane) = self.context_pane(ctx).and_then(|id| self.panes.get(&id)) {
                return self.window_owner(pane.window, ctx);
            }
            let sid = self.session_id(None, ctx)?;
            let session = &self.sessions[&sid];
            return Ok((sid, session.active, session.windows[&session.active]));
        };
        if let Some((sid, index, wid, _)) = self.whole_target(target, ctx)? {
            return Ok((sid, index, wid));
        }
        if !target.contains([':', '.']) {
            if let Some(wid) = target.strip_prefix('@').and_then(|s| s.parse().ok()) {
                return self.window_owner(wid, ctx);
            }
            if let Some(raw) = target.strip_prefix('%') {
                let pane = raw
                    .parse::<Id>()
                    .ok()
                    .and_then(|id| self.panes.get(&id))
                    .ok_or_else(|| format!("can't find pane: %{raw}"))?;
                return self.window_owner(pane.window, ctx);
            }
            if target.starts_with('$') {
                let sid = self.session_id(Some(target), ctx)?;
                let session = &self.sessions[&sid];
                return Ok((sid, session.active, session.windows[&session.active]));
            }
        }
        let (sid, raw, explicit) = if let Some((session, rest)) = target.split_once(':') {
            (self.session_name(Some(session), ctx)?, rest, true)
        } else {
            (self.session_id(None, ctx)?, target, false)
        };
        let (raw, pane) = raw
            .split_once('.')
            .map_or((raw, None), |(window, pane)| (window, Some(pane)));
        let found = match self.window_index(sid, raw) {
            Ok(index) => Ok((sid, index, self.sessions[&sid].windows[&index])),
            Err(error) => {
                if !explicit && let Ok(sid) = self.session_name(Some(raw), ctx) {
                    let session = &self.sessions[&sid];
                    Ok((sid, session.active, session.windows[&session.active]))
                } else {
                    Err(error)
                }
            }
        }?;
        if let Some(pane) = pane.filter(|pane| !pane.is_empty())
            && self.pane_in_window(found.2, pane).is_none()
        {
            return Err(format!("can't find pane: {pane}"));
        }
        Ok(found)
    }
    pub(super) fn adjacent_pane(&self, pid: Id, direction: u8) -> Option<Id> {
        let p = &self.panes[&pid];
        let window = &self.windows[&p.window];
        window
            .panes
            .iter()
            .copied()
            .filter(|id| *id != pid)
            .find(|id| {
                let q = &self.panes[id];
                match direction {
                    b'L' => {
                        (q.x + i32::from(q.cols) + 1 == p.x
                            || (p.x == 0 && q.x + i32::from(q.cols) == i32::from(window.cols)))
                            && q.y < p.y + i32::from(p.rows)
                            && q.y + i32::from(q.rows) > p.y
                    }
                    b'R' => {
                        (p.x + i32::from(p.cols) + 1 == q.x
                            || (p.x + i32::from(p.cols) == i32::from(window.cols) && q.x == 0))
                            && q.y < p.y + i32::from(p.rows)
                            && q.y + i32::from(q.rows) > p.y
                    }
                    b'U' => {
                        (q.y + i32::from(q.rows) + 1 == p.y
                            || (p.y == 0 && q.y + i32::from(q.rows) == i32::from(window.rows)))
                            && q.x < p.x + i32::from(p.cols)
                            && q.x + i32::from(q.cols) > p.x
                    }
                    _ => {
                        (p.y + i32::from(p.rows) + 1 == q.y
                            || (p.y + i32::from(p.rows) == i32::from(window.rows) && q.y == 0))
                            && q.x < p.x + i32::from(p.cols)
                            && q.x + i32::from(q.cols) > p.x
                    }
                }
            })
    }
    fn pane_in_window(&self, wid: Id, raw: &str) -> Option<Id> {
        let window = &self.windows[&wid];
        let raw = alias(raw);
        if raw.is_empty() {
            return Some(window.active);
        }
        if raw == "!" {
            return window
                .last
                .first()
                .copied()
                .filter(|id| window.panes.contains(id));
        }
        if let Some(raw) = raw.strip_prefix('%') {
            return raw
                .parse::<Id>()
                .ok()
                .filter(|id| window.panes.contains(id));
        }
        if let Some((forward, n)) = offset(raw) {
            let current = window.panes.iter().position(|id| *id == window.active)?;
            let n = n % window.panes.len();
            return Some(
                window.panes[(current + if forward { n } else { window.panes.len() - n })
                    % window.panes.len()],
            );
        }
        if raw.starts_with(['+', '-']) {
            return None;
        }
        let direction = match raw {
            "{up-of}" => Some(b'U'),
            "{down-of}" => Some(b'D'),
            "{left-of}" => Some(b'L'),
            "{right-of}" => Some(b'R'),
            _ => None,
        };
        if let Some(direction) = direction {
            return self.adjacent_pane(window.active, direction);
        }
        let base = window
            .options
            .get("pane-base-index")
            .or_else(|| self.global_window_options.get("pane-base-index"))
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(0);
        if let Some(index) = raw.parse::<usize>().ok().and_then(|i| i.checked_sub(base)) {
            return window.panes.get(index).copied();
        }
        let description = raw.trim_start_matches('{').trim_end_matches('}');
        window.panes.iter().copied().find(|id| {
            let p = &self.panes[id];
            match description {
                "top" => p.y == 0,
                "bottom" => p.y + i32::from(p.rows) == i32::from(window.rows),
                "left" => p.x == 0,
                "right" => p.x + i32::from(p.cols) == i32::from(window.cols),
                "top-left" => p.x == 0 && p.y == 0,
                "top-right" => p.y == 0 && p.x + i32::from(p.cols) == i32::from(window.cols),
                "bottom-left" => p.x == 0 && p.y + i32::from(p.rows) == i32::from(window.rows),
                "bottom-right" => {
                    p.x + i32::from(p.cols) == i32::from(window.cols)
                        && p.y + i32::from(p.rows) == i32::from(window.rows)
                }
                _ => false,
            }
        })
    }
    pub(super) fn pane_target(
        &self,
        target: Option<&str>,
        ctx: &ClientContext,
    ) -> Result<Id, String> {
        let Some(target) = target.filter(|t| !t.is_empty()) else {
            if let Some(id) = self.context_pane(ctx) {
                return Ok(id);
            }
            return Ok(self.windows[&self.window_target(None, ctx)?.2].active);
        };
        if let Some(target) = self.whole_target(target, ctx)? {
            return Ok(target.3);
        }
        if let Some(raw) = target.strip_prefix('%') {
            return raw
                .parse()
                .ok()
                .filter(|id| self.panes.contains_key(id))
                .ok_or_else(|| format!("can't find pane: %{raw}"));
        }
        if let Some((_, raw)) = target.split_once('.') {
            let (_, _, wid) = self.window_target(Some(target), ctx)?;
            return self
                .pane_in_window(wid, raw)
                .ok_or_else(|| format!("can't find pane: {raw}"));
        }
        if !target.contains(':') && !target.starts_with(['@', '$']) {
            let (_, _, wid) = self.window_target(None, ctx)?;
            if let Some(pane) = self.pane_in_window(wid, target) {
                return Ok(pane);
            }
        }
        Ok(self.windows[&self.window_target(Some(target), ctx)?.2].active)
    }
    pub(super) fn display_context(
        &self,
        target: Option<&str>,
        ctx: &ClientContext,
    ) -> Option<(Id, Id, Id)> {
        if let Ok(found) = self.pane_context(target, ctx) {
            return Some(found);
        }
        let target = target?;
        if matches!(target, "~" | "{marked}") || target.starts_with(['%', '@', '$']) {
            return None;
        }
        let (sid, raw) = if let Some((session, raw)) = target.split_once(':') {
            (self.session_id(Some(session), ctx).ok()?, raw)
        } else {
            (self.session_id(None, ctx).ok()?, target)
        };
        let session = &self.sessions[&sid];
        let raw = raw.split('.').next().unwrap();
        // C display-message keeps the partially resolved read-only context.
        let index = self
            .window_index(sid, raw)
            .ok()
            .or_else(|| {
                session
                    .windows
                    .iter()
                    .find(|(_, id)| self.windows[id].name.starts_with(raw))
                    .map(|(index, _)| *index)
            })
            .unwrap_or(session.active);
        let wid = session.windows[&index];
        Some((sid, wid, self.windows[&wid].active))
    }

    pub(super) fn pane_context(
        &self,
        target: Option<&str>,
        ctx: &ClientContext,
    ) -> Result<(Id, Id, Id), String> {
        let pid = self.pane_target(target, ctx)?;
        let wid = self.panes[&pid].window;
        let sid = if let Some((raw, _)) = target.and_then(|t| t.split_once(':')) {
            self.session_name(Some(raw), ctx)?
        } else if let Some(raw) = target.filter(|t| t.starts_with('$')) {
            self.session_id(Some(raw), ctx)?
        } else if let Some(raw) = target.filter(|t| !t.starts_with(['%', '@']) && !t.contains('.'))
        {
            self.window_target(Some(raw), ctx)
                .ok()
                .filter(|(_, _, id)| *id == wid)
                .map(|(id, _, _)| id)
                .unwrap_or(self.window_owner(wid, ctx)?.0)
        } else {
            self.window_owner(wid, ctx)?.0
        };
        Ok((sid, wid, pid))
    }
}
