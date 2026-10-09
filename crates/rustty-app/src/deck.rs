//! Pane-local reports and stable dashboard positions. No terminal input is sent here.
use crate::{
    presentation::Activity,
    workspace::{Id, MAX_DECK_SLOTS, Workspace},
};
use rustty::vt::agent::{Event, Snapshot, State};
use std::collections::{HashMap, VecDeque};

pub const PAGE_SIZE: usize = 9;
pub const ACK_HISTORY: usize = 64;
pub const GROUPS: [State; 4] = [State::NeedsInput, State::Working, State::Done, State::Idle];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub pane: Id,
    pub generation: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capture {
    pub slot: usize,
    pub assignment: Option<Id>,
    pub target: Option<Target>,
    pub layout_generation: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tile {
    pub capture: Capture,
    pub label: String,
    pub state: Option<State>,
    pub focused: bool,
    pub reserved: bool,
    pub unseen: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Board {
    pub generation: u64,
    pub mode_generation: u64,
    pub tiles: Vec<Tile>,
    pub page: usize,
    pub counts: [usize; 4],
    pub no_sessions: bool,
    pub brightness: u8,
    pub screens_awake: bool,
}
impl Default for Board {
    fn default() -> Self {
        Self {
            generation: 0,
            mode_generation: 0,
            tiles: Vec::new(),
            page: 0,
            counts: [0; 4],
            no_sessions: true,
            brightness: 100,
            screens_awake: true,
        }
    }
}
impl Board {
    pub fn pages(&self) -> usize {
        self.tiles.len().div_ceil(PAGE_SIZE).max(1)
    }
    pub fn tile(&self, slot: usize) -> Tile {
        self.tiles.get(slot).cloned().unwrap_or(Tile {
            capture: Capture {
                slot,
                assignment: None,
                target: None,
                layout_generation: self.generation,
            },
            label: String::new(),
            state: None,
            focused: false,
            reserved: false,
            unseen: false,
        })
    }
    pub fn valid(&self, capture: Capture) -> bool {
        capture.layout_generation == self.generation && self.tile(capture.slot).capture == capture
    }
}

/// Exists only in the owning live pane, never in a VT or workspace snapshot.
#[derive(Default)]
pub struct Report {
    snapshot: Option<Snapshot>,
    generation: u64,
    enclosing_command: bool,
    attention: VecDeque<Attention>,
}
struct Attention {
    thread: Option<String>,
    state: State,
    turn: Option<String>,
    completed: Option<String>,
    unseen: bool,
}
impl Report {
    pub fn is_registered(&self) -> bool {
        self.snapshot.is_some()
    }
    pub fn apply(&mut self, event: Event, generation: u64, command_running: bool) {
        match event {
            Event::Begin(snapshot) => {
                self.clear();
                self.generation = generation;
                self.enclosing_command = command_running;
                self.observe(&snapshot);
                self.snapshot = Some(snapshot);
            }
            Event::Update(snapshot) if self.snapshot.is_some() => {
                self.observe(&snapshot);
                self.snapshot = Some(snapshot);
            }
            Event::Update(_) => {}
            Event::End => self.clear(),
        }
    }
    pub fn clear(&mut self) {
        self.snapshot = None;
        self.attention.clear();
        self.enclosing_command = false;
    }
    pub fn command_ended(&mut self) {
        if self.enclosing_command {
            self.clear();
        }
    }
    fn observe(&mut self, snapshot: &Snapshot) {
        let previous = self
            .attention
            .iter()
            .position(|entry| entry.thread == snapshot.thread_id)
            .and_then(|index| self.attention.remove(index));
        let completed = previous.as_ref().and_then(|entry| entry.completed.clone());
        let same = previous.as_ref().is_some_and(|entry| {
            entry.state == snapshot.state
                && (snapshot.state != State::Done || entry.turn == snapshot.turn_id)
        });
        let unseen = match snapshot.state {
            State::Working => false,
            State::Done if completed.is_some() && completed == snapshot.turn_id => false,
            _ if same => previous.as_ref().is_some_and(|entry| entry.unseen),
            State::Idle => previous
                .as_ref()
                .is_some_and(|entry| entry.state != State::Done || entry.unseen),
            _ => true,
        };
        self.attention.push_back(Attention {
            thread: snapshot.thread_id.clone(),
            state: snapshot.state,
            turn: snapshot.turn_id.clone(),
            completed,
            unseen,
        });
        if self.attention.len() > ACK_HISTORY {
            self.attention.pop_front();
        }
    }
    pub fn acknowledge(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if let Some(entry) = self
            .attention
            .iter_mut()
            .find(|entry| entry.thread == snapshot.thread_id)
        {
            entry.unseen = false;
            if snapshot.state == State::Done {
                entry.completed.clone_from(&snapshot.turn_id);
            }
        }
    }
    pub fn view(&self, pane: Id, task_label: &str, _activity: &Activity) -> Option<PaneView> {
        let snapshot = self.snapshot.as_ref()?;
        let attention = self
            .attention
            .iter()
            .find(|entry| entry.thread == snapshot.thread_id);
        let acknowledged = snapshot.state == State::Done
            && snapshot.turn_id.is_some()
            && attention.is_some_and(|entry| entry.completed == snapshot.turn_id);
        let state = if acknowledged {
            State::Idle
        } else {
            snapshot.state
        };
        Some(PaneView {
            target: Target {
                pane,
                generation: self.generation,
            },
            state,
            label: task_label.to_owned(),
            unseen: attention.is_some_and(|entry| entry.unseen),
        })
    }
}
#[derive(Clone, Debug)]
pub struct PaneView {
    pub target: Target,
    pub state: State,
    pub label: String,
    pub unseen: bool,
}

#[derive(Default)]
pub struct Dashboard {
    pub board: Board,
    pub gesture: Option<Capture>,
    pub moving: bool,
    cursors: [Option<usize>; 4],
    positions: Vec<Option<Id>>,
}
impl Dashboard {
    pub fn screens_awake(&mut self, awake: bool) {
        if self.board.screens_awake != awake {
            self.board.screens_awake = awake;
            self.board.generation += 1;
            self.board.mode_generation += 1;
            self.gesture = None;
            self.moving = false;
        }
    }
    /// `reports` may contain retained panes; membership always comes from the workspace.
    pub fn update(
        &mut self,
        workspace: &mut Workspace,
        reports: &HashMap<Id, PaneView>,
        focused: Option<Id>,
    ) -> bool {
        let mut changed = workspace.reconcile_deck_positions();
        let live: Vec<_> = workspace
            .windows
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| t.root.panes())
            .collect();
        if self.gesture.is_some_and(|source| {
            source.target.is_none_or(|target| {
                !live.contains(&target.pane)
                    || reports.get(&target.pane).is_none_or(|r| r.target != target)
            })
        }) {
            self.gesture = None;
            self.moving = false;
        }
        if self.gesture.is_none() {
            for id in &live {
                if reports.contains_key(id) && !workspace.deck_positions.contains(&Some(*id)) {
                    let available = workspace
                        .deck_positions
                        .iter()
                        .position(Option::is_none)
                        .or_else(|| {
                            workspace.deck_positions.iter().position(|slot| {
                                slot.is_some_and(|pane| !reports.contains_key(&pane))
                            })
                        });
                    if let Some(slot) = available {
                        workspace.deck_positions[slot] = Some(*id);
                        changed = true;
                    } else if workspace.deck_positions.len() < MAX_DECK_SLOTS {
                        workspace.deck_positions.push(Some(*id));
                        changed = true;
                    }
                }
            }
        }
        let no_sessions = !live.iter().any(|id| reports.contains_key(id));
        if self.positions != workspace.deck_positions || self.board.no_sessions != no_sessions {
            self.board.generation += 1;
            self.positions.clone_from(&workspace.deck_positions);
        }
        if self.board.no_sessions != no_sessions {
            self.board.mode_generation += 1;
            self.gesture = None;
            self.moving = false;
        }
        self.board.no_sessions = no_sessions;
        self.board.counts = [0; 4];
        let mut names = HashMap::<&str, usize>::new();
        for id in &live {
            if let Some(report) = reports.get(id) {
                *names.entry(&report.label).or_default() += 1;
            }
        }
        self.board.tiles = workspace
            .deck_positions
            .iter()
            .enumerate()
            .map(|(slot, id)| {
                let report = id.and_then(|id| reports.get(&id));
                if let Some(report) = report {
                    for (i, state) in GROUPS.iter().enumerate() {
                        if *state == report.state {
                            self.board.counts[i] += 1;
                        }
                    }
                }
                let label = report.map_or_else(String::new, |r| {
                    if !r.label.is_empty() && names.get(r.label.as_str()).copied().unwrap_or(0) > 1
                    {
                        format!("{} ·{}", r.label, slot + 1)
                    } else {
                        r.label.clone()
                    }
                });
                Tile {
                    capture: Capture {
                        slot,
                        assignment: *id,
                        target: report.map(|r| r.target),
                        layout_generation: self.board.generation,
                    },
                    label,
                    state: report.map(|r| r.state),
                    focused: id.is_some() && *id == focused,
                    reserved: id.is_some() && report.is_none(),
                    unseen: report.is_some_and(|r| r.unseen),
                }
            })
            .collect();
        self.board.page = self.board.page.min(self.board.pages() - 1);
        changed
    }
    pub fn cycle(&mut self, group: usize, focused: Option<Id>) -> Option<Capture> {
        if self.moving {
            return None;
        }
        let state = *GROUPS.get(group)?;
        let members: Vec<_> = self
            .board
            .tiles
            .iter()
            .filter(|t| t.state == Some(state))
            .map(|t| t.capture)
            .collect();
        let cursor = members
            .iter()
            .find(|c| c.assignment == focused)
            .map(|c| c.slot)
            .or(self.cursors[group]);
        let next = cursor
            .and_then(|slot| members.iter().find(|c| c.slot > slot))
            .or_else(|| members.first())
            .copied()?;
        // Keep the slot cursor after Done acknowledgement removes its member.
        self.cursors[group] = Some(next.slot);
        self.board.page = next.slot / PAGE_SIZE;
        Some(next)
    }
    pub fn page(&mut self) {
        self.board.page = (self.board.page + 1) % self.board.pages();
    }
    pub fn swap(
        &mut self,
        workspace: &mut Workspace,
        source: Capture,
        destination: Capture,
    ) -> bool {
        let valid = self.moving
            && self.gesture == Some(source)
            && self.board.valid(source)
            && self.board.valid(destination)
            && source.target.is_some()
            && destination.slot < MAX_DECK_SLOTS;
        self.gesture = None;
        self.moving = false;
        if !valid || source.slot == destination.slot {
            return false;
        }
        let len = workspace.deck_positions.len().max(destination.slot + 1);
        workspace.deck_positions.resize(len, None);
        workspace.deck_positions.swap(source.slot, destination.slot);
        workspace.reconcile_deck_positions();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{Tab, WindowState};
    use std::path::PathBuf;
    fn setup(count: usize) -> (Workspace, HashMap<Id, PaneView>) {
        let mut workspace = Workspace::default();
        let mut reports = HashMap::new();
        for i in 0..count {
            let window = workspace.id();
            let tab = workspace.id();
            let pane = workspace.id();
            workspace.windows.push(WindowState {
                id: window,
                tabs: vec![Tab::new(tab, pane, PathBuf::from("/same"))],
                active_tab: 0,
                frame: [0.0, 0.0, 800.0, 600.0],
                quick: false,
            });
            reports.insert(
                pane,
                PaneView {
                    target: Target {
                        pane,
                        generation: 1,
                    },
                    label: "Review Δ".into(),
                    state: GROUPS[i % 4],
                    unseen: false,
                },
            );
        }
        (workspace, reports)
    }
    fn snapshot(state: State, thread: &str, turn: Option<&str>) -> Snapshot {
        Snapshot {
            state,
            label: None,
            thread_id: Some(thread.into()),
            turn_id: turn.map(str::to_owned),
        }
    }
    #[test]
    fn reporter_lifecycle_and_completion_acknowledgement() {
        let mut report = Report::default();
        let activity = Activity::default();
        let done = snapshot(State::Done, "A", Some("T1"));
        report.apply(Event::Update(done.clone()), 1, false);
        assert!(!report.is_registered());
        assert!(report.view(1, "pane", &activity).is_none());
        report.apply(Event::Begin(done.clone()), 2, true);
        assert!(report.is_registered());
        assert_eq!(
            report.view(1, "pane", &activity).unwrap().state,
            State::Done
        );
        report.acknowledge();
        report.apply(Event::Update(snapshot(State::Idle, "B", None)), 3, false);
        report.apply(Event::Update(done), 4, false);
        assert_eq!(
            report.view(1, "pane", &activity).unwrap().state,
            State::Idle
        );
        report.apply(
            Event::Update(snapshot(State::Done, "A", Some("T2"))),
            5,
            false,
        );
        assert_eq!(
            report.view(1, "pane", &activity).unwrap().state,
            State::Done
        );
        for state in [
            State::NeedsInput,
            State::Working,
            State::Idle,
            State::Error,
            State::Paused,
            State::Unknown,
        ] {
            report.apply(Event::Update(snapshot(state, "A", None)), 6, false);
            report.acknowledge();
            assert_eq!(report.view(1, "fallback", &activity).unwrap().state, state);
        }
        report.command_ended();
        assert!(report.view(1, "pane", &activity).is_none());
        report.apply(Event::Begin(snapshot(State::Idle, "A", None)), 7, false);
        report.command_ended();
        assert!(report.view(1, "pane", &activity).is_some());
        report.apply(Event::End, 8, false);
        assert!(!report.is_registered());
        assert!(report.view(1, "pane", &activity).is_none());
    }
    #[test]
    fn important_transitions_flash_until_seen_without_rearming_duplicates() {
        let mut report = Report::default();
        let activity = Activity::default();
        report.apply(Event::Begin(snapshot(State::Idle, "A", None)), 1, false);
        assert!(!report.view(1, "task", &activity).unwrap().unseen);
        for state in [
            State::NeedsInput,
            State::Idle,
            State::Error,
            State::Paused,
            State::Unknown,
        ] {
            report.apply(Event::Update(snapshot(State::Working, "A", None)), 1, false);
            assert!(!report.view(1, "task", &activity).unwrap().unseen);
            report.apply(Event::Update(snapshot(state, "A", None)), 1, false);
            let unseen = report.view(1, "task", &activity).unwrap();
            assert_eq!(unseen.state, state);
            assert!(unseen.unseen);
            report.apply(Event::Update(snapshot(state, "A", None)), 1, false);
            assert!(report.view(1, "task", &activity).unwrap().unseen);
            report.acknowledge();
            let mut duplicate = snapshot(state, "A", None);
            duplicate.label = Some("Updated display metadata".into());
            report.apply(Event::Update(duplicate), 1, false);
            let seen = report.view(1, "task", &activity).unwrap();
            assert_eq!(seen.state, state);
            assert_eq!(seen.target, unseen.target);
            assert!(!seen.unseen);
            report.apply(Event::Update(snapshot(State::Idle, "B", None)), 1, false);
            report.apply(Event::Update(snapshot(state, "A", None)), 1, false);
            assert!(!report.view(1, "task", &activity).unwrap().unseen);
        }
        report.apply(
            Event::Update(snapshot(State::Done, "A", Some("T1"))),
            1,
            false,
        );
        assert!(report.view(1, "task", &activity).unwrap().unseen);
        report.acknowledge();
        report.apply(Event::Update(snapshot(State::Idle, "A", None)), 1, false);
        assert!(!report.view(1, "task", &activity).unwrap().unseen);
        report.apply(Event::Update(snapshot(State::Idle, "B", None)), 1, false);
        report.apply(
            Event::Update(snapshot(State::Done, "A", Some("T1"))),
            1,
            false,
        );
        let seen = report.view(1, "task", &activity).unwrap();
        assert_eq!(seen.state, State::Idle);
        assert!(!seen.unseen);
        report.apply(
            Event::Update(snapshot(State::Done, "A", Some("T2"))),
            1,
            false,
        );
        assert!(report.view(1, "task", &activity).unwrap().unseen);
        report.apply(Event::End, 1, false);
        assert!(report.view(1, "task", &activity).is_none());
        report.apply(
            Event::Begin(snapshot(State::NeedsInput, "A", None)),
            2,
            false,
        );
        assert!(report.view(1, "task", &activity).unwrap().unseen);
    }
    #[test]
    fn explicit_unknown_survives_stale_attention_and_working_hints() {
        let mut report = Report::default();
        report.apply(Event::Begin(snapshot(State::Unknown, "A", None)), 1, false);
        let mut activity = Activity::default();
        activity.command_started();
        for title in ["⠋ Working", "[ ! ] Action Required | Codex"] {
            activity.title_changed(title);
            for progress in [1, 2, 3, 4] {
                activity.progress_reported(progress, None, std::time::Instant::now());
                assert_eq!(
                    report.view(1, "pane", &activity).unwrap().state,
                    State::Unknown
                );
            }
        }
    }
    #[test]
    fn tile_labels_use_local_task_names_without_changing_report_identity() {
        let mut report = Report::default();
        let activity = Activity::default();
        let mut first = snapshot(State::Working, "opaque-thread-a", None);
        first.label = Some("A long description supplied by the reporting program".into());
        report.apply(Event::Begin(first), 7, false);
        let initial = report.view(1, "foo-bar", &activity).unwrap();
        assert_eq!(initial.label, "foo-bar");
        let mut second = snapshot(State::Done, "opaque-thread-b", Some("turn-1"));
        second.label = Some("Completely different conversation prose".into());
        report.apply(Event::Update(second), 8, false);
        let updated = report.view(1, "foo-bar", &activity).unwrap();
        assert_eq!(updated.label, "foo-bar");
        assert!(report.view(1, "", &activity).unwrap().label.is_empty());
        assert_eq!(updated.target, initial.target);
        assert_eq!(updated.state, State::Done);

        let (mut workspace, mut reports) = setup(2);
        for view in reports.values_mut() {
            view.label = "foo-bar".into();
        }
        let mut dashboard = Dashboard::default();
        dashboard.update(&mut workspace, &reports, None);
        assert_eq!(dashboard.board.tile(0).label, "foo-bar ·1");
        assert_eq!(dashboard.board.tile(1).label, "foo-bar ·2");
        let positions = workspace.deck_positions.clone();
        let capture = dashboard.board.tile(0).capture;
        reports.get_mut(&positions[0].unwrap()).unwrap().state = State::Done;
        dashboard.update(&mut workspace, &reports, positions[1]);
        assert_eq!(workspace.deck_positions, positions);
        assert_eq!(dashboard.board.tile(0).capture, capture);
        assert_eq!(dashboard.board.tile(0).label, "foo-bar ·1");
        for view in reports.values_mut() {
            view.label.clear();
        }
        dashboard.update(&mut workspace, &reports, positions[1]);
        assert!(dashboard.board.tile(0).label.is_empty());
        assert!(dashboard.board.tile(1).label.is_empty());
    }
    #[test]
    fn acknowledgements_are_bounded_per_thread() {
        let mut report = Report::default();
        report.apply(Event::Begin(snapshot(State::Idle, "A", None)), 1, false);
        for i in 0..ACK_HISTORY + 1 {
            report.apply(
                Event::Update(snapshot(State::Done, &i.to_string(), Some("1"))),
                1,
                false,
            );
            report.acknowledge();
        }
        assert_eq!(report.attention.len(), ACK_HISTORY);
        report.apply(
            Event::Update(snapshot(State::Done, "0", Some("1"))),
            1,
            false,
        );
        assert_eq!(
            report.view(1, "", &Activity::default()).unwrap().state,
            State::Done
        );
    }
    #[test]
    fn slots_reservations_cycles_and_closed_panes() {
        let (mut workspace, mut reports) = setup(12);
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, None);
        assert_eq!(deck.board.pages(), 2);
        assert_eq!(deck.board.counts, [3; 4]);
        let initial = workspace.deck_positions.clone();
        let input = deck.cycle(0, None).unwrap();
        assert_eq!(input.slot, 0);
        assert_eq!(deck.cycle(0, input.assignment).unwrap().slot, 4);
        assert_eq!(deck.cycle(0, None).unwrap().slot, 8);
        assert_eq!(deck.cycle(0, None).unwrap().slot, 0);
        let done = deck.cycle(2, None).unwrap();
        reports.get_mut(&done.assignment.unwrap()).unwrap().state = State::Idle;
        deck.update(&mut workspace, &reports, done.assignment);
        assert_eq!(deck.cycle(2, done.assignment).unwrap().slot, 6);
        let ended = initial[0].unwrap();
        let saved = reports.remove(&ended).unwrap();
        deck.update(&mut workspace, &reports, None);
        assert!(deck.board.tile(0).reserved);
        assert!(deck.board.tile(0).capture.target.is_none());
        reports.insert(ended, saved);
        deck.update(&mut workspace, &reports, None);
        assert_eq!(workspace.deck_positions, initial);
        workspace.close_pane(ended);
        deck.update(&mut workspace, &reports, None);
        assert_eq!(workspace.deck_positions[0], None);
        assert_eq!(deck.board.counts[0], 2); // retained closed pane is not counted
        deck.page();
        assert_eq!(deck.board.page, 1);
        deck.page();
        assert_eq!(deck.board.page, 0);
        reports.clear();
        deck.update(&mut workspace, &reports, None);
        assert!(deck.board.no_sessions);
        assert!(deck.cycle(0, None).is_none());
        assert_eq!(workspace.deck_positions[1..], initial[1..]);
    }
    #[test]
    fn new_reporter_reclaims_reserved_board_without_losing_panes() {
        let (mut workspace, mut reports) = setup(10);
        let newcomer = workspace.windows[9].tabs[0].root.panes()[0];
        let new_report = reports.remove(&newcomer).unwrap();
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, None);
        let positions = workspace.deck_positions.clone();
        let returning = positions[0].unwrap();
        let old_report = reports[&returning].clone();
        let old_capture = deck.board.tile(0).capture;
        reports.clear();
        deck.update(&mut workspace, &reports, None);
        assert!(deck.board.no_sessions);
        assert_eq!(workspace.deck_positions, positions);

        reports.insert(newcomer, new_report);
        assert!(deck.update(&mut workspace, &reports, None));
        assert!(!deck.board.no_sessions);
        assert_eq!(deck.board.pages(), 1);
        assert_eq!(workspace.deck_positions[0], Some(newcomer));
        assert_eq!(workspace.deck_positions[1..], positions[1..]);
        assert!(!deck.board.valid(old_capture));
        assert_eq!(workspace.windows.len(), 10);

        reports.insert(returning, old_report);
        assert!(deck.update(&mut workspace, &reports, None));
        assert_eq!(workspace.deck_positions[0], Some(newcomer));
        assert_eq!(workspace.deck_positions[1], Some(returning));
        assert_eq!(deck.board.pages(), 1);
        assert!(!deck.update(&mut workspace, &reports, None));
    }
    #[test]
    fn holes_precede_eviction_and_all_registered_states_keep_their_slots() {
        let (mut workspace, mut reports) = setup(11);
        let first = workspace.windows[9].tabs[0].root.panes()[0];
        let second = workspace.windows[10].tabs[0].root.panes()[0];
        let first_report = reports.remove(&first).unwrap();
        let second_report = reports.remove(&second).unwrap();
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, None);
        let positions = workspace.deck_positions.clone();
        let reserved = positions[0].unwrap();
        let returning = reports.remove(&reserved).unwrap();
        reports.remove(&positions[1].unwrap());
        workspace.deck_positions[1] = None;
        for (id, state) in positions[2..].iter().zip([
            State::Idle,
            State::Working,
            State::NeedsInput,
            State::Done,
            State::Error,
            State::Paused,
            State::Unknown,
        ]) {
            reports.get_mut(&id.unwrap()).unwrap().state = state;
        }
        reports.insert(first, first_report);
        deck.update(&mut workspace, &reports, None);
        assert_eq!(workspace.deck_positions[0], Some(reserved));
        assert_eq!(workspace.deck_positions[1], Some(first));
        let reservation_capture = deck.board.tile(0).capture;

        reports.insert(second, second_report);
        deck.update(&mut workspace, &reports, None);
        assert_eq!(workspace.deck_positions[0], Some(second));
        assert_eq!(workspace.deck_positions[2..], positions[2..]);
        assert!(!deck.board.valid(reservation_capture));
        assert_eq!(deck.board.pages(), 1);

        reports.insert(reserved, returning);
        deck.update(&mut workspace, &reports, None);
        assert_eq!(workspace.deck_positions[9], Some(reserved));
        assert_eq!(workspace.deck_positions[2..9], positions[2..]);
        assert_eq!(deck.board.pages(), 2);
    }
    #[test]
    fn screen_sleep_preserves_information_and_invalidates_pending_keys() {
        let (mut workspace, reports) = setup(2);
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, None);
        let positions = workspace.deck_positions.clone();
        let capture = deck.board.tile(0).capture;
        deck.gesture = Some(capture);
        deck.moving = true;
        deck.board.brightness = 25;
        deck.screens_awake(false);
        deck.update(&mut workspace, &reports, None);
        assert!(!deck.board.valid(capture));
        assert!(deck.gesture.is_none());
        assert!(!deck.moving);
        let generation = deck.board.mode_generation;
        deck.screens_awake(false);
        assert_eq!(deck.board.mode_generation, generation);
        deck.screens_awake(true);
        deck.update(&mut workspace, &reports, None);
        assert_eq!(workspace.deck_positions, positions);
        assert_eq!(deck.board.brightness, 25);
        assert_eq!(deck.board.tile(0).state, Some(State::NeedsInput));
        assert_eq!(deck.board.tile(1).state, Some(State::Working));
        assert!(!deck.board.no_sessions);
    }
    #[test]
    fn eviction_waits_for_the_pending_swap() {
        let (mut workspace, mut reports) = setup(3);
        let newcomer = workspace.windows[2].tabs[0].root.panes()[0];
        let new_report = reports.remove(&newcomer).unwrap();
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, None);
        let positions = workspace.deck_positions.clone();
        reports.remove(&positions[1].unwrap());
        deck.update(&mut workspace, &reports, None);
        let source = deck.board.tile(0).capture;
        let destination = deck.board.tile(1).capture;
        deck.gesture = Some(source);
        reports.insert(newcomer, new_report);
        assert!(!deck.update(&mut workspace, &reports, None));
        assert_eq!(workspace.deck_positions, positions);
        assert!(deck.board.valid(destination));
        deck.moving = true;
        assert!(!deck.update(&mut workspace, &reports, None));
        assert!(deck.swap(&mut workspace, source, destination));

        deck.update(&mut workspace, &reports, None);
        assert_eq!(workspace.deck_positions, [Some(newcomer), positions[0]]);
        assert!(!deck.board.valid(destination));
        assert_eq!(deck.board.pages(), 1);
    }
    #[test]
    fn swap_reservations_holes_and_freeze_assignments() {
        let (mut workspace, mut reports) = setup(3);
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, None);
        let initial = workspace.deck_positions.clone();
        let source = deck.board.tile(0).capture;
        deck.gesture = Some(source);
        deck.moving = true;
        let destination = deck.board.tile(8).capture;
        assert!(deck.swap(&mut workspace, source, destination));
        deck.update(&mut workspace, &reports, None);
        assert_eq!(workspace.deck_positions[8], initial[0]);
        assert_eq!(workspace.deck_positions[0], None);
        let reserved = initial[1].unwrap();
        reports.remove(&reserved);
        deck.update(&mut workspace, &reports, None);
        let source = deck.board.tile(8).capture;
        let destination = deck.board.tile(1).capture;
        deck.gesture = Some(source);
        deck.moving = true;
        assert!(deck.cycle(0, None).is_none());
        deck.page();
        assert!(deck.swap(&mut workspace, source, destination));
        deck.update(&mut workspace, &reports, None);
        assert_eq!(workspace.deck_positions[8], Some(reserved));
        let source = deck.board.tile(1).capture;
        deck.gesture = Some(source);
        deck.moving = true;
        reports
            .get_mut(&source.assignment.unwrap())
            .unwrap()
            .target
            .generation += 1;
        deck.update(&mut workspace, &reports, None);
        assert!(deck.gesture.is_none());
        assert!(!deck.swap(&mut workspace, source, deck.board.tile(0).capture));
    }
}
