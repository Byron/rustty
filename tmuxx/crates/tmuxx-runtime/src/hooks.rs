//! Hook storage and event payloads. Hook commands use isolated queue groups.
use super::*;
use tmuxx_command::{Args, Command};
use tmuxx_core::{HookScope, hook_options};
#[path = "hook_monitors.rs"]
mod monitors;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Scope {
    GlobalSession,
    GlobalWindow,
    Session(Id),
    Window(Id),
    Pane(Id),
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Index {
    Number(u32),
    Key(String),
}
impl Index {
    fn text(&self) -> String {
        match self {
            Self::Number(n) => n.to_string(),
            Self::Key(key) => key.clone(),
        }
    }
}
type HookArray = BTreeMap<Index, Vec<Command>>;
#[derive(Default)]
pub(super) struct Hooks {
    entries: BTreeMap<(Scope, String), HookArray>,
    monitors: BTreeMap<(Scope, String), monitors::HookMonitor>,
    fires: BTreeMap<(Scope, String), (u32, u64)>,
    clients: std::collections::BTreeSet<u64>,
    pub(super) attached: std::collections::BTreeSet<u64>,
}
pub(super) struct Invocation {
    pub commands: Vec<Command>,
    pub context: ClientContext,
}

pub(super) fn command_readonly(name: &[u8]) -> bool {
    tmuxx_command::registry::find(name).is_ok_and(|entry| entry.readonly)
}

fn metadata(name: &[u8]) -> Option<(&'static str, &'static str, bool)> {
    tmuxx_command::registry::find(name)
        .ok()
        .map(|entry| (entry.name, entry.flags, entry.after_hook))
}
pub(super) fn valid_event(name: &[u8]) -> bool {
    name.starts_with(b"@") || hook_options().any(|(key, _)| key.as_bytes() == name)
}
fn kind(name: &str) -> Option<HookScope> {
    hook_options()
        .find(|(key, _)| *key == name)
        .map(|(_, kind)| kind)
}
fn indexed(raw: &[u8]) -> Result<(String, Option<Index>), String> {
    let value = std::str::from_utf8(raw).map_err(|_| "invalid hook name")?;
    let (name, index) = tmuxx_core::resolve_option_name(value)?;
    Ok((
        name,
        index.map(|key| {
            if key.bytes().all(|b| b.is_ascii_digit()) {
                Index::Number(key.parse().unwrap())
            } else {
                Index::Key(key)
            }
        }),
    ))
}

fn printed_arguments(args: &Args, command: &Command) -> Vec<u8> {
    let mut words = Vec::new();
    let mut flags = vec![b'-'];
    for (flag, values) in &args.repeated {
        for value in values {
            if value.is_none() {
                flags.push(*flag);
            }
        }
    }
    if flags.len() > 1 {
        words.push(flags);
    }
    for (flag, values) in &args.repeated {
        for value in values.iter().flatten() {
            words.push(vec![b'-', *flag]);
            words.push(tmuxx_command::display_argument(value));
        }
    }
    words.extend(args.rest.iter().enumerate().map(|(index, value)| {
        command.argument_block(args, index).map_or_else(
            || tmuxx_command::display_argument(value),
            |block| [b"{ ".to_vec(), printed_commands(block), b" }".to_vec()].concat(),
        )
    }));
    words.join(&b' ')
}
pub(super) fn printed_commands(commands: &[Command]) -> Vec<u8> {
    commands
        .iter()
        .map(|command| {
            let name = command.args.first().map(Vec::as_slice).unwrap_or_default();
            if let Some((canonical, spec, _)) = metadata(name)
                && let Ok(args) = Args::parse(&command.args[1..], spec)
            {
                let mut value = canonical.as_bytes().to_vec();
                let arguments = printed_arguments(&args, command);
                if !arguments.is_empty() {
                    value.push(b' ');
                    value.extend(arguments);
                }
                return value;
            }
            command
                .args
                .iter()
                .map(|arg| tmuxx_command::display_argument(arg))
                .collect::<Vec<_>>()
                .join(&b' ')
        })
        .collect::<Vec<_>>()
        .join(b" ; ".as_slice())
}

impl Host {
    pub(super) fn refresh_format_clients(&mut self) {
        self.engine.clients = self
            .hooks
            .attached
            .iter()
            .filter_map(|id| self.clients.get(id).map(|client| client.context.clone()))
            .collect();
    }
    pub(super) fn hook_target(
        &mut self,
        target: Option<&[u8]>,
        context: &ClientContext,
    ) -> Result<ClientContext, String> {
        let ids = self.expand_job(
            b"#{session_id}:#{window_id}:#{pane_id}",
            target,
            context,
            &[],
        )?;
        let fields: Vec<_> = ids.split(|byte| *byte == b':').collect();
        let id = |prefix, field: &[u8]| {
            field
                .strip_prefix(&[prefix])
                .and_then(|value| std::str::from_utf8(value).ok())
                .and_then(|value| value.parse::<Id>().ok())
        };
        let mut context = context.clone();
        context.session = fields.first().and_then(|field| id(b'$', field));
        context.current_pane = fields.get(2).and_then(|field| id(b'%', field));
        Ok(context)
    }
    fn hook_scope(
        &self,
        args: &Args,
        name: &str,
        context: &ClientContext,
    ) -> Result<Scope, String> {
        let scope = kind(name).unwrap_or(if args.has(b'p') {
            HookScope::Pane
        } else if args.has(b'w') {
            HookScope::Window
        } else {
            HookScope::Session
        });
        if args.has(b'p') && scope == HookScope::Pane {
            return context
                .current_pane
                .map(Scope::Pane)
                .ok_or_else(|| "no current pane".into());
        }
        if args.has(b'g') {
            return Ok(if scope == HookScope::Session {
                Scope::GlobalSession
            } else {
                Scope::GlobalWindow
            });
        }
        if scope != HookScope::Session {
            return context
                .current_pane
                .and_then(|id| self.engine.panes.get(&id))
                .map(|pane| Scope::Window(pane.window))
                .ok_or_else(|| "no current window".into());
        }
        context
            .session
            .map(Scope::Session)
            .ok_or_else(|| "no current session".into())
    }
    fn hook_array(&self, name: &str, context: &ClientContext) -> Option<&HookArray> {
        let session = context.session.map(Scope::Session);
        let pane = context.current_pane.map(Scope::Pane);
        let window = context
            .current_pane
            .and_then(|id| self.engine.panes.get(&id))
            .map(|pane| Scope::Window(pane.window));
        [
            session,
            Some(Scope::GlobalSession),
            pane,
            window,
            Some(Scope::GlobalWindow),
        ]
        .into_iter()
        .flatten()
        .find_map(|scope| self.hooks.entries.get(&(scope, name.into())))
    }
    fn invoke_hook(&mut self, name: &str, mut context: ClientContext) -> Vec<Invocation> {
        if context.no_hooks {
            return Vec::new();
        }
        let pane = context.current_pane.map(Scope::Pane);
        let window = context
            .current_pane
            .and_then(|pane| self.engine.panes.get(&pane))
            .map(|pane| Scope::Window(pane.window));
        if let Some(scope) = [
            context.session.map(Scope::Session),
            Some(Scope::GlobalSession),
            pane,
            window,
            Some(Scope::GlobalWindow),
        ]
        .into_iter()
        .flatten()
        .find(|scope| self.monitor_option(*scope, name).is_some())
        {
            self.record_hook_fire(scope, name);
        }
        context.no_hooks = true;
        context
            .formats
            .insert(b"hook".to_vec(), name.as_bytes().to_vec());
        if name.starts_with('@') {
            let mut lookup = b"#{".to_vec();
            lookup.extend_from_slice(name.as_bytes());
            lookup.push(b'}');
            let pane = context
                .current_pane
                .filter(|id| self.engine.panes.contains_key(id));
            let window = pane.map(|id| self.engine.panes[&id].window);
            if let Ok(value) = self
                .engine
                .expand(&lookup, context.session, window, pane, &context)
                && let Ok(commands) = self.prepare_commands(&value, &context)
            {
                return vec![Invocation { commands, context }];
            }
            return Vec::new();
        }
        self.hook_array(name, &context)
            .map(|array| {
                array
                    .values()
                    .map(|commands| Invocation {
                        commands: commands.clone(),
                        context: context.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    pub(super) fn hook_command(
        &mut self,
        command: &Command,
        context: &ClientContext,
    ) -> Result<(Outcome, Vec<Invocation>), String> {
        let showing = command.args[0] == b"show-hooks";
        let args = Args::parse(
            &command.args[1..],
            if showing { "BF:gpt:w" } else { "agpERTt:uB:w" },
        )?;
        if args.has(b'B') {
            return self
                .hook_monitor_command(showing, &args, context)
                .map(|out| (out, Vec::new()));
        }
        if args.rest.len() > if showing { 1 } else { 2 } {
            return Err("too many arguments".into());
        }
        let target = self.hook_target(args.get(b't'), context)?;
        let mut out = Outcome::default();
        if !showing && (args.has(b'R') || args.has(b'E')) {
            if args.rest.len() != 1 {
                return Err("hook event requires one name".into());
            }
            let name = self.expand_job(&args.rest[0], args.get(b't'), context, &[])?;
            let name = std::str::from_utf8(&name).map_err(|_| "invalid hook name")?;
            if args.has(b'E') && !name.starts_with('@') {
                return Err("event name must start with @".into());
            }
            let context = if args.has(b'E') {
                self.hook_payload(name, &target, None)
            } else {
                target
            };
            if args.has(b'E') {
                self.queue_event(name, context);
                return Ok((out, Vec::new()));
            }
            let mut context = context;
            context.no_hooks = false;
            return Ok((out, self.invoke_hook(name, context)));
        }
        let (name, index) = if let Some(raw) = args.rest.first() {
            indexed(&self.expand_job(raw, args.get(b't'), context, &[])?)?
        } else if showing {
            (String::new(), None)
        } else {
            return Err("missing argument".into());
        };
        if !name.is_empty() && kind(&name).is_none() && (!showing || !name.starts_with('@')) {
            let mut delegated = vec![if showing {
                b"show-options".to_vec()
            } else {
                b"set-option".to_vec()
            }];
            for flag in if showing { &b"gpw"[..] } else { &b"agpuw"[..] } {
                if args.has(*flag) {
                    delegated.push(vec![b'-', *flag]);
                }
            }
            for flag in *b"tF" {
                if let Some(value) = args.get(flag) {
                    delegated.extend([vec![b'-', flag], value.to_vec()]);
                }
            }
            delegated.extend(args.rest.clone());
            let mut context = context.clone();
            if showing {
                context
                    .formats
                    .insert(b"hook_fire_count".to_vec(), b"0".to_vec());
            }
            let out = self.engine.execute(
                &Command {
                    blocks: Default::default(),
                    args: delegated,
                },
                &mut context,
            )?;
            if !showing && name.starts_with('@') && !args.has(b'u') {
                self.engine.user_hook_names.insert(name);
            }
            return Ok((out, Vec::new()));
        }
        if showing {
            let scope = self.hook_scope(&args, &name, &target)?;
            return Ok((
                self.show_hook_options(
                    &args,
                    &target,
                    scope,
                    (!name.is_empty()).then_some((name, index)),
                )?,
                Vec::new(),
            ));
        }
        let scope = self.hook_scope(&args, &name, &target)?;
        let key = (scope, name);
        if args.has(b'u') {
            if index.is_none() {
                self.hooks.fires.remove(&key);
            }
            if let Some(index) = index {
                if let Some(array) = self.hooks.entries.get_mut(&key) {
                    array.remove(&index);
                }
            } else {
                self.hooks.entries.remove(&key);
            }
        } else {
            let value = args.rest.get(1).ok_or("empty value")?;
            let commands = if let Some(block) = command.argument_block(&args, 1) {
                block.to_vec()
            } else {
                self.prepare_commands(value, context)?
            };
            if !args.has(b'a') && index.is_none() {
                self.hooks.entries.entry(key.clone()).or_default().clear();
            }
            if self.hooks.entries.len() >= 4096 && !self.hooks.entries.contains_key(&key) {
                return Err("hook limit exceeded".into());
            }
            if commands.len() > 4096 {
                return Err("hook command limit exceeded".into());
            }
            let array = self.hooks.entries.entry(key.clone()).or_default();
            if let Some(index) = index {
                array.insert(index, commands);
            } else {
                if !args.has(b'a') {
                    array.clear();
                }
                if !commands.is_empty() {
                    let index = (0..4096)
                        .map(Index::Number)
                        .find(|index| !array.contains_key(index))
                        .ok_or("hook array limit exceeded")?;
                    array.insert(index, commands);
                }
            }
        }
        let (scope, name) = key;
        let array = self.hooks.entries.get(&(scope, name.clone()));
        let map = match scope {
            Scope::GlobalSession => Some(&mut self.engine.options),
            Scope::GlobalWindow => Some(&mut self.engine.global_window_options),
            Scope::Session(id) => self
                .engine
                .sessions
                .get_mut(&id)
                .map(|session| &mut session.options),
            Scope::Window(id) => self
                .engine
                .windows
                .get_mut(&id)
                .map(|window| &mut window.options),
            Scope::Pane(id) => self.engine.panes.get_mut(&id).map(|pane| &mut pane.options),
        };
        if let Some(map) = map {
            let prefix = format!("{name}[");
            map.retain(|key, _| key != &name && !key.starts_with(&prefix));
            if array.is_some() || matches!(scope, Scope::GlobalSession | Scope::GlobalWindow) {
                map.insert(name.clone(), String::new());
            }
            for (index, commands) in array.into_iter().flatten() {
                map.insert(
                    format!("{name}[{}]", index.text()),
                    String::from_utf8_lossy(&printed_commands(commands)).into_owned(),
                );
            }
        }
        out.changed = true;
        self.engine.sequence = self.engine.sequence.wrapping_add(1);
        Ok((out, Vec::new()))
    }
}

#[derive(Clone)]
struct PaneState {
    window: Id,
    cols: u16,
    rows: u16,
    title: Vec<u8>,
    pid: Option<u32>,
    dead_status: Option<i32>,
    dead_signal: Option<String>,
}
#[derive(Clone, Default)]
pub(super) struct Graph {
    sessions: BTreeMap<Id, (String, BTreeMap<u32, Id>, u32)>,
    windows: BTreeMap<Id, (String, Id, u16, u16, bool)>,
    panes: BTreeMap<Id, PaneState>,
    selection: Option<(Id, u32, ClientContext)>,
}
impl Host {
    pub(super) fn command_hook_snapshot(
        &mut self,
        command: &Command,
        context: &ClientContext,
    ) -> Option<Graph> {
        let mut graph = self.hook_snapshot()?;
        if let Some(("select-window", spec, _)) =
            command.args.first().and_then(|name| metadata(name))
            && let Ok(args) = Args::parse(&command.args[1..], spec)
            && let Ok(ids) = self.expand_job(
                b"#{session_id}:#{window_index}",
                args.get(b't'),
                context,
                &[],
            )
            && let Ok(ids) = std::str::from_utf8(&ids)
            && let Some((session, index)) =
                ids.strip_prefix('$').and_then(|ids| ids.split_once(':'))
            && let (Ok(session), Ok(index)) = (session.parse(), index.parse())
            && let Ok(current) = self.hook_target(None, context)
        {
            graph.selection = Some((session, index, current));
        }
        Some(graph)
    }
    pub(super) fn hook_snapshot(&self) -> Option<Graph> {
        Some(Graph {
            selection: None,
            sessions: self
                .engine
                .sessions
                .iter()
                .map(|(id, s)| (*id, (s.name.clone(), s.windows.clone(), s.active)))
                .collect(),
            windows: self
                .engine
                .windows
                .iter()
                .map(|(id, w)| (*id, (w.name.clone(), w.active, w.cols, w.rows, w.zoomed)))
                .collect(),
            panes: self
                .engine
                .panes
                .iter()
                .map(|(id, p)| {
                    (
                        *id,
                        PaneState {
                            window: p.window,
                            cols: p.cols,
                            rows: p.rows,
                            title: p.terminal.title().to_vec(),
                            pid: p.pid,
                            dead_status: p.dead_status,
                            dead_signal: p.dead_signal.clone(),
                        },
                    )
                })
                .collect(),
        })
    }
    fn hook_payload(
        &self,
        name: &str,
        context: &ClientContext,
        graph: Option<&Graph>,
    ) -> ClientContext {
        let mut context = context.clone();
        context.formats.clear();
        context
            .formats
            .insert(b"hook".to_vec(), name.as_bytes().to_vec());
        let mut field = |name: &[u8], value: Vec<u8>| {
            context.formats.insert(name.to_vec(), value);
        };
        let pane = context.current_pane;
        let window = pane.and_then(|id| {
            graph
                .and_then(|g| g.panes.get(&id))
                .map(|pane| pane.window)
                .or_else(|| self.engine.panes.get(&id).map(|p| p.window))
        });
        if let Some(id) = context.session {
            field(b"hook_session", format!("${id}").into_bytes());
            if let Some(name) = graph
                .and_then(|g| g.sessions.get(&id))
                .map(|s| &s.0)
                .or_else(|| self.engine.sessions.get(&id).map(|s| &s.name))
            {
                field(b"hook_session_name", name.as_bytes().to_vec());
            }
        }
        if let Some(id) = window {
            field(b"hook_window", format!("@{id}").into_bytes());
            if let Some(name) = graph
                .and_then(|g| g.windows.get(&id))
                .map(|w| &w.0)
                .or_else(|| self.engine.windows.get(&id).map(|w| &w.name))
            {
                field(b"hook_window_name", name.as_bytes().to_vec());
            }
            let index = context
                .session
                .and_then(|sid| {
                    graph
                        .and_then(|g| g.sessions.get(&sid))
                        .map(|s| &s.1)
                        .or_else(|| self.engine.sessions.get(&sid).map(|s| &s.windows))
                })
                .and_then(|windows| windows.iter().find(|(_, wid)| **wid == id))
                .map(|(index, _)| *index);
            if let Some(index) = index {
                field(b"hook_window_index", index.to_string().into_bytes());
            }
        }
        if let Some(id) = pane {
            field(b"hook_pane", format!("%{id}").into_bytes());
        }
        let all = name.starts_with("client-") || name.starts_with('@') || name == "pane-created";
        let session = all
            || name.starts_with("pane-command-")
            || name.starts_with("alert-")
            || name.starts_with("session-")
            || matches!(name, "window-linked" | "window-unlinked");
        let window = all
            || name.starts_with("alert-")
            || name == "session-window-changed"
            || name.starts_with("window-")
            || name.starts_with("pane-");
        let pane = all || name.starts_with("pane-") || name == "window-pane-changed";
        let index = all
            || name.starts_with("pane-command-")
            || name.starts_with("alert-")
            || name == "session-window-changed"
            || matches!(name, "window-linked" | "window-unlinked");
        context.formats.retain(|key, _| match key.as_slice() {
            b"hook_session" | b"hook_session_name" => session,
            b"hook_window" | b"hook_window_name" => window,
            b"hook_window_index" => index,
            b"hook_pane" => pane,
            _ => true,
        });
        context
            .formats
            .insert(b"hook_event".to_vec(), name.as_bytes().to_vec());
        if all && !context.client_name.is_empty() {
            context.formats.insert(
                b"hook_client".to_vec(),
                context.client_name.as_bytes().to_vec(),
            );
        }
        context
    }
    fn graph_context(
        &self,
        graph: &Graph,
        session: Option<Id>,
        window: Option<Id>,
        pane: Option<Id>,
    ) -> ClientContext {
        let window = window.or_else(|| pane.and_then(|id| graph.panes.get(&id)).map(|p| p.window));
        let session = session.or_else(|| {
            window
                .and_then(|wid| {
                    graph
                        .sessions
                        .iter()
                        .find(|(_, s)| s.1.values().any(|id| *id == wid))
                })
                .map(|(id, _)| *id)
        });
        let pane = pane.or_else(|| window.and_then(|wid| graph.windows.get(&wid)).map(|w| w.1));
        ClientContext {
            session,
            current_pane: pane,
            ..ClientContext::default()
        }
    }
    fn queue_event(&mut self, name: &str, context: ClientContext) {
        self.signal_hook_event(name, &context);
        for invocation in self.invoke_hook(name, context) {
            self.internal_queue(invocation.context, invocation.commands, 0, false);
        }
    }
    pub(super) fn client_opened(&mut self, id: u64, context: &ClientContext) {
        if self.hooks.clients.insert(id) {
            self.client_event("client-created", context, None);
        }
    }
    pub(super) fn client_closed(&mut self, id: u64, context: &ClientContext) {
        if self.hooks.attached.remove(&id) {
            self.client_event("client-detached", context, None);
            let targets = self
                .clients
                .iter()
                .filter(|(target, client)| **target != id && client.subscribed)
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            for target in targets {
                self.send(
                    target,
                    ServerMessage::Control {
                        data: format!("%client-detached {}", context.client_name).into_bytes(),
                    },
                );
            }
        }
        if self.hooks.clients.remove(&id) {
            self.client_event("client-closed", context, None);
        }
    }
    pub(super) fn client_session_destroyed(&mut self, context: &ClientContext) {
        // C clears the old session before switching a client away from destruction.
        self.client_event(
            "client-session-changed",
            context,
            Some(&ClientContext::default()),
        );
    }
    fn client_event(
        &mut self,
        name: &str,
        context: &ClientContext,
        previous: Option<&ClientContext>,
    ) {
        let target = self
            .hook_target(None, context)
            .unwrap_or_else(|_| context.clone());
        let mut payload = self.hook_payload(name, &target, None);
        if let Some(previous) = previous {
            if name == "client-session-changed" {
                for (prefix, session) in [("new", context.session), ("old", previous.session)] {
                    if let Some(session) = session {
                        payload.formats.insert(
                            format!("hook_{prefix}_session").into_bytes(),
                            format!("${session}").into_bytes(),
                        );
                        if let Some(session) = self.engine.sessions.get(&session) {
                            payload.formats.insert(
                                format!("hook_{prefix}_session_name").into_bytes(),
                                session.name.as_bytes().to_vec(),
                            );
                        }
                    }
                }
            } else if name == "client-resized" {
                for (key, value) in [
                    ("width", context.cols),
                    ("height", context.rows),
                    ("old_width", previous.cols),
                    ("old_height", previous.rows),
                ] {
                    payload.formats.insert(
                        format!("hook_{key}").into_bytes(),
                        value.to_string().into_bytes(),
                    );
                }
            }
        }
        self.queue_event(name, payload);
    }
    pub(super) fn client_command_hooks(
        &mut self,
        id: u64,
        command: &Command,
        previous: &ClientContext,
        context: &ClientContext,
    ) {
        if !self.hooks.clients.contains(&id) {
            return;
        }
        let Some((name, spec, _)) = command.args.first().and_then(|name| metadata(name)) else {
            return;
        };
        let attached = name == "attach-session"
            || (name == "new-session"
                && Args::parse(&command.args[1..], spec).is_ok_and(|args| !args.has(b'd')));
        if attached || name == "switch-client" {
            if let Some(session) = context.session {
                self.engine.session_attached(session);
            }
            if context.session != previous.session {
                self.client_event("client-session-changed", context, Some(previous));
            }
        }
        if attached && self.hooks.attached.insert(id) && name == "attach-session" {
            self.client_event("client-attached", context, None);
        }
        if context.session.is_some() {
            for window in self.engine.windows.keys() {
                self.latest_clients.entry(*window).or_insert(id);
            }
            let resized = name == "refresh-client" && !self.clients[&id].control;
            let args = Args::parse(&command.args[1..], spec).ok();
            let selected = matches!(
                name,
                "select-window" | "next-window" | "previous-window" | "last-window"
            ) || (matches!(name, "new-window" | "break-pane")
                && args.as_ref().is_some_and(|args| !args.has(b'd')));
            if (attached || name == "switch-client" || resized || selected)
                && let Ok(target) =
                    self.hook_target(args.as_ref().and_then(|args| args.get(b't')), context)
                && let Some(window) = target
                    .current_pane
                    .and_then(|pane| self.engine.panes.get(&pane).map(|pane| pane.window))
            {
                self.client_became_latest(id, window);
            }
        }
    }
    pub(super) fn command_hooks(
        &mut self,
        command: &Command,
        context: &ClientContext,
        before: Option<Graph>,
        success: bool,
    ) -> Vec<Invocation> {
        self.prune_hook_monitors();
        let selection = before.as_ref().and_then(|before| {
            let (session, index, current) = before.selection.as_ref()?;
            Some((
                *session,
                *index,
                before.sessions.get(session)?.2,
                current.clone(),
            ))
        });
        let created = before
            .as_ref()
            .and_then(|before| {
                self.engine
                    .panes
                    .keys()
                    .find(|id| !before.panes.contains_key(id))
            })
            .copied();
        if let Some(before) = before
            && success
        {
            self.graph_hooks(before, context.no_hooks);
        }
        if context.no_hooks {
            return Vec::new();
        }
        let Some(name) = command.args.first() else {
            return Vec::new();
        };
        let metadata = metadata(name);
        let hook = if !success {
            "command-error".into()
        } else if let Some((canonical, _, _)) = metadata {
            let canonical = match canonical {
                "new-pane" => "split-window",
                "next-window" | "previous-window" | "last-window" => "select-window",
                canonical => canonical,
            };
            let hook = format!("after-{canonical}");
            if kind(&hook).is_none() {
                return Vec::new();
            }
            hook
        } else {
            return Vec::new();
        };
        let args = metadata.and_then(|(_, spec, _)| Args::parse(&command.args[1..], spec).ok());
        let mut target = self
            .hook_target(args.as_ref().and_then(|args| args.get(b't')), context)
            .unwrap_or_else(|_| context.clone());
        if hook == "after-select-window"
            && let Some(session) = target
                .session
                .and_then(|sid| self.engine.sessions.get(&sid))
        {
            let navigating = metadata.is_some_and(|(name, _, _)| name != "select-window")
                || args
                    .as_ref()
                    .is_some_and(|args| args.has(b'n') || args.has(b'p') || args.has(b'l'));
            if !navigating
                && let Some((sid, requested, active, current)) = selection
                && (session.active == active
                    || (args.as_ref().is_some_and(|args| args.has(b'T'))
                        && requested == active
                        && current.session != Some(sid)))
            {
                target = current;
            } else {
                target.current_pane = session
                    .windows
                    .get(&session.active)
                    .and_then(|wid| self.engine.windows.get(wid))
                    .map(|window| window.active);
            }
        }
        if let Some(created) = created
            && metadata.is_some_and(|(name, _, _)| {
                matches!(
                    name,
                    "new-session" | "new-window" | "split-window" | "new-pane"
                )
            })
        {
            target = self
                .hook_target(Some(format!("%{created}").as_bytes()), context)
                .unwrap_or(target);
        }
        if success && target.current_pane.is_none() {
            return Vec::new();
        }
        target.formats.clear();
        if let Some((canonical, _, _)) = metadata {
            target
                .formats
                .insert(b"command".to_vec(), canonical.as_bytes().to_vec());
        }
        target
            .formats
            .insert(b"hook".to_vec(), hook.as_bytes().to_vec());
        target
            .formats
            .insert(b"hook_event".to_vec(), hook.as_bytes().to_vec());
        if let Some(args) = args {
            target.formats.insert(
                b"hook_arguments".to_vec(),
                printed_arguments(&args, command),
            );
            for (index, value) in args.rest.iter().enumerate() {
                target
                    .formats
                    .insert(format!("hook_argument_{index}").into_bytes(), value.clone());
            }
            for (flag, values) in args.repeated {
                if let Some(value) = values.last() {
                    target.formats.insert(
                        format!("hook_flag_{}", flag as char).into_bytes(),
                        value.clone().unwrap_or_else(|| b"1".to_vec()),
                    );
                }
                for (index, value) in values.into_iter().flatten().enumerate() {
                    target.formats.insert(
                        format!("hook_flag_{}_{index}", flag as char).into_bytes(),
                        value,
                    );
                }
            }
        }
        self.signal_hook_event(&hook, &target);
        self.invoke_hook(&hook, target)
    }
    pub(super) fn graph_hooks(&mut self, before: Graph, suppress_hooks: bool) {
        let Some(after) = self.hook_snapshot() else {
            return;
        };
        let mut events = Vec::new();
        for (id, (name, _, _)) in &before.sessions {
            if !after.sessions.contains_key(id) {
                let mut context = self.graph_context(&before, Some(*id), None, None);
                context
                    .formats
                    .insert(b"hook_session_name".to_vec(), name.as_bytes().to_vec());
                events.push((
                    "session-closed",
                    self.hook_payload("session-closed", &context, Some(&before)),
                ));
            }
        }
        for (sid, (_, windows, _)) in &before.sessions {
            for wid in windows.values() {
                if !after
                    .sessions
                    .get(sid)
                    .is_some_and(|session| session.1.values().any(|id| id == wid))
                {
                    let context = self.graph_context(&before, Some(*sid), Some(*wid), None);
                    events.push((
                        "window-unlinked",
                        self.hook_payload("window-unlinked", &context, Some(&before)),
                    ));
                }
            }
        }
        for id in before
            .windows
            .keys()
            .filter(|id| !after.windows.contains_key(id))
        {
            let context = self.graph_context(&before, None, Some(*id), None);
            events.push((
                "window-closed",
                self.hook_payload("window-closed", &context, Some(&before)),
            ));
        }
        for (id, (name, windows, active)) in &after.sessions {
            let context = self.graph_context(&after, Some(*id), windows.get(active).copied(), None);
            let event = match before.sessions.get(id) {
                None => Some("session-created"),
                Some(old) if old.0 != *name => Some("session-renamed"),
                Some(old) if old.2 != *active => Some("session-window-changed"),
                _ => None,
            };
            if let Some(event) = event {
                events.push((event, self.hook_payload(event, &context, Some(&after))));
            }
            for wid in windows.values() {
                if !before
                    .sessions
                    .get(id)
                    .is_some_and(|session| session.1.values().any(|id| id == wid))
                {
                    let context = self.graph_context(&after, Some(*id), Some(*wid), None);
                    events.push((
                        "window-linked",
                        self.hook_payload("window-linked", &context, Some(&after)),
                    ));
                }
            }
        }
        for (id, state) in &after.windows {
            let context = self.graph_context(&after, None, Some(*id), None);
            let mut names = Vec::new();
            if let Some(old) = before.windows.get(id) {
                if old.0 != state.0 {
                    names.push("window-renamed");
                }
                if old.1 != state.1 {
                    names.push("window-pane-changed");
                }
                if (old.2, old.3) != (state.2, state.3) {
                    names.push("window-resized");
                }
                if old.4 != state.4 {
                    names.push(if state.4 {
                        "window-zoomed"
                    } else {
                        "window-unzoomed"
                    });
                }
            } else {
                names.push("window-created");
            }
            for event in names {
                events.push((event, self.hook_payload(event, &context, Some(&after))));
            }
        }
        for (id, pane) in &after.panes {
            let context = self.graph_context(&after, None, Some(pane.window), Some(*id));
            let mut names = Vec::new();
            if let Some(old) = before.panes.get(id) {
                if old.pid != pane.pid {
                    names.push("pane-created");
                }
                if old.window != pane.window {
                    names.push("pane-moved");
                }
                if (old.cols, old.rows) != (pane.cols, pane.rows) {
                    names.push("pane-resized");
                }
                if old.title != pane.title {
                    names.push("pane-title-changed");
                }
            } else {
                names.push("pane-created");
            }
            for event in names {
                let mut context = self.hook_payload(event, &context, Some(&after));
                context
                    .formats
                    .insert(b"hook_new_title".to_vec(), pane.title.clone());
                context
                    .formats
                    .insert(b"hook_width".to_vec(), pane.cols.to_string().into_bytes());
                context
                    .formats
                    .insert(b"hook_height".to_vec(), pane.rows.to_string().into_bytes());
                if let Some(old) = before.panes.get(id) {
                    context.formats.insert(
                        b"hook_old_width".to_vec(),
                        old.cols.to_string().into_bytes(),
                    );
                    context.formats.insert(
                        b"hook_old_height".to_vec(),
                        old.rows.to_string().into_bytes(),
                    );
                }
                events.push((event, context));
            }
        }
        for (name, mut context) in events {
            context.no_hooks = suppress_hooks;
            self.queue_event(name, context);
        }
        self.hooks.entries.retain(|(scope, _), _| match scope {
            Scope::GlobalSession | Scope::GlobalWindow => true,
            Scope::Session(id) => self.engine.sessions.contains_key(id),
            Scope::Window(id) => self.engine.windows.contains_key(id),
            Scope::Pane(id) => self.engine.panes.contains_key(id),
        });
    }
    pub(super) fn fire_shell_hook(
        &mut self,
        pane: Id,
        started: bool,
        state: tmuxx_core::ShellState,
    ) {
        let name = if started {
            "pane-command-started"
        } else {
            "pane-command-finished"
        };
        if let Some(graph) = self.hook_snapshot() {
            let context = self.graph_context(&graph, None, None, Some(pane));
            let mut payload = self.hook_payload(name, &context, Some(&graph));
            for (name, value) in [
                ("hook_command_line", state.command_line),
                (
                    "hook_command_status",
                    state.command_status.map(|n| n.to_string()),
                ),
                (
                    "hook_command_start_time",
                    state.command_start_time.map(|n| n.to_string()),
                ),
                (
                    "hook_command_end_time",
                    state.command_end_time.map(|n| n.to_string()),
                ),
                (
                    "hook_command_duration",
                    state.command_start_time.map(|start| {
                        (if state.command_running {
                            self.engine.now / 1_000_000
                        } else {
                            state.command_end_time.unwrap_or(0)
                        })
                        .saturating_sub(start)
                        .to_string()
                    }),
                ),
            ] {
                if let Some(value) = value {
                    payload
                        .formats
                        .insert(name.as_bytes().to_vec(), value.into_bytes());
                }
            }
            self.queue_event(name, payload);
        }
    }
    pub(super) fn fire_winlink_hook(&mut self, name: &str, session: Id, index: u32, window: Id) {
        if let Some(graph) = self.hook_snapshot() {
            let context = self.graph_context(&graph, Some(session), Some(window), None);
            let mut payload = self.hook_payload(name, &context, Some(&graph));
            payload.formats.insert(
                b"hook_window_index".to_vec(),
                index.to_string().into_bytes(),
            );
            payload
                .formats
                .insert(b"window_index".to_vec(), index.to_string().into_bytes());
            self.queue_event(name, payload);
        }
    }
    pub(super) fn fire_pane_hook(&mut self, name: &str, pane: Id) {
        if let Some(graph) = self.hook_snapshot() {
            let context = self.graph_context(&graph, None, None, Some(pane));
            self.queue_event(name, self.hook_payload(name, &context, Some(&graph)));
        }
    }

    pub(super) fn process_exit_hooks(&mut self, pane: Id, before: Option<Graph>) {
        let Some(before) = before else {
            return;
        };
        let event = if self
            .engine
            .panes
            .get(&pane)
            .is_some_and(|pane| pane.status.is_some())
        {
            "pane-died"
        } else {
            "pane-exited"
        };
        let context = self.graph_context(&before, None, None, Some(pane));
        let mut payload = self.hook_payload(event, &context, Some(&before));
        if let Some(pane) = before.panes.get(&pane) {
            if let Some(status) = pane.dead_status {
                payload.formats.insert(
                    b"hook_exit_status".to_vec(),
                    status.to_string().into_bytes(),
                );
            }
            if let Some(signal) = &pane.dead_signal {
                payload
                    .formats
                    .insert(b"hook_exit_signal".to_vec(), signal.as_bytes().to_vec());
            }
            payload.formats.insert(
                b"hook_exit_success".to_vec(),
                u8::from(pane.dead_status == Some(0))
                    .to_string()
                    .into_bytes(),
            );
        }
        self.queue_event(event, payload);
        self.graph_hooks(before, false);
    }
}
