//! Window attachments own views; only temporary attachments own the engine.
use super::*;
use rustty::tmuxx::{Connection, ServerOptions};
use rustty_app::tmuxx_copy::{CopiedWindow, WindowCopy};
use rustty_app::tmuxx_workspace::Mapping;
use std::collections::BTreeSet;
use std::sync::mpsc;

pub(super) use rustty_app::tmuxx_menu::{Command as MenuCommand, Source, Target};
pub(super) use rustty_app::tmuxx_target::LOCAL_TARGET_HINT;

pub(super) fn temporary_endpoint(name: &str) -> std::io::Result<PathBuf> {
    #[cfg(windows)]
    return rustty::tmuxx::windows_endpoint(name);
    #[cfg(unix)]
    Ok(std::path::absolute(tmuxx_cli::default_native_endpoint()?)?
        .with_file_name(format!("{name}.native")))
}

pub(super) fn local_endpoint(target: &str) -> Result<PathBuf> {
    Ok(rustty_app::tmuxx_target::local_endpoint(target)?)
}

pub(super) fn connection_timeout(source: &Source) -> Duration {
    match source {
        Source::Ssh(_) | Source::SystemSsh(_) => Duration::from_secs(120),
        Source::Embedded(_) | Source::Local(_) => Duration::from_secs(10),
    }
}

pub(super) fn server_options(config: &config::Config, endpoint: &Path) -> Result<ServerOptions> {
    let mut options = ServerOptions::new(endpoint);
    if let Some(path) = &config.tmuxx_config_file {
        let check = || -> std::io::Result<()> {
            if !std::fs::metadata(path)?.is_file() {
                return Err(std::io::Error::other("not a regular file"));
            }
            std::fs::File::open(path)?;
            Ok(())
        };
        check().map_err(|error| {
            format!(
                "Could not read tmuxx configuration {}: {error}",
                path.display()
            )
        })?;
        options.config.push(path.clone());
    }
    Ok(options)
}

fn persistent_server_command(
    config: &config::Config,
    endpoint: &Path,
) -> Result<std::process::Command> {
    let options = server_options(config, endpoint)?;
    let executable = std::env::current_exe()?;
    #[cfg(windows)]
    let executable = executable.with_file_name("tmuxx.exe");
    let prefix: &[&str] = if cfg!(windows) { &[] } else { &["tmux"] };
    let mut command = tmuxx_cli::native_daemon_command(&executable, &options.endpoint, prefix);
    for path in &options.config {
        command.arg("-f").arg(path);
    }
    Ok(command)
}

struct PendingSession {
    name: String,
    submitted: bool,
    deadline: Instant,
    copy: Option<WindowCopy>,
    copying: bool,
}

struct PendingPane {
    reply: mpsc::Receiver<std::io::Result<Vec<u8>>>,
    tab: Option<Id>,
    pane: Option<u64>,
    focused: Id,
    deadline: Instant,
}

pub(super) struct Attachment {
    pub source: Source,
    pub connection: Option<Connection>,
    pub mapping: Mapping,
    pub selected: BTreeSet<u64>,
    pub instance: String,
    pub retry_at: Option<Instant>,
    pub status: Option<String>,
    pub monitoring: bool,
    reconnect_enabled: bool,
    open_all: bool,
    pending_session: Option<PendingSession>,
    pending_panes: Vec<PendingPane>,
    copied_window: Option<CopiedWindow>,
    connection_deadline: Option<Instant>,
    shutdown_deadline: Option<Instant>,
    restore: Vec<workspace::SavedAttachment>,
    replacing: bool,
}

impl Attachment {
    pub fn new(source: Source, connection: Option<Connection>, open_all: bool) -> Self {
        if let Some(connection) = &connection {
            let _ = connection.watch_server();
        }
        let reconnect_enabled = matches!(source, Source::Local(_));
        let mut mapping = Mapping::default();
        mapping.local_focus = !matches!(source, Source::SystemSsh(_));
        let connection_deadline = connection
            .is_some()
            .then(|| Instant::now() + connection_timeout(&source));
        Self {
            source,
            connection,
            mapping,
            selected: BTreeSet::new(),
            instance: String::new(),
            retry_at: None,
            status: None,
            monitoring: false,
            open_all,
            pending_session: None,
            pending_panes: Vec::new(),
            copied_window: None,
            connection_deadline,
            shutdown_deadline: None,
            restore: Vec::new(),
            replacing: false,
            reconnect_enabled,
        }
    }
    pub fn temporary(&self) -> bool {
        matches!(self.source, Source::Embedded(_))
    }
    pub fn target(&self) -> Target {
        Target {
            source: self.source.clone(),
            instance: self.instance.clone(),
        }
    }
    pub fn command_action(
        &mut self,
        workspace: &Workspace,
        window: Id,
        action: &Action,
        mut argv: Vec<String>,
    ) -> std::result::Result<(), String> {
        let connection = self
            .connection
            .as_ref()
            .filter(|connection| !connection.is_closed() && connection.instance() == self.instance)
            .ok_or_else(|| {
                "The tmuxx connection is offline; reconnect before changing its panels".to_owned()
            })?;
        if !self.mapping.local_focus
            || !matches!(action, Action::NewSplit(_) | Action::NewFloatingPane)
        {
            return connection
                .command(argv)
                .map(|_| ())
                .map_err(|e| e.to_string());
        }
        let tab = workspace
            .windows
            .iter()
            .find(|state| state.id == window)
            .and_then(|state| state.tabs.get(state.active_tab))
            .filter(|tab| self.mapping.windows.contains_key(&tab.id))
            .ok_or("The tab is no longer available")?;
        argv.extend(["-P".into(), "-F".into(), "#{pane_id}".into()]);
        let reply = connection
            .command_with_reply(argv.into_iter().map(String::into_bytes).collect())
            .map_err(|e| e.to_string())?;
        self.cancel_pending_focus(tab.id);
        self.pending_panes.push(PendingPane {
            reply,
            tab: Some(tab.id),
            pane: None,
            focused: tab.focused,
            deadline: Instant::now() + connection_timeout(&self.source),
        });
        Ok(())
    }
    pub fn cancel_pending_focus(&mut self, tab: Id) {
        // Keep the reply receiver so canceled focus cannot hide command failures.
        for pending in &mut self.pending_panes {
            if pending.tab == Some(tab) {
                pending.tab = None;
            }
        }
    }
    fn apply_pending_focus(&mut self, workspace: &mut Workspace) -> Vec<String> {
        let mut errors = Vec::new();
        self.pending_panes.retain_mut(|pending| {
            let tab = workspace
                .windows
                .iter_mut()
                .flat_map(|window| &mut window.tabs)
                .find(|tab| Some(tab.id) == pending.tab)
                .filter(|tab| {
                    self.mapping.windows.contains_key(&tab.id) && tab.focused == pending.focused
                });
            if tab.is_none() {
                pending.tab = None;
            }
            if pending.pane.is_none() {
                match pending.reply.try_recv() {
                    Ok(Ok(output)) => {
                        pending.pane = std::str::from_utf8(&output)
                            .ok()
                            .and_then(|text| text.trim().strip_prefix('%'))
                            .and_then(|id| id.parse().ok());
                        if pending.pane.is_none() {
                            errors.push("Pane creation returned an invalid pane ID".into());
                            return false;
                        }
                    }
                    Ok(Err(error)) => {
                        errors.push(format!("Could not create pane: {error}"));
                        return false;
                    }
                    Err(mpsc::TryRecvError::Empty) => return true,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        errors.push("Pane creation reply was lost".into());
                        return false;
                    }
                }
            }
            let Some(tab) = tab else {
                return false;
            };
            if let Some(pane) = tab
                .panes
                .keys()
                .find(|pane| self.mapping.panes.get(pane) == pending.pane.as_ref())
                .copied()
            {
                tab.focus(pane);
                return false;
            }
            // The command reply can arrive before its new pane appears in the snapshot.
            !self
                .mapping
                .panes
                .values()
                .any(|pane| Some(*pane) == pending.pane)
        });
        errors
    }
    fn monitor(&mut self, wake: Arc<dyn Fn() + Send + Sync>) -> Result<()> {
        let Source::Local(endpoint) = self.source.clone() else {
            return Err("Only local servers can be monitored after disconnect".into());
        };
        let instance = self.instance.clone();
        *self = Self::new(Source::Local(endpoint.clone()), None, false);
        self.instance = instance;
        self.monitoring = true;
        self.reconnect_enabled = false;
        if !self.instance.is_empty() {
            let connection = Connection::native_commands(&endpoint, &self.instance, wake)?;
            connection.watch_server()?;
            self.connection = Some(connection);
            self.connection_deadline = Some(Instant::now() + connection_timeout(&self.source));
        }
        Ok(())
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.retry_at
            .into_iter()
            .chain(
                self.pending_session
                    .as_ref()
                    .map(|pending| pending.deadline),
            )
            .chain(self.connection_deadline)
            .chain(self.shutdown_deadline)
            .chain(self.pending_panes.iter().map(|pending| pending.deadline))
            .min()
    }
    fn expire_connection(&mut self, now: Instant) -> bool {
        if self
            .connection_deadline
            .is_none_or(|deadline| deadline > now)
        {
            return false;
        }
        self.connection_deadline = None;
        if self.connection.as_ref().is_some_and(|connection| {
            if self.monitoring {
                !connection.is_closed()
                    && !self.instance.is_empty()
                    && connection.instance() == self.instance
            } else {
                connection.graph().is_some()
            }
        }) {
            return false;
        }
        self.connection = None;
        self.pending_panes.clear();
        self.status = Some(
            if self.monitoring {
                "Connection timed out waiting for the server handshake"
            } else {
                "Connection timed out waiting for the server session list"
            }
            .into(),
        );
        if self.reconnect_enabled && matches!(self.source, Source::Local(_)) {
            self.retry_at = Some(now + Duration::from_secs(2));
        } else {
            self.open_all = false;
            self.retry_at = None;
        }
        true
    }
    fn request_session(&mut self, name: String, now: Instant) -> Result<()> {
        if self.pending_session.is_some() {
            return Err("A session is already being created on this server".into());
        }
        self.pending_session = Some(PendingSession {
            name,
            submitted: false,
            deadline: now + Duration::from_secs(15),
            copy: None,
            copying: false,
        });
        Ok(())
    }
    fn expire_requests(&mut self, now: Instant) -> bool {
        let mut expired = false;
        let pending_panes = self.pending_panes.len();
        self.pending_panes.retain(|pending| pending.deadline > now);
        if pending_panes != self.pending_panes.len() {
            self.status = Some("Pane creation timed out; focus was left unchanged".into());
            expired = true;
        }
        if self
            .pending_session
            .as_ref()
            .is_some_and(|pending| pending.deadline <= now)
        {
            self.pending_session = None;
            self.copied_window = None;
            expired = true;
            self.status = Some(
                "Session creation timed out; check the server session list before trying again"
                    .into(),
            );
        }
        if self
            .shutdown_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            self.shutdown_deadline = None;
            self.status =
                Some("Server shutdown timed out; check the server before trying again".into());
            expired = true;
        }
        expired
    }
    fn observe_graph(
        &mut self,
        graph: &rustty::tmuxx::Graph,
        instance: String,
    ) -> Vec<(u64, [f64; 4])> {
        self.connection_deadline = None;
        if !self.instance.is_empty() && self.instance != instance {
            self.selected.clear();
            self.copied_window = None;
            self.pending_panes.clear();
            if self
                .pending_session
                .as_ref()
                .is_some_and(|pending| pending.submitted)
            {
                self.pending_session = None;
            }
            self.status = Some("The daemon was replaced; select a session to attach".into());
        }
        self.instance = instance;
        if self.open_all {
            self.selected
                .extend(graph.sessions.iter().map(|session| session.id));
            self.open_all = self.temporary() && graph.sessions.is_empty();
        }
        if let Some(pending) = &self.pending_session
            && pending.submitted
            && !pending.copying
            && self
                .copied_window
                .as_ref()
                .is_none_or(|copied| copy_is_visible(copied, graph))
            && let Some(session) = graph.sessions.iter().find(|session| {
                self.copied_window.as_ref().map_or_else(
                    || session.name == pending.name,
                    |copied| session.id == copied.session,
                )
            })
        {
            self.selected.insert(session.id);
            self.pending_session = None;
            self.status = None;
        }
        let mut frames = Vec::new();
        for saved in self.restore.drain(..) {
            if saved.instance_id == self.instance
                && !self.instance.is_empty()
                && graph
                    .sessions
                    .iter()
                    .any(|session| session.id == saved.session_id)
            {
                self.selected.insert(saved.session_id);
                frames.push((saved.session_id, saved.frame));
            } else {
                self.status = Some(format!(
                    "Saved session {} is no longer available; select a session to attach",
                    saved.session_name
                ));
            }
        }
        self.selected
            .retain(|id| graph.sessions.iter().any(|session| session.id == *id));
        frames
    }
    pub fn creating_window(&self) -> bool {
        self.pending_session.is_some() || self.open_all
    }
    pub fn waiting_for_window(&self) -> bool {
        self.open_all
            || self.pending_session.is_some()
            || self.shutdown_deadline.is_some()
            || !self.restore.is_empty()
    }
    fn saved_attachments(&self, workspace: &Workspace) -> Vec<workspace::SavedAttachment> {
        let Source::Local(endpoint) = &self.source else {
            return Vec::new();
        };
        if !self.reconnect_enabled {
            return Vec::new();
        }
        let mut saved: BTreeMap<_, _> = self
            .restore
            .iter()
            .cloned()
            .map(|saved| (saved.session_id, saved))
            .collect();
        if let Some(connection) = &self.connection
            && !self.instance.is_empty()
            && connection.instance() == self.instance
            && let Some(graph) = connection.graph()
        {
            for window in &workspace.windows {
                if let Some(session) = self
                    .mapping
                    .sessions
                    .get(&window.id)
                    .filter(|session| self.selected.contains(session))
                    .and_then(|sid| graph.sessions.iter().find(|s| s.id == *sid))
                {
                    saved.insert(
                        session.id,
                        workspace::SavedAttachment {
                            endpoint: endpoint.clone(),
                            instance_id: self.instance.clone(),
                            session_id: session.id,
                            session_name: session.name.clone(),
                            frame: window.frame,
                        },
                    );
                }
            }
        }
        saved.into_values().collect()
    }
    pub fn label(&self, names: &BTreeMap<String, String>) -> String {
        let source = match &self.source {
            Source::Embedded(_) => "temporary".into(),
            Source::Local(path) => rustty_app::tmuxx_target::local_label(path),
            Source::Ssh(host) | Source::SystemSsh(host) => format!("SSH · {host}"),
        };
        let source = names
            .iter()
            .find(|(_, target)| self.source.matches_target(target))
            .map(|(name, _)| format!("{name} · {source}"))
            .unwrap_or(source);
        if self.monitoring || self.connection.as_ref().is_none_or(Connection::is_closed) {
            format!("tmuxx · {source} · disconnected")
        } else {
            format!("tmuxx · {source}")
        }
    }

    pub fn window_tag(&self, window: Id, names: &BTreeMap<String, String>) -> (String, String) {
        let session = self.mapping.session_names.get(&window);
        let mut label = session.map_or_else(|| "tmuxx".into(), |name| format!("tmuxx · {name}"));
        if let Source::Ssh(host) | Source::SystemSsh(host) = &self.source {
            let host = names
                .iter()
                .find(|(_, target)| self.source.matches_target(target))
                .map(|(name, _)| name.as_str())
                .unwrap_or_else(|| host.rsplit('@').next().unwrap_or(host));
            label.push_str(" · ");
            label.push_str(host);
        }
        let source = self.label(names);
        let tooltip = session.map_or_else(
            || source.clone(),
            |name| format!("Session: {name}\n{source}"),
        );
        (label, tooltip)
    }
}

impl Attachment {
    fn copying_session(&self, session: u64, name: &str) -> bool {
        self.copied_window
            .as_ref()
            .is_some_and(|copied| copied.session == session)
            || self.pending_session.as_ref().is_some_and(|pending| {
                pending.name == name && (pending.copy.is_some() || pending.copying)
            })
    }

    pub(super) fn menu_entry(&self, name: Option<&str>) -> rustty_app::tmuxx_menu::Entry {
        use rustty_app::tmuxx_menu::Entry;
        let mut children = Vec::new();
        if let Some(status) = &self.status {
            children.push(Entry::label(status));
        }
        if let Source::Embedded(endpoint) | Source::Local(endpoint) = &self.source {
            children.push(Entry::label(format!(
                "Native endpoint: {}",
                endpoint.display()
            )));
        }
        if let Some(address) = self.source.connection_target() {
            children.push(Entry::command(
                "Copy Connection Address",
                MenuCommand::CopyAddress(address),
            ));
        }
        let target = self.target();
        let live = self.connection.as_ref().is_some_and(|connection| {
            !connection.is_closed()
                && !target.instance.is_empty()
                && connection.instance() == target.instance
        });
        let connections = self
            .connection
            .as_ref()
            .filter(|_| live)
            .and_then(Connection::server_status)
            .filter(|status| status.instance_id == target.instance)
            .map(|status| status.connections);
        if !matches!(self.source, Source::SystemSsh(_)) {
            children.push(Entry::label(connections.map_or_else(
                || "Connections: unavailable".into(),
                |count| format!("Connections: {count}"),
            )));
        }
        for session in self
            .connection
            .as_ref()
            .map(Connection::sessions)
            .unwrap_or_default()
        {
            if self.copying_session(session.id, &session.name) {
                children.push(Entry::label(format!("{} · copying…", session.name)));
                continue;
            }
            let label = if self.selected.contains(&session.id) {
                "Focus Window"
            } else {
                "Open Window"
            };
            let session_entries = if live {
                let mut items = vec![Entry::command(
                    label,
                    MenuCommand::Open(target.clone(), session.id),
                )];
                if !matches!(self.source, Source::SystemSsh(_))
                    && self.selected.contains(&session.id)
                    && self.mapping.sessions.values().any(|id| *id == session.id)
                {
                    items.push(Entry::command(
                        "Fit to This Window",
                        MenuCommand::Fit(target.clone(), session.id),
                    ));
                }
                items.extend(
                    session
                        .windows
                        .into_iter()
                        .map(|name| Entry::label(format!("Tab: {name}"))),
                );
                items.push(Entry::command(
                    "Terminate Session…",
                    MenuCommand::Kill(target.clone(), session.id),
                ));
                items
            } else {
                vec![Entry::label("Disconnected")]
            };
            children.push(Entry::submenu(session.name, session_entries));
        }
        if !self.temporary() && !self.monitoring {
            children.push(Entry::command(
                "Disconnect",
                MenuCommand::Disconnect(target.clone()),
            ));
        }
        if live {
            if self.monitoring {
                children.push(match connections {
                    Some(0) => Entry::command(
                        "Stop Idle Server…",
                        MenuCommand::ShutdownIdle(target.clone()),
                    ),
                    Some(_) => Entry::label("Stop Idle Server (connections remain)"),
                    None => Entry::label("Stop Idle Server (connection count unavailable)"),
                });
            } else {
                children.push(Entry::command(
                    "Stop Server…",
                    MenuCommand::Shutdown(target.clone()),
                ));
            }
        }
        if matches!(self.source, Source::Local(_)) && (self.monitoring || !live) {
            children.push(Entry::command("Reconnect", MenuCommand::Reconnect(target)));
        }
        if name.is_some()
            && let Some(target) = self.source.connection_target()
        {
            children.insert(0, Entry::label(target));
        }
        if self.connection.as_ref().is_none_or(Connection::is_closed)
            && let Some(target) = self.source.ssh_target()
        {
            children.insert(
                usize::from(name.is_some()),
                Entry::command("Reconnect", MenuCommand::Connect(target)),
            );
        } else if self.instance.is_empty() && self.status.is_none() {
            children.insert(0, Entry::label("Connecting…"));
        }
        Entry::submenu(
            name.map(str::to_owned).unwrap_or_else(|| {
                self.label(&BTreeMap::new())
                    .trim_start_matches("tmuxx · ")
                    .to_owned()
            }),
            children,
        )
    }
}

pub(super) fn shutdown_confirmation(
    attachments: &[Attachment],
    target: &Target,
    idle: bool,
) -> String {
    let others = attachments
        .iter()
        .find(|entry| entry.target() == *target)
        .and_then(|entry| {
            let connection = entry.connection.as_ref().filter(|connection| {
                !connection.is_closed() && connection.instance() == target.instance
            })?;
            let status = connection
                .server_status()
                .filter(|status| status.instance_id == target.instance)?;
            let own = if entry.monitoring {
                0
            } else {
                connection.view_connection_count()?
            };
            Some(status.connections.saturating_sub(own))
        });
    match others {
        Some(0) => "No other clients are connected. This will close the server's sessions.".into(),
        Some(count) if idle => format!(
            "Other connections: {count}. The server will stay running while they are connected."
        ),
        Some(count) => {
            format!("Other connections: {count}. Stopping this server will also end their work.")
        }
        None => {
            "The connection count is unavailable. This will close the server's sessions.".into()
        }
    }
}

fn menu_entries(
    attachments: &[Attachment],
    names: &BTreeMap<String, String>,
) -> Vec<rustty_app::tmuxx_menu::Entry> {
    use rustty_app::tmuxx_menu::Entry;
    let mut entries: Vec<_> = names
        .iter()
        .map(|(name, target)| {
            attachments
                .iter()
                .find(|entry| entry.source.matches_target(target))
                .map_or_else(
                    || Entry::command(name, MenuCommand::Connect(target.clone())),
                    |entry| entry.menu_entry(Some(name)),
                )
        })
        .collect();
    entries.extend(
        attachments
            .iter()
            .filter(|entry| {
                !names
                    .values()
                    .any(|target| entry.source.matches_target(target))
            })
            .map(|entry| entry.menu_entry(None)),
    );
    entries
}

fn hidden_error_window(
    background: bool,
    active: Option<Id>,
    windows: impl Iterator<Item = (Id, bool)>,
) -> Option<Id> {
    if background {
        return None;
    }
    let mut target = None;
    for (id, visible) in windows {
        if visible {
            return None;
        }
        if target.is_none() || active == Some(id) {
            target = Some(id);
        }
    }
    target
}

fn copy_is_visible(copied: &CopiedWindow, graph: &rustty::tmuxx::Graph) -> bool {
    let Some(session) = graph
        .sessions
        .iter()
        .find(|session| session.id == copied.session)
    else {
        return false;
    };
    let expected: Vec<_> = copied
        .window
        .tabs
        .iter()
        .map(|tab| copied.tabs[&tab.id])
        .collect();
    session
        .windows
        .values()
        .copied()
        .eq(expected.iter().copied())
        && session.windows.get(&(session.active as u32)) == expected.get(copied.window.active_tab)
        && copied.window.tabs.iter().all(|tab| {
            let zoomed = tab.zoom.and_then(|id| copied.panes.get(&id));
            graph.windows.iter().any(|window| {
                window.id == copied.tabs[&tab.id]
                    && window.active == *zoomed.unwrap_or(&copied.panes[&tab.focused])
                    && window.zoomed == zoomed.is_some()
                    && window.panes.len() == tab.panes.len()
                    && tab
                        .panes
                        .keys()
                        .all(|id| window.panes.contains(&copied.panes[id]))
            })
        })
}

fn apply_copy_metadata(copied: CopiedWindow, mapping: &Mapping, workspace: &mut Workspace) {
    let Some(window) = workspace
        .windows
        .iter_mut()
        .find(|window| mapping.sessions.get(&window.id) == Some(&copied.session))
    else {
        return;
    };
    window.frame = copied.window.frame;
    window.frame[0] += 30.0;
    window.frame[1] += 30.0;
    for tab in &mut window.tabs {
        let Some(source) = copied
            .window
            .tabs
            .iter()
            .find(|source| mapping.windows.get(&tab.id) == copied.tabs.get(&source.id))
        else {
            continue;
        };
        tab.color = source.color;
        for (id, saved) in &mut tab.panes {
            if let Some(source) = source.panes.iter().find_map(|(source, saved)| {
                (mapping.panes.get(id) == copied.panes.get(source)).then_some(saved)
            }) {
                *saved = source.clone();
            }
        }
    }
}

impl App {
    pub(super) fn finish_tmuxx_copy(
        &mut self,
        target: Target,
        name: String,
        result: std::result::Result<CopiedWindow, String>,
    ) {
        let Some(entry) = self.tmuxx.iter_mut().find(|entry| {
            entry.target() == target
                && entry
                    .pending_session
                    .as_ref()
                    .is_some_and(|pending| pending.name == name)
        }) else {
            return;
        };
        match result {
            Ok(copied) => {
                entry.copied_window = Some(copied);
                let pending = entry.pending_session.as_mut().unwrap();
                pending.copying = false;
                pending.deadline = Instant::now() + Duration::from_secs(15);
            }
            Err(error) => {
                entry.pending_session = None;
                entry.status = Some(error.clone());
                self.errors.push(error);
                self.reveal_tmuxx_errors();
            }
        }
    }

    fn reveal_tmuxx_errors(&mut self) {
        for host in self.windows.values_mut() {
            host.messages_open = true;
            host.repaint();
        }
        let target = hidden_error_window(
            self.background_smoke,
            self.active,
            self.windows.values().map(|host| {
                (
                    host.id,
                    host.visible && host.window.is_minimized() != Some(true),
                )
            }),
        );
        if let Some(id) = target
            && let Some(native) = self
                .windows
                .iter()
                .find_map(|(native, host)| (host.id == id).then_some(*native))
            && let Some(mut host) = self.windows.remove(&native)
        {
            let quick = self
                .workspace
                .windows
                .iter()
                .any(|window| window.id == id && window.quick);
            if let Some(platform) = &self.platform {
                match platform.preview_window(&host.window, quick, self.config()) {
                    Ok(()) => {
                        host.visible = true;
                        host.occluded = false;
                        host.quick_used |= quick;
                        if quick {
                            self.remember_quick_frame(&host);
                        }
                        host.repaint();
                    }
                    Err(error) => self.errors.push(error),
                }
            }
            self.windows.insert(native, host);
        }
    }

    pub(super) fn sync_tmuxx_menu(&mut self) {
        let entries = menu_entries(&self.tmuxx, &self.config().tmuxx_connections);
        if let Some(platform) = &mut self.platform
            && let Err(error) = platform.set_tmuxx_menu(entries)
        {
            self.errors.push(error);
        }
    }

    pub(super) fn native_tmuxx_command(
        &mut self,
        event_loop: &ActiveEventLoop,
        command: MenuCommand,
    ) {
        if !command.requires_confirmation() {
            let reveal_errors = matches!(
                command,
                MenuCommand::Connect(_) | MenuCommand::Reconnect(_) | MenuCommand::Fit(..)
            );
            let error_count = self.errors.len();
            self.tmuxx_menu_command(command);
            if reveal_errors && self.errors.len() > error_count {
                if self.windows.is_empty() {
                    self.add_window(false);
                    self.reconcile(event_loop);
                }
                self.reveal_tmuxx_errors();
            }
            return;
        }
        if self.windows.is_empty() {
            self.add_window(false);
            self.reconcile(event_loop);
        }
        let native = self
            .windows
            .iter()
            .find(|(_, host)| Some(host.id) == self.active)
            .or_else(|| self.windows.iter().next())
            .map(|(native, _)| *native);
        if let Some(native) = native
            && let Some(mut host) = self.windows.remove(&native)
        {
            host.confirm = Some(Confirmation::TmuxxTerminate(command));
            let quick = self
                .workspace
                .windows
                .iter()
                .any(|window| window.id == host.id && window.quick);
            self.reveal_host(&mut host, quick);
            self.active = Some(host.id);
            self.windows.insert(native, host);
        }
    }

    pub(super) fn tmuxx_window(&self, window: Id) -> Option<&Attachment> {
        self.tmuxx
            .iter()
            .find(|entry| entry.mapping.sessions.contains_key(&window))
    }
    pub(super) fn tmuxx_pane(&self, pane: Id) -> Option<&Attachment> {
        self.tmuxx
            .iter()
            .find(|entry| entry.mapping.panes.contains_key(&pane))
    }
    fn tmuxx_wake(&self) -> Arc<dyn Fn() + Send + Sync> {
        let proxy = self.proxy.clone();
        Arc::new(move || {
            let _ = proxy.send_event(Event::Tmuxx);
        })
    }
    pub(super) fn start_tmuxx(&mut self, temporary: bool) -> Result<()> {
        self.start_tmuxx_with_copy(temporary, None)
    }

    pub(super) fn copy_window_to_tmuxx(&mut self, host: &Host) -> Result<()> {
        if self
            .tmuxx_window(host.id)
            .is_some_and(|entry| matches!(entry.source, Source::Ssh(_) | Source::SystemSsh(_)))
        {
            return Err(
                "Copying remote panes into a local session cannot retain their working directories"
                    .into(),
            );
        }
        let mut window = self.workspace.windows
            [self.index(host.id).ok_or("Window is no longer available")?]
        .clone();
        for tab in &mut window.tabs {
            tab.title = Some(self.tab_name(tab).to_owned());
            for (id, saved) in &mut tab.panes {
                if let Some(pane) = self.panes.get(id) {
                    pane.update_saved(saved);
                }
            }
        }
        let metrics = host.fonts.metrics();
        let scale = host.window.scale_factor() as f32;
        let config = self.window_config(host.id);
        let size = window
            .tabs
            .iter()
            .map(|tab| {
                rustty_app::tmuxx_workspace::grid_size(
                    tab,
                    host.content,
                    true,
                    [
                        metrics.cell_width as f32 / scale,
                        metrics.cell_height as f32 / scale,
                    ],
                    [
                        config.window_padding_x.start + config.window_padding_x.end + 2.0,
                        config.window_padding_y.start
                            + config.window_padding_y.end
                            + if tab.panes.len() > 1 { 18.0 } else { 0.0 }
                            + 2.0,
                    ],
                )
            })
            .fold((1, 1), |(cols, rows), size| {
                (cols.max(size.0), rows.max(size.1))
            });
        let copy = WindowCopy { window, size };
        copy.validate()?;
        self.start_tmuxx_with_copy(false, Some(copy))
    }

    fn start_tmuxx_with_copy(&mut self, temporary: bool, copy: Option<WindowCopy>) -> Result<()> {
        if temporary && self.tmuxx.len() >= 64 {
            return Err("Close a tmuxx connection before opening another".into());
        }
        if temporary {
            let endpoint = temporary_endpoint(&format!(
                "rustty-{}-{}",
                std::process::id(),
                self.workspace.id()
            ))?;
            let initial = match self.config().command.as_ref() {
                Some(config::Command::Shell(command)) => vec![command.as_bytes().to_vec()],
                Some(config::Command::Direct(argv)) => {
                    argv.iter().map(|a| a.as_bytes().to_vec()).collect()
                }
                None => Vec::new(),
            };
            let connection = Connection::embedded_with_options(
                server_options(self.config(), &endpoint)?,
                self.tmuxx_wake(),
                initial,
            )?;
            self.tmuxx.push(Attachment::new(
                Source::Embedded(endpoint),
                Some(connection),
                true,
            ));
        } else {
            let endpoint = std::path::absolute(tmuxx_cli::default_native_endpoint()?)?;
            let source = Source::Local(endpoint.clone());
            let index = if let Some(index) = self.tmuxx.iter().position(|e| e.source == source) {
                if self.tmuxx[index].shutdown_deadline.is_some()
                    && self.tmuxx[index]
                        .connection
                        .as_ref()
                        .is_some_and(|connection| !connection.is_closed())
                {
                    return Err(
                        "The server is still stopping; try again after it disconnects".into(),
                    );
                }
                if self.tmuxx[index].pending_session.is_some() {
                    return Err("A session is already being created on this server".into());
                }
                index
            } else {
                if self.tmuxx.len() >= 64 {
                    return Err("Close a tmuxx connection before opening another".into());
                }
                self.tmuxx.push(Attachment::new(source, None, false));
                self.tmuxx.len() - 1
            };
            if self.tmuxx[index].monitoring
                || self.tmuxx[index]
                    .connection
                    .as_ref()
                    .is_none_or(Connection::is_closed)
            {
                let connection = Connection::native(&endpoint, self.tmuxx_wake()).ok();
                if connection.is_none() {
                    let mut child = persistent_server_command(self.config(), &endpoint)?.spawn()?;
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                }
                let entry = &mut self.tmuxx[index];
                entry.restore = entry.saved_attachments(&self.workspace);
                entry.connection = connection;
                entry.monitoring = false;
                if let Some(connection) = &entry.connection {
                    let _ = connection.watch_server();
                }
                entry.connection_deadline = entry
                    .connection
                    .is_some()
                    .then(|| Instant::now() + Duration::from_secs(10));
                entry.replacing = true;
                entry.retry_at = Some(Instant::now());
            }
            let name = format!("rustty-{}-{}", std::process::id(), self.workspace.id());
            self.tmuxx[index].reconnect_enabled = true;
            self.tmuxx[index].request_session(name, Instant::now())?;
            self.tmuxx[index].pending_session.as_mut().unwrap().copy = copy;
        }
        let _ = self.proxy.send_event(Event::Tmuxx);
        Ok(())
    }
    pub(super) fn connect_tmuxx(&mut self, target: &str) -> Result<()> {
        let source = if let Some(host) = target.strip_prefix("system-ssh:") {
            Source::SystemSsh(host.into())
        } else if let Some(host) = target.strip_prefix("ssh:") {
            Source::Ssh(host.into())
        } else {
            Source::Local(local_endpoint(target)?)
        };
        self.connect_tmuxx_source(source)
    }
    fn connect_tmuxx_source(&mut self, source: Source) -> Result<()> {
        let existing = self.tmuxx.iter().position(|entry| entry.source == source);
        if let Some(index) = existing
            && !self.tmuxx[index].monitoring
            && let Some(connection) = self.tmuxx[index].connection.as_ref()
            && !connection.is_closed()
        {
            let entry = &self.tmuxx[index];
            if !entry.instance.is_empty() && connection.instance() == entry.instance {
                let sessions = connection.sessions();
                if let Some(session) = sessions
                    .iter()
                    .find(|session| entry.selected.contains(&session.id))
                    .or_else(|| sessions.first())
                {
                    self.tmuxx_menu_command(MenuCommand::Open(entry.target(), session.id));
                }
            }
            return Ok(());
        }
        if existing.is_none() && self.tmuxx.len() >= 64 {
            return Err("Close a tmuxx connection before opening another".into());
        }
        let connection = match &source {
            Source::SystemSsh(host) => {
                Connection::system_ssh_with_env(host, &self.config().env, self.tmuxx_wake())?
            }
            Source::Ssh(host) => Connection::ssh_command_with_env(
                host,
                self.config().tmuxx_ssh_command_for(host),
                &self.config().env,
                self.tmuxx_wake(),
            )?,
            Source::Local(path) => Connection::native(path, self.tmuxx_wake())?,
            Source::Embedded(_) => unreachable!("connect targets refer to existing owners"),
        };
        if let Some(index) = existing {
            let _ = connection.watch_server();
            let entry = &mut self.tmuxx[index];
            entry.restore = entry.saved_attachments(&self.workspace);
            entry.open_all |= entry.instance.is_empty() && entry.restore.is_empty();
            entry.connection = Some(connection);
            entry.monitoring = false;
            entry.connection_deadline = Some(Instant::now() + connection_timeout(&source));
            entry.reconnect_enabled = matches!(source, Source::Local(_));
            entry.retry_at = None;
            entry.replacing = true;
            entry.status = None;
        } else {
            self.tmuxx
                .push(Attachment::new(source, Some(connection), true));
        }
        let _ = self.proxy.send_event(Event::Tmuxx);
        Ok(())
    }
    pub(super) fn restore_tmuxx(&mut self) {
        for saved in std::mem::take(&mut self.workspace.tmuxx) {
            let source = Source::Local(saved.endpoint.clone());
            let index = self
                .tmuxx
                .iter()
                .position(|entry| entry.source == source)
                .unwrap_or_else(|| {
                    let mut entry = Attachment::new(source, None, false);
                    entry.retry_at = Some(Instant::now());
                    self.tmuxx.push(entry);
                    self.tmuxx.len() - 1
                });
            self.tmuxx[index].restore.push(saved);
        }
        if self.tmuxx.len() < 64
            && let Ok(endpoint) = tmuxx_cli::default_native_endpoint().and_then(std::path::absolute)
            && !self
                .tmuxx
                .iter()
                .any(|entry| entry.source == Source::Local(endpoint.clone()))
            && let Ok(connection) = Connection::native(&endpoint, self.tmuxx_wake())
        {
            self.tmuxx.push(Attachment::new(
                Source::Local(endpoint),
                Some(connection),
                false,
            ));
        }
    }
    pub(super) fn refresh_tmuxx(&mut self, event_loop: &ActiveEventLoop) {
        let mut connection_error = false;
        let mut reset = Vec::new();
        let mut remove = Vec::new();
        let mut idle = Vec::new();
        for (index, entry) in self.tmuxx.iter_mut().enumerate() {
            let Some(connection) = &entry.connection else {
                if entry.temporary() && !entry.waiting_for_window() {
                    remove.push(index);
                }
                continue;
            };
            let mut errors = connection.errors();
            if entry.shutdown_deadline.is_some() && connection.is_closed() {
                errors.retain(|error| error != "server exited");
            }
            if !errors.is_empty() {
                connection_error = true;
                if !connection.is_closed() {
                    entry.shutdown_deadline = None;
                }
                if entry.copied_window.is_none()
                    && !entry
                        .pending_session
                        .as_ref()
                        .is_some_and(|pending| pending.copying || pending.copy.is_some())
                {
                    entry.pending_session = None;
                    entry.copied_window = None;
                }
                entry.status = Some(errors.join("; "));
                self.errors.extend(errors);
            }
            if connection.is_closed() {
                entry.connection_deadline = None;
                entry.pending_panes.clear();
                if !entry.reconnect_enabled {
                    entry.open_all = false;
                }
                if entry.shutdown_deadline.take().is_some() {
                    entry.selected.clear();
                    self.workspace
                        .windows
                        .retain(|window| !entry.mapping.sessions.contains_key(&window.id));
                    entry.status = Some("Server stopped".into());
                    entry.connection = None;
                } else {
                    entry.status = Some("Disconnected; input is not replayed".into());
                }
                if entry.reconnect_enabled && entry.retry_at.is_none() {
                    entry.retry_at = Some(Instant::now() + Duration::from_secs(2));
                }
                if entry.temporary() {
                    remove.push(index);
                }
                continue;
            }
            let Some(graph) = connection.graph() else {
                continue;
            };
            if entry.replacing {
                reset.extend(entry.mapping.panes.keys().copied());
                entry.pending_panes.clear();
                entry.replacing = false;
            }
            let instance = connection.instance();
            let frames = entry.observe_graph(&graph, instance);
            for session in entry
                .mapping
                .sessions
                .values()
                .filter(|session| !entry.selected.contains(session))
            {
                if let Err(error) = entry.connection.as_ref().unwrap().release_view(*session) {
                    self.errors.push(error.to_string());
                }
            }
            entry.expire_requests(Instant::now());
            if let Some(pending) = &mut entry.pending_session
                && !pending.submitted
            {
                if let Some(copy) = pending.copy.take() {
                    let Source::Local(endpoint) = &entry.source else {
                        unreachable!("window copies use the local persistent server")
                    };
                    let endpoint = endpoint.clone();
                    let name = pending.name.clone();
                    let target = Target {
                        source: entry.source.clone(),
                        instance: entry.instance.clone(),
                    };
                    let proxy = self.proxy.clone();
                    let pane_count: usize =
                        copy.window.tabs.iter().map(|tab| tab.panes.len()).sum();
                    pending.deadline =
                        Instant::now() + Duration::from_secs(30 + pane_count as u64 * 120);
                    pending.submitted = true;
                    pending.copying = true;
                    entry.status = Some("Copying window into a persistent session…".into());
                    std::thread::spawn(move || {
                        let result = (|| {
                            let connection = Connection::native_commands(
                                &endpoint,
                                &target.instance,
                                Arc::new(|| {}),
                            )
                            .map_err(|e| e.to_string())?;
                            copy.run(&connection, &name)
                        })();
                        let _ = proxy.send_event(Event::TmuxxCopied(target, name, result));
                    });
                } else {
                    match entry.connection.as_ref().unwrap().command([
                        "new-session",
                        "-d",
                        "-s",
                        &pending.name,
                    ]) {
                        Ok(_) => {
                            pending.submitted = true;
                            entry.status = Some("Creating session…".into());
                        }
                        Err(error) => {
                            entry.pending_session = None;
                            entry.status = Some(error.to_string());
                            self.errors.push(error.to_string());
                        }
                    }
                }
            }
            if let Err(error) = entry
                .mapping
                .apply(&mut self.workspace, &graph, &entry.selected)
            {
                self.errors.push(error);
                continue;
            }
            self.errors
                .extend(entry.apply_pending_focus(&mut self.workspace));
            for (session, frame) in frames {
                if let Some(id) = entry
                    .mapping
                    .sessions
                    .iter()
                    .find(|(_, sid)| **sid == session)
                    .map(|(id, _)| id)
                    && let Some(window) = self.workspace.windows.iter_mut().find(|w| w.id == *id)
                {
                    window.frame = frame;
                }
            }
            if entry.pending_session.is_none()
                && let Some(copied) = entry.copied_window.take()
            {
                apply_copy_metadata(copied, &entry.mapping, &mut self.workspace);
            }
            if entry.temporary() && !entry.waiting_for_window() && entry.selected.is_empty() {
                remove.push(index);
            }
            if matches!(entry.source, Source::Local(_))
                && graph.sessions.is_empty()
                && !entry.waiting_for_window()
            {
                idle.push(index);
            }
        }
        for index in idle {
            let wake = self.tmuxx_wake();
            if let Err(error) = self.tmuxx[index].monitor(wake) {
                self.tmuxx[index].status = Some(error.to_string());
            }
        }
        for index in remove.into_iter().rev() {
            let entry = self.tmuxx.remove(index);
            self.workspace
                .windows
                .retain(|window| !entry.mapping.sessions.contains_key(&window.id));
        }
        for view in reset {
            self.panes.remove(&view);
            self.failed_panes.remove(&view);
        }
        if connection_error && self.windows.is_empty() && self.workspace.windows.is_empty() {
            self.add_window(false);
        }
        self.reconcile(event_loop);
        self.sync_tmuxx_themes();
        if connection_error {
            self.reveal_tmuxx_errors();
        }
        for entry in &self.tmuxx {
            let Some(graph) = entry.connection.as_ref().and_then(Connection::graph) else {
                continue;
            };
            for (view, remote) in &entry.mapping.panes {
                if let Some(pane) = self.panes.get_mut(view)
                    && let Some(server) = graph.panes.iter().find(|p| p.id == *remote)
                {
                    if let Some(status) = server.status {
                        pane.exited = true;
                        pane.running = None;
                        pane.agent.clear();
                        pane.deck_progress.clear();
                        pane.deck_command.clear();
                        pane.exit_message =
                            Some(format!("Exited {status} — press any key to close"));
                    } else if pane.exited {
                        pane.exited = false;
                        pane.exit_message = None;
                        pane.started = Instant::now();
                    }
                }
            }
        }
        self.changed();
        for host in self.windows.values() {
            host.repaint();
        }
    }
    pub(super) fn retry_tmuxx(&mut self, now: Instant) {
        let error_count = self.errors.len();
        for entry in &mut self.tmuxx {
            if entry.expire_connection(now) {
                if let Some(status) = &entry.status {
                    self.errors.push(status.clone());
                }
                let _ = self.proxy.send_event(Event::Tmuxx);
            }
            if entry.expire_requests(now) {
                let _ = self.proxy.send_event(Event::Tmuxx);
            }
            if entry.retry_at.is_none_or(|at| at > now) {
                continue;
            }
            entry.retry_at = None;
            if !entry.reconnect_enabled {
                continue;
            }
            let Source::Local(endpoint) = &entry.source else {
                continue;
            };
            if entry.connection.as_ref().is_some_and(|c| !c.is_closed()) {
                continue;
            }
            // Cloning a macOS event-loop proxy wakes it; only do so for an actual retry.
            let wake = {
                let proxy = self.proxy.clone();
                Arc::new(move || {
                    let _ = proxy.send_event(Event::Tmuxx);
                })
            };
            match Connection::native(endpoint, wake) {
                Ok(connection) => {
                    let _ = connection.watch_server();
                    entry.restore = entry.saved_attachments(&self.workspace);
                    entry.connection = Some(connection);
                    entry.connection_deadline = Some(now + Duration::from_secs(10));
                    entry.replacing = true;
                    entry.status = None;
                    let _ = self.proxy.send_event(Event::Tmuxx);
                }
                Err(error) => {
                    entry.status = Some(error.to_string());
                    entry.retry_at = Some(now + Duration::from_secs(2));
                }
            }
        }
        if self.errors.len() > error_count {
            self.reveal_tmuxx_errors();
        }
    }
    pub(super) fn close_tmuxx_window(&mut self, window: Id) {
        let Some(index) = self
            .tmuxx
            .iter()
            .position(|e| e.mapping.sessions.contains_key(&window))
        else {
            return;
        };
        let entry = &mut self.tmuxx[index];
        let session = entry.mapping.sessions[&window];
        entry.selected.remove(&session);
        entry.restore.retain(|saved| saved.session_id != session);
        if let Some(connection) = &entry.connection
            && let Err(error) = connection.release_view(session)
        {
            self.errors.push(error.to_string());
        }
        self.workspace.windows.retain(|w| w.id != window);
        if entry.temporary() && entry.selected.is_empty() {
            self.tmuxx.remove(index);
        } else if matches!(entry.source, Source::Local(_))
            && entry.selected.is_empty()
            && !entry.waiting_for_window()
        {
            let wake = self.tmuxx_wake();
            if let Err(error) = self.tmuxx[index].monitor(wake) {
                self.tmuxx[index].status = Some(error.to_string());
            }
        }
        self.changed();
    }
    pub(super) fn tmuxx_menu_command(&mut self, command: MenuCommand) {
        let target = match &command {
            MenuCommand::CopyAddress(address) => {
                use vt::clipboard::{Content, Location, Write, WriteResult};
                let request = Write::osc52(
                    Location::Standard,
                    vec![Content {
                        mime: b"text/plain".to_vec(),
                        data: address.as_bytes().into(),
                    }],
                );
                let result = self
                    .platform
                    .as_ref()
                    .map_or(WriteResult::IoError, |platform| {
                        platform.clipboard_write(&request)
                    });
                if !matches!(result, WriteResult::Success { .. }) {
                    self.errors
                        .push(format!("Could not copy connection address: {result:?}"));
                    self.reveal_tmuxx_errors();
                }
                return;
            }
            MenuCommand::Connect(target) => {
                if let Err(error) = self.connect_tmuxx(target) {
                    self.errors.push(error.to_string());
                    for host in self.windows.values_mut() {
                        host.messages_open = true;
                        host.repaint();
                    }
                }
                self.sync_tmuxx_menu();
                return;
            }
            MenuCommand::Open(target, _)
            | MenuCommand::Fit(target, _)
            | MenuCommand::Kill(target, _)
            | MenuCommand::Execute(target, _)
            | MenuCommand::Disconnect(target)
            | MenuCommand::Reconnect(target)
            | MenuCommand::Shutdown(target)
            | MenuCommand::ShutdownIdle(target) => target,
        };
        let Some(index) = self
            .tmuxx
            .iter()
            .position(|entry| entry.target() == *target)
        else {
            self.errors
                .push("The tmuxx connection changed; select it again in the menu".into());
            return;
        };
        if !matches!(
            command,
            MenuCommand::Disconnect(_) | MenuCommand::Reconnect(_)
        ) && (target.instance.is_empty()
            || self.tmuxx[index]
                .connection
                .as_ref()
                .is_none_or(|connection| {
                    connection.is_closed() || connection.instance() != target.instance
                }))
        {
            self.errors.push(
                "The tmuxx server changed or disconnected; select it again in the menu".into(),
            );
            return;
        }
        match command {
            MenuCommand::Connect(_) | MenuCommand::CopyAddress(_) => {
                unreachable!("connection command handled above")
            }
            MenuCommand::Reconnect(target) => {
                if let Err(error) = self.connect_tmuxx_source(target.source) {
                    self.errors.push(error.to_string());
                }
            }
            MenuCommand::Open(_, session) => {
                let entry = &mut self.tmuxx[index];
                if entry
                    .connection
                    .as_ref()
                    .and_then(Connection::graph)
                    .is_some_and(|graph| {
                        graph
                            .sessions
                            .iter()
                            .any(|s| s.id == session && entry.copying_session(s.id, &s.name))
                    })
                {
                    return;
                }
                if entry.connection.as_ref().is_none_or(|connection| {
                    connection.is_closed()
                        || connection
                            .graph()
                            .is_none_or(|graph| !graph.sessions.iter().any(|s| s.id == session))
                }) {
                    self.errors
                        .push("The session is no longer available".into());
                    return;
                }
                entry.selected.insert(session);
                let window = entry
                    .mapping
                    .sessions
                    .iter()
                    .find_map(|(id, sid)| (*sid == session).then_some(*id));
                if let Some(id) = window
                    && let Some(native) = self
                        .windows
                        .iter()
                        .find_map(|(native, host)| (host.id == id).then_some(*native))
                    && let Some(mut host) = self.windows.remove(&native)
                {
                    self.reveal_host(&mut host, false);
                    self.active = Some(id);
                    self.windows.insert(native, host);
                }
            }
            MenuCommand::Fit(_, session) => {
                let entry = &self.tmuxx[index];
                if !entry.selected.contains(&session)
                    || !entry.mapping.sessions.values().any(|id| *id == session)
                {
                    self.errors
                        .push("Open this session in a local window before fitting it".into());
                } else if let Err(error) = entry.connection.as_ref().unwrap().fit_view_size(session)
                {
                    self.errors.push(error.to_string());
                }
            }
            MenuCommand::Kill(_, session) => {
                let entry = &self.tmuxx[index];
                let result = entry
                    .connection
                    .as_ref()
                    .filter(|connection| {
                        !connection.is_closed()
                            && connection
                                .graph()
                                .is_some_and(|graph| graph.sessions.iter().any(|s| s.id == session))
                    })
                    .ok_or_else(|| "The session is no longer available".to_owned())
                    .and_then(|connection| {
                        connection
                            .command(["kill-session".into(), "-t".into(), format!("${session}")])
                            .map_err(|e| e.to_string())
                    });
                if let Err(error) = result {
                    self.errors.push(error);
                    return;
                }
            }
            MenuCommand::Execute(_, argv) => {
                if let Err(error) = self.tmuxx[index].connection.as_ref().unwrap().command(argv) {
                    self.errors.push(error.to_string());
                    return;
                }
            }
            MenuCommand::Disconnect(_) => {
                let entry = &self.tmuxx[index];
                if let Some(connection) = &entry.connection {
                    for session in entry.mapping.sessions.values() {
                        if let Err(error) = connection.release_view(*session) {
                            self.errors.push(error.to_string());
                        }
                    }
                }
                self.workspace
                    .windows
                    .retain(|w| !entry.mapping.sessions.contains_key(&w.id));
                if matches!(entry.source, Source::Local(_)) {
                    let wake = self.tmuxx_wake();
                    if let Err(error) = self.tmuxx[index].monitor(wake) {
                        self.tmuxx[index].status = Some(error.to_string());
                    }
                } else {
                    self.tmuxx.remove(index);
                }
            }
            MenuCommand::Shutdown(_) | MenuCommand::ShutdownIdle(_) => {
                let idle = matches!(command, MenuCommand::ShutdownIdle(_));
                let entry = &mut self.tmuxx[index];
                let result = entry
                    .connection
                    .as_ref()
                    .ok_or_else(|| "Disconnected".to_owned())
                    .and_then(|c| {
                        c.command([if idle {
                            "tmuxx-stop-if-idle"
                        } else {
                            "kill-server"
                        }])
                        .map_err(|e| e.to_string())
                    });
                if let Err(error) = result {
                    self.errors.push(error);
                    return;
                }
                // Keep the transport alive until the server processes the command.
                entry.reconnect_enabled = false;
                entry.retry_at = None;
                entry.pending_session = None;
                entry.copied_window = None;
                entry.shutdown_deadline = Some(Instant::now() + Duration::from_secs(15));
                entry.open_all = false;
                entry.restore.clear();
                entry.status = Some("Stopping server…".into());
            }
        }
        let _ = self.proxy.send_event(Event::Tmuxx);
        self.changed();
    }
    pub(super) fn detach_all_tmuxx(&mut self) {
        let wake = self.tmuxx_wake();
        for entry in &mut self.tmuxx {
            entry.selected.clear();
            entry.restore.clear();
            entry.pending_session = None;
            entry.copied_window = None;
            entry.open_all = false;
            if let Some(connection) = &entry.connection {
                for session in entry.mapping.sessions.values() {
                    if let Err(error) = connection.release_view(*session) {
                        self.errors.push(error.to_string());
                    }
                }
            }
            self.workspace
                .windows
                .retain(|window| !entry.mapping.sessions.contains_key(&window.id));
            if matches!(entry.source, Source::Local(_))
                && !entry.monitoring
                && let Err(error) = entry.monitor(wake.clone())
            {
                entry.status = Some(error.to_string());
            }
        }
        self.tmuxx.retain(|entry| !entry.temporary());
        self.changed();
    }
    pub(super) fn saved_workspace(&self) -> Workspace {
        let mut saved = self.workspace.clone();
        saved.tmuxx.clear();
        for entry in &self.tmuxx {
            saved
                .windows
                .retain(|w| !entry.mapping.sessions.contains_key(&w.id));
            saved.tmuxx.extend(entry.saved_attachments(&self.workspace));
        }
        saved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustty::tmuxx::{Graph, ServerSession};

    #[test]
    fn disconnected_local_servers_remain_reachable_without_a_view_connection() {
        let mut entry = Attachment::new(Source::Local("/tmp/server.native".into()), None, false);
        entry.instance = "owner".into();
        entry.monitoring = true;
        entry.reconnect_enabled = false;
        let menu = entry.menu_entry(None);
        assert!(menu.label.contains("disconnected"));
        assert!(
            menu.children
                .iter()
                .any(|item| { item.command == Some(MenuCommand::Reconnect(entry.target())) })
        );
        assert!(menu.children.iter().all(|item| !matches!(
            item.command,
            Some(MenuCommand::Disconnect(_) | MenuCommand::ShutdownIdle(_))
        )));
        assert!(entry.deadline().is_none());
    }

    #[cfg(unix)]
    fn monitoring_peer(name: &str, hello: bool) -> (Attachment, std::os::unix::net::UnixStream) {
        use std::os::unix::net::UnixListener;
        let path = std::env::temp_dir().join(format!(
            "rustty-monitor-{name}-{}.native",
            std::process::id()
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let mut entry = Attachment::new(Source::Local(path.clone()), None, false);
        entry.instance = "owner".into();
        entry.monitor(Arc::new(|| {})).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        std::fs::remove_file(path).unwrap();
        if hello {
            tmuxx_protocol::native::write_record(
                &mut peer,
                &tmuxx_protocol::native::Hello {
                    instance_id: Some("owner".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            let until = Instant::now() + Duration::from_secs(2);
            while entry.connection.as_ref().unwrap().instance() != "owner" {
                assert!(Instant::now() < until, "monitor handshake did not complete");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        (entry, peer)
    }

    #[cfg(unix)]
    #[test]
    fn monitoring_requires_known_idle_count_and_never_offers_force_stop() {
        let (mut entry, mut peer) = monitoring_peer("menu", true);
        for count in [None, Some(0), Some(2)] {
            if let Some(count) = count {
                tmuxx_protocol::native::write_record(
                    &mut peer,
                    &tmuxx_protocol::native::ServerMessage::ServerStatus(
                        tmuxx_protocol::native::ServerStatus {
                            instance_id: "owner".into(),
                            connections: count,
                        },
                    ),
                )
                .unwrap();
                let until = Instant::now() + Duration::from_secs(2);
                while entry
                    .connection
                    .as_ref()
                    .unwrap()
                    .server_status()
                    .is_none_or(|status| status.connections != count)
                {
                    assert!(Instant::now() < until, "monitor status did not arrive");
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            let menu = entry.menu_entry(None);
            assert!(menu.children.iter().all(|item| !matches!(
                item.command,
                Some(MenuCommand::Shutdown(_) | MenuCommand::Disconnect(_))
            )));
            let stop = menu
                .children
                .iter()
                .find(|item| item.label.starts_with("Stop Idle Server"))
                .unwrap();
            assert_eq!(
                stop.command,
                (count == Some(0)).then(|| MenuCommand::ShutdownIdle(entry.target()))
            );
        }
        entry.monitoring = false;
        assert!(
            entry
                .menu_entry(None)
                .children
                .iter()
                .any(|item| item.command == Some(MenuCommand::Shutdown(entry.target())))
        );
    }

    #[cfg(unix)]
    #[test]
    fn server_shutdown_confirmation_warns_only_for_other_connections() {
        let (mut entry, mut peer) = monitoring_peer("shutdown-wording", true);
        let target = entry.target();
        let unknown = "The connection count is unavailable. This will close the server's sessions.";
        assert_eq!(
            shutdown_confirmation(std::slice::from_ref(&entry), &target, false),
            unknown
        );
        for (count, monitoring, idle, expected) in [
            (
                0,
                true,
                true,
                "No other clients are connected. This will close the server's sessions.",
            ),
            (
                1,
                false,
                false,
                "No other clients are connected. This will close the server's sessions.",
            ),
            (
                3,
                false,
                false,
                "Other connections: 2. Stopping this server will also end their work.",
            ),
            (
                3,
                true,
                true,
                "Other connections: 3. The server will stay running while they are connected.",
            ),
        ] {
            tmuxx_protocol::native::write_record(
                &mut peer,
                &tmuxx_protocol::native::ServerMessage::ServerStatus(
                    tmuxx_protocol::native::ServerStatus {
                        instance_id: "owner".into(),
                        connections: count,
                    },
                ),
            )
            .unwrap();
            let until = Instant::now() + Duration::from_secs(2);
            while entry
                .connection
                .as_ref()
                .unwrap()
                .server_status()
                .is_none_or(|status| status.connections != count)
            {
                assert!(Instant::now() < until, "server status did not arrive");
                std::thread::sleep(Duration::from_millis(2));
            }
            entry.monitoring = monitoring;
            assert_eq!(
                shutdown_confirmation(std::slice::from_ref(&entry), &target, idle),
                expected
            );
        }
        let stale = Target {
            instance: "previous-owner".into(),
            ..target
        };
        assert_eq!(
            shutdown_confirmation(std::slice::from_ref(&entry), &stale, false),
            unknown
        );
    }

    #[cfg(unix)]
    #[test]
    fn monitoring_deadline_requires_validated_identity_without_session_or_status() {
        for (index, expected) in [None, Some("owner"), Some("replacement"), Some("")]
            .into_iter()
            .enumerate()
        {
            let (mut entry, _peer) =
                monitoring_peer(&format!("deadline-{index}"), expected.is_some());
            if let Some(expected) = expected {
                entry.instance = expected.into();
            }
            let connection = entry.connection.as_ref().unwrap();
            assert!(connection.graph().is_none());
            assert!(connection.server_status().is_none());
            let deadline = entry.connection_deadline.unwrap();
            assert_eq!(entry.deadline(), Some(deadline));
            assert!(!entry.expire_connection(deadline - Duration::from_millis(1)));
            let expires = expected != Some("owner");
            assert_eq!(entry.expire_connection(deadline), expires);
            assert_eq!(entry.connection.is_none(), expires);
            assert!(entry.connection_deadline.is_none());
            assert!(entry.retry_at.is_none());
            assert!(!entry.reconnect_enabled);
            assert_eq!(
                entry.status.as_deref(),
                expires.then_some("Connection timed out waiting for the server handshake")
            );
        }
    }

    #[test]
    fn server_configuration_is_checked_and_passed_as_a_separate_daemon_argument() {
        let directory =
            std::env::temp_dir().join(format!("rustty-server-config-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("server settings.conf");
        std::fs::write(&path, "").unwrap();
        let mut config = config::Config::default();
        config.tmuxx_config_file = Some(path.clone());
        let endpoint = directory.join("server.native");
        let options = server_options(&config, &endpoint).unwrap();
        assert_eq!(options.config, std::slice::from_ref(&path));
        let command = persistent_server_command(&config, &endpoint).unwrap();
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            &args[args.len() - 2..],
            [std::ffi::OsStr::new("-f"), path.as_os_str()]
        );

        config.tmuxx_config_file = None;
        assert!(
            server_options(&config, &endpoint)
                .unwrap()
                .config
                .is_empty()
        );
        assert!(
            !persistent_server_command(&config, &endpoint)
                .unwrap()
                .get_args()
                .any(|arg| arg == "-f")
        );
        for invalid in [directory.clone(), directory.join("missing.conf")] {
            config.tmuxx_config_file = Some(invalid.clone());
            let error = server_options(&config, &endpoint).unwrap_err().to_string();
            assert!(error.contains(&invalid.display().to_string()), "{error}");
            assert!(!endpoint.exists());
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires TMUXX_CONPTY_RESOURCES from a verified bundle; run with --ignored"]
    fn temporary_window_endpoint_starts_a_windows_session() {
        let name = format!("rustty-window-test-{}", std::process::id());
        let endpoint = temporary_endpoint(&name).unwrap();
        let connection = Connection::embedded_with_command(
            &endpoint,
            Arc::new(|| {}),
            ["cmd.exe", "/D", "/Q"]
                .map(|arg| arg.as_bytes().to_vec())
                .to_vec(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let errors = connection.errors();
            assert!(errors.is_empty(), "{errors:?}");
            if connection.graph().is_some_and(|graph| {
                graph.sessions.len() == 1 && graph.panes.len() == 1 && graph.panes[0].pid.is_some()
            }) {
                break;
            }
            assert!(Instant::now() < deadline, "temporary session did not start");
            std::thread::sleep(Duration::from_millis(5));
        }
        // The Connect dialog resolves the same instance without losing its
        // account namespace; an invalid filename replacement cannot bind here.
        let target = local_endpoint(&format!("local:{name}")).unwrap();
        assert_eq!(target, local_endpoint(&format!("pipe:{name}")).unwrap());
        let attached = Connection::native(&target, Arc::new(|| {})).unwrap();
        drop(attached);
        drop(connection);
        assert!(Connection::native(&target, Arc::new(|| {})).is_err());
    }

    fn graph(sessions: &[(u64, &str)]) -> Graph {
        Graph {
            sessions: sessions
                .iter()
                .map(|(id, name)| ServerSession {
                    id: *id,
                    name: (*name).into(),
                    active: 0,
                    windows: BTreeMap::new(),
                })
                .collect(),
            windows: Vec::new(),
            panes: Vec::new(),
        }
    }

    fn local() -> Attachment {
        Attachment::new(
            Source::Local(std::env::temp_dir().join("rustty-test.native")),
            None,
            false,
        )
    }

    fn pending_pane_fixture() -> (Attachment, Workspace) {
        let mut entry = local();
        entry.instance = "owner".into();
        entry.mapping.sessions.insert(1, 7);
        entry.mapping.windows.insert(2, 9);
        entry.mapping.panes.extend([(3, 10), (4, 11)]);
        let mut tab = Tab::new(2, 3, PathBuf::new());
        tab.split(4, 5, Direction::Right, PathBuf::new());
        tab.focus(3);
        let mut workspace = Workspace::default();
        workspace.windows.push(WindowState {
            id: 1,
            tabs: vec![tab, Tab::new(6, 7, PathBuf::new())],
            active_tab: 1,
            frame: [0.0; 4],
            quick: false,
        });
        (entry, workspace)
    }

    fn pending_pane(entry: &mut Attachment) -> mpsc::Sender<std::io::Result<Vec<u8>>> {
        let (sender, reply) = mpsc::channel();
        entry.cancel_pending_focus(2);
        entry.pending_panes.push(PendingPane {
            reply,
            tab: Some(2),
            pane: None,
            focused: 3,
            deadline: Instant::now() + Duration::from_secs(10),
        });
        sender
    }

    #[test]
    fn created_pane_focus_waits_for_reply_and_mapping_without_switching_tabs() {
        for reply_first in [false, true] {
            let (mut entry, mut workspace) = pending_pane_fixture();
            let sender = pending_pane(&mut entry);
            if reply_first {
                entry.mapping.panes.remove(&4);
                sender.send(Ok(b"%11\n".to_vec())).unwrap();
            }
            assert!(entry.apply_pending_focus(&mut workspace).is_empty());
            assert_eq!(workspace.windows[0].tabs[0].focused, 3);
            assert_eq!(entry.pending_panes.len(), 1);
            if reply_first {
                entry.mapping.panes.insert(4, 11);
            } else {
                sender.send(Ok(b"%11\n".to_vec())).unwrap();
            }
            assert!(entry.apply_pending_focus(&mut workspace).is_empty());
            assert_eq!(workspace.windows[0].tabs[0].focused, 4);
            assert_eq!(workspace.windows[0].active_tab, 1);
            assert!(entry.pending_panes.is_empty());
        }
    }

    #[test]
    fn created_pane_focus_rejects_failed_invalid_and_lost_replies() {
        for cancel in [false, true] {
            for reply in [
                Some(Err(std::io::Error::other("split failed"))),
                Some(Ok(b"%11\n%12\n".to_vec())),
                Some(Ok(b"not a pane".to_vec())),
                None,
            ] {
                let (mut entry, mut workspace) = pending_pane_fixture();
                let sender = pending_pane(&mut entry);
                if cancel {
                    entry.cancel_pending_focus(2);
                    assert!(entry.apply_pending_focus(&mut workspace).is_empty());
                    assert_eq!(entry.pending_panes.len(), 1);
                }
                if let Some(reply) = reply {
                    sender.send(reply).unwrap();
                }
                drop(sender);
                assert_eq!(entry.apply_pending_focus(&mut workspace).len(), 1);
                assert_eq!(workspace.windows[0].tabs[0].focused, 3);
                assert!(entry.pending_panes.is_empty());
            }
        }
    }

    #[test]
    fn created_pane_focus_does_not_outlive_user_focus_tab_or_daemon() {
        for stale in ["focus", "cancel", "tab", "moved", "daemon", "timeout"] {
            let (mut entry, mut workspace) = pending_pane_fixture();
            let sender = pending_pane(&mut entry);
            sender.send(Ok(b"%11\n".to_vec())).unwrap();
            match stale {
                "focus" => workspace.windows[0].tabs[0].focus(4),
                "cancel" => entry.cancel_pending_focus(2),
                "tab" => {
                    workspace.windows[0].tabs.remove(0);
                    workspace.windows[0].active_tab = 0;
                }
                "moved" => {
                    workspace.windows[0].tabs[0].panes.remove(&4);
                }
                "daemon" => {
                    entry.observe_graph(&graph(&[(7, "work")]), "new-owner".into());
                }
                "timeout" => {
                    let deadline = entry.pending_panes[0].deadline;
                    assert_eq!(entry.deadline(), Some(deadline));
                    assert!(entry.expire_requests(deadline));
                }
                _ => unreachable!(),
            }
            let before = serde_json::to_value(&workspace).unwrap();
            assert!(entry.apply_pending_focus(&mut workspace).is_empty());
            assert_eq!(serde_json::to_value(&workspace).unwrap(), before);
            assert!(entry.pending_panes.is_empty(), "{stale}");
        }
        let (mut entry, mut workspace) = pending_pane_fixture();
        let old = pending_pane(&mut entry);
        let newest = pending_pane(&mut entry);
        old.send(Err(std::io::Error::other("older split failed")))
            .unwrap();
        newest.send(Ok(b"%11\n".to_vec())).unwrap();
        assert_eq!(
            entry.apply_pending_focus(&mut workspace),
            ["Could not create pane: older split failed"]
        );
        assert_eq!(workspace.windows[0].tabs[0].focused, 4);
    }

    #[cfg(unix)]
    #[test]
    fn native_created_panes_focus_only_the_requesting_view() {
        fn refresh(entry: &mut Attachment, workspace: &mut Workspace) -> Option<Graph> {
            let connection = entry.connection.as_ref().unwrap();
            assert!(!connection.is_closed(), "{:?}", connection.errors());
            let graph = connection.graph()?;
            entry.observe_graph(&graph, connection.instance());
            entry
                .mapping
                .apply(workspace, &graph, &entry.selected)
                .unwrap();
            assert!(entry.apply_pending_focus(workspace).is_empty());
            Some(graph)
        }
        let endpoint = std::env::temp_dir().join(format!(
            "rustty-created-focus-{}.native",
            std::process::id()
        ));
        let connection = Connection::embedded_with_command(
            &endpoint,
            Arc::new(|| {}),
            vec![b"/bin/sleep".to_vec(), b"60".to_vec()],
        )
        .unwrap();
        let mut entry = Attachment::new(Source::Embedded(endpoint), Some(connection), true);
        let mut workspace = Workspace::default();
        let deadline = Instant::now() + Duration::from_secs(10);
        while workspace.windows.is_empty() {
            refresh(&mut entry, &mut workspace);
            assert!(Instant::now() < deadline, "initial pane did not arrive");
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut observer = entry.mapping.clone();
        let mut other_workspace = workspace.clone();
        let original = other_workspace.windows[0].tabs[0].focused;
        for action in [Action::NewSplit(Direction::Right), Action::NewFloatingPane] {
            let host = workspace.windows[0].id;
            let previous = workspace.windows[0].tabs[0].focused;
            let argv = entry
                .mapping
                .action(&workspace, host, &action)
                .unwrap()
                .unwrap();
            entry
                .command_action(&workspace, host, &action, argv)
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(graph) = refresh(&mut entry, &mut workspace) {
                    observer
                        .apply(&mut other_workspace, &graph, &entry.selected)
                        .unwrap();
                    assert_eq!(other_workspace.windows[0].tabs[0].focused, original);
                    if entry.pending_panes.is_empty() {
                        assert_ne!(workspace.windows[0].tabs[0].focused, previous);
                        break;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "created pane did not receive focus"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    #[test]
    fn copied_window_waits_for_completion_and_restores_local_metadata() {
        let mut source = WindowState {
            id: 1,
            tabs: vec![
                Tab::new(2, 3, "/tmp/first".into()),
                Tab::new(4, 5, "/tmp/second".into()),
            ],
            active_tab: 1,
            frame: [100.0, 200.0, 900.0, 600.0],
            quick: false,
        };
        source.tabs[1].color = Some([12, 34, 56]);
        source.tabs[1].panes.get_mut(&5).unwrap().title_override = Some("Build".into());
        let before = serde_json::to_value(&source).unwrap();
        let copied = CopiedWindow {
            session: 7,
            window: source.clone(),
            tabs: [(2, 8), (4, 9)].into(),
            panes: [(3, 10), (5, 11)].into(),
        };
        let mut graph: Graph = serde_json::from_value(serde_json::json!({
            "sessions": [{"id": 7, "name": "copy", "active": 5, "windows": {"4": 8, "5": 9}}],
            "windows": [
                {"id": 8, "name": "one", "active": 10, "cols": 80, "rows": 24, "panes": [10],
                 "layout": rustty::tmuxx::Layout::pane(10, 80, 24).to_legacy().unwrap()},
                {"id": 9, "name": "two", "active": 11, "cols": 80, "rows": 24, "panes": [11],
                 "layout": rustty::tmuxx::Layout::pane(11, 80, 24).to_legacy().unwrap()}
            ],
            "panes": [
                {"id": 10, "window": 8, "cols": 80, "rows": 24},
                {"id": 11, "window": 9, "cols": 80, "rows": 24}
            ]
        }))
        .unwrap();
        let mut entry = local();
        entry
            .request_session("copy".into(), Instant::now())
            .unwrap();
        let pending = entry.pending_session.as_mut().unwrap();
        pending.submitted = true;
        pending.copying = true;
        entry.observe_graph(&graph, "daemon".into());
        assert!(entry.copying_session(7, "copy"));
        assert!(entry.selected.is_empty(), "partial copies must stay hidden");
        entry.copied_window = Some(copied);
        entry.pending_session.as_mut().unwrap().copying = false;
        graph.sessions[0].active = 4;
        entry.observe_graph(&graph, "daemon".into());
        assert!(
            entry.selected.is_empty(),
            "wait for the final selection snapshot"
        );
        graph.sessions[0].active = 5;
        graph.sessions[0].name = "renamed while copying".into();
        entry.observe_graph(&graph, "daemon".into());
        assert_eq!(entry.selected, [7].into());
        assert!(entry.pending_session.is_none());
        assert!(entry.copying_session(7, "renamed while copying"));
        let mut workspace = Workspace::default();
        entry
            .mapping
            .apply(&mut workspace, &graph, &entry.selected)
            .unwrap();
        apply_copy_metadata(
            entry.copied_window.take().unwrap(),
            &entry.mapping,
            &mut workspace,
        );
        assert!(!entry.copying_session(7, "renamed while copying"));
        let window = &workspace.windows[0];
        assert_eq!(window.frame, [130.0, 230.0, 900.0, 600.0]);
        assert_eq!(window.active_tab, 1);
        let tab = &window.tabs[1];
        assert_eq!(tab.color, Some([12, 34, 56]));
        assert_eq!(
            tab.panes[&tab.focused].working_directory,
            PathBuf::from("/tmp/second")
        );
        assert_eq!(
            tab.panes[&tab.focused].title_override.as_deref(),
            Some("Build")
        );
        assert_eq!(serde_json::to_value(&source).unwrap(), before);
    }

    #[test]
    fn connection_errors_reveal_hidden_hosts_without_exposing_background_smoke() {
        assert_eq!(
            hidden_error_window(false, Some(2), [(1, false), (2, false)].into_iter()),
            Some(2)
        );
        assert_eq!(
            hidden_error_window(false, None, [(1, false)].into_iter()),
            Some(1)
        );
        assert_eq!(
            hidden_error_window(false, Some(2), [(1, true), (2, false)].into_iter()),
            None
        );
        assert_eq!(
            hidden_error_window(true, Some(2), [(1, false), (2, false)].into_iter()),
            None
        );
        assert_eq!(hidden_error_window(false, None, [].into_iter()), None);
    }

    #[test]
    fn window_tags_include_session_and_remote_identity() {
        for (source, target) in [
            (
                Source::Ssh("user@build.example".into()),
                "ssh:user@build.example",
            ),
            (
                Source::SystemSsh("user@build.example".into()),
                "system-ssh:user@build.example",
            ),
        ] {
            let mut entry = Attachment::new(source, None, false);
            entry.mapping.session_names.insert(7, "work".into());
            let names = BTreeMap::from([("Azure Linux".into(), target.into())]);
            let (label, tooltip) = entry.window_tag(7, &names);
            assert_eq!(label, "tmuxx · work · Azure Linux");
            assert!(tooltip.contains("Session: work"));
            assert!(tooltip.contains("user@build.example"));
            assert!(tooltip.contains("disconnected"));
            assert_eq!(
                entry.window_tag(7, &BTreeMap::new()).0,
                "tmuxx · work · build.example"
            );
            entry.mapping.session_names.insert(7, "renamed 雪".into());
            assert_eq!(
                entry.window_tag(7, &names).0,
                "tmuxx · renamed 雪 · Azure Linux"
            );
        }
        for source in [
            Source::Embedded("temporary.native".into()),
            Source::Local("local.native".into()),
        ] {
            let mut entry = Attachment::new(source, None, false);
            entry.mapping.session_names.insert(7, "work".into());
            let (label, tooltip) = entry.window_tag(7, &BTreeMap::new());
            assert_eq!(label, "tmuxx · work");
            assert!(tooltip.starts_with("Session: work\n"));
            assert_eq!(entry.window_tag(8, &BTreeMap::new()).0, "tmuxx");
        }
    }

    #[test]
    fn configured_connections_are_available_before_connecting() {
        let names = BTreeMap::from([
            ("Azure Linux".into(), "ssh:20.240.208.217".into()),
            ("Legacy".into(), "system-ssh:old-host".into()),
        ]);
        let entries = menu_entries(&[], &names);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].label, "Azure Linux");
        assert_eq!(
            entries[0].command,
            Some(MenuCommand::Connect("ssh:20.240.208.217".into()))
        );
        assert_eq!(
            entries[1].command,
            Some(MenuCommand::Connect("system-ssh:old-host".into()))
        );
    }

    #[test]
    fn configured_connection_reuses_its_entry_and_reconnects_only_on_request() {
        let names = BTreeMap::from([("Azure Linux".into(), "ssh:20.240.208.217".into())]);
        let mut entry = Attachment::new(Source::Ssh("20.240.208.217".into()), None, false);
        entry.status = Some("Disconnected".into());
        let entries = menu_entries(std::slice::from_ref(&entry), &names);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "Azure Linux");
        assert!(
            entries[0]
                .children
                .iter()
                .any(|item| item.label == "Reconnect"
                    && item.command == Some(MenuCommand::Connect("ssh:20.240.208.217".into())))
        );
        assert_eq!(
            entry.label(&names),
            "tmuxx · Azure Linux · SSH · 20.240.208.217 · disconnected"
        );
        assert!(!entry.reconnect_enabled);
        assert!(entry.retry_at.is_none());

        let cleared = BTreeMap::new();
        let entries = menu_entries(std::slice::from_ref(&entry), &cleared);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "SSH · 20.240.208.217 · disconnected");
        assert!(!entry.label(&cleared).contains("Azure Linux"));
    }

    #[test]
    fn configured_sources_match_the_transport_and_keep_all_friendly_aliases() {
        let entry = Attachment::new(Source::Ssh("host".into()), None, false);
        let names = BTreeMap::from([
            ("Native".into(), "ssh:host".into()),
            ("Native alias".into(), "ssh:host".into()),
            ("Stock".into(), "system-ssh:host".into()),
        ]);
        let entries = menu_entries(&[entry], &names);
        assert_eq!(entries.len(), 3);
        assert!(entries[0].command.is_none());
        assert!(entries[1].command.is_none());
        assert_eq!(
            entries[2].command,
            Some(MenuCommand::Connect("system-ssh:host".into()))
        );
    }

    #[test]
    fn local_connections_show_friendly_names_and_copy_round_trippable_addresses() {
        let endpoint = local_endpoint("local:work").unwrap();
        let entry = Attachment::new(Source::Local(endpoint.clone()), None, false);
        assert_eq!(
            entry.label(&BTreeMap::new()),
            "tmuxx · local · work · disconnected"
        );
        let menu = entry.menu_entry(None);
        assert!(menu.children.iter().any(|item| {
            item.label == "Copy Connection Address"
                && item.command == Some(MenuCommand::CopyAddress("local:work".into()))
        }));
        assert!(
            menu.children
                .iter()
                .any(|item| { item.label == format!("Native endpoint: {}", endpoint.display()) })
        );

        let names = BTreeMap::from([("My work".into(), "local:work".into())]);
        let entries = menu_entries(std::slice::from_ref(&entry), &names);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "My work");
        assert_eq!(
            entry.label(&names),
            "tmuxx · My work · local · work · disconnected"
        );
        assert!(
            entries[0]
                .children
                .iter()
                .any(|item| { item.command == Some(MenuCommand::Reconnect(entry.target())) })
        );
    }

    #[test]
    fn ssh_connections_allow_time_for_authentication_prompts() {
        for source in [Source::Ssh("host".into()), Source::SystemSsh("host".into())] {
            assert_eq!(connection_timeout(&source), Duration::from_secs(120));
            let native = !matches!(source, Source::SystemSsh(_));
            assert_eq!(
                Attachment::new(source, None, false).mapping.local_focus,
                native
            );
        }
        for source in [
            Source::Embedded("local".into()),
            Source::Local("local".into()),
        ] {
            assert_eq!(connection_timeout(&source), Duration::from_secs(10));
            assert!(Attachment::new(source, None, false).mapping.local_focus);
        }
    }

    fn saved(instance: &str, session: u64) -> workspace::SavedAttachment {
        workspace::SavedAttachment {
            endpoint: std::env::temp_dir().join("rustty-test.native"),
            instance_id: instance.into(),
            session_id: session,
            session_name: "work".into(),
            frame: [10.0, 20.0, 800.0, 600.0],
        }
    }

    #[test]
    fn restores_only_the_saved_daemon_and_session_even_after_a_rename() {
        for (saved_instance, id, restored) in [
            ("daemon-a", 7, true),
            ("daemon-b", 7, false),
            ("", 7, false),
            ("daemon-a", 8, false),
        ] {
            let mut entry = local();
            entry.restore.push(saved(saved_instance, id));
            assert!(entry.waiting_for_window());
            let frames = entry.observe_graph(&graph(&[(7, "renamed")]), "daemon-a".into());
            assert_eq!(entry.selected.contains(&7), restored);
            assert_eq!(
                frames,
                if restored {
                    vec![(7, saved("daemon-a", 7).frame)]
                } else {
                    Vec::new()
                }
            );
            assert!(!entry.waiting_for_window());
            assert_eq!(entry.status.is_some(), !restored);
        }
    }

    #[test]
    fn a_replacement_daemon_cannot_reuse_selection_or_a_confirmed_menu_target() {
        let mut entry = local();
        entry.observe_graph(&graph(&[(7, "work")]), "daemon-a".into());
        entry.selected.insert(7);
        let target = entry.target();
        entry.request_session("new".into(), Instant::now()).unwrap();
        entry.pending_session.as_mut().unwrap().submitted = true;
        entry.observe_graph(&graph(&[(7, "work"), (8, "new")]), "daemon-b".into());
        assert_ne!(entry.target(), target);
        assert!(entry.selected.is_empty());
        assert!(entry.pending_session.is_none());
        assert!(entry.status.as_deref().unwrap().contains("replaced"));
    }

    #[test]
    fn initial_attach_selects_once_and_detached_sessions_stay_detached() {
        let mut entry = local();
        entry.open_all = true;
        entry.observe_graph(&graph(&[(7, "work")]), "daemon-a".into());
        assert_eq!(entry.selected, BTreeSet::from([7]));
        entry.selected.clear();
        entry.observe_graph(&graph(&[(7, "work"), (8, "new")]), "daemon-a".into());
        assert!(entry.selected.is_empty());
        assert!(!entry.waiting_for_window());

        let mut temporary = Attachment::new(Source::Embedded("unused.native".into()), None, true);
        temporary.observe_graph(&graph(&[]), "embedded".into());
        assert!(temporary.waiting_for_window());
        temporary.observe_graph(&graph(&[(1, "work")]), "embedded".into());
        assert_eq!(temporary.selected, BTreeSet::from([1]));
        assert!(!temporary.waiting_for_window());
    }

    #[test]
    fn pending_creation_is_single_bounded_and_not_satisfied_by_an_existing_name() {
        let now = Instant::now();
        let mut entry = local();
        entry.request_session("new".into(), now).unwrap();
        assert!(entry.waiting_for_window());
        assert!(entry.request_session("another".into(), now).is_err());
        assert_eq!(entry.deadline(), Some(now + Duration::from_secs(15)));
        entry.observe_graph(&graph(&[(7, "new")]), "daemon-a".into());
        assert!(entry.selected.is_empty());
        assert!(!entry.pending_session.as_ref().unwrap().submitted);
        entry.pending_session.as_mut().unwrap().submitted = true;
        entry.observe_graph(&graph(&[(7, "new")]), "daemon-a".into());
        assert_eq!(entry.selected, BTreeSet::from([7]));
        assert!(!entry.waiting_for_window());
        assert!(entry.deadline().is_none());

        entry.request_session("later".into(), now).unwrap();
        entry.retry_at = Some(now + Duration::from_secs(2));
        assert_eq!(entry.deadline(), entry.retry_at);
        entry.expire_requests(now + Duration::from_secs(15));
        assert!(entry.pending_session.is_none());
        assert!(entry.status.as_deref().unwrap().contains("timed out"));
    }

    #[test]
    fn unsent_creation_can_follow_a_restart_but_dead_sessions_are_deselected() {
        let mut entry = local();
        entry.observe_graph(&graph(&[(7, "old")]), "daemon-a".into());
        entry.selected.insert(7);
        entry.request_session("new".into(), Instant::now()).unwrap();
        entry.observe_graph(&graph(&[]), "daemon-b".into());
        assert!(entry.selected.is_empty());
        assert!(entry.pending_session.is_some());
    }

    #[test]
    fn disconnected_local_restore_records_survive_saving_but_stopped_servers_do_not() {
        let mut entry = local();
        let saved = saved("daemon-a", 7);
        entry.restore.push(saved.clone());
        assert_eq!(entry.saved_attachments(&Workspace::default()), vec![saved]);
        entry.reconnect_enabled = false;
        assert!(entry.saved_attachments(&Workspace::default()).is_empty());
    }

    #[test]
    fn queued_shutdown_waits_for_disconnect_but_has_a_deadline() {
        let now = Instant::now();
        let mut entry = local();
        entry.selected.insert(7);
        entry.shutdown_deadline = Some(now + Duration::from_secs(15));
        assert!(entry.waiting_for_window());
        assert_eq!(entry.deadline(), entry.shutdown_deadline);
        assert!(!entry.expire_requests(now));
        assert!(entry.expire_requests(now + Duration::from_secs(15)));
        assert!(!entry.waiting_for_window());
        assert!(entry.selected.contains(&7));
        assert!(
            entry
                .status
                .as_deref()
                .unwrap()
                .contains("shutdown timed out")
        );
    }

    #[cfg(unix)]
    #[test]
    fn stalled_connections_expire_without_losing_restore_or_retrying_ssh() {
        use std::io::Read;
        use std::os::unix::net::UnixListener;
        for (index, source) in [
            Source::Local(std::env::temp_dir().join("rustty-test.native")),
            Source::Ssh("remote".into()),
            Source::SystemSsh("remote".into()),
            Source::Embedded("temporary.native".into()),
        ]
        .into_iter()
        .enumerate()
        {
            let path = std::env::temp_dir().join(format!(
                "rustty-handshake-{}-{index}.native",
                std::process::id()
            ));
            let listener = UnixListener::bind(&path).unwrap();
            let connection = Connection::unix(&path, Arc::new(|| {})).unwrap();
            let (mut peer, _) = listener.accept().unwrap();
            // macOS rejects socket options once the disconnected peer has closed.
            peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
            std::fs::remove_file(path).unwrap();
            let mut entry = Attachment::new(source, Some(connection), true);
            let saved = saved("daemon-a", 7);
            if index == 0 {
                entry.restore.push(saved.clone());
            }
            let deadline = entry.connection_deadline.unwrap();
            assert_eq!(entry.deadline(), Some(deadline));
            assert!(!entry.expire_connection(deadline - Duration::from_millis(1)));
            assert!(entry.connection.is_some());
            assert!(entry.expire_connection(deadline));
            assert!(entry.connection.is_none());
            assert!(entry.status.as_deref().unwrap().contains("timed out"));
            assert!(!entry.expire_connection(deadline + Duration::from_secs(1)));
            if index == 0 {
                assert_eq!(entry.retry_at, Some(deadline + Duration::from_secs(2)));
                assert_eq!(entry.saved_attachments(&Workspace::default()), vec![saved]);
            } else {
                assert!(entry.retry_at.is_none());
                assert!(!entry.waiting_for_window());
            }
            let mut bytes = Vec::new();
            peer.read_to_end(&mut bytes).unwrap();
        }
    }

    #[test]
    fn observed_graph_cancels_the_connection_deadline() {
        let mut entry = local();
        let now = Instant::now();
        entry.connection_deadline = Some(now);
        entry.observe_graph(&graph(&[(7, "work")]), "daemon-a".into());
        assert!(entry.connection_deadline.is_none());
        assert!(!entry.expire_connection(now + Duration::from_secs(20)));
        assert!(entry.status.is_none());
    }
}
