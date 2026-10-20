//! C control-mode subscriptions and bounded, ordered client delivery.
use super::*;
use std::collections::VecDeque;
use tmuxx_command::{Args, Command};

const BYTE_LIMIT: usize = 64 * 1024 * 1024;
const MAX_AGE: Duration = Duration::from_secs(300);
#[derive(Default)]
pub(super) struct Controls {
    pub(super) clients: BTreeMap<u64, ControlClient>,
    pub(super) colors: BTreeMap<Id, [Option<[u8; 3]>; 2]>,
    next_command: u64,
}
pub(super) struct ControlClient {
    pub(super) wire: bool,
    pub(super) closing: Option<Instant>,
    flags: BTreeSet<String>,
    pause_after: Option<Duration>,
    panes: BTreeMap<Id, (bool, bool)>,
    subscriptions: BTreeMap<Vec<u8>, Subscription>,
    next_check: Instant,
    pending: VecDeque<Queued>,
    pending_bytes: usize,
    pub(super) after_reply: Vec<ServerMessage>,
}
impl Default for ControlClient {
    fn default() -> Self {
        Self {
            wire: false,
            closing: None,
            flags: Default::default(),
            pause_after: None,
            panes: Default::default(),
            subscriptions: Default::default(),
            next_check: Instant::now() + Duration::from_secs(1),
            pending: Default::default(),
            pending_bytes: 0,
            after_reply: Vec::new(),
        }
    }
}
struct Queued {
    message: ServerMessage,
    bytes: usize,
    at: Instant,
}
#[derive(Clone, Copy)]
pub(super) enum What {
    Session,
    Windows,
    Window(Id),
    Panes,
    Pane(Id),
}
pub(super) struct Subscription {
    pub(super) what: What,
    pub(super) format: Vec<u8>,
    pub(super) last: BTreeMap<SubscriptionTarget, Vec<u8>>,
}
pub(super) type SubscriptionTarget = (Option<Id>, Option<u32>, Option<Id>);

impl Subscription {
    pub(super) fn targets(&self, engine: &Engine, sid: Id) -> Vec<SubscriptionTarget> {
        let Some(session) = engine.sessions.get(&sid) else {
            return Vec::new();
        };
        let mut targets = Vec::new();
        match self.what {
            What::Session => targets.push((None, None, None)),
            what => {
                for (&index, &window) in &session.windows {
                    let Some(w) = engine.windows.get(&window) else {
                        continue;
                    };
                    match what {
                        What::Windows => targets.push((Some(window), Some(index), None)),
                        What::Window(id) if window == id => {
                            targets.push((Some(window), Some(index), None))
                        }
                        What::Panes | What::Pane(_) => {
                            for &pane in &w.panes {
                                if matches!(what, What::Panes)
                                    || matches!(what, What::Pane(id) if id == pane)
                                {
                                    targets.push((Some(window), Some(index), Some(pane)));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        targets
    }
}

/// Conservative in-memory cost, shared by enqueue and receive accounting.
pub(super) fn message_bytes(message: &ServerMessage) -> usize {
    128 + match message {
        ServerMessage::PaneOutput { data, .. } => data.len().saturating_mul(4).saturating_add(128),
        ServerMessage::Control { data } => data.len(),
        ServerMessage::Reply(response) => {
            response.output.len() + response.error.as_ref().map_or(0, String::len)
        }
        // Exactly one bounded immutable snapshot is retained separately per client.
        ServerMessage::Snapshot { .. } => 0,
        ServerMessage::SnapshotChunk { data, .. } => data.len(),
        ServerMessage::SnapshotAborted { error, .. } => error.len(),
        ServerMessage::HostEffect { action: state, .. } => {
            serde_json::to_vec(state).map_or(BYTE_LIMIT, |bytes| bytes.len())
        }
        ServerMessage::Exit { reason } => reason.len(),
        ServerMessage::File { path, data, .. } => path.len() + data.as_ref().map_or(0, Vec::len),
        ServerMessage::Detached { session, .. } => session.len(),
        ServerMessage::ClientFlags { flags } => flags.len(),
        _ => 0,
    }
}

fn decimal(bytes: &[u8]) -> Option<(u32, &[u8])> {
    let bytes = bytes.trim_ascii_start();
    let negative = bytes.first() == Some(&b'-');
    let bytes = if matches!(bytes.first(), Some(b'+' | b'-')) {
        &bytes[1..]
    } else {
        bytes
    };
    let end = bytes
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(bytes.len());
    if end == 0 {
        return None;
    }
    let value = std::str::from_utf8(&bytes[..end])
        .ok()?
        .parse::<u32>()
        .ok()?;
    Some((
        if negative {
            value.wrapping_neg()
        } else {
            value
        },
        &bytes[end..],
    ))
}
fn dimensions(bytes: &[u8], delimiters: &[u8]) -> Option<(u32, u32)> {
    let (x, tail) = decimal(bytes)?;
    if !delimiters.contains(tail.first()?) {
        return None;
    }
    Some((x, decimal(&tail[1..])?.0))
}
pub(super) fn subscription(bytes: &[u8]) -> Option<(Vec<u8>, Subscription)> {
    let mut parts = bytes.splitn(3, |byte| *byte == b':');
    let name = parts.next()?.to_vec();
    let what = parts.next()?;
    let format = parts.next()?.to_vec();
    let what = match what {
        b"" => What::Session,
        b"%*" => What::Panes,
        b"@*" => What::Windows,
        [b'%', rest @ ..] => What::Pane(
            decimal(rest)
                .filter(|(id, _)| *id <= i32::MAX as u32)?
                .0
                .into(),
        ),
        [b'@', rest @ ..] => What::Window(
            decimal(rest)
                .filter(|(id, _)| *id <= i32::MAX as u32)?
                .0
                .into(),
        ),
        _ => return None,
    };
    Some((
        name,
        Subscription {
            what,
            format,
            last: BTreeMap::new(),
        },
    ))
}

impl ControlClient {
    fn discard_pane(&mut self, pane: Id) {
        self.pending.retain(|queued| !matches!(queued.message, ServerMessage::PaneOutput { pane_id, .. } if pane_id == pane));
        self.pending_bytes = self.pending.iter().map(|queued| queued.bytes).sum();
    }
    fn flags(&mut self, value: &[u8], context: &mut ClientContext) {
        for flag in value.split(|byte| *byte == b',') {
            let (remove, flag) = if let Some(rest) = flag.strip_prefix(b"!") {
                (true, rest)
            } else {
                (false, flag)
            };
            if flag == b"pause-after" || flag.starts_with(b"pause-after=") {
                if flag != b"pause-after" && decimal(&flag[12..]).is_none() {
                    continue;
                }
                self.pause_after = if remove {
                    None
                } else {
                    Some(Duration::from_secs(
                        decimal(flag.get(12..).unwrap_or_default())
                            .map_or(0, |value| value.0)
                            .into(),
                    ))
                };
                continue;
            }
            let Ok(flag) = std::str::from_utf8(flag) else {
                continue;
            };
            if !matches!(
                flag,
                "read-only"
                    | "ignore-size"
                    | "no-detach-on-destroy"
                    | "no-output"
                    | "wait-exit"
                    | "new-layouts"
            ) {
                continue;
            }
            if remove {
                // C read-only is sticky for the life of this client.
                if flag != "read-only" {
                    self.flags.remove(flag);
                }
            } else {
                self.flags.insert(flag.to_owned());
            }
            if flag == "no-output" {
                self.pending
                    .retain(|queued| !matches!(queued.message, ServerMessage::PaneOutput { .. }));
                self.pending_bytes = self.pending.iter().map(|queued| queued.bytes).sum();
            }
        }
        context.ignore_size = self.flags.contains("ignore-size");
        let mut flags = vec![
            "attached".to_owned(),
            "focused".to_owned(),
            "control-mode".to_owned(),
        ];
        for flag in [
            "ignore-size",
            "no-detach-on-destroy",
            "no-output",
            "wait-exit",
            "new-layouts",
        ] {
            if self.flags.contains(flag) {
                flags.push(flag.to_owned());
            }
        }
        if let Some(age) = self.pause_after {
            flags.push(format!("pause-after={}", age.as_secs()));
        }
        if self.flags.contains("read-only") {
            flags.push("read-only".to_owned());
        }
        flags.push("UTF-8".to_owned());
        context.client_flags = flags.join(",");
    }
    fn enqueue(&mut self, message: ServerMessage) {
        let bytes = message_bytes(&message);
        self.pending_bytes += bytes;
        self.pending.push_back(Queued {
            message,
            bytes,
            at: Instant::now(),
        });
    }
}

impl Host {
    pub(super) fn prepare_control_commands(
        &mut self,
        id: u64,
        source: &[u8],
    ) -> Result<Vec<Command>, String> {
        let context = self.clients.get(&id).map(|client| client.context.clone());
        context
            .ok_or_else(|| "client disconnected".to_string())
            .and_then(|context| self.prepare_commands(source, &context))
            .map_err(|error| {
                let message = error
                    .split_once(": ")
                    .filter(|(line, _)| line.parse::<usize>().is_ok())
                    .map_or(error.as_str(), |(_, message)| message);
                format!("parse error: {message}")
            })
    }
    pub(super) fn control_command_result(
        &mut self,
        id: u64,
        flags: u64,
        mut output: Vec<u8>,
        error: Option<&str>,
    ) {
        self.control.next_command += 1;
        if let Some(error) = error {
            output.extend_from_slice(error.as_bytes());
            output.push(b'\n');
        }
        let guard = tmuxx_protocol::Guard {
            time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            number: self.control.next_command,
            flags,
        };
        let mut data = tmuxx_protocol::encode_command_result(guard, &output, error.is_some());
        data.pop(); // The transport terminates each Control packet with LF.
        self.send(id, ServerMessage::Control { data });
        self.control_after_reply(id);
    }
    pub(super) fn control_after_reply(&mut self, id: u64) {
        let messages = self
            .control
            .clients
            .get_mut(&id)
            .map(|state| std::mem::take(&mut state.after_reply))
            .unwrap_or_default();
        for message in messages {
            self.send(id, message);
        }
    }
    pub(super) fn control_unframed_output(&mut self, id: u64, mut data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        if data.last() == Some(&b'\n') {
            data.pop();
        }
        self.send(id, ServerMessage::Control { data });
    }
    pub(super) fn prepare_notification(
        &self,
        notification: EngineNotification,
    ) -> Vec<(u64, ServerMessage)> {
        self.clients
            .iter()
            .filter(|(_, client)| client.subscribed)
            .filter_map(|(id, client)| {
                let data = match &notification {
                    EngineNotification::Raw(data) => {
                        let mut data = data.clone();
                        if client.control
                            && let Some(rest) = data
                                .strip_prefix(b"%unlinked-window-")
                                .or_else(|| data.strip_prefix(b"%window-"))
                            && let Some(space) = rest.iter().position(|byte| *byte == b' ')
                            && matches!(&rest[..space], b"add" | b"renamed")
                            && let Some(window) = rest[space + 1..]
                                .strip_prefix(b"@")
                                .and_then(decimal)
                                .map(|(id, _)| Id::from(id))
                        {
                            let session = client
                                .context
                                .session
                                .and_then(|sid| self.engine.sessions.get(&sid))?;
                            let linked = session.windows.values().any(|id| *id == window);
                            let mut notice = if linked {
                                b"%window-".to_vec()
                            } else {
                                b"%unlinked-window-".to_vec()
                            };
                            notice.extend_from_slice(rest);
                            data = notice;
                        }
                        data
                    }
                    EngineNotification::WindowLinked {
                        window,
                        linked_sessions,
                    }
                    | EngineNotification::WindowUnlinked {
                        window,
                        linked_sessions,
                    } => {
                        let linked =
                            !client.control || linked_sessions.contains(&client.context.session?);
                        let prefix = if linked {
                            "%window-"
                        } else {
                            "%unlinked-window-"
                        };
                        let event =
                            if matches!(notification, EngineNotification::WindowLinked { .. }) {
                                "add"
                            } else {
                                "close"
                            };
                        format!("{prefix}{event} @{window}").into_bytes()
                    }
                    EngineNotification::WindowRenamed {
                        window,
                        name,
                        linked_sessions,
                    } => {
                        let linked =
                            !client.control || linked_sessions.contains(&client.context.session?);
                        let prefix = if linked {
                            "%window-"
                        } else {
                            "%unlinked-window-"
                        };
                        format!("{prefix}renamed @{window} {name}").into_bytes()
                    }
                    EngineNotification::LayoutChange {
                        window,
                        legacy,
                        extended,
                        session_flags,
                        native_flags,
                    } => {
                        let flags = if client.control {
                            session_flags.get(&client.context.session?)?.as_slice()
                        } else {
                            native_flags.as_bytes()
                        };
                        let new_layouts = client.control
                            && client
                                .context
                                .client_flags
                                .split(',')
                                .any(|flag| flag == "new-layouts");
                        let (layout, visible) = if new_layouts { extended } else { legacy };
                        let mut data =
                            format!("%layout-change @{window} {layout} {visible} ").into_bytes();
                        data.extend_from_slice(flags);
                        data
                    }
                };
                Some((*id, ServerMessage::Control { data }))
            })
            .collect()
    }
    pub(super) fn broadcast_notification(&mut self, notification: EngineNotification) {
        for (id, message) in self.prepare_notification(notification) {
            self.send(id, message);
        }
    }
    pub(super) fn reset_control_pane(&mut self, pane: Id) {
        for state in self.control.clients.values_mut() {
            state.discard_pane(pane);
        }
    }
    pub(super) fn initialize_control(&mut self, id: u64, context: &mut ClientContext) {
        let initial = context.client_flags.clone();
        let state = self.control.clients.entry(id).or_insert_with(|| {
            let mut state = ControlClient::default();
            state.flags(initial.as_bytes(), context);
            state
        });
        state.flags(&[], context);
    }
    pub(super) fn control_flags(&mut self, id: u64, value: &[u8], context: &mut ClientContext) {
        let previous = self
            .clients
            .get(&id)
            .map(|client| client.context.client_flags.clone());
        let state = self.control.clients.entry(id).or_default();
        // switch-client -r explicitly toggles these, unlike sticky refresh -f removal.
        for (flag, enabled) in [
            (
                "read-only",
                context
                    .client_flags
                    .split(',')
                    .any(|flag| flag == "read-only"),
            ),
            ("ignore-size", context.ignore_size),
        ] {
            if enabled {
                state.flags.insert(flag.to_owned());
            } else {
                state.flags.remove(flag);
            }
        }
        state.flags(value, context);
        if previous.as_ref() != Some(&context.client_flags) {
            self.send_control(
                id,
                ServerMessage::ClientFlags {
                    flags: context.client_flags.clone(),
                },
            );
        }
    }
    pub(super) fn refresh_control(
        &mut self,
        id: u64,
        command: &Command,
        context: &mut ClientContext,
    ) -> Result<Outcome, String> {
        let args = Args::parse(&command.args[1..], "A:B:cC:Df:r:F:lLRSt:U")?;
        args.no_rest()?;
        args.reject(b"cDlLRSU")?;
        let target = if let Some(target) = args.get(b't') {
            let text = String::from_utf8_lossy(target);
            let exact = text.strip_prefix('=').unwrap_or(&text);
            let found = self
                .clients
                .iter()
                .find(|(_, client)| client.context.client_name == exact)
                .map(|(id, _)| *id);
            if let Some(id) = found {
                id
            } else {
                let candidates = self
                    .clients
                    .iter()
                    .filter(|(_, client)| client.context.client_name.starts_with(exact))
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>();
                if candidates.len() == 1 && !text.starts_with('=') {
                    candidates[0]
                } else {
                    return Err(format!("can't find client: {text}"));
                }
            }
        } else {
            id
        };
        let mut target_context = if target == id {
            context.clone()
        } else {
            self.clients[&target].context.clone()
        };
        if !self
            .clients
            .get(&target)
            .is_some_and(|client| client.control)
        {
            // Native subscribed views share the sizing command without using C control framing.
            if target == id && context.control && args.flags.len() == 1 && args.has(b'C') {
                return self.execute(command, context);
            }
            if target == id && context.control && args.flags.len() == 1 && args.has(b'r') {
                return self.control_color_report(args.get(b'r').unwrap());
            }
            return Err("not a control client".into());
        }
        if args.has(b'F') || args.has(b'f') {
            let mut flags = args.get(b'F').unwrap_or_default().to_vec();
            flags.push(b',');
            flags.extend_from_slice(args.get(b'f').unwrap_or_default());
            self.control_flags(target, &flags, &mut target_context);
        }
        let mut out = if let Some(value) = args.get(b'r') {
            self.control_color_report(value)?
        } else {
            Outcome::default()
        };
        let state = self.control.clients.entry(target).or_default();
        let mut notifications = Vec::new();
        if let Some(values) = args.repeated.get(&b'A') {
            for value in values.iter().flatten() {
                let Some(rest) = value.strip_prefix(b"%") else {
                    continue;
                };
                let Some((pane, _)) = decimal(rest) else {
                    continue;
                };
                let pane = Id::from(pane);
                let Some(colon) = rest.iter().position(|byte| *byte == b':') else {
                    continue;
                };
                let action = &rest[colon + 1..];
                if !self.engine.panes.contains_key(&pane) {
                    continue;
                }
                let (off, paused) = state.panes.entry(pane).or_default();
                let discard = match action {
                    b"on" if *off => {
                        *off = false;
                        true
                    }
                    b"off" => {
                        *off = true;
                        true
                    }
                    b"pause" if !*paused => {
                        *paused = true;
                        notifications.push(format!("%pause %{pane}"));
                        true
                    }
                    b"continue" if *paused => {
                        *paused = false;
                        notifications.push(format!("%continue %{pane}"));
                        true
                    }
                    _ => false,
                };
                if discard {
                    state.discard_pane(pane);
                }
            }
        } else if let Some(values) = args.repeated.get(&b'B') {
            for value in values.iter().flatten() {
                if let Some((name, sub)) = subscription(value) {
                    if state.subscriptions.len() >= 256 && !state.subscriptions.contains_key(&name)
                    {
                        return Err("control subscription limit exceeded".into());
                    }
                    if state.subscriptions.is_empty() {
                        state.next_check = Instant::now() + Duration::from_secs(1);
                    }
                    state.subscriptions.insert(name, sub);
                } else {
                    state.subscriptions.remove(value);
                }
            }
        } else if let Some(size) = args.get(b'C') {
            if let Some(window) = size.strip_prefix(b"@").and_then(decimal) {
                if let Some((x, y)) = window
                    .1
                    .strip_prefix(b":")
                    .and_then(|bytes| dimensions(bytes, b"x"))
                {
                    if !(1..=10000).contains(&x) || !(1..=10000).contains(&y) {
                        return Err("size too small or too big".into());
                    }
                    target_context
                        .window_sizes
                        .insert(window.0.into(), (x as u16, y as u16));
                    target_context.window_size_requested = true;
                } else {
                    target_context.window_sizes.remove(&window.0.into());
                }
            } else {
                let (x, y) = dimensions(size, b",x").ok_or("bad size argument")?;
                if !(1..=10000).contains(&x) || !(1..=10000).contains(&y) {
                    return Err("size too small or too big".into());
                }
                target_context.cols = x as u16;
                target_context.rows = y as u16;
                target_context.size_requested = true;
            }
            target_context.resize_order = self.engine.sequence + 1;
            self.engine.sequence += 1;
            out.changed = true;
        }
        if target == id {
            *context = target_context;
        } else {
            self.clients.get_mut(&target).unwrap().context = target_context;
        }
        for data in notifications {
            let message = ServerMessage::Control {
                data: data.into_bytes(),
            };
            if target == id {
                self.control
                    .clients
                    .get_mut(&target)
                    .unwrap()
                    .after_reply
                    .push(message);
            } else {
                self.send_control(target, message);
            }
        }
        Ok(out)
    }

    pub(super) fn send_control(&mut self, id: u64, message: ServerMessage) {
        let Some(client) = self.clients.get(&id) else {
            return;
        };
        let state = self.control.clients.entry(id).or_default();
        if state.closing.is_some() && !matches!(message, ServerMessage::Detached { .. }) {
            return;
        }
        if let ServerMessage::PaneOutput { pane_id, .. } = &message
            && (state.flags.contains("no-output")
                || state
                    .panes
                    .get(pane_id)
                    .is_some_and(|(off, paused)| *off || *paused))
        {
            return;
        }
        let reply = matches!(message, ServerMessage::Reply(_));
        let bytes = message_bytes(&message)
            + if reply {
                state.after_reply.iter().map(message_bytes).sum()
            } else {
                0
            };
        if bytes
            .saturating_add(state.pending_bytes)
            .saturating_add(client.delivery_bytes.load(Ordering::Acquire))
            > BYTE_LIMIT
        {
            let _ = client.tx.try_send(ServerMessage::Exit {
                reason: "control output exceeds byte budget".into(),
            });
            self.disconnect(id);
            return;
        }
        state.enqueue(message);
        if reply {
            for message in std::mem::take(&mut state.after_reply) {
                state.enqueue(message);
            }
        }
        self.flush_control(id);
    }

    fn flush_control(&mut self, id: u64) {
        let Some(client) = self.clients.get(&id).cloned() else {
            return;
        };
        let Some(state) = self.control.clients.get_mut(&id) else {
            return;
        };
        if state
            .closing
            .is_some_and(|at| at.elapsed() >= Duration::from_secs(10))
        {
            self.disconnect(id);
            return;
        }
        let mut disconnect = false;
        let mut detached = false;
        while let Some(queued) = state.pending.pop_front() {
            state.pending_bytes -= queued.bytes;
            let age = queued.at.elapsed();
            if let ServerMessage::PaneOutput { pane_id, .. } = &queued.message
                && age >= state.pause_after.unwrap_or(MAX_AGE)
                && age.as_millis() > 0
            {
                if state.pause_after.is_some() {
                    let pane = *pane_id;
                    state.panes.entry(pane).or_default().1 = true;
                    state.discard_pane(pane);
                    state.enqueue(ServerMessage::Control {
                        data: format!("%pause %{pane}").into_bytes(),
                    });
                    continue;
                }
                disconnect = true;
                break;
            }
            let message = match &queued.message {
                ServerMessage::PaneOutput { pane_id, data, .. } if state.pause_after.is_some() => {
                    let mut output =
                        format!("%extended-output %{pane_id} {} : ", age.as_millis()).into_bytes();
                    output.extend(tmuxx_protocol::escape_output(data));
                    ServerMessage::Control { data: output }
                }
                _ => queued.message.clone(),
            };
            let bytes = message_bytes(&message);
            let completes_detach = matches!(message, ServerMessage::Detached { .. });
            client.delivery_bytes.fetch_add(bytes, Ordering::AcqRel);
            match client.tx.try_send(message) {
                Ok(()) if completes_detach => {
                    detached = true;
                    break;
                }
                Ok(()) => {}
                Err(error) => {
                    client.delivery_bytes.fetch_sub(bytes, Ordering::AcqRel);
                    if matches!(error, TrySendError::Disconnected(_)) {
                        disconnect = true;
                    } else {
                        state.pending_bytes += queued.bytes;
                        state.pending.push_front(queued);
                    }
                    break;
                }
            }
        }
        if detached {
            self.disconnect(id);
        } else if disconnect {
            let _ = client.tx.try_send(ServerMessage::Exit {
                reason: "too far behind".into(),
            });
            self.disconnect(id);
        }
    }

    pub(super) fn control_tick(&mut self) {
        let ids = self.control.clients.keys().copied().collect::<Vec<_>>();
        for id in ids {
            self.check_subscriptions(id);
            self.flush_control(id);
        }
    }

    fn check_subscriptions(&mut self, id: u64) {
        let Some(client) = self.clients.get(&id) else {
            return;
        };
        let context = client.context.clone();
        let Some(sid) = context.session else {
            return;
        };
        if !self.engine.sessions.contains_key(&sid) {
            return;
        }
        let state = self.control.clients.get_mut(&id).unwrap();
        if Instant::now() < state.next_check {
            return;
        }
        state.next_check = Instant::now() + Duration::from_secs(1);
        let mut notifications = Vec::new();
        for (name, subscription) in &mut state.subscriptions {
            let targets = subscription.targets(&self.engine, sid);
            let all = matches!(subscription.what, What::Windows | What::Panes);
            if all {
                subscription.last.retain(|key, _| targets.contains(key));
            }
            for (window, index, pane) in targets {
                let mut context = context.clone();
                if let Some(index) = index {
                    context
                        .formats
                        .insert(b"window_index".to_vec(), index.to_string().into_bytes());
                }
                let Ok(value) =
                    self.engine
                        .expand(&subscription.format, Some(sid), window, pane, &context)
                else {
                    continue;
                };
                let key = (window, index, pane);
                if subscription.last.get(&key) == Some(&value) {
                    continue;
                }
                subscription.last.insert(key, value.clone());
                let mut output = b"%subscription-changed ".to_vec();
                output.extend(name);
                output.extend(
                    format!(
                        " ${sid} {} {} {} : ",
                        window.map_or("-".into(), |id| format!("@{id}")),
                        index.map_or("-".into(), |index| index.to_string()),
                        pane.map_or("-".into(), |id| format!("%{id}"))
                    )
                    .as_bytes(),
                );
                output.extend(value);
                let rank = match subscription.what {
                    What::Session => (0, name.clone(), 0, 0, Vec::new()),
                    What::Window(_) | What::Pane(_) => {
                        (1, name.clone(), index.unwrap_or(0), 0, Vec::new())
                    }
                    What::Panes => (
                        2,
                        Vec::new(),
                        index.unwrap_or(0),
                        self.engine.windows[&window.unwrap()]
                            .panes
                            .iter()
                            .position(|id| Some(*id) == pane)
                            .unwrap_or(0),
                        name.clone(),
                    ),
                    What::Windows => (3, Vec::new(), index.unwrap_or(0), 0, name.clone()),
                };
                notifications.push((rank, ServerMessage::Control { data: output }));
            }
        }
        notifications.sort_by(|(left, _), (right, _)| left.cmp(right));
        for (_, message) in notifications {
            self.send_control(id, message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_drains_existing_output_before_closing_and_bounds_stalled_peers() {
        let (mut host, rx) = host(1);
        reply(&mut host);
        output(&mut host, b"before-detach");
        host.detach_client(1, true);
        assert!(host.clients.contains_key(&1));
        assert!(matches!(receive(&host, &rx), ServerMessage::Reply(_)));
        host.flush_control(1);
        assert!(
            matches!(receive(&host, &rx), ServerMessage::PaneOutput { data, .. } if data == b"before-detach")
        );
        let bytes = host.clients[&1].delivery_bytes.clone();
        host.flush_control(1);
        assert!(!host.clients.contains_key(&1));
        assert_eq!(
            rx.recv().unwrap(),
            ServerMessage::Detached {
                session: "test".into(),
                hangup: true
            }
        );
        bytes.store(0, Ordering::Release);

        let (mut host, rx) = self::host(1);
        reply(&mut host);
        host.detach_client(1, false);
        host.control.clients.get_mut(&1).unwrap().closing =
            Some(Instant::now() - Duration::from_secs(11));
        host.flush_control(1);
        assert!(!host.clients.contains_key(&1));
        assert!(matches!(rx.recv().unwrap(), ServerMessage::Reply(_)));
        assert!(rx.recv().is_err());
    }

    fn command(words: &[&str]) -> Command {
        Command {
            blocks: Default::default(),
            args: words.iter().map(|word| word.as_bytes().to_vec()).collect(),
        }
    }
    #[test]
    fn control_guards_keep_zero_request_id_completion_separate() {
        let (mut host, rx) = host(32);
        host.request_parsed(
            1,
            Request {
                request_id: 0,
                argv: vec![
                    b"display".to_vec(),
                    b"-p".to_vec(),
                    b"first".to_vec(),
                    b";".to_vec(),
                    b"display".to_vec(),
                    b"-p".to_vec(),
                    b"second".to_vec(),
                ],
                stdin: None,
                subscribe: false,
                control: true,
            },
            None,
            Some(1),
        );
        assert!(host.scheduled_work());
        let mut decoder = tmuxx_protocol::Decoder::default();
        let mut events = Vec::new();
        let mut replies = Vec::new();
        while let Ok(message) = rx.try_recv() {
            match message {
                ServerMessage::Control { mut data } => {
                    data.push(b'\n');
                    events.extend(decoder.feed(&data).unwrap());
                }
                ServerMessage::Reply(reply) => replies.push(reply),
                _ => {}
            }
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, tmuxx_protocol::Event::Begin(_)))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter_map(
                    |event| if let tmuxx_protocol::Event::CommandOutput(line) = event {
                        Some(line.as_slice())
                    } else {
                        None
                    }
                )
                .collect::<Vec<_>>(),
            [b"first".as_slice(), b"second"]
        );
        assert_eq!(
            replies,
            [Response {
                request_id: 0,
                status: None,
                output: Vec::new(),
                error: None
            }]
        );
    }
    fn host(capacity: usize) -> (Host, Receiver<ServerMessage>) {
        let (tx, _) = mpsc::sync_channel(1);
        let mut host = Host::new(tx, &Options::new("unused-control-test"), BTreeMap::new());
        let mut context = ClientContext {
            control: true,
            client_name: "control-test".into(),
            ..ClientContext::default()
        };
        host.engine
            .execute(
                &command(&["new-session", "-d", "-s", "test", "sleep", "30"]),
                &mut context,
            )
            .unwrap();
        let (tx, rx) = mpsc::sync_channel(capacity);
        host.initialize_control(1, &mut context);
        host.clients.insert(
            1,
            Client {
                tx,
                context,
                subscribed: true,
                control: true,
                delivery_bytes: Arc::new(AtomicUsize::new(0)),
            },
        );
        (host, rx)
    }
    fn refresh(host: &mut Host, words: &[&str]) -> Result<Outcome, String> {
        let mut context = host.clients[&1].context.clone();
        let out = host.refresh_control(1, &command(words), &mut context);
        host.clients.get_mut(&1).unwrap().context = context;
        out
    }
    fn reply(host: &mut Host) {
        host.send_control(
            1,
            ServerMessage::Reply(Response {
                request_id: 1,
                status: None,
                output: Vec::new(),
                error: None,
            }),
        );
    }
    fn receive(host: &Host, rx: &Receiver<ServerMessage>) -> ServerMessage {
        let message = rx.try_recv().unwrap();
        if let Some(client) = host.clients.get(&1) {
            client
                .delivery_bytes
                .fetch_sub(message_bytes(&message), Ordering::AcqRel);
        }
        message
    }
    fn output(host: &mut Host, bytes: &[u8]) {
        host.send_control(
            1,
            ServerMessage::PaneOutput {
                pane_id: 0,
                sequence: 1,
                data: bytes.to_vec(),
            },
        );
    }
    fn check(host: &mut Host) {
        host.control.clients.get_mut(&1).unwrap().next_check = Instant::now();
        host.check_subscriptions(1);
    }
    fn records(rx: &Receiver<ServerMessage>) -> Vec<Vec<u8>> {
        rx.try_iter()
            .map(|message| match message {
                ServerMessage::Control { data } => data,
                other => panic!("unexpected: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn flags_and_sizes_follow_c_numeric_prefix_and_precedence() {
        let (mut host, _rx) = host(8);
        refresh(
            &mut host,
            &[
                "refresh",
                "-F",
                "no-output,ignore-size",
                "-f",
                "!no-output,new-layouts,pause-after=3junk,unknown",
            ],
        )
        .unwrap();
        assert_eq!(
            host.clients[&1].context.client_flags,
            "attached,focused,control-mode,ignore-size,new-layouts,pause-after=3,UTF-8"
        );
        refresh(&mut host, &["refresh", "-C", " +90x 35junk"]).unwrap();
        assert_eq!(
            (host.clients[&1].context.cols, host.clients[&1].context.rows),
            (90, 35)
        );
        refresh(&mut host, &["refresh", "-C", "@0:55x22"]).unwrap();
        assert_eq!(
            host.clients[&1].context.window_sizes.get(&0),
            Some(&(55, 22))
        );
        refresh(&mut host, &["refresh", "-C", "@0:"]).unwrap();
        assert!(host.clients[&1].context.window_sizes.is_empty());
        refresh(
            &mut host,
            &[
                "refresh",
                "-A",
                "%0:unknown",
                "-B",
                "ignored::x",
                "-C",
                "1,1",
            ],
        )
        .unwrap();
        assert!(host.control.clients[&1].subscriptions.is_empty());
        assert_eq!(host.clients[&1].context.cols, 90);
        assert_eq!(
            refresh(&mut host, &["refresh", "-C", "0,10"])
                .err()
                .as_deref(),
            Some("size too small or too big")
        );
        assert_eq!(
            refresh(&mut host, &["refresh", "-C", "bad"])
                .err()
                .as_deref(),
            Some("bad size argument")
        );
        refresh(&mut host, &["refresh", "-f", "read-only,!read-only"]).unwrap();
        assert!(
            host.clients[&1]
                .context
                .client_flags
                .contains(",read-only,")
        );
    }

    #[test]
    fn pause_follows_reply_and_resume_does_not_replay_suppressed_output() {
        let (mut host, rx) = host(8);
        refresh(&mut host, &["refresh", "-A", "%0:pause", "-A", "%0:pause"]).unwrap();
        assert!(rx.try_recv().is_err());
        output(&mut host, b"discarded");
        reply(&mut host);
        assert!(matches!(receive(&host, &rx), ServerMessage::Reply(_)));
        assert_eq!(
            receive(&host, &rx),
            ServerMessage::Control {
                data: b"%pause %0".to_vec()
            }
        );
        assert!(rx.try_recv().is_err());
        refresh(&mut host, &["refresh", "-A", "%0:continue"]).unwrap();
        reply(&mut host);
        assert!(matches!(receive(&host, &rx), ServerMessage::Reply(_)));
        assert_eq!(
            receive(&host, &rx),
            ServerMessage::Control {
                data: b"%continue %0".to_vec()
            }
        );
        output(&mut host, b"new");
        assert!(
            matches!(receive(&host, &rx), ServerMessage::PaneOutput { data, .. } if data == b"new")
        );
        refresh(&mut host, &["refresh", "-A", "%0:off"]).unwrap();
        output(&mut host, b"also discarded");
        refresh(&mut host, &["refresh", "-A", "%0:on"]).unwrap();
        output(&mut host, b"fresh");
        assert!(
            matches!(receive(&host, &rx), ServerMessage::PaneOutput { data, .. } if data == b"fresh")
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn redundant_on_and_continue_preserve_queued_output() {
        let (mut host, rx) = host(1);
        reply(&mut host);
        output(&mut host, b"queued");
        refresh(&mut host, &["refresh", "-A", "%0:on", "-A", "%0:continue"]).unwrap();
        assert_eq!(host.control.clients[&1].pending.len(), 1);
        receive(&host, &rx);
        host.flush_control(1);
        assert!(
            matches!(receive(&host, &rx), ServerMessage::PaneOutput { data, .. } if data == b"queued")
        );
    }

    #[test]
    fn queued_age_pauses_even_while_delivery_channel_is_full() {
        let (mut host, rx) = host(1);
        refresh(&mut host, &["refresh", "-f", "pause-after=1"]).unwrap();
        assert!(matches!(
            receive(&host, &rx),
            ServerMessage::ClientFlags { .. }
        ));
        reply(&mut host);
        output(&mut host, b"stale");
        host.control
            .clients
            .get_mut(&1)
            .unwrap()
            .pending
            .front_mut()
            .unwrap()
            .at = Instant::now() - Duration::from_secs(2);
        host.flush_control(1);
        assert!(host.control.clients[&1].panes[&0].1);
        receive(&host, &rx);
        host.flush_control(1);
        assert_eq!(
            receive(&host, &rx),
            ServerMessage::Control {
                data: b"%pause %0".to_vec()
            }
        );
        assert_eq!(host.clients[&1].delivery_bytes.load(Ordering::Acquire), 0);
        assert_eq!(host.control.clients[&1].pending_bytes, 0);
    }

    #[test]
    fn byte_overflow_and_default_age_disconnect_explicitly() {
        let (mut host, rx) = host(1);
        host.clients[&1]
            .delivery_bytes
            .store(BYTE_LIMIT, Ordering::Release);
        output(&mut host, b"x");
        assert!(!host.clients.contains_key(&1));
        assert_eq!(
            receive(&host, &rx),
            ServerMessage::Exit {
                reason: "control output exceeds byte budget".into()
            }
        );
        let (mut host, rx) = self::host(1);
        reply(&mut host);
        output(&mut host, b"stale");
        host.control
            .clients
            .get_mut(&1)
            .unwrap()
            .pending
            .front_mut()
            .unwrap()
            .at = Instant::now() - MAX_AGE - Duration::from_secs(1);
        receive(&host, &rx);
        host.flush_control(1);
        assert!(!host.clients.contains_key(&1));
        assert_eq!(
            receive(&host, &rx),
            ServerMessage::Exit {
                reason: "too far behind".into()
            }
        );
    }

    #[test]
    fn subscriptions_report_initial_changes_and_each_link_index() {
        let (mut host, rx) = host(16);
        host.engine
            .sessions
            .get_mut(&0)
            .unwrap()
            .windows
            .insert(4, 0);
        refresh(
            &mut host,
            &[
                "refresh",
                "-B",
                "session::#{session_name}",
                "-B",
                "panes:%*:#{window_index}:#{pane_id}",
                "-B",
                "windows:@*:#{window_index}",
            ],
        )
        .unwrap();
        check(&mut host);
        assert_eq!(
            records(&rx),
            [
                b"%subscription-changed session $0 - - - : test".to_vec(),
                b"%subscription-changed panes $0 @0 0 %0 : 0:%0".to_vec(),
                b"%subscription-changed panes $0 @0 4 %0 : 4:%0".to_vec(),
                b"%subscription-changed windows $0 @0 0 - : 0".to_vec(),
                b"%subscription-changed windows $0 @0 4 - : 4".to_vec(),
            ]
        );
        check(&mut host);
        assert!(records(&rx).is_empty());
        host.engine.sessions.get_mut(&0).unwrap().name = "renamed".into();
        check(&mut host);
        assert_eq!(
            records(&rx),
            [b"%subscription-changed session $0 - - - : renamed".to_vec()]
        );
        refresh(&mut host, &["refresh", "-B", "session"]).unwrap();
        host.engine.sessions.get_mut(&0).unwrap().name = "removed".into();
        check(&mut host);
        assert!(records(&rx).is_empty());
        assert!(subscription(b"bad:%-1:ignored").is_none());
        assert!(subscription(b"accepted:%0suffix:value").is_some());
    }
}
