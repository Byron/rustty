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

pub(super) struct Connection {
    pub report_generation: u64,
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
    fn deck_focused(&self) -> Option<Id> {
        self.windows
            .values()
            .find(|host| host.visible && host.focused && !host.occluded && host.peek.is_none())
            .and_then(|host| self.focused(host.id))
    }
    fn sync_deck(&mut self) {
        let focused = self.deck_focused();
        if let Some(pane) = focused.and_then(|id| self.panes.get_mut(&id)) {
            pane.agent.acknowledge();
        }
        let reports = self
            .workspace
            .windows
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|tab| {
                tab.root
                    .panes()
                    .into_iter()
                    .filter_map(|id| {
                        let pane = self.panes.get(&id)?;
                        let fallback = pane
                            .title_override
                            .as_deref()
                            .filter(|s| !s.is_empty())
                            .or(tab.title.as_deref().filter(|s| !s.is_empty()))
                            .map(str::to_owned)
                            .unwrap_or_else(|| {
                                directory_name(&pane.cwd).unwrap_or_else(|| "Terminal".into())
                            });
                        pane.agent
                            .view(id, &fallback, &pane.activity)
                            .map(|view| (id, view))
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        if self
            .deck
            .dashboard
            .update(&mut self.workspace, &reports, focused)
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
                        .agent
                        .view(target.pane, "", &pane.activity)
                        .is_some_and(|r| r.target == target)
            })
    }
    /// Notification and physical dashboard actions share the exact native reveal path.
    pub(super) fn reveal_pane(&mut self, pane: Id) {
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
        let Some(native) = self
            .windows
            .iter()
            .find_map(|(native, host)| (host.id == id).then_some(*native))
        else {
            return;
        };
        let index = self.index(id).unwrap();
        self.workspace.windows[index].active_tab = tab;
        self.focus_pane(id, pane);
        if let Some(mut host) = self.windows.remove(&native) {
            host.peek = None;
            if quick {
                self.quick_visible(&mut host, true, false);
            } else {
                host.visible = true;
                host.window.set_visible(true);
                host.window.set_minimized(false);
                host.window.focus_window();
            }
            host.repaint();
            self.windows.insert(native, host);
        }
        self.active = Some(id);
        if let Some(state) = self.panes.get_mut(&pane) {
            state.agent.acknowledge();
        }
        self.sync_host_state();
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
        match action {
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
                    let focused = self.deck_focused();
                    if let Some(capture) = self.deck.dashboard.cycle(group, focused)
                        && let Some(target) = capture.target.filter(|t| self.live_deck_target(*t))
                    {
                        self.reveal_pane(target.pane);
                    }
                }
            }
            Action::Page { generation } => {
                if generation == self.deck.dashboard.board.mode_generation {
                    self.deck.dashboard.page();
                }
            }
            Action::Disconnected(result) => {
                self.deck.stopping = true;
                self.deck.dashboard.gesture = None;
                self.deck.dashboard.moving = false;
                if let Err(error) = result {
                    self.deck.diagnostic(&error);
                }
            }
            Action::Brightness(_) => unreachable!(),
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
    let native = app
        .windows
        .iter()
        .find_map(|(key, host)| (host.id == one).then_some(*key))
        .unwrap();
    app.windows[&native].window.set_minimized(true);
    app.reveal_pane(hidden);
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
    // Saving `never` leaves even already-existing workspace bytes untouched.
    let before = std::fs::read(&app.state_path).ok();
    let policy = app.loaded.config.window_save_state;
    app.loaded.config.window_save_state = config::WindowSaveState::Never;
    app.save();
    assert_eq!(std::fs::read(&app.state_path).ok(), before);
    app.loaded.config.window_save_state = policy;
    app.workspace = saved;
    app.reconcile(event_loop);
    app.sync_deck();
    assert!(app.deck.worker.is_none());
    assert!(app.deck.discovery.is_none());
    eprintln!(
        "Native Stream Deck smoke: exact panes across windows/tabs, minimized reveal, quick animation request, completion acknowledgement, input preservation, pane removal and save-state-never passed; no USB used"
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
    fn absent_device_has_no_resident_worker_and_reloads_cannot_accelerate_discovery() {
        let now = Instant::now();
        let mut connection = Connection::default();
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
