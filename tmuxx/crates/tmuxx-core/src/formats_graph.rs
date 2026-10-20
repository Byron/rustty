use super::*;

impl Engine {
    // The outer Option distinguishes unknown fields from fields absent in this context.
    pub(super) fn graph_format(
        &self,
        key: &str,
        sid: Option<Id>,
        wid: Option<Id>,
        pid: Option<Id>,
        ctx: &ClientContext,
    ) -> Option<Option<String>> {
        let known = matches!(
            key,
            "active_window_index"
                | "last_window_index"
                | "next_session_id"
                | "server_sessions"
                | "session_active"
                | "session_path"
                | "session_stack"
                | "session_marked"
                | "session_attached_list"
                | "session_group_attached"
                | "session_group_attached_list"
                | "session_group_many_attached"
                | "window_active_sessions"
                | "window_active_sessions_list"
                | "window_active_clients"
                | "window_active_clients_list"
                | "window_linked_sessions_list"
                | "window_start_flag"
                | "window_end_flag"
                | "window_stack_index"
                | "window_marked_flag"
                | "window_manual_width"
                | "window_manual_height"
                | "window_flags"
                | "window_raw_flags"
                | "window_bigger"
                | "pane_at_top"
                | "pane_at_bottom"
                | "pane_at_left"
                | "pane_at_right"
                | "pane_flags"
                | "pane_unzoomed_width"
                | "pane_unzoomed_height"
                | "pane_x"
                | "pane_y"
                | "host"
                | "host_short"
                | "pid"
                | "uid"
                | "user"
                | "socket_path"
                | "session_format"
                | "window_format"
                | "pane_format"
                | "session_activity_flag"
                | "session_bell_flag"
                | "session_silence_flag"
                | "session_alert"
                | "session_alerts"
                | "window_activity_flag"
                | "window_bell_flag"
                | "window_silence_flag"
        );
        if !known {
            return None;
        }
        Some((|| {
            let s = sid.and_then(|id| self.sessions.get(&id));
            let w = wid
                .or_else(|| s?.windows.get(&s?.active).copied())
                .and_then(|id| self.windows.get(&id));
            let p = pid
                .or_else(|| w.map(|w| w.active))
                .and_then(|id| self.panes.get(&id));
            let index = || {
                let s = s?;
                ctx.formats
                    .get(b"window_index".as_slice())
                    .and_then(|v| text(v).parse::<u32>().ok())
                    .filter(|index| s.windows.get(index) == w.map(|w| &w.id))
                    .or_else(|| {
                        s.windows
                            .iter()
                            .find(|(_, id)| Some(**id) == w.map(|w| w.id))
                            .map(|(i, _)| *i)
                    })
            };
            let flag = |value| u8::from(value).to_string();
            let linked = || {
                let wid = w?.id;
                let mut links =
                    self.sessions
                        .values()
                        .flat_map(|s| {
                            s.windows.iter().filter(move |(_, id)| **id == wid).map(
                                move |(i, _)| {
                                    (s.links.get(i).map_or(u64::MAX, |link| link.order), s, *i)
                                },
                            )
                        })
                        .collect::<Vec<_>>();
                links.sort_by_key(|link| link.0);
                Some(links)
            };
            let marked = self.marked.filter(|_| self.marked_target().is_ok());
            let value = match key {
                "host" | "pid" | "uid" | "user" | "socket_path" => self.identity.get(key)?.clone(),
                "host_short" => self
                    .identity
                    .get("host")?
                    .split('.')
                    .next()
                    .unwrap_or_default()
                    .into(),
                "next_session_id" => format!("${}", self.next_session),
                "server_sessions" => self.sessions.len().to_string(),
                "active_window_index" => s?.active.to_string(),
                "last_window_index" => s?.windows.last_key_value()?.0.to_string(),
                "session_active" => {
                    if !ctx.has_format_client() {
                        return None;
                    }
                    flag(ctx.session == Some(s?.id))
                }
                "session_path" => s?.cwd.clone()?,
                "session_marked" => {
                    let id = s?.id;
                    flag(marked.is_some_and(|marked| marked.session == id))
                }
                "session_stack" => {
                    let s = s?;
                    let mut stack = std::iter::once(s.active)
                        .chain(s.last.iter().copied())
                        .map(|i| i.to_string())
                        .collect::<Vec<_>>()
                        .join(",");
                    stack.truncate(stack.len().min(1023));
                    stack
                }
                "session_attached_list" => {
                    let id = s?.id;
                    self.clients
                        .iter()
                        .filter(|c| c.session == Some(id))
                        .map(|c| c.client_name.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                }
                "session_group_attached"
                | "session_group_attached_list"
                | "session_group_many_attached" => {
                    let group = s?.group.as_ref()?;
                    let clients = self
                        .clients
                        .iter()
                        .filter(|c| {
                            c.session
                                .and_then(|id| self.sessions.get(&id))
                                .is_some_and(|s| s.group.as_ref() == Some(group))
                        })
                        .collect::<Vec<_>>();
                    match key {
                        "session_group_attached" => clients.len().to_string(),
                        "session_group_many_attached" => flag(clients.len() > 1),
                        _ => clients
                            .iter()
                            .map(|c| c.client_name.as_str())
                            .collect::<Vec<_>>()
                            .join(","),
                    }
                }
                "window_linked_sessions_list"
                | "window_active_sessions"
                | "window_active_sessions_list" => {
                    let links = linked()?
                        .into_iter()
                        .filter(|(_, s, i)| key == "window_linked_sessions_list" || s.active == *i)
                        .collect::<Vec<_>>();
                    if key == "window_active_sessions" {
                        links.len().to_string()
                    } else {
                        links
                            .iter()
                            .map(|(_, s, _)| s.name.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
                    }
                }
                "window_active_clients" | "window_active_clients_list" => {
                    let wid = w?.id;
                    let clients = self
                        .clients
                        .iter()
                        .filter(|c| {
                            c.session
                                .and_then(|id| self.sessions.get(&id))
                                .is_some_and(|s| s.windows.get(&s.active) == Some(&wid))
                        })
                        .collect::<Vec<_>>();
                    if key == "window_active_clients" {
                        clients.len().to_string()
                    } else {
                        clients
                            .iter()
                            .map(|c| c.client_name.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
                    }
                }
                "window_start_flag" => flag(index()? == *s?.windows.first_key_value()?.0),
                "window_end_flag" => flag(index()? == *s?.windows.last_key_value()?.0),
                "window_stack_index" => s?
                    .last
                    .iter()
                    .position(|i| Some(*i) == index())
                    .map_or(0, |i| i + 1)
                    .to_string(),
                "window_marked_flag" => {
                    let i = index()?;
                    flag(marked.is_some_and(|m| Some(m.session) == sid && m.index == i))
                }
                "window_manual_width" | "window_manual_height" => {
                    let w = w?;
                    if self.window_option(w.id, "window-size") != Some("manual") {
                        String::new()
                    } else if key.ends_with("width") {
                        w.manual_size.0.to_string()
                    } else {
                        w.manual_size.1.to_string()
                    }
                }
                "window_flags" | "window_raw_flags" => {
                    let (s, w, i) = (s?, w?, index()?);
                    let mut flags = alert_flags(s.links.get(&i).map_or(0, |l| l.alerts));
                    if key == "window_flags" {
                        flags = flags.replace('#', "##");
                    }
                    if s.active == i {
                        flags.push('*');
                    }
                    if s.last.first() == Some(&i) {
                        flags.push('-');
                    }
                    if marked.is_some_and(|m| m.session == s.id && m.index == i) {
                        flags.push('M');
                    }
                    if w.modal.is_some() {
                        flags.push('O');
                    }
                    if w.zoomed {
                        flags.push('Z');
                    }
                    flags
                }
                "window_bigger" => {
                    if !ctx.has_format_client() {
                        return None;
                    }
                    "0".into()
                }
                "pane_x" => p?.x.to_string(),
                "pane_y" => p?.y.to_string(),
                "pane_at_left" => flag(p?.x == 0),
                "pane_at_right" => flag(p?.x + i32::from(p?.cols) == i32::from(w?.cols)),
                "pane_at_top" | "pane_at_bottom" => {
                    let (p, w) = (p?, w?);
                    let floating = w.layout.find(p.id as u32)?.is_floating();
                    let status = if !floating {
                        self.window_option(w.id, "pane-border-status")
                    } else if self.pane_option(p.id, "pane-border-lines") == Some("none") {
                        Some("off")
                    } else {
                        match self.pane_option(p.id, "pane-border-status") {
                            Some("top-floating") => Some("top"),
                            Some("bottom-floating") => Some("bottom"),
                            status => status,
                        }
                    };
                    if key == "pane_at_top" {
                        flag(p.y == i32::from(status == Some("top")))
                    } else {
                        flag(
                            p.y + i32::from(p.rows)
                                == i32::from(w.rows) - i32::from(status == Some("bottom")),
                        )
                    }
                }
                "pane_unzoomed_width" | "pane_unzoomed_height" => {
                    let (p, w) = (p?, w?);
                    let layout = w.layout.find(p.id as u32)?;
                    let geometry = self.pane_content_geometry(
                        w.id,
                        layout.geometry,
                        w.layout.geometry,
                        layout.is_floating(),
                    );
                    if key.ends_with("width") {
                        geometry.width.to_string()
                    } else {
                        geometry.height.to_string()
                    }
                }
                "pane_flags" => {
                    let (p, w) = (p?, w?);
                    let mut flags = String::new();
                    if w.active == p.id {
                        flags.push('*');
                    }
                    if w.last.first() == Some(&p.id) {
                        flags.push('-');
                    }
                    if w.zoom_target() == Some(p.id) {
                        flags.push('Z');
                    }
                    if w.layout.find(p.id as u32)?.is_floating() {
                        flags.push('F');
                    }
                    if w.over_zoom.contains(&p.id) {
                        flags.push('A');
                    }
                    if w.modal == Some(p.id) {
                        flags.push('O');
                    }
                    flags
                }
                "session_format" | "window_format" | "pane_format" => {
                    let kind = ctx.format_type.unwrap_or(if pid.is_some() {
                        b'P'
                    } else if wid.is_some() {
                        b'W'
                    } else if sid.is_some() {
                        b'S'
                    } else {
                        0
                    });
                    flag(
                        kind == match key {
                            "session_format" => b'S',
                            "window_format" => b'W',
                            _ => b'P',
                        },
                    )
                }
                "window_activity_flag" | "window_bell_flag" | "window_silence_flag" => {
                    let bits = s?.links.get(&index()?).map_or(0, |l| l.alerts);
                    flag(bits & alert_bit(key) != 0)
                }
                "session_activity_flag" | "session_bell_flag" | "session_silence_flag" => flag(
                    s?.links
                        .values()
                        .any(|link| link.alerts & alert_bit(key) != 0),
                ),
                "session_alert" => {
                    let mut seen = 0;
                    let mut flags = String::new();
                    for link in s?.links.values() {
                        flags.push_str(&alert_flags(link.alerts & !seen));
                        seen |= link.alerts;
                    }
                    flags
                }
                "session_alerts" => {
                    let mut flags = s?
                        .links
                        .iter()
                        .filter(|(_, link)| link.alerts != 0)
                        .map(|(i, link)| format!("{i}{}", alert_flags(link.alerts)))
                        .collect::<Vec<_>>()
                        .join(",");
                    flags.truncate(flags.len().min(1023));
                    flags
                }
                _ => unreachable!(),
            };
            Some(value)
        })())
    }
}
fn alert_bit(key: &str) -> u8 {
    if key.contains("activity") {
        1
    } else if key.contains("bell") {
        2
    } else {
        4
    }
}
fn alert_flags(bits: u8) -> String {
    [(1, '#'), (2, '!'), (4, '~')]
        .into_iter()
        .filter_map(|(bit, c)| (bits & bit != 0).then_some(c))
        .collect()
}

impl Engine {
    pub(super) fn list_clients(
        &self,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(args, "F:f:O:rt:")?;
        a.no_rest()?;
        let sid = a
            .string(b't')
            .map(|target| self.session_id(Some(&target), ctx))
            .transpose()?;
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
        let mut clients = self.clients.iter().collect::<Vec<_>>();
        if let Some(order) = order.as_deref() {
            if order == "order" {
                if a.has(b'r') {
                    clients.reverse();
                }
            } else {
                clients.sort_by(|a_client, b_client| {
                    let cmp = match order {
                        "activity" => b_client.activity.cmp(&a_client.activity),
                        "creation" => a_client.created.cmp(&b_client.created),
                        "size" => {
                            (a_client.cols, a_client.rows).cmp(&(b_client.cols, b_client.rows))
                        }
                        _ => std::cmp::Ordering::Equal,
                    }
                    .then(a_client.client_name.cmp(&b_client.client_name));
                    if a.has(b'r') { cmp.reverse() } else { cmp }
                });
            }
        }
        let mut out = Outcome::default();
        for (index, client) in clients.into_iter().enumerate() {
            if client.session.is_none() || sid.is_some_and(|sid| client.session != Some(sid)) {
                continue;
            }
            let session = client.session;
            let window = session
                .and_then(|sid| self.sessions.get(&sid))
                .and_then(|s| s.windows.get(&s.active))
                .copied();
            let mut context = client.clone();
            context.formats.clear();
            context.format_type = Some(0);
            context
                .formats
                .insert(b"line".to_vec(), index.to_string().into_bytes());
            if let Some(filter) = a.get(b'f')
                && !tmuxx_format::truthy(&self.expand(filter, session, window, None, &context)?)
            {
                continue;
            }
            line(&mut out.output, self.expand(a.get(b'F').unwrap_or(b"#{client_name}: #{session_name} [#{client_width}x#{client_height} #{client_termname}] #{?#{!=:#{client_uid},#{uid}},[user #{?client_user,#{client_user},#{client_uid},}] ,}#{?client_flags,(,}#{client_flags}#{?client_flags,),}"), session, window, None, &context)?);
        }
        Ok(out)
    }
}
