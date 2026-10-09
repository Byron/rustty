//! One connection-scoped owner for HID, fonts, image uploads and button reads.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use elgato_streamdeck::{StreamDeck, StreamDeckInput, info::Kind};
use image::{DynamicImage, RgbImage};
use rustty::vt::agent::State;

use crate::{
    deck::{Board, Capture},
    deck_render::{Mark, Renderer, Visual},
};

// hidapi 2.6.7 retains a process-global manager scheduled on its first thread's run loop.
#[cfg(target_os = "macos")]
struct HidScope {
    _lock: std::sync::MutexGuard<'static, ()>,
}
#[cfg(target_os = "macos")]
impl HidScope {
    fn new() -> Self {
        static HID: Mutex<()> = Mutex::new(());
        Self {
            _lock: HID.lock().unwrap_or_else(|error| error.into_inner()),
        }
    }
}
#[cfg(target_os = "macos")]
impl Drop for HidScope {
    fn drop(&mut self) {
        unsafe extern "C" {
            fn hid_exit() -> std::ffi::c_int;
        }
        // SAFETY: every app HID scope is serialized, and all devices/API objects drop before this guard on the same thread.
        unsafe {
            hid_exit();
        }
    }
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub kind: Kind,
    pub serial: String,
}

/// Enumeration only. The application calls this off the UI thread at most once a second.
pub fn discover(serial: Option<&str>) -> Result<Option<Candidate>, String> {
    #[cfg(target_os = "macos")]
    let _scope = HidScope::new();
    let hid = elgato_streamdeck::new_hidapi().map_err(|e| e.to_string())?;
    let devices: Vec<_> = elgato_streamdeck::list_devices(&hid)
        .into_iter()
        .filter(|(_, found)| serial.is_none_or(|wanted| found == wanted))
        .collect();
    match devices.as_slice() {
        [] => Ok(None),
        [(kind, serial)] => {
            Layout::new(*kind)?;
            Ok(Some(Candidate {
                kind: *kind,
                serial: serial.clone(),
            }))
        }
        _ => Err("Multiple Stream Decks found; select stream-deck-serial".into()),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Gesture(Option<Capture>),
    Move(Option<Capture>),
    Focus(Capture),
    Swap {
        source: Capture,
        destination: Capture,
    },
    Cycle {
        group: usize,
        generation: u64,
    },
    Page {
        generation: u64,
    },
    Brightness(u8),
    Disconnected(Result<(), String>),
}

pub struct Worker {
    latest: Arc<Mutex<Option<Board>>>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), String>>>,
}

impl Worker {
    pub fn spawn(
        candidate: Candidate,
        initial: Board,
        callback: impl Fn(Action) + Send + 'static,
    ) -> std::io::Result<Self> {
        let latest = Arc::new(Mutex::new(None));
        let stopping = Arc::new(AtomicBool::new(false));
        let updates = Arc::clone(&latest);
        let cancel = Arc::clone(&stopping);
        let thread = thread::Builder::new()
            .name("rustty-stream-deck".into())
            .spawn(move || {
                let result = connected(candidate, initial, &updates, &cancel, &callback);
                callback(Action::Disconnected(result.clone()));
                result
            })?;
        Ok(Self {
            latest,
            stopping,
            thread: Some(thread),
        })
    }

    /// Replace the single pending snapshot; status bursts cannot queue renders.
    pub fn submit(&self, board: Board) {
        *self.latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(board);
    }
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
    }
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
    /// Call after `is_finished`; the UI never waits for a HID operation.
    pub fn join(mut self) -> Result<(), String> {
        self.thread
            .take()
            .unwrap()
            .join()
            .map_err(|_| "Stream Deck worker panicked".to_string())?
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone, Copy, Debug)]
struct Layout {
    columns: usize,
}
impl Layout {
    fn new(kind: Kind) -> Result<Self, String> {
        if !kind.is_visual()
            || kind.row_count() != 3
            || kind.column_count() != 5
            || kind.key_count() != 15
        {
            return Err(format!(
                "Stream Deck {kind:?} is unsuitable: this dashboard requires a 5×3 display-key layout"
            ));
        }
        Ok(Self {
            columns: usize::from(kind.column_count()),
        })
    }
    fn agent_key(self, index: usize) -> usize {
        (index / 3) * self.columns + index % 3
    }
    fn function_key(self, index: usize) -> usize {
        (index / 2) * self.columns + 3 + index % 2
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Binding {
    Agent(Capture),
    Cycle { group: usize, generation: u64 },
    Page { generation: u64 },
    Brightness,
    None,
}
#[derive(Clone, Debug, PartialEq)]
struct Key {
    visual: Visual,
    binding: Binding,
}

fn keys(
    board: &Board,
    layout: Layout,
    moving: Option<&Capture>,
    brightness: u8,
    elapsed: Duration,
) -> Vec<Key> {
    if board.no_sessions {
        return [
            "R", "T", "🦀", "T", "R", "U", "T", "💻", "T", "U", "S", "Y", "✨", "Y", "S",
        ]
        .into_iter()
        .map(|label| Key {
            visual: Visual::Decorative(label.into()),
            binding: Binding::None,
        })
        .collect();
    }
    let mut keys = vec![
        Key {
            visual: Visual::Decorative(String::new()),
            binding: Binding::None
        };
        15
    ];
    for index in 0..9 {
        let tile = board.tile(board.page * 9 + index);
        let is_moving = moving.is_some_and(|source| same_capture(source, &tile.capture));
        keys[layout.agent_key(index)] = Key {
            visual: Visual::Agent {
                label: tile.label,
                state: tile.state,
                focused: tile.focused,
                reserved: tile.reserved,
                moving: is_moving,
                flash_dim: tile.unseen && !tile.focused && !is_moving && flash_dim(elapsed),
            },
            binding: Binding::Agent(tile.capture),
        };
    }
    for (group, state) in [State::NeedsInput, State::Working, State::Done, State::Idle]
        .into_iter()
        .enumerate()
    {
        keys[layout.function_key(group)] = Key {
            visual: Visual::Function {
                value: board.counts[group].to_string(),
                mark: Mark::State(state),
                dim: moving.is_some() || board.counts[group] == 0,
            },
            binding: Binding::Cycle {
                group,
                generation: board.mode_generation,
            },
        };
    }
    keys[layout.function_key(4)] = Key {
        visual: Visual::Function {
            value: format!("{brightness}%"),
            mark: Mark::Brightness,
            dim: false,
        },
        binding: Binding::Brightness,
    };
    keys[layout.function_key(5)] = Key {
        visual: Visual::Function {
            value: format!("{}/{}", board.page + 1, board.pages()),
            mark: Mark::Page,
            dim: board.pages() == 1,
        },
        binding: Binding::Page {
            generation: board.mode_generation,
        },
    };
    keys
}

fn flash_dim(elapsed: Duration) -> bool {
    (elapsed.as_millis() / 600) % 2 == 1
}

fn same_capture(left: &Capture, right: &Capture) -> bool {
    left.slot == right.slot && left.assignment == right.assignment && left.target == right.target
}
fn live(board: &Board, capture: &Capture) -> bool {
    capture.target.is_some() && same_capture(capture, &board.tile(capture.slot).capture)
}

struct Press {
    key: usize,
    capture: Capture,
    at: Duration,
}
struct Gestures {
    held: [bool; 15],
    suppressed: [bool; 15],
    functions: Vec<Option<Binding>>,
    baseline: bool,
    normal: bool,
    mode_generation: u64,
    press: Option<Press>,
    moving: Option<Capture>,
}
impl Gestures {
    fn new(no_sessions: bool, mode_generation: u64) -> Self {
        Self {
            held: [false; 15],
            suppressed: [false; 15],
            functions: vec![None; 15],
            baseline: false,
            normal: !no_sessions,
            mode_generation,
            press: None,
            moving: None,
        }
    }
    fn cancel(&mut self, emit: &impl Fn(Action)) {
        let active = self.press.take().is_some() || self.moving.is_some();
        if self.moving.take().is_some() {
            emit(Action::Move(None));
        }
        if active {
            emit(Action::Gesture(None));
        }
        self.suppressed = self.held;
        self.functions.fill(None);
    }
    fn reconcile(&mut self, board: &Board, emit: &impl Fn(Action)) {
        let normal = !board.no_sessions && board.screens_awake;
        let changed_mode = self.mode_generation != board.mode_generation || self.normal != normal;
        let invalid = self
            .press
            .as_ref()
            .is_some_and(|press| !live(board, &press.capture))
            || self
                .moving
                .as_ref()
                .is_some_and(|source| !live(board, source));
        if changed_mode || invalid {
            self.cancel(emit);
        }
        self.normal = normal;
        self.mode_generation = board.mode_generation;
    }
    fn advance(&mut self, now: Duration, emit: &impl Fn(Action)) {
        if self.moving.is_none()
            && let Some(press) = &self.press
            && self.held[press.key]
            && now.saturating_sub(press.at) >= Duration::from_secs(2)
        {
            let source = self.press.take().unwrap().capture;
            self.moving = Some(source);
            for function in &mut self.functions {
                if matches!(function, Some(Binding::Cycle { .. })) {
                    *function = None;
                }
            }
            emit(Action::Move(Some(source)));
        }
    }
    fn buttons(
        &mut self,
        states: &[bool],
        displayed: &[Option<Key>],
        now: Duration,
        emit: &impl Fn(Action),
    ) -> bool {
        if states.len() < 15 {
            return false;
        }
        if !self.baseline {
            self.held.copy_from_slice(&states[..15]);
            self.suppressed = self.held;
            self.baseline = true;
            return false;
        }
        self.advance(now, emit);
        let mut brightness = false;
        for (index, &down) in states.iter().take(15).enumerate() {
            if self.held[index] == down {
                continue;
            }
            self.held[index] = down;
            if self.suppressed[index] {
                if !down {
                    self.suppressed[index] = false;
                    self.functions[index] = None;
                }
                continue;
            }
            if !self.normal {
                continue;
            }
            if down {
                let Some(key) = &displayed[index] else {
                    continue;
                };
                match &key.binding {
                    Binding::Agent(destination) => {
                        if let Some(source) = self.moving.take() {
                            if source.slot != destination.slot {
                                emit(Action::Swap {
                                    source,
                                    destination: *destination,
                                });
                            }
                            emit(Action::Move(None));
                            emit(Action::Gesture(None));
                            self.press = None;
                            for (index, &held) in states.iter().take(15).enumerate() {
                                if held
                                    && matches!(
                                        displayed[index].as_ref().map(|key| &key.binding),
                                        Some(Binding::Agent(_))
                                    )
                                {
                                    self.suppressed[index] = true;
                                }
                            }
                        } else if self.press.is_none() && destination.target.is_some() {
                            self.press = Some(Press {
                                key: index,
                                capture: *destination,
                                at: now,
                            });
                            emit(Action::Gesture(Some(*destination)));
                        }
                    }
                    Binding::Cycle { .. } if self.moving.is_some() => {}
                    Binding::None => {}
                    binding => self.functions[index] = Some(binding.clone()),
                }
            } else {
                if self.press.as_ref().is_some_and(|press| press.key == index) {
                    let press = self.press.take().unwrap();
                    emit(Action::Focus(press.capture));
                    emit(Action::Gesture(None));
                }
                match self.functions[index].take() {
                    Some(Binding::Brightness) => brightness = true,
                    Some(Binding::Page { generation }) => emit(Action::Page { generation }),
                    Some(Binding::Cycle { group, generation }) if self.moving.is_none() => {
                        emit(Action::Cycle { group, generation })
                    }
                    _ => {}
                }
            }
        }
        brightness
    }
}

trait Device {
    fn read(&self, timeout: Duration) -> Result<Option<Vec<bool>>, String>;
    fn upload(&self, index: usize, image: RgbImage) -> Result<(), String>;
    fn brightness(&self, value: u8) -> Result<(), String>;
}
impl Device for StreamDeck {
    fn read(&self, timeout: Duration) -> Result<Option<Vec<bool>>, String> {
        match self.read_input(Some(timeout)).map_err(|e| e.to_string())? {
            StreamDeckInput::ButtonStateChange(buttons) if !buttons.is_empty() => Ok(Some(buttons)),
            _ => Ok(None),
        }
    }
    fn upload(&self, index: usize, image: RgbImage) -> Result<(), String> {
        self.set_button_image(index as u8, DynamicImage::ImageRgb8(image))
            .map_err(|e| e.to_string())?;
        self.flush().map_err(|e| e.to_string())
    }
    fn brightness(&self, value: u8) -> Result<(), String> {
        self.set_brightness(value).map_err(|e| e.to_string())
    }
}

fn connected(
    candidate: Candidate,
    initial: Board,
    latest: &Mutex<Option<Board>>,
    stopping: &AtomicBool,
    emit: &impl Fn(Action),
) -> Result<(), String> {
    let layout = Layout::new(candidate.kind)?;
    if stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    let _scope = HidScope::new();
    let hid = elgato_streamdeck::new_hidapi().map_err(|e| e.to_string())?;
    let device = StreamDeck::connect(&hid, candidate.kind, &candidate.serial)
        .map_err(|e| format!("Cannot open Stream Deck: {e}"))?;
    let mut renderer = Renderer::new(candidate.kind.key_image_format().size)?;
    let result = run(
        &device,
        &mut |visual| renderer.render(visual),
        initial,
        layout,
        latest,
        stopping,
        emit,
    );
    // Never reset a device after an I/O failure; another controller may have acquired it.
    if result.is_ok() {
        device.reset().map_err(|e| e.to_string())?;
    }
    result
}

fn run(
    device: &impl Device,
    render: &mut impl FnMut(&Visual) -> Result<RgbImage, String>,
    mut board: Board,
    layout: Layout,
    latest: &Mutex<Option<Board>>,
    stopping: &AtomicBool,
    emit: &impl Fn(Action),
) -> Result<(), String> {
    if stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    let mut brightness = board.brightness;
    let mut screens_awake = board.screens_awake;
    device.brightness(if screens_awake { brightness } else { 0 })?;
    let mut gestures = Gestures::new(board.no_sessions, board.mode_generation);
    let mut displayed = vec![None; 15];
    let mut next_upload = 0;
    let start = Instant::now();
    let result = (|| {
        while !stopping.load(Ordering::Acquire) {
            if let Some(latest) = latest.lock().unwrap_or_else(|e| e.into_inner()).take() {
                board = latest;
            }
            gestures.reconcile(&board, emit);
            if screens_awake != board.screens_awake {
                device.brightness(if board.screens_awake { brightness } else { 0 })?;
                screens_awake = board.screens_awake;
                if screens_awake {
                    // Refresh retained artwork in case system sleep also reset the USB display.
                    displayed.fill(None);
                }
            }
            gestures.advance(start.elapsed(), emit);
            if screens_awake {
                let desired = keys(
                    &board,
                    layout,
                    gestures.moving.as_ref(),
                    brightness,
                    start.elapsed(),
                );
                // Keep input and hold deadlines responsive between individual image writes.
                for offset in 0..desired.len() {
                    let index = (next_upload + offset) % desired.len();
                    let key = desired[index].clone();
                    if displayed[index]
                        .as_ref()
                        .is_none_or(|old: &Key| old.visual != key.visual)
                    {
                        device.upload(index, render(&key.visual)?)?;
                        displayed[index] = Some(key);
                        next_upload = (index + 1) % desired.len();
                        break;
                    }
                    displayed[index] = Some(key);
                }
            }
            // Some firmware sends only changes; drain startup reports before accepting fresh presses.
            gestures.baseline = screens_awake && displayed.iter().all(Option::is_some);
            if let Some(buttons) = device.read(Duration::from_millis(20))?
                && gestures.buttons(&buttons, &displayed, start.elapsed(), emit)
            {
                let next = next_brightness(brightness);
                device.brightness(next)?;
                brightness = next;
                emit(Action::Brightness(brightness));
            }
        }
        Ok(())
    })();
    gestures.cancel(emit);
    result
}

fn next_brightness(value: u8) -> u8 {
    match value {
        25 => 50,
        50 => 75,
        75 => 100,
        _ => 25,
    }
}

/// Offline contact sheet: normal board, dim flash phase, move board, no-session board.
pub fn preview(path: &std::path::Path) -> Result<(), String> {
    use crate::deck::{Target, Tile};
    let mut renderer = Renderer::new((72, 72))?;
    let states = [
        State::Working,
        State::NeedsInput,
        State::Done,
        State::Idle,
        State::Error,
        State::Paused,
        State::Unknown,
    ];
    let tiles = (0..9)
        .map(|slot| Tile {
            capture: Capture {
                slot,
                assignment: Some(slot as u64 + 1),
                target: if slot < 7 {
                    Some(Target {
                        pane: slot as u64 + 1,
                        generation: 1,
                    })
                } else {
                    None
                },
                layout_generation: 1,
            },
            label: [
                "foo-bar",
                "fix-login",
                "api-cache",
                "schema-v2",
                "long-worktree-task-slug ·2",
                "e\u{301}-👩‍💻-Δ",
                "foo-bar ·2",
                "old-task",
                "",
            ][slot]
                .into(),
            state: states.get(slot).copied(),
            focused: slot == 0,
            reserved: slot == 7,
            unseen: slot < 7,
        })
        .collect();
    let mut board = Board {
        mode_generation: 1,
        generation: 1,
        page: 0,
        tiles,
        counts: [1, 1, 1, 1],
        no_sessions: false,
        brightness: 100,
        screens_awake: true,
    };
    let layout = Layout::new(Kind::Mk2Scissor)?;
    let mut sheet = RgbImage::from_pixel(5 * 80 + 8, 4 * 3 * 80 + 32, image::Rgb([13, 15, 17]));
    for section in 0..4 {
        if section == 3 {
            board.no_sessions = true;
        }
        let source = (section == 2).then(|| board.tile(0).capture);
        for (index, key) in keys(
            &board,
            layout,
            source.as_ref(),
            board.brightness,
            if section == 1 {
                Duration::from_millis(600)
            } else {
                Duration::ZERO
            },
        )
        .iter()
        .enumerate()
        {
            image::imageops::replace(
                &mut sheet,
                &renderer.render(&key.visual)?,
                (8 + (index % 5) * 80) as i64,
                (8 + section * 248 + (index / 5) * 80) as i64,
            );
        }
    }
    sheet.save(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::{Target, Tile};
    use std::cell::{Cell, RefCell};

    fn board() -> Board {
        Board {
            mode_generation: 1,
            generation: 1,
            page: 0,
            counts: [0, 1, 0, 0],
            brightness: 100,
            screens_awake: true,
            no_sessions: false,
            tiles: (0..19)
                .map(|slot| Tile {
                    capture: Capture {
                        slot,
                        assignment: (slot != 2).then_some(slot as u64 + 1),
                        target: (slot != 2 && slot != 3).then_some(Target {
                            pane: slot as u64 + 1,
                            generation: 1,
                        }),
                        layout_generation: 1,
                    },
                    label: format!("Agent {slot}"),
                    state: (slot != 2 && slot != 3).then_some(State::Working),
                    focused: false,
                    reserved: slot == 3,
                    unseen: false,
                })
                .collect(),
        }
    }
    fn shown(board: &Board) -> Vec<Option<Key>> {
        keys(
            board,
            Layout::new(Kind::Mk2Scissor).unwrap(),
            None,
            100,
            Duration::ZERO,
        )
        .into_iter()
        .map(Some)
        .collect()
    }
    fn input(
        gesture: &mut Gestures,
        keys: &[Option<Key>],
        held: &[usize],
        time: u64,
        actions: &RefCell<Vec<Action>>,
    ) -> bool {
        let states: Vec<_> = (0..15).map(|index| held.contains(&index)).collect();
        gesture.buttons(&states, keys, Duration::from_millis(time), &|action| {
            actions.borrow_mut().push(action)
        })
    }

    #[test]
    fn flashing_changes_only_unseen_agent_artwork_and_never_key_targets() {
        let layout = Layout::new(Kind::Mk2Scissor).unwrap();
        let mut board = board();
        board.tiles[0].unseen = true;
        board.tiles[0].state = Some(State::NeedsInput);
        let bright = keys(&board, layout, None, 100, Duration::from_millis(599));
        let dim = keys(&board, layout, None, 100, Duration::from_millis(600));
        for index in 0..15 {
            assert_eq!(bright[index].binding, dim[index].binding);
            assert_eq!(bright[index].visual == dim[index].visual, index != 0);
        }
        assert_eq!(
            bright,
            keys(&board, layout, None, 100, Duration::from_millis(1200))
        );
        board.tiles[0].focused = true;
        assert!(matches!(
            keys(&board, layout, None, 100, Duration::from_millis(600))[0].visual,
            Visual::Agent {
                flash_dim: false,
                ..
            }
        ));
        board.tiles[0].focused = false;
        let source = board.tile(0).capture;
        assert!(matches!(
            keys(
                &board,
                layout,
                Some(&source),
                100,
                Duration::from_millis(600)
            )[0]
            .visual,
            Visual::Agent {
                flash_dim: false,
                moving: true,
                ..
            }
        ));
        board.no_sessions = true;
        assert_eq!(
            keys(&board, layout, None, 100, Duration::ZERO),
            keys(&board, layout, None, 100, Duration::from_millis(600))
        );
    }
    #[test]
    fn exact_regions_and_rejected_models() {
        let layout = Layout::new(Kind::Mk2Scissor).unwrap();
        assert_eq!(
            (0..9).map(|i| layout.agent_key(i)).collect::<Vec<_>>(),
            [0, 1, 2, 5, 6, 7, 10, 11, 12]
        );
        assert_eq!(
            (0..6).map(|i| layout.function_key(i)).collect::<Vec<_>>(),
            [3, 4, 8, 9, 13, 14]
        );
        for kind in [
            Kind::Pedal,
            Kind::Mini,
            Kind::Xl,
            Kind::Neo,
            Kind::Plus,
            Kind::PlusXl,
        ] {
            assert!(Layout::new(kind).is_err());
        }
    }

    #[test]
    fn tap_boundary_hold_and_consumed_releases() {
        let board = board();
        let keys = shown(&board);
        let actions = RefCell::new(Vec::new());
        let mut gesture = Gestures::new(false, 1);
        input(&mut gesture, &keys, &[], 0, &actions);
        input(&mut gesture, &keys, &[0], 0, &actions);
        input(&mut gesture, &keys, &[], 1999, &actions);
        assert!(
            actions
                .borrow()
                .iter()
                .any(|a| matches!(a, Action::Focus(_)))
        );
        actions.borrow_mut().clear();
        input(&mut gesture, &keys, &[0], 3000, &actions);
        gesture.advance(Duration::from_millis(4999), &|a| {
            actions.borrow_mut().push(a)
        });
        assert!(gesture.moving.is_none());
        gesture.advance(Duration::from_millis(5000), &|a| {
            actions.borrow_mut().push(a)
        });
        assert!(gesture.moving.is_some());
        input(&mut gesture, &keys, &[], 5001, &actions);
        assert!(gesture.moving.is_some());
        input(&mut gesture, &keys, &[1], 5002, &actions);
        input(&mut gesture, &keys, &[], 5003, &actions);
        assert_eq!(
            actions
                .borrow()
                .iter()
                .filter(|a| matches!(a, Action::Swap { .. }))
                .count(),
            1
        );
        assert!(
            !actions
                .borrow()
                .iter()
                .any(|a| matches!(a, Action::Focus(_)))
        );
    }

    #[test]
    fn source_held_empty_reserved_and_already_held_destination() {
        for destination in [1, 2, 5] {
            let keys = shown(&board());
            let actions = RefCell::new(Vec::new());
            let mut gesture = Gestures::new(false, 1);
            input(&mut gesture, &keys, &[], 0, &actions);
            input(&mut gesture, &keys, &[0], 1, &actions);
            input(&mut gesture, &keys, &[0, 1], 2, &actions);
            input(&mut gesture, &keys, &[0, 1], 2001, &actions);
            assert!(gesture.moving.is_some());
            input(&mut gesture, &keys, &[0], 2002, &actions);
            input(&mut gesture, &keys, &[0, destination], 2003, &actions);
            input(&mut gesture, &keys, &[], 2004, &actions);
            assert_eq!(
                actions
                    .borrow()
                    .iter()
                    .filter(|a| matches!(a, Action::Swap { .. }))
                    .count(),
                1
            );
            assert!(
                !actions
                    .borrow()
                    .iter()
                    .any(|a| matches!(a, Action::Focus(_)))
            );
        }
    }

    #[test]
    fn cross_page_functions_cancellation_and_source_identity() {
        let mut board = board();
        let mut keys = shown(&board);
        let actions = RefCell::new(Vec::new());
        let mut gesture = Gestures::new(false, 1);
        input(&mut gesture, &keys, &[], 0, &actions);
        input(&mut gesture, &keys, &[0], 0, &actions);
        input(&mut gesture, &keys, &[], 2000, &actions);
        input(&mut gesture, &keys, &[14, 13, 3], 2001, &actions);
        assert!(input(&mut gesture, &keys, &[], 2002, &actions));
        assert!(gesture.moving.is_some());
        assert!(
            actions
                .borrow()
                .iter()
                .any(|a| matches!(a, Action::Page { .. }))
        );
        assert!(
            !actions
                .borrow()
                .iter()
                .any(|a| matches!(a, Action::Cycle { .. }))
        );
        board.page = 1;
        keys = shown(&board);
        input(&mut gesture, &keys, &[2], 2003, &actions);
        assert!(
            actions
                .borrow()
                .iter()
                .any(|a| matches!(a,Action::Swap{destination,..} if destination.slot==11))
        );
        input(&mut gesture, &keys, &[], 2004, &actions);
        input(&mut gesture, &keys, &[0], 3000, &actions);
        board.tiles[9].capture.target.as_mut().unwrap().generation += 1;
        gesture.reconcile(&board, &|a| actions.borrow_mut().push(a));
        input(&mut gesture, &keys, &[], 3001, &actions);
        assert!(
            !actions
                .borrow()
                .iter()
                .any(|a| matches!(a, Action::Focus(_)))
        );
    }

    #[test]
    fn no_session_transition_is_inert_and_suppresses_held_keys() {
        let mut board = board();
        let keys = shown(&board);
        let actions = RefCell::new(Vec::new());
        let mut gesture = Gestures::new(false, 1);
        input(&mut gesture, &keys, &[0, 13], 0, &actions);
        input(&mut gesture, &keys, &[], 1, &actions);
        assert!(actions.borrow().is_empty());
        input(&mut gesture, &keys, &[0], 2, &actions);
        board.no_sessions = true;
        gesture.reconcile(&board, &|a| actions.borrow_mut().push(a));
        let decorative = shown(&board);
        assert!(
            decorative
                .iter()
                .all(|key| matches!(key.as_ref().unwrap().binding, Binding::None))
        );
        input(&mut gesture, &decorative, &[0, 13], 3000, &actions);
        board.no_sessions = false;
        gesture.reconcile(&board, &|a| actions.borrow_mut().push(a));
        assert!(!input(&mut gesture, &keys, &[], 3001, &actions));
        assert!(
            !actions
                .borrow()
                .iter()
                .any(|a| matches!(a, Action::Focus(_) | Action::Swap { .. }))
        );
        let labels: Vec<_> = decorative
            .iter()
            .map(|key| match &key.as_ref().unwrap().visual {
                Visual::Decorative(s) => s.as_str(),
                _ => panic!(),
            })
            .collect();
        assert_eq!(
            labels,
            [
                "R", "T", "🦀", "T", "R", "U", "T", "💻", "T", "U", "S", "Y", "✨", "Y", "S"
            ]
        );
    }

    #[test]
    fn selecting_source_cancels_and_simultaneous_destinations_are_consumed() {
        let board = board();
        let keys = shown(&board);
        for destination in [vec![0], vec![1, 2]] {
            let actions = RefCell::new(Vec::new());
            let mut gesture = Gestures::new(false, 1);
            input(&mut gesture, &keys, &[], 0, &actions);
            input(&mut gesture, &keys, &[0], 1, &actions);
            input(&mut gesture, &keys, &[], 2001, &actions);
            input(&mut gesture, &keys, &destination, 2002, &actions);
            input(&mut gesture, &keys, &[], 2003, &actions);
            assert!(gesture.press.is_none() && gesture.moving.is_none());
            assert!(
                !actions
                    .borrow()
                    .iter()
                    .any(|a| matches!(a, Action::Focus(_)))
            );
            assert_eq!(
                actions
                    .borrow()
                    .iter()
                    .filter(|a| matches!(a, Action::Swap { .. }))
                    .count(),
                usize::from(destination != [0])
            );
        }
    }

    #[test]
    fn coalesced_board_mode_transitions_cancel_gestures() {
        let mut board = board();
        let keys = shown(&board);
        let actions = RefCell::new(Vec::new());
        let mut gesture = Gestures::new(false, 1);
        input(&mut gesture, &keys, &[], 0, &actions);
        input(&mut gesture, &keys, &[0, 13], 1, &actions);
        board.mode_generation += 2;
        gesture.reconcile(&board, &|a| actions.borrow_mut().push(a));
        assert!(!input(&mut gesture, &keys, &[], 2001, &actions));
        assert!(gesture.press.is_none() && gesture.moving.is_none());
        assert!(
            !actions
                .borrow()
                .iter()
                .any(|a| matches!(a, Action::Focus(_)))
        );
    }

    #[test]
    fn swap_consumes_agents_but_preserves_held_page_and_brightness() {
        let keys = shown(&board());
        let actions = RefCell::new(Vec::new());
        let mut gesture = Gestures::new(false, 1);
        input(&mut gesture, &keys, &[], 0, &actions);
        input(&mut gesture, &keys, &[0, 3], 1, &actions);
        gesture.advance(Duration::from_millis(2001), &|a| {
            actions.borrow_mut().push(a)
        });
        input(&mut gesture, &keys, &[0, 3, 13, 14], 2002, &actions);
        input(&mut gesture, &keys, &[0, 1, 3, 13, 14], 2003, &actions);
        assert!(input(&mut gesture, &keys, &[], 2004, &actions));
        assert!(
            actions
                .borrow()
                .iter()
                .any(|a| matches!(a, Action::Page { .. }))
        );
        assert!(
            !actions
                .borrow()
                .iter()
                .any(|a| matches!(a, Action::Focus(_) | Action::Cycle { .. }))
        );
    }

    struct FakeDevice<'a> {
        brightness: RefCell<Vec<u8>>,
        fail_brightness: bool,
        quiet_start: bool,
        reads: Cell<usize>,
        uploads: RefCell<Vec<usize>>,
        stopping: &'a AtomicBool,
        latest: &'a Mutex<Option<Board>>,
    }
    impl Device for FakeDevice<'_> {
        fn read(&self, _: Duration) -> Result<Option<Vec<bool>>, String> {
            let count = self.reads.get() + 1;
            self.reads.set(count);
            if count > 18 {
                self.stopping.store(true, Ordering::Release);
            }
            if self.quiet_start && count < 16 {
                return Ok(None);
            }
            let mut keys = vec![false; 15];
            keys[13] = count == 16 || count == 17;
            Ok(Some(keys))
        }
        fn upload(&self, index: usize, _: RgbImage) -> Result<(), String> {
            self.uploads.borrow_mut().push(index);
            if self.uploads.borrow().len() == 1 {
                let mut board = board();
                board.tiles[0].label = "Latest".into();
                *self.latest.lock().unwrap() = Some(board.clone());
                board.tiles[0].label = "Newest".into();
                *self.latest.lock().unwrap() = Some(board);
            }
            Ok(())
        }
        fn brightness(&self, value: u8) -> Result<(), String> {
            if value != 100 && self.fail_brightness {
                return Err("disconnected".into());
            }
            self.brightness.borrow_mut().push(value);
            Ok(())
        }
    }
    #[test]
    fn coalescing_successful_mapping_and_brightness_failure() {
        for (failure, quiet_start) in [(false, false), (false, true), (true, false)] {
            let stopping = AtomicBool::new(false);
            let latest = Mutex::new(None);
            let events = RefCell::new(Vec::new());
            let visuals = RefCell::new(Vec::new());
            let device = FakeDevice {
                brightness: RefCell::new(Vec::new()),
                fail_brightness: failure,
                quiet_start,
                reads: Cell::new(0),
                uploads: RefCell::new(Vec::new()),
                stopping: &stopping,
                latest: &latest,
            };
            let result = run(
                &device,
                &mut |v| {
                    visuals.borrow_mut().push(v.clone());
                    Ok(RgbImage::new(72, 72))
                },
                board(),
                Layout::new(Kind::Mk2Scissor).unwrap(),
                &latest,
                &stopping,
                &|e| events.borrow_mut().push(e),
            );
            assert_eq!(result.is_err(), failure);
            assert!(
                visuals
                    .borrow()
                    .iter()
                    .any(|v| matches!(v,Visual::Agent{label,..} if label=="Newest"))
            );
            assert!(
                !visuals
                    .borrow()
                    .iter()
                    .any(|v| matches!(v,Visual::Agent{label,..} if label=="Latest"))
            );
            assert_eq!(
                events
                    .borrow()
                    .iter()
                    .filter(|e| matches!(e, Action::Brightness(25)))
                    .count(),
                usize::from(!failure)
            );
            assert!(device.reads.get() >= 18);
        }
        assert_eq!(
            [100, 25, 50, 75, 100].map(next_brightness),
            [25, 50, 75, 100, 25]
        );
    }

    #[test]
    fn screen_sleep_dims_retains_state_and_suppresses_held_keys_on_wake() {
        struct PowerDevice<'a> {
            board: Board,
            reads: Cell<usize>,
            writes: RefCell<Vec<u8>>,
            uploads: RefCell<Vec<(usize, usize)>>,
            fail_write: Option<usize>,
            latest: &'a Mutex<Option<Board>>,
            stopping: &'a AtomicBool,
        }
        impl Device for PowerDevice<'_> {
            fn read(&self, _: Duration) -> Result<Option<Vec<bool>>, String> {
                let count = self.reads.get() + 1;
                self.reads.set(count);
                if count == 17 || count == 19 || count == 20 {
                    let mut board = self.board.clone();
                    board.screens_awake = count == 20;
                    board.mode_generation += count as u64;
                    board.tiles[0].label = "Updated while asleep".into();
                    *self.latest.lock().unwrap() = Some(board);
                }
                if count == 40 {
                    self.stopping.store(true, Ordering::Release);
                }
                let mut keys = vec![false; 15];
                keys[0] = (16..37).contains(&count);
                keys[13] = count == 18 || count == 38;
                Ok(Some(keys))
            }
            fn upload(&self, index: usize, _: RgbImage) -> Result<(), String> {
                self.uploads.borrow_mut().push((self.reads.get(), index));
                Ok(())
            }
            fn brightness(&self, value: u8) -> Result<(), String> {
                self.writes.borrow_mut().push(value);
                if self.fail_write == Some(self.writes.borrow().len()) {
                    return Err("power write failed".into());
                }
                Ok(())
            }
        }
        for (initially_asleep, fail_write) in [
            (false, None),
            (true, None),
            (false, Some(2)),
            (false, Some(3)),
        ] {
            let stopping = AtomicBool::new(false);
            let latest = Mutex::new(None);
            let events = RefCell::new(Vec::new());
            let visuals = RefCell::new(Vec::new());
            let mut board = board();
            board.brightness = 25;
            board.screens_awake = !initially_asleep;
            let device = PowerDevice {
                board: board.clone(),
                reads: Cell::new(0),
                writes: RefCell::new(Vec::new()),
                uploads: RefCell::new(Vec::new()),
                fail_write,
                latest: &latest,
                stopping: &stopping,
            };
            let result = run(
                &device,
                &mut |visual| {
                    visuals.borrow_mut().push(visual.clone());
                    Ok(RgbImage::new(72, 72))
                },
                board,
                Layout::new(Kind::Mk2Scissor).unwrap(),
                &latest,
                &stopping,
                &|event| events.borrow_mut().push(event),
            );
            assert_eq!(result.is_err(), fail_write.is_some());
            assert!(!events.borrow().iter().any(|event| matches!(
                event,
                Action::Focus(_) | Action::Swap { .. } | Action::Cycle { .. } | Action::Page { .. }
            )));
            assert!(
                !device
                    .uploads
                    .borrow()
                    .iter()
                    .any(|(at, _)| (17..20).contains(at))
            );
            if fail_write.is_none() {
                assert_eq!(
                    *device.writes.borrow(),
                    if initially_asleep {
                        vec![0, 25, 50]
                    } else {
                        vec![25, 0, 25, 50]
                    }
                );
                assert_eq!(
                    events
                        .borrow()
                        .iter()
                        .filter(|event| matches!(event, Action::Brightness(_)))
                        .cloned()
                        .collect::<Vec<_>>(),
                    [Action::Brightness(50)]
                );
                assert!(visuals.borrow().iter().any(|visual| matches!(visual, Visual::Agent { label, .. } if label == "Updated while asleep")));
                assert_eq!(
                    device
                        .uploads
                        .borrow()
                        .iter()
                        .filter(|(at, _)| *at >= 20 && *at < 35)
                        .count(),
                    15
                );
            } else {
                assert!(
                    !events
                        .borrow()
                        .iter()
                        .any(|event| matches!(event, Action::Brightness(_)))
                );
            }
        }
    }
    #[test]
    fn reconnect_restores_last_successful_brightness_and_shutdown_skips_rendering() {
        for cancelled in [false, true] {
            let stopping = AtomicBool::new(cancelled);
            let latest = Mutex::new(None);
            let device = FakeDevice {
                brightness: RefCell::new(Vec::new()),
                fail_brightness: false,
                quiet_start: false,
                reads: Cell::new(0),
                uploads: RefCell::new(Vec::new()),
                stopping: &stopping,
                latest: &latest,
            };
            let mut board = board();
            board.brightness = 75;
            run(
                &device,
                &mut |_| Ok(RgbImage::new(72, 72)),
                board,
                Layout::new(Kind::Mk2Scissor).unwrap(),
                &latest,
                &stopping,
                &|_| {},
            )
            .unwrap();
            if cancelled {
                assert!(device.uploads.borrow().is_empty());
                assert!(device.brightness.borrow().is_empty());
                assert_eq!(device.reads.get(), 0);
            } else {
                assert_eq!(*device.brightness.borrow(), [75, 100]);
            }
        }
    }

    #[test]
    #[ignore = "owns and changes the connected Stream Deck; close its existing controller first"]
    fn hardware_usb_smoke() {
        let candidate = discover(None)
            .unwrap()
            .expect("connect one 5×3 Stream Deck");
        #[cfg(target_os = "macos")]
        let _scope = HidScope::new();
        let hid = elgato_streamdeck::new_hidapi().unwrap();
        let device = StreamDeck::connect(&hid, candidate.kind, &candidate.serial).unwrap();
        eprintln!(
            "Stream Deck {:?}; firmware {}; serial {}; key size {:?}",
            candidate.kind,
            device.firmware_version().unwrap(),
            candidate.serial,
            candidate.kind.key_image_format().size
        );
        let mut renderer = Renderer::new(candidate.kind.key_image_format().size).unwrap();
        let layout = Layout::new(candidate.kind).unwrap();
        let mut board = board();
        for no_sessions in [false, true] {
            board.no_sessions = no_sessions;
            for (index, key) in keys(&board, layout, None, 100, Duration::ZERO)
                .iter()
                .enumerate()
            {
                device
                    .upload(index, renderer.render(&key.visual).unwrap())
                    .unwrap();
            }
        }
        for value in [100, 25, 50, 75, 100] {
            device.brightness(value).unwrap();
        }
        let _ = device.read(Duration::from_millis(20)).unwrap();
        device.reset().unwrap();
        eprintln!(
            "Normal/decorative uploads, brightness writes, finite input read, and reset succeeded; no physical gestures asserted."
        );
    }

    #[test]
    #[ignore = "opens the connected Stream Deck on successive threads; close its controller first"]
    fn hardware_cross_thread_reconnect() {
        for _ in 0..3 {
            let candidate = thread::spawn(|| discover(None))
                .join()
                .unwrap()
                .unwrap()
                .expect("connect one 5×3 Stream Deck");
            thread::spawn(move || {
                #[cfg(target_os = "macos")]
                let _scope = HidScope::new();
                let hid = elgato_streamdeck::new_hidapi().unwrap();
                let device = StreamDeck::connect(&hid, candidate.kind, &candidate.serial).unwrap();
                assert!(!device.firmware_version().unwrap().is_empty());
                let _ = device.read(Duration::from_millis(20)).unwrap();
            })
            .join()
            .unwrap();
        }
        eprintln!(
            "Three discovery/open/read/close cycles on separate threads passed; no images or brightness changed."
        );
    }

    #[test]
    #[ignore = "writes a disposable offscreen preview; no USB"]
    fn contact_sheet() {
        let path = std::env::var_os("RUSTTY_DECK_PREVIEW").expect("set RUSTTY_DECK_PREVIEW");
        preview(std::path::Path::new(&path)).unwrap();
    }
}
