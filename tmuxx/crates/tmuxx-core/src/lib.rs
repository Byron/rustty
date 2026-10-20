//! Authoritative tmuxx state. All operating-system work is returned as effects.

use std::collections::{BTreeMap, BTreeSet};
mod admission;
mod alerts;
mod buffers;
mod client_commands;
mod clipboard;
mod floating;
mod format_metadata;
mod formats;
mod formats_client;
mod formats_collections;
mod formats_graph;
mod formats_terminal;
mod graph;
mod images;
mod selection;
mod terminal_options;
pub use formats_terminal::ShellState;
mod input;
mod layouts;
mod names;
mod options;
mod sizes;
mod styles;
pub use names::{clean_name, parse_window_name};
mod format_jobs;
pub use format_jobs::FormatJobRequest;
mod panes;
mod process;
mod targets;
pub use options::{HookScope, hook_options, resolve_option_name};
use tmuxx_command::{Args, Command};
use tmuxx_layout::{Axis, Layout};
use tmuxx_terminal::Terminal;
pub use tmuxx_terminal::{HostAction, HostResponse};

pub type Id = u64;
pub const MAX_DIAGNOSTIC_MESSAGES: usize = 16_384;

#[derive(Clone, Copy)]
struct Marked {
    session: Id,
    index: u32,
    window: Id,
    pane: Id,
}

#[derive(Debug, Clone)]
pub enum Effect {
    Alert {
        session: Id,
        index: u32,
        window: Id,
        event: &'static str,
    },
    ShellPrompt {
        pane: Id,
    },
    ShellCommand {
        pane: Id,
        started: bool,
        state: ShellState,
    },
    WindowRenamed {
        window: Id,
        name: String,
    },
    ClipboardChanged {
        pane: Option<Id>,
        buffer: Option<String>,
        deleted: Vec<String>,
    },
    Empty {
        pane: Id,
    },
    FeedStdin {
        pane: Id,
    },
    WaitPane {
        pane: Id,
    },
    Pipe {
        pane: Id,
        command: Option<Vec<u8>>,
        input: bool,
        output: bool,
    },
    Host {
        pane: Id,
        action: tmuxx_terminal::HostAction,
    },
    ClientClipboard {
        client: String,
        pane: Id,
        data: Vec<u8>,
    },
    Spawn {
        pane: Id,
        shell: String,
        argv: Vec<Vec<u8>>,
        cwd: Option<String>,
        env: BTreeMap<String, String>,
        cols: u16,
        rows: u16,
    },
    Write {
        pane: Id,
        data: Vec<u8>,
    },
    Resize {
        pane: Id,
        cols: u16,
        rows: u16,
    },
    Kill {
        pane: Id,
    },
    Shutdown,
    LoadBuffer {
        name: Option<Vec<u8>>,
        path: Vec<u8>,
        clipboard: Option<(String, Id)>,
    },
    SaveBuffer {
        path: Vec<u8>,
        data: Vec<u8>,
        append: bool,
    },
}

#[derive(Clone, Debug)]
pub enum Notification {
    Raw(Vec<u8>),
    WindowLinked {
        window: Id,
        linked_sessions: BTreeSet<Id>,
    },
    WindowUnlinked {
        window: Id,
        linked_sessions: BTreeSet<Id>,
    },
    WindowRenamed {
        window: Id,
        name: String,
        linked_sessions: BTreeSet<Id>,
    },
    LayoutChange {
        window: Id,
        legacy: (String, String),
        extended: (String, String),
        session_flags: BTreeMap<Id, Vec<u8>>,
        native_flags: &'static str,
    },
}
impl From<Vec<u8>> for Notification {
    fn from(data: Vec<u8>) -> Self {
        Self::Raw(data)
    }
}

#[derive(Default, Debug)]
pub struct Outcome {
    pub output: Vec<u8>,
    pub effects: Vec<Effect>,
    pub notifications: Vec<Notification>,
    pub client_notifications: Vec<(String, Vec<u8>)>,
    pub detach: bool,
    pub detach_hangup: bool,
    pub changed: bool,
}

#[derive(Clone, Debug)]
pub struct ClientContext {
    pub created: u64,
    pub activity: u64,
    pub session: Option<Id>,
    pub last_session: Option<Id>,
    /// Authenticated identity supplied by the runtime, separate from format overrides.
    pub identity: BTreeMap<String, String>,
    pub control: bool,
    /// Attachment is independent of control transport and of the command's target session.
    pub attached: bool,
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
    pub cwd: Option<String>,
    pub environment: BTreeMap<String, String>,
    pub host_effects: bool,
    pub size_requested: bool,
    pub window_size_requested: bool,
    pub resize_order: u64,
    pub current_pane: Option<Id>,
    pub formats: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Canonical queue item name; explicit command-state formats take precedence.
    pub command: Option<&'static str>,
    pub format_type: Option<u8>,
    pub format_client: Option<bool>,
    pub native_view: bool,
    pub no_hooks: bool,
    pub no_format_jobs: bool,
    pub client_flags: String,
    pub client_name: String,
    pub ignore_size: bool,
    pub window_sizes: BTreeMap<Id, (u16, u16)>,
}

impl Default for ClientContext {
    fn default() -> Self {
        Self {
            created: 0,
            activity: 0,
            session: None,
            last_session: None,
            identity: BTreeMap::new(),
            control: false,
            attached: false,
            cols: 80,
            rows: 24,
            cell_width: 0,
            cell_height: 0,
            cwd: None,
            environment: BTreeMap::new(),
            host_effects: false,
            size_requested: false,
            window_size_requested: false,
            resize_order: 0,
            current_pane: None,
            formats: BTreeMap::new(),
            command: None,
            format_type: None,
            format_client: None,
            native_view: false,
            no_hooks: false,
            no_format_jobs: false,
            client_flags: String::new(),
            client_name: String::new(),
            ignore_size: false,
            window_sizes: BTreeMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct Pane {
    pub id: Id,
    pub window: Id,
    pub cols: u16,
    pub rows: u16,
    pub x: i32,
    pub y: i32,
    pub terminal: Terminal,
    pub history_limit: usize,
    pub argv: Vec<Vec<u8>>,
    pub shell: String,
    pub shell_state: ShellState,
    pub cwd: Option<String>,
    pub current_command: Option<String>,
    pub current_path: Option<String>,
    pub pid: Option<u32>,
    pub tty: String,
    pub status: Option<i32>,
    pub dead_status: Option<i32>,
    pub dead_signal: Option<String>,
    pub dead_time: Option<u64>,
    pub last_output_time: Option<u64>,
    pub output_generation: u64,
    pub persistent: bool,
    pub input_disabled: bool,
    pub empty: bool,
    pub pipe: bool,
    pub pipe_pid: Option<u32>,
    pub saved_float: Option<tmuxx_layout::Geometry>,
    pub modal_capture_keys: bool,
    pub modal_close_click: bool,
    pub modal_close_cancel: bool,
    pub options: BTreeMap<String, String>,
}

#[derive(Clone)]
pub struct Window {
    pub id: Id,
    silence_deadline: Option<u64>,
    pub activity: u64,
    pub name: String,
    pub panes: Vec<Id>,
    pub active: Id,
    pub last: Vec<Id>,
    pub cols: u16,
    pub rows: u16,
    pub zoomed: bool,
    pub manual_size: (u16, u16),
    pub zoomed_pane: Option<Id>,
    pub zoom_geometry: BTreeMap<u32, tmuxx_layout::Geometry>,
    pub over_zoom: BTreeSet<Id>,
    pub layout: Layout,
    pub last_layout: Option<usize>,
    pub old_layout: Option<Layout>,
    pub last_float: (i32, i32),
    pub modal: Option<Id>,
    pub options: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Default)]
struct Winlink {
    order: u64,
    alerts: u8,
}

#[derive(Clone)]
pub struct Session {
    pub id: Id,
    pub created: u64,
    pub activity: u64,
    pub last_attached: u64,
    pub cwd: Option<String>,
    links: BTreeMap<u32, Winlink>,
    selected_windows: Vec<Id>,
    pub name: String,
    pub windows: BTreeMap<u32, Id>,
    pub active: u32,
    pub last: Vec<u32>,
    pub group: Option<String>,
    pub options: BTreeMap<String, String>,
    pub environment: BTreeMap<String, String>,
    pub hidden_environment: BTreeSet<String>,
    pub removed_environment: BTreeSet<String>,
}

pub struct Engine {
    /// Owner-supplied microseconds since the Unix epoch. Pure commands advance by one.
    pub now: u64,
    pub started: u64,
    /// OS identity is supplied by the owner; the core performs no OS lookups.
    pub identity: BTreeMap<String, String>,
    /// Owner-supplied search path used only when PATH is absent at every level.
    /// Explicit hidden/removed values and pane/client overrides still win.
    pub default_child_path: String,
    pub sessions: BTreeMap<Id, Session>,
    pub windows: BTreeMap<Id, Window>,
    pub panes: BTreeMap<Id, Pane>,
    pub options: BTreeMap<String, String>,
    pub environment: BTreeMap<String, String>,
    pub hidden_environment: BTreeSet<String>,
    pub removed_environment: BTreeSet<String>,
    pub server_options: BTreeMap<String, String>,
    pub global_window_options: BTreeMap<String, String>,
    /// Runtime hook registrations affect user-option listing without changing their string values.
    pub user_hook_names: BTreeSet<String>,
    pub monitored_user_options: BTreeSet<(HookScope, Option<Id>, String)>,
    pub buffers: BTreeMap<String, Vec<u8>>,
    buffer_created: BTreeMap<String, u64>,
    /// Attached client contexts supplied by the owner for client format loops.
    pub clients: Vec<ClientContext>,
    format_jobs: std::cell::RefCell<format_jobs::FormatJobs>,
    image_quota: tmuxx_terminal::ImageQuota,
    automatic_buffers: BTreeSet<String>,
    buffer_order: Vec<String>,
    pub sequence: u64,
    marked: Option<Marked>,
    next_link: u64,
    next_session: Id,
    next_window: Id,
    next_pane: Id,
    next_buffer: u64,
}

impl Default for Engine {
    fn default() -> Self {
        let now = timestamp();
        Self {
            now,
            started: now,
            identity: BTreeMap::new(),
            default_child_path: "/usr/bin:/bin".into(),
            sessions: BTreeMap::new(),
            windows: BTreeMap::new(),
            panes: BTreeMap::new(),
            options: options::defaults(b'c'),
            server_options: options::defaults(b's'),
            global_window_options: options::defaults(b'w'),
            user_hook_names: BTreeSet::new(),
            monitored_user_options: BTreeSet::new(),
            automatic_buffers: BTreeSet::new(),
            environment: BTreeMap::new(),
            hidden_environment: BTreeSet::new(),
            removed_environment: BTreeSet::new(),
            buffers: BTreeMap::new(),
            buffer_created: BTreeMap::new(),
            clients: Vec::new(),
            format_jobs: Default::default(),
            image_quota: tmuxx_terminal::ImageQuota::default(),
            buffer_order: Vec::new(),
            sequence: 0,
            marked: None,
            next_link: 0,
            next_session: 0,
            next_window: 0,
            next_pane: 0,
            next_buffer: 0,
        }
    }
}

pub fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .min(u64::MAX as u128) as u64
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
fn line(out: &mut Vec<u8>, bytes: impl AsRef<[u8]>) {
    out.extend_from_slice(bytes.as_ref());
    out.push(b'\n');
}

struct EnvironmentUpdate {
    pattern: String,
    matches: Vec<(String, String)>,
}

fn client_flag(context: &mut ClientContext, name: &str, enabled: bool) {
    let mut flags = context
        .client_flags
        .split(',')
        .filter(|flag| !flag.is_empty() && *flag != name)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if enabled {
        flags.push(name.to_owned());
    }
    context.client_flags = flags.join(",");
}

impl Engine {
    pub fn session_attached(&mut self, id: Id) {
        if let Some(session) = self.sessions.get_mut(&id) {
            session.activity = self.now;
            session.last_attached = self.now;
            if let Some(link) = session.links.get_mut(&session.active) {
                link.alerts = 0;
            }
        }
    }
    fn prepare_environment_update(
        &self,
        sid: Option<Id>,
        source: &BTreeMap<String, String>,
    ) -> Result<Vec<EnvironmentUpdate>, String> {
        let mut updates = Vec::new();
        for (_, pattern) in self.option_array(sid, "update-environment") {
            let mut matches = Vec::new();
            for (name, value) in source {
                if tmuxx_format::glob_match(pattern.as_bytes(), name.as_bytes())? {
                    matches.push((name.clone(), value.clone()));
                }
            }
            updates.push(EnvironmentUpdate { pattern, matches });
        }
        Ok(updates)
    }
    fn update_environment(&mut self, sid: Id, updates: Vec<EnvironmentUpdate>) {
        for EnvironmentUpdate { pattern, matches } in updates {
            let session = self.sessions.get_mut(&sid).unwrap();
            if matches.is_empty() {
                session.environment.remove(&pattern);
                session.hidden_environment.remove(&pattern);
                session.removed_environment.insert(pattern);
            } else {
                for (name, value) in matches {
                    session.environment.insert(name.clone(), value.clone());
                    session.hidden_environment.remove(&name);
                    session.removed_environment.remove(&name);
                }
            }
        }
    }

    pub fn native_input(
        &mut self,
        pid: Id,
        input: &tmuxx_terminal::Input,
    ) -> Result<Outcome, String> {
        let pane = self.panes.get(&pid).ok_or("pane not found")?;
        if self.windows[&pane.window].modal == Some(pid) && pane.modal_close_cancel {
            use tmuxx_terminal::{Input, Key, KeyAction};
            if let Input::Key { event, .. } = input
                && event.action != KeyAction::Release
                && !event.modifiers.alt
                && !event.modifiers.super_key
                && !event.modifiers.shift
                && ((event.key == Key::Escape && !event.modifiers.control)
                    || (event.key == Key::Char('c') && event.modifiers.control))
            {
                return Ok(self.close_pane(pid));
            }
        }
        if pane.status.is_some() {
            if self
                .pane_option(pid, "remain-on-exit")
                .is_some_and(|value| matches!(value, "key" | "failed-key"))
                && matches!(input, tmuxx_terminal::Input::Key { event, .. } if event.action != tmuxx_terminal::KeyAction::Release)
            {
                return Ok(self.close_pane(pid));
            }
            return Err("pane is dead".into());
        }
        if pane.input_disabled {
            return Err("pane input is disabled".into());
        }
        let targets = if matches!(
            input,
            tmuxx_terminal::Input::Mouse(_) | tmuxx_terminal::Input::Reply(_)
        ) {
            vec![pid]
        } else {
            self.input_targets(pid)
        };
        let mut out = Outcome::default();
        let mut total = 0usize;
        for pane in targets {
            let data = self.encode_pane_input(pane, input);
            total = total.checked_add(data.len()).ok_or("input too large")?;
            if total > 16 * 1024 * 1024 {
                return Err("input exceeds 16 MiB command limit".into());
            }
            out.effects.push(Effect::Write { pane, data });
        }
        Ok(out)
    }

    fn input_targets(&self, pid: Id) -> Vec<Id> {
        let pane = &self.panes[&pid];
        let window = &self.windows[&pane.window];
        if pane.status.is_some() || pane.input_disabled {
            return Vec::new();
        }
        let mut targets = vec![pid];
        if self.pane_option(pid, "synchronize-panes") == Some("on") && !window.zoomed {
            targets.extend(window.panes.iter().copied().filter(|id| {
                *id != pid
                    && self.panes[id].status.is_none()
                    && !self.panes[id].input_disabled
                    && self.pane_option(*id, "synchronize-panes") == Some("on")
            }));
        }
        targets
    }

    pub fn spawn_environment(&self, sid: Id) -> BTreeMap<String, String> {
        let session = &self.sessions[&sid];
        let mut environment = self.environment.clone();
        environment.extend(session.environment.clone());
        for key in self
            .hidden_environment
            .iter()
            .chain(&self.removed_environment)
            .filter(|key| !session.environment.contains_key(*key))
            .chain(&session.hidden_environment)
            .chain(&session.removed_environment)
        {
            environment.remove(key);
        }
        environment
    }

    pub fn session_option(&self, sid: Id, key: &str) -> Option<&str> {
        self.sessions
            .get(&sid)
            .and_then(|session| session.options.get(key))
            .or_else(|| self.options.get(key))
            .map(String::as_str)
    }
    fn sessions_by_name(&self) -> Vec<&Session> {
        let mut sessions: Vec<_> = self.sessions.values().collect();
        sessions.sort_by(|a, b| a.name.cmp(&b.name));
        sessions
    }
    #[allow(clippy::too_many_arguments)]
    fn create_pane(
        &mut self,
        wid: Id,
        sid: Id,
        cols: u16,
        rows: u16,
        argv: Vec<Vec<u8>>,
        cwd: Option<String>,
        env: BTreeMap<String, String>,
        out: &mut Outcome,
    ) -> Id {
        let id = self.next_pane;
        self.next_pane += 1;
        let history = self
            .session_option(sid, "history-limit")
            .and_then(|s| s.parse().ok())
            .unwrap_or(2000);
        let shell = self
            .session_option(sid, "default-shell")
            .unwrap_or("/bin/sh")
            .to_owned();
        let argv = if argv.is_empty() {
            self.window_option(wid, "window-default-command")
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    self.session_option(sid, "default-command")
                        .filter(|value| !value.is_empty())
                })
                .map(|value| vec![value.as_bytes().to_vec()])
                .unwrap_or(argv)
        } else {
            argv
        };
        self.panes.insert(
            id,
            Pane {
                id,
                window: wid,
                cols,
                rows,
                x: 0,
                y: 0,
                terminal: {
                    let mut terminal = Terminal::new(cols, rows, history);
                    let cell = self
                        .windows
                        .get(&wid)
                        .and_then(|window| self.panes.get(&window.active))
                        .map_or((16, 32), |pane| pane.terminal.cell_size());
                    terminal.set_cell_size(cell.0, cell.1);
                    if let Some(host) = self.identity.get("host") {
                        terminal.set_title(host.as_bytes());
                    }
                    terminal
                },
                history_limit: history,
                argv: argv.clone(),
                shell: shell.clone(),
                shell_state: ShellState::default(),
                cwd: cwd.clone(),
                current_command: None,
                current_path: None,
                pid: None,
                tty: String::new(),
                status: None,
                dead_status: None,
                dead_signal: None,
                dead_time: None,
                last_output_time: None,
                output_generation: 0,
                persistent: true,
                input_disabled: false,
                empty: false,
                pipe: false,
                pipe_pid: None,
                saved_float: None,
                modal_capture_keys: false,
                modal_close_click: false,
                modal_close_cancel: false,
                options: BTreeMap::new(),
            },
        );
        out.effects.push(Effect::Spawn {
            pane: id,
            shell,
            argv,
            cwd,
            env,
            cols,
            rows,
        });
        id
    }

    #[allow(clippy::too_many_arguments)]
    fn create_window(
        &mut self,
        sid: Id,
        name: Option<String>,
        cols: u16,
        rows: u16,
        argv: Vec<Vec<u8>>,
        cwd: Option<String>,
        detached: bool,
        index: u32,
        environment: BTreeMap<String, String>,
        out: &mut Outcome,
    ) -> (u32, Id, Id) {
        let wid = self.next_window;
        self.next_window += 1;
        let pane = self.create_pane(wid, sid, cols, rows, argv, cwd, environment, out);
        let window_options = if name.is_some() {
            BTreeMap::from([("automatic-rename".into(), "off".into())])
        } else {
            BTreeMap::new()
        };
        let name = name.unwrap_or_else(|| self.panes[&pane].current_command());
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
                over_zoom: BTreeSet::new(),
                layout: Layout::pane(pane as u32, cols as u32, rows as u32),
                last_layout: None,
                old_layout: None,
                last_float: (0, 0),
                modal: None,
                options: window_options,
            },
        );
        self.reset_silence(wid);
        self.insert_link(sid, index, wid, 0);
        let session = self.sessions.get_mut(&sid).unwrap();
        session.selected_windows.push(wid);
        if session.windows.len() == 1 {
            session.select(index);
        } else if !detached {
            self.select_session_window(sid, index, out);
        }
        out.notifications.push(self.window_linked(wid));
        (index, wid, pane)
    }

    pub fn execute(
        &mut self,
        command: &Command,
        ctx: &mut ClientContext,
    ) -> Result<Outcome, String> {
        let previous = ctx.command;
        ctx.command = command
            .args
            .first()
            .and_then(|name| tmuxx_command::registry::find(name).ok())
            .map(|entry| entry.name);
        let result = self.execute_inner(command, ctx);
        ctx.command = previous;
        let mut out = result?;
        out.effects
            .extend(self.finish_window_selections(ctx.control.then_some(ctx.session).flatten()));
        if out.changed {
            out.effects.extend(self.synchronize_terminal_options());
        }
        Ok(out)
    }

    fn execute_inner(
        &mut self,
        command: &Command,
        ctx: &mut ClientContext,
    ) -> Result<Outcome, String> {
        if command.args.is_empty() {
            return Ok(Outcome::default());
        }
        let name = tmuxx_command::registry::validate_server(command)?
            .name
            .to_owned();
        self.now = self.now.saturating_add(1);
        let args = &command.args[1..];
        let mut out = Outcome::default();
        match name.as_str() {
            "new-session" | "new" => {
                let a = Args::parse(args, "Ac:dDe:EF:f:n:Ps:t:x:Xy:")?;
                let environment = process::environment_overrides(&a)?;
                if !ctx.control && !a.has(b'd') {
                    return Err(
                        "ordinary terminal attachment is not supported; use Rustty or -C".into(),
                    );
                }
                let session_name = if let Some(name) = a.get(b's') {
                    let name = self.expand(name, None, None, None, ctx)?;
                    clean_name(
                        std::str::from_utf8(&name).map_err(|_| "invalid session name")?,
                        false,
                    )
                } else {
                    self.next_session.to_string()
                };
                let window_name = a
                    .get(b'n')
                    .map(|name| {
                        let name = self.expand(name, None, None, None, ctx)?;
                        Ok::<_, String>(clean_name(
                            std::str::from_utf8(&name).map_err(|_| "invalid window name")?,
                            false,
                        ))
                    })
                    .transpose()?;
                if let Some(existing) = self.sessions.values().find(|s| s.name == session_name) {
                    if a.has(b'A') {
                        if !ctx.control {
                            return Err(
                                "ordinary terminal attachment is not supported; use Rustty or -C"
                                    .into(),
                            );
                        }
                        let sid = existing.id;
                        if !a.has(b'E') {
                            let updates =
                                self.prepare_environment_update(Some(sid), &ctx.environment)?;
                            self.update_environment(sid, updates);
                        }
                        ctx.set_session(Some(sid));
                        ctx.attached = true;
                        self.session_attached(sid);
                        out.notifications.push(
                            format!("%session-changed ${sid} {}", self.sessions[&sid].name)
                                .into_bytes()
                                .into(),
                        );
                        return Ok(out);
                    }
                    return Err(format!("duplicate session: {session_name}"));
                }
                if a.has(b't') && (!a.rest.is_empty() || a.has(b'n')) {
                    return Err("command or window name given with target".into());
                }
                let group = a.string(b't').map(|target| self.group_target(&target, ctx));
                let size = self.default_size_raw(None);
                let dimension = |flag, default, client: u16, label| -> Result<u32, String> {
                    let Some(value) = a.get(flag) else {
                        return Ok(default);
                    };
                    if value == b"-" {
                        return Ok(u32::from(client));
                    }
                    let number = text(value)
                        .parse::<i64>()
                        .map_err(|_| format!("{label} invalid"))?;
                    if number < 1 {
                        return Err(format!("{label} too small"));
                    }
                    if number > u16::MAX as i64 {
                        return Err(format!("{label} too large"));
                    }
                    Ok(number as u32)
                };
                let cols = dimension(b'x', size.0, ctx.cols, "width")?;
                let rows = dimension(b'y', size.1, ctx.rows, "height")?;
                let cwd = a
                    .get(b'c')
                    .map(|value| {
                        self.expand(value, None, None, None, ctx)
                            .and_then(|value| admission::path(&value))
                    })
                    .transpose()?
                    .or_else(|| ctx.cwd.clone());
                let updates = if a.has(b'E') {
                    Vec::new()
                } else {
                    self.prepare_environment_update(None, &ctx.environment)?
                };
                let sid = self.next_session;
                self.next_session += 1;
                self.sessions.insert(
                    sid,
                    Session {
                        id: sid,
                        created: self.now,
                        activity: self.now,
                        last_attached: 0,
                        cwd: cwd.clone(),
                        links: BTreeMap::new(),
                        selected_windows: Vec::new(),
                        name: session_name.clone(),
                        windows: BTreeMap::new(),
                        active: 0,
                        last: Vec::new(),
                        group: None,
                        options: if a.has(b'x') || a.has(b'y') {
                            BTreeMap::from([("default-size".into(), format!("{cols}x{rows}"))])
                        } else {
                            BTreeMap::new()
                        },
                        environment: BTreeMap::new(),
                        hidden_environment: BTreeSet::new(),
                        removed_environment: BTreeSet::new(),
                    },
                );
                self.update_environment(sid, updates);
                let session = self.sessions.get_mut(&sid).unwrap();
                for (name, value) in environment {
                    session.hidden_environment.remove(&name);
                    session.removed_environment.remove(&name);
                    session.environment.insert(name, value);
                }
                let (cols, rows, cell_width, cell_height) = self.new_window_size(sid, ctx);
                let (wid, pane) = if let Some((group, Some(source))) = &group {
                    // C allocates then discards an initial window before joining.
                    let temporary = self.next_window;
                    out.notifications.push(Notification::WindowLinked {
                        window: temporary,
                        linked_sessions: BTreeSet::from([sid]),
                    });
                    self.next_window += 1;
                    self.next_pane += 1;
                    let windows = self.sessions[source].windows.clone();
                    let initial = self
                        .options
                        .get("base-index")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    let active = if windows.contains_key(&initial) {
                        initial
                    } else {
                        *windows.first_key_value().unwrap().0
                    };
                    let links = self.sessions[source].links.clone();
                    self.sessions.get_mut(source).unwrap().group = Some(group.clone());
                    let session = self.sessions.get_mut(&sid).unwrap();
                    session.windows.clear();
                    session.active = active;
                    session.group = Some(group.clone());
                    for (&index, &window) in &windows {
                        self.insert_link(
                            sid,
                            index,
                            window,
                            links.get(&index).map_or(0, |link| link.alerts),
                        );
                        out.notifications.push(self.window_linked(window));
                    }
                    out.notifications.push(Notification::WindowUnlinked {
                        window: temporary,
                        linked_sessions: BTreeSet::new(),
                    });
                    let session = &self.sessions[&sid];
                    let wid = session.windows[&active];
                    (wid, self.windows[&wid].active)
                } else {
                    self.sessions.get_mut(&sid).unwrap().group = group.map(|(name, _)| name);
                    let index = self.free_window_index(sid)?;
                    let (_, wid, pane) = self.create_window(
                        sid,
                        window_name,
                        cols,
                        rows,
                        a.rest.clone(),
                        cwd,
                        false,
                        index,
                        self.pane_environment(sid, ctx, BTreeMap::new()),
                        &mut out,
                    );
                    self.panes
                        .get_mut(&pane)
                        .unwrap()
                        .terminal
                        .set_cell_size(cell_width, cell_height);
                    (wid, pane)
                };
                if !a.has(b'd') || ctx.session.is_none() {
                    ctx.set_session(Some(sid));
                }
                if !a.has(b'd') {
                    ctx.attached = true;
                    self.session_attached(sid);
                }
                out.notifications.push(b"%sessions-changed".to_vec().into());
                if ctx.control && !a.has(b'd') {
                    out.notifications.push(
                        format!("%session-changed ${sid} {session_name}")
                            .into_bytes()
                            .into(),
                    );
                }
                if a.has(b'P') {
                    line(
                        &mut out.output,
                        self.expand(
                            a.get(b'F').unwrap_or(b"#{session_name}:"),
                            Some(sid),
                            Some(wid),
                            Some(pane),
                            ctx,
                        )?,
                    );
                }
                out.changed = true;
            }
            "has-session" | "has" => {
                let a = Args::parse(args, "t:")?;
                a.no_rest()?;
                self.session_id(a.string(b't').as_deref(), ctx)?;
            }
            "attach-session" | "attach" => {
                out = self.attach_client(args, ctx)?;
            }
            "detach-client" | "detach" => {
                Args::parse(args, "")?.no_rest()?;
                ctx.attached = false;
                out.detach = true;
            }
            "switch-client" | "switchc" => {
                out = self.switch_client(args, ctx)?;
            }
            "new-window" | "neww" => {
                out = self.new_window(args, ctx)?;
            }
            "swap-window" | "swapw" => {
                out = self.swap_window(args, ctx)?;
            }
            "swap-pane" | "swapp" => {
                out = self.swap_pane(args, ctx)?;
            }
            "rotate-window" | "rotatew" => {
                out = self.rotate_window(args, ctx)?;
            }
            "join-pane" | "joinp" | "move-pane" | "movep" => {
                out = self.join_pane(&name, args, ctx)?;
            }
            "new-pane" | "newp" => {
                out = self.create_pane_command(&name, args, ctx)?;
            }
            "break-pane" | "breakp" => {
                out = self.break_pane(args, ctx)?;
            }
            "split-window" | "splitw" => {
                out = self.create_pane_command(&name, args, ctx)?;
            }
            "list-clients" | "lsc" => return self.list_clients(args, ctx),
            "list-sessions" | "ls" => {
                let a = Args::parse(args, "F:f:")?;
                a.no_rest()?;
                let context = ClientContext {
                    format_type: Some(b'S'),
                    ..ctx.clone()
                };
                let ctx = &context;
                for s in self.sessions_by_name() {
                    if let Some(filter) = a.get(b'f')
                        && !tmuxx_format::truthy(&self.expand(
                            filter,
                            Some(s.id),
                            s.windows.get(&s.active).copied(),
                            None,
                            ctx,
                        )?)
                    {
                        continue;
                    }
                    line(
                        &mut out.output,
                        self.expand(
                            a.get(b'F')
                                .unwrap_or(b"#{session_name}: #{session_windows} windows"),
                            Some(s.id),
                            s.windows.get(&s.active).copied(),
                            None,
                            ctx,
                        )?,
                    );
                }
            }
            "resize-window" | "resizew" => {
                let a = Args::parse(args, "aADLRUx:y:t:")?;
                if a.rest.len() > 1 {
                    return Err("too many arguments".into());
                }
                let (sid, _, wid) = self.window_target(a.string(b't').as_deref(), ctx)?;
                let amount = a
                    .rest
                    .first()
                    .map(|v| text(v).parse::<u16>().map_err(|_| "invalid adjustment"))
                    .transpose()?
                    .unwrap_or(1);
                let window = &self.windows[&wid];
                let mut cols = a.number(b'x', window.cols)?;
                let mut rows = a.number(b'y', window.rows)?;
                if a.has(b'L') {
                    cols = cols.saturating_sub(amount).max(1);
                }
                if a.has(b'R') {
                    cols = cols.saturating_add(amount);
                }
                if a.has(b'U') {
                    rows = rows.saturating_sub(amount).max(1);
                }
                if a.has(b'D') {
                    rows = rows.saturating_add(amount);
                }
                if a.has(b'A') || a.has(b'a') {
                    (cols, rows) = self.automatic_window_size(sid, wid, ctx, a.has(b'A'));
                }
                self.windows.get_mut(&wid).unwrap().manual_size = (cols, rows);
                self.windows
                    .get_mut(&wid)
                    .unwrap()
                    .options
                    .insert("window-size".into(), "manual".into());
                let (cols, rows) = self.client_size_limit(wid, ctx, (cols, rows));
                out = self.resize_window(wid, cols, rows)?;
            }
            "resize-pane" | "resizep" => return self.resize_pane(args, ctx),
            "select-layout" | "selectl" | "next-layout" | "nextl" | "previous-layout" | "prevl" => {
                return self.select_layout(&name, args, ctx);
            }
            "list-windows" | "lsw" => {
                let a = Args::parse(args, "aF:f:t:")?;
                a.no_rest()?;
                let sid = if a.has(b'a') {
                    None
                } else {
                    Some(self.session_id(a.string(b't').as_deref(), ctx)?)
                };
                for s in self
                    .sessions_by_name()
                    .into_iter()
                    .filter(|s| sid.is_none_or(|id| id == s.id))
                {
                    for (index, wid) in &s.windows {
                        let context = self.winlink_context(ctx, s.id, *index);
                        let ctx = &context;
                        if let Some(filter) = a.get(b'f')
                            && !tmuxx_format::truthy(&self.expand(
                                filter,
                                Some(s.id),
                                Some(*wid),
                                None,
                                ctx,
                            )?)
                        {
                            continue;
                        }
                        line(&mut out.output, self.expand(a.get(b'F').unwrap_or(b"#{window_index}: #{window_name} (#{window_panes} panes) [#{window_width}x#{window_height}]"), Some(s.id), Some(*wid), None, ctx)?);
                    }
                }
            }
            "list-panes" | "lsp" => {
                let a = Args::parse(args, "asF:f:t:")?;
                a.no_rest()?;
                let target = a.string(b't');
                let windows: Vec<(Id, u32, Id)> = if a.has(b'a') {
                    self.sessions_by_name()
                        .into_iter()
                        .flat_map(|s| s.windows.iter().map(|(index, w)| (s.id, *index, *w)))
                        .collect()
                } else if a.has(b's') {
                    let s = &self.sessions[&self.session_id(target.as_deref(), ctx)?];
                    s.windows
                        .iter()
                        .map(|(index, w)| (s.id, *index, *w))
                        .collect()
                } else {
                    let (s, index, w) = self.window_target(target.as_deref(), ctx)?;
                    vec![(s, index, w)]
                };
                for (sid, index, wid) in windows {
                    let context = self.winlink_context(ctx, sid, index);
                    let ctx = &context;
                    for pid in &self.windows[&wid].panes {
                        if let Some(filter) = a.get(b'f')
                            && !tmuxx_format::truthy(&self.expand(
                                filter,
                                Some(sid),
                                Some(wid),
                                Some(*pid),
                                ctx,
                            )?)
                        {
                            continue;
                        }
                        line(
                            &mut out.output,
                            self.expand(
                                a.get(b'F').unwrap_or(
                                    b"#{pane_index}: [#{pane_width}x#{pane_height}] #{pane_id}",
                                ),
                                Some(sid),
                                Some(wid),
                                Some(*pid),
                                ctx,
                            )?,
                        );
                    }
                }
            }
            "display-message" | "display" => {
                let origin = ctx.client_name.clone();
                let a = Args::parse(args, "aCc:d:jlINpt:F:v")?;
                a.reject(b"Cd")?;
                if a.has(b'I') && !a.has(b'j') {
                    let pane = self.pane_target(a.string(b't').as_deref(), ctx)?;
                    if !self.panes[&pane].empty {
                        return Err("pane is not empty".into());
                    }
                    out.effects.push(Effect::FeedStdin { pane });
                    return Ok(out);
                }
                if a.rest.len() > 1 {
                    return Err("too many arguments".into());
                }
                if a.has(b'F') && !a.rest.is_empty() {
                    return Err("only one of -F or argument must be given".into());
                }
                let target = a.string(b't');
                let selected = self.display_context(target.as_deref(), ctx);
                let (sid, wid, pid) = (
                    selected.map(|x| x.0),
                    selected.map(|x| x.1),
                    selected.map(|x| x.2),
                );
                let format_client = self.display_client(a.string(b'c').as_deref(), sid, ctx)?;
                let context = self
                    .window_target(target.as_deref(), ctx)
                    .ok()
                    .filter(|(s, _, w)| Some(*s) == sid && Some(*w) == wid)
                    .map(|(sid, index, _)| self.winlink_context(&format_client, sid, index));
                let ctx = context.as_ref().unwrap_or(&format_client);
                if a.has(b'a') && !a.has(b'j') {
                    for key in format_metadata::FORMAT_KEYS {
                        if let Some(value) = self.enumerated_format(key, sid, wid, pid, ctx) {
                            out.output.extend(key.as_bytes());
                            out.output.push(b'=');
                            out.output.extend(value);
                            out.output.push(b'\n');
                        }
                    }
                    if !ctx.formats.contains_key(b"command".as_slice()) {
                        out.output.extend(b"command=display-message\n");
                    }
                    for (key, value) in &ctx.formats {
                        out.output.extend(key);
                        out.output.push(b'=');
                        out.output.extend(value);
                        out.output.push(b'\n');
                    }
                    return Ok(out);
                }
                let format = a
                    .get(b'F')
                    .or_else(|| a.rest.first().map(Vec::as_slice))
                    .unwrap_or(if a.has(b'j') { b"" } else {
                    b"[#{session_name}] #{window_index}:#{window_name}, current pane #{pane_index}"
                });
                let data = if a.has(b'l') {
                    format.to_vec()
                } else {
                    self.expand_depth(format, (sid, wid, pid), ctx, 0, true)?
                };
                let data = if a.has(b'j') {
                    tmuxx_command::json::canonicalize(&data)?
                } else {
                    data
                };
                if a.has(b'v') {
                    for (label, bytes) in [
                        (b"input".as_slice(), format),
                        (b"result".as_slice(), data.as_slice()),
                    ] {
                        out.output.extend(b"tmuxx format ");
                        out.output.extend(label);
                        out.output.extend(b": ");
                        out.output.extend(tmuxx_command::display_argument(
                            &bytes[..bytes.len().min(4096)],
                        ));
                        if bytes.len() > 4096 {
                            out.output.extend(b" [truncated]");
                        }
                        out.output.push(b'\n');
                    }
                }
                if a.has(b'p') {
                    out.output.extend(data);
                    out.output.push(b'\n');
                } else if ctx.has_format_client() && ctx.control {
                    let message = [b"%message ".as_slice(), &data].concat();
                    if ctx.client_name == origin {
                        out.output.extend(message);
                        out.output.push(b'\n');
                    } else {
                        out.client_notifications
                            .push((ctx.client_name.clone(), message));
                    }
                }
            }
            "clear-history" | "clearhist" => {
                let a = Args::parse(args, "Ht:")?;
                a.no_rest()?;
                let pid = self.pane_target(a.string(b't').as_deref(), ctx)?;
                self.panes
                    .get_mut(&pid)
                    .unwrap()
                    .terminal
                    .clear_history(a.has(b'H'));
                out.changed = true;
            }
            "capture-pane" | "capturep" => {
                let a = Args::parse(args, "ab:CeE:FHIJLMNpPqRS:Tt:")?;
                a.no_rest()?;
                let pid = self.pane_target(a.string(b't').as_deref(), ctx)?;
                let bound = |flag, fallback| -> Option<i32> {
                    a.get(flag).and_then(|raw| {
                        let value = self
                            .expand(
                                raw,
                                ctx.session,
                                Some(self.panes[&pid].window),
                                Some(pid),
                                ctx,
                            )
                            .ok()?;
                        if value == b"-" {
                            Some(fallback)
                        } else {
                            text(&value)
                                .parse::<i32>()
                                .ok()
                                .filter(|value| *value <= i16::MAX as i32)
                        }
                    })
                };
                let mut data =
                    self.panes[&pid]
                        .terminal
                        .capture_with(tmuxx_terminal::CaptureOptions {
                            raw: a.has(b'R'),
                            start: bound(b'S', i32::MIN),
                            end: bound(b'E', i32::MAX),
                            join: a.has(b'J'),
                            escape: a.has(b'e'),
                            escape_sequences: a.has(b'C'),
                            alternate: a.has(b'a'),
                            pending: a.has(b'P'),
                            preserve_trailing: a.has(b'N'),
                            trim_unused: a.has(b'T'),
                            quiet: a.has(b'q'),
                            number_lines: a.has(b'L'),
                            hyperlinks: a.has(b'H'),
                            timestamps: a.has(b'I'),
                            flags: a.has(b'F'),
                        })?;
                if a.has(b'p') {
                    if data.last() != Some(&b'\n') {
                        data.push(b'\n');
                    }
                    out.output = data;
                } else if let Some((name, deleted)) =
                    self.store_buffer(a.get(b'b').map(<[u8]>::to_vec), data)?
                {
                    out.notifications.extend(
                        deleted.into_iter().map(|name| {
                            format!("%paste-buffer-deleted {name}").into_bytes().into()
                        }),
                    );
                    out.notifications
                        .push(format!("%paste-buffer-changed {name}").into_bytes().into());
                    out.changed = true;
                }
            }
            "send-keys" | "send" => {
                let a = Args::parse(args, "FHKlMRXc:N:t:")?;
                a.reject(b"Kc")?;
                let pid = self.pane_target(a.string(b't').as_deref(), ctx)?;
                if a.has(b'X') || a.has(b'M') {
                    return Err("unsupported copy-mode and mouse command injection".into());
                }
                let repeat = a
                    .get(b'N')
                    .map(|n| text(n).parse::<usize>().map_err(|_| "invalid repeat count"))
                    .transpose()?
                    .unwrap_or(1);
                if a.has(b'R') {
                    self.panes.get_mut(&pid).unwrap().terminal.reset_from_host();
                    out.changed = true;
                }
                let panes = self.input_targets(pid);
                let mut total = 0usize;
                for pane in panes {
                    let mut data = Vec::new();
                    for key in &a.rest {
                        if a.has(b'H') {
                            data.push(
                                u8::from_str_radix(&text(key), 16)
                                    .map_err(|_| format!("invalid hex byte: {}", text(key)))?,
                            );
                        } else if a.has(b'l') {
                            data.extend_from_slice(key);
                        } else {
                            data.extend_from_slice(&self.encode_named_key(pane, key));
                        }
                    }
                    let size = data.len().checked_mul(repeat).ok_or("input too large")?;
                    total = total.checked_add(size).ok_or("input too large")?;
                    if total > 16 * 1024 * 1024 {
                        return Err("input exceeds 16 MiB command limit".into());
                    }
                    out.effects.push(Effect::Write {
                        pane,
                        data: data.repeat(repeat),
                    });
                }
            }
            "send-prefix" => {
                let a = Args::parse(args, "2t:")?;
                a.no_rest()?;
                let pid = self.pane_target(a.string(b't').as_deref(), ctx)?;
                let (sid, _, _) = self.window_target(Some(&format!("%{pid}")), ctx)?;
                let key = self
                    .session_option(sid, if a.has(b'2') { "prefix2" } else { "prefix" })
                    .unwrap_or("None");
                if let Some(event) = parse_key(key.as_bytes()) {
                    for pane in self.input_targets(pid) {
                        out.effects.push(Effect::Write {
                            pane,
                            data: self.encode_pane_key(
                                pane,
                                &event,
                                tmuxx_terminal::KeyEncodeOptions {
                                    macos_option_as_alt: true,
                                },
                            ),
                        });
                    }
                }
            }
            "select-pane" | "selectp" | "last-pane" | "lastp" => {
                out = self.select_pane(&name, args, ctx)?;
            }
            "select-window" | "selectw" | "next-window" | "next" | "previous-window" | "prev"
            | "last-window" | "last" => {
                out = self.select_window(&name, args, ctx)?;
            }
            "rename-session" | "rename" => {
                let a = Args::parse(args, "t:")?;
                if a.rest.len() != 1 {
                    return Err("usage: rename-session [-t target-session] new-name".into());
                }
                let sid = self.session_id(a.string(b't').as_deref(), ctx)?;
                let name = self.expand(&a.rest[0], Some(sid), None, None, ctx)?;
                let name = clean_name(
                    std::str::from_utf8(&name).map_err(|_| "invalid session name")?,
                    false,
                );
                if self
                    .sessions
                    .values()
                    .any(|s| s.id != sid && s.name == name)
                {
                    return Err(format!("duplicate session: {name}"));
                }
                self.sessions.get_mut(&sid).unwrap().name = name.clone();
                out.notifications.push(
                    format!("%session-renamed ${sid} {name}")
                        .into_bytes()
                        .into(),
                );
                out.changed = true;
            }
            "rename-window" | "renamew" => {
                let a = Args::parse(args, "t:")?;
                if a.rest.len() != 1 {
                    return Err("usage: rename-window [-t target-window] new-name".into());
                }
                let (sid, _, wid) = self.window_target(a.string(b't').as_deref(), ctx)?;
                let name = self.expand(&a.rest[0], Some(sid), Some(wid), None, ctx)?;
                let name = clean_name(
                    std::str::from_utf8(&name).map_err(|_| "invalid window name")?,
                    false,
                );
                self.windows.get_mut(&wid).unwrap().name = name.clone();
                out.notifications.push(self.window_renamed(wid));
                self.windows
                    .get_mut(&wid)
                    .unwrap()
                    .options
                    .insert("automatic-rename".into(), "off".into());
                out.changed = true;
            }
            "link-window" | "linkw" | "move-window" | "movew" => {
                out = self.link_window(&name, args, ctx)?;
            }
            "unlink-window" | "unlinkw" => {
                out = self.unlink_window(args, ctx)?;
            }
            "respawn-pane" | "respawnp" => {
                out = self.respawn(args, ctx, false)?;
            }
            "respawn-window" | "respawnw" => {
                out = self.respawn(args, ctx, true)?;
            }
            "pipe-pane" | "pipep" => {
                out = self.pipe_pane(args, ctx)?;
            }
            "kill-pane" | "killp" => {
                let a = Args::parse(args, "at:")?;
                a.no_rest()?;
                let pid = self.pane_target(a.string(b't').as_deref(), ctx)?;
                if a.has(b'a') {
                    for other in self.windows[&self.panes[&pid].window].panes.clone() {
                        if other != pid {
                            self.remove_pane(other, &mut out);
                        }
                    }
                } else {
                    self.remove_pane(pid, &mut out);
                }
                out.changed = true;
            }
            "kill-window" | "killw" => {
                let a = Args::parse(args, "af:t:")?;
                a.no_rest()?;
                let (sid, _, wid) = self.window_target(a.string(b't').as_deref(), ctx)?;
                if a.has(b'f') && !a.has(b'a') {
                    return Err("-f only valid with -a".into());
                }
                if a.has(b'a') {
                    self.kill_other_windows(sid, wid, a.get(b'f'), ctx, &mut out)?;
                } else {
                    self.remove_window(wid, &mut out);
                }
                out.changed = true;
            }
            "kill-session" => {
                out = self.kill_session(args, ctx)?;
            }
            "kill-server" => {
                Args::parse(args, "")?.no_rest()?;
                out.effects.push(Effect::Shutdown);
                out.changed = true;
            }
            "start-server" | "start" => {
                Args::parse(args, "")?.no_rest()?;
            }
            "set-option" | "set" | "set-window-option" | "setw" => {
                out = self.set_option(&name, args, ctx)?;
            }
            "show-options" | "show" | "show-window-options" | "showw" => {
                out = self.show_options(&name, args, ctx)?;
            }
            "set-environment" | "setenv" => {
                let a = Args::parse(args, "Fghrut:")?;
                if a.rest.is_empty() || a.rest.len() > 2 {
                    return Err("usage: set-environment name [value]".into());
                }
                let key = String::from_utf8(a.rest[0].clone())
                    .map_err(|_| "non-UTF8 environment names are unsupported")?;
                if key.is_empty() {
                    return Err("empty variable name".into());
                }
                if key.contains('=') {
                    return Err("variable name contains =".into());
                }
                if key.contains('\0') {
                    return Err("invalid environment variable".into());
                }
                let sid = if a.has(b'g') {
                    None
                } else {
                    let target = a.string(b't');
                    Some(self.session_id(target.as_deref(), ctx).map_err(|_| {
                        target.map_or_else(
                            || "no current session".into(),
                            |target| format!("no such session: {target}"),
                        )
                    })?)
                };
                let mut value = a
                    .rest
                    .get(1)
                    .map(|value| {
                        String::from_utf8(value.clone())
                            .map_err(|_| "non-UTF8 environment values are unsupported")
                    })
                    .transpose()?;
                if a.has(b'u') || a.has(b'r') {
                    if value.is_some() {
                        return Err(format!(
                            "can't specify a value with -{}",
                            if a.has(b'u') { 'u' } else { 'r' }
                        ));
                    }
                } else if value.is_none() {
                    return Err("no value specified".into());
                }
                if a.has(b'F')
                    && let Some(raw) = &value
                {
                    let target = self.session_id(a.string(b't').as_deref(), ctx).ok();
                    value = Some(admission::text(
                        &self.expand(raw.as_bytes(), target, None, None, ctx)?,
                        "environment value",
                    )?);
                }
                if value.as_ref().is_some_and(|value| value.contains('\0')) {
                    return Err("environment value contains NUL".into());
                }
                let (map, hidden, removed) = if let Some(id) = sid {
                    let session = self.sessions.get_mut(&id).unwrap();
                    (
                        &mut session.environment,
                        &mut session.hidden_environment,
                        &mut session.removed_environment,
                    )
                } else {
                    (
                        &mut self.environment,
                        &mut self.hidden_environment,
                        &mut self.removed_environment,
                    )
                };
                hidden.remove(&key);
                removed.remove(&key);
                if a.has(b'u') || a.has(b'r') {
                    map.remove(&key);
                    if a.has(b'r') && !a.has(b'u') {
                        removed.insert(key);
                    }
                } else {
                    if a.has(b'h') {
                        hidden.insert(key.clone());
                    }
                    map.insert(key, value.unwrap());
                }
                out.changed = true;
            }
            "show-environment" | "showenv" => {
                let a = Args::parse(args, "ghst:")?;
                if a.rest.len() > 1 {
                    return Err("too many arguments".into());
                }
                let (map, hidden, removed) = if a.has(b'g') {
                    (
                        &self.environment,
                        &self.hidden_environment,
                        &self.removed_environment,
                    )
                } else {
                    let target = a.string(b't');
                    let sid = self.session_id(target.as_deref(), ctx).map_err(|_| {
                        target.map_or_else(
                            || "no current session".into(),
                            |target| format!("no such session: {target}"),
                        )
                    })?;
                    let session = &self.sessions[&sid];
                    (
                        &session.environment,
                        &session.hidden_environment,
                        &session.removed_environment,
                    )
                };
                let keys = if let Some(key) = a.rest.first() {
                    let key = admission::text(key, "environment name")?;
                    if !map.contains_key(&key) && !removed.contains(&key) {
                        return Err(format!("unknown variable: {key}"));
                    }
                    vec![key]
                } else {
                    map.keys()
                        .chain(removed)
                        .cloned()
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect()
                };
                for key in keys {
                    if hidden.contains(&key) != a.has(b'h') {
                        continue;
                    }
                    let value = map.get(&key);
                    let result = match (a.has(b's'), value) {
                        (false, Some(value)) => format!("{key}={value}"),
                        (false, None) => format!("-{key}"),
                        (true, None) => format!("unset {key};"),
                        (true, Some(value)) => {
                            let escaped: String = value
                                .chars()
                                .flat_map(|ch| {
                                    if ['\\', '\"', '$', '`'].contains(&ch) {
                                        vec!['\\', ch]
                                    } else {
                                        vec![ch]
                                    }
                                })
                                .collect();
                            format!("{key}=\"{escaped}\"; export {key};")
                        }
                    };
                    line(&mut out.output, result);
                }
            }
            "server-access" => {
                let a = Args::parse(args, "adglrw")?;
                if a.has(b'l') {
                    if self.identity.get("uid").is_some_and(|uid| uid != "0") {
                        line(
                            &mut out.output,
                            format!(
                                "{} (U,W)",
                                self.identity
                                    .get("user")
                                    .map(String::as_str)
                                    .unwrap_or("unknown")
                            ),
                        );
                    }
                } else if a.rest.is_empty() {
                    return Err("missing user or group argument".into());
                } else {
                    return Err(
                        "server-access changes are unsupported: server is same-user-only".into(),
                    );
                }
            }
            "set-buffer" | "setb" => {
                let a = Args::parse(args, "ab:t:n:w")?;
                if a.rest.len() > 1 {
                    return Err("too many arguments".into());
                }
                if let Some(raw_name) = a.get(b'n') {
                    let old = self.buffer_name(a.get(b'b')).map_err(|error| {
                        if error == "no buffers" {
                            "no buffer".into()
                        } else {
                            error.replacen("no buffer ", "unknown buffer: ", 1)
                        }
                    })?;
                    if raw_name.is_empty() {
                        return Err("new name is empty".into());
                    }
                    let new_name = buffers::creation_name(raw_name)?;
                    if old == new_name {
                        return Ok(out);
                    }
                    let data = self.buffers.remove(&old).unwrap();
                    if let Some(created) = self.buffer_created.remove(&old) {
                        self.buffer_created.insert(new_name.clone(), created);
                    }
                    self.buffer_order.retain(|name| *name != new_name);
                    for name in &mut self.buffer_order {
                        if *name == old {
                            *name = new_name.clone();
                        }
                    }
                    self.automatic_buffers.remove(&old);
                    self.automatic_buffers.remove(&new_name);
                    if self.buffers.insert(new_name.clone(), data).is_some() {
                        out.notifications.push(
                            format!("%paste-buffer-deleted {new_name}")
                                .into_bytes()
                                .into(),
                        );
                    }
                    out.notifications
                        .push(format!("%paste-buffer-deleted {old}").into_bytes().into());
                    out.notifications.push(
                        format!("%paste-buffer-changed {new_name}")
                            .into_bytes()
                            .into(),
                    );
                    out.changed = true;
                    self.sequence += 1;
                    return Ok(out);
                }
                let mut data = a.rest.first().cloned().ok_or("no data specified")?;
                if data.is_empty() {
                    return Ok(out);
                }
                if a.string(b'b').is_some_and(|name| name.is_empty()) {
                    return Err("empty buffer name".into());
                }
                if a.has(b'a')
                    && a.has(b'b')
                    && let Ok(key) = self.buffer_name(a.get(b'b'))
                {
                    let mut previous = self.buffers[&key].clone();
                    previous.extend(data);
                    data = previous;
                }
                if a.has(b'w')
                    && let Some((client, pane)) =
                        self.clipboard_target(a.string(b't').as_deref(), ctx)
                {
                    out.effects.push(Effect::ClientClipboard {
                        client,
                        pane,
                        data: data.clone(),
                    });
                }
                let (key, deleted) = self
                    .store_buffer(a.get(b'b').map(<[u8]>::to_vec), data)?
                    .unwrap();
                out.notifications.extend(
                    deleted
                        .into_iter()
                        .map(|name| format!("%paste-buffer-deleted {name}").into_bytes().into()),
                );
                out.notifications
                    .push(format!("%paste-buffer-changed {key}").into_bytes().into());
                out.changed = true;
            }
            "show-buffer" | "showb" => {
                let a = Args::parse(args, "b:")?;
                let key = self.buffer_name(a.get(b'b'))?;
                out.output = self.buffers[&key].clone();
            }
            "delete-buffer" | "deleteb" => {
                let a = Args::parse(args, "b:")?;
                let key = self.buffer_name(a.get(b'b')).map_err(|error| {
                    if error == "no buffers" {
                        "no buffer".into()
                    } else {
                        error.replacen("no buffer ", "unknown buffer: ", 1)
                    }
                })?;
                self.buffers.remove(&key);
                self.buffer_created.remove(&key);
                self.buffer_order.retain(|name| *name != key);
                out.notifications
                    .push(format!("%paste-buffer-deleted {key}").into_bytes().into());
                out.changed = true;
            }
            "paste-buffer" | "pasteb" => {
                let a = Args::parse(args, "dprSb:s:t:")?;
                a.no_rest()?;
                let pane = self.pane_target(a.string(b't').as_deref(), ctx)?;
                if self.panes[&pane].status.is_some() {
                    return Err("target pane has exited".into());
                }
                if a.string(b'b').is_none() && self.top_buffer_name().is_none() {
                    return Ok(out);
                }
                let key = self.buffer_name(a.get(b'b'))?;
                let mut data = Vec::new();
                let bracketed = a.has(b'p') && self.panes[&pane].terminal.mode(true, 2004);
                if bracketed {
                    data.extend(b"\x1b[200~");
                }
                let separator = a
                    .get(b's')
                    .unwrap_or(if a.has(b'r') { b"\n" } else { b"\r" });
                data.extend(buffers::paste(&self.buffers[&key], separator, a.has(b'S')));
                if bracketed {
                    data.extend(b"\x1b[201~");
                }
                if !self.panes[&pane].input_disabled {
                    out.effects.push(Effect::Write { pane, data });
                }
                if a.has(b'd') {
                    self.buffers.remove(&key);
                    self.buffer_created.remove(&key);
                    self.buffer_order.retain(|name| *name != key);
                    out.notifications
                        .push(format!("%paste-buffer-deleted {key}").into_bytes().into());
                }
                out.changed = true;
            }
            "load-buffer" | "loadb" => {
                let a = Args::parse(args, "b:t:w")?;
                if a.rest.len() != 1 {
                    return Err("usage: load-buffer [-b buffer-name] path".into());
                }
                out.effects.push(Effect::LoadBuffer {
                    name: a.get(b'b').map(<[u8]>::to_vec),
                    path: self.expand(&a.rest[0], ctx.session, None, None, ctx)?,
                    clipboard: a
                        .has(b'w')
                        .then(|| self.clipboard_target(a.string(b't').as_deref(), ctx))
                        .flatten(),
                });
                out.changed = true;
            }
            "save-buffer" | "saveb" => {
                let a = Args::parse(args, "ab:")?;
                if a.rest.len() != 1 {
                    return Err("usage: save-buffer [-a] [-b buffer-name] path".into());
                }
                let name = self.buffer_name(a.get(b'b'))?;
                let path = self.expand(&a.rest[0], ctx.session, None, None, ctx)?;
                if path == b"-" {
                    out.output = self.buffers[&name].clone();
                } else {
                    out.effects.push(Effect::SaveBuffer {
                        path,
                        data: self.buffers[&name].clone(),
                        append: a.has(b'a'),
                    });
                }
            }
            "list-buffers" | "lsb" => {
                let a = Args::parse(args, "F:f:")?;
                a.no_rest()?;
                for name in self.buffer_order.iter().rev() {
                    if !self.buffers.contains_key(name) {
                        continue;
                    }
                    let lookup = |key: &[u8]| {
                        self.buffer_format(key, Some(name)).or_else(|| {
                            let key = std::str::from_utf8(key).ok()?;
                            self.format_value(key, None, None, None, ctx)
                                .map(String::into_bytes)
                        })
                    };
                    if let Some(filter) = a.get(b'f')
                        && !tmuxx_format::truthy(
                            &tmuxx_format::expand(filter, lookup).map_err(|e| e.to_string())?,
                        )
                    {
                        continue;
                    }
                    line(
                        &mut out.output,
                        tmuxx_format::expand(
                            a.get(b'F').unwrap_or(
                                b"#{buffer_name}: #{buffer_size} bytes: \"#{buffer_sample}\"",
                            ),
                            lookup,
                        )
                        .map_err(|e| e.to_string())?,
                    );
                }
            }
            "refresh-client" | "refresh" => {
                let a = Args::parse(args, "cDlLRSUAB:C:f:r:t:")?;
                a.reject(b"cDlLRSUABfrt")?;
                a.no_rest()?;
                if let Some(size) = a.string(b'C') {
                    let (x, y) = size.split_once(',').ok_or("invalid client size")?;
                    ctx.cols = x.parse().map_err(|_| "invalid client width")?;
                    ctx.rows = y.parse().map_err(|_| "invalid client height")?;
                    if ctx.cols == 0 || ctx.rows == 0 {
                        return Err("invalid client size".into());
                    }
                    ctx.size_requested = true;
                    ctx.resize_order = self.sequence + 1;
                    out.changed = true;
                }
            }
            "list-commands" | "lscm" => {
                let args = Args::parse(args, "F:h")?;
                let entries = if let Some(name) = args.rest.first() {
                    vec![tmuxx_command::registry::find(name)?]
                } else {
                    tmuxx_command::registry::COMMANDS.iter().collect()
                };
                let width = entries
                    .iter()
                    .map(|entry| entry.name.len())
                    .max()
                    .unwrap_or(0);
                let help = args.repeated.get(&b'h').map_or(0, Vec::len);
                let default = "#{command_list_name}#{?command_list_alias, (#{command_list_alias}),} #{command_list_usage}";
                let verbose = format!("{default}\n    #{{command_list_description}}");
                let template = args.get(b'F').unwrap_or({
                    if help > 1 {
                        verbose.as_bytes()
                    } else if help == 1 {
                        b"#{p|#{command_list_name_width}:command_list_name} #{command_list_description}"
                    } else {
                        default.as_bytes()
                    }
                });
                let mut context = ctx.clone();
                context.format_client = Some(false);
                context.format_type = Some(0);
                context.formats.insert(
                    b"command_list_name_width".to_vec(),
                    width.to_string().into_bytes(),
                );
                for entry in entries {
                    for (key, value) in [
                        ("name", entry.name),
                        ("alias", entry.alias),
                        ("usage", entry.usage),
                        ("description", entry.description),
                    ] {
                        context.formats.insert(
                            format!("command_list_{key}").into_bytes(),
                            value.as_bytes().to_vec(),
                        );
                    }
                    let value = self.expand(template, None, None, None, &context)?;
                    if !value.is_empty() {
                        out.output.extend(value);
                        out.output.push(b'\n');
                    }
                }
            }
            _ => return Err(format!("unsupported command: {name}")),
        }
        if out.changed {
            self.sequence = self.sequence.wrapping_add(1);
        }
        Ok(out)
    }

    fn buffer_name(&self, name: Option<&[u8]>) -> Result<String, String> {
        let name = name
            .or_else(|| self.top_buffer_name().map(str::as_bytes))
            .ok_or("no buffers")?;
        let name =
            std::str::from_utf8(name).map_err(|_| format!("no buffer {}", name.escape_ascii()))?;
        if self.buffers.contains_key(name) {
            Ok(name.to_owned())
        } else {
            Err(format!("no buffer {name}"))
        }
    }

    fn top_buffer_name(&self) -> Option<&str> {
        self.buffer_order
            .iter()
            .rev()
            .find(|name| self.automatic_buffers.contains(*name) && self.buffers.contains_key(*name))
            .map(String::as_str)
    }

    pub fn store_buffer(
        &mut self,
        name: Option<Vec<u8>>,
        data: Vec<u8>,
    ) -> Result<Option<(String, Vec<String>)>, String> {
        if data.is_empty() {
            return Ok(None);
        }
        let name = name.as_deref().map(buffers::creation_name).transpose()?;
        let mut deleted = Vec::new();
        let automatic = name.is_none();
        let name = name.unwrap_or_else(|| {
            loop {
                let name = format!("buffer{}", self.next_buffer);
                self.next_buffer += 1;
                if !self.buffers.contains_key(&name) {
                    break name;
                }
            }
        });
        if automatic {
            self.automatic_buffers
                .retain(|name| self.buffers.contains_key(name));
            let limit = self
                .global_option("buffer-limit")
                .and_then(|v| v.parse().ok())
                .unwrap_or(50);
            while self.automatic_buffers.len() >= limit {
                let Some(oldest) = self
                    .buffer_order
                    .iter()
                    .find(|name| self.automatic_buffers.contains(*name))
                    .cloned()
                else {
                    break;
                };
                self.buffers.remove(&oldest);
                deleted.push(oldest.clone());
                self.buffer_created.remove(&oldest);
                self.automatic_buffers.remove(&oldest);
                self.buffer_order.retain(|name| name != &oldest);
            }
            self.automatic_buffers.insert(name.clone());
        } else {
            self.automatic_buffers.remove(&name);
        }
        if self.buffers.insert(name.clone(), data).is_some() {
            deleted.push(name.clone());
        }
        self.buffer_created
            .insert(name.clone(), self.now / 1_000_000);
        self.buffer_order.retain(|other| *other != name);
        self.buffer_order.push(name.clone());
        Ok(Some((name, deleted)))
    }

    fn remove_window(&mut self, wid: Id, out: &mut Outcome) {
        self.remove_window_with_renumber(wid, out, true);
    }

    fn window_unlinked(&self, window: Id) -> Notification {
        Notification::WindowUnlinked {
            window,
            linked_sessions: self
                .sessions
                .values()
                .filter(|session| session.windows.values().any(|id| *id == window))
                .map(|session| session.id)
                .collect(),
        }
    }
    fn window_linked(&self, window: Id) -> Notification {
        Notification::WindowLinked {
            window,
            linked_sessions: self
                .sessions
                .values()
                .filter(|session| session.windows.values().any(|id| *id == window))
                .map(|session| session.id)
                .collect(),
        }
    }
    fn window_renamed(&self, window: Id) -> Notification {
        Notification::WindowRenamed {
            window,
            name: self.windows[&window].name.clone(),
            linked_sessions: self
                .sessions
                .values()
                .filter(|session| session.windows.values().any(|id| *id == window))
                .map(|session| session.id)
                .collect(),
        }
    }

    fn remove_window_with_renumber(&mut self, wid: Id, out: &mut Outcome, renumber: bool) {
        if self.windows.get(&wid).is_some_and(|window| window.zoomed) {
            self.windows.get_mut(&wid).unwrap().zoomed = false;
            self.sync_layout(wid, out);
            out.notifications.push(self.layout_event(wid));
        }
        let mut affected = self
            .sessions
            .values()
            .filter(|s| s.windows.values().any(|id| *id == wid))
            .map(|s| (s.name.clone(), s.id, s.group.clone()))
            .collect::<Vec<_>>();
        affected.sort();
        let mut groups = BTreeSet::new();
        affected.retain(|(_, _, group)| {
            group
                .as_ref()
                .is_none_or(|group| groups.insert(group.clone()))
        });
        for (_, sid, _) in &affected {
            if !self.sessions.contains_key(sid) {
                continue;
            }
            let indices = self.sessions[sid]
                .windows
                .iter()
                .filter(|(_, id)| **id == wid)
                .map(|(index, _)| *index)
                .collect::<Vec<_>>();
            for index in indices {
                if self.sessions[sid].windows.len() == 1 {
                    self.remove_session_group(*sid, out);
                    break;
                }
                self.select_before_unlink(*sid, index, out);
                out.notifications.push(self.window_unlinked(wid));
                self.sessions.get_mut(sid).unwrap().windows.remove(&index);
                self.sync_group(*sid, out);
            }
        }
        if let Some(w) = self.windows.remove(&wid) {
            for p in w.panes {
                self.panes.remove(&p);
                out.effects.push(Effect::Kill { pane: p });
            }
        }
        for s in self.sessions.values_mut() {
            s.windows.retain(|_, w| *w != wid);
            s.links.retain(|index, _| s.windows.contains_key(index));
            s.repair_current();
        }
        self.sessions.retain(|_, s| !s.windows.is_empty());
        if renumber {
            for (_, sid, _) in affected {
                self.renumber_session(sid);
            }
        }
    }

    fn remove_session_group(&mut self, sid: Id, out: &mut Outcome) {
        let group = self.sessions[&sid].group.clone();
        let destroyed = self
            .sessions
            .values()
            .filter(|session| session.id == sid || group.is_some() && session.group == group)
            .map(|session| session.id)
            .collect::<Vec<_>>();
        for session in destroyed {
            self.remove_session(session, out);
        }
    }

    fn remove_session(&mut self, sid: Id, out: &mut Outcome) {
        if let Some(session) = self.sessions.get(&sid) {
            out.notifications.push(b"%sessions-changed".to_vec().into());
            for wid in session.windows.values() {
                out.notifications.push(self.window_unlinked(*wid));
            }
        }
        let Some(session) = self.sessions.remove(&sid) else {
            return;
        };
        for wid in session.windows.values() {
            if !self
                .sessions
                .values()
                .any(|s| s.windows.values().any(|w| w == wid))
            {
                self.remove_window(*wid, out);
            }
        }
    }

    fn remove_pane(&mut self, pid: Id, out: &mut Outcome) {
        let restore_zoom = if let Some(pane) = self.panes.get(&pid) {
            let wid = pane.window;
            let window = &self.windows[&wid];
            let restore = window
                .is_over_zoom(pid)
                .then(|| window.zoom_target())
                .flatten();
            if window.zoomed {
                self.windows.get_mut(&wid).unwrap().zoomed = false;
                self.sync_layout(wid, out);
                out.notifications.push(self.layout_event(wid));
            }
            restore
        } else {
            None
        };
        let Some(p) = self.panes.remove(&pid) else {
            return;
        };
        out.effects.push(Effect::Kill { pane: pid });
        let w = self.windows.get_mut(&p.window).unwrap();
        if w.modal == Some(pid) {
            w.modal = None;
        }
        if w.zoom_target() == Some(pid) {
            w.zoomed = false;
        }
        w.over_zoom.remove(&pid);
        if w.panes.len() == 1 {
            w.panes.clear();
            self.remove_window(p.window, out);
            return;
        }
        w.layout.remove(pid as u32);
        out.notifications.push(self.layout_event(p.window));
        let w = self.windows.get_mut(&p.window).unwrap();
        w.panes.retain(|p| *p != pid);
        w.last.retain(|id| *id != pid);
        if w.active == pid {
            w.active = w.last.first().copied().unwrap_or(w.panes[0]);
            w.last.retain(|id| *id != w.active);
            out.notifications.push(
                format!("%window-pane-changed @{} %{}", p.window, w.active)
                    .into_bytes()
                    .into(),
            );
        }
        self.sync_layout(p.window, out);
        if let Some(target) = restore_zoom.filter(|_| self.windows[&p.window].panes.len() > 1) {
            let w = self.windows.get_mut(&p.window).unwrap();
            w.zoomed = true;
            w.zoomed_pane = Some(if w.panes.contains(&target) {
                target
            } else {
                w.active
            });
            w.save_zoom_geometry();
            self.sync_layout(p.window, out);
            out.notifications.push(self.layout_event(p.window));
        }
    }

    pub fn set_persistent(&mut self, pane: Id, persistent: bool) -> Result<Outcome, String> {
        let state = self.panes.get_mut(&pane).ok_or("pane not found")?;
        state.persistent = persistent;
        self.sequence += 1;
        Ok(Outcome {
            changed: true,
            ..Outcome::default()
        })
    }

    pub fn close_pane(&mut self, pane: Id) -> Outcome {
        let mut out = Outcome::default();
        if self.panes.contains_key(&pane) {
            self.remove_pane(pane, &mut out);
            out.changed = true;
            self.sequence += 1;
            out.effects.extend(self.finish_window_selections(None));
            out.effects.extend(self.synchronize_terminal_options());
        }
        out
    }

    pub fn set_process(&mut self, pane: Id, pid: Option<u32>, tty: String) {
        if let Some(p) = self.panes.get_mut(&pane) {
            p.pid = pid;
            p.tty = tty;
        }
    }

    pub fn feed(&mut self, pane: Id, bytes: &[u8]) -> Vec<Effect> {
        if bytes.is_empty() {
            return Vec::new();
        }
        self.synchronize_images();
        let Some(p) = self.panes.get_mut(&pane) else {
            return Vec::new();
        };
        self.sequence = self.sequence.wrapping_add(1);
        self.now = self.now.saturating_add(1);
        p.output_generation = p.output_generation.wrapping_add(1);
        self.windows.get_mut(&p.window).unwrap().activity = self.now;
        let wid = p.window;
        let effects = p.terminal.feed_at(bytes, self.now / 1_000_000);
        let bell = effects
            .iter()
            .any(|effect| matches!(effect, tmuxx_terminal::Effect::Bell));
        self.synchronize_images();
        let mut result = self.terminal_effects(pane, effects);
        result.extend(self.window_alerts(wid, 1 | if bell { 2 } else { 0 }, None));
        result
    }

    pub fn exited(&mut self, pane: Id, status: i32) -> Outcome {
        self.exited_with_signal(pane, status, None)
    }

    /// Record child status before the PTY is drained; descendants may still use it.
    pub fn record_exit(&mut self, pane: Id, status: i32, signal: Option<String>) {
        if let Some(pane) = self.panes.get_mut(&pane) {
            pane.dead_status = signal.is_none().then_some(status);
            pane.dead_signal = signal;
        }
    }

    pub fn exited_with_signal(&mut self, pane: Id, status: i32, signal: Option<String>) -> Outcome {
        self.record_exit(pane, status, signal);
        let mut out = Outcome::default();
        if let Some(p) = self.panes.get_mut(&pane) {
            p.status = Some(status);
            p.terminal.clear_agent_status();
            let remain = p
                .options
                .get("remain-on-exit")
                .or_else(|| self.windows[&p.window].options.get("remain-on-exit"))
                .or_else(|| self.global_window_options.get("remain-on-exit"))
                .is_some_and(|v| {
                    matches!(v.as_str(), "on" | "1" | "key")
                        || (matches!(v.as_str(), "failed" | "failed-key") && status != 0)
                });
            if !remain {
                self.remove_pane(pane, &mut out);
            } else {
                p.dead_time = Some(self.now);
                p.terminal.hide_cursor();
            }
            out.changed = true;
            self.sequence = self.sequence.wrapping_add(1);
            out.effects.extend(self.finish_window_selections(None));
            out.effects.extend(self.synchronize_terminal_options());
        }
        out
    }

    pub fn expand(
        &self,
        format: &[u8],
        sid: Option<Id>,
        wid: Option<Id>,
        pid: Option<Id>,
        ctx: &ClientContext,
    ) -> Result<Vec<u8>, String> {
        self.expand_depth(format, (sid, wid, pid), ctx, 0, false)
    }

    fn format_value(
        &self,
        key: &str,
        sid: Option<Id>,
        wid: Option<Id>,
        pid: Option<Id>,
        ctx: &ClientContext,
    ) -> Option<String> {
        if let Some(value) = self.client_format(key, ctx) {
            return value;
        }
        let s = sid.and_then(|id| self.sessions.get(&id));
        let w = wid.and_then(|id| self.windows.get(&id));
        let p = pid
            .or_else(|| w.map(|w| w.active))
            .and_then(|id| self.panes.get(&id));
        if let Some(value) = self.graph_format(key, sid, wid, pid, ctx) {
            return value;
        }
        let value = match key {
            "image_support" | "sixel_support" => "1".into(),
            "version" => env!("CARGO_PKG_VERSION").into(),
            "start_time" => (self.started / 1_000_000).to_string(),
            "session_id" => format!("${}", s?.id),
            "session_name" => s?.name.clone(),
            "session_created" => (s?.created / 1_000_000).to_string(),
            "session_activity" => (s?.activity / 1_000_000).to_string(),
            "session_last_attached" => {
                let value = s?.last_attached;
                if value == 0 {
                    return None;
                }
                (value / 1_000_000).to_string()
            }
            "session_windows" => s?.windows.len().to_string(),
            "session_grouped" => u8::from(s?.group.is_some()).to_string(),
            "session_group" => s?.group.clone().unwrap_or_default(),
            "session_group_size" => s?
                .group
                .as_ref()
                .map(|group| {
                    self.sessions
                        .values()
                        .filter(|s| s.group.as_ref() == Some(group))
                        .count()
                        .to_string()
                })
                .unwrap_or_default(),
            "session_group_list" => s?
                .group
                .as_ref()
                .map(|group| {
                    self.sessions
                        .values()
                        .filter(|s| s.group.as_ref() == Some(group))
                        .map(|s| s.name.clone())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default(),
            "session_attached" => {
                let sid = s?.id;
                self.clients
                    .iter()
                    .filter(|client| client.session == Some(sid))
                    .count()
                    .to_string()
            }
            "session_many_attached" => {
                let sid = s?.id;
                u8::from(
                    self.clients
                        .iter()
                        .filter(|client| client.session == Some(sid))
                        .count()
                        > 1,
                )
                .to_string()
            }
            "window_id" => format!("@{}", w?.id),
            "window_name" => w?.name.clone(),
            "window_activity" => (w?.activity / 1_000_000).to_string(),
            "window_index" => s?
                .windows
                .iter()
                .find(|(_, id)| Some(**id) == wid)?
                .0
                .to_string(),
            "window_cell_width" => self
                .panes
                .get(&w?.active)?
                .terminal
                .cell_size()
                .0
                .to_string(),
            "window_cell_height" => self
                .panes
                .get(&w?.active)?
                .terminal
                .cell_size()
                .1
                .to_string(),
            "window_width" => w?.cols.to_string(),
            "window_height" => w?.rows.to_string(),
            "window_panes" => w?.panes.len().to_string(),
            "window_linked" => {
                w?;
                u8::from(
                    self.sessions
                        .values()
                        .map(|s| s.windows.values().filter(|id| Some(**id) == wid).count())
                        .sum::<usize>()
                        > 1,
                )
                .to_string()
            }
            "window_linked_sessions" => {
                w?;
                self.sessions
                    .values()
                    .filter(|s| s.windows.values().any(|id| Some(*id) == wid))
                    .map(|s| (s.group.clone(), if s.group.is_some() { 0 } else { s.id }))
                    .collect::<BTreeSet<_>>()
                    .len()
                    .to_string()
            }
            "window_last_flag" => u8::from(
                s?.last
                    .first()
                    .is_some_and(|index| s.unwrap().windows.get(index).copied() == wid),
            )
            .to_string(),
            "window_active" => u8::from(s?.windows.get(&s?.active).copied() == wid).to_string(),
            "window_zoomed_flag" => u8::from(w?.zoomed).to_string(),
            "window_layout" => self.formatted_layout(w?.id, false, ctx),
            "window_visible_layout" => self.formatted_layout(w?.id, true, ctx),
            "pane_id" => format!("%{}", p?.id),
            "pane_index" => {
                let pane = p?.id;
                let base: usize = w?
                    .options
                    .get("pane-base-index")
                    .or_else(|| self.global_window_options.get("pane-base-index"))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                (w?.panes.iter().position(|id| *id == pane)? + base).to_string()
            }
            "pane_title" => text(p?.terminal.title()),
            "cursor_x" => p?.terminal.cursor().0.to_string(),
            "cursor_y" => p?.terminal.cursor().1.to_string(),
            "pane_width" => p?.cols.to_string(),
            "pane_height" => p?.rows.to_string(),
            "pane_left" => p?.x.to_string(),
            "pane_top" => p?.y.to_string(),
            "pane_right" => (p?.x + i32::from(p?.cols) - 1).to_string(),
            "pane_bottom" => (p?.y + i32::from(p?.rows) - 1).to_string(),
            "pane_active" => u8::from(w?.active == p?.id).to_string(),
            "pane_input_off" => u8::from(p?.input_disabled).to_string(),
            "pane_floating_flag" => {
                u8::from(w?.layout.find(p?.id as u32)?.is_floating()).to_string()
            }
            "pane_zoomed_flag" => u8::from(w?.zoom_target() == Some(p?.id)).to_string(),
            "pane_modal_flag" => u8::from(w?.modal == Some(p?.id)).to_string(),
            "window_modal_pane" => w?.modal.map(|id| format!("%{id}")).unwrap_or_default(),
            "pane_z" => w?.pane_z(p?.id).to_string(),
            "pane_marked" => {
                p?;
                u8::from(
                    self.marked_target()
                        .ok()
                        .is_some_and(|(_, _, _, mp)| Some(mp) == pid),
                )
                .to_string()
            }
            "pane_marked_set" => {
                p?;
                u8::from(self.marked_target().is_ok()).to_string()
            }
            "pane_last" => u8::from(w?.last.first().copied() == Some(p?.id)).to_string(),
            "pane_dead" => u8::from(p?.status.is_some()).to_string(),
            "pane_empty" => u8::from(p?.empty).to_string(),
            "pane_pipe" => u8::from(p?.pipe).to_string(),
            "pane_pipe_pid" => p?
                .pipe
                .then_some(p?.pipe_pid)
                .flatten()
                .map(|pid| pid.to_string())
                .unwrap_or_default(),
            "pane_dead_status" => p?.dead_status.map(|s| s.to_string()).unwrap_or_default(),
            "pane_dead_signal" => p?.dead_signal.clone().unwrap_or_default(),
            "pane_dead_time" => p?
                .dead_time
                .filter(|time| *time != 0)
                .map(|time| (time / 1_000_000).to_string())
                .unwrap_or_default(),
            "pane_last_output_time" => p?
                .last_output_time
                .filter(|time| *time != 0)
                .map(|time| (time / 1_000_000).to_string())
                .unwrap_or_default(),
            "pane_output_generation" => p?.output_generation.to_string(),
            "pane_pid" => p?
                .pid
                .filter(|pid| *pid != 0)
                .map(|p| p.to_string())
                .unwrap_or_default(),
            "pane_tty" => p?.tty.clone(),
            "pane_current_path" => p?
                .current_path
                .as_ref()
                .or(p?.cwd.as_ref())
                .cloned()
                .unwrap_or_default(),
            "pane_start_path" => p?.cwd.clone().unwrap_or_default(),
            "pane_start_command" => p?.start_command(),
            "pane_start_command_list" => p?
                .argv
                .iter()
                .map(|arg| format!("'{}'", text(arg).replace('\'', "'\\''")))
                .collect::<Vec<_>>()
                .join(" "),
            "pane_current_command" => p?.current_command(),
            "pane_in_mode" => {
                p?;
                "0".into()
            }
            "history_limit" => p?.history_limit.to_string(),
            "history_size" => p?.terminal.history_len().to_string(),
            "alternate_on" => u8::from(
                p?.terminal.mode(true, 1049)
                    || p?.terminal.mode(true, 47)
                    || p?.terminal.mode(true, 1047),
            )
            .to_string(),
            "cursor_flag" => u8::from(p?.terminal.mode(true, 25)).to_string(),
            "pane_key_mode" => match p?.terminal.extended_keys_mode() {
                1 => "Ext 1",
                2 => "Ext 2",
                _ => "VT10x",
            }
            .into(),
            "keypad_flag" => u8::from(p?.terminal.mode(true, 66)).to_string(),
            "keypad_cursor_flag" => u8::from(p?.terminal.mode(true, 1)).to_string(),
            "insert_flag" => u8::from(p?.terminal.mode(false, 4)).to_string(),
            "wrap_flag" => u8::from(p?.terminal.mode(true, 7)).to_string(),
            _ => {
                return p
                    .and_then(|p| self.terminal_format(key, p))
                    .or_else(|| p.and_then(|p| options::format_value(&p.options, key)))
                    .or_else(|| w.and_then(|w| options::format_value(&w.options, key)))
                    .or_else(|| s.and_then(|s| options::format_value(&s.options, key)))
                    .or_else(|| options::format_value(&self.options, key))
                    .or_else(|| options::format_value(&self.global_window_options, key))
                    .or_else(|| options::format_value(&self.server_options, key))
                    .or_else(|| s.and_then(|s| s.environment.get(key)).cloned())
                    .or_else(|| {
                        if s.is_some_and(|s| s.removed_environment.contains(key)) {
                            None
                        } else {
                            self.environment.get(key).cloned()
                        }
                    });
            }
        };
        Some(value)
    }

    pub fn layout(&self, wid: Id) -> String {
        self.windows[&wid]
            .layout
            .to_legacy()
            .expect("engine layouts are valid")
    }

    fn formatted_layout(&self, wid: Id, visible: bool, ctx: &ClientContext) -> String {
        if ctx.control
            && !ctx
                .client_flags
                .split(',')
                .any(|flag| flag == "new-layouts")
        {
            return if visible {
                self.visible_layout(wid)
            } else {
                self.layout(wid)
            };
        }
        let window = &self.windows[&wid];
        let mut layout = if visible {
            window.visible_layout()
        } else {
            let mut layout = window.layout.clone();
            if window.zoomed {
                for (id, geometry) in &window.zoom_geometry {
                    if let Some(pane) = layout.find_mut(*id) {
                        pane.geometry = *geometry;
                    }
                }
            }
            layout
        };
        fn metadata(layout: &mut Layout, window: &Window, base: u32) {
            match &mut layout.kind {
                tmuxx_layout::Kind::Pane(pane) => {
                    if let Some(id) = pane.id {
                        pane.active = u64::from(id) == window.active;
                        pane.index = window
                            .panes
                            .iter()
                            .position(|pid| *pid == u64::from(id))
                            .map(|i| base + i as u32);
                        pane.last = window
                            .last
                            .iter()
                            .position(|pid| *pid == u64::from(id))
                            .map(|i| i as u32);
                    }
                }
                tmuxx_layout::Kind::Split { children, .. } => {
                    for child in children {
                        metadata(child, window, base);
                    }
                }
            }
        }
        let base = window
            .options
            .get("pane-base-index")
            .or_else(|| self.global_window_options.get("pane-base-index"))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        metadata(&mut layout, window, base);
        layout.to_json_v2().expect("engine layouts are valid")
    }

    fn visible_layout(&self, wid: Id) -> String {
        self.windows[&wid]
            .visible_layout()
            .to_legacy()
            .expect("visible layout is valid")
    }

    pub fn resize_window(&mut self, wid: Id, cols: u16, rows: u16) -> Result<Outcome, String> {
        let mut layout = self
            .windows
            .get(&wid)
            .ok_or("window not found")?
            .layout
            .clone();
        layout
            .resize(cols as u32, rows as u32)
            .map_err(|e| e.to_string())?;
        self.clamp_floating(&mut layout, cols, rows);
        let window = self.windows.get_mut(&wid).unwrap();
        window.layout = layout;
        window.cols = cols;
        window.rows = rows;
        let mut out = Outcome {
            changed: true,
            ..Outcome::default()
        };
        self.sync_layout_with_hidden(wid, &mut out, true);
        out.notifications.push(self.layout_event(wid));
        Ok(out)
    }

    fn sync_layout(&mut self, wid: Id, out: &mut Outcome) {
        self.sync_layout_with_hidden(wid, out, false);
    }

    fn sync_layout_with_hidden(&mut self, wid: Id, out: &mut Outcome, resize_hidden: bool) {
        let window = &self.windows[&wid];
        for id in &window.panes {
            if !resize_hidden && !window.pane_visible(*id) {
                continue;
            }
            let mut geometry = window
                .layout
                .find(*id as u32)
                .expect("pane is in layout")
                .geometry;
            if window.zoom_target() == Some(*id) {
                geometry = tmuxx_layout::Geometry {
                    x: 0,
                    y: 0,
                    width: window.cols as u32,
                    height: window.rows as u32,
                };
            }
            geometry = self.pane_content_geometry(
                wid,
                geometry,
                window.layout.geometry,
                window.layout.find(*id as u32).unwrap().is_floating()
                    && window.zoom_target() != Some(*id),
            );
            let pane = self.panes.get_mut(id).unwrap();
            pane.x = geometry.x;
            pane.y = geometry.y;
            if pane.cols != geometry.width as u16 || pane.rows != geometry.height as u16 {
                pane.cols = geometry.width as u16;
                pane.rows = geometry.height as u16;
                pane.terminal.resize(pane.cols, pane.rows);
                out.effects.push(Effect::Resize {
                    pane: *id,
                    cols: pane.cols,
                    rows: pane.rows,
                });
            }
        }
    }

    fn layout_event(&self, wid: Id) -> Notification {
        let context = ClientContext::default();
        Notification::LayoutChange {
            window: wid,
            legacy: (self.layout(wid), self.visible_layout(wid)),
            extended: (
                self.formatted_layout(wid, false, &context),
                self.formatted_layout(wid, true, &context),
            ),
            session_flags: self
                .sessions
                .values()
                .filter(|session| session.windows.values().any(|id| *id == wid))
                .map(|session| {
                    (
                        session.id,
                        self.expand(
                            b"#{window_raw_flags}",
                            Some(session.id),
                            Some(wid),
                            None,
                            &context,
                        )
                        .expect("built-in window flags"),
                    )
                })
                .collect(),
            native_flags: if self.windows[&wid].zoomed { "*Z" } else { "*" },
        }
    }

    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "sessions": self.sessions.values().map(|s| serde_json::json!({"id":s.id,"name":s.name,"active":s.active,"windows":s.windows})).collect::<Vec<_>>(),
            "windows": self.windows.values().map(|w| serde_json::json!({"id":w.id,"name":w.name,"active":w.active,"cols":w.cols,"rows":w.rows,"zoomed":w.zoomed,"zoomed_pane":w.zoom_target(),"visible_layout":self.formatted_layout(w.id,true,&ClientContext::default()),"panes":w.panes,"layout":if w.layout.panes().iter().any(|p| p.z.is_some()) { self.formatted_layout(w.id,false,&ClientContext::default()) } else { self.layout(w.id) }})).collect::<Vec<_>>(),
            "panes":self.panes.values().map(|p| serde_json::json!({"id":p.id,"window":p.window,"x":p.x,"y":p.y,"cols":p.cols,"rows":p.rows,"pid":p.pid,"status":p.status,"persistent":p.persistent,"modal":self.windows[&p.window].modal == Some(p.id),"modal_capture_keys":p.modal_capture_keys,"modal_close_click":p.modal_close_click,"modal_close_cancel":p.modal_close_cancel,"terminal":p.terminal.snapshot()})).collect::<Vec<_>>()
        })
    }
}

fn parse_key(bytes: &[u8]) -> Option<tmuxx_terminal::KeyEvent> {
    use tmuxx_terminal::{Key, KeyEvent, Modifiers};
    let original = std::str::from_utf8(bytes).ok()?;
    let mut name = original;
    let mut modifiers = Modifiers::default();
    if let Some(rest) = name.strip_prefix('^')
        && !rest.is_empty()
    {
        modifiers.control = true;
        if rest.len() == 1 {
            let mut event =
                KeyEvent::new(Key::Char((rest.as_bytes()[0] as char).to_ascii_lowercase()));
            event.modifiers = modifiers;
            return Some(event);
        }
        name = rest;
    }
    if let Some(hex) = name.strip_prefix("0x") {
        let end = hex.bytes().take_while(u8::is_ascii_hexdigit).count();
        return u32::from_str_radix(&hex[..end], 16)
            .ok()
            .and_then(char::from_u32)
            .map(|cp| KeyEvent::new(Key::Char(cp)));
    }
    loop {
        if let Some(rest) = name.strip_prefix("C-").or_else(|| name.strip_prefix("c-")) {
            modifiers.control = true;
            name = rest;
        } else if let Some(rest) = name.strip_prefix("M-").or_else(|| name.strip_prefix("m-")) {
            modifiers.alt = true;
            name = rest;
        } else if let Some(rest) = name.strip_prefix("S-").or_else(|| name.strip_prefix("s-")) {
            modifiers.shift = true;
            name = rest;
        } else {
            break;
        }
    }
    let canonical = name.to_ascii_lowercase();
    let key = match canonical.as_str() {
        "enter" => Some(Key::Enter),
        "tab" => Some(Key::Char('\t')),
        "btab" => {
            modifiers.shift = true;
            Some(Key::Tab)
        }
        "space" => Some(Key::Char(' ')),
        "bspace" => Some(Key::Backspace),
        "escape" => Some(Key::Escape),
        "up" => Some(Key::Up),
        "down" => Some(Key::Down),
        "left" => Some(Key::Left),
        "right" => Some(Key::Right),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "ppage" | "pageup" | "pgup" => Some(Key::PageUp),
        "npage" | "pagedown" | "pgdn" => Some(Key::PageDown),
        "ic" | "insert" => Some(Key::Insert),
        "dc" | "delete" => Some(Key::Delete),
        "kpenter" => Some(Key::KeypadEnter),
        "kp/" => Some(Key::KeypadDivide),
        "kp*" => Some(Key::KeypadMultiply),
        "kp-" => Some(Key::KeypadSubtract),
        "kp+" => Some(Key::KeypadAdd),
        "kp." => Some(Key::KeypadDecimal),
        key if key.len() == 3 && key.starts_with("kp") && key.as_bytes()[2].is_ascii_digit() => {
            Some(Key::Keypad(key.as_bytes()[2] - b'0'))
        }
        _ => input::CONTROL_NAMES
            .iter()
            .position(|control| control.eq_ignore_ascii_case(name))
            .map(|cp| Key::Char(cp as u8 as char))
            .or_else(|| {
                canonical
                    .strip_prefix('f')
                    .and_then(|n| n.parse::<u8>().ok())
                    .filter(|n| (1..=12).contains(n))
                    .map(Key::Function)
                    .or_else(|| {
                        (name.chars().count() == 1
                            && !name.chars().next().unwrap().is_ascii_control())
                        .then(|| Key::Char(name.chars().next().unwrap()))
                    })
            })
            .or_else(|| (name == "\x7f").then_some(Key::Char('\x7f'))),
    };
    if let Some(key) = key {
        let mut event = KeyEvent::new(key);
        event.modifiers = modifiers;
        Some(event)
    } else {
        None
    }
}

pub const SUPPORTED_COMMANDS: &[&str] = &[
    "show-messages",
    "server-access",
    "new-session",
    "has-session",
    "attach-session",
    "detach-client",
    "switch-client",
    "new-window",
    "swap-window",
    "last-window",
    "next-window",
    "previous-window",
    "split-window",
    "new-pane",
    "swap-pane",
    "rotate-window",
    "join-pane",
    "move-pane",
    "break-pane",
    "resize-window",
    "resize-pane",
    "select-layout",
    "next-layout",
    "previous-layout",
    "list-clients",
    "list-sessions",
    "list-windows",
    "list-panes",
    "display-message",
    "capture-pane",
    "clear-history",
    "send-keys",
    "send-prefix",
    "select-pane",
    "last-pane",
    "select-window",
    "rename-session",
    "rename-window",
    "link-window",
    "move-window",
    "unlink-window",
    "respawn-pane",
    "respawn-window",
    "pipe-pane",
    "kill-pane",
    "kill-window",
    "kill-session",
    "kill-server",
    "start-server",
    "set-option",
    "show-options",
    "set-environment",
    "show-environment",
    "set-buffer",
    "show-buffer",
    "delete-buffer",
    "paste-buffer",
    "load-buffer",
    "save-buffer",
    "list-buffers",
    "refresh-client",
    "list-commands",
];
#[cfg(test)]
mod tests {
    use super::*;
    fn run(e: &mut Engine, c: &mut ClientContext, line: &str) -> Result<Outcome, String> {
        let commands = tmuxx_command::parse(line.as_bytes()).unwrap();
        e.execute(&commands[0], c)
    }
    #[test]
    fn graph_and_effects() {
        let mut e = Engine::default();
        let mut c = ClientContext::default();
        let result = run(&mut e, &mut c, "new-session -d -s demo -x 80 -y 24").unwrap();
        assert!(matches!(result.effects[0], Effect::Spawn { pane: 0, .. }));
        run(&mut e, &mut c, "split-window -h -t %0").unwrap();
        assert_eq!(e.panes.len(), 2);
        assert_eq!(e.panes[&0].cols + e.panes[&1].cols + 1, 80);
        let r = run(&mut e, &mut c, "list-panes -F '#{pane_id} #{pane_width}'").unwrap();
        assert_eq!(r.output, b"%0 40\n%1 39\n");
        run(&mut e, &mut c, "kill-pane -t %1").unwrap();
        assert_eq!(e.panes[&0].cols, 80);
    }
    #[test]
    fn reject_interactive_before_mutation() {
        let mut e = Engine::default();
        let mut c = ClientContext::default();
        assert!(run(&mut e, &mut c, "new-session").is_err());
        assert!(e.sessions.is_empty());
    }
    #[test]
    fn missing_target_is_not_active_target() {
        let mut e = Engine::default();
        let mut c = ClientContext::default();
        run(&mut e, &mut c, "new -d").unwrap();
        assert!(run(&mut e, &mut c, "kill-pane -t %900").is_err());
        assert_eq!(e.panes.len(), 1);
    }

    #[test]
    fn linked_windows_share_panes_and_unlink_keeps_other_owner() {
        let mut e = Engine::default();
        let mut c = ClientContext::default();
        run(&mut e, &mut c, "new -d -s first").unwrap();
        run(&mut e, &mut c, "new -d -s second").unwrap();
        run(&mut e, &mut c, "link-window -s first:0 -t second:3").unwrap();
        assert_eq!(e.sessions[&1].windows[&3], 0);
        run(&mut e, &mut c, "rename-window -t second:3 shared").unwrap();
        assert_eq!(
            run(&mut e, &mut c, "list-windows -t first -F '#{window_name}'")
                .unwrap()
                .output,
            b"shared\n"
        );
        let removed = run(&mut e, &mut c, "kill-session -t first").unwrap();
        assert!(
            !removed
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::Kill { pane: 0 }))
        );
        assert!(e.panes.contains_key(&0));
        run(&mut e, &mut c, "move-window -s second:3 -t second:5").unwrap();
        assert_eq!(e.sessions[&1].windows[&5], 0);
        assert!(run(&mut e, &mut c, "unlink-window -t second:5").is_err());
    }

    #[test]
    fn replacing_last_window_keeps_destination_session() {
        let mut e = Engine::default();
        let mut c = ClientContext::default();
        run(&mut e, &mut c, "new -d -s first").unwrap();
        run(&mut e, &mut c, "new -d -s second").unwrap();
        run(&mut e, &mut c, "link-window -k -s first:0 -t second:0").unwrap();
        assert_eq!(e.sessions[&1].windows[&0], 0);
        assert!(!e.panes.contains_key(&1));
    }

    #[test]
    fn byte_buffers_preserve_order_and_translate_paste_newlines() {
        let mut e = Engine::default();
        let mut c = ClientContext::default();
        run(&mut e, &mut c, "new -d").unwrap();
        e.store_buffer(Some("z".into()), b"old".to_vec()).unwrap();
        e.store_buffer(Some("a".into()), b"new\n\0".to_vec())
            .unwrap();
        assert!(run(&mut e, &mut c, "show-buffer").is_err());
        assert!(
            run(&mut e, &mut c, "paste-buffer")
                .unwrap()
                .effects
                .is_empty()
        );
        assert_eq!(
            run(&mut e, &mut c, "show-buffer -b a").unwrap().output,
            b"new\n\0"
        );
        let out = run(&mut e, &mut c, "paste-buffer -b a").unwrap();
        assert!(matches!(&out.effects[0], Effect::Write { data, .. } if data == b"new\r^@"));
        run(&mut e, &mut c, "set-buffer -b a -n renamed").unwrap();
        assert_eq!(
            run(
                &mut e,
                &mut c,
                "list-buffers -F '#{buffer_name}:#{buffer_size}'"
            )
            .unwrap()
            .output,
            b"renamed:5\nz:3\n"
        );
    }

    #[test]
    fn unsupported_flags_do_not_mutate_graph() {
        let mut e = Engine::default();
        let mut c = ClientContext::default();
        run(&mut e, &mut c, "new -d").unwrap();
        assert!(run(&mut e, &mut c, "split-window -Q").is_err());
        assert!(run(&mut e, &mut c, "new-window -Q").is_err());
        assert_eq!((e.windows.len(), e.panes.len()), (1, 1));
        assert_eq!(
            run(&mut e, &mut c, "display-message -p -t %99 hello")
                .unwrap()
                .output,
            b"hello\n"
        );
    }

    #[test]
    fn layouts_resize_nested_panes_and_restore_after_zoom() {
        let mut e = Engine::default();
        let mut c = ClientContext::default();
        run(&mut e, &mut c, "new -d -x 80 -y 24").unwrap();
        run(&mut e, &mut c, "splitw -h -l 20").unwrap();
        run(&mut e, &mut c, "resizew -x 100 -y 30").unwrap();
        assert_eq!((e.panes[&0].cols, e.panes[&1].cols), (69, 30));
        run(&mut e, &mut c, "resizep -t %1 -x 40").unwrap();
        assert_eq!((e.panes[&0].cols, e.panes[&1].cols), (59, 40));
        run(&mut e, &mut c, "splitw -t %0 -v -l 8").unwrap();
        run(&mut e, &mut c, "resizep -Z -t %2").unwrap();
        assert_eq!((e.panes[&2].cols, e.panes[&2].rows), (100, 30));
        run(&mut e, &mut c, "resizep -Z -t %2").unwrap();
        assert_eq!((e.panes[&2].cols, e.panes[&2].rows), (59, 8));
        run(&mut e, &mut c, "killp -t %0").unwrap();
        assert_eq!((e.panes[&2].cols, e.panes[&2].rows), (59, 30));
        e.windows[&0].layout.validate_tiled().unwrap();
    }

    #[test]
    fn key_encoding_follows_application_modes_and_literal_bypasses_them() {
        let mut e = Engine::default();
        let mut c = ClientContext::default();
        run(&mut e, &mut c, "new -d").unwrap();
        e.feed(0, b"\x1b[?1h");
        let out = run(&mut e, &mut c, "send Up C-c M-x").unwrap();
        assert!(
            matches!(&out.effects[0], Effect::Write { data, .. } if data == b"\x1bOA\x03\x1bx")
        );
        let out = run(&mut e, &mut c, "send -l Up").unwrap();
        assert!(matches!(&out.effects[0], Effect::Write { data, .. } if data == b"Up"));
    }
}
