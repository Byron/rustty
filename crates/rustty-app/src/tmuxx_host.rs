//! Window attachments own views; only temporary attachments own the engine.
use super::*;
use rustty::tmuxx::Connection;
use rustty_app::tmuxx_workspace::Mapping;
use std::collections::BTreeSet;

pub(super) use rustty_app::tmuxx_menu::{Command as MenuCommand, Source, Target};

#[cfg(unix)]
pub(super) const LOCAL_TARGET_HINT: &str = "unix:/path/socket.native";
#[cfg(windows)]
pub(super) const LOCAL_TARGET_HINT: &str = "pipe:INSTANCE";

pub(super) fn temporary_endpoint(name: &str) -> std::io::Result<PathBuf> {
    #[cfg(windows)]
    return rustty::tmuxx::windows_endpoint(name);
    #[cfg(unix)]
    Ok(std::path::absolute(tmuxx_cli::default_native_endpoint()?)?
        .with_file_name(format!("{name}.native")))
}

pub(super) fn local_endpoint(target: &str) -> Result<PathBuf> {
    #[cfg(unix)]
    if let Some(path) = target.strip_prefix("unix:") {
        return Ok(std::path::absolute(path)?);
    }
    #[cfg(windows)]
    if let Some(instance) = target.strip_prefix("pipe:") {
        return Ok(rustty::tmuxx::windows_endpoint(instance)?);
    }
    Err(format!("Use {LOCAL_TARGET_HINT}, ssh:HOST, or system-ssh:HOST").into())
}

pub(super) fn connection_timeout(source: &Source) -> Duration {
    match source {
        Source::Ssh(_) | Source::SystemSsh(_) => Duration::from_secs(120),
        Source::Embedded(_) | Source::Local(_) => Duration::from_secs(10),
    }
}

struct PendingSession {
    name: String,
    submitted: bool,
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
    reconnect_enabled: bool,
    open_all: bool,
    pending_session: Option<PendingSession>,
    connection_deadline: Option<Instant>,
    shutdown_deadline: Option<Instant>,
    restore: Vec<workspace::SavedAttachment>,
    replacing: bool,
}

impl Attachment {
    pub fn new(source: Source, connection: Option<Connection>, open_all: bool) -> Self {
        let reconnect_enabled = matches!(source, Source::Local(_));
        let connection_deadline = connection
            .is_some()
            .then(|| Instant::now() + connection_timeout(&source));
        Self {
            source,
            connection,
            mapping: Mapping::default(),
            selected: BTreeSet::new(),
            instance: String::new(),
            retry_at: None,
            status: None,
            open_all,
            pending_session: None,
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
        if self
            .connection
            .as_ref()
            .is_some_and(|connection| connection.graph().is_some())
        {
            return false;
        }
        self.connection = None;
        self.status = Some("Connection timed out waiting for the server session list".into());
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
        });
        Ok(())
    }
    fn expire_requests(&mut self, now: Instant) -> bool {
        let mut expired = false;
        if self
            .pending_session
            .as_ref()
            .is_some_and(|pending| pending.deadline <= now)
        {
            self.pending_session = None;
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
            && let Some(session) = graph
                .sessions
                .iter()
                .find(|session| session.name == pending.name)
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
            Source::Local(path) => format!("local · {}", path.display()),
            Source::Ssh(host) | Source::SystemSsh(host) => format!("SSH · {host}"),
        };
        let source = self
            .source
            .ssh_target()
            .and_then(|target| {
                names
                    .iter()
                    .find(|(_, value)| **value == target)
                    .map(|(name, _)| format!("{name} · {source}"))
            })
            .unwrap_or(source);
        if self.connection.as_ref().is_none_or(Connection::is_closed) {
            format!("tmuxx · {source} · disconnected")
        } else {
            format!("tmuxx · {source}")
        }
    }
}

impl Attachment {
    fn menu_entry(&self, name: Option<&str>) -> rustty_app::tmuxx_menu::Entry {
        use rustty_app::tmuxx_menu::Entry;
        let mut children = Vec::new();
        if let Some(status) = &self.status {
            children.push(Entry::label(status));
        }
        if let Source::Embedded(endpoint) = &self.source {
            children.push(Entry::label(format!(
                "Native endpoint: {}",
                endpoint.display()
            )));
        }
        let target = self.target();
        let live = self.connection.as_ref().is_some_and(|connection| {
            !connection.is_closed()
                && !target.instance.is_empty()
                && connection.instance() == target.instance
        });
        for session in self
            .connection
            .as_ref()
            .map(Connection::sessions)
            .unwrap_or_default()
        {
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
        if !self.temporary() {
            children.push(Entry::command(
                "Disconnect",
                MenuCommand::Disconnect(target.clone()),
            ));
        }
        if live {
            children.push(Entry::command(
                "Stop Server…",
                MenuCommand::Shutdown(target),
            ));
        }
        if name.is_some()
            && let Some(target) = self.source.ssh_target()
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
                .find(|entry| entry.source.ssh_target().as_ref() == Some(target))
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
                entry
                    .source
                    .ssh_target()
                    .is_none_or(|target| !names.values().any(|value| *value == target))
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

impl App {
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
            self.reveal_host(&mut host, quick);
            self.active = Some(id);
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
            let connecting = matches!(command, MenuCommand::Connect(_));
            let error_count = self.errors.len();
            self.tmuxx_menu_command(command);
            if connecting && self.errors.len() > error_count {
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
            let connection =
                Connection::embedded_with_command(&endpoint, self.tmuxx_wake(), initial)?;
            self.tmuxx.push(Attachment::new(
                Source::Embedded(endpoint),
                Some(connection),
                true,
            ));
        } else {
            let endpoint = std::path::absolute(tmuxx_cli::default_native_endpoint()?)?;
            let source = Source::Local(endpoint.clone());
            let index = if let Some(index) = self.tmuxx.iter().position(|e| e.source == source) {
                if !self.tmuxx[index].reconnect_enabled
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
            if self.tmuxx[index]
                .connection
                .as_ref()
                .is_none_or(Connection::is_closed)
            {
                let connection = Connection::native(&endpoint, self.tmuxx_wake()).ok();
                if connection.is_none() {
                    let executable = std::env::current_exe()?;
                    #[cfg(windows)]
                    let executable = executable.with_file_name("tmuxx.exe");
                    let prefix: &[&str] = if cfg!(windows) { &[] } else { &["tmux"] };
                    let mut child =
                        tmuxx_cli::native_daemon_command(&executable, &endpoint, prefix).spawn()?;
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                }
                let entry = &mut self.tmuxx[index];
                entry.restore = entry.saved_attachments(&self.workspace);
                entry.connection = connection;
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
        let existing = self.tmuxx.iter().position(|entry| entry.source == source);
        if let Some(index) = existing
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
            let entry = &mut self.tmuxx[index];
            entry.restore = entry.saved_attachments(&self.workspace);
            entry.open_all |= entry.instance.is_empty() && entry.restore.is_empty();
            entry.connection = Some(connection);
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
        for (index, entry) in self.tmuxx.iter_mut().enumerate() {
            let Some(connection) = &entry.connection else {
                if entry.temporary() && !entry.waiting_for_window() {
                    remove.push(index);
                }
                continue;
            };
            let errors = connection.errors();
            if !errors.is_empty() {
                connection_error = true;
                entry.pending_session = None;
                entry.status = Some(errors.join("; "));
                self.errors.extend(errors);
            }
            if connection.is_closed() {
                entry.connection_deadline = None;
                if !entry.reconnect_enabled {
                    entry.open_all = false;
                }
                if entry.shutdown_deadline.take().is_some() {
                    entry.selected.clear();
                    self.workspace
                        .windows
                        .retain(|window| !entry.mapping.sessions.contains_key(&window.id));
                    entry.status = Some("Server stopped".into());
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
            if let Err(error) = entry
                .mapping
                .apply(&mut self.workspace, &graph, &entry.selected)
            {
                self.errors.push(error);
                continue;
            }
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
            if entry.temporary() && !entry.waiting_for_window() && entry.selected.is_empty() {
                remove.push(index);
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
        }
        self.changed();
    }
    pub(super) fn tmuxx_menu_command(&mut self, command: MenuCommand) {
        let target = match &command {
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
            | MenuCommand::Kill(target, _)
            | MenuCommand::Execute(target, _)
            | MenuCommand::Disconnect(target)
            | MenuCommand::Shutdown(target) => target,
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
        if !matches!(command, MenuCommand::Disconnect(_))
            && (target.instance.is_empty()
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
            MenuCommand::Connect(_) => unreachable!("connection command handled above"),
            MenuCommand::Open(_, session) => {
                let entry = &mut self.tmuxx[index];
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
                let entry = self.tmuxx.remove(index);
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
            }
            MenuCommand::Shutdown(_) => {
                let entry = &mut self.tmuxx[index];
                let result = entry
                    .connection
                    .as_ref()
                    .ok_or_else(|| "Disconnected".to_owned())
                    .and_then(|c| c.command(["kill-server"]).map_err(|e| e.to_string()));
                if let Err(error) = result {
                    self.errors.push(error);
                    return;
                }
                // Keep the transport alive until the server processes the command.
                entry.reconnect_enabled = false;
                entry.retry_at = None;
                entry.pending_session = None;
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
        for entry in &mut self.tmuxx {
            entry.selected.clear();
            entry.restore.clear();
            entry.pending_session = None;
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
        let target = local_endpoint(&format!("pipe:{name}")).unwrap();
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
    fn ssh_connections_allow_time_for_authentication_prompts() {
        for source in [Source::Ssh("host".into()), Source::SystemSsh("host".into())] {
            assert_eq!(connection_timeout(&source), Duration::from_secs(120));
        }
        for source in [
            Source::Embedded("local".into()),
            Source::Local("local".into()),
        ] {
            assert_eq!(connection_timeout(&source), Duration::from_secs(10));
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
