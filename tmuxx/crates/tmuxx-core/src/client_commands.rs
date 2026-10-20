use super::*;

impl Engine {
    pub(super) fn attach_client(
        &mut self,
        args: &[Vec<u8>],
        ctx: &mut ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(args, "c:dEf:rt:x")?;
        a.no_rest()?;
        // Cross-client detachment is resolved by the host before calling the engine.
        a.reject(b"dx")?;
        if !ctx.control {
            return Err("ordinary terminal attachment is not supported; use Rustty or -C".into());
        }
        let (sid, index, wid, pane) = self.client_destination(a.string(b't').as_deref(), ctx)?;
        let readonly = ctx.client_flags.split(',').any(|flag| flag == "read-only");
        if readonly
            && (a.has(b'c')
                || self.sessions[&sid].active != index
                || self.windows[&wid].active != pane)
        {
            return Err("client is read-only".into());
        }
        let cwd = a
            .get(b'c')
            .map(|value| {
                self.expand(value, Some(sid), Some(wid), Some(pane), ctx)
                    .and_then(|value| admission::path(&value))
            })
            .transpose()?;
        let becomes_readonly = readonly
            || a.has(b'r')
            || a.get(b'f')
                .is_some_and(|flags| flags.split(|b| *b == b',').any(|flag| flag == b"read-only"));
        let updates = if !a.has(b'E') && !becomes_readonly {
            self.prepare_environment_update(Some(sid), &ctx.environment)?
        } else {
            Vec::new()
        };
        if let Some(cwd) = cwd {
            self.sessions.get_mut(&sid).unwrap().cwd = Some(cwd);
        }
        if a.has(b'r') {
            client_flag(ctx, "read-only", true);
            client_flag(ctx, "ignore-size", true);
            ctx.ignore_size = true;
        }
        if a.get(b'f')
            .is_some_and(|flags| flags.split(|b| *b == b',').any(|flag| flag == b"read-only"))
        {
            client_flag(ctx, "read-only", true);
        }
        self.update_environment(sid, updates);
        // attach records the previous session even when it is already current.
        ctx.last_session = ctx.session;
        let mut out = self.select_client_destination(sid, index, wid, pane, false, ctx);
        out.changed = true;
        Ok(out)
    }

    pub(super) fn switch_client(
        &mut self,
        args: &[Vec<u8>],
        ctx: &mut ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(args, "c:EFlnO:pt:rT:Z")?;
        a.no_rest()?;
        a.reject(b"cT")?;
        let destination = self.client_destination(a.string(b't').as_deref(), ctx)?;
        let order = a.string(b'O').map(|value| value.to_ascii_lowercase());
        if order.as_ref().is_some_and(|order| {
            ![
                "activity", "creation", "index", "key", "modifier", "name", "title", "order",
                "size", "z",
            ]
            .contains(&order.as_str())
        }) {
            return Err("invalid sort order".into());
        }
        let sid = if a.has(b'n') || a.has(b'p') {
            let mut sessions = self.sessions.values().collect::<Vec<_>>();
            sessions.sort_by(|a_session, b_session| {
                let cmp = match order.as_deref() {
                    Some("activity") => b_session.activity.cmp(&a_session.activity),
                    Some("creation") => a_session.created.cmp(&b_session.created),
                    Some("index" | "key") => a_session.id.cmp(&b_session.id),
                    _ => std::cmp::Ordering::Equal,
                }
                .then(a_session.name.cmp(&b_session.name));
                if a.has(b'r') { cmp.reverse() } else { cmp }
            });
            let current = sessions
                .iter()
                .position(|session| Some(session.id) == ctx.session)
                .ok_or(if a.has(b'n') {
                    "can't find next session"
                } else {
                    "can't find previous session"
                })?;
            sessions[(current + if a.has(b'n') { 1 } else { sessions.len() - 1 }) % sessions.len()]
                .id
        } else if a.has(b'l') {
            ctx.last_session
                .filter(|sid| self.sessions.contains_key(sid))
                .ok_or("can't find last session")?
        } else {
            destination.0
        };
        let (sid, index, wid, pane) = if a.has(b'n') || a.has(b'p') || a.has(b'l') {
            self.client_destination(Some(&format!("${sid}")), ctx)?
        } else {
            destination
        };
        let readonly = ctx.client_flags.split(',').any(|flag| flag == "read-only");
        if readonly && (self.sessions[&sid].active != index || self.windows[&wid].active != pane) {
            return Err("client is read-only".into());
        }
        let becomes_readonly = if a.has(b'r') { !readonly } else { readonly };
        let updates = if !a.has(b'E') && !becomes_readonly {
            self.prepare_environment_update(Some(sid), &ctx.environment)?
        } else {
            Vec::new()
        };
        if a.has(b'r') {
            client_flag(ctx, "read-only", !readonly);
            client_flag(ctx, "ignore-size", !readonly);
            ctx.ignore_size = !readonly;
        }
        self.update_environment(sid, updates);
        Ok(self.select_client_destination(sid, index, wid, pane, a.has(b'Z'), ctx))
    }

    fn client_destination(
        &self,
        target: Option<&str>,
        ctx: &ClientContext,
    ) -> Result<(Id, u32, Id, Id), String> {
        let (sid, index, wid) = if target.is_none_or(|target| !target.contains([':', '.', '%'])) {
            let sid = self.session_id(target, ctx)?;
            let session = &self.sessions[&sid];
            (sid, session.active, session.windows[&session.active])
        } else {
            self.window_target(target, ctx)?
        };
        let pane = if target.is_some_and(|target| target.contains('.') || target.starts_with('%')) {
            self.pane_target(target, ctx)?
        } else {
            self.windows[&wid].active
        };
        Ok((sid, index, wid, pane))
    }

    fn select_client_destination(
        &mut self,
        sid: Id,
        index: u32,
        wid: Id,
        pane: Id,
        keep_zoom: bool,
        ctx: &mut ClientContext,
    ) -> Outcome {
        let mut out = Outcome::default();
        let window = self.windows.get_mut(&wid).unwrap();
        let zoom = keep_zoom && window.zoomed;
        if window.active != pane {
            if !window.pane_visible(pane) {
                window.zoomed = false;
                self.sync_layout(wid, &mut out);
            }
            let window = self.windows.get_mut(&wid).unwrap();
            window.select(pane);
            if zoom && !window.zoomed {
                window.zoomed = true;
                window.zoomed_pane = Some(pane);
            }
            self.sync_layout(wid, &mut out);
            out.notifications.push(
                format!("%window-pane-changed @{wid} %{pane}")
                    .into_bytes()
                    .into(),
            );
            out.changed = true;
        }
        self.sessions.get_mut(&sid).unwrap().select(index);
        ctx.set_session(Some(sid));
        ctx.attached = true;
        ctx.current_pane = None;
        self.session_attached(sid);
        out.notifications.push(
            format!("%session-changed ${sid} {}", self.sessions[&sid].name)
                .into_bytes()
                .into(),
        );
        out.changed = true;
        out
    }
}
