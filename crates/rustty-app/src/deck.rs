//! Pane-local reports and stable dashboard positions. No terminal input is sent here.
use crate::{
    presentation::Activity,
    workspace::{Id, MAX_DECK_SLOTS, Workspace},
};
use rustty::vt::agent::{Event, Snapshot, State};
use std::{
    cmp::Reverse,
    collections::{HashMap, VecDeque},
    time::Instant,
};

pub const PAGE_SIZE: usize = 9;
pub const ACK_HISTORY: usize = 64;
pub const GROUPS: [State; 4] = [State::NeedsInput, State::Working, State::Done, State::Idle];

fn in_group(state: State, group: usize) -> bool {
    GROUPS.get(group).is_some_and(|expected| {
        state == *expected || group == 0 && matches!(state, State::Error | State::Paused)
    })
}

/// OSC progress contributes only to aggregate controls, never to agent tiles.
#[derive(Default)]
pub struct ProgressReport {
    state: Option<State>,
    active: bool,
    enclosing_command: bool,
    outcome: Option<State>,
    deadline: Option<Instant>,
}
impl ProgressReport {
    pub fn state(&self) -> Option<State> {
        self.state
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }
    pub fn expire(&mut self, now: Instant) {
        if self.deadline.is_some_and(|deadline| deadline <= now) {
            self.active = false;
            self.deadline = None;
            self.state = None;
        }
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn apply(&mut self, effect: &rustty::vt::Effect, command_running: bool) {
        use rustty::vt::Effect;
        match effect {
            Effect::Progress { state: 1..=4, .. } => {
                if !self.active {
                    self.clear();
                    self.enclosing_command = command_running;
                }
                self.active = true;
                self.deadline = Some(Instant::now() + crate::presentation::PROGRESS_TIMEOUT);
                self.state = Some(match effect {
                    Effect::Progress { state: 2, .. } => State::Error,
                    Effect::Progress { state: 4, .. } => State::Paused,
                    _ => State::Working,
                });
                if self.state == Some(State::Error) {
                    self.outcome = Some(State::Error);
                }
            }
            Effect::Progress { state: 0, .. } => self.finish(),
            Effect::ProgressReset | Effect::CommandStart => self.clear(),
            Effect::CommandEnd { exit_code } if self.enclosing_command => {
                self.active = false;
                self.deadline = None;
                self.enclosing_command = false;
                self.state = Some(match exit_code {
                    _ if self.outcome == Some(State::Error) => State::Error,
                    Some(0) => State::Done,
                    Some(_) => State::Error,
                    None => self.outcome.unwrap_or(State::NeedsInput),
                });
            }
            Effect::Notification { title, body }
                if self.state.is_some() || self.enclosing_command =>
            {
                // wait-pr/notify-me declare results with leading emoji; prose is not a status.
                let message = if title.is_empty() { body } else { title };
                let message = String::from_utf8_lossy(message);
                let message = message.trim_start();
                let outcome = if message.starts_with('❌')
                    || message.starts_with('⚠')
                    || message.starts_with("✅⚠")
                {
                    Some(State::Error)
                } else if message.starts_with('✅') {
                    Some(State::Done)
                } else {
                    None
                };
                if let Some(outcome) = outcome
                    && self.outcome != Some(State::Error)
                {
                    self.outcome = Some(outcome);
                    if !self.active {
                        self.state = Some(outcome);
                    }
                }
            }
            _ => {}
        }
    }
    /// OSC removal alone says nothing about whether the work succeeded.
    pub fn finish(&mut self) {
        if self.active {
            self.active = false;
            self.deadline = None;
            self.state = Some(self.outcome.unwrap_or(State::NeedsInput));
        }
    }
    pub fn acknowledge(&mut self) {
        // A clear may precede the shell's exit status in a later PTY read.
        if !self.active {
            self.state = None;
            if !self.enclosing_command {
                self.clear();
            }
        }
    }
}

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
    last_activity: Option<Instant>,
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
        if self.snapshot.as_ref().is_none_or(|previous| {
            previous.state != snapshot.state
                || previous.thread_id != snapshot.thread_id
                || previous.turn_id != snapshot.turn_id
        }) {
            self.last_activity = Some(Instant::now());
        }
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
            last_activity: self.last_activity,
            label: task_label.to_owned(),
            unseen: attention.is_some_and(|entry| entry.unseen),
        })
    }
}
#[derive(Clone, Debug)]
pub struct PaneView {
    pub target: Target,
    pub state: State,
    pub last_activity: Option<Instant>,
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
    last_activity: Vec<Option<Instant>>,
    groups: [Vec<(usize, Id)>; 4],
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
        progress: &HashMap<Id, State>,
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
        let no_sessions = !live
            .iter()
            .any(|id| reports.contains_key(id) || progress.contains_key(id));
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
        self.groups = std::array::from_fn(|group| {
            workspace
                .deck_positions
                .iter()
                .enumerate()
                .filter_map(|(slot, id)| id.map(|id| (slot, id)))
                .chain(live.iter().enumerate().filter_map(|(index, id)| {
                    (!workspace.deck_positions.contains(&Some(*id)))
                        .then_some((workspace.deck_positions.len() + index, *id))
                }))
                .filter(|(_, id)| {
                    live.contains(id)
                        && (reports.get(id).is_some_and(|r| in_group(r.state, group))
                            || progress.get(id).is_some_and(|s| in_group(*s, group)))
                })
                .collect()
        });
        self.board.counts = std::array::from_fn(|group| self.groups[group].len());
        self.last_activity = workspace
            .deck_positions
            .iter()
            .map(|id| id.and_then(|id| reports.get(&id)?.last_activity))
            .collect();
        self.board.page = self.board.page.min(self.board.pages() - 1);
        changed
    }
    pub fn cycle(&mut self, group: usize, focused: Option<Id>) -> Option<Id> {
        if self.moving {
            return None;
        }
        let members = self.groups.get(group)?;
        let cursor = members
            .iter()
            .find(|(_, id)| Some(*id) == focused)
            .map(|(slot, _)| *slot)
            .or(self.cursors[group]);
        let next = cursor
            .and_then(|slot| members.iter().find(|(rank, _)| *rank > slot))
            .or_else(|| members.first())
            .copied()?;
        // Keep the slot cursor after Done acknowledgement removes its member.
        self.cursors[group] = Some(next.0);
        if let Some(tile) =
            self.board.tiles.iter().find(|tile| {
                tile.capture.assignment == Some(next.1) && tile.capture.target.is_some()
            })
        {
            self.board.page = tile.capture.slot / PAGE_SIZE;
        }
        Some(next.1)
    }
    pub fn page(&mut self) {
        self.board.page = (self.board.page + 1) % self.board.pages();
    }
    pub fn pull_to_first(&mut self, workspace: &mut Workspace) -> bool {
        self.board.page = 0;
        // Host commands must cancel worker-local gestures even when every slot stays put.
        self.board.mode_generation += 1;
        self.gesture = None;
        self.moving = false;
        if self.positions != workspace.deck_positions {
            return false;
        }
        let mut candidates: Vec<_> = self
            .board
            .tiles
            .iter()
            .filter(|tile| tile.state.is_some_and(|state| state != State::Idle))
            .collect();
        candidates.sort_by_key(|tile| {
            let priority = match tile.state {
                Some(State::NeedsInput) => 0,
                Some(State::Done) => 1,
                Some(State::Working) => 2,
                Some(State::Paused) => 3,
                _ => 4,
            };
            (
                priority,
                Reverse(self.last_activity[tile.capture.slot]),
                tile.capture.slot,
            )
        });
        candidates.truncate(PAGE_SIZE);
        // Keep the selected first-page agents in their familiar positions.
        let mut destinations: Vec<_> = (0..PAGE_SIZE)
            .filter(|slot| !candidates.iter().any(|tile| tile.capture.slot == *slot))
            .collect();
        destinations.sort_by_key(|&slot| {
            let tile = self.board.tile(slot);
            let priority = if tile.capture.assignment.is_none() {
                0
            } else if tile.reserved {
                1
            } else if tile.state == Some(State::Idle) {
                2
            } else {
                3
            };
            (
                priority,
                self.last_activity.get(slot).copied().flatten(),
                slot,
            )
        });
        let mut changed = false;
        for (source, destination) in candidates
            .iter()
            .filter(|tile| tile.capture.slot >= PAGE_SIZE)
            .zip(destinations)
        {
            workspace
                .deck_positions
                .swap(source.capture.slot, destination);
            changed = true;
        }
        if changed {
            workspace.reconcile_deck_positions();
            self.board.generation += 1;
        }
        changed
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
                    last_activity: None,
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
    fn wait_pr_and_notify_me_results_survive_split_reads() {
        use rustty::vt::{Effect, Terminal};
        for (ending, expected) in [
            (
                "\x1b]9;✅ PR #42 merged: title\x1b\\\x1b]9;4;0\x1b\\\x1b]133;D;0\x07",
                State::Done,
            ),
            (
                "\x1b]9;❌ PR #42 CI failed: title\x1b\\\x1b]9;4;0\x1b\\\x1b]133;D;1\x07",
                State::Error,
            ),
            (
                "\x1b]9;✅⚠️ PR #42 CI finished, but is not merged: title\x1b\\\x1b]9;4;0\x1b\\\x1b]133;D;0\x07",
                State::Error,
            ),
            (
                "\x1b]9;4;0\x1b\\\x1b]9;✅ task finished\x1b\\\x1b]133;D;0\x07",
                State::Done,
            ),
            (
                "\x1b]9;4;0\x1b\\\x1b]9;❌ task failed\x1b\\\x1b]133;D;1\x07",
                State::Error,
            ),
            ("\x1b]9;4;0\x1b\\\x1b]133;D;0\x07", State::Done),
            ("\x1b]9;4;0\x1b\\\x1b]133;D;2\x07", State::Error),
            ("\x1b]9;4;0\x1b\\\x1b]133;D\x07", State::NeedsInput),
        ] {
            let stream = format!("\x1b]133;C\x07\x1b]9;4;3\x1b\\{ending}");
            for split in 0..=stream.len() {
                let mut terminal = Terminal::new(80, 24, 0);
                terminal.shell_command_events = true;
                let mut report = ProgressReport::default();
                let mut running = false;
                for bytes in [&stream.as_bytes()[..split], &stream.as_bytes()[split..]] {
                    for effect in terminal.feed(bytes) {
                        report.apply(&effect, running);
                        match effect {
                            Effect::CommandStart => running = true,
                            Effect::CommandEnd { .. } => running = false,
                            _ => {}
                        }
                    }
                }
                assert_eq!(report.state(), Some(expected), "{ending:?} at {split}");
                report.acknowledge();
                assert_eq!(report.state(), None);
            }
        }
    }
    #[test]
    fn progress_expiry_reset_and_acknowledgement_never_invent_success() {
        use rustty::vt::Effect;
        let mut report = ProgressReport::default();
        let progress = |state| Effect::Progress {
            state,
            value: Some(100),
        };
        for (state, expected) in [
            (1, State::Working),
            (3, State::Working),
            (2, State::Error),
            (4, State::Paused),
        ] {
            report.clear();
            report.apply(&progress(state), false);
            report.acknowledge();
            assert_eq!(report.state(), Some(expected));
            report.expire(report.deadline().unwrap());
            assert_eq!(report.state(), None);
            assert_eq!(report.deadline(), None);
            report.acknowledge();
            assert_eq!(report.state(), None);
        }
        report.apply(&progress(3), true);
        report.apply(&progress(0), true);
        report.acknowledge();
        assert_eq!(report.state(), None);
        report.apply(&Effect::CommandEnd { exit_code: Some(1) }, true);
        assert_eq!(report.state(), Some(State::Error));
        report.apply(&Effect::CommandStart, false);
        assert_eq!(report.state(), None);
        report.apply(&progress(1), true);
        report.apply(&Effect::ProgressReset, true);
        report.apply(&Effect::CommandEnd { exit_code: Some(0) }, true);
        assert_eq!(report.state(), None);
        for ending in [progress(0), Effect::CommandEnd { exit_code: Some(0) }] {
            report.apply(&ending, false);
            assert_eq!(report.state(), None);
        }
        report.apply(&progress(3), false);
        report.apply(&progress(0), false);
        assert_eq!(report.state(), Some(State::NeedsInput));
        report.apply(
            &Effect::Notification {
                title: Vec::new(),
                body: "✅ task finished".as_bytes().to_vec(),
            },
            false,
        );
        assert_eq!(report.state(), Some(State::Done));
        report.acknowledge();
        report.apply(&progress(3), true);
        report.apply(&progress(0), true);
        report.acknowledge();
        report.apply(
            &Effect::Notification {
                title: Vec::new(),
                body: "✅⚠️ task needs attention".as_bytes().to_vec(),
            },
            true,
        );
        report.apply(&Effect::CommandEnd { exit_code: Some(0) }, true);
        assert_eq!(report.state(), Some(State::Error));
    }
    #[test]
    fn progress_timeout_loses_tracking_until_fresh_progress_or_a_command_result() {
        use rustty::vt::Effect;
        let mut report = ProgressReport::default();
        let progress = Effect::Progress {
            state: 3,
            value: None,
        };
        report.apply(&progress, true);
        let deadline = report.deadline().unwrap();
        report.expire(deadline - std::time::Duration::from_nanos(1));
        assert_eq!(report.state(), Some(State::Working));
        report.expire(deadline);
        assert_eq!(report.state(), None);
        assert_eq!(report.deadline(), None);
        report.expire(deadline);
        assert_eq!(report.state(), None);
        report.apply(&progress, true);
        assert_eq!(report.state(), Some(State::Working));
        assert!(report.deadline().is_some());

        for (exit_code, expected) in [(Some(0), State::Done), (Some(1), State::Error)] {
            report.clear();
            report.apply(&progress, true);
            report.expire(report.deadline().unwrap());
            report.apply(&Effect::CommandEnd { exit_code }, true);
            assert_eq!(report.state(), Some(expected));
        }
        report.clear();
        report.apply(
            &Effect::Progress {
                state: 2,
                value: None,
            },
            true,
        );
        report.expire(report.deadline().unwrap());
        assert_eq!(report.state(), None);
        report.apply(&Effect::CommandEnd { exit_code: Some(0) }, true);
        assert_eq!(report.state(), Some(State::Error));
    }
    #[test]
    fn progress_aggregates_deduplicate_without_allocating_agent_slots() {
        let (mut workspace, mut reports) = setup(3);
        let ids: Vec<_> = workspace
            .windows
            .iter()
            .map(|w| w.tabs[0].root.panes()[0])
            .collect();
        let mut progress = HashMap::from([
            (ids[0], State::Error),
            (ids[1], State::Working),
            (ids[2], State::Working),
        ]);
        reports.get_mut(&ids[0]).unwrap().state = State::Paused;
        reports.get_mut(&ids[1]).unwrap().state = State::Working;
        reports.remove(&ids[2]);
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, &progress, None);
        assert_eq!(deck.board.counts, [1, 2, 0, 0]);
        assert_eq!(workspace.deck_positions, [Some(ids[0]), Some(ids[1])]);
        assert_eq!(deck.board.tile(0).state, Some(State::Paused));
        assert_eq!(deck.cycle(1, None), Some(ids[1]));
        assert_eq!(deck.cycle(1, Some(ids[1])), Some(ids[2]));
        reports.clear();
        deck.update(&mut workspace, &reports, &progress, None);
        assert!(!deck.board.no_sessions);
        assert!(deck.board.tiles.iter().all(|t| t.state.is_none()));
        let positions = workspace.deck_positions.clone();
        progress.insert(ids[1], State::Done);
        progress.insert(ids[2], State::Done);
        deck.update(&mut workspace, &reports, &progress, None);
        assert_eq!(deck.cycle(2, None), Some(ids[1]));
        progress.remove(&ids[1]);
        deck.update(&mut workspace, &reports, &progress, Some(ids[1]));
        assert_eq!(deck.cycle(2, Some(ids[1])), Some(ids[2]));
        workspace.close_pane(ids[2]);
        deck.update(&mut workspace, &reports, &progress, None);
        assert_eq!(deck.board.counts[2], 0);
        progress.clear();
        let mode = deck.board.mode_generation;
        deck.update(&mut workspace, &reports, &progress, None);
        assert!(deck.board.no_sessions);
        assert!(deck.board.mode_generation > mode);
        assert_eq!(workspace.deck_positions, positions);
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
        dashboard.update(&mut workspace, &reports, &HashMap::new(), None);
        assert_eq!(dashboard.board.tile(0).label, "foo-bar ·1");
        assert_eq!(dashboard.board.tile(1).label, "foo-bar ·2");
        let positions = workspace.deck_positions.clone();
        let capture = dashboard.board.tile(0).capture;
        reports.get_mut(&positions[0].unwrap()).unwrap().state = State::Done;
        dashboard.update(&mut workspace, &reports, &HashMap::new(), positions[1]);
        assert_eq!(workspace.deck_positions, positions);
        assert_eq!(dashboard.board.tile(0).capture, capture);
        assert_eq!(dashboard.board.tile(0).label, "foo-bar ·1");
        for view in reports.values_mut() {
            view.label.clear();
        }
        dashboard.update(&mut workspace, &reports, &HashMap::new(), positions[1]);
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
    fn activity_recency_ignores_duplicate_metadata_and_acknowledgement() {
        let mut report = Report::default();
        let activity = Activity::default();
        let old = Instant::now() - std::time::Duration::from_secs(60);
        let initial = snapshot(State::Done, "A", Some("T1"));
        report.apply(Event::Update(initial.clone()), 1, false);
        assert!(report.last_activity.is_none());
        report.apply(Event::Begin(initial.clone()), 1, false);
        assert!(report.last_activity.unwrap() > old);
        report.last_activity = Some(old);
        let mut duplicate = initial.clone();
        duplicate.label = Some("A different label".into());
        report.apply(Event::Update(duplicate), 1, false);
        report.acknowledge();
        let view = report.view(1, "task", &activity).unwrap();
        assert_eq!(view.state, State::Idle);
        assert_eq!(view.last_activity, Some(old));
        for changed in [
            snapshot(State::Done, "A", Some("T2")),
            snapshot(State::Done, "B", Some("T2")),
            snapshot(State::Working, "B", Some("T2")),
            snapshot(State::Idle, "B", Some("T2")),
        ] {
            report.last_activity = Some(old);
            report.apply(Event::Update(changed.clone()), 1, false);
            let observed = report.view(1, "task", &activity).unwrap().last_activity;
            assert!(observed.unwrap() > old);
            report.apply(Event::Update(changed), 1, false);
            assert_eq!(report.last_activity, observed);
        }
        report.last_activity = Some(old);
        report.apply(Event::Begin(initial), 2, false);
        assert!(report.last_activity.unwrap() > old);
    }
    #[test]
    fn pull_prioritizes_categories_and_recency_without_reordering_kept_agents() {
        let (mut workspace, mut reports) = setup(17);
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        let initial = workspace.deck_positions.clone();
        for report in reports.values_mut() {
            report.state = State::Idle;
        }
        let now = Instant::now();
        for (slot, state, age) in [
            (1, State::NeedsInput, 100),
            (4, State::Done, 100),
            (6, State::Working, 100),
            (9, State::NeedsInput, 20),
            (10, State::NeedsInput, 10),
            (11, State::Done, 5),
            (12, State::Working, 4),
            (13, State::Paused, 3),
            (14, State::Error, 2),
            (15, State::Unknown, 2),
        ] {
            let report = reports.get_mut(&initial[slot].unwrap()).unwrap();
            report.state = state;
            report.last_activity = Some(now - std::time::Duration::from_secs(age));
        }
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        assert!(deck.pull_to_first(&mut workspace));
        for (destination, source) in [(0, 10), (2, 9), (3, 11), (5, 12), (7, 13), (8, 14)] {
            assert_eq!(workspace.deck_positions[destination], initial[source]);
            assert_eq!(workspace.deck_positions[source], initial[destination]);
        }
        for kept in [1, 4, 6, 15, 16] {
            assert_eq!(workspace.deck_positions[kept], initial[kept]);
        }
        assert!(!deck.pull_to_first(&mut workspace));
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        let pulled = workspace.deck_positions.clone();
        assert!(!deck.pull_to_first(&mut workspace));
        assert_eq!(workspace.deck_positions, pulled);
    }
    #[test]
    fn pull_prefers_empty_then_reserved_then_oldest_idle_and_preserves_assignments() {
        let (mut workspace, mut reports) = setup(12);
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        let initial = workspace.deck_positions.clone();
        reports.remove(&initial[0].unwrap());
        workspace.close_pane(initial[0].unwrap());
        reports.remove(&initial[1].unwrap());
        let now = Instant::now();
        for (slot, id) in initial.iter().enumerate().skip(2) {
            let report = reports.get_mut(&id.unwrap()).unwrap();
            report.state = if slot < PAGE_SIZE {
                State::Idle
            } else {
                State::Working
            };
            report.last_activity = Some(now - std::time::Duration::from_secs(slot as u64));
        }
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        let before = workspace.deck_positions.clone();
        let capture = deck.board.tile(9).capture;
        deck.gesture = Some(capture);
        deck.moving = true;
        deck.board.page = 1;
        assert!(deck.pull_to_first(&mut workspace));
        assert_eq!(deck.board.page, 0);
        assert!(deck.gesture.is_none());
        assert!(!deck.moving);
        assert!(!deck.board.valid(capture));
        assert_eq!(workspace.deck_positions[0], initial[9]);
        assert_eq!(workspace.deck_positions[1], initial[10]);
        assert_eq!(workspace.deck_positions[8], initial[11]);
        assert_eq!(workspace.deck_positions[9], None);
        assert_eq!(workspace.deck_positions[10], initial[1]);
        assert_eq!(workspace.deck_positions[11], initial[8]);
        assert_eq!(&workspace.deck_positions[2..8], &initial[2..8]);
        let mut before: Vec<_> = before.into_iter().flatten().collect();
        let mut after: Vec<_> = workspace.deck_positions.iter().copied().flatten().collect();
        before.sort_unstable();
        after.sort_unstable();
        assert_eq!(before, after);
    }
    #[test]
    fn pull_selects_nine_and_evicts_least_recent_nonselected_agents_first() {
        let (mut workspace, mut reports) = setup(18);
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        let initial = workspace.deck_positions.clone();
        let now = Instant::now();
        for (slot, id) in initial.iter().enumerate() {
            let report = reports.get_mut(&id.unwrap()).unwrap();
            report.state = State::Working;
            report.last_activity = Some(now - std::time::Duration::from_secs(slot as u64));
        }
        for id in &initial[9..18] {
            reports.get_mut(&id.unwrap()).unwrap().state = State::NeedsInput;
        }
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        assert!(deck.pull_to_first(&mut workspace));
        for destination in 0..9 {
            assert_eq!(
                workspace.deck_positions[destination],
                initial[17 - destination]
            );
        }
        assert_eq!(workspace.deck_positions.len(), initial.len());
    }
    #[test]
    fn pull_never_promotes_idle_or_retained_closed_agents_and_returns_to_first_page() {
        let (mut workspace, mut reports) = setup(12);
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        let initial = workspace.deck_positions.clone();
        for report in reports.values_mut() {
            report.state = State::Idle;
        }
        reports.get_mut(&initial[11].unwrap()).unwrap().state = State::NeedsInput;
        workspace.close_pane(initial[11].unwrap());
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        let positions = workspace.deck_positions.clone();
        deck.board.page = 1;
        assert!(!deck.pull_to_first(&mut workspace));
        assert_eq!(deck.board.page, 0);
        assert_eq!(workspace.deck_positions, positions);
        assert!(
            deck.board
                .tiles
                .iter()
                .all(|tile| tile.state != Some(State::NeedsInput))
        );
    }
    #[test]
    fn pull_without_moves_still_invalidates_worker_control_mode() {
        let (mut workspace, reports) = setup(2);
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        for _ in 0..2 {
            let mode = deck.board.mode_generation;
            let layout = deck.board.generation;
            let positions = workspace.deck_positions.clone();
            deck.gesture = Some(deck.board.tile(0).capture);
            deck.moving = true;
            assert!(!deck.pull_to_first(&mut workspace));
            assert_eq!(deck.board.mode_generation, mode + 1);
            assert_eq!(deck.board.generation, layout);
            assert_eq!(workspace.deck_positions, positions);
            assert!(deck.gesture.is_none());
            assert!(!deck.moving);
        }
        workspace.deck_positions.swap(0, 1);
        let mode = deck.board.mode_generation;
        assert!(!deck.pull_to_first(&mut workspace));
        assert_eq!(deck.board.mode_generation, mode + 1);
    }
    #[test]
    fn slots_reservations_cycles_and_closed_panes() {
        let (mut workspace, mut reports) = setup(12);
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        assert_eq!(deck.board.pages(), 2);
        assert_eq!(deck.board.counts, [3; 4]);
        let initial = workspace.deck_positions.clone();
        let input = deck.cycle(0, None).unwrap();
        assert_eq!(Some(input), initial[0]);
        assert_eq!(Some(deck.cycle(0, Some(input)).unwrap()), initial[4]);
        assert_eq!(Some(deck.cycle(0, None).unwrap()), initial[8]);
        assert_eq!(Some(deck.cycle(0, None).unwrap()), initial[0]);
        let done = deck.cycle(2, None).unwrap();
        reports.get_mut(&done).unwrap().state = State::Idle;
        deck.update(&mut workspace, &reports, &HashMap::new(), Some(done));
        assert_eq!(Some(deck.cycle(2, Some(done)).unwrap()), initial[6]);
        let ended = initial[0].unwrap();
        let saved = reports.remove(&ended).unwrap();
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        assert!(deck.board.tile(0).reserved);
        assert!(deck.board.tile(0).capture.target.is_none());
        reports.insert(ended, saved);
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        assert_eq!(workspace.deck_positions, initial);
        workspace.close_pane(ended);
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        assert_eq!(workspace.deck_positions[0], None);
        assert_eq!(deck.board.counts[0], 2); // retained closed pane is not counted
        deck.page();
        assert_eq!(deck.board.page, 1);
        deck.page();
        assert_eq!(deck.board.page, 0);
        reports.clear();
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
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
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        let positions = workspace.deck_positions.clone();
        let returning = positions[0].unwrap();
        let old_report = reports[&returning].clone();
        let old_capture = deck.board.tile(0).capture;
        reports.clear();
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        assert!(deck.board.no_sessions);
        assert_eq!(workspace.deck_positions, positions);

        reports.insert(newcomer, new_report);
        assert!(deck.update(&mut workspace, &reports, &HashMap::new(), None));
        assert!(!deck.board.no_sessions);
        assert_eq!(deck.board.pages(), 1);
        assert_eq!(workspace.deck_positions[0], Some(newcomer));
        assert_eq!(workspace.deck_positions[1..], positions[1..]);
        assert!(!deck.board.valid(old_capture));
        assert_eq!(workspace.windows.len(), 10);

        reports.insert(returning, old_report);
        assert!(deck.update(&mut workspace, &reports, &HashMap::new(), None));
        assert_eq!(workspace.deck_positions[0], Some(newcomer));
        assert_eq!(workspace.deck_positions[1], Some(returning));
        assert_eq!(deck.board.pages(), 1);
        assert!(!deck.update(&mut workspace, &reports, &HashMap::new(), None));
    }
    #[test]
    fn holes_precede_eviction_and_all_registered_states_keep_their_slots() {
        let (mut workspace, mut reports) = setup(11);
        let first = workspace.windows[9].tabs[0].root.panes()[0];
        let second = workspace.windows[10].tabs[0].root.panes()[0];
        let first_report = reports.remove(&first).unwrap();
        let second_report = reports.remove(&second).unwrap();
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
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
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        assert_eq!(workspace.deck_positions[0], Some(reserved));
        assert_eq!(workspace.deck_positions[1], Some(first));
        let reservation_capture = deck.board.tile(0).capture;

        reports.insert(second, second_report);
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        assert_eq!(workspace.deck_positions[0], Some(second));
        assert_eq!(workspace.deck_positions[2..], positions[2..]);
        assert!(!deck.board.valid(reservation_capture));
        assert_eq!(deck.board.pages(), 1);

        reports.insert(reserved, returning);
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        assert_eq!(workspace.deck_positions[9], Some(reserved));
        assert_eq!(workspace.deck_positions[2..9], positions[2..]);
        assert_eq!(deck.board.pages(), 2);
    }
    #[test]
    fn screen_sleep_preserves_information_and_invalidates_pending_keys() {
        let (mut workspace, reports) = setup(2);
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        let positions = workspace.deck_positions.clone();
        let capture = deck.board.tile(0).capture;
        deck.gesture = Some(capture);
        deck.moving = true;
        deck.board.brightness = 25;
        deck.screens_awake(false);
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        assert!(!deck.board.valid(capture));
        assert!(deck.gesture.is_none());
        assert!(!deck.moving);
        let generation = deck.board.mode_generation;
        deck.screens_awake(false);
        assert_eq!(deck.board.mode_generation, generation);
        deck.screens_awake(true);
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
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
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        let positions = workspace.deck_positions.clone();
        reports.remove(&positions[1].unwrap());
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        let source = deck.board.tile(0).capture;
        let destination = deck.board.tile(1).capture;
        deck.gesture = Some(source);
        reports.insert(newcomer, new_report);
        assert!(!deck.update(&mut workspace, &reports, &HashMap::new(), None));
        assert_eq!(workspace.deck_positions, positions);
        assert!(deck.board.valid(destination));
        deck.moving = true;
        assert!(!deck.update(&mut workspace, &reports, &HashMap::new(), None));
        assert!(deck.swap(&mut workspace, source, destination));

        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        assert_eq!(workspace.deck_positions, [Some(newcomer), positions[0]]);
        assert!(!deck.board.valid(destination));
        assert_eq!(deck.board.pages(), 1);
    }
    #[test]
    fn swap_reservations_holes_and_freeze_assignments() {
        let (mut workspace, mut reports) = setup(3);
        let mut deck = Dashboard::default();
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        let initial = workspace.deck_positions.clone();
        let source = deck.board.tile(0).capture;
        deck.gesture = Some(source);
        deck.moving = true;
        let destination = deck.board.tile(8).capture;
        assert!(deck.swap(&mut workspace, source, destination));
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        assert_eq!(workspace.deck_positions[8], initial[0]);
        assert_eq!(workspace.deck_positions[0], None);
        let reserved = initial[1].unwrap();
        reports.remove(&reserved);
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        let source = deck.board.tile(8).capture;
        let destination = deck.board.tile(1).capture;
        deck.gesture = Some(source);
        deck.moving = true;
        assert!(deck.cycle(0, None).is_none());
        deck.page();
        assert!(deck.swap(&mut workspace, source, destination));
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        assert_eq!(workspace.deck_positions[8], Some(reserved));
        let source = deck.board.tile(1).capture;
        deck.gesture = Some(source);
        deck.moving = true;
        reports
            .get_mut(&source.assignment.unwrap())
            .unwrap()
            .target
            .generation += 1;
        deck.update(&mut workspace, &reports, &HashMap::new(), None);
        assert!(deck.gesture.is_none());
        assert!(!deck.swap(&mut workspace, source, deck.board.tile(0).capture));
    }
}
