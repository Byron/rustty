//! UI-thread ownership and native activation for the dashboard.
use super::*;
use rustty_app::{
    deck::{Dashboard, Target},
    deck_device::{self, Action, Candidate, Worker},
};
use std::{
    sync::mpsc::{self, Receiver},
    thread::JoinHandle,
};

type Discovery = JoinHandle<std::result::Result<Option<Candidate>, String>>;

fn task_label(terminal_title: &str, cwd: &std::path::Path) -> String {
    fn clean(value: &str) -> Option<String> {
        let value: String = value.chars().filter(|c| !c.is_control()).collect();
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    }
    let name = (|| {
        let title = clean(terminal_title)?;
        // A known directory name can identify the worktree above a nested cwd.
        // Arbitrary status prose from OSC titles does not name the task.
        cwd.ancestors().find_map(|ancestor| {
            let name = ancestor.file_name()?.to_string_lossy();
            (title == name || title == ancestor.as_os_str().to_string_lossy())
                .then(|| clean(&name))
                .flatten()
        })
    })()
    .or_else(|| {
        cwd.file_name()
            .and_then(|name| clean(&name.to_string_lossy()))
    })
    .unwrap_or_default();
    // This is the local project.task naming convention, without Git discovery.
    match name.split_once('.') {
        Some((project, task)) if !project.is_empty() && !task.is_empty() => task.trim().to_owned(),
        _ => name,
    }
}

pub(super) struct Connection {
    pub report_generation: u64,
    connected: bool,
    dashboard: Dashboard,
    worker: Option<Worker>,
    discovery: Option<Discovery>,
    actions: Option<Receiver<Action>>,
    wake_pending: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    next_check: Option<Instant>,
    last_check: Option<Instant>,
    last_config: Option<(bool, Option<String>)>,
    stopping: bool,
    last_diagnostic: Option<Instant>,
}
impl Default for Connection {
    fn default() -> Self {
        Self {
            report_generation: 0,
            connected: false,
            dashboard: Dashboard::default(),
            worker: None,
            discovery: None,
            actions: None,
            wake_pending: Arc::new(AtomicBool::new(false)),
            overflow: Arc::new(AtomicBool::new(false)),
            next_check: None,
            last_check: None,
            last_config: None,
            stopping: false,
            last_diagnostic: None,
        }
    }
}
impl Connection {
    pub(super) fn commands_available(&self) -> bool {
        self.connected && !self.stopping
    }
    pub(super) fn registration_transition(&mut self) {
        self.dashboard.board.mode_generation += 1;
    }
    pub fn deadline(&self) -> Option<Instant> {
        if self.discovery.is_some() || (self.stopping && self.worker.is_some()) {
            Some(Instant::now() + Duration::from_millis(20))
        } else {
            self.next_check
        }
    }
    pub fn busy(&self) -> bool {
        self.worker.is_some() || self.discovery.is_some()
    }
    pub fn stop(&mut self) {
        self.connected = false;
        self.stopping = true;
        self.next_check = None;
        self.dashboard.gesture = None;
        self.dashboard.moving = false;
        if let Some(worker) = &self.worker {
            worker.stop();
        }
    }
    pub fn reap(&mut self) {
        if self.worker.as_ref().is_some_and(Worker::is_finished) {
            self.connected = false;
            let _ = self.worker.take().unwrap().join();
            self.actions = None;
        }
        if self.discovery.as_ref().is_some_and(JoinHandle::is_finished) {
            let _ = self.discovery.take().unwrap().join();
        }
    }
    fn diagnostic(&mut self, message: &str) {
        let now = Instant::now();
        if self
            .last_diagnostic
            .is_none_or(|at| now.duration_since(at) >= Duration::from_secs(60))
        {
            eprintln!("Rustty Stream Deck: {message}");
            self.last_diagnostic = Some(now);
        }
    }
}
impl App {
    fn cycle_deck(&mut self, group: usize) {
        self.cycle_deck_with_host(group, None);
    }
    fn cycle_deck_with_host(&mut self, group: usize, host: Option<&mut Host>) {
        self.sync_deck_with_host(host.as_deref());
        // Chained shortcuts can navigate again before the native focus event arrives.
        let focused = if host.is_some() {
            self.active.and_then(|id| self.focused(id))
        } else {
            self.deck_focused()
        };
        if let Some(pane) = self.deck.dashboard.cycle(group, focused) {
            self.reveal_pane_with_host(pane, host);
        }
    }
    fn pull_deck_to_first(&mut self) {
        self.sync_deck();
        if self.deck.dashboard.pull_to_first(&mut self.workspace) {
            self.changed();
        }
        self.sync_deck();
    }
    pub(super) fn deck_command(&mut self, command: config::StreamDeckAction) -> bool {
        self.deck_command_with_host(command, None)
    }
    pub(super) fn deck_command_with_host(
        &mut self,
        command: config::StreamDeckAction,
        mut host: Option<&mut Host>,
    ) -> bool {
        use config::StreamDeckAction;
        if !self.config().stream_deck || !self.deck.commands_available() {
            return false;
        }
        if !self.deck.dashboard.board.screens_awake
            && !matches!(
                command,
                StreamDeckAction::NextPage | StreamDeckAction::Brightness
            )
        {
            return false;
        }
        match command {
            StreamDeckAction::NeedsInput => self.cycle_deck_with_host(0, host.as_deref_mut()),
            StreamDeckAction::Working => self.cycle_deck_with_host(1, host.as_deref_mut()),
            StreamDeckAction::Done => self.cycle_deck_with_host(2, host.as_deref_mut()),
            StreamDeckAction::Idle => self.cycle_deck_with_host(3, host.as_deref_mut()),
            StreamDeckAction::NextPage => self.deck.dashboard.page(),
            StreamDeckAction::Brightness => {
                if let Some(worker) = &self.deck.worker {
                    worker.request_brightness();
                }
            }
            StreamDeckAction::PullToFirst => {
                self.sync_deck_with_host(host.as_deref());
                if self.deck.dashboard.pull_to_first(&mut self.workspace) {
                    self.changed();
                }
            }
        }
        self.sync_deck_with_host(host.as_deref());
        if let Some(worker) = &self.deck.worker {
            worker.submit(self.deck.dashboard.board.clone());
        }
        true
    }
    fn deck_focused(&self) -> Option<Id> {
        if !self.deck.dashboard.board.screens_awake {
            return None;
        }
        self.windows
            .values()
            .find(|host| host.visible && host.focused && !host.occluded && host.peek.is_none())
            .and_then(|host| self.focused(host.id))
    }
    pub(super) fn notification_focused(&self, pane: Id, host: Option<&Host>) -> bool {
        host.into_iter()
            .chain(self.windows.values())
            .any(|host| host.visible && host.focused && self.focused(host.id) == Some(pane))
    }
    pub(super) fn update_deck_commands(&mut self) {
        let config = &self.loaded.config;
        let mut had_reports = false;
        let mut has_reports = false;
        for id in self
            .workspace
            .windows
            .iter()
            .flat_map(|window| &window.tabs)
            .flat_map(|tab| tab.pane_ids())
        {
            if let Some(pane) = self.panes.get_mut(&id).filter(|pane| !pane.exited) {
                had_reports |= pane.has_deck_report();
                pane.deck_command.update(config.notify_on_command_finish);
                has_reports |= pane.has_deck_report();
            }
        }
        if had_reports != has_reports {
            self.deck.registration_transition();
        }
    }
    fn sync_deck(&mut self) {
        self.sync_deck_with_host(None);
    }
    fn sync_deck_with_host(&mut self, host: Option<&Host>) {
        self.update_deck_commands();
        let focused = host
            .filter(|host| {
                self.deck.dashboard.board.screens_awake
                    && host.visible
                    && host.focused
                    && !host.occluded
                    && host.peek.is_none()
            })
            .and_then(|host| self.focused(host.id))
            .or_else(|| self.deck_focused());
        if let Some(pane) = focused.and_then(|id| self.panes.get_mut(&id)) {
            pane.agent.acknowledge();
            pane.deck_progress.acknowledge();
            pane.deck_command.acknowledge();
        }
        let reports = self
            .workspace
            .windows
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|tab| tab.pane_ids())
            .filter_map(|id| {
                let pane = self.panes.get(&id).filter(|pane| {
                    !pane.exited
                        && (pane.agent.is_registered() || pane.deck_command.is_registered())
                })?;
                let label = task_label(&pane.title, &pane.cwd);
                pane.deck_view(id, &label).map(|mut view| {
                    view.remote = self.tmuxx_pane(id).is_some();
                    (id, view)
                })
            })
            .collect();
        let progress = self
            .panes
            .iter()
            .filter(|(_, pane)| !pane.exited)
            .filter_map(|(&id, pane)| pane.deck_progress.state().map(|state| (id, state)))
            .collect();
        if self
            .deck
            .dashboard
            .update(&mut self.workspace, &reports, &progress, focused)
        {
            self.changed();
        }
    }
    fn live_deck_target(&self, target: Target) -> bool {
        self.workspace
            .windows
            .iter()
            .flat_map(|w| &w.tabs)
            .any(|t| t.panes.contains_key(&target.pane))
            && self.panes.get(&target.pane).is_some_and(|pane| {
                !pane.exited
                    && pane
                        .deck_view(target.pane, "")
                        .is_some_and(|r| r.target == target)
            })
    }
    /// Notification and physical dashboard actions share the exact native reveal path.
    pub(super) fn reveal_pane(&mut self, pane: Id) {
        self.reveal_pane_with_host(pane, None);
    }
    fn reveal_pane_with_host(&mut self, pane: Id, mut host: Option<&mut Host>) {
        let target = self.workspace.windows.iter().find_map(|window| {
            window
                .tabs
                .iter()
                .position(|tab| tab.panes.contains_key(&pane))
                .map(|tab| (window.id, tab, window.quick))
        });
        let Some((id, tab, quick)) = target else {
            return;
        };
        let native = self
            .windows
            .iter()
            .find_map(|(native, host)| (host.id == id).then_some(*native));
        if native.is_none() && host.as_deref().is_none_or(|host| host.id != id) {
            return;
        }
        let index = self.index(id).unwrap();
        self.workspace.windows[index].active_tab = tab;
        self.focus_pane(id, pane);
        if let Some(host) = host.as_deref_mut().filter(|host| host.id == id) {
            host.peek = None;
            self.reveal_host(host, quick);
        } else if let Some(native) = native
            && let Some(mut host) = self.windows.remove(&native)
        {
            host.peek = None;
            self.reveal_host(&mut host, quick);
            self.windows.insert(native, host);
        }
        self.active = Some(id);
        if let Some(state) = self.panes.get_mut(&pane) {
            state.agent.acknowledge();
            state.deck_progress.acknowledge();
            state.deck_command.acknowledge();
        }
        // Detached hosts are restored before their caller synchronizes session focus.
        if host.is_none() {
            self.sync_host_state();
        }
    }
    pub(super) fn reveal_host(&mut self, host: &mut Host, quick: bool) {
        if quick {
            self.quick_visible(host, true, false);
        } else {
            host.visible = true;
            if !self.background_smoke {
                host.window.set_visible(true);
                host.window.set_minimized(false);
                host.window.focus_window();
            }
        }
        host.repaint();
    }
    fn deck_action(&mut self, action: Action) {
        // Brightness acknowledgement remains truthful even if disable raced the USB write.
        if let Action::Brightness(value) = action {
            self.deck.dashboard.board.brightness = value;
            return;
        }
        if self.deck.stopping || !self.config().stream_deck {
            return;
        }
        if !self.deck.dashboard.board.screens_awake
            && !matches!(
                action,
                Action::Connected
                    | Action::Disconnected(_)
                    | Action::Page { .. }
                    | Action::WakeScreens
            )
        {
            return;
        }
        match action {
            Action::Connected => self.deck.connected = true,
            Action::Gesture(source) => {
                self.deck.dashboard.gesture = source.filter(|s| {
                    self.deck.dashboard.board.valid(*s)
                        && s.target.is_some_and(|t| self.live_deck_target(t))
                });
                if self.deck.dashboard.gesture.is_none() {
                    self.deck.dashboard.moving = false;
                }
            }
            Action::Move(source) => {
                self.deck.dashboard.moving =
                    source.is_some() && source == self.deck.dashboard.gesture;
            }
            Action::Focus(capture) => {
                self.sync_deck();
                if self.deck.dashboard.board.valid(capture)
                    && let Some(target) = capture.target.filter(|t| self.live_deck_target(*t))
                {
                    self.deck.dashboard.board.page = capture.slot / rustty_app::deck::PAGE_SIZE;
                    self.reveal_pane(target.pane);
                }
            }
            Action::Swap {
                source,
                destination,
            } => {
                self.sync_deck();
                if self
                    .deck
                    .dashboard
                    .swap(&mut self.workspace, source, destination)
                {
                    self.changed();
                }
            }
            Action::Cycle { group, generation } => {
                self.sync_deck();
                if generation == self.deck.dashboard.board.mode_generation {
                    self.cycle_deck(group);
                }
            }
            Action::PullToFirst {
                generation,
                mode_generation,
            } => {
                self.sync_deck();
                if generation == self.deck.dashboard.board.generation
                    && mode_generation == self.deck.dashboard.board.mode_generation
                {
                    self.pull_deck_to_first();
                }
            }
            Action::Page { generation } => {
                if generation == self.deck.dashboard.board.mode_generation {
                    self.deck.dashboard.page();
                }
            }
            Action::Disconnected(result) => {
                self.deck.connected = false;
                self.deck.stopping = true;
                self.deck.dashboard.gesture = None;
                self.deck.dashboard.moving = false;
                if let Err(error) = result {
                    self.deck.diagnostic(&error);
                }
            }
            Action::WakeScreens =>
            {
                #[cfg(target_os = "macos")]
                if !self.deck.dashboard.board.screens_awake {
                    if Platform::screens_awake() == Some(true) {
                        self.deck_screens_awake(true);
                    } else if let Some(platform) = &self.platform
                        && let Err(error) = platform.wake_screens()
                    {
                        self.deck.diagnostic(&error);
                    }
                }
            }
            Action::Brightness(_) => unreachable!(),
        }
    }
    #[cfg(target_os = "macos")]
    pub(super) fn deck_screens_awake(&mut self, awake: bool) {
        self.deck.dashboard.screens_awake(awake);
        self.sync_deck();
        if let Some(worker) = &self.deck.worker {
            worker.submit(self.deck.dashboard.board.clone());
        }
    }
    pub(super) fn tick_deck(&mut self) {
        let config = (
            self.config().stream_deck,
            self.config().stream_deck_serial.clone(),
        );
        if self.deck.last_config.as_ref() != Some(&config) {
            self.deck.stop();
            self.deck.last_config = Some(config.clone());
            if !self.deck.busy() {
                self.deck.stopping = false;
            }
            if config.0 {
                self.deck.next_check =
                    Some(discovery_deadline(Instant::now(), self.deck.last_check));
            }
        }
        self.deck.wake_pending.store(false, Ordering::Release);
        let actions = self
            .deck
            .actions
            .as_ref()
            .map(|rx| rx.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        if self.deck.overflow.swap(false, Ordering::AcqRel) {
            self.deck
                .diagnostic("input queue overflow; reconnecting to reset held keys");
            self.deck.stop();
        }
        for action in actions {
            self.deck_action(action);
        }
        self.sync_deck();
        let now = Instant::now();
        if self.deck.worker.as_ref().is_some_and(Worker::is_finished) {
            self.deck.connected = false;
            if let Err(error) = self.deck.worker.take().unwrap().join() {
                self.deck.diagnostic(&error);
            }
            self.deck.actions = None;
            self.deck.dashboard.gesture = None;
            self.deck.dashboard.moving = false;
            self.deck.stopping = false;
            self.deck.next_check = config.0.then_some(now + Duration::from_secs(1));
        }
        if self
            .deck
            .discovery
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            let result = self
                .deck
                .discovery
                .take()
                .unwrap()
                .join()
                .unwrap_or_else(|_| Err("discovery panicked".into()));
            self.deck.next_check = config.0.then_some(now + Duration::from_secs(1));
            let cancelled = self.deck.stopping;
            self.deck.stopping = false;
            match result {
                Ok(Some(candidate)) if config.0 && !cancelled && self.deck.worker.is_none() => {
                    let (tx, rx) = mpsc::sync_channel(64);
                    let pending = Arc::clone(&self.deck.wake_pending);
                    let overflow = Arc::clone(&self.deck.overflow);
                    let proxy = self.proxy.clone();
                    match Worker::spawn(
                        candidate,
                        self.deck.dashboard.board.clone(),
                        move |action| {
                            if tx.try_send(action).is_err() {
                                overflow.store(true, Ordering::Release);
                            }
                            if !pending.swap(true, Ordering::AcqRel) {
                                let _ = proxy.send_event(Event::DeckWake);
                            }
                        },
                    ) {
                        Ok(worker) => {
                            self.deck.worker = Some(worker);
                            self.deck.actions = Some(rx);
                            self.deck.next_check = None;
                        }
                        Err(error) => self.deck.diagnostic(&error.to_string()),
                    }
                }
                Err(error) if config.0 && !cancelled => self.deck.diagnostic(&error),
                _ => {}
            }
        }
        if let Some(platform) = &mut self.platform
            && let Err(error) = platform.set_stream_deck_connected(self.deck.commands_available())
        {
            self.deck.diagnostic(&error);
        }
        if !config.0 {
            self.deck.next_check = None;
            return;
        }
        if let Some(worker) = &self.deck.worker {
            if !self.deck.stopping {
                worker.submit(self.deck.dashboard.board.clone());
            }
        } else if self.deck.discovery.is_none() && self.deck.next_check.is_some_and(|at| at <= now)
        {
            self.deck.stopping = false;
            let serial = config.1;
            let proxy = self.proxy.clone();
            self.deck.next_check = Some(now + Duration::from_secs(1));
            self.deck.last_check = Some(now);
            match std::thread::Builder::new()
                .name("rustty-deck-discovery".into())
                .spawn(move || {
                    let result = deck_device::discover(serial.as_deref());
                    let _ = proxy.send_event(Event::DeckWake);
                    result
                }) {
                Ok(discovery) => self.deck.discovery = Some(discovery),
                Err(error) => self.deck.diagnostic(&error.to_string()),
            }
        }
    }
}

/// Disposable native smoke fixture: reports are injected, USB remains disabled.
pub(super) fn check_native_deck(app: &mut App, event_loop: &ActiveEventLoop) -> Result<()> {
    use rustty::vt::agent::{Event as AgentEvent, Snapshot, State};
    let saved = app.workspace.clone();
    let screens_awake = app.deck.dashboard.board.screens_awake;
    // The offscreen fixture must also work while the real displays sleep.
    app.deck.dashboard.screens_awake(true);
    let one = app.add_window(false);
    let two = app.add_window(false);
    let quick = app.add_window(true);
    app.reconcile(event_loop);
    let first = app.focused(one).ok_or("missing test pane")?;
    let second = app.focused(two).ok_or("missing second test pane")?;
    let tab = app.workspace.id();
    let hidden = app.workspace.id();
    let one_index = app.index(one).unwrap();
    app.workspace.windows[one_index]
        .tabs
        .push(Tab::new(tab, hidden, std::env::temp_dir()));
    app.reconcile(event_loop);
    let snapshot = |state, turn| Snapshot {
        state,
        label: Some("same project".into()),
        thread_id: Some("thread-A".into()),
        turn_id: turn,
    };
    for (generation, id) in [first, second, hidden].into_iter().enumerate() {
        app.panes.get_mut(&id).unwrap().agent.apply(
            AgentEvent::Begin(snapshot(State::NeedsInput, None)),
            generation as u64 + 1,
            false,
        );
    }
    app.sync_deck();
    assert_eq!(app.deck.dashboard.board.counts[0], 3);
    let tile = |app: &App, pane| {
        app.deck
            .dashboard
            .board
            .tiles
            .iter()
            .find(|tile| tile.capture.assignment == Some(pane))
            .unwrap()
            .clone()
    };
    let local = tile(app, hidden);
    assert!(!local.remote);
    for source in [
        tmuxx_host::Source::Local("unused.native".into()),
        tmuxx_host::Source::Ssh("unused".into()),
    ] {
        let mut entry = tmuxx_host::Attachment::new(source, None, false);
        entry.mapping.panes.insert(hidden, 7);
        app.tmuxx.push(entry);
        app.sync_deck();
        let mut marked = tile(app, hidden);
        assert!(marked.remote);
        assert!(!tile(app, first).remote);
        marked.remote = false;
        assert_eq!(marked, local);
        app.tmuxx.pop();
    }
    app.sync_deck();
    assert_eq!(tile(app, hidden), local);
    let target = app
        .deck
        .dashboard
        .board
        .tiles
        .iter()
        .find(|t| t.capture.assignment == Some(hidden))
        .unwrap()
        .capture;
    let input_before = app.panes[&hidden].input_bytes;
    assert!(
        app.panes[&hidden]
            .agent
            .view(hidden, "", &app.panes[&hidden].activity)
            .unwrap()
            .unseen
    );
    let native = app
        .windows
        .iter()
        .find_map(|(key, host)| (host.id == one).then_some(*key))
        .unwrap();
    if !app.background_smoke {
        app.windows[&native].window.set_minimized(true);
    }
    app.reveal_pane(hidden);
    assert!(
        !app.panes[&hidden]
            .agent
            .view(hidden, "", &app.panes[&hidden].activity)
            .unwrap()
            .unseen
    );
    app.panes.get_mut(&hidden).unwrap().agent.apply(
        AgentEvent::Update(snapshot(State::NeedsInput, None)),
        3,
        false,
    );
    app.sync_deck();
    assert!(
        !app.panes[&hidden]
            .agent
            .view(hidden, "", &app.panes[&hidden].activity)
            .unwrap()
            .unseen
    );
    assert_eq!(app.focused(one), Some(hidden));
    assert_eq!(app.workspace.windows[app.index(one).unwrap()].active_tab, 1);
    assert!(app.windows[&native].visible);
    assert!(app.windows[&native].peek.is_none());
    assert_eq!(app.panes[&hidden].input_bytes, input_before);
    assert_eq!(
        app.panes[&hidden]
            .agent
            .view(hidden, "", &app.panes[&hidden].activity)
            .unwrap()
            .state,
        State::NeedsInput
    );
    app.reveal_pane(second);
    assert_eq!(app.active, Some(two));
    let quick_pane = app.focused(quick).unwrap();
    app.reveal_pane(quick_pane);
    assert!(
        app.windows
            .values()
            .find(|host| host.id == quick)
            .unwrap()
            .visible
    );
    app.panes.get_mut(&hidden).unwrap().agent.apply(
        AgentEvent::Update(snapshot(State::Done, Some("turn-1".into()))),
        3,
        false,
    );
    app.reveal_pane(hidden);
    assert_eq!(
        app.panes[&hidden]
            .agent
            .view(hidden, "", &app.panes[&hidden].activity)
            .unwrap()
            .state,
        State::Idle
    );
    app.workspace.close_pane(hidden);
    app.reconcile(event_loop);
    app.sync_deck();
    assert!(!app.deck.dashboard.board.valid(target));
    assert!(
        app.panes
            .get(&hidden)
            .is_none_or(|p| p.agent.view(hidden, "", &p.activity).is_none())
    );
    // Simulate readiness without starting discovery or owning the physical device.
    use config::StreamDeckAction;
    let enabled = app.loaded.config.stream_deck;
    let stopping = app.deck.stopping;
    app.loaded.config.stream_deck = true;
    app.deck.stopping = false;
    assert!(!app.deck_command(StreamDeckAction::PullToFirst));
    app.deck_action(Action::Connected);
    for (id, state) in [(first, State::Idle), (second, State::Working)] {
        app.panes.get_mut(&id).unwrap().agent.apply(
            AgentEvent::Update(snapshot(state, None)),
            0,
            false,
        );
    }
    let input_before = [
        app.panes[&first].input_bytes,
        app.panes[&second].input_bytes,
    ];
    app.workspace.deck_positions = vec![Some(first)];
    app.workspace.deck_positions.resize(9, None);
    app.workspace.deck_positions.push(Some(second));
    app.sync_deck();
    assert!(app.deck_command(StreamDeckAction::NextPage));
    assert_eq!(app.deck.dashboard.board.page, 1);
    assert!(app.platform_action(
        event_loop,
        config::Action::StreamDeck(StreamDeckAction::PullToFirst),
        false,
    ));
    assert_eq!(app.workspace.deck_positions, [Some(first), Some(second)]);
    assert_eq!(app.deck.dashboard.board.page, 0);
    let queued = Action::PullToFirst {
        generation: app.deck.dashboard.board.generation,
        mode_generation: app.deck.dashboard.board.mode_generation,
    };
    assert!(app.deck_command(StreamDeckAction::PullToFirst));
    let mode = app.deck.dashboard.board.mode_generation;
    app.deck_action(queued);
    assert_eq!(app.deck.dashboard.board.mode_generation, mode);
    app.deck_action(Action::PullToFirst {
        generation: app.deck.dashboard.board.generation,
        mode_generation: mode,
    });
    assert_eq!(app.deck.dashboard.board.mode_generation, mode + 1);
    app.reveal_pane(first);
    let key = app
        .windows
        .iter()
        .find_map(|(key, host)| (host.id == two).then_some(*key))
        .unwrap();
    let mut host = app.windows.remove(&key).unwrap();
    assert!(app.action(
        event_loop,
        &mut host,
        config::Action::StreamDeck(StreamDeckAction::Working),
        false,
    ));
    app.windows.insert(key, host);
    assert_eq!(app.active, Some(two));
    assert_eq!(app.focused(two), Some(second));
    for scenario in 0..3 {
        if scenario == 2 {
            let window = app.workspace.windows.remove(app.index(two).unwrap());
            let index = app.index(one).unwrap();
            app.workspace.windows[index].tabs.extend(window.tabs);
            app.reconcile(event_loop);
        }
        app.reveal_pane(first);
        for id in [first, second] {
            app.panes.get_mut(&id).unwrap().title = "keep until reset".into();
        }
        let mut binding = config::KeyBinding::parse(if scenario == 1 {
            "global:ctrl+alt+p=stream_deck:working"
        } else {
            "ctrl+alt+p=stream_deck:working"
        })?;
        binding.actions.push(config::Action::Reset);
        if scenario == 1 {
            app.platform_binding(event_loop, binding);
        } else {
            let mut host = app.windows.remove(&native).unwrap();
            assert!(app.host_binding(event_loop, &mut host, binding));
            app.windows.insert(native, host);
        }
        assert_eq!(app.panes[&first].title, "keep until reset");
        assert!(app.panes[&second].title.is_empty());
        assert_eq!(app.focused(app.active.unwrap()), Some(second));
    }
    assert_eq!(
        [
            app.panes[&first].input_bytes,
            app.panes[&second].input_bytes
        ],
        input_before
    );
    // Progress-only panes use aggregate reveal without registering an agent tile.
    use rustty::vt::Effect;
    for pane in app.panes.values_mut() {
        pane.agent.clear();
        pane.deck_progress.clear();
    }
    let positions = app.workspace.deck_positions.clone();
    app.panes.get_mut(&second).unwrap().deck_progress.apply(
        &Effect::Progress {
            state: 3,
            value: None,
        },
        true,
    );
    app.reveal_pane(first);
    app.sync_deck();
    assert!(!app.deck.dashboard.board.no_sessions);
    assert_eq!(app.deck.dashboard.board.counts, [0, 1, 0, 0]);
    assert!(
        app.deck
            .dashboard
            .board
            .tiles
            .iter()
            .all(|t| t.state.is_none())
    );
    assert!(app.deck_command(StreamDeckAction::Working));
    assert_eq!(app.focused(app.active.unwrap()), Some(second));
    app.reveal_pane(first);
    let report = &mut app.panes.get_mut(&second).unwrap().deck_progress;
    report.expire(report.deadline().unwrap());
    app.sync_deck();
    assert_eq!(app.deck.dashboard.board.counts, [0; 4]);
    assert!(app.deck.dashboard.board.no_sessions);
    assert!(app.deck.dashboard.cycle(0, Some(first)).is_none());
    assert!(app.deck_command(StreamDeckAction::NeedsInput));
    assert_eq!(app.focused(app.active.unwrap()), Some(first));
    app.panes
        .get_mut(&second)
        .unwrap()
        .deck_progress
        .apply(&Effect::CommandEnd { exit_code: Some(1) }, true);
    app.sync_deck();
    assert_eq!(app.deck.dashboard.board.counts, [0; 4]);
    assert!(app.deck_command(StreamDeckAction::NeedsInput));
    assert_eq!(app.focused(app.active.unwrap()), Some(first));
    assert_eq!(app.panes[&second].deck_progress.state(), None);
    app.reveal_pane(first);
    let report = &mut app.panes.get_mut(&second).unwrap().deck_progress;
    report.apply(
        &Effect::Progress {
            state: 1,
            value: Some(100),
        },
        true,
    );
    report.apply(&Effect::CommandEnd { exit_code: Some(0) }, true);
    app.sync_deck();
    assert_eq!(app.deck.dashboard.board.counts, [0, 0, 1, 0]);
    assert!(app.deck_command(StreamDeckAction::Done));
    assert_eq!(app.focused(app.active.unwrap()), Some(second));
    assert_eq!(app.panes[&second].deck_progress.state(), None);
    assert_eq!(app.workspace.deck_positions, positions);
    assert_eq!(app.panes[&second].input_bytes, input_before[1]);

    // Qualifying shell commands use the same tiles and physical focus action as agents.
    let notification_policy = app.loaded.config.notify_on_command_finish;
    let notification_after = app.loaded.config.notify_on_command_finish_after;
    let notification_action = app.loaded.config.notify_on_command_finish_action;
    let policy = config::NotifyOnCommandFinish::Always;
    let after = Duration::from_secs(30);
    app.loaded.config.notify_on_command_finish = policy;
    app.loaded.config.notify_on_command_finish_after = after;
    app.loaded.config.notify_on_command_finish_action.notify = false;
    app.reveal_pane(first);
    let now = Instant::now();
    app.panes
        .get_mut(&second)
        .unwrap()
        .deck_command
        .start(now, 100);
    app.update_deck_commands();
    app.sync_deck();
    assert!(app.deck.dashboard.board.no_sessions);
    app.update_deck_commands();
    app.sync_deck();
    let tile = |app: &App| {
        app.deck
            .dashboard
            .board
            .tiles
            .iter()
            .find(|tile| tile.capture.assignment == Some(second))
            .unwrap()
            .clone()
    };
    assert!(app.deck.dashboard.board.no_sessions);
    assert!(!app.panes[&second].deck_command.is_registered());
    for (generation, exit_code, expected, group) in [
        (101, Some(0), State::Done, 2),
        (102, Some(1), State::Error, 0),
        (103, None, State::NeedsInput, 0),
    ] {
        let report = &mut app.panes.get_mut(&second).unwrap().deck_command;
        report.start(now, generation);
        report.finish(now + after, policy, after, false, exit_code);
        app.sync_deck();
        let completed = tile(app);
        assert_eq!(completed.state, Some(expected));
        assert_eq!(app.deck.dashboard.board.counts[group], 1);
        assert!(completed.unseen);
        app.deck_action(Action::Focus(completed.capture));
        app.sync_deck();
        assert_eq!(app.focused(app.active.unwrap()), Some(second));
        assert!(app.deck.dashboard.board.no_sessions);
        assert!(!app.panes[&second].deck_command.is_registered());
        app.reveal_pane(first);
    }
    let report = &mut app.panes.get_mut(&second).unwrap().deck_command;
    report.start(now, 104);
    report.finish(now + after, policy, after, false, Some(0));
    app.sync_deck();
    let old_completion = tile(app).capture;
    let report = &mut app.panes.get_mut(&second).unwrap().deck_command;
    report.start(now, 105);
    report.finish(
        now + Duration::from_millis(1),
        policy,
        after,
        false,
        Some(0),
    );
    app.sync_deck();
    assert!(app.deck.dashboard.board.no_sessions);
    app.deck_action(Action::Focus(old_completion));
    assert_eq!(app.focused(app.active.unwrap()), Some(first));

    app.panes
        .get_mut(&second)
        .unwrap()
        .deck_command
        .start(now, 106);
    app.panes.get_mut(&second).unwrap().deck_command.finish(
        now + after,
        policy,
        after,
        false,
        Some(0),
    );
    app.update_deck_commands();
    app.sync_deck();
    let command_capture = tile(app).capture;
    app.panes.get_mut(&second).unwrap().agent.apply(
        AgentEvent::Begin(snapshot(State::NeedsInput, None)),
        107,
        true,
    );
    app.sync_deck();
    assert_eq!(tile(app).state, Some(State::NeedsInput));
    assert_eq!(tile(app).capture.target.unwrap().generation, 107);
    assert_eq!(app.deck.dashboard.board.counts, [1, 0, 0, 0]);
    app.deck_action(Action::Focus(command_capture));
    assert_eq!(app.focused(app.active.unwrap()), Some(first));
    app.panes.get_mut(&second).unwrap().agent.clear();
    app.panes.get_mut(&second).unwrap().deck_command.clear();

    // Simulate focus in the hidden native window without asking the OS to activate it.
    let focused_hosts: Vec<_> = app
        .windows
        .iter()
        .map(|(&id, host)| (id, host.focused))
        .collect();
    for (&id, host) in &mut app.windows {
        host.focused = id == native;
    }
    let unfocused = config::NotifyOnCommandFinish::Unfocused;
    app.loaded.config.notify_on_command_finish = unfocused;
    app.reveal_pane(second);
    app.panes
        .get_mut(&second)
        .unwrap()
        .deck_command
        .start(now - after, 108);
    app.update_deck_commands();
    app.sync_deck();
    assert!(app.notification_focused(second, None));
    assert!(app.deck.dashboard.board.no_sessions);
    app.reveal_pane(first);
    app.sync_deck();
    assert!(app.deck.dashboard.board.no_sessions);
    app.reveal_pane(second);
    app.panes.get_mut(&second).unwrap().deck_command.finish(
        Instant::now(),
        unfocused,
        after,
        true,
        Some(0),
    );
    app.sync_deck();
    assert!(app.deck.dashboard.board.no_sessions);
    for (id, focused) in focused_hosts {
        app.windows.get_mut(&id).unwrap().focused = focused;
    }
    app.loaded.config.notify_on_command_finish = policy;
    app.reveal_pane(first);

    #[cfg(unix)]
    {
        // Exercise the PTY decoder and drain path with an owned, input-free child.
        app.loaded.config.notify_on_command_finish_after = Duration::from_millis(20);
        let session = Session::spawn(
            app.config(),
            SessionOptions {
                command: Some(config::Command::Direct(vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    r#"printf '\033]133;A\007$ \033]133;B\007tix\r\n\033]133;C\007'
sleep 0.2
printf '\033]133;D;0\007'; sleep 0.2
printf '\033]133;C\007'; sleep 0.2; printf '\033]133;D;0\007'
sleep 0.2
printf '\033]133;C\007'; printf '%s' "$1"; printf '\033]9;4;3\007'
sleep 0.2
printf '%s' "$2"; printf '\033]9;4;2\007'
sleep 0.2
printf '%s' "$3"; printf '\033]9;4;3\007'
sleep 0.2
printf '\033]133;D;0\007'
sleep 0.2
printf '\033]133;C;cmdline_url=tix\007\033]9;4;3\007'
sleep 0.2
printf '\033]9;✅ merged\007\033]9;4;0\007'
sleep 0.2
printf '\033]133;D;1\007'
sleep 0.2
printf '\033]133;C\007\033]777;notify;Finished;notification-only\007'
sleep 0.2
printf '\033]133;D;0\007'; exec sleep 120"#
                        .into(),
                    "review-status-smoke".into(),
                    String::from_utf8(AgentEvent::Begin(snapshot(State::Working, None)).frame()?)?,
                    String::from_utf8(
                        AgentEvent::Update(snapshot(State::Done, Some("review".into()))).frame()?,
                    )?,
                    String::from_utf8(AgentEvent::End.frame()?)?,
                ])),
                ..SessionOptions::default()
            },
            Arc::new(|| {}),
        )?;
        let previous = std::mem::replace(&mut app.panes.get_mut(&second).unwrap().session, session);
        previous.close();
        app.closing.push(previous);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_excluded = false;
        loop {
            app.drain(event_loop, second);
            app.sync_deck();
            assert!(app.deck.dashboard.board.no_sessions);
            saw_excluded |= app.panes[&second].running.is_some();
            if saw_excluded && app.panes[&second].running.is_none() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "excluded command did not complete"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!app.panes[&second].unseen);
        let mut saw_running = false;
        loop {
            app.drain(event_loop, second);
            app.sync_deck();
            if app.panes[&second].deck_command.is_registered() {
                assert_eq!(tile(app).state, Some(State::Done));
                break;
            }
            saw_running |= app.panes[&second].running.is_some();
            assert!(app.deck.dashboard.board.no_sessions);
            assert!(
                Instant::now() < deadline,
                "OSC 133 command did not complete"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(saw_running, "quiet command did not start");
        assert!(app.panes[&second].running.is_none());
        assert_eq!(app.panes[&second].input_bytes, input_before[1]);
        app.deck_action(Action::Focus(tile(app).capture));
        app.sync_deck();
        assert!(app.deck.dashboard.board.no_sessions);
        app.reveal_pane(first);
        let mut saw_review_working = false;
        let mut saw_review_done = false;
        loop {
            app.drain(event_loop, second);
            app.sync_deck();
            let state = tile(app).state;
            saw_review_working |= state == Some(State::Working);
            if state == Some(State::Done) {
                saw_review_done = true;
                assert_eq!(app.deck.dashboard.board.counts, [0, 0, 1, 0]);
                app.deck_action(Action::Focus(tile(app).capture));
                app.sync_deck();
                assert_eq!(tile(app).state, Some(State::Idle));
            }
            if saw_review_done && !app.panes[&second].agent.is_registered() {
                assert!(app.deck.dashboard.board.no_sessions);
                if app.panes[&second].running.is_none() {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "review status lifecycle did not complete"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(saw_review_working);
        assert!(!app.panes[&second].unseen);
        // Observe explicit notification effects without posting real desktop alerts.
        let platform = app.platform.take();
        app.reveal_pane(first);
        let mut saw_progress = false;
        let mut saw_result = false;
        loop {
            app.drain(event_loop, second);
            app.sync_deck();
            saw_progress |= app.panes[&second].deck_progress.state() == Some(State::Working);
            assert!(!app.panes[&second].deck_command.is_registered());
            if app.panes[&second].deck_progress.state() == Some(State::Done) {
                saw_result = true;
                assert_eq!(app.deck.dashboard.board.counts, [0, 0, 1, 0]);
                assert!(app.deck_command(StreamDeckAction::Done));
            }
            if saw_result && app.panes[&second].running.is_none() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "progress command did not complete"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(saw_progress);
        assert!(app.deck.dashboard.board.no_sessions);
        assert!(!app.panes[&second].unseen);
        app.reveal_pane(first);
        let mut saw_notification = false;
        loop {
            app.drain(event_loop, second);
            app.sync_deck();
            if app.panes[&second].unseen {
                saw_notification = true;
                app.reveal_pane(second);
            }
            assert!(app.deck.dashboard.board.no_sessions);
            if saw_notification && app.panes[&second].running.is_none() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "notification command did not complete"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!app.panes[&second].unseen);
        app.platform = platform;
    }
    app.loaded.config.notify_on_command_finish = notification_policy;
    app.loaded.config.notify_on_command_finish_after = notification_after;
    app.loaded.config.notify_on_command_finish_action = notification_action;
    app.panes.get_mut(&second).unwrap().deck_command.clear();
    app.deck_action(Action::Disconnected(Ok(())));
    assert!(!app.deck_command(StreamDeckAction::PullToFirst));
    app.loaded.config.stream_deck = enabled;
    app.deck.stopping = stopping;
    // Saving `never` leaves even already-existing workspace bytes untouched.
    let before = std::fs::read(&app.state_path).ok();
    let policy = app.loaded.config.window_save_state;
    app.loaded.config.window_save_state = config::WindowSaveState::Never;
    app.save();
    assert_eq!(std::fs::read(&app.state_path).ok(), before);
    app.loaded.config.window_save_state = policy;
    app.workspace = saved;
    app.deck.dashboard.screens_awake(screens_awake);
    app.reconcile(event_loop);
    app.sync_deck();
    assert!(app.deck.worker.is_none());
    assert!(app.deck.discovery.is_none());
    eprintln!(
        "Native Stream Deck smoke: exact panes across windows/tabs, completion acknowledgement, qualifying command tiles and agent precedence, progress-only Working/Attention/Done cycling and timeout removal, menu/shortcut chains, pull-to-first, stale control rejection, disconnected gating, input preservation, pane removal and save-state-never passed; no USB used; native minimization and quick animation {}",
        if app.background_smoke {
            "skipped"
        } else {
            "checked"
        },
    );
    Ok(())
}

fn discovery_deadline(now: Instant, previous: Option<Instant>) -> Instant {
    previous.map_or(now, |at| now.max(at + Duration::from_secs(1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_labels_follow_local_project_task_names() {
        use std::path::Path;
        for (directory, expected) in [
            ("/work/gitoxide.foo-bar", "foo-bar"),
            ("/work/gitoxide.foo.bar", "foo.bar"),
            ("/work/gitoxide", "gitoxide"),
            ("/work/.config", ".config"),
            ("/work/gitoxide.", "gitoxide."),
            ("/work/répo.修复🔧", "修复🔧"),
            ("", ""),
            ("/", ""),
        ] {
            assert_eq!(task_label("", Path::new(directory)), expected);
        }
        let nested = Path::new("/work/gitoxide.foo-bar/crates/parser");
        for title in ["gitoxide.foo-bar", "/work/gitoxide.foo-bar"] {
            assert_eq!(task_label(title, nested), "foo-bar");
        }
        for title in [
            "⠋ Working",
            "[ ! ] Action Required | Codex",
            "opaque-thread-id",
            "repo.manual",
            "repo.tab",
        ] {
            assert_eq!(task_label(title, nested), "parser");
            assert_eq!(
                task_label(title, Path::new("/work/gitoxide.foo-bar")),
                "foo-bar"
            );
        }
        assert_eq!(
            task_label("", Path::new("/work/ \nrepo.修复\u{1b}🔧\t ")),
            "修复🔧"
        );
        assert_eq!(task_label("\n\t", Path::new("/work/task")), "task");
    }
    #[test]
    fn absent_device_has_no_resident_worker_and_reloads_cannot_accelerate_discovery() {
        let now = Instant::now();
        let mut connection = Connection::default();
        assert!(!connection.commands_available());
        connection.connected = true;
        assert!(connection.commands_available());
        assert!(!connection.busy());
        assert!(connection.deadline().is_none());
        assert_eq!(discovery_deadline(now, None), now);
        assert_eq!(
            discovery_deadline(now + Duration::from_millis(999), Some(now)),
            now + Duration::from_secs(1)
        );
        assert_eq!(
            discovery_deadline(now + Duration::from_secs(2), Some(now)),
            now + Duration::from_secs(2)
        );
        connection.next_check = Some(now);
        connection.stop();
        assert!(!connection.commands_available());
        assert!(connection.deadline().is_none());
        // Completed discovery is reaped without starting a USB/rendering worker.
        connection.discovery = Some(std::thread::spawn(|| Ok(None)));
        while !connection.discovery.as_ref().unwrap().is_finished() {
            std::thread::yield_now();
        }
        connection.reap();
        assert!(!connection.busy());
    }
}
