#[path = "clipboard_preview.rs"]
mod clipboard_preview;
#[path = "command_palette.rs"]
mod command_palette;
#[path = "deck_host.rs"]
mod deck_host;
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[path = "handoff_ui.rs"]
mod handoff_ui;
#[cfg(target_os = "macos")]
#[path = "desktop_cursor.rs"]
mod native_cursor;
#[cfg(target_os = "macos")]
#[path = "desktop_progress.rs"]
mod native_progress;
#[path = "outliner_host.rs"]
mod outliner_host;
#[path = "desktop_painter.rs"]
mod painter;
#[path = "smoke.rs"]
mod smoke;
#[path = "theme_preview.rs"]
mod theme_preview;
#[path = "tmuxx_host.rs"]
mod tmuxx_host;
#[path = "tmuxx_theme.rs"]
mod tmuxx_theme;
use command_palette::{CommandPalette, PaletteEvent};
use egui::{Color32, Pos2, Sense, Vec2, ViewportId};
use rustty::{
    app_paths,
    config::{self, Action, Config, Direction, LoadedConfig},
    session::{Input as SessionInput, Session, SessionEvent, SessionOptions},
    vt,
};
use rustty_app::{
    accessibility::TerminalText,
    input, outliner, outliner_ui,
    platform::{Platform, PlatformEvent},
    presentation::{
        Activity, CompletionFlash, DirectoryLabel, FocusHint, Progress, TabAccent, common_label,
        cursor_blink_phase, directory_labels, paint_zoom_icon, zoom_label,
    },
    search::{self, Search},
    workspace::{self, Axis, Id, Peek, Rect, SavedPane, Tab, WindowState, Workspace},
    zoom_animation,
};
use rustty_font::{FontConfig, FontFeature};
use rustty_render::{Frame, RenderOptions};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
#[cfg(target_os = "windows")]
use winit::platform::windows::WindowAttributesExtWindows;
#[cfg(target_os = "macos")]
use winit::platform::{macos::WindowAttributesExtMacOS, scancode::PhysicalKeyExtScancode};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalPosition, LogicalSize},
    event::{ElementState, Ime, Modifiers, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::PhysicalKey,
    window::{CursorIcon, Fullscreen, Theme, Window, WindowId},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const INPUT_BUDGET: usize = 16 * 1024 * 1024;
const MIN_WINDOW_SIZE: LogicalSize<f64> = LogicalSize::new(240.0, 120.0);

#[derive(Debug)]
enum Event {
    DeckWake,
    Tmuxx,
    TmuxxCopied(
        tmuxx_host::Target,
        String,
        std::result::Result<rustty_app::tmuxx_copy::CopiedWindow, String>,
    ),
    Output(Id),
    Platform(PlatformEvent),
    Repaint(egui::RequestRepaintInfo, Instant),
    Access(egui_winit::accesskit_winit::Event),
}
impl From<egui_winit::accesskit_winit::Event> for Event {
    fn from(value: egui_winit::accesskit_winit::Event) -> Self {
        Self::Access(value)
    }
}

fn dispatch_binding(
    binding: config::KeyBinding,
    mut dispatch: impl FnMut(Action, bool) -> bool,
) -> bool {
    let mut performed = false;
    for action in binding.actions {
        performed |= dispatch(action, binding.flags.all);
    }
    performed
}

struct Pane {
    agent: rustty_app::deck::Report,
    deck_progress: rustty_app::deck::ProgressReport,
    deck_command: rustty_app::deck::CommandReport,
    session: Session,
    host_state: (bool, bool, vt::query::ColorScheme),
    wake_pending: Arc<AtomicBool>,
    input: VecDeque<SessionInput>,
    input_bytes: usize,
    title: String,
    title_override: Option<String>,
    cwd: PathBuf,
    last_directory_report: String,
    running: Option<Instant>,
    activity: Activity,
    unseen: bool,
    exited: bool,
    started: Instant,
    exit_message: Option<String>,
    links: vt::search::LinkMatcher,
    selection_gesture: vt::selection_gesture::SelectionGesture,
    mouse_cell: Option<[u16; 2]>,
    scroll: input::ScrollAccumulator,
    sync_output: SynchronizedOutput,
    search: Option<Search>,
}

struct PaneStartup {
    directory: PathBuf,
    saved: Option<SavedPane>,
    reload_config: bool,
    host_state: (bool, bool, vt::query::ColorScheme),
    wake_pending: Arc<AtomicBool>,
    started: Instant,
    input: VecDeque<SessionInput>,
    input_bytes: usize,
}

impl Pane {
    fn reported_active(&self) -> bool {
        !self.exited && self.agent.reported_active(&self.activity)
    }

    fn has_deck_report(&self) -> bool {
        !self.exited
            && (self.agent.is_registered()
                || self.deck_command.is_registered()
                || self.deck_progress.state().is_some())
    }

    fn deck_view(&self, id: Id, label: &str) -> Option<rustty_app::deck::PaneView> {
        if self.exited {
            return None;
        }
        self.agent
            .view(id, label, &self.activity)
            .or_else(|| self.deck_command.view(id, label))
    }

    fn reset_selection_gesture(&mut self) {
        if let Ok(mut terminal) = self.session.terminal() {
            self.selection_gesture.reset(&mut terminal);
        }
    }

    fn update_saved(&self, saved: &mut SavedPane) -> bool {
        let title = (!self.title.is_empty()).then_some(self.title.as_str());
        if saved.working_directory == self.cwd
            && saved.title.as_deref() == title
            && saved.title_override == self.title_override
        {
            return false;
        }
        saved.working_directory.clone_from(&self.cwd);
        saved.title = title.map(str::to_owned);
        saved.title_override.clone_from(&self.title_override);
        true
    }

    fn send_input(&mut self, input: SessionInput) -> std::io::Result<()> {
        QueuedInput {
            session: &self.session,
            queue: &mut self.input,
            bytes: &mut self.input_bytes,
        }
        .enqueue(input)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if !self.session.accepts_input() {
            self.input.clear();
            self.input_bytes = 0;
            return Ok(());
        }
        while let Some(bytes) = self.input.front() {
            match self.session.write_input(bytes) {
                Ok(()) => {
                    self.input_bytes -= self.input.pop_front().unwrap().queue_bytes();
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    self.input.clear();
                    self.input_bytes = 0;
                    return if self.session.accepts_input() {
                        Err(error)
                    } else {
                        Ok(())
                    };
                }
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct SynchronizedOutput {
    generation: u64,
    deadline: Option<Instant>,
}

impl SynchronizedOutput {
    fn update(&mut self, terminal: &mut vt::Terminal, now: Instant) -> Option<Instant> {
        if !terminal.modes.dec(2026) {
            self.deadline = None;
        } else if self.deadline.is_none()
            || self.generation != terminal.synchronized_output_generation
        {
            self.generation = terminal.synchronized_output_generation;
            // Match Ghostty's failsafe for an application that never ends a batch.
            self.deadline = Some(now + Duration::from_secs(1));
        } else if self.deadline.is_some_and(|deadline| deadline <= now) {
            terminal.set_mode(true, 2026, false);
            self.deadline = None;
        }
        self.deadline
    }
}

/// A successful write means the complete packet is accepted by the pane's ordered queue.
struct QueuedInput<'a> {
    session: &'a Session,
    queue: &'a mut VecDeque<SessionInput>,
    bytes: &'a mut usize,
}
impl QueuedInput<'_> {
    fn enqueue(&mut self, input: SessionInput) -> std::io::Result<()> {
        if !self.session.accepts_input() {
            self.queue.clear();
            *self.bytes = 0;
            return Ok(());
        }
        let size = input.queue_bytes();
        if size == 0 {
            return Ok(());
        }
        if self.queue.is_empty() {
            match self.session.write_input(&input) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => {
                    return if self.session.accepts_input() {
                        Err(error)
                    } else {
                        Ok(())
                    };
                }
            }
        }
        queue_input(self.queue, self.bytes, input)
    }
}

fn queue_input(
    queue: &mut VecDeque<SessionInput>,
    bytes: &mut usize,
    input: SessionInput,
) -> std::io::Result<()> {
    let size = input.queue_bytes();
    if bytes.saturating_add(size) > INPUT_BUDGET {
        return Err(std::io::Error::other(
            "Input queue is full. Wait for the command to read its input.",
        ));
    }
    if size > 0 {
        *bytes += size;
        queue.push_back(input);
    }
    Ok(())
}

impl std::io::Write for QueuedInput<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.enqueue(SessionInput::Reply(bytes.to_vec()))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct PendingPaste {
    pane: Id,
    data: Vec<u8>,
}

enum Confirmation {
    DumpSessions {
        error: Option<String>,
    },
    Quit {
        confirmation: Box<Confirmation>,
        save_dump: bool,
        keep_across_reboots: bool,
        error: Option<String>,
    },
    Close(Action),
    TmuxxClose {
        target: tmuxx_host::Target,
        session: u64,
    },
    TmuxxTerminate(tmuxx_host::MenuCommand),
    Paste(PendingPaste),
    OpenUrl {
        uri: String,
        cwd: PathBuf,
        handler: String,
    },
}

impl Confirmation {
    fn for_quit(
        confirmation: Option<Self>,
        action: Action,
        setting: config::SessionDumpOnQuit,
        quitting: bool,
        option: bool,
    ) -> Option<Self> {
        if !quitting || setting == config::SessionDumpOnQuit::Disabled {
            return confirmation;
        }
        let confirmation = confirmation.or_else(|| option.then_some(Self::Close(action)))?;
        Some(Self::Quit {
            confirmation: Box::new(confirmation),
            save_dump: setting == config::SessionDumpOnQuit::Checked,
            keep_across_reboots: false,
            error: None,
        })
    }
}

fn quit_option_modifier(modifiers: winit::keyboard::ModifiersState) -> bool {
    modifiers.alt_key() && (!cfg!(target_os = "windows") || !modifiers.control_key())
}

struct LayoutPicker {
    choices: Vec<workspace::SavedLayout>,
    path: String,
    error: Option<String>,
}

enum LayoutCommand {
    Open(PathBuf),
    Browse,
    Close,
}

// Host viewport and selection changes do not advance the terminal generation.
#[derive(Clone, PartialEq)]
struct PaneRenderKey {
    generation: u64,
    viewport_offset: usize,
    selection: Option<vt::Selection>,
    cursor: (usize, usize, vt::screen::CursorShape, bool, bool),
    options: RenderOptions,
    rect: egui::Rect,
    scale: f32,
}

impl PaneRenderKey {
    fn matches_terminal(&self, terminal: &vt::Terminal) -> bool {
        let screen = terminal.screen();
        let cursor = &screen.cursor;
        self.generation == terminal.generation
            && self.viewport_offset == screen.viewport_offset
            && self.selection == screen.selection
            && self.cursor
                == (
                    cursor.col,
                    cursor.row,
                    cursor.shape,
                    cursor.visible,
                    cursor.blink,
                )
    }

    fn new(terminal: &vt::Terminal, options: RenderOptions, rect: egui::Rect, scale: f32) -> Self {
        let screen = terminal.screen();
        let cursor = &screen.cursor;
        Self {
            generation: terminal.generation,
            viewport_offset: screen.viewport_offset,
            selection: screen.selection,
            cursor: (
                cursor.col,
                cursor.row,
                cursor.shape,
                cursor.visible,
                cursor.blink,
            ),
            options,
            rect,
            scale,
        }
    }
}

struct PreparedPane {
    key: PaneRenderKey,
    frame: Arc<Frame>,
    dim: Option<rustty_render::Color>,
    // Accessibility may first be requested while synchronized output holds this frame.
    screen: vt::Screen,
    cell: [f32; 2],
    text: Option<TerminalText>,
    ime_rect: egui::Rect,
}

#[cfg(target_os = "macos")]
struct CursorCandidate {
    pane: Id,
    key: PaneRenderKey,
    frame: Arc<Frame>,
}

#[cfg(target_os = "macos")]
impl CursorCandidate {
    fn matches(&self, other: &Self) -> bool {
        let mut key = self.key.clone();
        key.options.blink_visible = other.key.options.blink_visible;
        self.pane == other.pane
            && key == other.key
            && self.frame.generation == other.frame.generation
    }

    fn patch(&self, off: &Self, composed: &ComposedFrame) -> Option<([u32; 4], Frame)> {
        if !self.matches(off) || !self.key.options.blink_visible || off.key.options.blink_visible {
            return None;
        }
        let pane = composed
            .panes
            .iter()
            .find(|pane| Arc::ptr_eq(&pane.frame, &off.frame))?;
        let rect =
            |[x, y, w, h]: [f32; 4]| egui::Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
        let bounds = rect(self.frame.cursor_row_bounds?)
            .union(rect(off.frame.cursor_row_bounds?))
            .translate(Vec2::new(pane.rect[0], pane.rect[1]));
        let bounds = egui::Rect::from_min_max(bounds.min.floor(), bounds.max.ceil())
            .intersect(rect(pane.clip))
            .intersect(rect(pane.rect));
        // The retained patch must cover every changed pixel, including clipped edges.
        let min = bounds.min.ceil();
        let max = bounds.max.floor();
        if min.x < 0.0 || min.y < 0.0 || min.x >= max.x || min.y >= max.y {
            return None;
        }
        if min != bounds.min || max != bounds.max {
            return None;
        }
        // Other panes and their dimming are inside the terminal callback, below the native layer.
        if composed.panes.iter().any(|other| {
            !std::ptr::eq(other, pane)
                && rect(other.rect)
                    .intersect(rect(other.clip))
                    .intersect(bounds)
                    .is_positive()
        }) {
            return None;
        }
        let pixels = [
            min.x as u32,
            min.y as u32,
            (max.x - min.x) as u32,
            (max.y - min.y) as u32,
        ];
        let mut patch = Frame::empty([pixels[2], pixels[3]]);
        patch
            .append_clipped(
                &self.frame,
                [pane.rect[0] - min.x, pane.rect[1] - min.y],
                [0.0, 0.0, pixels[2] as f32, pixels[3] as f32],
            )
            .ok()?;
        Some((pixels, patch))
    }
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct CursorState {
    enabled: bool,
    layer: Option<native_cursor::NativeCursorLayer>,
    on: Option<CursorCandidate>,
    active: bool,
    deadline: Option<Instant>,
    updates: u64,
    activations: u64,
    patch_pixels: Option<[u32; 4]>,
}

#[cfg(target_os = "macos")]
fn cursor_patch_uncovered(
    primitives: &[egui::ClippedPrimitive],
    pixels: [u32; 4],
    scale: f32,
) -> bool {
    let [x, y, width, height] = pixels.map(|v| v as f32 / scale);
    let patch = egui::Rect::from_min_size(Pos2::new(x, y), Vec2::new(width, height));
    let mut terminal_seen = false;
    for primitive in primitives {
        match &primitive.primitive {
            egui::epaint::Primitive::Callback(_) => {
                // The terminal is currently the only callback; unknown callbacks disable the patch.
                if terminal_seen {
                    return false;
                }
                terminal_seen = true;
            }
            egui::epaint::Primitive::Mesh(mesh) if terminal_seen => {
                let clip = patch.intersect(primitive.clip_rect);
                if clip.is_positive()
                    && mesh.indices.as_chunks::<3>().0.iter().any(|triangle| {
                        let vertices = triangle.map(|index| mesh.vertices[index as usize]);
                        vertices.iter().any(|v| v.color.a() != 0)
                            && egui::Rect::from_points(&vertices.map(|v| v.pos))
                                .intersect(clip)
                                .is_positive()
                    })
                {
                    return false;
                }
            }
            _ => {}
        }
    }
    terminal_seen
}

#[derive(PartialEq)]
struct HoveredLink {
    pane: Id,
    uri: String,
    bounds: Vec<egui::Rect>,
}

impl PreparedPane {
    fn compose(
        &mut self,
        visual: &zoom_animation::Pane,
        content: egui::Rect,
        scale: f32,
        config: &Config,
        dim: Option<rustty_render::Color>,
        animated: bool,
    ) -> ComposedPane {
        self.dim = dim;
        let clip = visual.rect.intersect(content);
        ComposedPane {
            transition: animated.then(|| ZoomPaint {
                background: rustty_render::Color::rgb(self.key.options.background)
                    .opacity(config.background_opacity),
                frame_background: rustty_render::Color::rgb(config.background.to_array()),
                opacity: visual.opacity,
                background_opacity: visual.background_opacity,
                moving: visual.moving,
            }),
            frame: Arc::clone(&self.frame),
            rect: [
                visual.rect.left() * scale,
                visual.rect.top() * scale,
                visual.layout.width() * scale,
                visual.layout.height() * scale,
            ],
            clip: [
                clip.left() * scale,
                clip.top() * scale,
                clip.width().max(0.0) * scale,
                clip.height().max(0.0) * scale,
            ],
            dim,
        }
    }

    fn matches(&mut self, key: &PaneRenderKey, fonts: &rustty_render::Renderer) -> bool {
        let options = &mut self.key.options;
        let cursor = &self.screen.cursor;
        let blinking_cursor = options.focused
            && options.cursor_visible
            && cursor.visible
            && cursor.blink
            && self.screen.viewport_offset == 0
            && options.preedit.as_ref().is_none_or(|p| p.text.is_empty());
        if !blinking_cursor && !self.frame.blinking_text {
            // Only normalize the retained key. New content may introduce blink
            // and must still be prepared with the incoming, actual phase.
            options.blink_visible = key.options.blink_visible;
        }
        self.key == *key && self.frame.generation == fonts.generation()
    }

    fn new(
        key: PaneRenderKey,
        screen: vt::Screen,
        fonts: &mut rustty_render::Renderer,
    ) -> std::result::Result<Self, rustty_render::RenderError> {
        let metrics = fonts.metrics();
        let frame = fonts.prepare(&screen, &key.options)?;
        let origin = [key.rect.left(), key.rect.top()];
        let padding = key.options.padding;
        let [x, y, width, height] = frame.ime_cursor.unwrap_or([
            padding[0] + screen.cursor.col as f32 * metrics.cell_width as f32,
            padding[1] + screen.cursor.row as f32 * metrics.cell_height as f32,
            metrics.cell_width as f32,
            metrics.cell_height as f32,
        ]);
        let ime_rect = egui::Rect::from_min_size(
            Pos2::new(origin[0] + x / key.scale, origin[1] + y / key.scale),
            Vec2::new(width / key.scale, height / key.scale),
        );
        let cell = [
            metrics.cell_width as f32 / key.scale,
            metrics.cell_height as f32 / key.scale,
        ];
        Ok(Self {
            key,
            frame: Arc::new(frame),
            dim: None,
            screen,
            cell,
            text: None,
            ime_rect,
        })
    }

    fn accessibility(&mut self, pane: Id) -> &TerminalText {
        self.text.get_or_insert_with(|| {
            let key = &self.key;
            TerminalText::new(
                pane,
                &self.screen,
                [
                    key.rect.left() + key.options.padding[0] / key.scale,
                    key.rect.top() + key.options.padding[1] / key.scale,
                ],
                self.cell,
            )
        })
    }
}

#[derive(Clone, Copy, PartialEq)]
struct ZoomPaint {
    background: rustty_render::Color,
    frame_background: rustty_render::Color,
    opacity: f32,
    background_opacity: f32,
    moving: bool,
}

struct ComposedPane {
    frame: Arc<Frame>,
    rect: [f32; 4],
    clip: [f32; 4],
    dim: Option<rustty_render::Color>,
    transition: Option<ZoomPaint>,
}

struct ComposedFrame {
    panes: Vec<ComposedPane>,
    frame: Arc<Frame>,
}

impl ComposedFrame {
    fn matches(&self, size: [u32; 2], panes: &[ComposedPane]) -> bool {
        self.frame.size == size
            && self.panes.len() == panes.len()
            && self.panes.iter().zip(panes).all(|(old, new)| {
                Arc::ptr_eq(&old.frame, &new.frame)
                    && old.rect == new.rect
                    && old.clip == new.clip
                    && old.dim == new.dim
                    && old.transition == new.transition
            })
    }

    fn new(
        size: [u32; 2],
        panes: Vec<ComposedPane>,
    ) -> std::result::Result<Self, rustty_render::ComposeError> {
        let mut frame = Frame::empty(size);
        for (index, pane) in panes.iter().enumerate() {
            let rect = |[x, y, w, h]: [f32; 4]| {
                egui::Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h))
            };
            let mut clips = vec![rect(pane.clip)];
            for above in panes[index + 1..]
                .iter()
                .filter(|pane| pane.transition.is_some_and(|paint| paint.moving))
            {
                clips = clips
                    .into_iter()
                    .flat_map(|clip| uncovered_rects(clip, rect(above.clip)))
                    .collect();
            }
            for clip in clips {
                let clip = [clip.left(), clip.top(), clip.width(), clip.height()];
                let start = frame.quads.len();
                frame.append_clipped(&pane.frame, [pane.rect[0], pane.rect[1]], clip)?;
                if let Some(transition) = pane.transition {
                    // Prepared terminals start with their full background; stretch only that quad.
                    let background = rustty_render::Quad::solid(clip, transition.background);
                    if let Some(first) = frame.quads.get_mut(start) {
                        *first = background;
                    } else {
                        frame.quads.push(background);
                    }
                }
                if let Some(color) = pane.dim {
                    frame.quads.push(rustty_render::Quad::solid(clip, color));
                }
                if let Some(transition) = pane.transition {
                    // Apply the background mix once to all content, including colored glyphs.
                    let opacity = if transition.background_opacity > 0.0 {
                        transition.opacity / transition.background_opacity
                    } else {
                        0.0
                    };
                    for quad in &mut frame.quads[start + 1..] {
                        quad.color = quad.color.opacity(opacity);
                    }
                    if transition.background_opacity < 1.0 {
                        frame.quads.push(rustty_render::Quad::solid(
                            clip,
                            transition
                                .frame_background
                                .opacity(1.0 - transition.background_opacity),
                        ));
                    }
                }
            }
        }
        Ok(Self {
            panes,
            frame: Arc::new(frame),
        })
    }
}

struct Host {
    id: Id,
    tmuxx_theme: Option<String>,
    viewport: ViewportId,
    window: Arc<Window>,
    egui: egui_winit::State,
    accesskit_active: bool,
    fonts: rustty_render::Renderer,
    rects: BTreeMap<Id, egui::Rect>,
    pane_order: Vec<Id>,
    modal_pane: Option<Id>,
    overlay_panes: std::collections::BTreeSet<Id>,
    prepared: BTreeMap<Id, PreparedPane>,
    composed: Option<ComposedFrame>,
    zoom_animation: zoom_animation::State,
    pane_prepares: u64,
    content: Rect,
    min_inner_size: LogicalSize<f64>,
    divider_drag: Option<(Id, Axis, Rect)>,
    modifiers: Modifiers,
    consumed_keys: HashSet<PhysicalKey>,
    sequence: Vec<usize>,
    sequence_len: usize,
    composing: bool,
    preedit: String,
    preedit_selection: Option<(usize, usize)>,
    peek: Option<Peek>,
    presented_peek: Option<Peek>,
    navigation_warning: Option<(Id, Instant)>,
    mouse: Pos2,
    deferred_pointer: Option<Pos2>,
    link_hit: Option<(Id, u64, usize, vt::GridPoint, egui::Rect, Vec2)>,
    hovered_link: Option<HoveredLink>,
    mouse_button: Option<vt::MouseButton>,
    selection_drag: Option<(Id, input::SelectionDrag)>,
    focused: bool,
    focus_hint: FocusHint,
    cursor_blink_started: Instant,
    #[cfg(target_os = "macos")]
    cursor: CursorState,
    #[cfg(target_os = "macos")]
    progress: native_progress::NativeProgressLayers,
    visible: bool,
    quick_used: bool,
    occluded: bool,
    deadline: Option<Instant>,
    search_focus: Option<Id>,
    search_rects: BTreeMap<Id, egui::Rect>,
    focus_text_input: bool,
    unfocus_text_input: bool,
    popup_open: bool,
    ui_areas: input::UiAreas,
    messages_open: bool,
    messages_focused: bool,
    layout_picker: Option<LayoutPicker>,
    palette: CommandPalette,
    confirm: Option<Confirmation>,
    tmuxx_connect: Option<String>,
    clipboard_request: VecDeque<clipboard_preview::Pending>,
    capture: bool,
    frames: u64,
    last_frame_started: Option<Instant>,
}
impl Host {
    #[cfg(target_os = "macos")]
    fn cursor_candidate(&self, focused: Id) -> Option<CursorCandidate> {
        // shortcut: opaque cursor pane without images; extend after validating transparent and image-backed patches.
        if !self.focused
            || self.composing
            || !self.preedit.is_empty()
            || self.peek.is_some()
            || self.zoom_animation.is_running()
            || self.modal_input()
        {
            return None;
        }
        let composed = self.composed.as_ref()?;
        let prepared = self.prepared.get(&focused)?;
        let pane = composed
            .panes
            .iter()
            .find(|pane| Arc::ptr_eq(&pane.frame, &prepared.frame))?;
        let key = &prepared.key;
        if pane.dim.is_some()
            || pane.transition.is_some()
            || key.options.background_opacity != 1.0
            || !key.options.focused
            || !key.options.cursor_visible
            || !key.cursor.3
            || !key.cursor.4
            || key.viewport_offset != 0
            || prepared.frame.blinking_text
            || prepared.frame.cursor_row_bounds.is_none()
            || !prepared.screen.graphics.images.is_empty()
        {
            return None;
        }
        Some(CursorCandidate {
            pane: focused,
            key: key.clone(),
            frame: Arc::clone(&prepared.frame),
        })
    }

    #[cfg(target_os = "macos")]
    fn update_cursor(
        &mut self,
        painter: &painter::Painter,
        candidate: Option<CursorCandidate>,
        primitives: &[egui::ClippedPrimitive],
        deadline: Instant,
    ) {
        let previous = self.cursor.on.take();
        let Some(candidate) = candidate else {
            return;
        };
        if candidate.key.options.blink_visible {
            self.cursor.on = Some(candidate);
            return;
        }
        let Some(on) = previous else {
            return;
        };
        let Some((pixels, patch)) = on.patch(&candidate, self.composed.as_ref().unwrap()) else {
            return;
        };
        if !cursor_patch_uncovered(primitives, pixels, candidate.key.scale) {
            return;
        }
        let result = (|| -> Result<()> {
            if self.cursor.layer.is_none() {
                self.cursor.layer = Some(native_cursor::NativeCursorLayer::new(
                    Arc::clone(&self.window),
                    &painter.render_state().ok_or("GPU unavailable")?,
                )?);
            }
            self.cursor.layer.as_mut().unwrap().present(pixels, &patch)
        })();
        match result {
            Ok(()) => {
                self.cursor.on = Some(on);
                self.cursor.active = true;
                self.cursor.deadline = Some(deadline);
                self.cursor.activations += 1;
                self.cursor.patch_pixels = Some(pixels);
            }
            Err(error) => {
                eprintln!("Rustty cursor layer: {error}");
                self.cursor.layer = None;
            }
        }
    }

    fn hovered_pane(&self) -> Option<Id> {
        if !self.egui.is_pointer_in_window() {
            return None;
        }
        self.pane_at(self.mouse)
    }
    fn pane_at(&self, position: Pos2) -> Option<Id> {
        let content = egui::Rect::from_min_size(
            Pos2::new(self.content.x, self.content.y),
            Vec2::new(self.content.width, self.content.height),
        );
        topmost_pane(
            &self.pane_order,
            &self.rects,
            content,
            position,
            self.modal_pane,
        )
    }
    fn ui_input(&self) -> bool {
        self.search_focus.is_some()
            || self.modal_input()
            || self.messages_open && self.messages_focused
    }
    fn modal_input(&self) -> bool {
        self.palette.open
            || self.popup_open
            || self.layout_picker.is_some()
            || self.confirm.is_some()
            || self.tmuxx_connect.is_some()
            || !self.clipboard_request.is_empty()
    }
    fn repaint(&self) {
        if self.visible && !self.occluded {
            self.window.request_redraw();
        }
    }
}

fn topmost_pane(
    order: &[Id],
    rects: &BTreeMap<Id, egui::Rect>,
    content: egui::Rect,
    position: Pos2,
    modal: Option<Id>,
) -> Option<Id> {
    if !content.contains(position) {
        return None;
    }
    order.iter().rev().copied().find(|id| {
        modal.is_none_or(|modal| modal == *id)
            && rects.get(id).is_some_and(|rect| rect.contains(position))
    })
}

struct DirectoryBadge {
    label: DirectoryLabel,
    bounds: egui::Rect,
    centered: bool,
    shared: bool,
    active: bool,
    attention: bool,
    pinned: bool,
    flash: f32,
    accent: TabAccent,
}
impl DirectoryBadge {
    fn paint(self, ui: &egui::Ui) {
        let [r, g, b] = self.accent.background;
        let accent = Color32::from_rgb(r, g, b);
        let color = if self.label.focused {
            let [r, g, b] = self.accent.foreground;
            Color32::from_rgb(r, g, b)
        } else {
            ui.visuals().strong_text_color()
        };
        let scale = if self.shared { 1.0 } else { 0.7 };
        let pin_width = if self.pinned { 20.0 } else { 0.0 };
        let large_padding = Vec2::new(16.0, 10.0) * scale;
        let layout = |large| {
            let mut job = egui::text::LayoutJob::simple(
                format!(
                    "{}{}",
                    self.label.name,
                    if self.attention { " ●" } else { "" }
                ),
                egui::FontId::proportional(if large { 22.0 * scale } else { 12.0 }),
                color,
                if self.centered {
                    (self.bounds.width() * 0.75 - 24.0 * scale - pin_width).max(1.0)
                } else if large {
                    f32::INFINITY
                } else {
                    (self.bounds.width() - 32.0 - pin_width).max(1.0)
                },
            );
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            job.sections[0]
                .format
                .coords
                .push("wght", if large { 600.0 } else { 500.0 });
            if self.label.shows_activity(self.active) {
                job.sections[0].format.underline = egui::Stroke::new(1.0, color);
            }
            ui.painter().layout_job(job)
        };
        let mut galley = layout(true);
        let large = self.centered
            || (galley.size().x + pin_width + 2.0 * large_padding.x + 16.0 <= self.bounds.width()
                && galley.size().y + 2.0 * large_padding.y + 16.0 <= self.bounds.height());
        if !large {
            galley = layout(false);
        }
        let padding = if large {
            large_padding
        } else {
            Vec2::new(8.0, 4.0)
        };
        let size = galley.size() + Vec2::new(pin_width, 0.0) + 2.0 * padding;
        let bounds = if self.centered {
            egui::Rect::from_center_size(self.bounds.center(), size)
        } else {
            egui::Rect::from_min_size(
                self.bounds.right_top() + Vec2::new(-size.x - 8.0, 8.0),
                size,
            )
        };
        let painter = ui.painter().with_clip_rect(self.bounds);
        let rounding = if large { 10.0 * scale } else { 6.0 };
        painter.rect_filled(
            bounds,
            rounding,
            if self.label.focused {
                accent
            } else {
                ui.visuals().widgets.inactive.bg_fill
            },
        );
        painter.rect_stroke(
            bounds,
            rounding,
            egui::Stroke::new(1.0, color.gamma_multiply(0.35)),
            egui::StrokeKind::Inside,
        );
        if self.flash > 0.0 {
            painter.rect_filled(bounds, rounding, accent.gamma_multiply(self.flash));
        }
        if self.pinned {
            outliner_ui::paint_pin(
                &painter,
                egui::Rect::from_center_size(
                    Pos2::new(bounds.left() + padding.x + 8.0, bounds.center().y),
                    Vec2::splat(16.0),
                ),
                color,
            );
        }
        painter.galley(
            bounds.min + padding + Vec2::new(pin_width, 0.0),
            galley,
            color,
        );
    }
}

fn paint_panel_pin(ui: &egui::Ui, pane: Id, bounds: egui::Rect) {
    let rect = egui::Rect::from_min_size(
        bounds.right_top() + Vec2::new(-28.0, 8.0),
        Vec2::splat(20.0),
    );
    let painter = ui.painter().with_clip_rect(bounds);
    painter.rect_filled(rect, 4, ui.visuals().widgets.inactive.bg_fill);
    outliner_ui::paint_pin(&painter, rect.shrink(2.0), ui.visuals().strong_text_color());
    let response = ui.interact(
        rect,
        egui::Id::new(("panel-pin", pane)),
        egui::Sense::hover(),
    );
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, "Pinned panel"));
    response.on_hover_text("Pinned panel");
}

fn paint_server_selection(ui: &egui::Ui, pane: Id, bounds: egui::Rect, accent: Color32) {
    let rect =
        egui::Rect::from_min_size(bounds.right_bottom() - Vec2::splat(28.0), Vec2::splat(20.0));
    let painter = ui.painter().with_clip_rect(bounds);
    painter.rect_filled(rect, 4, ui.visuals().widgets.inactive.bg_fill);
    painter.circle_stroke(rect.center(), 5.0, egui::Stroke::new(1.5, accent));
    painter.circle_filled(rect.center(), 1.5, accent);
    let response = ui.interact(
        rect,
        egui::Id::new(("server-selection", pane)),
        egui::Sense::hover(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Label, true, "Selected on tmuxx server")
    });
    response.on_hover_text("Selected on tmuxx server; your keyboard focus stays local");
}

struct ThemePreview {
    owner: Id,
    original: Config,
    scope: theme_preview::Scope,
}

struct App {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    handoff: Option<handoff_ui::State>,
    import_selector: bool,
    startup_tmuxx: Option<String>,
    deck: deck_host::Connection,
    tmuxx: Vec<tmuxx_host::Attachment>,
    tmuxx_smoke: Option<PathBuf>,
    loaded: LoadedConfig,
    config_loader: config::ConfigLoader,
    config_args: Vec<String>,
    theme_preview: Option<ThemePreview>,
    tmuxx_themes: BTreeMap<String, Config>,
    workspace: Workspace,
    state_path: PathBuf,
    resources: Option<PathBuf>,
    panes: HashMap<Id, Pane>,
    directory_label_paths: HashMap<Id, PathBuf>,
    directory_labels: HashMap<Id, String>,
    pending_visuals: BTreeMap<Id, Arc<Vec<u8>>>,
    starting_panes: HashMap<Id, (PaneStartup, mpsc::Receiver<std::io::Result<Session>>)>,
    failed_panes: BTreeMap<Id, String>,
    activity_flashes: HashMap<Id, CompletionFlash>,
    closing: Vec<Session>,
    windows: HashMap<WindowId, Host>,
    outliner: Option<outliner_host::OutlinerHost>,
    platform: Option<Platform>,
    consumed_global_keys: HashSet<PhysicalKey>,
    context: egui::Context,
    painter: painter::Painter,
    proxy: EventLoopProxy<Event>,
    active: Option<Id>,
    started: Instant,
    save_at: Option<Instant>,
    errors: Vec<String>,
    history: Vec<(Instant, Workspace)>,
    redo: Vec<(Instant, Workspace)>,
    initial_command_pane: Option<Id>,
    close_at: Option<Instant>,
    smoke: Option<smoke::Smoke>,
    background_smoke: bool,
    smoke_error: Option<String>,
    startup_error: Option<String>,
}

pub fn run() -> Result<()> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "Rustty — native Rust terminal\n\nUsage: rustty [--config-file=PATH] [--key=value] [-e COMMAND ...]\n       rustty --tmuxx=embedded|persistent|{}|ssh:HOST|system-ssh:HOST\n       rustty tmux [tmuxx options and commands]\n       rustty --config-info\n\nRustty settings override Ghostty settings. See crates/rustty-app/README.md.",
            tmuxx_host::LOCAL_TARGET_HINT
        );
        println!(
            "\nA sole saved session dump loads automatically at startup; multiple dumps open a chooser. Hold Option (Alt on Windows) while quitting to save one.\nConfigure session-dump-on-quit = disabled|unchecked|checked (default: unchecked)."
        );
        return Ok(());
    }
    let tmuxx_mode = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--tmuxx=").map(str::to_owned));
    args.retain(|arg| !arg.starts_with("--tmuxx="));
    let show_config = args.iter().any(|arg| arg == "--config-info");
    args.retain(|arg| arg != "--config-info");
    // Keep this policy available while Smoke::step temporarily owns the fixture.
    let tmuxx_smoke = std::env::var_os("RUSTTY_TMUXX_SMOKE_DIR").map(PathBuf::from);
    let background_smoke = tmuxx_smoke.is_some()
        || (std::env::var_os("RUSTTY_SMOKE_DIR").is_some()
            && std::env::var_os("RUSTTY_SMOKE_OFFSCREEN").is_some());
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let background_smoke = background_smoke || handoff_ui::smoke_directory().is_some();
    let mut builder = EventLoop::<Event>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        if background_smoke {
            builder.with_activation_policy(ActivationPolicy::Accessory);
        }
        if background_smoke || std::env::var_os("RUSTTY_SMOKE_DIR").is_some() {
            builder.with_activate_ignoring_other_apps(false);
        }
    }
    #[cfg(target_os = "windows")]
    rustty_app::platform::configure_event_loop(&mut builder);
    let event_loop = builder.build()?;
    let resources = resource_dir();
    let mut config_loader = config::ConfigLoader::from_env()?;
    config_loader.resources_dir = resources.clone().or(config_loader.resources_dir);
    let mut loaded = load_config(&mut config_loader, &args, Platform::system_theme());
    if show_config {
        println!(
            "Configuration: {:?}\nOwn settings: {}\nEdit settings: {}\nRenderer: {:?}",
            loaded.family,
            loaded.own_config_path.display(),
            loaded.edit_config_path.display(),
            loaded.config.renderer
        );
        for source in &loaded.sources {
            println!("  {}", source.display());
        }
        for diagnostic in &loaded.diagnostics {
            eprintln!("{diagnostic}");
        }
        let shell = rustty::session::resolve_shell(
            loaded
                .config
                .initial_command
                .as_ref()
                .or(loaded.config.command.as_ref()),
        );
        println!("Shell: {shell}");
        if let Some(diagnostic) = shell.diagnostic() {
            eprintln!("{diagnostic}");
        }
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        let conpty_resources = std::env::var_os("TMUXX_CONPTY_RESOURCES")
            .map(PathBuf::from)
            .or_else(|| resources.clone());
        rustty_app::platform::initialize_terminal_runtime(conpty_resources.as_deref().ok_or(
            "Rustty resources are missing; build or copy the complete application folder.",
        )?)?;
    }
    let smoke = smoke::Smoke::from_env(&mut loaded)?;
    #[cfg(debug_assertions)]
    handoff_ui::configure_smoke(&mut loaded);
    let mut state_path = app_paths::data_dir()?.join("workspace.json");
    let mut errors = loaded
        .diagnostics
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if let Some(smoke) = &smoke {
        state_path = smoke.directory.join("workspace.json");
    } else if let Some(directory) = &tmuxx_smoke {
        state_path = directory.join("workspace.json");
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if let Some(directory) = handoff_ui::smoke_directory() {
        state_path = directory.join("workspace.json");
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let handoff = if background_smoke && handoff_ui::smoke_directory().is_none() {
        None
    } else {
        match handoff_ui::State::new() {
            Ok(state) => Some(state),
            Err(error) => {
                errors.push(format!("State import is unavailable: {error}"));
                None
            }
        }
    };
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let import_requested = handoff
        .as_ref()
        .is_some_and(|state| !state.dumps.is_empty());
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let import_requested = false;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if !import_requested
        && loaded.config.window_save_state != config::WindowSaveState::Never
        && let Some(state) = &handoff
        && let Err(error) = state.store.claim_workspace(&state_path)
    {
        errors.push(format!("Could not claim workspace saving: {error}"));
    }
    let workspace = if smoke.is_some() {
        Workspace::default()
    } else if loaded.config.window_save_state != config::WindowSaveState::Never {
        match Workspace::load(&state_path) {
            Ok(state) => state.unwrap_or_default(),
            Err(error) => {
                errors.push(format!("Could not restore windows: {error}"));
                Workspace::default()
            }
        }
    } else {
        Workspace::default()
    };
    let proxy = event_loop.create_proxy();
    let context = egui::Context::default();
    context.set_theme(ui_theme(&loaded.config));
    configure_ui_fonts(&context);
    let repaint = proxy.clone();
    context.set_request_repaint_callback(move |info| {
        if let Some(deadline) = Instant::now().checked_add(info.delay) {
            let _ = repaint.send_event(Event::Repaint(info, deadline));
        }
    });
    let mut gpu_config = egui_wgpu::WgpuConfiguration {
        surface: egui_wgpu::SurfaceConfig {
            // Cursor retirement transactions need a third Metal drawable for follow-up repaints.
            #[cfg(target_os = "macos")]
            desired_maximum_frame_latency: Some(2),
            ..egui_wgpu::SurfaceConfig::LOW_LATENCY
        },
        ..Default::default()
    };
    #[cfg(target_os = "windows")]
    if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut gpu_config.wgpu_setup {
        // An HWND swapchain is opaque even for a transparent Winit window.
        // WGPU owns the DirectComposition visual and its premultiplied surface.
        setup
            .instance_descriptor
            .backend_options
            .dx12
            .presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual;
        // Both current shader pipelines support FXC; this keeps the portable
        // distribution independent of a separately installed DXC DLL.
        setup
            .instance_descriptor
            .backend_options
            .dx12
            .shader_compiler = wgpu::Dx12Compiler::Fxc;
    }
    if smoke.is_some() {
        let on_status = gpu_config.on_surface_status.clone();
        let reported = AtomicBool::new(false);
        gpu_config.on_surface_status = Arc::new(move |status| {
            if !reported.swap(true, Ordering::Relaxed) {
                eprintln!("Native smoke surface: {status:?}");
            }
            on_status(status)
        });
    }
    let painter = pollster::block_on(painter::Painter::new(
        context.clone(),
        gpu_config,
        loaded.config.renderer,
    ))?;
    let now = Instant::now();
    let close_at = std::env::var("RUSTTY_SMOKE_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|s| now + Duration::from_secs(s))
        .or_else(|| tmuxx_smoke.as_ref().map(|_| now + Duration::from_secs(5)));
    let mut app = App {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        handoff,
        import_selector: import_requested,
        startup_tmuxx: tmuxx_mode,
        deck: deck_host::Connection::default(),
        tmuxx_smoke: tmuxx_smoke.clone(),
        tmuxx: Vec::new(),
        loaded,
        config_loader,
        config_args: args,
        theme_preview: None,
        tmuxx_themes: BTreeMap::new(),
        workspace,
        state_path,
        resources,
        panes: HashMap::new(),
        directory_label_paths: HashMap::new(),
        directory_labels: HashMap::new(),
        pending_visuals: BTreeMap::new(),
        starting_panes: HashMap::new(),
        failed_panes: BTreeMap::new(),
        activity_flashes: HashMap::new(),
        closing: Vec::new(),
        windows: HashMap::new(),
        outliner: None,
        platform: None,
        consumed_global_keys: HashSet::new(),
        context,
        painter,
        proxy,
        active: None,
        started: now,
        save_at: None,
        errors,
        history: Vec::new(),
        redo: Vec::new(),
        initial_command_pane: None,
        close_at,
        smoke,
        background_smoke,
        smoke_error: None,
        startup_error: None,
    };
    app.reload_tmuxx_themes();
    if !import_requested {
        app.start_sessions()?;
    }
    let check_shutdown = background_smoke || app.smoke.is_some();
    let result = event_loop.run_app(&mut app);
    let shutdown_windows = if check_shutdown {
        app.windows
            .values()
            .map(|host| Arc::downgrade(&host.window))
            .chain(app.outliner.iter().map(|host| Arc::downgrade(&host.window)))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    app.shutdown();
    if shutdown_windows
        .iter()
        .any(|window| window.upgrade().is_some())
    {
        return Err("native smoke retained a window after shutdown".into());
    }
    result?;
    if let Some(error) = app.startup_error.take() {
        return Err(error.into());
    }
    if let Some(error) = app.smoke_error.take() {
        return Err(error.into());
    }
    if let Some(directory) = tmuxx_smoke
        && !directory.join("result.json").is_file()
    {
        return Err("tmuxx native smoke did not capture a frame".into());
    }
    if app.smoke.is_some() {
        return Err("native smoke exited before completing its checks".into());
    }
    Ok(())
}

pub(super) fn resource_dir() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    #[cfg(target_os = "windows")]
    {
        let bundled = executable.parent()?.join("resources");
        if bundled.is_dir() {
            return Some(bundled);
        }
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        let development = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(profile)
            .join("Rustty/resources");
        development.is_dir().then_some(development)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let bundled = executable.parent()?.parent()?.join("Resources/rustty");
        if bundled.is_dir() {
            return Some(bundled);
        }
        let development = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/debug/Rustty.app/Contents/Resources/rustty");
        development.is_dir().then_some(development)
    }
}

fn font_config(config: &Config, scale: f32) -> FontConfig {
    let variations = |values: &[config::FontVariation]| {
        values
            .iter()
            .map(|v| rustty_font::FontVariation {
                tag: v.tag,
                value: v.value,
            })
            .collect()
    };
    FontConfig {
        families: config.font_family.clone(),
        bold_families: config.font_family_bold.clone(),
        italic_families: config.font_family_italic.clone(),
        bold_italic_families: config.font_family_bold_italic.clone(),
        size_points: config.font_size,
        scale_factor: scale,
        variations: variations(&config.font_variation),
        bold_variations: variations(&config.font_variation_bold),
        italic_variations: variations(&config.font_variation_italic),
        bold_italic_variations: variations(&config.font_variation_bold_italic),
        style_requests: [
            &config.font_style,
            &config.font_style_bold,
            &config.font_style_italic,
            &config.font_style_bold_italic,
        ]
        .map(|style| match style {
            config::FontStyleRequest::Default => rustty_font::FontStyleRequest::Default,
            config::FontStyleRequest::Disabled => rustty_font::FontStyleRequest::Disabled,
            config::FontStyleRequest::Named(name) => {
                rustty_font::FontStyleRequest::Named(name.clone())
            }
        }),
        codepoint_map: config
            .font_codepoint_map
            .iter()
            .map(|m| rustty_font::CodepointMap {
                start: m.start,
                end: m.end,
                family: m.family.clone(),
            })
            .collect(),
        synthetic_styles: config.font_synthetic_style,
        thicken: config.font_thicken,
        thicken_strength: config.font_thicken_strength,
        features: config
            .font_feature
            .iter()
            .filter_map(|feature| {
                let (tag, value) = feature.split_once('=').unwrap_or((feature, "1"));
                let (tag, value) = if let Some(tag) = tag.strip_prefix('-') {
                    (tag, 0)
                } else {
                    (tag.strip_prefix('+').unwrap_or(tag), value.parse().ok()?)
                };
                Some(FontFeature {
                    tag: tag.as_bytes().try_into().ok()?,
                    value,
                })
            })
            .collect(),
    }
}

impl App {
    fn start_sessions(&mut self) -> Result<()> {
        let tmuxx_mode = self.startup_tmuxx.take();
        let loaded = &self.loaded;
        let proxy = &self.proxy;
        let tmuxx_smoke = &self.tmuxx_smoke;
        let workspace = &mut self.workspace;
        self.tmuxx = if let Some(mode) = tmuxx_mode.as_deref().filter(|mode| *mode != "persistent")
        {
            let wake_proxy = proxy.clone();
            let wake = Arc::new(move || {
                let _ = wake_proxy.send_event(Event::Tmuxx);
            });
            let (source, connection) = if mode == "embedded" {
                let endpoint = match std::env::var_os("TMUXX_ENDPOINT") {
                    Some(endpoint) => PathBuf::from(endpoint),
                    None => tmuxx_host::temporary_endpoint(&format!(
                        "rustty-{}-startup",
                        std::process::id()
                    ))?,
                };
                let initial = if tmuxx_smoke.is_some() {
                    vec![b"/bin/sh".to_vec()]
                } else {
                    match loaded
                        .config
                        .initial_command
                        .as_ref()
                        .or(loaded.config.command.as_ref())
                    {
                        Some(config::Command::Shell(command)) => vec![command.as_bytes().to_vec()],
                        Some(config::Command::Direct(argv)) => {
                            argv.iter().map(|arg| arg.as_bytes().to_vec()).collect()
                        }
                        None => Vec::new(),
                    }
                };
                let connection = rustty::tmuxx::Connection::embedded_with_options(
                    tmuxx_host::server_options(&loaded.config, &endpoint)?,
                    wake,
                    initial,
                )?;
                (tmuxx_host::Source::Embedded(endpoint), connection)
            } else if let Some(host) = mode.strip_prefix("system-ssh:") {
                (
                    tmuxx_host::Source::SystemSsh(host.into()),
                    rustty::tmuxx::Connection::system_ssh_with_env(host, &loaded.config.env, wake)?,
                )
            } else if let Some(host) = mode.strip_prefix("ssh:") {
                (
                    tmuxx_host::Source::Ssh(host.into()),
                    rustty::tmuxx::Connection::ssh_command_with_env(
                        host,
                        loaded.config.tmuxx_ssh_command_for(host),
                        &loaded.config.env,
                        wake,
                    )?,
                )
            } else {
                let endpoint = tmuxx_host::local_endpoint(mode)?;
                let connection = rustty::tmuxx::Connection::native(&endpoint, wake)?;
                (tmuxx_host::Source::Local(endpoint), connection)
            };
            let deadline = Instant::now() + tmuxx_host::connection_timeout(&source);
            while connection.graph().is_none() {
                if Instant::now() > deadline {
                    return Err("timed out waiting for tmuxx snapshot".into());
                }
                let errors = connection.errors();
                if !errors.is_empty() {
                    return Err(errors.join("; ").into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if let Some(directory) = &tmuxx_smoke {
                std::fs::create_dir_all(directory)?;
                connection.command([
                    "split-window",
                    "-h",
                    "/bin/sh",
                    "-c",
                    r"printf '\033[2J\033[HNative tmuxx pane B\n'; sleep 30",
                ])?;
                let deadline = Instant::now() + Duration::from_secs(5);
                while connection.graph().is_none_or(|graph| graph.panes.len() < 2) {
                    if Instant::now() > deadline {
                        return Err("tmuxx smoke split did not appear".into());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                if std::env::var_os("RUSTTY_TMUXX_SMOKE_FLOATING").is_some() {
                    connection.command([
                        "break-pane",
                        "-W",
                        "-s",
                        "%1",
                        "-x",
                        "42",
                        "-y",
                        "10",
                        "-X",
                        "-2",
                        "-Y",
                        "4",
                    ])?;
                    while connection.graph().is_none_or(|graph| {
                        !graph
                            .windows
                            .iter()
                            .any(|window| window.layout.contains("\"z\""))
                    }) {
                        if Instant::now() > deadline {
                            return Err("tmuxx smoke floating pane did not appear".into());
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
            if tmuxx_smoke.is_some() {
                *workspace = Workspace::default();
            }
            let mut attachment = tmuxx_host::Attachment::new(source, Some(connection), false);
            let graph = attachment.connection.as_ref().unwrap().graph().unwrap();
            attachment
                .selected
                .extend(graph.sessions.iter().map(|s| s.id));
            attachment
                .mapping
                .apply(workspace, &graph, &attachment.selected)?;
            vec![attachment]
        } else {
            Vec::new()
        };
        if handoff_ui::smoke_directory().is_none() {
            self.restore_tmuxx();
        }
        if tmuxx_mode.as_deref() == Some("persistent") {
            self.start_tmuxx(false)?;
        }
        Ok(())
    }

    fn config(&self) -> &Config {
        &self.loaded.config
    }
    fn index(&self, id: Id) -> Option<usize> {
        self.workspace
            .windows
            .iter()
            .position(|window| window.id == id)
    }
    fn tab(&self, id: Id) -> Option<&Tab> {
        let window = &self.workspace.windows[self.index(id)?];
        window.tabs.get(window.active_tab)
    }
    fn tab_mut(&mut self, id: Id) -> Option<&mut Tab> {
        let index = self.index(id)?;
        let window = &mut self.workspace.windows[index];
        window.tabs.get_mut(window.active_tab)
    }
    fn focused(&self, id: Id) -> Option<Id> {
        self.tab(id).map(|tab| tab.focused)
    }
    fn visible_panes(&self) -> HashSet<Id> {
        let mut visible = HashSet::new();
        for host in self.windows.values() {
            if let Some(index) = self.index(host.id) {
                visible.extend(window_visible_panes(
                    &self.workspace.windows[index],
                    host.visible,
                    host.occluded,
                    host.peek.is_some(),
                ));
            }
        }
        visible
    }
    fn sync_host_state(&mut self) {
        self.deck.dirty = true;
        let now = Instant::now();
        let mut focused_panes = HashSet::new();
        for host in self.windows.values_mut() {
            let focused = self
                .workspace
                .windows
                .iter()
                .find(|window| window.id == host.id)
                .and_then(|window| window.tabs.get(window.active_tab))
                .map(|tab| tab.focused)
                .filter(|_| host.focused && host.visible);
            focused_panes.extend(focused);
            if host.focus_hint.focus(focused, now) {
                host.repaint();
            }
        }
        let visible = self.visible_panes();
        let schemes = self
            .workspace
            .windows
            .iter()
            .flat_map(|window| {
                let scheme = self.window_color_scheme(window.id);
                window
                    .tabs
                    .iter()
                    .flat_map(Tab::pane_ids)
                    .map(move |pane| (pane, scheme))
            })
            .collect::<HashMap<_, _>>();
        for (&id, pane) in &mut self.panes {
            let scheme = schemes
                .get(&id)
                .copied()
                .unwrap_or_else(|| color_scheme(self.config_loader.dark_mode));
            let state = (visible.contains(&id), focused_panes.contains(&id), scheme);
            if pane.host_state.1 && !state.1 {
                pane.reset_selection_gesture();
            }
            if !state.0 {
                pane.activity.reset_progress_animation();
            }
            if !pane.exited && pane.session.accepts_input() && pane.host_state != state {
                match pane.session.set_host_state(state.0, state.1, state.2) {
                    Ok(()) => pane.host_state = state,
                    Err(error) => self.errors.push(error.to_string()),
                }
            }
        }
    }
    fn changed(&mut self) {
        self.deck.dirty = true;
        self.repaint_outliner();
        // Continuous resizing or title updates must not postpone saving forever.
        self.save_at
            .get_or_insert_with(|| Instant::now() + Duration::from_millis(500));
    }
    fn remember(&mut self) {
        if !self.config().undo_timeout.is_zero() {
            self.history.push((
                Instant::now() + self.config().undo_timeout,
                self.saved_workspace(),
            ));
        }
        if self.history.len() > 50 {
            self.history.remove(0);
        }
        self.redo.clear();
    }
    fn directory(&self, window: Id) -> PathBuf {
        self.focused(window)
            .filter(|_| {
                !self.tmuxx_window(window).is_some_and(|entry| {
                    matches!(
                        &entry.source,
                        tmuxx_host::Source::Ssh(_) | tmuxx_host::Source::SystemSsh(_)
                    )
                })
            })
            .and_then(|id| self.panes.get(&id))
            .map(|pane| pane.cwd.clone())
            .or_else(|| self.config().working_directory.clone())
            .unwrap_or_else(|| self.config_loader.home.clone())
    }
    fn add_window(&mut self, quick: bool) -> Id {
        let directory = self.directory(self.active.unwrap_or(0));
        let id = self.workspace.id();
        let tab = self.workspace.id();
        let pane = self.workspace.id();
        let frame = if quick {
            self.platform
                .as_ref()
                .and_then(|platform| platform.quick_terminal_frame(self.config(), None))
        } else {
            None
        }
        .unwrap_or([100.0, 100.0, 1000.0, 680.0]);
        self.workspace.windows.push(WindowState {
            id,
            tabs: vec![Tab::new(tab, pane, directory)],
            active_tab: 0,
            frame,
            quick,
        });
        self.changed();
        id
    }
    fn spawn_pane(
        &mut self,
        id: Id,
        directory: PathBuf,
        host: Option<&Host>,
        background: Option<[u8; 3]>,
    ) -> Result<()> {
        if self.panes.contains_key(&id)
            || self.failed_panes.contains_key(&id)
            || self.starting_panes.contains_key(&id)
        {
            return Ok(());
        }
        let visual = self.pending_visuals.get(&id).cloned();
        let result = self.spawn_pane_with_visual(id, directory, host, background, visual);
        if let Err(error) = &result {
            self.failed_panes.insert(id, error.to_string());
        }
        result
    }
    fn spawn_pane_with_visual(
        &mut self,
        id: Id,
        directory: PathBuf,
        host: Option<&Host>,
        background: Option<[u8; 3]>,
        visual: Option<Arc<Vec<u8>>>,
    ) -> Result<()> {
        let directory = if self.tmuxx_pane(id).is_some() {
            PathBuf::new()
        } else {
            directory
                .is_absolute()
                .then(|| directory.clone())
                .or_else(|| directory_from_osc(&directory.to_string_lossy()))
                .filter(|path| path.is_dir())
                .or_else(|| {
                    self.config()
                        .working_directory
                        .clone()
                        .filter(|path| path.is_dir())
                })
                .unwrap_or_else(|| self.config_loader.home.clone())
        };
        let wake_pending = Arc::new(AtomicBool::new(false));
        let pending = wake_pending.clone();
        let proxy = self.proxy.clone();
        let wake = Arc::new(move || {
            if !pending.swap(true, Ordering::AcqRel) {
                let _ = proxy.send_event(Event::Output(id));
            }
        });
        let initial = *self.initial_command_pane.get_or_insert(id);
        let command = (id == initial)
            .then(|| self.config().initial_command.clone())
            .flatten();
        let started = Instant::now();
        let visible = self.workspace.windows.iter().any(|window| {
            let native = host
                .filter(|host| host.id == window.id)
                .or_else(|| self.windows.values().find(|host| host.id == window.id));
            initial_visible_panes(
                window,
                native.map(|host| (host.visible, host.occluded, host.peek.is_some())),
            )
            .contains(&id)
        });
        let focused = self.workspace.windows.iter().any(|window| {
            let native = host
                .filter(|host| host.id == window.id)
                .or_else(|| self.windows.values().find(|host| host.id == window.id));
            native.is_some_and(|host| host.focused && host.visible)
                && window
                    .tabs
                    .get(window.active_tab)
                    .is_some_and(|tab| tab.focused == id)
        });
        let pane_window = self.pane_window(id);
        let scheme = pane_window.map_or(color_scheme(self.config_loader.dark_mode), |window| {
            self.window_color_scheme(window)
        });
        let host_state = (visible, focused, scheme);
        let startup = PaneStartup {
            directory: directory.clone(),
            reload_config: false,
            saved: self
                .workspace
                .windows
                .iter()
                .flat_map(|window| &window.tabs)
                .find_map(|tab| tab.panes.get(&id))
                .cloned(),
            host_state,
            wake_pending,
            started,
            input: VecDeque::new(),
            input_bytes: 0,
        };
        let tmuxx = self.tmuxx_pane(id);
        let created = if let Some(entry) = tmuxx {
            Session::attach_tmuxx(
                entry
                    .connection
                    .as_ref()
                    .ok_or("tmuxx connection is offline")?,
                entry.mapping.panes[&id],
                wake,
            )
        } else {
            let host = host.or_else(|| {
                self.windows
                    .values()
                    .find(|host| Some(host.id) == pane_window)
            });
            let (cols, rows) = visual
                .as_ref()
                .and(host)
                .and_then(|host| {
                    let tab = self
                        .workspace
                        .windows
                        .iter()
                        .flat_map(|window| &window.tabs)
                        .find(|tab| tab.panes.contains_key(&id))?;
                    let scale = host.window.scale_factor() as f32;
                    let metrics = host.fonts.metrics();
                    let minimum = pane_minimum_size(
                        [metrics.cell_width, metrics.cell_height],
                        scale,
                        self.window_config(host.id),
                    );
                    let rect = tab
                        .layout_with_minimum(
                            host.content,
                            host.peek.is_some(),
                            metrics.cell_height as f32 / scale,
                            minimum,
                        )
                        .into_iter()
                        .find(|(pane, _)| *pane == id)
                        .or_else(|| {
                            tab.layout_with_minimum(
                                host.content,
                                true,
                                metrics.cell_height as f32 / scale,
                                minimum,
                            )
                            .into_iter()
                            .find(|(pane, _)| *pane == id)
                        })?
                        .1;
                    Some(pane_grid_size(
                        Vec2::new(rect.width, rect.height) - Vec2::splat(2.0),
                        scale,
                        [metrics.cell_width, metrics.cell_height],
                        self.window_config(host.id),
                    ))
                })
                .unwrap_or((80, 24));
            let options = SessionOptions {
                cols,
                rows,
                working_directory: Some(directory.clone()),
                command,
                resources: self.resources.clone(),
                color_scheme: Some(host_state.2),
                background,
                visible: host_state.0,
                focused: host_state.1,
            };
            if let Some(visual) = visual {
                // Initial import stays transactional; later tabs must not wait for history or shells.
                if !self.import_selector {
                    let config = self.config().clone();
                    let proxy = self.proxy.clone();
                    let (sender, receiver) = mpsc::channel();
                    std::thread::Builder::new()
                        .name("rustty-pane-restore".into())
                        .spawn(move || {
                            let session = Session::spawn_restored(&config, options, wake, &visual);
                            if sender.send(session).is_ok() {
                                // Early PTY output may already have consumed the coalesced wake.
                                let _ = proxy.send_event(Event::Output(id));
                            }
                        })?;
                    self.starting_panes.insert(id, (startup, receiver));
                    return Ok(());
                }
                Session::spawn_restored(self.config(), options, wake, &visual)
            } else {
                Session::spawn(self.config(), options, wake)
            }
        };
        self.finish_spawn_pane(id, startup, created)
    }
    fn finish_spawn_pane(
        &mut self,
        id: Id,
        startup: PaneStartup,
        created: std::io::Result<Session>,
    ) -> Result<()> {
        let PaneStartup {
            directory,
            saved,
            host_state,
            wake_pending,
            started,
            input,
            input_bytes,
            ..
        } = startup;
        let session = match created {
            Ok(session) => session,
            Err(error) => {
                self.failed_panes.insert(id, error.to_string());
                return Err(error.into());
            }
        };
        if self.tmuxx_pane(id).is_some() {
            if let Some(window) = self
                .pane_window(id)
                .filter(|window| self.tmuxx_theme(*window).is_some())
            {
                session.apply_appearance(self.window_config(window))?;
            }
            session.set_host_state(host_state.0, host_state.1, host_state.2)?;
        }
        if let Some(diagnostic) = session.shell().diagnostic()
            && !self.errors.iter().any(|error| error == diagnostic)
        {
            self.errors.push(diagnostic.to_owned());
        }
        let saved = self
            .workspace
            .windows
            .iter()
            .flat_map(|window| &window.tabs)
            .find_map(|tab| tab.panes.get(&id))
            .or(saved.as_ref());
        let title_override = saved.and_then(|pane| pane.title_override.clone());
        let title = title_override
            .clone()
            .or_else(|| saved.and_then(|pane| pane.title.clone()))
            .unwrap_or_default();
        let directory_report = session.terminal()?.working_directory.clone();
        let directory = session
            .directory_from_report(&directory_report)
            .unwrap_or(directory);
        self.panes.insert(
            id,
            Pane {
                agent: rustty_app::deck::Report::default(),
                deck_progress: rustty_app::deck::ProgressReport::default(),
                deck_command: rustty_app::deck::CommandReport::default(),
                session,
                host_state,
                wake_pending,
                input,
                input_bytes,
                title,
                title_override,
                cwd: directory,
                last_directory_report: directory_report,
                running: None,
                activity: Activity::default(),
                unseen: false,
                exited: false,
                started,
                exit_message: None,
                links: vt::search::LinkMatcher::default(),
                selection_gesture: vt::selection_gesture::SelectionGesture::default(),
                mouse_cell: None,
                scroll: input::ScrollAccumulator::default(),
                sync_output: SynchronizedOutput::default(),
                search: None,
            },
        );
        self.pending_visuals.remove(&id);
        Ok(())
    }

    fn poll_starting_pane(&mut self, id: Id) {
        let Some((_, receiver)) = self.starting_panes.get(&id) else {
            return;
        };
        let created = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(std::io::Error::other("terminal startup worker stopped"))
            }
        };
        let (startup, _) = self.starting_panes.remove(&id).unwrap();
        // Config and window focus may have changed while the pane was starting.
        let created = created.and_then(|session| {
            if startup.reload_config {
                session.apply_config(self.config())?;
            } else {
                session.apply_appearance(self.config())?;
            }
            Ok(session)
        });
        if let Err(error) = self.finish_spawn_pane(id, startup, created) {
            self.errors.push(error.to_string());
        }
        self.sync_host_state();
        self.repaint_pane(id, true);
    }
    fn open_window(&mut self, event_loop: &ActiveEventLoop, id: Id) -> Result<()> {
        if self.windows.values().any(|host| host.id == id) {
            return Ok(());
        }
        let state = self.workspace.windows[self.index(id).ok_or("missing window")?].clone();
        self.open_host(event_loop, state)
    }
    fn open_host(&mut self, event_loop: &ActiveEventLoop, state: WindowState) -> Result<()> {
        let id = state.id;
        let config = self.window_config(id).clone();
        let activate =
            !(self.background_smoke || cfg!(target_os = "macos") && self.smoke.is_some());
        let quick = state.quick;
        let mut frame = state.frame;
        if quick
            && let Some(bounds) = self.platform.as_ref().and_then(|platform| {
                platform.quick_terminal_frame(&config, Some([frame[2], frame[3]]))
            })
        {
            frame = bounds;
        }
        let attributes = Window::default_attributes()
            .with_title("Rustty")
            .with_active(activate)
            .with_decorations(!quick)
            .with_visible(false)
            .with_inner_size(LogicalSize::new(
                frame[2].max(MIN_WINDOW_SIZE.width),
                frame[3].max(MIN_WINDOW_SIZE.height),
            ))
            .with_position(LogicalPosition::new(frame[0], frame[1]))
            .with_min_inner_size(MIN_WINDOW_SIZE)
            .with_transparent(config.background_opacity < 1.0);
        #[cfg(target_os = "macos")]
        let attributes = attributes
            // AppKit consumes the activation click before any terminal/UI action.
            .with_accepts_first_mouse(false)
            .with_nonactivating_panel(quick)
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
            .with_title_hidden(true);
        #[cfg(target_os = "windows")]
        let attributes = attributes.with_skip_taskbar(quick);
        let window = Arc::new(event_loop.create_window(attributes)?);
        #[cfg(target_os = "macos")]
        if self.smoke.is_some()
            && !self.background_smoke
            && let Some(monitor) = smoke::monitor(event_loop)
        {
            let position = monitor.position().to_logical::<f64>(monitor.scale_factor());
            // AppKit can constrain the initial frame to the active screen.
            window.set_outer_position(LogicalPosition::new(position.x + 40.0, position.y + 40.0));
        }
        if let Some(platform) = &self.platform {
            platform.configure_window(&window, quick, &config)?;
        }
        let viewport = ViewportId::from_hash_of(id);
        pollster::block_on(self.painter.set_window(viewport, window.clone()))?;
        let fonts =
            rustty_render::Renderer::new(font_config(&config, window.scale_factor() as f32))?;
        for family in fonts.missing_families() {
            self.errors
                .push(format!("Font family unavailable: {family}"));
        }
        let mut egui = egui_winit::State::new(
            self.context.clone(),
            viewport,
            &*window,
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(self.painter.max_texture_side()),
        );
        if !self.background_smoke {
            egui.init_accesskit(event_loop, &window, self.proxy.clone());
        }
        if !self.import_selector {
            for (pane, directory) in startup_panes(&state, self.tmuxx_window(state.id).is_some()) {
                self.spawn_pane(pane, directory, None, None)?;
            }
        }
        #[cfg(target_os = "macos")]
        if !quick && !self.background_smoke && self.smoke.is_some() {
            // Winit's set_visible(true) makes a window key even with with_active(false).
            self.platform
                .as_ref()
                .ok_or("macOS platform unavailable")?
                .preview_window(&window, false, &config)?;
        } else {
            window.set_visible(!quick && !self.background_smoke);
        }
        #[cfg(not(target_os = "macos"))]
        window.set_visible(!quick && !self.background_smoke);
        let host = Host {
            id,
            tmuxx_theme: self.tmuxx_theme(id).map(str::to_owned),
            viewport,
            window: window.clone(),
            egui,
            accesskit_active: false,
            fonts,
            rects: BTreeMap::new(),
            pane_order: Vec::new(),
            modal_pane: None,
            overlay_panes: std::collections::BTreeSet::new(),
            prepared: BTreeMap::new(),
            composed: None,
            zoom_animation: zoom_animation::State::default(),
            pane_prepares: 0,
            content: Rect::UNIT,
            min_inner_size: MIN_WINDOW_SIZE,
            divider_drag: None,
            modifiers: Modifiers::default(),
            consumed_keys: HashSet::new(),
            sequence: Vec::new(),
            sequence_len: 0,
            composing: false,
            preedit: String::new(),
            preedit_selection: None,
            peek: None,
            presented_peek: None,
            navigation_warning: None,
            mouse: Pos2::ZERO,
            deferred_pointer: None,
            link_hit: None,
            hovered_link: None,
            mouse_button: None,
            selection_drag: None,
            focused: false,
            focus_hint: FocusHint::default(),
            cursor_blink_started: Instant::now(),
            #[cfg(target_os = "macos")]
            cursor: CursorState {
                enabled: std::env::var("RUSTTY_CURSOR_LAYER").as_deref() != Ok("0"),
                ..Default::default()
            },
            #[cfg(target_os = "macos")]
            progress: native_progress::NativeProgressLayers::default(),
            visible: !quick,
            quick_used: false,
            occluded: false,
            deadline: None,
            search_focus: None,
            search_rects: BTreeMap::new(),
            focus_text_input: false,
            unfocus_text_input: false,
            popup_open: false,
            ui_areas: input::UiAreas::default(),
            messages_open: false,
            messages_focused: false,
            layout_picker: None,
            palette: CommandPalette::default(),
            confirm: None,
            tmuxx_connect: None,
            clipboard_request: VecDeque::new(),
            capture: false,
            frames: 0,
            last_frame_started: None,
        };
        self.windows.insert(window.id(), host);
        if !quick {
            if activate {
                window.focus_window();
            }
            self.active = Some(id);
        }
        window.request_redraw();
        Ok(())
    }
    fn quick_visible(&mut self, host: &mut Host, visible: bool, restore_focus: bool) {
        let result = if self.background_smoke {
            Ok(())
        } else if let Some(platform) = &self.platform {
            if visible {
                platform.show_quick(&host.window, self.config())
            } else {
                platform.hide_quick(&host.window, restore_focus, self.config())
            }
        } else {
            Err("macOS window services are unavailable".into())
        };
        match result {
            Ok(()) => {
                host.visible = visible;
                host.quick_used |= visible;
                if !visible {
                    self.cancel_selection_drag(host);
                }
                self.remember_quick_frame(host);
                host.repaint();
                self.repaint_outliner();
            }
            Err(error) => self.errors.push(error),
        }
    }

    fn remember_quick_frame(&mut self, host: &Host) -> bool {
        let Some(index) = self
            .index(host.id)
            .filter(|&i| self.workspace.windows[i].quick)
        else {
            return false;
        };
        if let Some(platform) = &self.platform {
            match platform.quick_terminal_saved_frame(&host.window) {
                Ok(frame) if self.workspace.windows[index].frame != frame => {
                    self.workspace.windows[index].frame = frame;
                    self.changed();
                }
                Ok(_) => {}
                Err(error) => self.errors.push(error),
            }
        }
        true
    }

    fn save(&mut self) {
        self.save_at = None;
        if self.import_selector {
            return;
        }
        if self.config().window_save_state == config::WindowSaveState::Never {
            return;
        }
        let workspace = self.saved_workspace();
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        let result = if let Some(state) = &self.handoff {
            state.store.save_workspace(&self.state_path, &workspace)
        } else {
            workspace.save(&self.state_path)
        };
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let result = workspace.save(&self.state_path);
        if let Err(error) = result {
            self.errors.push(format!("Could not save windows: {error}"));
        }
    }
    fn write(&mut self, pane: Id, bytes: Vec<u8>) {
        if !self.panes.contains_key(&pane)
            && let Some(directory) = self
                .workspace
                .windows
                .iter()
                .flat_map(|w| &w.tabs)
                .find_map(|tab| {
                    tab.panes
                        .get(&pane)
                        .map(|saved| saved.working_directory.clone())
                })
            && let Err(error) = self.spawn_pane(pane, directory, None, None)
        {
            self.errors.push(error.to_string());
        }
        self.send_input(pane, SessionInput::Text(bytes));
    }
    fn send_input(&mut self, pane: Id, input: SessionInput) {
        let result = if let Some(pane) = self.panes.get_mut(&pane) {
            pane.send_input(input)
        } else if let Some((startup, _)) = self.starting_panes.get_mut(&pane) {
            queue_input(&mut startup.input, &mut startup.input_bytes, input)
        } else {
            return;
        };
        if let Err(error) = result {
            self.errors.push(error.to_string());
        }
    }
    fn terminal_input(&mut self, host: &mut Host, pane: Id) {
        if !self
            .panes
            .get(&pane)
            .is_some_and(|pane| pane.session.accepts_input())
        {
            return;
        }
        self.cancel_selection_drag(host);
        let now = Instant::now();
        let focused = self
            .focused(host.id)
            .filter(|_| host.focused && host.visible);
        host.focus_hint.focus(focused, now);
        if focused == Some(pane) {
            host.focus_hint.dismiss();
            host.cursor_blink_started = now;
            host.repaint();
        }
    }
    fn paste_target_exists(&self, window: Id, pane: Id) -> bool {
        self.index(window)
            .is_some_and(|index| window_contains_pane(&self.workspace.windows[index], pane))
            && (self.starting_panes.contains_key(&pane)
                || self
                    .panes
                    .get(&pane)
                    .is_some_and(|pane| !pane.exited && pane.session.accepts_input()))
    }
    fn paste(&mut self, host: &mut Host, paste: PendingPaste, approved: bool) {
        if paste.data.is_empty() || !self.paste_target_exists(host.id, paste.pane) {
            return;
        }
        let Ok(mut terminal) = self
            .panes
            .get(&paste.pane)
            .map(|pane| pane.session.terminal())
            .transpose()
        else {
            return;
        };
        if !approved && paste_needs_confirmation(self.config(), terminal.as_deref(), &paste.data) {
            host.confirm = Some(Confirmation::Paste(paste));
            host.repaint();
            return;
        }
        if let Some(terminal) = &mut terminal {
            terminal.screen_mut().viewport_offset = 0;
            terminal.screen_mut().selection = None;
        }
        drop(terminal);
        self.terminal_input(host, paste.pane);
        self.send_input(paste.pane, SessionInput::Paste(paste.data));
        host.repaint();
    }
    fn open_url(&mut self, host: &mut Host, uri: String, cwd: PathBuf, approved: bool) {
        if let Some(platform) = &self.platform {
            match platform.open_url(&uri, &cwd, approved) {
                Ok(Some(handler)) => {
                    host.confirm = Some(Confirmation::OpenUrl { uri, cwd, handler })
                }
                Ok(None) => {}
                Err(error) => self.errors.push(error),
            }
            host.repaint();
        }
    }
    fn drop_file(&mut self, host: &mut Host, position: Pos2, path: &Path) {
        if host.modal_input() || host.ui_areas.contains(position) {
            return;
        }
        let Some(pane) = host.pane_at(position) else {
            return;
        };
        let Some(state) = self.panes.get(&pane) else {
            return;
        };
        let data = match state.session.shell().quote_path(path) {
            Ok(data) => data,
            Err(error) => {
                self.errors
                    .push(format!("Could not quote dropped path: {error}"));
                host.repaint();
                return;
            }
        };
        self.paste(host, PendingPaste { pane, data }, true);
    }
    fn drop_text(&mut self, host: &mut Host, position: Pos2, text: String) {
        if host.modal_input() || host.ui_areas.contains(position) {
            return;
        }
        let Some(pane) = host.pane_at(position) else {
            return;
        };
        self.paste(
            host,
            PendingPaste {
                pane,
                data: text.into_bytes(),
            },
            false,
        );
    }
    fn paste_event(
        &mut self,
        window: Id,
        pane: Id,
        location: vt::clipboard::Location,
    ) -> std::result::Result<bool, String> {
        if !self.paste_target_exists(window, pane) {
            return Ok(true);
        }
        let Some(platform) = &self.platform else {
            return Ok(false);
        };
        let Some(state) = self.panes.get(&pane) else {
            return Ok(false);
        };
        if !state
            .session
            .terminal()
            .map_err(|error| error.to_string())?
            .modes
            .dec(5522)
        {
            return Ok(false);
        }
        // Query only native types, outside the terminal lock. Lazy pasteboard
        // providers must not be asked for payload bytes to announce a paste.
        let request = paste_listing_request(location);
        let vt::clipboard::ReadResult::Success(listing) = platform.clipboard_read(&request) else {
            return Err("Could not list the clipboard's available formats.".into());
        };
        let Pane {
            session,
            input,
            input_bytes,
            ..
        } = self.panes.get_mut(&pane).unwrap();
        let mut terminal = session.terminal().map_err(|error| error.to_string())?;
        let mut output = QueuedInput {
            session,
            queue: input,
            bytes: input_bytes,
        };
        let emitted = emit_paste_event(
            &mut terminal,
            location,
            &listing.available,
            &mut Platform::secure_random,
            &mut output,
        )
        .map_err(|error| error.to_string())?;
        if emitted {
            terminal.screen_mut().viewport_offset = 0;
            terminal.screen_mut().selection = None;
        }
        Ok(emitted)
    }
    fn focus_pane(&mut self, window: Id, pane: Id) {
        let previous = self.focused(window);
        if let Some(tab) = self.tab_mut(window) {
            tab.focus(pane);
        }
        if self.focused(window) != Some(pane) {
            return;
        }
        if let Some(tab) = self.tab(window).map(|tab| tab.id) {
            for entry in &mut self.tmuxx {
                entry.cancel_pending_focus(tab);
            }
        }
        if let Some(previous) = previous.filter(|&id| id != pane)
            && let Some(state) = self.panes.get_mut(&previous)
        {
            state.reset_selection_gesture();
        }
        if let Some(entry) = self.tmuxx_pane(pane)
            && let Some(connection) = entry.view_connection()
            && let Some((session, index, pid)) =
                entry.mapping.focus_target(&self.workspace, window, pane)
            && let Err(error) = connection.focus_pane(session, index, pid)
        {
            self.errors.push(error.to_string());
        }
        if let Some(state) = self.panes.get_mut(&pane) {
            state.unseen = false;
            if let Some(platform) = &self.platform {
                platform.clear_notifications(pane);
            }
        }
        self.changed();
    }
    fn select_outline(&mut self, host: &mut Host, target: outliner::Target) {
        let previous = self.focused(host.id);
        let Some(index) = self.index(host.id) else {
            return;
        };
        host.search_focus = None;
        host.search_rects.clear();
        host.focus_text_input = false;
        host.unfocus_text_input = true;
        if let Some(pane) = outliner::select(&mut self.workspace.windows[index], target) {
            if let Some(previous) = previous.and_then(|id| self.panes.get_mut(&id)) {
                previous.reset_selection_gesture();
            }
            self.cancel_selection_drag(host);
            host.peek = None;
            self.focus_pane(host.id, pane);
            host.repaint();
        }
    }
    fn repaint_outliner(&self) {
        if let Some(host) = &self.outliner {
            host.repaint();
        }
    }
    fn hide_outliner(&mut self) {
        if let Some(host) = &mut self.outliner {
            self.consumed_global_keys.extend(host.held_keys.drain());
            host.visible = false;
            host.focused = false;
            host.deadline = None;
            host.window.set_visible(false);
        }
    }
    fn toggle_outliner(&mut self, event_loop: &ActiveEventLoop) {
        if self.outliner.as_ref().is_some_and(|host| host.visible) {
            self.hide_outliner();
            return;
        }
        if self.outliner.is_none() {
            let Some(platform) = &self.platform else {
                return;
            };
            match outliner_host::OutlinerHost::new(
                event_loop,
                &self.context,
                &mut self.painter,
                platform,
                self.proxy.clone(),
                self.background_smoke,
            ) {
                Ok(host) => self.outliner = Some(host),
                Err(error) => {
                    self.errors.push(error.to_string());
                    return;
                }
            }
        }
        let host = self.outliner.as_mut().unwrap();
        host.visible = true;
        if !self.background_smoke {
            host.window.set_visible(true);
        }
        host.repaint();
    }
    fn refresh_directory_labels(&mut self) {
        let paths = self
            .workspace
            .windows
            .iter()
            .flat_map(|window| &window.tabs)
            .flat_map(|tab| &tab.panes)
            .map(|(&id, saved)| {
                (
                    id,
                    self.panes
                        .get(&id)
                        .map_or(saved.working_directory.as_path(), |pane| pane.cwd.as_path()),
                )
            });
        if paths.clone().count() == self.directory_label_paths.len()
            && paths.clone().all(|(id, path)| {
                self.directory_label_paths
                    .get(&id)
                    .is_some_and(|previous| previous == path)
            })
        {
            return;
        }
        let paths: HashMap<_, _> = paths.map(|(id, path)| (id, path.to_owned())).collect();
        let labels = directory_labels(paths.iter().map(|(&id, path)| (id, path.as_path())));
        self.directory_label_paths = paths;
        if self.directory_labels != labels {
            self.directory_labels = labels;
            for host in self.windows.values() {
                host.repaint();
            }
            self.repaint_outliner();
        }
    }

    fn outline_rows(&self) -> Vec<outliner::Row> {
        let tab_labels = self
            .workspace
            .windows
            .iter()
            .flat_map(|window| &window.tabs)
            .map(|tab| (tab.id, self.tab_name(tab).to_owned()))
            .collect();
        let quick_used = self
            .windows
            .values()
            .filter(|host| host.quick_used)
            .map(|host| host.id)
            .collect();
        let mut rows = outliner::workspace_rows(
            &self.workspace,
            self.active,
            &self.directory_labels,
            &tab_labels,
            &quick_used,
            &self
                .tmuxx
                .iter()
                .flat_map(|entry| {
                    entry
                        .mapping
                        .sessions
                        .keys()
                        .map(|id| (*id, entry.label(&self.config().tmuxx_connections)))
                })
                .collect(),
        );
        outliner::count_working(&mut rows, |id| {
            self.panes.get(&id).is_some_and(Pane::reported_active)
        });
        rows
    }
    fn draw_outliner(
        &mut self,
        event_loop: &ActiveEventLoop,
        capture: Option<&Path>,
    ) -> Result<()> {
        let rows = self.outline_rows();
        let Some(mut host) = self.outliner.take() else {
            return Ok(());
        };
        if !host.visible {
            self.outliner = Some(host);
            return Ok(());
        }
        host.deadline = None;
        let accent = self
            .platform
            .as_ref()
            .and_then(Platform::accent_color)
            .unwrap_or([70, 130, 210]);
        let result = host.draw(
            event_loop,
            &mut self.painter,
            &rows,
            &self.workspace.pinned_panes,
            accent,
            capture,
        );
        self.outliner = Some(host);
        let result = result?;
        if let Some(pane) = result.unpin {
            self.workspace.pinned_panes.retain(|id| *id != pane);
            self.repaint_pane(pane, true);
            self.changed();
        }
        if let Some((pane, before)) = result.reorder
            && self.workspace.reorder_pin(pane, before)
        {
            self.changed();
        }
        if let Some(target) = result.selected {
            self.select_outline_target(target);
        } else if let Some(target) = result.preview {
            self.select_outline_target_with_activation(target, false);
        }
        Ok(())
    }
    fn select_outline_target(&mut self, target: outliner::Target) {
        self.select_outline_target_with_activation(target, true);
    }
    fn select_outline_target_with_activation(&mut self, target: outliner::Target, activate: bool) {
        let Some(id) = outliner::target_window(&self.workspace, target) else {
            return;
        };
        let Some(key) = self
            .windows
            .iter()
            .find_map(|(&key, host)| (host.id == id).then_some(key))
        else {
            return;
        };
        if self.config().quick_terminal_autohide
            && let Some(previous) = self.active.filter(|previous| *previous != id)
            && self
                .index(previous)
                .is_some_and(|index| self.workspace.windows[index].quick)
            && let Some(previous_key) = self.windows.iter().find_map(|(&key, host)| {
                (host.id == previous && host.visible && !host.ui_input()).then_some(key)
            })
        {
            // A previewed quick terminal never gained native focus to lose.
            let mut previous_host = self.windows.remove(&previous_key).unwrap();
            self.quick_visible(&mut previous_host, false, false);
            self.windows.insert(previous_key, previous_host);
        }
        if activate && let Some(host) = &mut self.outliner {
            self.consumed_global_keys.extend(host.held_keys.drain());
        }
        let mut host = self.windows.remove(&key).unwrap();
        let previous = self.focused(id);
        let selected = outliner::select_workspace(&mut self.workspace, self.active, target);
        if activate || selected.is_some() {
            host.search_focus = None;
            host.search_rects.clear();
            host.focus_text_input = false;
            host.unfocus_text_input = true;
        }
        if let Some((_, pane)) = selected {
            if let Some(previous) = previous.and_then(|id| self.panes.get_mut(&id)) {
                previous.reset_selection_gesture();
            }
            self.cancel_selection_drag(&mut host);
            host.peek = None;
            self.focus_pane(id, pane);
        }
        let quick = self.workspace.windows[self.index(id).unwrap()].quick;
        if activate {
            self.reveal_host(&mut host, quick);
        } else if let Some(platform) = &self.platform {
            let result = if self.background_smoke {
                Ok(())
            } else {
                platform.preview_window(&host.window, quick, self.config())
            };
            match result {
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
        self.active = Some(id);
        self.windows.insert(key, host);
        self.repaint_outliner();
        self.sync_host_state();
    }
    fn outliner_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        let mut host = self.outliner.take().unwrap();
        if let WindowEvent::KeyboardInput {
            event: key,
            is_synthetic: false,
            ..
        } = &event
        {
            input::key_is_consumed(&mut host.held_keys, key.physical_key, key.state, true);
            if key.state == ElementState::Pressed
                && let Some(binding) = self
                    .config()
                    .keybinds
                    .iter()
                    .rev()
                    .find(|binding| {
                        binding.table.is_none()
                            && binding.trigger.len() == 1
                            && input::matches(&binding.trigger[0], key, host.modifiers.state())
                            && matches!(binding.actions.as_slice(), [Action::GotoPin(_)])
                    })
                    .cloned()
            {
                self.consumed_global_keys.extend(host.held_keys.drain());
                self.outliner = Some(host);
                self.platform_binding(event_loop, binding);
                return;
            }
            if key.state == ElementState::Pressed
                && !key.repeat
                && !host.search.open
                && outliner_host::opens_search(&key.logical_key, host.modifiers.state())
            {
                host.held_keys.remove(&key.physical_key);
                self.consumed_global_keys.insert(key.physical_key);
                host.search.start();
                host.repaint();
                self.outliner = Some(host);
                // Do not translate the opening slash into text for the new editor.
                return;
            }
        }
        let response = host.egui.on_window_event(&host.window, &event);
        if response.repaint && !matches!(event, WindowEvent::RedrawRequested) {
            host.repaint();
        }
        let mut close = false;
        let mut action = None;
        match &event {
            WindowEvent::CloseRequested => close = true,
            WindowEvent::Focused(focused) => {
                host.focused = *focused;
                if *focused {
                    host.search.focus();
                    host.repaint();
                } else {
                    self.consumed_global_keys.extend(host.held_keys.drain());
                    host.modifiers = Modifiers::default();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => host.modifiers = *modifiers,
            WindowEvent::Resized(size) => {
                if let (Some(width), Some(height)) =
                    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    self.painter.on_window_resized(host.viewport, width, height);
                }
                host.repaint();
            }
            WindowEvent::ScaleFactorChanged { .. } | WindowEvent::Occluded(false) => host.repaint(),
            WindowEvent::ThemeChanged(theme) => {
                self.update_system_theme(event_loop.system_theme().or(Some(*theme)), None);
                host.repaint();
            }
            WindowEvent::KeyboardInput {
                event: key,
                is_synthetic: false,
                ..
            } if key.state == ElementState::Pressed && !key.repeat && !host.search.open => {
                close = key.logical_key
                    == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape);
                action = self.config().keybinds.iter().rev().find_map(|binding| {
                    (binding.table.is_none()
                        && binding.trigger.len() == 1
                        && input::matches(&binding.trigger[0], key, host.modifiers.state())
                        && binding.actions.len() == 1
                        && matches!(
                            binding.actions[0],
                            Action::ToggleOutliner
                                | Action::TogglePin
                                | Action::CloseSurface
                                | Action::CloseTab
                                | Action::CloseWindow
                                | Action::Quit
                        ))
                    .then(|| binding.actions[0].clone())
                });
            }
            _ => {}
        }
        self.outliner = Some(host);
        if close {
            self.hide_outliner();
        } else if let Some(action) = action {
            self.platform_action(event_loop, action, false);
        } else if matches!(event, WindowEvent::RedrawRequested)
            && let Err(error) = self.draw_outliner(event_loop, None)
        {
            self.errors.push(error.to_string());
            self.hide_outliner();
        }
    }
    fn tab_name<'a>(&'a self, tab: &'a Tab) -> &'a str {
        let pane = self.panes.get(&tab.focused);
        tab.title
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.tmuxx
                    .iter()
                    .find_map(|entry| entry.mapping.window_names.get(&tab.id))
                    .map(String::as_str)
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| pane.map(|p| p.title.as_str()).filter(|s| !s.is_empty()))
            .unwrap_or("Terminal")
    }
    fn tab_label(&self, tab: &Tab) -> (String, bool) {
        let title = self.tab_name(tab);
        let working = tab
            .panes
            .keys()
            .filter(|id| self.panes.get(id).is_some_and(Pane::reported_active))
            .count();
        let attention = tab
            .panes
            .keys()
            .any(|id| self.panes.get(id).is_some_and(|p| p.unseen));
        let is_active = tab
            .panes
            .keys()
            .any(|id| self.panes.get(id).is_some_and(|p| p.activity.is_active()));
        let suffix = format!(
            "{}{}",
            if attention { " ●" } else { "" },
            if working > 0 {
                format!(" ▶ {working}")
            } else {
                String::new()
            }
        );
        let label = format!("{}{suffix}", title.chars().take(26).collect::<String>());
        (label, is_active)
    }

    fn pane_tab_label(&self, pane: Id) -> Option<(String, bool)> {
        self.workspace
            .windows
            .iter()
            .flat_map(|window| &window.tabs)
            .find(|tab| tab.panes.contains_key(&pane))
            .map(|tab| self.tab_label(tab))
    }

    fn drain(&mut self, event_loop: &ActiveEventLoop, id: Id) {
        self.poll_starting_pane(id);
        self.deck.dirty = true;
        let previous_tab_label = self.pane_tab_label(id);
        let previously_working = self.panes.get(&id).is_some_and(Pane::reported_active);
        let tmuxx = self.tmuxx_pane(id).is_some();
        let previous_directory = self.panes.get(&id).map(|pane| pane.cwd.clone());
        let live = previous_tab_label.is_some();
        let previous_errors = self.errors.len();
        let mut content_changed = false;
        let mut title_changed = false;
        let mut close = false;
        let mut stopped = false;
        let mut clipboard = Vec::new();
        let focused = self.notification_focused(id, None);
        let config = &self.loaded.config;
        let mut deck_panes = self
            .workspace
            .windows
            .iter()
            .flat_map(|window| &window.tabs)
            .flat_map(|tab| tab.pane_ids())
            .filter(|id| self.panes.get(id).is_some_and(Pane::has_deck_report))
            .count();
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        pane.wake_pending.store(false, Ordering::Release);
        if let Err(error) = pane.flush()
            && !pane.session.has_exited()
        {
            self.errors.push(error.to_string());
        }
        let directory_report = if let Ok(mut terminal) = pane.session.terminal() {
            pane.sync_output.update(&mut terminal, Instant::now());
            Some(terminal.working_directory.clone())
        } else {
            pane.sync_output.deadline = None;
            None
        };
        // Git/MSYS path conversion may invoke cygpath. Never hold the terminal
        // mutex during that call, and only convert reports that actually change.
        // Without a shell report, attached process metadata can change independently.
        if let Some(report) = directory_report
            && (report != pane.last_directory_report || tmuxx && report.is_empty())
        {
            if let Some(directory) = pane.session.directory_from_report(&report) {
                content_changed |= pane.cwd != directory;
                pane.cwd = directory;
            }
            pane.last_directory_report = report;
        }
        let events = pane.session.events().collect::<Vec<_>>();
        for event in events {
            let was_registered = pane.has_deck_report();
            if live
                && !pane.exited
                && let SessionEvent::Effect(effect) = &event
            {
                if pane.agent.is_registered()
                    || pane.deck_command.has_agent_report()
                    || matches!(effect, vt::Effect::AgentStatus(vt::agent::Event::Begin(_)))
                {
                    pane.deck_progress.clear();
                } else {
                    pane.deck_progress.apply(effect, pane.running.is_some());
                }
                if matches!(
                    effect,
                    vt::Effect::Progress { state: 1..=4, .. } | vt::Effect::Notification { .. }
                ) {
                    pane.deck_command.suppress();
                }
            }
            content_changed |= !matches!(
                &event,
                SessionEvent::Effect(vt::Effect::Title(_) | vt::Effect::AgentStatus(_))
            );
            match event {
                SessionEvent::Effect(vt::Effect::AgentStatus(event)) if live && !pane.exited => {
                    if matches!(event, vt::agent::Event::Begin(_)) {
                        self.deck.report_generation += 1;
                        // The reporter can end before OSC 133 D arrives, including codex review.
                        pane.deck_command.agent_reported();
                    }
                    pane.agent
                        .apply(event, self.deck.report_generation, pane.running.is_some());
                }
                SessionEvent::Effect(vt::Effect::Title(title)) => {
                    let title = String::from_utf8_lossy(&title);
                    pane.activity.title_changed(&title);
                    if pane.title_override.is_none() {
                        title_changed |= pane.title != title;
                        pane.title = title.into_owned();
                    }
                }
                SessionEvent::Effect(vt::Effect::Progress { state, value }) => {
                    stopped |= pane.activity.progress_reported(
                        if config.progress_style { state } else { 0 },
                        value,
                        Instant::now(),
                    );
                }
                SessionEvent::Effect(vt::Effect::ProgressReset) => {
                    stopped |= pane.activity.progress_reported(0, None, Instant::now());
                    pane.deck_command.clear();
                }
                SessionEvent::Effect(vt::Effect::CommandStart { command_line }) => {
                    let now = Instant::now();
                    pane.running = Some(now);
                    if live && !pane.exited {
                        self.deck.report_generation += 1;
                        pane.deck_command.start(now, self.deck.report_generation);
                        if config.command_finish_excluded(command_line.as_deref()) {
                            pane.deck_command.suppress();
                        }
                        if pane.agent.is_registered() {
                            pane.deck_command.agent_reported();
                        }
                    }
                    pane.activity.command_started();
                }
                SessionEvent::Effect(vt::Effect::CommandEnd { exit_code }) => {
                    pane.agent.command_ended();
                    let notify = pane.deck_command.finish(
                        Instant::now(),
                        config.notify_on_command_finish,
                        config.notify_on_command_finish_after,
                        focused,
                        exit_code,
                    );
                    stopped |= pane.activity.command_finished();
                    pane.running = None;
                    if notify {
                        pane.unseen = !focused;
                        if live
                            && config.notify_on_command_finish_action.notify
                            && let Some(platform) = &self.platform
                        {
                            let body = format!(
                                "{} — exited {}",
                                pane.cwd.display(),
                                exit_code.unwrap_or(0)
                            );
                            if let Err(error) = platform.notify(id, "Command finished", &body) {
                                self.errors.push(error);
                            }
                        }
                    }
                }
                SessionEvent::NotificationObserved { title, body } if live && !pane.exited => {
                    pane.deck_command.suppress();
                    if !pane.agent.is_registered() && !pane.deck_command.has_agent_report() {
                        pane.deck_progress.apply(
                            &vt::Effect::Notification { title, body },
                            pane.running.is_some(),
                        );
                    }
                }
                SessionEvent::Effect(vt::Effect::Notification { title, body })
                | SessionEvent::NotificationDelivery { title, body } => {
                    pane.unseen = !focused;
                    if live
                        && let Some(platform) = &self.platform
                        && let Err(error) = platform.notify(
                            id,
                            &String::from_utf8_lossy(&title),
                            &String::from_utf8_lossy(&body),
                        )
                    {
                        self.errors.push(error);
                    }
                }
                SessionEvent::Effect(vt::Effect::Bell) => {
                    pane.unseen |= !focused;
                }
                SessionEvent::Effect(
                    effect @ (vt::Effect::ClipboardRead(_) | vt::Effect::ClipboardWrite(_)),
                ) => clipboard.push(effect),
                SessionEvent::OutputClosed => {
                    pane.agent.clear();
                    pane.deck_progress.clear();
                    pane.deck_command.clear();
                }
                SessionEvent::Exited {
                    code,
                    signal,
                    runtime,
                } => {
                    pane.exited = true;
                    pane.agent.clear();
                    pane.deck_progress.clear();
                    pane.deck_command.clear();
                    pane.running = None;
                    pane.title = format!(
                        "Exited {code}{}",
                        signal.map(|s| format!(" ({s})")).unwrap_or_default()
                    );
                    pane.exit_message = Some(format!("{} — press any key to close", pane.title));
                    if self.smoke.is_some() {
                        eprintln!("Native smoke: pane {id} {} after {runtime:?}", pane.title);
                    }
                    close = live && !hold_after_exit(config, runtime);
                }
                // The connection reports a shared transport failure once for all its panes.
                SessionEvent::Error(error) if !tmuxx => self.errors.push(error),
                _ => {}
            }
            let registered = pane.has_deck_report();
            if live && registered != was_registered {
                let was_empty = deck_panes == 0;
                if registered {
                    deck_panes += 1;
                } else {
                    deck_panes -= 1;
                }
                if was_empty != (deck_panes == 0) {
                    self.deck.registration_transition();
                }
            }
        }
        // The child waiter can report exit before the reader delivers its final effects.
        if pane.exited {
            pane.agent.clear();
            pane.deck_progress.clear();
            pane.deck_command.clear();
            pane.running = None;
            stopped |= pane.activity.clear();
        }
        // Only a change in the rendered tab label/underline warrants a hidden-tab
        // repaint. Raw titles can be masked by a custom title or another pane.
        let indicators_changed = stopped || previous_tab_label != self.pane_tab_label(id);
        let directory_changed =
            previous_directory.as_ref() != self.panes.get(&id).map(|pane| &pane.cwd);
        if directory_changed {
            self.refresh_directory_labels();
        }
        if directory_changed
            || title_changed
            || previously_working != self.panes.get(&id).is_some_and(Pane::reported_active)
        {
            self.repaint_outliner();
        }
        if stopped {
            self.activity_stopped(id, Instant::now());
        }
        for request in clipboard {
            self.queue_clipboard(id, request);
        }
        if close {
            self.remember();
            self.workspace.close_pane(id);
            self.changed();
        }
        let mut changed = false;
        for window in &mut self.workspace.windows {
            for tab in &mut window.tabs {
                if let Some(saved) = tab.panes.get_mut(&id)
                    && let Some(pane) = self.panes.get(&id)
                {
                    changed |= pane.update_saved(saved);
                }
            }
        }
        if changed {
            self.changed();
        }
        if let Some(platform) = &self.platform {
            platform.set_badge(
                self.workspace
                    .windows
                    .iter()
                    .flat_map(|window| &window.tabs)
                    .flat_map(|tab| tab.panes.keys())
                    .filter(|id| self.panes.get(id).is_some_and(|pane| pane.unseen))
                    .count(),
            );
        }
        for host in self.windows.values() {
            let Some(window) = self
                .index(host.id)
                .map(|index| &self.workspace.windows[index])
            else {
                continue;
            };
            let Some(tab) = window
                .tabs
                .iter()
                .position(|tab| tab.panes.contains_key(&id))
            else {
                continue;
            };
            if window.tabs[window.active_tab].focused == id {
                self.sync_window_title(host);
            }
            // OSC 22 changes native cursor metadata without changing terminal pixels.
            if !self.background_smoke
                && host.hovered_pane() == Some(id)
                && let Some(cursor) = self.pointer_cursor(host)
            {
                host.window.set_cursor(cursor);
            }
            let title_in_ui = title_changed
                && (host.accesskit_active && host.prepared.contains_key(&id)
                    || matches!(&host.confirm, Some(Confirmation::Paste(paste)) if paste.pane == id)
                    || host
                        .clipboard_request
                        .front()
                        .is_some_and(|pending| pending.pane == id));
            let terminal_changed = tab == window.active_tab
                && (content_changed
                    || !self.panes.get(&id).is_some_and(|pane| {
                        pane.session.terminal().ok().is_some_and(|terminal| {
                            host.prepared.get(&id).is_some_and(|prepared| {
                                prepared.key.matches_terminal(&terminal)
                                    && prepared.frame.generation == host.fonts.generation()
                            })
                        })
                    }));
            // Empty reader/writer wakes use the same check as title-only output.
            // All effects and saved metadata have already been processed above.
            if indicators_changed
                || self.errors.len() != previous_errors
                || title_in_ui
                || terminal_changed
            {
                host.repaint();
            }
        }
        // Output updates this pane's metadata above. Only closing a pane changes
        // the layout and requires reconciling every window and session.
        if close {
            self.reconcile(event_loop);
        }
    }

    fn repaint_pane(&self, pane: Id, indicators_changed: bool) {
        for host in self.windows.values() {
            if self.index(host.id).is_some_and(|index| {
                let window = &self.workspace.windows[index];
                window.tabs.iter().enumerate().any(|(index, tab)| {
                    tab.panes.contains_key(&pane)
                        && (index == window.active_tab || indicators_changed)
                })
            }) {
                // Plain output in hidden tabs does not invalidate the visible terminals.
                host.repaint();
            }
        }
    }

    fn sync_window_title(&self, host: &Host) {
        let title = self
            .focused(host.id)
            .and_then(|id| self.panes.get(&id))
            .map(|pane| {
                if pane.title.is_empty() {
                    pane.cwd.display().to_string()
                } else {
                    pane.title.clone()
                }
            })
            .unwrap_or_else(|| "Rustty".into());
        let title = self.tmuxx_window(host.id).map_or(title.clone(), |entry| {
            format!(
                "{title} — {}",
                entry.label(&self.config().tmuxx_connections)
            )
        });
        let zoom = self.tab(host.id).and_then(zoom_label);
        let title = if let Some(zoom) = zoom {
            format!("{title} [{zoom}] — Rustty")
        } else {
            format!("{title} — Rustty")
        };
        if host.window.title() != title {
            host.window.set_title(&title);
        }
    }

    fn activity_stopped(&mut self, pane: Id, now: Instant) {
        for window in &self.workspace.windows {
            for (index, tab) in window.tabs.iter().enumerate() {
                if !tab.panes.contains_key(&pane) {
                    continue;
                }
                if index != window.active_tab {
                    self.activity_flashes.entry(tab.id).or_default().start(now);
                }
                if pane != tab.focused {
                    self.activity_flashes
                        .entry(pane)
                        .or_default()
                        .start_if_idle(now);
                }
                if let Some(host) = self.windows.values().find(|host| host.id == window.id)
                    && let Some(quadrant) = tab.label_quad(pane, host.peek.is_some())
                    && self
                        .quadrant_label(tab, host, quadrant, host.peek, false)
                        .is_some_and(|label| !label.focused || host.peek.is_some())
                {
                    self.activity_flashes
                        .entry(quadrant)
                        .or_default()
                        .start_if_idle(now);
                }
            }
        }
    }

    fn quadrant_label(
        &self,
        tab: &Tab,
        host: &Host,
        quadrant: Id,
        peek: Option<Peek>,
        retained: bool,
    ) -> Option<DirectoryLabel> {
        // A full-pane zoom does not display its containing quadrant's label.
        let tree = if retained {
            &tab.root
        } else {
            tab.visible_tree(peek.is_some())
        };
        let node = tree.node(quadrant)?;
        let name = common_label(
            node.panes()
                .iter()
                .map(|id| self.directory_labels.get(id).map(String::as_str)),
        );
        let focused = peek.map_or(tab.focused, |peek| peek.target);
        // Eligibility does not depend on whether focus hides the aggregate.
        DirectoryLabel::new(name, node.contains(focused), host.focused, true)
    }

    fn flash_opacity(&self, id: Id, now: Instant, deadline: &mut Option<Instant>) -> f32 {
        let Some(flash) = self.activity_flashes.get(&id) else {
            return 0.0;
        };
        if let Some(next) = flash.next_repaint(now) {
            *deadline = Some(deadline.map_or(next, |old| old.min(next)));
        }
        flash.opacity(now)
    }
}

impl App {
    fn queue_clipboard(&mut self, pane: Id, request: vt::Effect) {
        let supported = match &request {
            vt::Effect::ClipboardRead(read) => read.location,
            vt::Effect::ClipboardWrite(write) => write.location,
            _ => return,
        } != vt::clipboard::Location::Primary;
        let policy = clipboard_policy(self.config(), &request);
        let window = self
            .workspace
            .windows
            .iter()
            .find(|window| window.tabs.iter().any(|tab| tab.panes.contains_key(&pane)))
            .map(|window| window.id);
        let key = self
            .windows
            .iter()
            .find(|(_, host)| Some(host.id) == window)
            .map(|(key, _)| *key);
        let Some(key) = key else {
            self.finish_clipboard(pane, request, false, false, None);
            return;
        };
        let mut host = self.windows.remove(&key).unwrap();
        if policy == config::ClipboardAccess::Ask && supported && host.clipboard_request.len() < 16
        {
            let read_result = match &request {
                vt::Effect::ClipboardRead(read) => Some(
                    self.platform
                        .as_ref()
                        .map_or(vt::clipboard::ReadResult::IoError, |platform| {
                            platform.clipboard_read(read)
                        }),
                ),
                _ => None,
            };
            host.clipboard_request
                .push_back(clipboard_preview::Pending {
                    pane,
                    request,
                    read_result,
                    preview: Default::default(),
                });
            host.repaint();
        } else {
            self.finish_clipboard(
                pane,
                request,
                policy != config::ClipboardAccess::Deny
                    && (policy == config::ClipboardAccess::Allow || !supported),
                false,
                None,
            );
        }
        self.windows.insert(key, host);
    }
    fn finish_clipboard(
        &mut self,
        pane: Id,
        request: vt::Effect,
        allow: bool,
        remember: bool,
        preview: Option<vt::clipboard::ReadResult>,
    ) {
        use vt::clipboard::{ReadResult, WriteResult};
        let Some(session) = self.panes.get(&pane).map(|pane| &pane.session) else {
            return;
        };
        // Native pasteboard providers may block. Never hold the terminal mutex
        // during native access, or while queuing the reply to the PTY worker.
        let reply = match request {
            vt::Effect::ClipboardRead(read) => {
                let mut result = if allow {
                    // Approval sends exactly the representations shown in the preview.
                    preview.unwrap_or_else(|| {
                        self.platform
                            .as_ref()
                            .map_or(ReadResult::IoError, |p| p.clipboard_read(&read))
                    })
                } else {
                    ReadResult::Denied
                };
                if let ReadResult::Success(success) = &mut result {
                    success.remember = remember && read.can_remember;
                }
                session.reply_clipboard_read(read, result)
            }
            vt::Effect::ClipboardWrite(write) => {
                let mut result = if allow {
                    self.platform
                        .as_ref()
                        .map_or(WriteResult::IoError, |p| p.clipboard_write(&write))
                } else {
                    WriteResult::Denied
                };
                if let WriteResult::Success { remember: grant } = &mut result {
                    *grant = remember && write.can_remember;
                }
                session.reply_clipboard_write(write, result)
            }
            _ => return,
        };
        match reply {
            Ok(Some(bytes)) => self.write(pane, bytes),
            Ok(None) => {}
            Err(error) => self.errors.push(error.to_string()),
        }
    }
    fn selection_text(&self) -> Option<String> {
        use vt::clipboard::{Location, Read, ReadResult, Terminator, is_text_mime};
        let ReadResult::Success(success) = self
            .platform
            .as_ref()?
            .clipboard_read(&Read::osc52(Location::Selection, Terminator::St))
        else {
            return None;
        };
        success
            .contents
            .iter()
            .find(|c| is_text_mime(&c.mime))
            .map(|c| String::from_utf8_lossy(&c.data).into_owned())
    }
    fn set_selection_text(&mut self, text: String) {
        use vt::clipboard::{Content, Location, Write, WriteResult};
        let request = Write::osc52(
            Location::Selection,
            vec![Content {
                mime: b"text/plain".to_vec(),
                data: text.into_bytes().into(),
            }],
        );
        if let Some(platform) = &self.platform {
            let result = platform.clipboard_write(&request);
            if !matches!(result, WriteResult::Success { .. }) {
                self.errors
                    .push(format!("Could not copy selection: {result:?}"));
            }
        }
    }
    fn action(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
        action: Action,
        approved: bool,
    ) -> bool {
        let ui_input = host.ui_input();
        let clipboard = if ui_input {
            match action {
                Action::PasteFromClipboard => host.egui.clipboard_text(),
                Action::PasteFromSelection => self.selection_text(),
                _ => None,
            }
        } else {
            None
        };
        if input::edit_menu_action(host.egui.egui_input_mut(), &action, ui_input, clipboard) {
            host.repaint();
            return true;
        }
        let focused = self.focused(host.id);
        let exits_app = self.config().quit_after_last_window_closed
            && (closes_last_window(&self.workspace, host.id, &action)
                || action == Action::CloseAllWindows);
        let quitting = action == Action::Quit || exits_app;
        let option = quit_option_modifier(host.modifiers.state()) || Platform::option_pressed();
        let dump_setting = self.config().session_dump_on_quit;
        if action == Action::CloseWindow
            && let Some(entry) = self.tmuxx_window(host.id)
        {
            if entry.view_connection().is_none() {
                if !approved
                    && let Some(confirmation) =
                        Confirmation::for_quit(None, action.clone(), dump_setting, quitting, option)
                {
                    host.confirm = Some(confirmation);
                    host.repaint();
                    return true;
                }
                host.confirm = None;
                self.close_tmuxx_window(host.id);
                return true;
            }
            host.confirm = Confirmation::for_quit(
                Some(Confirmation::TmuxxClose {
                    target: entry.target(),
                    session: entry.mapping.sessions[&host.id],
                }),
                action,
                dump_setting,
                quitting,
                option,
            );
            host.repaint();
            return true;
        }
        if !approved
            && matches!(
                action,
                Action::CloseSurface
                    | Action::CloseTab
                    | Action::CloseWindow
                    | Action::CloseAllWindows
                    | Action::Quit
            )
        {
            let candidates = match action {
                Action::CloseSurface if !exits_app => focused.into_iter().collect::<Vec<_>>(),
                Action::CloseTab if !exits_app => self
                    .tab(host.id)
                    .map(|tab| tab.pane_ids())
                    .unwrap_or_default(),
                Action::CloseWindow if !exits_app => self
                    .index(host.id)
                    .map(|i| {
                        self.workspace.windows[i]
                            .tabs
                            .iter()
                            .flat_map(|t| t.pane_ids())
                            .collect()
                    })
                    .unwrap_or_default(),
                _ => self
                    .workspace
                    .windows
                    .iter()
                    .flat_map(|window| &window.tabs)
                    .flat_map(|tab| tab.panes.keys().copied())
                    .collect(),
            };
            let needs_confirmation = candidates.iter().any(|id| {
                if matches!(action, Action::Quit | Action::CloseAllWindows)
                    && self.tmuxx_pane(*id).is_some_and(|e| !e.temporary())
                {
                    return false;
                }
                if self.starting_panes.contains_key(id) {
                    return self.config().confirm_close_surface
                        != config::ConfirmCloseSurface::False;
                }
                self.panes.get(id).is_some_and(|p| {
                    !p.exited
                        && (self.config().confirm_close_surface
                            == config::ConfirmCloseSurface::Always
                            || self.config().confirm_close_surface
                                == config::ConfirmCloseSurface::True
                                && (p.running.is_some()
                                    || self.config().shell_integration
                                        == config::ShellIntegration::None))
                })
            });
            let confirmation = if needs_confirmation {
                if matches!(action, Action::CloseSurface | Action::CloseTab)
                    && let Some(entry) = self.tmuxx_window(host.id)
                    && let Some(Ok(argv)) = entry.mapping.action(&self.workspace, host.id, &action)
                {
                    Some(Confirmation::TmuxxTerminate(
                        tmuxx_host::MenuCommand::Execute(entry.target(), argv),
                    ))
                } else {
                    Some(Confirmation::Close(action.clone()))
                }
            } else {
                None
            };
            if let Some(confirmation) =
                Confirmation::for_quit(confirmation, action.clone(), dump_setting, quitting, option)
            {
                host.confirm = Some(confirmation);
                host.repaint();
                return true;
            }
        }
        if let Some(entry) = self
            .tmuxx
            .iter_mut()
            .find(|entry| entry.mapping.sessions.contains_key(&host.id))
            && let Some(command) = entry.mapping.action(&self.workspace, host.id, &action)
        {
            let result = command
                .and_then(|argv| entry.command_action(&self.workspace, host.id, &action, argv));
            if let Err(error) = result {
                self.errors.push(error);
            }
            return true;
        }
        if exits_app {
            // Closing the last window quits the app. Save its intact layout,
            // just as explicit Quit does, before shutdown closes the sessions.
            self.save();
            event_loop.exit();
            return true;
        }
        match action {
            Action::Ignore => {}
            Action::Unbind => return false,
            Action::Text(bytes) => {
                if let Some(id) = focused {
                    if !bytes.is_empty() {
                        self.terminal_input(host, id);
                    }
                    self.write(id, bytes);
                }
            }
            Action::NewWindow => {
                self.remember();
                self.add_window(false);
            }
            Action::NewTab => {
                let directory = self.directory(host.id);
                let tab = self.workspace.id();
                let pane = self.workspace.id();
                self.remember();
                if let Some(index) = self.index(host.id) {
                    let window = &mut self.workspace.windows[index];
                    window.tabs.push(Tab::new(tab, pane, directory.clone()));
                    window.active_tab = window.tabs.len() - 1;
                }
                if let Err(error) = self.spawn_pane(pane, directory, Some(host), None) {
                    self.errors.push(error.to_string());
                }
            }
            Action::NewSplit(direction) => {
                if self
                    .tab(host.id)
                    .is_some_and(|tab| !tab.root.contains(tab.focused))
                {
                    self.errors
                        .push("Choose a tiled panel before creating a split.".into());
                    return false;
                }
                let directory = self.directory(host.id);
                let background = focused.and_then(|id| self.panes.get(&id)).and_then(|pane| {
                    pane.session
                        .terminal()
                        .ok()?
                        .color_override(vt::color::Target::Background)
                });
                let pane = self.workspace.id();
                let split = self.workspace.id();
                self.remember();
                if let Some(tab) = self.tab_mut(host.id) {
                    tab.split(pane, split, direction, directory.clone());
                }
                if let Err(error) = self.spawn_pane(pane, directory, Some(host), background) {
                    self.errors.push(error.to_string());
                }
            }
            Action::GotoSplit(direction) => {
                let from = focused;
                let quadrant = matches!(
                    direction,
                    Direction::QuadrantLeft
                        | Direction::QuadrantRight
                        | Direction::QuadrantUp
                        | Direction::QuadrantDown
                );
                let target = from.and_then(|from| {
                    self.tab(host.id)?
                        .target_for_view(from, direction, host.peek.is_some())
                });
                let target = target.map(|pane| {
                    if quadrant {
                        self.tab(host.id)
                            .and_then(|tab| tab.remembered_for_view(pane, host.peek.is_some()))
                            .unwrap_or(pane)
                    } else {
                        pane
                    }
                });
                let moved = target.is_some() && target != from;
                if quadrant
                    && (host.peek.is_some()
                        || self
                            .tab(host.id)
                            .is_none_or(|tab| tab.quad_roots.is_empty()))
                {
                    if host.peek.is_none()
                        && let Some(tab) = self.tab_mut(host.id)
                    {
                        host.peek = tab.begin_peek(input::modifiers(host.modifiers.state()));
                    }
                    if let Some(peek) = &mut host.peek {
                        peek.navigation_used = true;
                        if let Some(target) = target {
                            peek.target = target;
                        }
                    }
                    if (moved || host.peek.is_some())
                        && let Some(tab) = self.tab_mut(host.id)
                    {
                        tab.zoom = None;
                        tab.quadrant_zoom = None;
                    }
                }
                if !moved {
                    if !quadrant
                        && let Some(tab) = self.tab_mut(host.id)
                        && tab.unzoom_after_blocked_navigation()
                    {
                        host.navigation_warning = None;
                    } else if let Some(from) = from {
                        host.navigation_warning =
                            Some((from, Instant::now() + Duration::from_millis(500)));
                        host.repaint();
                        return quadrant && host.peek.is_some();
                    } else {
                        return false;
                    }
                } else if let Some(target) = target {
                    if !quadrant && let Some(tab) = self.tab_mut(host.id) {
                        tab.zoom = tab.pane_navigation_zoom(target);
                    }
                    host.navigation_warning = None;
                    self.focus_pane(host.id, target);
                }
            }
            Action::ToggleSplitZoom => {
                if let Some(tab) = self.tab_mut(host.id) {
                    tab.toggle_zoom();
                }
            }
            Action::ToggleQuadrantZoom => {
                if let Some(tab) = self.tab_mut(host.id) {
                    tab.toggle_quadrant_zoom();
                }
            }
            Action::ToggleQuadRoot => {
                if let Some(peek) = host.peek.take()
                    && let Some(tab) = self.tab_mut(host.id)
                {
                    let target = tab.finish_peek(peek);
                    tab.focus(target);
                }
                self.remember();
                if let Some(tab) = self.tab_mut(host.id) {
                    tab.toggle_quad_root();
                }
            }
            Action::ResizeSplit { direction, amount } => {
                let minimum = self.split_minimum(host);
                self.remember();
                if let Some(tab) = self.tab_mut(host.id)
                    && tab.root.resize_with_minimum(
                        tab.focused,
                        direction,
                        f32::from(amount),
                        host.content,
                        minimum,
                    )
                {
                    tab.zoom = None;
                    tab.quadrant_zoom = None;
                    host.peek = None;
                }
            }
            Action::EqualizeSplits => {
                self.remember();
                if let Some(tab) = self.tab_mut(host.id) {
                    tab.root.equalize();
                }
            }
            Action::NextTab
            | Action::PreviousTab
            | Action::LastTab
            | Action::GotoTab(_)
            | Action::MoveTab(_) => {
                if let Some(index) = self.index(host.id) {
                    let window = &mut self.workspace.windows[index];
                    let count = window.tabs.len();
                    let old = window.active_tab;
                    let new = match action {
                        Action::NextTab => (old + 1) % count,
                        Action::PreviousTab => (old + count - 1) % count,
                        Action::LastTab => count - 1,
                        Action::GotoTab(tab) => tab.saturating_sub(1).min(count - 1),
                        Action::MoveTab(offset) => {
                            (old as i64 + i64::from(offset)).clamp(0, count as i64 - 1) as usize
                        }
                        _ => old,
                    };
                    if matches!(action, Action::MoveTab(_)) {
                        let tab = window.tabs.remove(old);
                        window.tabs.insert(new, tab);
                    }
                    window.active_tab = new;
                    host.peek = None;
                    let pane = window.tabs[new].focused;
                    self.focus_pane(host.id, pane);
                }
            }
            Action::CloseSurface | Action::CloseTab | Action::CloseWindow => {
                self.remember();
                if let Some(index) = self.index(host.id) {
                    let window = &mut self.workspace.windows[index];
                    let tab = window.active_tab;
                    if matches!(action, Action::CloseWindow) {
                        self.workspace.windows.remove(index);
                    } else {
                        if matches!(action, Action::CloseTab) || {
                            let pane = window.tabs[tab].focused;
                            !window.tabs[tab].close(pane)
                        } {
                            window.tabs.remove(tab);
                        }
                        if window.tabs.is_empty() {
                            self.workspace.windows.remove(index);
                        } else {
                            window.active_tab = tab.min(window.tabs.len() - 1);
                        }
                    }
                }
            }
            Action::CloseAllWindows => {
                self.remember();
                self.detach_all_tmuxx();
                self.workspace.windows.clear();
            }
            Action::Quit => {
                self.save();
                event_loop.exit();
            }
            Action::ToggleQuickTerminal => {
                if self
                    .index(host.id)
                    .is_some_and(|i| self.workspace.windows[i].quick)
                {
                    self.quick_visible(host, !host.visible, true);
                } else {
                    let id = self
                        .workspace
                        .windows
                        .iter()
                        .find(|w| w.quick)
                        .map(|w| w.id)
                        .unwrap_or_else(|| self.add_window(true));
                    if let Err(error) = self.open_window(event_loop, id) {
                        self.errors.push(error.to_string());
                    }
                    let key = self
                        .windows
                        .iter()
                        .find(|(_, h)| h.id == id)
                        .map(|(key, _)| *key);
                    if let Some(key) = key
                        && let Some(mut quick) = self.windows.remove(&key)
                    {
                        let visible = !quick.visible;
                        self.quick_visible(&mut quick, visible, true);
                        self.windows.insert(key, quick);
                    }
                }
            }
            Action::ToggleFullscreen => {
                if self.background_smoke {
                    return false;
                }
                host.window
                    .set_fullscreen(if host.window.fullscreen().is_some() {
                        None
                    } else {
                        Some(Fullscreen::Borderless(None))
                    })
            }
            Action::Reset => {
                if let Some(pane) = focused.and_then(|id| self.panes.get_mut(&id)) {
                    if let Err(error) = pane.session.reset_terminal() {
                        self.errors.push(error.to_string());
                    } else {
                        pane.reset_selection_gesture();
                        pane.mouse_cell = None;
                        pane.sync_output.deadline = None;
                        pane.search = None;
                        pane.activity.progress_reported(0, None, Instant::now());
                        pane.deck_progress.clear();
                        pane.deck_command.clear();
                        if pane.title_override.is_none() {
                            pane.title.clear();
                        }
                        host.search_focus = None;
                        host.selection_drag = None;
                        host.hovered_link = None;
                    }
                }
            }
            Action::ToggleCommandPalette => {
                if self
                    .theme_preview
                    .as_ref()
                    .is_some_and(|preview| preview.owner == host.id)
                {
                    self.cancel_theme_preview(Some(host));
                } else {
                    host.palette.toggle();
                }
                host.focus_text_input = host.palette.open || host.search_focus.is_some();
            }
            Action::ToggleOutliner => {
                self.toggle_outliner(event_loop);
            }
            Action::NewTemporaryTmuxxWindow | Action::NewPersistentTmuxxWindow => {
                if let Err(error) = self.start_tmuxx(action == Action::NewTemporaryTmuxxWindow) {
                    self.errors.push(error.to_string());
                }
            }
            Action::CopyWindowToTmuxx => {
                if let Err(error) = self.copy_window_to_tmuxx(host) {
                    self.errors.push(error.to_string());
                    host.messages_open = true;
                }
            }
            Action::ConnectTmuxx => {
                host.tmuxx_connect = Some("ssh:".into());
            }
            Action::NewFloatingPane => {
                self.errors
                    .push("Floating panels require a tmuxx window.".into());
            }
            Action::TogglePin => {
                if let Some(pane) = focused {
                    self.workspace.toggle_pin(pane);
                }
            }
            Action::GotoPin(index) => {
                let Some((window, tab, pane)) =
                    self.workspace
                        .prepare_pin_navigation(index, host.id, &mut host.peek)
                else {
                    return false;
                };
                let target = outliner::Target::Pane { tab, pane };
                if window == host.id {
                    self.select_outline(host, target);
                    let quick = self.workspace.windows[self.index(window).unwrap()].quick;
                    self.reveal_host(host, quick);
                    self.active = Some(window);
                } else {
                    self.select_outline_target(target);
                }
            }
            Action::Themes => {
                self.cancel_theme_preview(Some(host));
                match self.config_loader.themes(self.loaded.family) {
                    Ok(themes) => {
                        self.theme_preview = Some(ThemePreview {
                            owner: host.id,
                            original: self.config().clone(),
                            scope: self
                                .tmuxx_theme_scope(host.id)
                                .unwrap_or(theme_preview::Scope::Global),
                        });
                        host.palette.show_themes(themes);
                        host.focus_text_input = true;
                    }
                    Err(error) => self.errors.push(format!("Could not list themes: {error}")),
                }
            }
            Action::StreamDeck(command) => {
                if !self.deck_command_with_host(command, Some(host)) {
                    return false;
                }
            }
            Action::CopyToClipboard => {
                let text = focused.and_then(|id| {
                    self.panes
                        .get(&id)?
                        .session
                        .terminal()
                        .ok()?
                        .screen()
                        .selection_text()
                });
                let Some(text) = text else {
                    return false;
                };
                host.egui.set_clipboard_text(text);
            }
            Action::PasteFromClipboard | Action::PasteFromSelection => {
                if let Some(id) = focused {
                    let location = if action == Action::PasteFromSelection {
                        vt::clipboard::Location::Selection
                    } else {
                        vt::clipboard::Location::Standard
                    };
                    match self.paste_event(host.id, id, location) {
                        Ok(true) => self.terminal_input(host, id),
                        Err(error) => self.errors.push(error),
                        Ok(false) => {
                            let text = if action == Action::PasteFromSelection {
                                self.selection_text()
                            } else {
                                host.egui.clipboard_text()
                            };
                            if let Some(text) = text {
                                self.paste(
                                    host,
                                    PendingPaste {
                                        pane: id,
                                        data: text.into_bytes(),
                                    },
                                    false,
                                );
                            }
                        }
                    }
                }
            }
            Action::SelectAll => {
                self.cancel_selection_drag(host);
                if let Some(pane) = focused.and_then(|id| self.panes.get(&id))
                    && let Ok(mut terminal) = pane.session.terminal()
                {
                    let screen = terminal.screen_mut();
                    let first = screen
                        .all_rows()
                        .next()
                        .map(|r| vt::GridPoint { row: r.id, col: 0 });
                    let last = screen.all_rows().next_back().map(|r| vt::GridPoint {
                        row: r.id,
                        col: r.cells.len() - 1,
                    });
                    if let (Some(start), Some(end)) = (first, last) {
                        screen.selection = Some(vt::Selection {
                            start,
                            end,
                            rectangular: false,
                        });
                    }
                }
            }
            Action::ClearScreen => {
                if let Some(id) = focused {
                    self.terminal_input(host, id);
                    self.write(id, b"\x0c".to_vec());
                }
            }
            Action::StartSearch
            | Action::SearchSelection
            | Action::EndSearch
            | Action::NavigateSearch { .. } => {
                return focused.is_some_and(|id| self.search_action(host, id, action));
            }
            Action::ScrollToTop
            | Action::ScrollToBottom
            | Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::ScrollToSelection
            | Action::JumpToPrompt(_) => {
                if let Some(pane) = focused.and_then(|id| self.panes.get(&id))
                    && let Ok(mut terminal) = pane.session.terminal()
                {
                    let screen = terminal.screen_mut();
                    let rows = screen.height() as isize;
                    match action {
                        Action::ScrollToTop => screen.viewport_offset = screen.history_len(),
                        Action::ScrollToBottom => screen.viewport_offset = 0,
                        Action::ScrollPageUp => screen.scroll_viewport(rows),
                        Action::ScrollPageDown => screen.scroll_viewport(-rows),
                        Action::ScrollToSelection => {
                            if let Some(selection) = screen.selection {
                                reveal(screen, selection.start);
                            }
                        }
                        Action::JumpToPrompt(offset) => {
                            let start = screen.history_len().saturating_sub(screen.viewport_offset);
                            let prompts = screen
                                .all_rows()
                                .enumerate()
                                .filter(|(_, r)| {
                                    r.cells
                                        .iter()
                                        .any(|c| c.semantic() == vt::SemanticContent::Prompt)
                                })
                                .map(|(i, _)| i)
                                .collect::<Vec<_>>();
                            let target = if offset < 0 {
                                prompts
                                    .iter()
                                    .rev()
                                    .copied()
                                    .filter(|i| *i < start)
                                    .nth((-i64::from(offset) - 1) as usize)
                            } else {
                                prompts
                                    .iter()
                                    .copied()
                                    .filter(|i| *i > start)
                                    .nth(offset.saturating_sub(1) as usize)
                            };
                            if let Some(target) = target {
                                screen.viewport_offset =
                                    screen.history_len().saturating_sub(target);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Action::OpenConfig => {
                if let Some(platform) = &self.platform
                    && let Err(error) = platform.open_config(&self.loaded.edit_config_path)
                {
                    self.errors.push(error);
                }
            }
            Action::OpenLayout => {
                let choices = workspace::saved_layouts(&self.config_loader.home, &self.state_path);
                let path = choices
                    .iter()
                    .find(|choice| choice.available)
                    .map(|choice| choice.path.to_string_lossy().into_owned())
                    .unwrap_or_default();
                host.layout_picker = Some(LayoutPicker {
                    choices,
                    path,
                    error: None,
                });
            }
            Action::ReloadConfig => {
                if let Some(theme) = event_loop.system_theme() {
                    self.config_loader.dark_mode = theme == Theme::Dark;
                }
                self.reload_config(Some(host));
            }
            Action::IncreaseFontSize(amount) => {
                self.loaded.config.font_size = (self.config().font_size + amount).clamp(4.0, 200.0);
                for theme in self.tmuxx_themes.values_mut() {
                    theme.font_size = (theme.font_size + amount).clamp(4.0, 200.0);
                }
                self.update_fonts(Some(host));
            }
            Action::DecreaseFontSize(amount) => {
                self.loaded.config.font_size = (self.config().font_size - amount).clamp(4.0, 200.0);
                for theme in self.tmuxx_themes.values_mut() {
                    theme.font_size = (theme.font_size - amount).clamp(4.0, 200.0);
                }
                self.update_fonts(Some(host));
            }
            Action::ResetFontSize => {
                let loaded = self.config_loader.load_with_args(&self.config_args);
                self.loaded.config.font_size = loaded.config.font_size;
                for (name, theme) in &mut self.tmuxx_themes {
                    theme.font_size = self
                        .config_loader
                        .load_with_theme(&self.config_args, name)
                        .config
                        .font_size;
                }
                self.update_fonts(Some(host));
            }
            Action::Undo | Action::Redo => {
                if !self.undo_layout(action == Action::Redo) {
                    return false;
                }
            }
        }
        if self.focused(host.id) != focused {
            host.search_focus = None;
            host.search_rects.clear();
        }
        self.changed();
        host.repaint();
        true
    }
    fn undo_layout(&mut self, redo: bool) -> bool {
        let now = Instant::now();
        self.history.retain(|(expires, _)| *expires > now);
        self.redo.retain(|(expires, _)| *expires > now);
        let previous = if redo {
            self.redo.pop()
        } else {
            self.history.pop()
        };
        let Some((_, mut previous)) = previous else {
            return false;
        };
        let current = self.saved_workspace();
        previous.windows.extend(
            self.workspace
                .windows
                .iter()
                .filter(|window| self.tmuxx_window(window.id).is_some())
                .cloned(),
        );
        self.workspace.restore(previous);
        let record = (now + self.config().undo_timeout, current);
        if redo {
            self.history.push(record);
        } else {
            self.redo.push(record);
        }
        self.changed();
        true
    }
    fn update_system_theme(&mut self, theme: Option<Theme>, host: Option<&mut Host>) {
        if let Some(theme) = theme
            && self.config_loader.dark_mode != (theme == Theme::Dark)
        {
            self.config_loader.dark_mode = theme == Theme::Dark;
            self.reload_config(host);
        }
    }
    fn reload_config(&mut self, mut host: Option<&mut Host>) {
        self.cancel_theme_preview(host.as_deref_mut());
        let previous_renderer = self.loaded.config.renderer;
        self.loaded = load_config(&mut self.config_loader, &self.config_args, None);
        if self.background_smoke || self.smoke.is_some() {
            smoke::Smoke::configure(&mut self.loaded);
        }
        #[cfg(debug_assertions)]
        handoff_ui::configure_smoke(&mut self.loaded);
        self.errors = self
            .loaded
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .collect();
        if self.loaded.config.renderer != previous_renderer {
            self.errors
                .push("Restart Rustty to apply the renderer setting.".into());
        }
        self.failed_panes.clear();
        self.context.set_theme(ui_theme(&self.loaded.config));
        if let Some(platform) = &mut self.platform
            && let Err(error) = platform.update_config(&self.loaded.config)
        {
            self.errors.push(error);
        }
        self.reload_tmuxx_themes();
        self.apply_pane_configs(Session::apply_config, &theme_preview::Scope::Global);
        for (startup, _) in self.starting_panes.values_mut() {
            startup.reload_config = true;
        }
        for pane in self.panes.values_mut() {
            if !self.loaded.config.progress_style {
                pane.activity.progress_reported(0, None, Instant::now());
            }
        }
        self.update_fonts(host.as_deref_mut());
        if let Some(host) = host.as_deref() {
            self.sync_window_title(host);
        }
        self.configure_windows(host);
        for host in self.windows.values() {
            self.sync_window_title(host);
        }
        self.sync_tmuxx_menu();
        if let Some(outliner) = &self.outliner {
            outliner.repaint();
        }
    }
    fn configure_windows(&mut self, host: Option<&mut Host>) {
        let configs = self.window_configs();
        let themes = self
            .workspace
            .windows
            .iter()
            .map(|window| (window.id, self.tmuxx_theme(window.id).map(str::to_owned)))
            .collect::<BTreeMap<_, _>>();
        for window in host.into_iter().chain(self.windows.values_mut()) {
            window.tmuxx_theme = themes.get(&window.id).cloned().flatten();
            if let Some(platform) = &self.platform {
                let quick = self
                    .workspace
                    .windows
                    .iter()
                    .any(|state| state.id == window.id && state.quick);
                if let Err(error) = platform.configure_window(
                    &window.window,
                    quick,
                    configs.get(&window.id).unwrap_or(&self.loaded.config),
                ) {
                    self.errors.push(error);
                }
            }
            window.repaint();
        }
    }
    fn apply_theme_config(
        &mut self,
        config: Config,
        mut host: Option<&mut Host>,
        scope: &theme_preview::Scope,
    ) {
        let fonts = self
            .workspace
            .windows
            .iter()
            .map(|window| (window.id, font_config(self.window_config(window.id), 1.0)))
            .collect::<Vec<_>>();
        self.loaded.config = config;
        self.context.set_theme(ui_theme(self.config()));
        self.apply_pane_configs(Session::apply_appearance, scope);
        let fonts_changed = fonts
            .iter()
            .any(|(window, previous)| *previous != font_config(self.window_config(*window), 1.0));
        if fonts_changed {
            self.update_fonts(host.as_deref_mut());
        }
        self.configure_windows(host);
    }
    fn cancel_theme_preview(&mut self, mut host: Option<&mut Host>) {
        let Some(preview) = self.theme_preview.take() else {
            return;
        };
        for window in host
            .iter_mut()
            .map(|host| &mut **host)
            .chain(self.windows.values_mut())
        {
            if window.id == preview.owner {
                window.palette.open = false;
                window.focus_text_input = window.search_focus.is_some();
            }
        }
        self.apply_theme_config(preview.original, host, &preview.scope);
    }
    fn preview_theme(&mut self, host: &mut Host, theme: config::ThemeChoice, accept: bool) {
        let Some(preview) = self
            .theme_preview
            .as_ref()
            .filter(|preview| preview.owner == host.id)
        else {
            return;
        };
        let loaded = self
            .config_loader
            .load_with_theme(&self.config_args, &theme.path.to_string_lossy());
        let errors = loaded
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.path == theme.path
                    || diagnostic
                        .path
                        .file_name()
                        .is_some_and(|name| name == "<theme-preview>")
            })
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if !errors.is_empty() {
            self.cancel_theme_preview(Some(host));
            self.errors.extend(errors);
            return;
        }
        let scope = preview.scope.clone();
        let mut config = theme_preview::appearance(&preview.original, loaded.config);
        if scope != theme_preview::Scope::Global {
            let name = theme.path.to_string_lossy().into_owned();
            self.tmuxx_themes.entry(name.clone()).or_insert(config);
            config = preview.original.clone();
            scope.select(&mut config, &name);
        }
        self.apply_theme_config(config, Some(host), &scope);
        if accept {
            self.theme_preview = None;
            self.errors.push(format!(
                "Theme {} selected. To keep it, add or edit this line in {}:\n\n{}\n\nNo configuration file was changed. Reloading configuration or restarting Rustty restores the saved theme.",
                theme.name,
                self.loaded.edit_config_path.display(),
                scope.config_line(&theme),
            ));
        }
    }
    fn update_fonts(&mut self, host: Option<&mut Host>) {
        let configs = self.window_configs();
        for current in host.into_iter().chain(self.windows.values_mut()) {
            match rustty_render::Renderer::new(font_config(
                configs.get(&current.id).unwrap_or(&self.loaded.config),
                current.window.scale_factor() as f32,
            )) {
                Ok(fonts) => current.fonts = fonts,
                Err(error) => self.errors.push(error.to_string()),
            }
            current.repaint();
        }
    }
    fn search_action(&mut self, host: &mut Host, id: Id, action: Action) -> bool {
        if !self
            .tab(host.id)
            .is_some_and(|tab| tab.panes.contains_key(&id))
        {
            return false;
        }
        let Some(pane) = self.panes.get_mut(&id) else {
            return false;
        };
        let Ok(mut terminal) = pane.session.terminal() else {
            return false;
        };
        match action {
            Action::StartSearch | Action::SearchSelection => {
                let search = pane.search.get_or_insert_with(Search::default);
                if action == Action::SearchSelection {
                    search.query = terminal.screen().selection_text().unwrap_or_default();
                }
                search.refresh(&mut terminal);
                host.search_focus = Some(id);
                host.focus_text_input = true;
            }
            Action::EndSearch => {
                let Some(mut search) = pane.search.take() else {
                    return false;
                };
                search.clear_highlight(&mut terminal);
                host.search_rects.remove(&id);
                if host.search_focus == Some(id) {
                    host.search_focus = None;
                }
            }
            Action::NavigateSearch { next } => {
                let Some(search) = &mut pane.search else {
                    return false;
                };
                search.navigate(&mut terminal, next);
            }
            _ => return false,
        }
        host.repaint();
        true
    }
    fn reconcile(&mut self, event_loop: &ActiveEventLoop) {
        if self.import_selector {
            return;
        }
        if self.workspace.reconcile_pinned_panes() {
            self.changed();
        }
        if self
            .theme_preview
            .as_ref()
            .is_some_and(|preview| self.index(preview.owner).is_none())
        {
            self.cancel_theme_preview(None);
        }
        // With no native windows there is no ThemeChanged event to observe.
        // Refresh before creating a session that could immediately query it.
        if self.windows.is_empty() {
            self.update_system_theme(event_loop.system_theme(), None);
        }
        let removed = self
            .windows
            .iter()
            .filter(|(_, host)| self.index(host.id).is_none())
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in removed {
            if let Some(host) = self.windows.remove(&id) {
                if let Some(platform) = &self.platform {
                    platform.forget_window(&host.window);
                }
                for pending in host.clipboard_request {
                    self.finish_clipboard(pending.pane, pending.request, false, false, None);
                }
                self.painter.remove_window(
                    host.viewport,
                    host.id,
                    &self
                        .windows
                        .values()
                        .map(|host| host.viewport)
                        .chain(self.outliner.iter().map(|host| host.viewport))
                        .collect(),
                );
            }
        }
        let required = self
            .workspace
            .windows
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .map(|(&id, s)| (id, s.working_directory.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut retained = required
            .keys()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        for (id, pane) in &mut self.panes {
            if !required.contains_key(id) {
                pane.agent.clear();
                pane.deck_command.clear();
            }
        }
        for (_, state) in self.history.iter().chain(&self.redo) {
            retained.extend(
                state
                    .windows
                    .iter()
                    .flat_map(|window| &window.tabs)
                    .flat_map(|tab| tab.panes.keys().copied()),
            );
        }
        for (_, mut pane) in self.panes.extract_if(|id, _| !retained.contains(id)) {
            pane.reset_selection_gesture();
            pane.session.close();
            self.closing.push(pane.session);
        }
        self.closing.retain(|session| !session.has_exited());
        self.failed_panes.retain(|id, _| retained.contains(id));
        self.pending_visuals.retain(|id, _| retained.contains(id));
        self.starting_panes.retain(|id, _| retained.contains(id));
        let startup = self
            .workspace
            .windows
            .iter()
            .flat_map(|window| startup_panes(window, self.tmuxx_window(window.id).is_some()))
            .collect::<Vec<_>>();
        for (id, directory) in startup {
            if let Err(error) = self.spawn_pane(id, directory, None, None) {
                self.errors.push(error.to_string());
            }
        }
        let mut metadata_changed = false;
        for window in &mut self.workspace.windows {
            for tab in &mut window.tabs {
                for (id, saved) in &mut tab.panes {
                    if let Some(pane) = self.panes.get(id) {
                        metadata_changed |= pane.update_saved(saved);
                    }
                }
            }
        }
        if metadata_changed {
            self.changed();
        }
        self.refresh_directory_labels();
        for id in self
            .workspace
            .windows
            .iter()
            .map(|w| w.id)
            .collect::<Vec<_>>()
        {
            if let Err(error) = self.open_window(event_loop, id) {
                self.errors.push(error.to_string());
            }
        }
        self.sync_host_state();
        self.sync_tmuxx_menu();
        if self.config().quit_after_last_window_closed
            && !self.workspace.windows.iter().any(window_keeps_app_running)
            && !self.tmuxx.iter().any(|entry| entry.waiting_for_window())
        {
            self.save();
            event_loop.exit();
        }
    }
    fn binding_action(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
        action: Action,
        all: bool,
    ) -> bool {
        if all && let Action::Text(bytes) = action {
            if !bytes.is_empty()
                && let Some(id) = self.focused(host.id)
            {
                self.terminal_input(host, id);
            }
            for id in self
                .workspace
                .windows
                .iter()
                .flat_map(|window| &window.tabs)
                .flat_map(|tab| tab.panes.keys().copied())
                .collect::<Vec<_>>()
            {
                self.write(id, bytes.clone());
            }
            true
        } else {
            self.action(event_loop, host, action, false)
        }
    }

    fn take_active_host(&mut self) -> Option<(WindowId, Host)> {
        let key = self
            .windows
            .iter()
            .find(|(_, host)| Some(host.id) == self.active)
            .or_else(|| self.windows.iter().next())
            .map(|(id, _)| *id)?;
        self.windows.remove_entry(&key)
    }

    fn host_binding(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
        binding: config::KeyBinding,
    ) -> bool {
        let mut target = host.id;
        dispatch_binding(binding, |action, all| {
            let navigation = matches!(
                action,
                Action::GotoPin(_)
                    | Action::StreamDeck(
                        config::StreamDeckAction::NeedsInput
                            | config::StreamDeckAction::Working
                            | config::StreamDeckAction::Done
                            | config::StreamDeckAction::Idle
                            | config::StreamDeckAction::Back
                    )
            );
            let performed = if target == host.id {
                self.binding_action(event_loop, host, action, all)
            } else {
                let key = self
                    .windows
                    .iter()
                    .find_map(|(&key, host)| (host.id == target).then_some(key));
                let Some((key, mut target_host)) =
                    key.and_then(|key| self.windows.remove_entry(&key))
                else {
                    return false;
                };
                // Keep every other host available to global actions such as config reload.
                let original_key = host.window.id();
                std::mem::swap(host, &mut target_host);
                self.windows.insert(original_key, target_host);
                let performed = self.binding_action(event_loop, host, action, all);
                let mut target_host = self.windows.remove(&original_key).unwrap();
                std::mem::swap(host, &mut target_host);
                self.windows.insert(key, target_host);
                performed
            };
            if navigation && performed {
                target = self.active.unwrap_or(target);
            }
            performed
        })
    }

    fn platform_binding(&mut self, event_loop: &ActiveEventLoop, mut binding: config::KeyBinding) {
        let mut target = self.take_active_host();
        if target.is_none() {
            let mut actions = std::mem::take(&mut binding.actions).into_iter();
            for action in actions.by_ref() {
                self.platform_action(event_loop, action, binding.flags.all);
                target = self.take_active_host();
                if target.is_some() {
                    break;
                }
            }
            binding.actions = actions.collect();
        }
        if let Some((_, host)) = &mut target {
            self.host_binding(event_loop, host, binding);
        }
        if let Some((key, host)) = target {
            self.windows.insert(key, host);
            // Reconcile after the chain: closing every window may be followed
            // by creating a new one, which must not exit the event loop midway.
            self.reconcile(event_loop);
        }
    }

    fn platform_action(&mut self, event_loop: &ActiveEventLoop, action: Action, all: bool) -> bool {
        if self.import_selector {
            match action {
                Action::CloseWindow | Action::CloseSurface => {
                    self.cancel_import_selector(event_loop);
                    return true;
                }
                Action::Quit => {
                    event_loop.exit();
                    return true;
                }
                _ => {}
            }
            return false;
        }
        if self
            .outliner
            .as_ref()
            .is_some_and(|host| host.visible && host.focused && host.search.open)
        {
            let clipboard = match action {
                Action::PasteFromClipboard => self.outliner.as_mut().unwrap().egui.clipboard_text(),
                Action::PasteFromSelection => self.selection_text(),
                _ => None,
            };
            let host = self.outliner.as_mut().unwrap();
            if input::edit_menu_action(host.egui.egui_input_mut(), &action, true, clipboard) {
                host.repaint();
                return true;
            }
        }
        if matches!(
            action,
            Action::NewTemporaryTmuxxWindow | Action::NewPersistentTmuxxWindow
        ) {
            if let Err(error) = self.start_tmuxx(action == Action::NewTemporaryTmuxxWindow) {
                self.errors.push(error.to_string());
            }
            return true;
        }
        if action == Action::ConnectTmuxx && self.windows.is_empty() {
            self.add_window(false);
            self.reconcile(event_loop);
        }
        if action == Action::ToggleOutliner {
            self.toggle_outliner(event_loop);
            return true;
        }
        if self
            .outliner
            .as_ref()
            .is_some_and(|host| host.focused && host.visible)
            && matches!(
                action,
                Action::CloseSurface | Action::CloseTab | Action::CloseWindow
            )
        {
            self.hide_outliner();
            return true;
        }
        if let Action::StreamDeck(command) = action {
            return self.deck_command(command);
        }
        if action == Action::OpenLayout && self.windows.is_empty() {
            let id = self.add_window(false);
            self.reconcile(event_loop);
            self.active = Some(id);
        }
        if let Some((key, mut host)) = self.take_active_host() {
            let performed = self.binding_action(event_loop, &mut host, action, all);
            self.windows.insert(key, host);
            self.reconcile(event_loop);
            performed
        } else if matches!(action, Action::Undo | Action::Redo) {
            let performed = self.undo_layout(action == Action::Redo);
            if performed {
                self.reconcile(event_loop);
            }
            performed
        } else if action == Action::ToggleQuickTerminal {
            let id = self.add_window(true);
            self.reconcile(event_loop);
            let key = self
                .windows
                .iter()
                .find(|(_, host)| host.id == id)
                .map(|(key, _)| *key);
            if let Some(key) = key
                && let Some(mut host) = self.windows.remove(&key)
            {
                self.quick_visible(&mut host, true, true);
                self.windows.insert(key, host);
            }
            true
        } else if matches!(action, Action::NewWindow | Action::NewTab) {
            self.add_window(false);
            self.reconcile(event_loop);
            true
        } else if action == Action::Quit {
            if self.config().session_dump_on_quit != config::SessionDumpOnQuit::Disabled
                && Platform::option_pressed()
            {
                let mut checked =
                    self.config().session_dump_on_quit == config::SessionDumpOnQuit::Checked;
                let mut error = None;
                loop {
                    match Platform::confirm_quit_dump(checked, error.as_deref()) {
                        Ok(Some(false)) => break,
                        Ok(Some(true)) => match self.dump_session_state(false) {
                            Ok(_) => break,
                            Err(failure) => {
                                checked = true;
                                error = Some(format!("Could not save session state: {failure}"));
                            }
                        },
                        Ok(None) => return true,
                        Err(error) => {
                            self.errors.push(error);
                            return true;
                        }
                    }
                }
            }
            self.save();
            event_loop.exit();
            true
        } else {
            false
        }
    }

    fn keyboard(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
        key: &winit::event::KeyEvent,
    ) -> bool {
        let ui_input = host.ui_input();
        let search_input =
            host.search_focus.is_some() && !host.modal_input() && !host.messages_focused;
        let owner_key = !ui_input
            && self
                .focused(host.id)
                .and_then(|id| self.panes.get(&id))
                .is_some_and(|pane| pane.session.accepts_input())
            && self
                .tab(host.id)
                .and_then(|tab| tab.modal_overlay())
                .is_some_and(|modal| {
                    modal_captures_key(modal, &key.logical_key, host.modifiers.state())
                });
        if owner_key {
            host.sequence.clear();
            host.sequence_len = 0;
        }
        if !owner_key
            && input::key_is_consumed(
                &mut host.consumed_keys,
                key.physical_key,
                key.state,
                ui_input && !search_input,
            )
            && !(search_input && key.repeat)
        {
            return !ui_input;
        }
        if key.state == ElementState::Pressed && !host.composing && !owner_key {
            let candidates = if host.sequence_len == 0 {
                (0..self.config().keybinds.len()).collect::<Vec<_>>()
            } else {
                host.sequence.clone()
            };
            let mut matched = candidates
                .into_iter()
                .filter(|&index| {
                    let binding = &self.config().keybinds[index];
                    binding.table.is_none()
                        && (!search_input || binding.actions.iter().all(input::search_shortcut))
                        && binding
                            .trigger
                            .get(host.sequence_len)
                            .is_some_and(|trigger| {
                                input::matches(trigger, key, host.modifiers.state())
                            })
                })
                .collect::<Vec<_>>();
            // An explicit Option binding wins; otherwise Option augments the configured quit key.
            if matched.is_empty()
                && host.sequence_len == 0
                && quit_option_modifier(host.modifiers.state())
            {
                let without_option = host.modifiers.state() & !winit::keyboard::ModifiersState::ALT;
                matched.extend(self.config().keybinds.iter().enumerate().filter_map(
                    |(index, binding)| {
                        (binding.table.is_none()
                            && binding.actions == [Action::Quit]
                            && binding.trigger.len() == 1
                            && input::matches(&binding.trigger[0], key, without_option))
                        .then_some(index)
                    },
                ));
            }
            let complete = matched
                .iter()
                .rev()
                .copied()
                .find(|&i| self.config().keybinds[i].trigger.len() == host.sequence_len + 1);
            if let Some(index) = complete {
                host.sequence.clear();
                host.sequence_len = 0;
                let binding = self.config().keybinds[index].clone();
                let flags = binding.flags;
                let zoom_toggle = binding.actions.iter().any(|action| {
                    matches!(
                        action,
                        Action::ToggleSplitZoom
                            | Action::ToggleQuadrantZoom
                            | Action::ToggleQuadRoot
                    )
                });
                // Find permits repeats through its UI key tracking, but zoom still fires once.
                if flags.consumed
                    && zoom_toggle
                    && key.repeat
                    && host.consumed_keys.contains(&key.physical_key)
                {
                    return true;
                }
                let pin_navigation = binding
                    .actions
                    .iter()
                    .any(|action| matches!(action, Action::GotoPin(_)));
                let performed = self.host_binding(event_loop, host, binding);
                if flags.consumed && (!flags.performable || performed) {
                    let ui_input = ui_input || host.ui_input();
                    input::consume_shortcut_key(
                        &mut host.consumed_keys,
                        &mut self.consumed_global_keys,
                        key.physical_key,
                        pin_navigation,
                        ui_input || zoom_toggle,
                    );
                    return true;
                }
            } else if !matched.is_empty() {
                host.sequence = matched;
                host.sequence_len += 1;
                return true;
            } else {
                host.sequence.clear();
                host.sequence_len = 0;
            }
        }
        let ui_input = host.ui_input();
        if !owner_key
            && input::key_is_consumed(
                &mut host.consumed_keys,
                key.physical_key,
                key.state,
                ui_input,
            )
        {
            return !ui_input;
        }
        let Some(id) = self.focused(host.id) else {
            return true;
        };
        if self.panes.get(&id).is_some_and(|p| p.exited) && key.state == ElementState::Pressed {
            host.consumed_keys.insert(key.physical_key);
            self.action(event_loop, host, Action::CloseSurface, true);
            return true;
        }
        let options = vt::KeyEncodeOptions {
            macos_option_as_alt: input::option_as_alt(
                self.config().macos_option_as_alt,
                host.modifiers.lalt_state(),
                host.modifiers.ralt_state(),
            ),
        };
        if self.starting_panes.contains_key(&id) {
            if let Some(event) = input::terminal_key(key, host.modifiers, host.composing, options) {
                self.send_input(id, SessionInput::Key { event, options });
            }
            return true;
        }
        if self
            .panes
            .get(&id)
            .is_none_or(|pane| !pane.session.accepts_input())
        {
            return true;
        }
        let bytes = self.panes.get(&id).and_then(|pane| {
            let mut terminal = pane.session.terminal().ok()?;
            let event = input::terminal_key(key, host.modifiers, host.composing, options)?;
            let bytes = input::encode_terminal_key(&mut terminal, &event, options);
            Some((bytes, SessionInput::Key { event, options }))
        });
        if let Some((bytes, input)) = bytes {
            if key.state == ElementState::Pressed && !bytes.is_empty() {
                self.terminal_input(host, id);
            }
            self.send_input(id, input);
        }
        host.repaint();
        // Discard the native translation now, before a later event can open Find.
        true
    }
}

fn modal_captures_key(
    modal: &workspace::Overlay,
    key: &winit::keyboard::Key,
    modifiers: winit::keyboard::ModifiersState,
) -> bool {
    use winit::keyboard::{Key, ModifiersState, NamedKey};
    modal.capture_keys
        || (modal.close_cancel
            && ((key == &Key::Named(NamedKey::Escape) && modifiers.is_empty())
                || (modifiers == ModifiersState::CONTROL
                    && matches!(key, Key::Character(text) if text == "c"))))
}

fn modal_outside_click(
    rect: Option<&egui::Rect>,
    content: Rect,
    mouse: Pos2,
    action: vt::MouseAction,
    button: Option<vt::MouseButton>,
) -> bool {
    action == vt::MouseAction::Press
        && matches!(
            button,
            Some(vt::MouseButton::Left | vt::MouseButton::Middle | vt::MouseButton::Right)
        )
        && content.contains([mouse.x, mouse.y])
        && rect.is_some_and(|rect| !rect.contains(mouse))
}

fn reveal(screen: &mut vt::Screen, point: vt::GridPoint) {
    let index = screen.all_rows().position(|row| row.id == point.row);
    if let Some(index) = index {
        screen.viewport_offset = screen.history_len().saturating_sub(index);
    }
}

fn selection_pointer(
    terminal: &vt::Terminal,
    mouse: Pos2,
    rect: egui::Rect,
    scale: f32,
    padding: [f32; 2],
    cell: [u32; 2],
    rectangle: bool,
) -> vt::selection_gesture::AutoscrollTick<'static> {
    let surface = (mouse - rect.min) * scale;
    let position = surface - Vec2::new(padding[0], padding[1]);
    vt::selection_gesture::AutoscrollTick {
        viewport: [
            (position.x.max(0.0) as u32 / cell[0]).min(u32::from(terminal.cols) - 1),
            (position.y.max(0.0) as u32 / cell[1]).min(u32::from(terminal.rows) - 1),
        ],
        xpos: f64::from(surface.x),
        ypos: f64::from(surface.y),
        rectangle,
        word_boundaries: vt::selection::DEFAULT_WORD_BOUNDARIES,
        geometry: vt::selection_gesture::Geometry {
            columns: u32::from(terminal.cols),
            cell_width: cell[0],
            padding_left: padding[0] as u32,
            screen_height: (rect.height() * scale) as u32,
        },
    }
}

fn pane_minimum_size(cell: [u32; 2], scale: f32, config: &Config) -> [f32; 2] {
    [
        (cell[0] as f32 / scale
            + config.window_padding_x.start
            + config.window_padding_x.end
            + 2.0)
            .ceil(),
        (cell[1] as f32 / scale
            + config.window_padding_y.start
            + config.window_padding_y.end
            + 2.0)
            .ceil(),
    ]
}

fn pane_grid_size(size: Vec2, scale: f32, cell: [u32; 2], config: &Config) -> (u16, u16) {
    let physical = size * scale;
    let padding = [
        config.window_padding_x.start + config.window_padding_x.end,
        config.window_padding_y.start + config.window_padding_y.end,
    ];
    let columns = ((physical.x - padding[0] * scale).max(cell[0] as f32) / cell[0] as f32)
        .floor()
        .clamp(1.0, u16::MAX as f32) as u16;
    let rows = ((physical.y - padding[1] * scale).max(cell[1] as f32) / cell[1] as f32)
        .floor()
        .clamp(1.0, u16::MAX as f32) as u16;
    (columns, rows)
}

impl App {
    fn split_minimum(&self, host: &Host) -> [f32; 2] {
        if self.tmuxx_window(host.id).is_some() {
            return [0.0; 2];
        }
        let metrics = host.fonts.metrics();
        pane_minimum_size(
            [metrics.cell_width, metrics.cell_height],
            host.window.scale_factor() as f32,
            self.window_config(host.id),
        )
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop, host: &mut Host) -> Result<()> {
        self.draw_frame(event_loop, host, None)
    }

    fn draw_frame(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
        offscreen: Option<&wgpu::Texture>,
    ) -> Result<()> {
        host.messages_open = !self.errors.is_empty();
        if !host.visible || host.occluded {
            #[cfg(target_os = "macos")]
            host.progress.clear();
            self.cancel_selection_drag(host);
            return Ok(());
        }
        let Some(index) = self.index(host.id) else {
            return Ok(());
        };
        let config = self.window_config(host.id).clone();
        let state = self.workspace.windows[index].clone();
        if let Some(Confirmation::Paste(paste)) = &host.confirm
            && !self.paste_target_exists(host.id, paste.pane)
        {
            host.confirm = None;
        }
        let active = &state.tabs[state.active_tab];
        let focused = active.focused;
        let tmuxx_tag = self
            .tmuxx_window(host.id)
            .map(|entry| entry.window_tag(host.id, &self.config().tmuxx_connections));
        let breadcrumb = outliner::breadcrumb(active, &self.directory_labels);
        if host
            .selection_drag
            .as_ref()
            .is_some_and(|(id, _)| *id != focused)
        {
            self.cancel_selection_drag(host);
        }
        if host.search_focus.is_some_and(|id| {
            id != focused || self.panes.get(&id).is_none_or(|pane| pane.search.is_none())
        }) {
            host.search_focus = None;
        }
        let scale = host.window.scale_factor() as f32;
        let size = host.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        let cursor_eligible =
            host.cursor.enabled && offscreen.is_none() && !host.capture && !self.background_smoke;
        let native_progress = cfg!(target_os = "macos")
            && offscreen.is_none()
            && !host.capture
            && !self.background_smoke;
        #[cfg(target_os = "macos")]
        if host.cursor.active {
            host.cursor.deadline = None;
            // Synchronized output must freeze the phase that was actually visible.
            if host
                .cursor
                .layer
                .as_ref()
                .is_some_and(|layer| layer.is_visible())
                && let Some(on) = &host.cursor.on
                && let Some(prepared) = host.prepared.get_mut(&on.pane)
                && on.matches(&CursorCandidate {
                    pane: on.pane,
                    key: prepared.key.clone(),
                    frame: Arc::clone(&prepared.frame),
                })
            {
                prepared.key = on.key.clone();
                prepared.frame = Arc::clone(&on.frame);
            }
        }
        let mut raw = host.egui.take_egui_input(&host.window);
        raw.viewport_id = host.viewport;
        if !self.passive_mouse_motion(host, host.mouse)
            && let Some(position) = host.deferred_pointer.take()
        {
            raw.events.push(egui::Event::PointerMoved(position));
        }
        input::filter_egui_events(&mut raw, host.ui_input());
        if host.zoom_animation.is_running()
            && !host.modal_input()
            && host.content.contains([host.mouse.x, host.mouse.y])
        {
            raw.events.retain(|event| {
                !matches!(
                    event,
                    egui::Event::PointerMoved(_)
                        | egui::Event::PointerButton { .. }
                        | egui::Event::MouseWheel { .. }
                        | egui::Event::Touch { .. }
                )
            });
            raw.events.push(egui::Event::PointerGone);
        }
        let context = self.context.clone();
        let now = Instant::now();
        // Popups also use the shared context; restore its theme after this window's pass.
        let previous_theme = context.options(|options| options.theme_preference);
        context.set_theme(ui_theme(&config));
        let platform_accent = self
            .platform
            .as_ref()
            .and_then(Platform::accent_color)
            .unwrap_or_else(|| {
                let [r, g, b, _] = context.global_style().visuals.selection.bg_fill.to_array();
                [r, g, b]
            });
        let panel_accent = TabAccent::new(active.color, platform_accent);
        let tab_accent = panel_accent.for_window(host.focused, config.unfocused_tab_saturation);
        let frame_background =
            tab_accent.frame_background(config.background.to_array(), config.tab_frame_color_mix);
        let frame_color = Color32::from_rgb(
            frame_background[0],
            frame_background[1],
            frame_background[2],
        );
        let accent = Color32::from_rgb(
            tab_accent.background[0],
            tab_accent.background[1],
            tab_accent.background[2],
        );
        self.activity_flashes.remove(&active.id);
        let mut commands = Vec::new();
        let mut palette_event = None;
        let mut search_commands = Vec::new();
        let mut layout_command = None;
        let mut tab_selection = None;
        let mut outline_selection = None;
        let mut presentation_changed = false;
        let mut retry_pane = None;
        let mut render_error = None;
        let (blink_on, blink_deadline) = cursor_blink_phase(host.cursor_blink_started, now);
        let mut needs_blink = false;
        let mut animation_deadline: Option<Instant> = None;
        let animations_enabled = config.window_animations.unwrap_or_else(|| {
            #[cfg(target_os = "macos")]
            {
                !Platform::reduce_motion()
            }
            #[cfg(not(target_os = "macos"))]
            {
                !self.painter.is_software()
            }
        });
        let mut zoom_panes = Vec::new();
        if host
            .navigation_warning
            .is_some_and(|(_, until)| until <= Instant::now())
        {
            host.navigation_warning = None;
        }

        self.sync_window_title(host);
        // The context is shared, but native accessibility activation is per window.
        if host.accesskit_active {
            context.enable_accesskit();
        } else {
            context.disable_accesskit();
        }
        // Escape clears egui focus before drawing; retain this window's keyboard owner.
        let messages_keyboard_input = host.messages_open
            && host.messages_focused
            && !(host.search_focus.is_some() && host.focus_text_input)
            && !host.modal_input();
        let mut output = context.run_ui(raw, |root_ui| {
            host.messages_focused = messages_input(&context) || messages_keyboard_input;
            // The shared context must be inside this window's viewport before clearing focus.
            if std::mem::take(&mut host.unfocus_text_input) {
                root_ui.memory_mut(egui::Memory::stop_text_input);
            }
            let ctx = &context;
            root_ui.visuals_mut().selection.bg_fill = accent;
            root_ui.visuals_mut().selection.stroke.color = Color32::from_rgb(
                tab_accent.foreground[0],
                tab_accent.foreground[1],
                tab_accent.foreground[2],
            );
            root_ui.visuals_mut().hyperlink_color = accent;
            if state.tabs.len() > 1 {
                let mut selected_tab = None;
                let panel = egui::Panel::top("tabs")
                    .exact_size(38.0)
                    .frame(
                        egui::Frame::new()
                            .fill(rgb(config.background))
                            .inner_margin(6.0),
                    )
                    .show(root_ui, |ui| {
                        let header_bounds = ui.max_rect();
                        ui.horizontal(|ui| {
                            ui.add_space(72.0);
                            let tag_width = tmuxx_tag.as_ref().map_or(0.0, |(label, _)| outliner_ui::tmuxx_tag_width(ui, label, ui.available_width() * 0.4));
                            let tag_space = if tmuxx_tag.is_some() { tag_width + ui.spacing().item_spacing.x } else { 0.0 };
                            let available = (ui.available_width() - tag_space).max(1.0);
                            let crumb_width = (available * 0.4)
                                .min(420.0)
                                .min((available - 160.0).max(0.0));
                            outline_selection = outliner_ui::breadcrumb(ui, &breadcrumb, crumb_width);
                            let mut measure = ui.new_child(egui::UiBuilder::new()
                                .id_salt("tab-strip-size")
                                .invisible().sizing_pass()
                                .layout(egui::Layout::left_to_right(egui::Align::Center)));
                            for (index, tab) in state.tabs.iter().enumerate() {
                                let mut atoms = egui::Atoms::new(self.tab_label(tab).0);
                                if tab.zoom.is_some() {
                                    atoms.push_right(egui::Atom::custom(egui::Id::new(("tab-zoom", tab.id)), Vec2::splat(12.0)));
                                }
                                measure.add(egui::Button::selectable(index == state.active_tab, atoms).fill(Color32::TRANSPARENT));
                            }
                            measure.add_sized([24.0, 22.0], egui::Button::new("+"));
                            let mut available = ui.available_rect_before_wrap();
                            available.max.x -= tag_space;
                            let tabs_rect = centered_tab_strip_rect(header_bounds, available, measure.min_rect().width());
                            let tabs_width = (tabs_rect.width() - 24.0 - ui.spacing().item_spacing.x).max(1.0);
                            let focus_id = egui::Id::new(("tab-strip-focus", host.id));
                            let focus_changed =
                                ui.data(|data| data.get_temp::<Id>(focus_id)) != Some(active.id);
                            ui.scope_builder(egui::UiBuilder::new()
                                .id_salt("centered-tabs").max_rect(tabs_rect)
                                .layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                            egui::ScrollArea::horizontal()
                                .id_salt(("tab-strip", host.id))
                                .max_width(tabs_width)
                                .max_height(26.0)
                                .auto_shrink([true, true])
                                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        for (index, tab) in state.tabs.iter().enumerate() {
                                            let (label, is_active) = self.tab_label(tab);
                                            let selected = index == state.active_tab;
                                            let colors = TabAccent::new(tab.color, platform_accent).for_window(host.focused, config.unfocused_tab_saturation);
                                            let color = Color32::from_rgb(
                                                colors.background[0],
                                                colors.background[1],
                                                colors.background[2],
                                            );
                                            let foreground = if selected {
                                                Color32::from_rgb(
                                                    colors.foreground[0],
                                                    colors.foreground[1],
                                                    colors.foreground[2],
                                                )
                                            } else {
                                                ui.visuals().text_color()
                                            };
                                            let mut text = egui::RichText::new(label).color(foreground);
                                            if !selected && is_active {
                                                text = text.underline();
                                            }
                                            let flash =
                                                self.flash_opacity(tab.id, now, &mut animation_deadline)
                                                    * 0.45;
                                            let fill = if selected {
                                                color
                                            } else {
                                                color.gamma_multiply(flash.max(if tab.color.is_some() {
                                                    0.12
                                                } else {
                                                    0.0
                                                }))
                                            };
                                            let icon_id = egui::Id::new(("tab-zoom", tab.id));
                                            let mut atoms = egui::Atoms::new(text);
                                            if tab.zoom.is_some() {
                                                atoms.push_right(egui::Atom::custom(
                                                    icon_id,
                                                    Vec2::splat(12.0),
                                                ));
                                            }
                                            let mut button = egui::Button::selectable(selected, atoms)
                                                .fill(fill);
                                            if selected {
                                                button = button.corner_radius(egui::CornerRadius {
                                                    sw: 0,
                                                    se: 0,
                                                    ..ui.visuals().widgets.inactive.corner_radius
                                                });
                                            }
                                            let button = button.atom_ui(ui);
                                            if let Some(rect) = button.rect(icon_id) {
                                                paint_zoom_icon(ui.painter(), rect, tab, foreground);
                                            }
                                            let response = button.response;
                                            if selected {
                                                selected_tab = Some((response.rect, ui.clip_rect()));
                                            }
                                            if let Some(zoom) = zoom_label(tab) {
                                                response.widget_info(|| {
                                                    egui::WidgetInfo::selected(
                                                        egui::WidgetType::SelectableLabel,
                                                        ui.is_enabled(),
                                                        selected,
                                                        format!("{} · {zoom}", self.tab_name(tab)),
                                                    )
                                                });
                                            }
                                            if selected && focus_changed {
                                                response.scroll_to_me(None);
                                            }
                                            if tab.color.is_some() {
                                                ui.painter().line_segment(
                                                    [
                                                        response.rect.left_top() + Vec2::new(4.0, 1.0),
                                                        response.rect.right_top() + Vec2::new(-4.0, 1.0),
                                                    ],
                                                    egui::Stroke::new(2.0, color),
                                                );
                                            }
                                            if response.clicked() {
                                                tab_selection = Some(index);
                                            }
                                            let window_index = self.index(host.id).unwrap();
                                            let actual =
                                                &mut self.workspace.windows[window_index].tabs[index];
                                            let (changed, close) =
                                                show_tab_context_menu(&response, actual, platform_accent, &config);
                                            presentation_changed |= changed;
                                            if close {
                                                tab_selection = Some(index);
                                                commands.push(Action::CloseTab);
                                            }
                                            if let Some(zoom) = zoom_label(tab) {
                                                response.on_hover_text(zoom);
                                            }
                                        }
                                    });
                                });
                            if ui
                                .add_sized([24.0, 22.0], egui::Button::new("+"))
                                .on_hover_text("New tab")
                                .clicked()
                            {
                                commands.push(Action::NewTab);
                            }
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if let Some((label, tooltip)) = &tmuxx_tag { outliner_ui::tmuxx_tag(ui, label, tooltip, tag_width); }
                            });
                            if !ui.is_sizing_pass() {
                                ui.data_mut(|data| data.insert_temp(focus_id, active.id));
                            }
                        });
                    });
                if let Some((tab, mut clip)) = selected_tab {
                    clip.min.y = panel.response.rect.top();
                    clip.max.y = panel.response.rect.bottom();
                    paint_tab_bleed(root_ui.painter(), tab, clip.intersect(panel.response.rect), accent, frame_color);
                }
            }
            // Cache this viewport's popup state for native events between frames.
            host.popup_open = egui::Popup::is_any_open(ctx);
            if host.ui_input() {
                self.cancel_selection_drag(host);
                if let Some(pane) = self.panes.get_mut(&focused) {
                    pane.reset_selection_gesture();
                }
                host.mouse_button = None;
                host.selection_drag = None;
                host.divider_drag = None;
                host.composing = false;
                host.preedit.clear();
                host.preedit_selection = None;
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(root_ui, |ui| {
                    let content = ui.max_rect();
                    host.content = Rect {
                        x: content.left(),
                        y: content.top(),
                        width: content.width(),
                        height: content.height(),
                    };
                    let cell_height = host.fonts.metrics().cell_height as f32 / scale;
                    let minimum = self.split_minimum(host);
                    let required = active.visible_tree(host.peek.is_some()).minimum_size(minimum);
                    let min_inner_size = LogicalSize::new(
                        MIN_WINDOW_SIZE.width.max(f64::from(required[0] + size.width as f32 / scale - content.width())),
                        MIN_WINDOW_SIZE.height.max(f64::from(required[1] + size.height as f32 / scale - content.height())),
                    );
                    if host.min_inner_size != min_inner_size {
                        host.min_inner_size = min_inner_size;
                        host.window.set_min_inner_size(Some(min_inner_size));
                        let current = size.to_logical::<f64>(f64::from(scale));
                        if current.width < min_inner_size.width || current.height < min_inner_size.height {
                            let _ = host.window.request_inner_size(LogicalSize::new(current.width.max(min_inner_size.width), current.height.max(min_inner_size.height)));
                        }
                    }
                    let layout = active.layout_with_minimum(host.content, host.peek.is_some(), cell_height, minimum);
                    zoom_panes = host.zoom_animation.update(active, host.peek.is_some(), host.content, cell_height, minimum, scale, &config, animations_enabled, now);
                    if host.zoom_animation.is_running() {
                        ui.ctx().request_repaint();
                        self.cancel_selection_drag(host);
                        host.divider_drag = None;
                        host.hovered_link = None;
                    }
                    host.pane_order = layout.iter().map(|(id, _)| *id).collect();
                    host.modal_pane = active.modal_pane();
                    host.overlay_panes = active.overlays.iter().map(|overlay| overlay.pane).collect();
                    let [r, g, b] = frame_background;
                    let gutter = Color32::from_rgba_unmultiplied(
                        r,
                        g,
                        b,
                        (config.background_opacity * 255.0).round() as u8,
                    );
                    if host.zoom_animation.is_running() {
                        let mut uncovered = vec![content];
                        for pane in &zoom_panes {
                            uncovered = uncovered.into_iter().flat_map(|rect| uncovered_rects(rect, pane.rect.expand(1.0))).collect();
                        }
                        for rect in uncovered {
                            ui.painter().rect_filled(rect, 0.0, gutter);
                        }
                    }
                    for pane in &zoom_panes {
                        let outer = pane.rect.expand(1.0);
                        paint_pane_gutter(ui.painter(), outer, outer.shrink(1.0), gutter);
                    }
                    host.rects = layout
                        .iter()
                        .map(|(id, r)| {
                            (
                                *id,
                                egui::Rect::from_min_size(
                                    Pos2::new(r.x, r.y),
                                    Vec2::new(r.width, r.height),
                                )
                                .shrink(1.0),
                            )
                        })
                        .collect();
                    host.prepared.retain(|id, _| zoom_panes.iter().any(|pane| pane.id == *id));
                    let mut composed = None;
                    let mut accessible = Vec::new();
                    if let Some(entry) = self.tmuxx_window(host.id)
                        && let Some(connection) = entry.view_connection()
                        && let Some(session) = entry.mapping.sessions.get(&host.id)
                    {
                        let metrics = host.fonts.metrics();
                        let (cols, rows) = self.tab(host.id).map(|tab| rustty_app::tmuxx_workspace::grid_size(
                            tab, host.content, host.peek.is_some(),
                            [metrics.cell_width as f32 / scale, metrics.cell_height as f32 / scale],
                            [config.window_padding_x.start + config.window_padding_x.end + 2.0,
                             config.window_padding_y.start + config.window_padding_y.end + 2.0],
                        )).unwrap_or((80, 24));
                        if let Err(error) = connection.view_size(*session, cols, rows, metrics.cell_width.min(u16::MAX.into()) as u16, metrics.cell_height.min(u16::MAX.into()) as u16) { render_error = Some(error.to_string()); }
                    }
                    let mut terminal_ime_rect = None;
                    // An atlas eviction can happen halfway through a multi-pane frame.
                    // Rebuild against its new generation before handing a frame to WGPU.
                    for attempt in 0..2 {
                        let mut composition = Vec::with_capacity(host.rects.len());
                        accessible.clear();
                        for visual in &zoom_panes {
                            let (id, rect) = (visual.id, visual.layout);
                            if !active.contains(id) {
                                let Some(prepared) = host.prepared.get_mut(&id) else { continue; };
                                let dim = prepared.dim;
                                if prepared.frame.generation != host.fonts.generation() {
                                    match host.fonts.prepare(&prepared.screen, &prepared.key.options) {
                                        Ok(frame) => {
                                            prepared.frame = Arc::new(frame);
                                            host.pane_prepares += 1;
                                        }
                                        Err(error) => {
                                            render_error = Some(error.to_string());
                                            continue;
                                        }
                                    }
                                }
                                composition.push(prepared.compose(visual, content, scale, &config, dim, true));
                                continue;
                            }
                            let Some(pane) = self.panes.get_mut(&id) else {
                                continue;
                            };
                            let physical = rect.size() * scale;
                            let padding = [
                                config.window_padding_x.start * scale,
                                config.window_padding_y.start * scale,
                            ];
                            let metrics = host.fonts.metrics();
                            let (cols, rows) = pane_grid_size(
                                rect.size(), scale, [metrics.cell_width, metrics.cell_height],
                                &config,
                            );
                            // Image placement and pixel mouse input share the renderer's cell grid.
                            if let Err(error) = pane.session.resize(
                                cols,
                                rows,
                                u32::from(cols)
                                    .saturating_mul(metrics.cell_width)
                                    .min(u16::MAX as u32) as u16,
                                u32::from(rows)
                                    .saturating_mul(metrics.cell_height)
                                    .min(u16::MAX as u32) as u16,
                            ) {
                                render_error = Some(error.to_string());
                                continue;
                            }
                            let mut terminal = match pane.session.terminal() {
                                Ok(terminal) => terminal,
                                Err(error) => {
                                    render_error = Some(error.to_string());
                                    continue;
                                }
                            };
                            if terminal.modes.dec(2026)
                                && host.prepared.get(&id).is_some_and(|prepared| {
                                    prepared.frame.generation != host.fonts.generation()
                                })
                            {
                                // Like resize, atlas replacement ends a batch: old
                                // texture coordinates cannot mix with the new atlas.
                                terminal.set_mode(true, 2026, false);
                            }
                            let synchronized =
                                pane.sync_output.update(&mut terminal, now).is_some();
                            if !synchronized && let Some(search) = &mut pane.search {
                                search.refresh(&mut terminal);
                            }
                            if !synchronized
                                && let Some((owner, drag)) = &mut host.selection_drag
                                && *owner == id
                            {
                                let pointer = selection_pointer(
                                    &terminal,
                                    host.mouse,
                                    rect,
                                    scale,
                                    padding,
                                    [metrics.cell_width, metrics.cell_height],
                                    host.modifiers.state().alt_key(),
                                );
                                if !drag.refresh(
                                    &mut terminal,
                                    &mut pane.selection_gesture,
                                    host.mouse,
                                    pointer,
                                    now,
                                ) {
                                    host.selection_drag = None;
                                }
                            }
                            let now_ms =
                                self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                            if !synchronized && let Some(next) = terminal.tick_graphics(now_ms) {
                                let deadline = Instant::now()
                                    + Duration::from_millis(next.saturating_sub(now_ms));
                                animation_deadline = Some(
                                    animation_deadline.map_or(deadline, |old| old.min(deadline)),
                                );
                            }
                            let accepts_input = !pane.exited && pane.session.accepts_input();
                            let is_focused = host.focused && id == focused && host.peek.is_none() && accepts_input;
                            needs_blink |= !synchronized
                                && is_focused
                                && !pane.exited
                                && !host.composing
                                && terminal.screen().cursor.visible
                                && terminal
                                    .screen()
                                    .cursor
                                    .row
                                    .saturating_add(terminal.screen().viewport_offset)
                                    < terminal.rows as usize
                                && terminal.screen().cursor.blink;
                            let (foreground, background) = if terminal.modes.dec(5) {
                                (terminal.background, terminal.foreground)
                            } else {
                                (terminal.foreground, terminal.background)
                            };
                            let cursor_color = terminal.cursor_color.unwrap_or(foreground);
                            let resolve = |color: config::TerminalColor| match color {
                                config::TerminalColor::Rgb(color) => [color.r, color.g, color.b],
                                config::TerminalColor::CellForeground => foreground,
                                config::TerminalColor::CellBackground => background,
                            };
                            let options = RenderOptions {
                                size: [physical.x.max(1.0) as u32, physical.y.max(1.0) as u32],
                                padding,
                                foreground,
                                background,
                                cursor_color,
                                cursor_text: config.cursor_text.map(resolve).unwrap_or(background),
                                selection_background: config
                                    .selection_background
                                    .map(resolve)
                                    .unwrap_or([65, 85, 120]),
                                selection_foreground: config.selection_foreground.map(resolve),
                                search_highlights: pane
                                    .search
                                    .as_mut()
                                    .map(|search| search.highlights(&mut terminal))
                                    .unwrap_or_default(),
                                palette: terminal
                                    .palette
                                    .as_slice()
                                    .try_into()
                                    .unwrap_or([[0; 3]; 256]),
                                focused: is_focused,
                                cursor_visible: accepts_input && (id != focused || !host.composing),
                                blink_visible: blink_on || !is_focused,
                                background_opacity: config.background_opacity,
                                preedit: (is_focused && !host.preedit.is_empty()).then(|| {
                                    rustty_render::Preedit {
                                        text: host.preedit.clone(),
                                        selection: host.preedit_selection,
                                    }
                                }),
                            };
                            let key = PaneRenderKey::new(&terminal, options, rect, scale);
                            let snapshot = (!synchronized
                                && !host
                                    .prepared
                                    .get_mut(&id)
                                    .is_some_and(|pane| pane.matches(&key, &host.fonts)))
                            .then(|| pane.session.display_screen(&terminal));
                            drop(terminal);
                            if let Some(snapshot) = snapshot {
                                let snapshot = match snapshot {
                                    Ok(snapshot) => snapshot,
                                    Err(error) => {
                                        render_error = Some(error.to_string());
                                        continue;
                                    }
                                };
                                match PreparedPane::new(key, snapshot, &mut host.fonts) {
                                    Ok(prepared) => {
                                        host.prepared.insert(id, prepared);
                                        host.pane_prepares += 1;
                                    }
                                    Err(error) => {
                                        host.prepared.remove(&id);
                                        render_error = Some(error.to_string());
                                        continue;
                                    }
                                }
                            }
                            let Some(prepared) = host.prepared.get_mut(&id) else {
                                continue;
                            };
                            needs_blink |= !synchronized
                                && is_focused
                                && !pane.exited
                                && prepared.frame.blinking_text;
                            composition.push(prepared.compose(
                                visual, content, scale, &config,
                                (id != focused && config.unfocused_split_opacity < 1.0).then(
                                    || {
                                        let color = config
                                            .unfocused_split_fill
                                            .unwrap_or(config.background);
                                        rustty_render::Color::rgb([color.r, color.g, color.b])
                                            .opacity(1.0 - config.unfocused_split_opacity)
                                    },
                                ),
                                host.zoom_animation.is_running(),
                            ));
                            accessible.push((id, visual.rect.intersect(content)));
                            if id == focused && accepts_input {
                                terminal_ime_rect = Some(prepared.ime_rect.translate(visual.rect.min - rect.min));
                            }
                        }
                        host.fonts.retain_images(
                            host.prepared
                                .values()
                                .flat_map(|pane| pane.screen.graphics.images.values()),
                        );
                        let size = [size.width, size.height];
                        if let Some(retained) = &host.composed
                            && retained.matches(size, &composition)
                        {
                            composed = Some(Arc::clone(&retained.frame));
                            break;
                        }
                        match ComposedFrame::new(size, composition) {
                            Ok(retained) => {
                                composed = Some(Arc::clone(&retained.frame));
                                host.composed = Some(retained);
                                break;
                            }
                            Err(_) if attempt == 0 => {}
                            Err(_) => {
                                render_error =
                                    Some("Visible glyphs exceed the shared atlas budget".into());
                                host.composed = None;
                            }
                        }
                    }
                    ui.painter().add(
                        self.painter.terminal_callback(
                            egui::Rect::from_min_size(
                                Pos2::ZERO,
                                Vec2::new(size.width as f32 / scale, size.height as f32 / scale),
                            ),
                            host.id,
                            composed.unwrap_or_else(|| {
                                Arc::new(Frame::empty([size.width, size.height]))
                            }),
                        ),
                    );
                    let displayed = host.zoom_animation.is_running().then(|| {
                        let mut displayed = active.clone();
                        if let Some((zoom, quadrant, _)) = host.zoom_animation.display_zoom() {
                            displayed.zoom = zoom;
                            displayed.quadrant_zoom = quadrant;
                        }
                        displayed
                    });
                    let active = displayed.as_ref().unwrap_or(active);
                    let peek = if host.zoom_animation.display_zoom().is_some_and(|mode| mode.2) {
                        host.peek.or(host.presented_peek)
                    } else {
                        None
                    };
                    host.presented_peek = host.peek.or(peek);
                    for (id, rect) in accessible {
                        let response = ui.interact(
                            rect,
                            egui::Id::new(("terminal", id)),
                            Sense::click_and_drag(),
                        );
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, "Terminal")
                        });
                        ctx.accesskit_node_builder(response.id, |node| {
                            node.set_role(egui::accesskit::Role::Terminal);
                            node.set_label(
                                self.panes
                                    .get(&id)
                                    .map(|p| p.title.as_str())
                                    .unwrap_or("Terminal"),
                            );
                            node.add_action(egui::accesskit::Action::Focus);
                            node.add_action(egui::accesskit::Action::ScrollUp);
                            node.add_action(egui::accesskit::Action::ScrollDown);
                        });
                        // The character picker can commit while the terminal window is unfocused.
                        if id == focused {
                            input::terminal_input(&response, terminal_ime_rect, host.ui_input());
                        }
                        zoom_chrome(ui, &zoom_panes, id, rect, rgb(config.background), |ui| {
                            if host.navigation_warning.is_some_and(|(pane, _)| pane == id) {
                                ui.painter().rect_stroke(
                                    rect.shrink(2.0),
                                    0.0,
                                    egui::Stroke::new(3.0, Color32::from_rgb(230, 125, 65)),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            if active.pane_ids().len() > 1 {
                                let selected = peek
                                    .map(|peek| peek.target == id)
                                    .unwrap_or(id == focused);
                                let color = if selected {
                                    accent
                                } else {
                                    config
                                        .split_divider_color
                                        .map(rgb)
                                        .unwrap_or(Color32::from_gray(65))
                                };
                                paint_pane_frame(ui.painter(), rect, color);
                            }
                            if config.progress_style
                                && let Some(pane) = self.panes.get_mut(&id)
                                && let Some(progress) = pane.activity.progress()
                            {
                                let offset = pane.activity.progress_offset(now);
                                let native_started = (native_progress && zoom_panes.iter().all(|pane| {
                                    !pane.moving && pane.opacity == 1.0 && pane.background_opacity == 1.0
                                })).then(|| pane.activity.progress_started()).flatten();
                                if !paint_progress(
                                    ui,
                                    id,
                                    rect,
                                    progress,
                                    Color32::from_rgb(panel_accent.background[0], panel_accent.background[1], panel_accent.background[2]),
                                    offset,
                                    native_started,
                                ) {
                                    pane.activity.reset_progress_animation();
                                }
                            }
                        });
                    }
                    for visual in &zoom_panes {
                        let (id, rect) = (visual.id, visual.rect);
                        if !active.contains(id) { continue; }
                        zoom_chrome(ui, &zoom_panes, id, rect, rgb(config.background), |ui| {
                            if let Some(error) = self.failed_panes.get(&id) {
                                ui.scope_builder(
                                    egui::UiBuilder::new().max_rect(rect.shrink(20.0)),
                                    |ui| {
                                        ui.label(format!("Could not start terminal: {error}"));
                                        if ui.button("Retry").clicked() {
                                            retry_pane = Some(id);
                                        }
                                    },
                                );
                            } else if let Some(message) = self
                                .panes
                                .get(&id)
                                .and_then(|pane| pane.exit_message.as_ref())
                            {
                                let galley = ui.painter().layout_no_wrap(
                                    message.clone(),
                                    egui::FontId::proportional(13.0),
                                    Color32::WHITE,
                                );
                                let bounds = egui::Rect::from_center_size(
                                    rect.center_bottom() - Vec2::new(0.0, 20.0),
                                    galley.size(),
                                );
                                ui.painter().rect_filled(
                                    bounds.expand(5.0),
                                    3.0,
                                    Color32::from_black_alpha(230),
                                );
                                ui.painter().galley(bounds.min, galley, Color32::WHITE);
                            }
                        });
                    }
                    if let Some(zoom) = zoom_label(active)
                        && peek.is_none()
                        && active.panes.len() > 1
                        && state.tabs.len() == 1
                    {
                        let bounds = egui::Rect::from_center_size(
                            Pos2::new(content.center().x, content.top() + 10.0),
                            Vec2::splat(12.0),
                        );
                        zoom_chrome(ui, &zoom_panes, focused, content, rgb(config.background), |ui| {
                                paint_zoom_icon(ui.painter(), bounds, active, rgb(config.foreground));
                        });
                        ui.interact(bounds, egui::Id::new(("window-zoom", host.id)), Sense::hover())
                            .on_hover_text(zoom);
                    }
                    if !host.zoom_animation.is_running() {
                        self.update_hover_link(host);
                    }
                    if let Some(link) = &host.hovered_link
                        && let Some(prepared) = host.prepared.get(&link.pane)
                    {
                        let metrics = host.fonts.metrics();
                        let thickness = metrics.underline_thickness.ceil();
                        let offset = (metrics.baseline + metrics.underline_position)
                            .min(metrics.cell_height as f32 - thickness)
                            / scale;
                        let [r, g, b] = prepared.key.options.foreground;
                        let painter = ui.painter().with_clip_rect(host.rects[&link.pane]);
                        for bounds in &link.bounds {
                            painter.hline(
                                bounds.left()..=bounds.right(),
                                bounds.top() + offset,
                                egui::Stroke::new(thickness / scale, Color32::from_rgb(r, g, b)),
                            );
                        }
                    }
                    let mut quadrants = BTreeMap::<Id, egui::Rect>::new();
                    for visual in &zoom_panes {
                        let (pane, rect) = (visual.id, visual.rect);
                        if let Some(quadrant) = active.label_quad(pane, peek.is_some()) {
                            quadrants
                                .entry(quadrant)
                                .and_modify(|bounds| *bounds = bounds.union(rect))
                                .or_insert(rect);
                        }
                    }
                    let mut shared_labels = HashSet::new();
                    for (id, bounds) in quadrants {
                        let members = active.root.node(id).unwrap().panes();
                        let retained = host.zoom_animation.is_running()
                            && members.iter().all(|id| zoom_panes.iter().any(|pane| pane.id == *id && !pane.moving));
                        let attention = members
                            .iter()
                            .any(|id| self.panes.get(id).is_some_and(|pane| pane.unseen));
                        let representative = members.iter().find(|id| zoom_panes.iter().any(|pane| pane.id == **id)).copied().unwrap_or(id);
                        zoom_chrome(ui, &zoom_panes, representative, bounds, rgb(config.background), |ui| {
                            if let Some(peek) = peek {
                                let selected = active.root.quadrant(peek.target);
                                if !peek.navigation_used && Some(id) != selected {
                                    ui.painter().rect_filled(
                                        bounds,
                                        0.0,
                                        if attention {
                                            accent
                                        } else {
                                            config
                                                .unfocused_split_fill
                                                .map(rgb)
                                                .unwrap_or(Color32::BLACK)
                                        }
                                        .gamma_multiply(config.quadrant_peek_opacity),
                                    );
                                }
                                if Some(id) == selected {
                                    paint_pane_frame(ui.painter(), bounds, accent);
                                }
                            }
                            if let Some(label) = self.quadrant_label(active, host, id, peek, retained) {
                                let visible = !label.focused || peek.is_some();
                                shared_labels.extend(members.iter().copied().filter(|pane| label.suppresses_panel_name(*pane == focused, peek.is_some())));
                                if !visible { return; }
                                DirectoryBadge {
                                    label,
                                    bounds,
                                    centered: true,
                                    shared: true,
                                    pinned: false,
                                    attention,
                                    accent: panel_accent,
                                    active: members.iter().any(|id| {
                                        self.panes
                                            .get(id)
                                            .is_some_and(|pane| pane.activity.is_active())
                                    }),
                                    flash: self.flash_opacity(id, now, &mut animation_deadline),
                                }
                                .paint(ui);
                            }
                        });
                    }
                    for visual in &zoom_panes {
                        let (id, bounds) = (visual.id, visual.rect);
                        if !active.contains(id) { continue; }
                        let Some(pane) = self.panes.get(&id) else {
                            continue;
                        };
                        zoom_chrome(ui, &zoom_panes, id, bounds, rgb(config.background), |ui| {
                            let pinned = self.workspace.pinned_panes.contains(&id);
                            let selected = peek.map_or(focused, |peek| peek.target) == id;
                            let server_selected = id != focused && self.tmuxx_window(host.id)
                                .is_some_and(|entry| entry.mapping.server_focused.contains(&id));
                            if let Some(label) = DirectoryLabel::new(
                                self.directory_labels.get(&id).cloned(),
                                selected,
                                host.focused,
                                peek.is_some() || host.focus_hint.visible(id, now),
                            ).filter(|_| !shared_labels.contains(&id)) {
                                let centered = label.centered_in_pane(active, id, visual.label_quadrant);
                                let flash = if label.shows_flash(true, false) {
                                    self.flash_opacity(id, now, &mut animation_deadline)
                                } else {
                                    0.0
                                };
                                DirectoryBadge {
                                    label,
                                    bounds,
                                    centered,
                                    shared: active.label_quad(id, peek.is_some()) == Some(id),
                                    pinned,
                                    flash,
                                    accent: panel_accent,
                                    active: pane.activity.is_active(),
                                    attention: pane.unseen,
                                }
                                .paint(ui);
                            } else if pinned {
                                paint_panel_pin(ui, id, bounds);
                            }
                            if server_selected {
                                paint_server_selection(ui, id, bounds, accent);
                            }
                        });
                    }
                });
            #[cfg(target_os = "macos")]
            let mut controls_right = root_ui.max_rect().right() - 8.0;
            if state.tabs.len() == 1 && let Some((label, tooltip)) = &tmuxx_tag {
                let bounds = root_ui.max_rect();
                let width = outliner_ui::tmuxx_tag_width(root_ui, label, (bounds.width() - 88.0).max(0.0));
                let rect = egui::Rect::from_min_size(
                    Pos2::new(bounds.right() - width - 8.0, bounds.top() + 6.0),
                    Vec2::new(width, 18.0),
                );
                #[cfg(target_os = "macos")]
                { controls_right = rect.left() - 6.0; }
                let mut tag_ui = root_ui.new_child(egui::UiBuilder::new().id_salt("tmuxx-window-tag").max_rect(rect));
                tag_ui.painter().rect_filled(rect.expand(3.0), 4, rgb(config.background));
                outliner_ui::tmuxx_tag(&mut tag_ui, label, tooltip, width);
            }
            #[cfg(target_os = "macos")]
            if state.tabs.len() == 1 {
                let buttons = Platform::window_controls_rect(&host.window).map(|[x, y, width, height]| {
                    egui::Rect::from_min_size(Pos2::new(x, y), Vec2::new(width, height))
                });
                if buttons.is_some() || host.window.fullscreen().is_some() {
                    outline_selection = paint_window_controls_background(
                        root_ui,
                        buttons,
                        &breadcrumb,
                        (controls_right - buttons.map_or(10.0, |buttons| buttons.right() + 12.0) - 12.0).clamp(0.0, 420.0),
                        rgb(config.background),
                        rgb(config.foreground),
                    );
                }
            }
            host.search_rects.clear();
            let modal_input = host.modal_input();
            let requested_search = host.search_focus.filter(|_| host.focus_text_input);
            let mut search_focus = None;
            root_ui.scope(|ui| {
                if modal_input {
                    ui.disable();
                } else if let Some(id) = requested_search {
                    // Set the keyboard target before drawing any pane's editor. Otherwise
                    // an earlier overlay can consume text queued for the newly opened one.
                    let id = search::field_id(id);
                    ui.memory_mut(|memory| {
                        if !memory.has_focus(id) {
                            memory.request_focus(id);
                        }
                    });
                }
                for visual in &zoom_panes {
                    let (id, bounds) = (visual.id, visual.rect);
                    if !active.contains(id) { continue; }
                    let Some(pane) = self.panes.get_mut(&id) else {
                        continue;
                    };
                    let Some(search) = &mut pane.search else {
                        continue;
                    };
                    let mut focus =
                        !modal_input && host.search_focus == Some(id) && host.focus_text_input;
                    let requested = focus;
                    let response = zoom_chrome(ui, &zoom_panes, id, bounds, rgb(config.background), |ui| search.show(
                        ui, id, bounds,
                        id == focused && !messages_keyboard_input && !messages_input(ctx),
                        &mut focus, &config,
                    ));
                    if response.response.layer_id != ui.layer_id() {
                        clip_zoom_chrome(ctx, response.response.layer_id, egui::layers::ShapeIdx(0), &zoom_panes, id, rgb(config.background));
                    }
                    if requested && !focus {
                        host.focus_text_input = false;
                    }
                    if response.focused || focus {
                        search_focus = Some(id);
                    }
                    host.search_rects
                        .insert(id, response.response.rect.intersect(bounds));
                    if response.changed {
                        if let Ok(mut terminal) = pane.session.terminal() {
                            search.refresh(&mut terminal);
                        }
                        host.repaint();
                    }
                    if let Some(action) = response.action {
                        search_commands.push((id, action));
                    }
                }
            });
            if !modal_input {
                host.search_focus = requested_search.or(search_focus);
                if let Some(id) = host.search_focus
                    && id != focused
                {
                    self.focus_pane(host.id, id);
                    host.repaint();
                }
            }
            if host.palette.open {
                palette_event = host
                    .palette
                    .show(
                        root_ui,
                        egui::Id::new(("palette", host.id)),
                        &config,
                        &mut host.focus_text_input,
                    )
                    .or(palette_event.take());
                if !host.palette.open {
                    host.focus_text_input = host.search_focus.is_some();
                }
            }
            if let Some(mut target) = host.tmuxx_connect.clone() {
                let mut connect = false;
                let mut cancel = false;
                egui::Window::new("Connect to tmuxx")
                    .collapsible(false).resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(format!("{} · ssh:HOST · system-ssh:HOST", tmuxx_host::LOCAL_TARGET_HINT));
                        ui.text_edit_singleline(&mut target);
                        ui.label("SSH uses your SSH configuration; it never reconnects automatically.");
                        ui.horizontal(|ui| {
                            cancel = ui.button("Cancel").clicked();
                            connect = ui.button("Connect").clicked();
                        });
                    });
                host.tmuxx_connect = if cancel { None } else { Some(target.clone()) };
                if connect {
                    match self.connect_tmuxx(&target) {
                        Ok(()) => host.tmuxx_connect = None,
                        Err(error) => self.errors.push(error.to_string()),
                    }
                }
            }
            let mut detached = false;
            let mut confirmed = None;
            if let Some(confirmation) = &mut host.confirm {
                let (confirmation, save_dump, keep_across_reboots, error) = match confirmation {
                    Confirmation::Quit { confirmation, save_dump, keep_across_reboots, error } => {
                        (&**confirmation, Some(save_dump), Some(keep_across_reboots), error.as_deref())
                    }
                    confirmation => {
                        let error = match &*confirmation {
                            Confirmation::DumpSessions { error } => error.as_deref(),
                            _ => None,
                        };
                        (&*confirmation, None, None, error)
                    }
                };
                let (title, button) = match confirmation {
                    Confirmation::DumpSessions { .. } => ("Dump all sessions?", "Dump Sessions"),
                    Confirmation::Close(_) if save_dump.is_some() => ("Save session state before quitting?", "Quit"),
                    Confirmation::Close(_) => ("Close running commands?", "Close"),
                    Confirmation::TmuxxClose { .. } => ("Close tmuxx window?", "Terminate session"),
                    Confirmation::TmuxxTerminate(tmuxx_host::MenuCommand::Shutdown(_) | tmuxx_host::MenuCommand::ShutdownIdle(_)) => ("Stop tmuxx server?", "Stop Server"),
                    Confirmation::TmuxxTerminate(_) => ("Terminate tmuxx programs?", "Terminate"),
                    Confirmation::Paste(_) => ("Paste potentially unsafe text?", "Paste"),
                    Confirmation::OpenUrl { .. } => {
                        ("Open link from terminal output?", "Open Link")
                    }
                    Confirmation::Quit { .. } => unreachable!("nested quit confirmation"),
                };
                egui::Window::new(title)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        match confirmation {
                            Confirmation::DumpSessions { .. } => {
                                ui.set_max_width(440.0);
                                ui.label("Save screen contents, scrollback, and working directories from all local sessions in this instance? tmuxx sessions are excluded.");
                                ui.label("This may include personally identifiable information (PII), secrets, and other sensitive data.");
                                ui.label("The dump remains on disk until another Rustty instance successfully imports it. Opening Rustty without importing does not remove it.");
                            }
                            Confirmation::TmuxxClose { .. } => {
                                if self.tmuxx_window(host.id).is_some_and(|entry| entry.temporary()) {
                                    ui.label("This temporary session ends when its window closes.");
                                } else {
                                    ui.label("Close the window to keep programs running, or terminate the session.");
                                }
                            }
                            Confirmation::TmuxxTerminate(command @ (tmuxx_host::MenuCommand::Shutdown(target) | tmuxx_host::MenuCommand::ShutdownIdle(target))) => {
                                ui.label(tmuxx_host::shutdown_confirmation(
                                    &self.tmuxx,
                                    target,
                                    matches!(command, tmuxx_host::MenuCommand::ShutdownIdle(_)),
                                ));
                            }
                            Confirmation::TmuxxTerminate(_) => {
                                ui.label("This stops the selected server programs, including work in other clients.");
                            }
                            Confirmation::Close(_) if save_dump.is_some() => {
                                ui.label("Quitting closes local terminals. Persistent tmuxx sessions keep running.");
                            }
                            Confirmation::Close(_) => {
                                ui.label("Closing this terminal will stop its running commands.");
                            }
                            Confirmation::Paste(paste) => {
                                if let Some(pane) = self.panes.get(&paste.pane) {
                                    ui.label(format!("Terminal: {}", pane.title));
                                }
                                ui.label(
                                    "This paste may run commands. Review the text before continuing.",
                                );
                                egui::ScrollArea::vertical()
                                    .max_height(160.0)
                                    .show(ui, |ui| {
                                        let shown = paste.data.len().min(4096);
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(String::from_utf8_lossy(
                                                    &paste.data[..shown],
                                                ))
                                                .monospace(),
                                            )
                                            .wrap(),
                                        );
                                        if shown < paste.data.len() {
                                            ui.label("… preview truncated");
                                        }
                                    });
                            }
                            Confirmation::OpenUrl { uri, handler, .. } => {
                                ui.label(format!("This link will open in {handler}."));
                                ui.label(
                                    "Only continue if you recognize and trust the destination.",
                                );
                                egui::ScrollArea::vertical()
                                    .max_height(160.0)
                                    .show(ui, |ui| {
                                        ui.add(
                                            egui::Label::new(egui::RichText::new(uri).monospace())
                                                .wrap(),
                                        );
                                    });
                            }
                            Confirmation::Quit { .. } => unreachable!("nested quit confirmation"),
                        }
                        if let (Some(save_dump), Some(keep_across_reboots)) = (save_dump, keep_across_reboots) {
                            ui.checkbox(save_dump, "Save session state for a future Rustty instance");
                            ui.add_enabled_ui(*save_dump, |ui| {
                                ui.checkbox(keep_across_reboots, "Keep session state across reboots");
                            });
                        }
                        if let Some(error) = error {
                            ui.colored_label(ui.visuals().error_fg_color, error);
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked()
                                || ui.input(|input| input.key_pressed(egui::Key::Escape))
                            {
                                confirmed = Some(false);
                            }
                            if matches!(confirmation, Confirmation::TmuxxClose { .. })
                                && self.tmuxx_window(host.id).is_some_and(|entry| !entry.temporary())
                                && ui.button("Close window · keep session").clicked()
                            { detached = true; }
                            if ui.button(button).clicked() {
                                confirmed = Some(true);
                            }
                        });
                    });
            }
            if detached { confirmed = Some(true); }
            if let Some(approved) = confirmed
                && let Some(mut confirmation) = host.confirm.take()
                && approved
            {
                if let Confirmation::Quit { save_dump, keep_across_reboots, error, .. } = &mut confirmation {
                    if *save_dump && let Err(failure) = self.dump_session_state(*keep_across_reboots) {
                        *error = Some(format!("Could not save session state: {failure}"));
                        host.confirm = Some(confirmation);
                        host.repaint();
                        return;
                    }
                    let Confirmation::Quit { confirmation: close, .. } = confirmation else { unreachable!() };
                    confirmation = *close;
                }
                if detached {
                    self.close_tmuxx_window(host.id);
                    return;
                }
                match confirmation {
                    Confirmation::DumpSessions { .. } => {
                        if let Err(error) = self.dump_session_state(false) {
                            host.confirm = Some(Confirmation::DumpSessions {
                                error: Some(format!("Could not save session state: {error}")),
                            });
                            host.repaint();
                        }
                    }
                    Confirmation::TmuxxClose { target, session } => self.tmuxx_menu_command(
                        tmuxx_host::MenuCommand::Kill(target, session)
                    ),
                    Confirmation::TmuxxTerminate(command) => self.tmuxx_menu_command(command),
                    Confirmation::Close(action) => {
                        self.action(event_loop, host, action, true);
                    }
                    Confirmation::Paste(paste) => self.paste(host, paste, true),
                    Confirmation::OpenUrl { uri, cwd, .. } => self.open_url(host, uri, cwd, true),
                    Confirmation::Quit { .. } => unreachable!("nested quit confirmation"),
                }
            }
            if let Some(pending) = host.clipboard_request.front_mut() {
                let pane = pending.pane;
                let request = &pending.request;
                let write = matches!(request, vt::Effect::ClipboardWrite(_));
                let (name, can_remember) = match request {
                    vt::Effect::ClipboardRead(read) => (&read.name, read.can_remember),
                    vt::Effect::ClipboardWrite(write) => (&write.name, write.can_remember),
                    _ => unreachable!(),
                };
                let name = String::from_utf8_lossy(name).into_owned();
                let title = self
                    .panes
                    .get(&pane)
                    .map(|pane| pane.title.clone())
                    .unwrap_or_default();
                let mut decision = None;
                egui::Window::new("Terminal clipboard access")
                    .collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        if !title.is_empty() {
                            ui.label(&title);
                        }
                        if !name.is_empty() {
                            ui.label(format!("Program: {name}"));
                        }
                        ui.label(if write {
                            "A terminal program wants to replace the clipboard."
                        } else {
                            "A terminal program wants to read the clipboard."
                        });
                        match request {
                            vt::Effect::ClipboardWrite(write) => {
                                if write.contents.is_empty() {
                                    ui.label("The clipboard will be cleared.");
                                } else {
                                    pending.preview.show(ui, &write.contents);
                                }
                            }
                            vt::Effect::ClipboardRead(_) => match pending.read_result.as_ref() {
                                Some(vt::clipboard::ReadResult::Success(success)) => {
                                    pending.preview.show(ui, &success.contents);
                                    for mime in &success.available {
                                        ui.label(String::from_utf8_lossy(mime));
                                    }
                                }
                                _ => { ui.label("The clipboard could not be read."); }
                            },
                            _ => unreachable!(),
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Deny").clicked()
                                || ui.input(|input| input.key_pressed(egui::Key::Escape))
                            {
                                decision = Some((false, false));
                            }
                            if ui.button("Allow once").clicked() {
                                decision = Some((true, false));
                            }
                            if can_remember
                                && ui.button("Allow for this session").clicked()
                            {
                                decision = Some((true, true));
                            }
                        });
                    });
                if let Some((allow, remember)) = decision
                    && let Some(pending) = host.clipboard_request.pop_front()
                {
                    self.finish_clipboard(pending.pane, pending.request, allow, remember, pending.read_result);
                }
            }
            if let Some(picker) = &mut host.layout_picker {
                layout_command = show_layout_picker(ctx, picker).or(layout_command.take());
            }
            let _ = show_messages(ctx, &mut self.errors, messages_keyboard_input);
            host.messages_open = !self.errors.is_empty();
            host.messages_focused = host.messages_open && messages_input(ctx);
        });
        context.set_theme(previous_theme);
        host.ui_areas.capture(&context);
        if presentation_changed {
            self.changed();
        }
        if let Some(error) = render_error
            && !self.errors.contains(&error)
        {
            self.errors.push(error);
        }
        if let Some(index) = tab_selection
            && let Some(window) = self.index(host.id)
        {
            self.workspace.windows[window].active_tab = index;
            let pane = self.workspace.windows[window].tabs[index].focused;
            self.focus_pane(host.id, pane);
            host.search_focus = None;
            host.search_rects.clear();
            host.peek = None;
            host.repaint();
        }
        if let Some(target) = outline_selection {
            self.select_outline(host, target);
        }
        if let Some(id) = retry_pane {
            self.failed_panes.remove(&id);
        }
        match palette_event {
            Some(PaletteEvent::Action(action)) => commands.push(action),
            Some(PaletteEvent::PreviewTheme(theme)) => self.preview_theme(host, theme, false),
            Some(PaletteEvent::AcceptTheme(theme)) => self.preview_theme(host, theme, true),
            None => {}
        }
        if !host.palette.open
            && self
                .theme_preview
                .as_ref()
                .is_some_and(|preview| preview.owner == host.id)
        {
            self.cancel_theme_preview(Some(host));
        }
        for action in commands {
            self.action(event_loop, host, action, false);
        }
        for (id, action) in search_commands {
            self.search_action(host, id, action);
        }
        if let Some(command) = layout_command {
            self.layout_command(host, command);
        }
        if let Some(update) = &mut output.platform_output.accesskit_update {
            for (&id, pane) in &mut host.prepared {
                if host.rects.contains_key(&id) {
                    pane.accessibility(id).append_to(update);
                }
            }
        }
        if !self.background_smoke {
            host.egui.handle_platform_output_with_event_loop(
                &host.window,
                event_loop,
                output.platform_output,
            );
        }
        if !self.background_smoke
            && let Some(cursor) = self.pointer_cursor(host)
        {
            host.window.set_cursor(cursor);
        }
        #[cfg(target_os = "macos")]
        let progress_transaction = if offscreen.is_none()
            && !self.background_smoke
            && (host.progress.len() > 0
                || output.shapes.iter().any(|shape| {
                    matches!(&shape.shape,
                egui::Shape::Callback(callback) if callback.callback.is::<ProgressOverlay>())
                })) {
            let main = host.cursor.layer.as_ref().map_or_else(
                || native_cursor::main_layer(&host.window, None),
                native_cursor::NativeCursorLayer::main_layer,
            );
            match main {
                Ok(main) => Some(native_progress::begin_full_frame(main)),
                Err(error) => {
                    eprintln!("Rustty progress layer: {error}");
                    None
                }
            }
        } else {
            None
        };
        #[cfg(target_os = "macos")]
        if resolve_progress_overlays(
            &mut output.shapes,
            egui::Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(
                    size.width as f32 / output.pixels_per_point,
                    size.height as f32 / output.pixels_per_point,
                ),
            ),
            |bars| {
                if !bars.is_empty() && progress_transaction.is_none() {
                    host.progress.clear();
                    return false;
                }
                let bars: Vec<_> = bars
                    .iter()
                    .map(|bar| native_progress::Bar {
                        rect: bar.rect * (output.pixels_per_point / scale),
                        ..*bar
                    })
                    .collect();
                match host.progress.update(&host.window, &bars) {
                    Ok(()) => true,
                    Err(error) => {
                        eprintln!("Rustty progress layer: {error}");
                        false
                    }
                }
            },
        ) {
            host.repaint();
        }
        let primitives = self
            .context
            .tessellate(output.shapes, output.pixels_per_point);
        if host.capture
            && !self.painter.is_software()
            && let Some(smoke) = &self.smoke
            && smoke.offscreen
        {
            let state = self.painter.render_state().ok_or("GPU unavailable")?;
            smoke::capture(
                &state,
                &primitives,
                &output.textures_delta,
                [size.width, size.height],
                output.pixels_per_point,
                &smoke.directory.join("window.png"),
            )?;
            host.capture = false;
        }
        if let Some(directory) = &self.tmuxx_smoke
            && self.started.elapsed() > Duration::from_secs(1)
            && !directory.join("result.json").exists()
            && !host.prepared.is_empty()
        {
            let state = self.painter.render_state().ok_or("GPU unavailable")?;
            smoke::capture(
                &state,
                &primitives,
                &output.textures_delta,
                [size.width, size.height],
                output.pixels_per_point,
                &directory.join("window.png"),
            )?;
            let result = serde_json::json!({ "panes": host.prepared.len(), "windows": self.workspace.windows.len(), "instance": self.tmuxx.first().map(|entry| &entry.instance), "errors": self.errors, "hidden": !host.window.is_visible().unwrap_or(false), "focused": host.window.has_focus(), "floating_panes": host.overlay_panes.len(), "draw_order": host.pane_order });
            std::fs::write(
                directory.join("result.json"),
                serde_json::to_vec_pretty(&result)?,
            )?;
        }
        #[cfg(target_os = "macos")]
        let paints_before = if host.cursor.enabled || progress_transaction.is_some() {
            self.painter.terminal_paints(host.id)
        } else {
            0
        };
        #[cfg(target_os = "macos")]
        let cursor_transaction =
            if host.cursor.active && offscreen.is_none() && !self.background_smoke {
                host.cursor
                    .layer
                    .as_ref()
                    .and_then(|layer| match layer.begin_full_frame() {
                        Ok(transaction) => Some(transaction),
                        Err(error) => {
                            eprintln!("Rustty cursor layer: {error}");
                            None
                        }
                    })
            } else {
                None
            };
        if offscreen.is_some() || self.background_smoke && !self.painter.is_software() {
            let state = self.painter.render_state().ok_or("GPU unavailable")?;
            smoke::submit_offscreen(
                &state,
                &primitives,
                &output.textures_delta,
                [size.width, size.height],
                output.pixels_per_point,
                offscreen,
            );
            output.textures_delta.clear();
        } else {
            self.painter.paint(
                host.viewport,
                output.pixels_per_point,
                &primitives,
                &mut output.textures_delta,
                std::mem::take(&mut host.capture),
                &host.window,
            )?;
        }
        #[cfg(target_os = "macos")]
        let painted = self.painter.terminal_paints(host.id) > paints_before;
        #[cfg(target_os = "macos")]
        if host.cursor.enabled && host.cursor.active {
            if cursor_transaction.is_some_and(|transaction| transaction.finish(painted)) {
                host.cursor.active = false;
                host.cursor.on = None;
            } else {
                // Acquisition or layer recreation can delay replacement of the retained pixels.
                host.repaint();
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(transaction) = progress_transaction {
            let current = host
                .cursor
                .layer
                .as_ref()
                .map_or_else(
                    || native_cursor::main_layer(&host.window, None),
                    native_cursor::NativeCursorLayer::main_layer,
                )
                .ok();
            if !transaction.finish(painted, current) {
                host.repaint();
            }
        }
        #[cfg(target_os = "macos")]
        if host.cursor.enabled {
            if !host.cursor.active {
                let candidate = (cursor_eligible && painted && needs_blink)
                    .then(|| host.cursor_candidate(focused))
                    .flatten();
                host.update_cursor(&self.painter, candidate, &primitives, blink_deadline);
            }
            if host.cursor.active {
                needs_blink = false;
            }
        }
        host.frames += 1;
        host.last_frame_started = Some(now);
        host.deadline = animation_deadline;
        if self
            .tmuxx_smoke
            .as_ref()
            .is_some_and(|directory| !directory.join("result.json").exists())
        {
            let deadline = now + Duration::from_millis(100);
            host.deadline = Some(host.deadline.map_or(deadline, |old| old.min(deadline)));
        }
        if let Some(deadline) = host.focus_hint.deadline(now) {
            host.deadline = Some(host.deadline.map_or(deadline, |old| old.min(deadline)));
        }
        if let Some((_, deadline)) = host.navigation_warning {
            host.deadline = Some(host.deadline.map_or(deadline, |old| old.min(deadline)));
        }
        if needs_blink {
            let deadline = blink_deadline;
            host.deadline = Some(host.deadline.map_or(deadline, |old| old.min(deadline)));
        }
        Ok(())
    }
    fn passive_mouse_motion(&self, host: &Host, position: Pos2) -> bool {
        !host.modal_input()
            && host.peek.is_none()
            && host.divider_drag.is_none()
            && host.mouse_button.is_none()
            && host.egui.is_pointer_in_window()
            && !self.context.egui_is_using_pointer()
            && [host.mouse, position].into_iter().all(|position| {
                host.rects.iter().any(|(id, rect)| {
                    // Undo the painting inset: split gaps only change the native cursor.
                    rect.expand(1.0).contains(position) && !self.failed_panes.contains_key(id)
                }) && !host.ui_areas.contains(position)
            })
    }
    fn layout_command(&mut self, host: &mut Host, command: LayoutCommand) {
        let result = match command {
            LayoutCommand::Close => {
                host.layout_picker = None;
                Ok(())
            }
            LayoutCommand::Browse => self
                .platform
                .as_ref()
                .ok_or_else(|| "macOS file services are unavailable".to_owned())
                .and_then(Platform::choose_layout_path)
                .map(|path| {
                    if let Some(path) = path
                        && let Some(picker) = &mut host.layout_picker
                    {
                        picker.path = path.to_string_lossy().into_owned();
                        picker.error = None;
                    }
                }),
            LayoutCommand::Open(path) => {
                let path = path
                    .to_str()
                    .and_then(|path| path.strip_prefix("~/"))
                    .map(|path| self.config_loader.home.join(path))
                    .unwrap_or(path);
                workspace::load_layout(&path)
                    .and_then(|layout| {
                        let mut next = self.workspace.clone();
                        next.append_layout(layout)?;
                        self.remember();
                        self.workspace = next;
                        self.changed();
                        host.layout_picker = None;
                        Ok(())
                    })
                    .map_err(|error| format!("Could not open saved layout: {error}"))
            }
        };
        if let Err(error) = result
            && let Some(picker) = &mut host.layout_picker
        {
            picker.error = Some(error);
        }
        host.repaint();
    }
    fn cancel_selection_drag(&mut self, host: &mut Host) {
        if let Some((id, _)) = host.selection_drag.take() {
            if let Some(pane) = self.panes.get_mut(&id) {
                pane.reset_selection_gesture();
            }
            host.mouse_button = None;
        }
    }

    fn selection_scroll(&mut self, host: &mut Host, now: Instant) {
        self.refresh_selection(host, now, true);
    }

    /// User input resolves its endpoint immediately, including during a batch;
    /// background ticks wait for synchronized output to become visible.
    fn refresh_selection(&mut self, host: &mut Host, now: Instant, defer_synchronized: bool) {
        let Some(&(id, _)) = host.selection_drag.as_ref() else {
            return;
        };
        if host.mouse_button != Some(vt::MouseButton::Left)
            || !host.visible
            || host.occluded
            || host.ui_input()
            || host.peek.is_some()
            || self.focused(host.id) != Some(id)
            || !host.rects.contains_key(&id)
        {
            self.cancel_selection_drag(host);
            return;
        }
        let scale = host.window.scale_factor() as f32;
        let metrics = host.fonts.metrics();
        let padding = [
            self.window_config(host.id).window_padding_x.start * scale,
            self.window_config(host.id).window_padding_y.start * scale,
        ];
        let Some(pane) = self.panes.get_mut(&id) else {
            self.cancel_selection_drag(host);
            return;
        };
        let Ok(mut terminal) = pane.session.terminal() else {
            host.selection_drag = None;
            return;
        };
        let drag = &mut host.selection_drag.as_mut().unwrap().1;
        if defer_synchronized && let Some(deadline) = pane.sync_output.update(&mut terminal, now) {
            if drag.deadline.is_some() {
                drag.deadline = Some(deadline);
            }
            return;
        }
        let pointer = selection_pointer(
            &terminal,
            host.mouse,
            host.rects[&id],
            scale,
            padding,
            [metrics.cell_width, metrics.cell_height],
            host.modifiers.state().alt_key(),
        );
        let previous = (
            terminal.screen().viewport_offset,
            terminal.screen().selection,
        );
        if !drag.refresh(
            &mut terminal,
            &mut pane.selection_gesture,
            host.mouse,
            pointer,
            now,
        ) {
            host.selection_drag = None;
        }
        if previous
            != (
                terminal.screen().viewport_offset,
                terminal.screen().selection,
            )
        {
            host.repaint();
        }
    }

    fn scroll(&mut self, host: &mut Host, delta: MouseScrollDelta) {
        self.refresh_selection(host, Instant::now(), false);
        if host.modal_input()
            || host.zoom_animation.is_running()
            || host.peek.is_some()
            || host.divider_drag.is_some()
            || host.selection_drag.is_none() && host.ui_areas.contains(host.mouse)
        {
            return;
        }
        if let Some(id) = host
            .selection_drag
            .as_ref()
            .map(|(id, _)| *id)
            .or_else(|| host.hovered_pane())
        {
            let mut cursor_keys = None;
            let mouse = host.selection_drag.is_none()
                && self
                    .panes
                    .get(&id)
                    .filter(|pane| pane.session.accepts_input())
                    .and_then(|p| {
                        p.session.terminal().ok().map(|terminal| {
                            input::mouse_reporting(
                                &terminal,
                                host.modifiers.state(),
                                self.config().mouse_shift_capture,
                            )
                        })
                    })
                    .unwrap_or(false);
            let rows = match delta {
                MouseScrollDelta::LineDelta(_, y) if mouse => y.abs().ceil().copysign(y) as isize,
                MouseScrollDelta::LineDelta(_, y) => (y * 3.0).round() as isize,
                MouseScrollDelta::PixelDelta(pos) => self.panes.get_mut(&id).map_or(0, |pane| {
                    pane.scroll.take_pixels(
                        pos.y,
                        host.window.scale_factor(),
                        host.fonts.metrics().cell_height,
                    )
                }),
            };
            if mouse {
                for _ in 0..rows.unsigned_abs().min(128) {
                    self.mouse(
                        host,
                        vt::MouseAction::Press,
                        Some(if rows > 0 {
                            vt::MouseButton::WheelUp
                        } else {
                            vt::MouseButton::WheelDown
                        }),
                    );
                }
            } else if let Some(pane) = self.panes.get(&id)
                && let Ok(mut terminal) = pane.session.terminal()
            {
                if pane.session.accepts_input()
                    && terminal.is_alternate_screen()
                    && terminal.mouse_mode == 0
                    && terminal.modes.dec(1007)
                {
                    // Like Ghostty, alternate scroll emits ordinary cursor keys,
                    // independent of Kitty keyboard flags and held modifiers.
                    let count = rows.unsigned_abs().min(128);
                    let sequence = [
                        0x1b,
                        if terminal.modes.dec(1) { b'O' } else { b'[' },
                        if rows > 0 { b'A' } else { b'B' },
                    ];
                    if count != 0 && host.selection_drag.is_none() {
                        terminal.screen_mut().selection = None;
                    }
                    cursor_keys = Some(sequence.repeat(count));
                } else {
                    terminal.screen_mut().scroll_viewport(rows);
                }
            }
            if let Some(bytes) = cursor_keys {
                self.write(id, bytes);
            }
        }
        self.refresh_selection(host, Instant::now(), false);
        host.repaint();
    }
    /// Resolve the same target used by the platform's modified link click.
    /// Without the modifier there is no scan; an unchanged cell reuses its hit.
    fn update_hover_link(&mut self, host: &mut Host) -> bool {
        let result = (|| {
            if !input::link_modifier(host.modifiers.state())
                || !self.config().link_url
                || host.modal_input()
                || host.peek.is_some()
                || host.divider_drag.is_some()
                || self.platform.is_none()
                || host.ui_areas.contains(host.mouse)
            {
                return None;
            }
            let tab = self.tab(host.id)?;
            if tab
                .visible_tree(false)
                .divider_at_with_minimum(
                    host.content,
                    [host.mouse.x, host.mouse.y],
                    3.0,
                    self.split_minimum(host),
                )
                .is_some()
            {
                return None;
            }
            let id = host.hovered_pane()?;
            let rect = host.rects[&id];
            let config = self.window_config(host.id).clone();
            let pane = self.panes.get_mut(&id)?;
            let terminal = pane.session.terminal().ok()?;
            if pane.session.accepts_input()
                && input::mouse_reporting(
                    &terminal,
                    host.modifiers.state(),
                    config.mouse_shift_capture,
                )
                || terminal.modes.dec(2026)
            {
                return None;
            }
            let text = egui::Rect::from_min_max(
                rect.min + Vec2::new(config.window_padding_x.start, config.window_padding_y.start),
                rect.max - Vec2::new(config.window_padding_x.end, config.window_padding_y.end),
            );
            if !text.contains(host.mouse) {
                return None;
            }
            let metrics = host.fonts.metrics();
            let cell = Vec2::new(metrics.cell_width as f32, metrics.cell_height as f32)
                / host.window.scale_factor() as f32;
            let position = host.mouse - text.min;
            let screen = terminal.screen();
            let row = screen.viewport().nth((position.y / cell.y) as usize)?;
            let mut col = (position.x / cell.x) as usize;
            if row.cells.get(col)?.spacer_head() {
                return None;
            }
            if row.cells[col].width() == 0 && col > 0 {
                col -= 1;
            }
            let point = vt::GridPoint { row: row.id, col };
            let key = (
                id,
                terminal.generation,
                screen.viewport_offset,
                point,
                text,
                cell,
            );
            if host.link_hit == Some(key) {
                return Some(false);
            }
            let screen = screen.snapshot_viewport();
            let next = pane
                .links
                .links(&screen)
                .into_iter()
                .find(|link| link.contains(&screen, point))
                .map(|link| HoveredLink {
                    pane: id,
                    bounds: link_bounds(&screen, &link, text.min, cell),
                    uri: link.uri,
                });
            host.link_hit = Some(key);
            let changed = host.hovered_link != next;
            host.hovered_link = next;
            Some(changed)
        })();
        result.unwrap_or_else(|| {
            host.link_hit = None;
            host.hovered_link.take().is_some()
        })
    }

    fn pointer_cursor(&self, host: &Host) -> Option<CursorIcon> {
        if host.selection_drag.is_some() {
            return Some(CursorIcon::Text);
        }
        if host.modal_input()
            || host.ui_areas.contains(host.mouse)
            || host.peek.is_some()
            || host
                .search_rects
                .values()
                .any(|rect| rect.contains(host.mouse))
        {
            return None;
        }
        let divider = host.divider_drag.or_else(|| {
            if host.modal_pane.is_some()
                || host
                    .hovered_pane()
                    .is_some_and(|id| host.overlay_panes.contains(&id))
            {
                return None;
            }
            self.tab(host.id)?
                .visible_tree(false)
                .divider_at_with_minimum(
                    host.content,
                    [host.mouse.x, host.mouse.y],
                    3.0,
                    self.split_minimum(host),
                )
        });
        if let Some((_, axis, _)) = divider
            && self
                .tmuxx_window(host.id)
                .is_none_or(|entry| entry.view_connection().is_some())
        {
            return Some(match axis {
                Axis::Horizontal => CursorIcon::ColResize,
                Axis::Vertical => CursorIcon::RowResize,
            });
        }
        let id = host.hovered_pane()?;
        if host
            .hovered_link
            .as_ref()
            .is_some_and(|link| link.pane == id)
        {
            return Some(CursorIcon::Pointer);
        }
        let pane = self.panes.get(&id)?;
        if !pane.session.accepts_input() {
            return Some(CursorIcon::Text);
        }
        pane.session.terminal().ok()?.mouse_shape().parse().ok()
    }

    fn mouse(&mut self, host: &mut Host, action: vt::MouseAction, button: Option<vt::MouseButton>) {
        if host.zoom_animation.is_running()
            && host.peek.is_none()
            && action != vt::MouseAction::Release
        {
            return;
        }
        if host.selection_drag.is_some() && action != vt::MouseAction::Press {
            self.refresh_selection(host, Instant::now(), false);
            let text = if action == vt::MouseAction::Release
                && button == Some(vt::MouseButton::Left)
                && let Some((id, _)) = host.selection_drag.take()
                && let Some(pane) = self.panes.get_mut(&id)
                && let Ok(terminal) = pane.session.terminal()
            {
                // Refresh above has already marked movement and resolved the
                // endpoint, including content that moved just before mouse-up.
                let point = terminal
                    .screen()
                    .selection
                    .map(|selection| selection.end)
                    .or_else(|| pane.selection_gesture.anchor(&terminal));
                pane.selection_gesture.release(&terminal, point);
                terminal.screen().selection_text()
            } else {
                None
            };
            if let Some(text) = text {
                if matches!(
                    self.config().copy_on_select,
                    config::CopyOnSelect::Clipboard | config::CopyOnSelect::Both
                ) {
                    host.egui.set_clipboard_text(text.clone());
                }
                if matches!(
                    self.config().copy_on_select,
                    config::CopyOnSelect::Primary | config::CopyOnSelect::Both
                ) {
                    self.set_selection_text(text);
                }
            }
            return;
        }
        if !host.modal_input()
            && action == vt::MouseAction::Press
            && !matches!(
                button,
                Some(
                    vt::MouseButton::WheelUp
                        | vt::MouseButton::WheelDown
                        | vt::MouseButton::WheelLeft
                        | vt::MouseButton::WheelRight
                )
            )
        {
            if let Some((&id, _)) = host
                .search_rects
                .iter()
                .find(|(_, rect)| rect.contains(host.mouse))
            {
                self.focus_pane(host.id, id);
                host.search_focus = Some(id);
                host.focus_text_input = true;
                host.mouse_button = None;
                host.selection_drag = None;
                host.repaint();
                return;
            }
            if !host.ui_areas.contains(host.mouse) {
                host.search_focus = None;
            }
        }
        if self.update_hover_link(host) {
            host.repaint();
        }
        if !self.background_smoke
            && let Some(cursor) = self.pointer_cursor(host)
        {
            host.window.set_cursor(cursor);
        }
        if let Some((id, axis, bounds)) = host.divider_drag {
            if action == vt::MouseAction::Release {
                host.divider_drag = None;
            } else if action == vt::MouseAction::Move {
                let minimum = self.split_minimum(host);
                let ratio = match axis {
                    Axis::Horizontal => (host.mouse.x - bounds.x) / bounds.width,
                    Axis::Vertical => (host.mouse.y - bounds.y) / bounds.height,
                };
                if let Some(entry) = self.tmuxx_window(host.id) {
                    if let Some(connection) = entry.view_connection()
                        && let Some((pane, axis, total)) = entry.mapping.dividers.get(&id)
                    {
                        let amount =
                            (ratio.clamp(0.01, 0.99) * *total as f32).round().max(1.0) as u32;
                        if let Err(error) = connection.command([
                            "resize-pane".into(),
                            "-t".into(),
                            format!("%{pane}"),
                            if *axis == Axis::Horizontal {
                                "-x".into()
                            } else {
                                "-y".into()
                            },
                            amount.to_string(),
                        ]) {
                            self.errors.push(error.to_string());
                        }
                    }
                } else if let Some(tab) = self.tab_mut(host.id) {
                    tab.root.set_ratio_with_minimum(id, ratio, bounds, minimum);
                }
                self.changed();
            }
            host.repaint();
            return;
        }
        if host.ui_areas.contains(host.mouse) {
            return;
        }
        if !host.modal_input()
            && let Some(modal) = self.tab(host.id).and_then(|tab| tab.modal_overlay())
            && modal.close_click
            && modal_outside_click(
                host.rects.get(&modal.pane),
                host.content,
                host.mouse,
                action,
                button,
            )
            && let Some(entry) = self.tmuxx_window(host.id)
            && let Some(pane) = entry.mapping.panes.get(&modal.pane)
            && let Some(connection) = entry.view_connection()
        {
            if let Err(error) =
                connection.command(["kill-pane".into(), "-t".into(), format!("%{pane}")])
            {
                self.errors.push(error.to_string());
            }
            host.mouse_button = None;
            return;
        }
        if !host.modal_input()
            && host.peek.is_none()
            && host.modal_pane.is_none()
            && !host
                .hovered_pane()
                .is_some_and(|id| host.overlay_panes.contains(&id))
        {
            let divider = self.tab(host.id).and_then(|tab| {
                tab.visible_tree(false).divider_at_with_minimum(
                    host.content,
                    [host.mouse.x, host.mouse.y],
                    3.0,
                    self.split_minimum(host),
                )
            });
            if let Some(divider) = divider {
                if action == vt::MouseAction::Press
                    && button == Some(vt::MouseButton::Left)
                    && self
                        .tmuxx_window(host.id)
                        .is_none_or(|entry| entry.view_connection().is_some())
                {
                    self.remember();
                    host.divider_drag = Some(divider);
                }
                return;
            }
        }
        let hit = host.hovered_pane();
        if hit.is_none() && (action == vt::MouseAction::Press || host.mouse_button.is_none()) {
            return;
        }
        if host.ui_areas.contains(host.mouse) {
            return;
        }
        if let Some(peek) = &mut host.peek {
            if action == vt::MouseAction::Press
                && button == Some(vt::MouseButton::Left)
                && let Some(target) = hit.and_then(|id| self.tab(host.id)?.activate_quadrant(id))
            {
                peek.target = target;
                self.focus_pane(host.id, target);
                host.repaint();
            }
            return;
        }
        let wheel = matches!(
            button,
            Some(
                vt::MouseButton::WheelUp
                    | vt::MouseButton::WheelDown
                    | vt::MouseButton::WheelLeft
                    | vt::MouseButton::WheelRight
            )
        );
        if !wheel
            && action == vt::MouseAction::Press
            && let Some(id) = hit
        {
            self.focus_pane(host.id, id);
        }
        // Wheel reports follow the pointer without changing the keyboard target.
        let Some(id) = (if wheel { hit } else { self.focused(host.id) }) else {
            return;
        };
        let Some(rect) = host.rects.get(&id) else {
            return;
        };
        let scale = host.window.scale_factor() as f32;
        let metrics = host.fonts.metrics();
        let surface = (host.mouse - rect.min) * scale;
        let padding = [
            self.window_config(host.id).window_padding_x.start * scale,
            self.window_config(host.id).window_padding_y.start * scale,
            self.window_config(host.id).window_padding_x.end * scale,
            self.window_config(host.id).window_padding_y.end * scale,
        ];
        let position = surface - Vec2::new(padding[0], padding[1]);
        let config = &self.loaded.config;
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        let Ok(mut terminal) = pane.session.terminal() else {
            return;
        };
        if pane.session.accepts_input()
            && input::mouse_reporting(
                &terminal,
                host.modifiers.state(),
                config.mouse_shift_capture,
            )
        {
            pane.selection_gesture.reset(&mut terminal);
            host.selection_drag = None;
            let bytes = terminal.encode_mouse(
                vt::MouseEvent {
                    action,
                    button,
                    x: f64::from(surface.x),
                    y: f64::from(surface.y),
                    modifiers: input::terminal_modifiers(host.modifiers.state()),
                },
                vt::MouseEncodeOptions {
                    screen_size: [
                        f64::from(rect.width() * scale),
                        f64::from(rect.height() * scale),
                    ],
                    cell_size: [metrics.cell_width, metrics.cell_height],
                    padding: padding.map(f64::from),
                    any_button_pressed: host.mouse_button.is_some(),
                    last_cell: Some(&mut pane.mouse_cell),
                },
            );
            drop(terminal);
            self.send_input(id, SessionInput::Mouse(bytes));
            // Mouse reporting does not change the local screen; PTY output will repaint it.
            if action != vt::MouseAction::Move || !self.errors.is_empty() {
                host.repaint();
            }
            return;
        }
        if action != vt::MouseAction::Press || button != Some(vt::MouseButton::Left) {
            return;
        }
        let col = (position.x.max(0.0) as usize / metrics.cell_width as usize)
            .min(terminal.cols as usize - 1);
        let row = (position.y.max(0.0) as usize / metrics.cell_height as usize)
            .min(terminal.rows as usize - 1);
        let point = terminal
            .screen()
            .viewport()
            .nth(row)
            .map(|r| vt::GridPoint { row: r.id, col });
        if let Some(point) = point {
            if input::link_modifier(host.modifiers.state()) && config.link_url {
                pane.selection_gesture.reset(&mut terminal);
                host.selection_drag = None;
                if let Some(link) = &host.hovered_link
                    && link.pane == id
                {
                    let uri = link.uri.clone();
                    let cwd = pane.cwd.clone();
                    drop(terminal);
                    self.open_url(host, uri, cwd, false);
                }
            } else {
                host.selection_drag = Some((id, input::SelectionDrag::new(point, host.mouse)));
                let selection = input::selection_press(
                    &mut terminal,
                    &mut pane.selection_gesture,
                    &mut pane.links,
                    vt::selection_gesture::Press {
                        time: Some(pane.started.elapsed().as_nanos() as i128),
                        point,
                        xpos: f64::from(surface.x),
                        ypos: f64::from(surface.y),
                        max_distance: f64::from(metrics.cell_width),
                        repeat_interval: Platform::double_click_interval().as_nanos() as u64,
                        word_boundaries: vt::selection::DEFAULT_WORD_BOUNDARIES,
                        behaviors: vt::selection_gesture::DEFAULT_BEHAVIORS,
                    },
                );
                terminal.screen_mut().selection = selection;
            }
        }
        host.repaint();
    }
}

fn link_bounds(
    screen: &vt::Screen,
    link: &vt::search::Link,
    origin: Pos2,
    cell: Vec2,
) -> Vec<egui::Rect> {
    let rows: Vec<_> = screen.viewport().collect();
    let Some(start) = rows.iter().position(|row| row.id == link.start.row) else {
        return Vec::new();
    };
    let Some(end) = rows.iter().position(|row| row.id == link.end.row) else {
        return Vec::new();
    };
    (start..=end)
        .map(|index| {
            let left = if index == start { link.start.col } else { 0 };
            let right = if index == end {
                link.end.col
                    + rows[index]
                        .cells
                        .get(link.end.col)
                        .map_or(1, |cell| usize::from(cell.width().max(1)))
            } else {
                rows[index].cells.len()
            };
            egui::Rect::from_min_size(
                origin + Vec2::new(left as f32 * cell.x, index as f32 * cell.y),
                Vec2::new(right.saturating_sub(left) as f32 * cell.x, cell.y),
            )
        })
        .collect()
}

fn rgb(color: config::Rgb) -> Color32 {
    Color32::from_rgb(color.r, color.g, color.b)
}

impl ApplicationHandler<Event> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.platform.is_none() {
            let proxy = self.proxy.clone();
            let mut platform_config = self.config().clone();
            if self.import_selector {
                platform_config
                    .keybinds
                    .retain(|binding| !binding.flags.global);
            }
            match Platform::new(
                Arc::new(move |event| {
                    let _ = proxy.send_event(Event::Platform(event));
                }),
                &platform_config,
            ) {
                Ok(platform) => self.platform = Some(platform),
                Err(error) => self.errors.push(error),
            }
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if self.import_selector {
            if let Err(error) = self.open_import_selector(event_loop) {
                self.startup_error = Some(error.to_string());
                event_loop.exit();
            }
            return;
        }
        self.retry_tmuxx(Instant::now());
        if self.workspace.windows.iter().all(|w| w.quick)
            && (!self.tmuxx.iter().any(|entry| entry.waiting_for_window())
                || self
                    .tmuxx
                    .iter()
                    .all(|entry| entry.connection.is_none() && !entry.creating_window()))
        {
            self.add_window(false);
        }
        self.reconcile(event_loop);
        if self.windows.is_empty()
            && !self.errors.is_empty()
            && !self.tmuxx.iter().any(|entry| entry.waiting_for_window())
        {
            // Initialization failed before there was a client area in which to
            // show diagnostics. Let main report it instead of running invisibly.
            self.startup_error = Some(self.errors.join("\n"));
            event_loop.exit();
        }
    }
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Event) {
        match event {
            Event::Tmuxx => {
                if !self.import_selector {
                    self.refresh_tmuxx(event_loop);
                }
            }
            Event::TmuxxCopied(target, name, result) => {
                self.finish_tmuxx_copy(target, name, result);
                self.refresh_tmuxx(event_loop);
            }
            Event::DeckWake => {}
            Event::Output(id) => {
                if let Some(smoke) = &mut self.smoke {
                    smoke.record("pty-output");
                }
                self.drain(event_loop, id);
            }
            Event::Repaint(info, deadline) => {
                let current_pass = self.context.cumulative_pass_nr_for(info.viewport_id);
                if !repaint_is_current(info.current_cumulative_pass_nr, current_pass) {
                    if let Some(smoke) = &mut self.smoke {
                        smoke.record("egui-stale");
                    }
                    return;
                }
                if let Some(smoke) = &mut self.smoke {
                    smoke.record(if info.delay.is_zero() {
                        "egui-immediate"
                    } else {
                        "egui-delayed"
                    });
                }
                if let Some(host) = self
                    .windows
                    .values_mut()
                    .find(|host| host.viewport == info.viewport_id)
                {
                    // CPU presentation has no vsync backpressure. Pace egui's
                    // animation requests; input, PTY output and resize still
                    // request their own immediate redraws.
                    let deadline = if self.painter.is_software() && info.delay.is_zero() {
                        software_repaint_deadline(
                            deadline,
                            host.last_frame_started,
                            host.window
                                .current_monitor()
                                .and_then(|m| m.refresh_rate_millihertz()),
                        )
                    } else {
                        deadline
                    };
                    if deadline <= Instant::now() {
                        host.repaint();
                    } else {
                        host.deadline =
                            Some(host.deadline.map_or(deadline, |old| old.min(deadline)));
                    }
                } else if let Some(host) = self
                    .outliner
                    .as_mut()
                    .filter(|host| host.viewport == info.viewport_id)
                {
                    if deadline <= Instant::now() {
                        host.repaint();
                    } else {
                        host.deadline =
                            Some(host.deadline.map_or(deadline, |old| old.min(deadline)));
                    }
                }
            }
            Event::Platform(event) => {
                match event {
                    #[cfg(target_os = "macos")]
                    PlatformEvent::ScreensAwake(awake) => self.deck_screens_awake(awake),
                    PlatformEvent::NotificationClicked(pane) => {
                        self.reveal_pane(pane);
                    }
                    PlatformEvent::Tmuxx(command) => {
                        if !self.import_selector {
                            self.native_tmuxx_command(event_loop, command);
                        }
                    }
                    PlatformEvent::DumpSessions => self.request_session_dump(),
                    PlatformEvent::Action(action) => {
                        self.platform_action(event_loop, action, false);
                    }
                    #[cfg(target_os = "windows")]
                    PlatformEvent::GlobalHotkey(binding) => {
                        // A config reload can remove a binding while its event is queued.
                        if self.config().keybinds.contains(&binding) {
                            self.platform_binding(event_loop, binding);
                        }
                    }
                    #[cfg(target_os = "macos")]
                    PlatformEvent::GlobalHotkey { id, key_code } => {
                        self.consumed_global_keys
                            .insert(PhysicalKey::from_scancode(u32::from(key_code)));
                        if let Some(binding) = self
                            .platform
                            .as_ref()
                            .and_then(|platform| platform.global_keybinding(id))
                        {
                            self.platform_binding(event_loop, binding);
                        }
                    }
                    #[cfg(target_os = "macos")]
                    PlatformEvent::DroppedText {
                        window,
                        position: [x, y],
                        text,
                    } => {
                        if !self.import_selector
                            && let Some(mut host) = self.windows.remove(&window)
                        {
                            self.drop_text(&mut host, Pos2::new(x, y), text);
                            self.windows.insert(window, host);
                        }
                    }
                    #[cfg(target_os = "macos")]
                    PlatformEvent::Warning(message) => {
                        self.errors.push(message);
                        for host in self.windows.values_mut() {
                            host.messages_open = true;
                            host.repaint();
                        }
                    }
                }
                self.sync_host_state();
            }
            Event::Access(event) => {
                use egui_winit::accesskit_winit::WindowEvent as AccessEvent;
                if let Some(host) = self
                    .outliner
                    .as_mut()
                    .filter(|host| host.window.id() == event.window_id)
                {
                    match event.window_event {
                        AccessEvent::ActionRequested(request) => {
                            host.egui.on_accesskit_action_request(request)
                        }
                        AccessEvent::InitialTreeRequested => host.accesskit_active = true,
                        AccessEvent::AccessibilityDeactivated => host.accesskit_active = false,
                    }
                    host.repaint();
                    return;
                }
                if let Some(mut host) = self.windows.remove(&event.window_id) {
                    match event.window_event {
                        AccessEvent::ActionRequested(request) => {
                            let pane = host
                                .prepared
                                .iter()
                                .find(|(_, pane)| {
                                    pane.text
                                        .as_ref()
                                        .is_some_and(|text| text.contains(request.target_node))
                                })
                                .map(|(&id, _)| id);
                            let mut handled = false;
                            if let Some(id) = pane {
                                use egui::accesskit::{Action as AccessAction, ActionData};
                                match request.action {
                                    AccessAction::Focus => {
                                        self.focus_pane(host.id, id);
                                        if !self.background_smoke {
                                            host.window.focus_window();
                                        }
                                        handled = true;
                                    }
                                    AccessAction::ScrollUp | AccessAction::ScrollDown => {
                                        if let Some(pane) = self.panes.get(&id)
                                            && let Ok(mut terminal) = pane.session.terminal()
                                        {
                                            let amount = (terminal.rows as isize).max(1);
                                            terminal.screen_mut().scroll_viewport(
                                                if request.action == AccessAction::ScrollUp {
                                                    amount
                                                } else {
                                                    -amount
                                                },
                                            );
                                        }
                                        handled = true;
                                    }
                                    AccessAction::SetTextSelection => {
                                        self.cancel_selection_drag(&mut host);
                                        if let Some(ActionData::SetTextSelection(range)) =
                                            &request.data
                                            && let Some(selection) = host.prepared[&id]
                                                .text
                                                .as_ref()
                                                .and_then(|text| text.selection(*range))
                                            && let Some(pane) = self.panes.get(&id)
                                            && let Ok(mut terminal) = pane.session.terminal()
                                        {
                                            // Ignore a row that was pruned between painting and the native callback.
                                            if selection.is_none_or(|s| {
                                                terminal.screen().row_by_id(s.start.row).is_some()
                                                    && terminal
                                                        .screen()
                                                        .row_by_id(s.end.row)
                                                        .is_some()
                                            }) {
                                                terminal.screen_mut().selection = selection;
                                            }
                                        }
                                        handled = true;
                                    }
                                    _ => {}
                                }
                            }
                            if !handled {
                                host.egui.on_accesskit_action_request(request);
                            }
                        }
                        AccessEvent::InitialTreeRequested => host.accesskit_active = true,
                        AccessEvent::AccessibilityDeactivated => host.accesskit_active = false,
                    }
                    host.repaint();
                    self.windows.insert(event.window_id, host);
                    self.sync_host_state();
                }
            }
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, window: WindowId, event: WindowEvent) {
        // AppKit can close renderer-owned windows while the application unwinds.
        if event_loop.exiting() || std::thread::panicking() {
            return;
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if self.import_selector {
            self.import_window_event(event_loop, window, event);
            return;
        }
        if let WindowEvent::KeyboardInput {
            event,
            is_synthetic: false,
            ..
        } = &event
            && input::global_key_is_consumed(
                &mut self.consumed_global_keys,
                event.physical_key,
                event.state,
                event.repeat,
            )
        {
            return;
        }
        if self
            .outliner
            .as_ref()
            .is_some_and(|host| host.window.id() == window)
        {
            self.outliner_window_event(event_loop, event);
            return;
        }
        let Some(mut host) = self.windows.remove(&window) else {
            return;
        };
        host.messages_open = !self.errors.is_empty();
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) {
            // Clear the old hint before this click can focus another pane.
            host.focus_hint.dismiss();
            host.repaint();
        }
        if let Some(smoke) = &mut self.smoke {
            let input = match &event {
                WindowEvent::CursorMoved { position, .. } => {
                    let position = position.to_logical::<f32>(host.window.scale_factor());
                    let changed = host.mouse != Pos2::new(position.x, position.y);
                    smoke.pointer(Some(Pos2::new(position.x, position.y)), host.frames);
                    smoke.record(if changed {
                        "cursor-moved"
                    } else {
                        "cursor-unchanged"
                    });
                    changed
                }
                WindowEvent::CursorLeft { .. } => {
                    smoke.pointer(None, host.frames);
                    smoke.record("cursor-left");
                    false
                }
                WindowEvent::KeyboardInput {
                    is_synthetic: false,
                    ..
                }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::Touch(_) => {
                    smoke.record("user-input");
                    true
                }
                WindowEvent::RedrawRequested => {
                    smoke.record("redraw");
                    false
                }
                WindowEvent::Resized(_) => {
                    smoke.record("resize");
                    false
                }
                WindowEvent::Focused(_) => {
                    smoke.record("focus");
                    false
                }
                WindowEvent::Ime(_) => {
                    smoke.record("ime");
                    false
                }
                WindowEvent::Occluded(_) => {
                    smoke.record("occlusion");
                    false
                }
                _ => {
                    smoke.record("other-window-event");
                    false
                }
            };
            if input {
                smoke.input(host.frames);
            }
        }
        let passive_motion = if let WindowEvent::CursorMoved { position, .. } = &event {
            let position = position.to_logical::<f32>(host.window.scale_factor());
            self.passive_mouse_motion(&host, Pos2::new(position.x, position.y))
        } else {
            false
        };
        let egui_event_start = host.egui.egui_input_mut().events.len();
        let response = host.egui.on_window_event(&host.window, &event);
        let egui_event_end = host.egui.egui_input_mut().events.len();
        if passive_motion {
            // Merely ignoring response.repaint leaves PointerMoved queued, which
            // restarts egui's repaint loop when the next cursor-blink frame runs.
            host.deferred_pointer = input::defer_pointer_move(host.egui.egui_input_mut());
        } else if matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::Touch(_)
        ) {
            host.deferred_pointer = None;
        }
        if response.repaint && !passive_motion && !matches!(event, WindowEvent::RedrawRequested) {
            host.repaint();
        }
        match event {
            WindowEvent::CloseRequested => {
                self.action(event_loop, &mut host, Action::CloseWindow, false);
            }
            WindowEvent::RedrawRequested => {
                host.deadline = None;
                if let Err(error) = self.draw(event_loop, &mut host) {
                    let error = error.to_string();
                    if !self.errors.contains(&error) {
                        self.errors.push(error);
                    }
                }
            }
            WindowEvent::Resized(size) => {
                if let (Some(width), Some(height)) =
                    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    self.painter.on_window_resized(host.viewport, width, height);
                }
                if !self.remember_quick_frame(&host)
                    && let Some(index) = self.index(host.id)
                {
                    let size = size.to_logical::<f64>(host.window.scale_factor());
                    self.workspace.windows[index].frame[2] = size.width.max(1.0);
                    self.workspace.windows[index].frame[3] = size.height.max(1.0);
                    self.changed();
                }
                host.repaint();
            }
            WindowEvent::Moved(position) => {
                if !self.remember_quick_frame(&host)
                    && let Some(index) = self.index(host.id)
                {
                    let pos = position.to_logical::<f64>(host.window.scale_factor());
                    self.workspace.windows[index].frame[0] = pos.x;
                    self.workspace.windows[index].frame[1] = pos.y;
                    self.changed();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.update_fonts(Some(&mut host));
                host.repaint();
            }
            WindowEvent::ThemeChanged(theme) => {
                self.update_system_theme(
                    event_loop.system_theme().or(Some(theme)),
                    Some(&mut host),
                );
                host.repaint();
            }
            WindowEvent::Focused(focused) => {
                let was_focused = host.focused;
                host.focused = focused;
                if focused {
                    self.active = Some(host.id);
                    if let Some(pane) = self.focused(host.id) {
                        self.focus_pane(host.id, pane);
                    }
                } else {
                    self.cancel_selection_drag(&mut host);
                    host.peek = None;
                    host.modifiers = Modifiers::default();
                    host.consumed_keys.clear();
                    host.divider_drag = None;
                    host.mouse_button = None;
                    host.selection_drag = None;
                    host.composing = false;
                    host.preedit.clear();
                    host.preedit_selection = None;
                    host.sequence.clear();
                    host.sequence_len = 0;
                    if self
                        .index(host.id)
                        .is_some_and(|i| self.workspace.windows[i].quick)
                    {
                        let lost = self.platform.as_ref().map_or(Ok(was_focused), |platform| {
                            platform.quick_resigned_focus(&host.window, was_focused)
                        });
                        let lost = match lost {
                            Ok(lost) => lost,
                            Err(error) => {
                                self.errors.push(error);
                                false
                            }
                        };
                        if lost && self.config().quick_terminal_autohide && !host.ui_input() {
                            self.quick_visible(&mut host, false, false);
                        }
                    }
                }
                host.repaint();
            }
            WindowEvent::Occluded(occluded) => {
                host.occluded = occluded;
                if occluded {
                    self.cancel_selection_drag(&mut host);
                    host.deadline = None;
                } else {
                    host.repaint();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                host.modifiers = modifiers;
                let current = input::modifiers(host.modifiers.state());
                if let Some(peek) = host.peek
                    && !input::chord_held(peek.chord, current)
                {
                    host.peek = None;
                    if let Some(tab) = self.tab_mut(host.id) {
                        let target = tab.finish_peek(peek);
                        self.focus_pane(host.id, target);
                    }
                } else if host.peek.is_none()
                    && !host.ui_input()
                    && self
                        .tab(host.id)
                        .is_some_and(|tab| tab.quadrant_zoom.is_some())
                {
                    let trigger = self
                        .config()
                        .keybinds
                        .iter()
                        .filter(|b| b.table.is_none() && b.trigger.len() == 1)
                        .find(|b| {
                            b.trigger[0].modifiers == current
                                && current != config::Modifiers::default()
                                && b.actions.iter().any(|a| {
                                    matches!(
                                        a,
                                        Action::GotoSplit(
                                            Direction::QuadrantLeft
                                                | Direction::QuadrantRight
                                                | Direction::QuadrantUp
                                                | Direction::QuadrantDown
                                        )
                                    )
                                })
                        });
                    let chord = trigger.map(|binding| binding.trigger[0].modifiers);
                    if let Some(chord) = chord
                        && let Some(tab) = self.tab_mut(host.id)
                    {
                        host.peek = tab.begin_peek(chord);
                    }
                }
                host.repaint();
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if self.keyboard(event_loop, &mut host, &event) => {
                // Retain UI events queued by actions, but discard this key's native translation.
                host.egui
                    .egui_input_mut()
                    .events
                    .drain(egui_event_start..egui_event_end);
            }
            WindowEvent::Ime(ime) if !host.ui_input() => {
                host.egui
                    .egui_input_mut()
                    .events
                    .drain(egui_event_start..egui_event_end);
                let accepts_input = self.focused(host.id).is_some_and(|id| {
                    self.starting_panes.contains_key(&id)
                        || self
                            .panes
                            .get(&id)
                            .is_some_and(|pane| pane.session.accepts_input())
                });
                match ime {
                    Ime::Preedit(text, selection) if accepts_input => {
                        if !text.is_empty()
                            && let Some(pane) = self.focused(host.id)
                        {
                            self.terminal_input(&mut host, pane);
                        }
                        host.composing = !text.is_empty();
                        host.preedit = text;
                        host.preedit_selection = selection;
                    }
                    Ime::Commit(text) if accepts_input => {
                        host.composing = false;
                        host.preedit.clear();
                        host.preedit_selection = None;
                        if let Some(pane) = self.focused(host.id)
                            && !text.is_empty()
                        {
                            self.terminal_input(&mut host, pane);
                            let mut event = vt::KeyEvent::new(vt::Key::Unidentified);
                            event.text = Some(text);
                            self.send_input(
                                pane,
                                SessionInput::Key {
                                    event,
                                    options: Default::default(),
                                },
                            );
                        }
                    }
                    Ime::Disabled | Ime::Preedit(..) | Ime::Commit(_) => {
                        host.composing = false;
                        host.preedit.clear();
                        host.preedit_selection = None;
                    }
                    Ime::Enabled => {}
                }
                host.repaint();
            }
            WindowEvent::CursorMoved { position, .. } => {
                let pos = position.to_logical::<f32>(host.window.scale_factor());
                host.mouse = Pos2::new(pos.x, pos.y);
                if !host.modal_input() {
                    let button = host.mouse_button;
                    self.mouse(&mut host, vt::MouseAction::Move, button);
                }
                // Motion cannot add or remove windows or sessions. Even divider
                // drags update their geometry directly, without reconciliation.
                self.windows.insert(window, host);
                return;
            }
            WindowEvent::MouseInput { state, button, .. } if !host.modal_input() => {
                let button = match button {
                    MouseButton::Left => Some(vt::MouseButton::Left),
                    MouseButton::Middle => Some(vt::MouseButton::Middle),
                    MouseButton::Right => Some(vt::MouseButton::Right),
                    _ => None,
                };
                if state == ElementState::Pressed {
                    host.mouse_button = button;
                }
                self.mouse(
                    &mut host,
                    if state == ElementState::Pressed {
                        vt::MouseAction::Press
                    } else {
                        vt::MouseAction::Release
                    },
                    button,
                );
                if state == ElementState::Released {
                    host.mouse_button = None;
                    host.selection_drag = None;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.scroll(&mut host, delta);
            }
            WindowEvent::DroppedFile(path) => match Platform::cursor_position(&host.window) {
                Ok([x, y]) => self.drop_file(&mut host, Pos2::new(x, y), &path),
                Err(error) => self.errors.push(error),
            },
            _ => {}
        }
        self.windows.insert(window, host);
        self.reconcile(event_loop);
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        self.tick_import_selector();
        #[cfg(all(any(target_os = "macos", target_os = "windows"), debug_assertions))]
        self.tick_handoff_smoke(event_loop);
        if let Some(smoke) = &mut self.smoke {
            smoke.record("about-to-wait");
        }
        let now = Instant::now();
        self.retry_tmuxx(now);
        if self.workspace.windows.is_empty() && !self.errors.is_empty() && !self.tmuxx.is_empty() {
            self.add_window(false);
            self.reconcile(event_loop);
        }
        for pane in self.panes.values_mut() {
            self.deck.dirty |= pane.deck_progress.deadline().is_some_and(|at| at <= now);
            pane.deck_progress.expire(now);
        }
        #[cfg(target_os = "windows")]
        if let Some(platform) = &self.platform {
            platform.tick();
        }
        if let Some(mut smoke) = self.smoke.take() {
            let result = smoke.step(self, event_loop).and_then(|done| {
                smoke.check_background_windows(self)?;
                Ok(done)
            });
            match result {
                Ok(false) => self.smoke = Some(smoke),
                Ok(true) => {
                    event_loop.exit();
                    return;
                }
                Err(error) => {
                    self.smoke_error = Some(error.to_string());
                    event_loop.exit();
                    return;
                }
            }
        }
        let now = Instant::now();
        let sync_expired: Vec<_> = self
            .panes
            .iter()
            .filter(|(_, pane)| {
                pane.sync_output
                    .deadline
                    .is_some_and(|deadline| deadline <= now)
            })
            .map(|(&id, _)| id)
            .collect();
        for id in sync_expired {
            self.drain(event_loop, id);
        }
        let scroll_windows = self
            .windows
            .iter()
            .filter(|(_, host)| {
                host.selection_drag
                    .as_ref()
                    .is_some_and(|(_, drag)| drag.deadline.is_some_and(|deadline| deadline <= now))
            })
            .map(|(&window, _)| window)
            .collect::<Vec<_>>();
        for window in scroll_windows {
            if let Some(mut host) = self.windows.remove(&window) {
                self.selection_scroll(&mut host, now);
                self.windows.insert(window, host);
            }
        }
        let expired = self
            .panes
            .iter_mut()
            .filter_map(|(&id, pane)| {
                pane.activity
                    .deadline()
                    .filter(|&deadline| deadline <= now)
                    .map(|_| {
                        let previously_working = pane.reported_active();
                        let stopped = pane.activity.expire(now);
                        (id, stopped, previously_working != pane.reported_active())
                    })
            })
            .collect::<Vec<_>>();
        for (id, stopped, working_changed) in expired {
            self.deck.dirty = true;
            if working_changed {
                self.repaint_outliner();
            }
            if stopped {
                self.activity_stopped(id, now);
            }
            self.repaint_pane(id, true);
        }
        self.activity_flashes
            .retain(|_, flash| flash.next_repaint(now).is_some());
        let count = self.history.len() + self.redo.len();
        self.history.retain(|(expires, _)| *expires > now);
        self.redo.retain(|(expires, _)| *expires > now);
        if count != self.history.len() + self.redo.len() || !self.closing.is_empty() {
            self.reconcile(event_loop);
        }
        if !self.import_selector {
            self.tick_deck();
        }
        if self.close_at.is_some_and(|at| at <= now) {
            self.save();
            event_loop.exit();
            return;
        }
        if self.save_at.is_some_and(|at| at <= now) {
            self.save();
        }
        let mut next = self
            .save_at
            .into_iter()
            .chain(self.close_at)
            .chain(self.tmuxx.iter().filter_map(|entry| entry.deadline()))
            .chain(self.deck.deadline())
            .chain(self.panes.values().flat_map(|pane| {
                pane.activity
                    .deadline()
                    .into_iter()
                    .chain(pane.deck_progress.deadline())
                    .chain(pane.sync_output.deadline)
            }))
            .chain((!self.closing.is_empty()).then_some(now + Duration::from_millis(20)))
            .chain(
                self.history
                    .iter()
                    .chain(&self.redo)
                    .map(|(expires, _)| *expires),
            )
            .chain(self.smoke.as_ref().map(|smoke| {
                smoke
                    .next_cursor_poll()
                    .unwrap_or(now + Duration::from_millis(50))
            }))
            .min();
        #[cfg(target_os = "windows")]
        if let Some(deadline) = self.platform.as_ref().and_then(Platform::next_deadline) {
            next = Some(next.map_or(deadline, |old| old.min(deadline)));
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(deadline) = self.import_refresh_deadline() {
            next = Some(next.map_or(deadline, |old| old.min(deadline)));
        }
        #[cfg(all(any(target_os = "macos", target_os = "windows"), debug_assertions))]
        if let Some(deadline) = self.handoff_smoke_deadline() {
            next = Some(next.map_or(deadline, |old| old.min(deadline)));
        }
        for host in self.windows.values_mut() {
            #[cfg(target_os = "macos")]
            if !host.visible || host.occluded {
                host.progress.clear();
            }
            #[cfg(target_os = "macos")]
            if host.visible
                && !host.occluded
                && host.cursor.active
                && let Some(mut deadline) = host.cursor.deadline
            {
                if deadline <= now {
                    let (visible, next_deadline) =
                        cursor_blink_phase(host.cursor_blink_started, now);
                    if let Some(layer) = &host.cursor.layer
                        && visible != layer.is_visible()
                    {
                        layer.set_visible(visible);
                        host.cursor.updates += 1;
                    }
                    deadline = next_deadline;
                    host.cursor.deadline = Some(deadline);
                }
                next = Some(next.map_or(deadline, |old| old.min(deadline)));
            }
            if let Some(deadline) = host
                .selection_drag
                .as_ref()
                .and_then(|(_, drag)| drag.deadline)
            {
                next = Some(next.map_or(deadline, |old| old.min(deadline)));
            }
            if let Some(deadline) = host.deadline {
                if deadline <= now {
                    host.deadline = None;
                    host.repaint();
                } else if host.visible && !host.occluded {
                    next = Some(next.map_or(deadline, |old| old.min(deadline)));
                }
            }
        }
        if let Some(host) = &mut self.outliner
            && host.visible
            && let Some(deadline) = host.deadline
        {
            if deadline <= now {
                host.deadline = None;
                host.repaint();
            } else {
                next = Some(next.map_or(deadline, |old| old.min(deadline)));
            }
        }
        event_loop.set_control_flow(next.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }
    fn exiting(&mut self, _: &ActiveEventLoop) {
        self.deck.stop();
        if self.startup_error.is_none() {
            self.save();
        }
        for session in self
            .panes
            .values()
            .map(|pane| &pane.session)
            .chain(&self.closing)
        {
            session.close();
        }
    }
}

impl App {
    fn close_starting_panes(&mut self) {
        self.starting_panes
            .retain(|_, (_, receiver)| match receiver.try_recv() {
                Ok(Ok(session)) => {
                    session.close();
                    self.closing.push(session);
                    false
                }
                Err(mpsc::TryRecvError::Empty) => true,
                Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => false,
            });
    }

    fn shutdown(&mut self) {
        self.deck.stop();
        self.hide_outliner();
        let windows = std::mem::take(&mut self.windows);
        let outliner = self.outliner.take();
        let remaining = egui::ViewportIdSet::default();
        for host in windows.values() {
            host.window.set_visible(false);
            if let Some(platform) = &self.platform {
                platform.forget_window(&host.window);
            }
            self.painter
                .remove_window(host.viewport, host.id, &remaining);
        }
        if let Some(host) = &outliner {
            self.painter.remove_window(host.viewport, 0, &remaining);
        }
        drop(outliner);
        drop(windows);
        for session in self
            .panes
            .values()
            .map(|pane| &pane.session)
            .chain(&self.closing)
        {
            session.close();
        }
        // Window callbacks are finished. Give the child-owner workers a bounded
        // interval for HUP/SIGKILL escalation and reaping before main returns.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline
            && (self.deck.busy()
                || !self.starting_panes.is_empty()
                || self
                    .panes
                    .values()
                    .map(|pane| &pane.session)
                    .chain(&self.closing)
                    .any(|session| !session.has_exited()))
        {
            self.deck.reap();
            self.close_starting_panes();
            for session in self
                .panes
                .values()
                .map(|pane| &pane.session)
                .chain(&self.closing)
            {
                session.events().for_each(drop);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.panes.clear();
        self.closing.clear();
        self.starting_panes.clear();
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Release native owners before field destruction frees the workspace.
        self.shutdown();
    }
}

fn clipboard_policy(config: &Config, request: &vt::Effect) -> config::ClipboardAccess {
    let (policy, exempt) = match request {
        vt::Effect::ClipboardRead(read) => {
            (config.clipboard_read, read.granted || read.mimes.is_empty())
        }
        vt::Effect::ClipboardWrite(write) => (config.clipboard_write, write.granted),
        _ => return config::ClipboardAccess::Deny,
    };
    if policy == config::ClipboardAccess::Ask && exempt {
        config::ClipboardAccess::Allow
    } else {
        policy
    }
}

fn window_keeps_app_running(window: &WindowState) -> bool {
    // A Windows quick terminal is an auxiliary window, usually hidden. It must
    // not keep the process and its executable alive after the last window closes.
    !cfg!(target_os = "windows") || !window.quick
}

fn closes_last_window(workspace: &Workspace, window_id: Id, action: &Action) -> bool {
    let Some(window) = workspace
        .windows
        .iter()
        .find(|window| window.id == window_id)
    else {
        return false;
    };
    let closes_window = match action {
        Action::CloseWindow => true,
        Action::CloseTab => window.tabs.len() == 1,
        Action::CloseSurface => window.tabs.len() == 1 && window.tabs[0].pane_ids().len() == 1,
        _ => false,
    };
    closes_window
        && !workspace
            .windows
            .iter()
            .filter(|window| window.id != window_id)
            .any(window_keeps_app_running)
}

fn window_contains_pane(window: &WindowState, pane: Id) -> bool {
    window.tabs.iter().any(|tab| tab.panes.contains_key(&pane))
}

fn paste_needs_confirmation(config: &Config, terminal: Option<&vt::Terminal>, data: &[u8]) -> bool {
    config.clipboard_paste_protection
        && !vt::input::paste_is_safe(
            data,
            config.clipboard_paste_bracketed_safe
                && terminal.is_some_and(|terminal| terminal.modes.dec(2004)),
        )
}

fn paste_listing_request(location: vt::clipboard::Location) -> vt::clipboard::Read {
    let mut request = vt::clipboard::Read::osc52(location, vt::clipboard::Terminator::St);
    request.mimes.clear();
    request.list = true;
    request
}

fn emit_paste_event(
    terminal: &mut vt::Terminal,
    location: vt::clipboard::Location,
    mimes: &[Vec<u8>],
    random: &mut vt::paste::SecureRandom<'_>,
    output: &mut dyn std::io::Write,
) -> std::result::Result<bool, vt::paste::Error> {
    // Mode can change while native formats are queried. Return to text capture
    // instead of reading a native payload while holding the terminal lock.
    if !terminal.modes.dec(5522) {
        return Ok(false);
    }
    let mut read = |_: &[u8]| {
        Err(std::io::Error::other(
            "paste events must not read clipboard payloads",
        ))
    };
    terminal.paste(
        vt::paste::Request {
            source: vt::paste::Source::Clipboard(location),
            contents: vt::paste::Contents::Reader {
                mimes,
                read: &mut read,
            },
            allow_unsafe: false,
        },
        Some(random),
        output,
    )
}

fn hold_after_exit(config: &Config, runtime: Duration) -> bool {
    config.wait_after_command
        || runtime.as_millis() <= u128::from(config.abnormal_command_exit_runtime)
}

fn directory_from_osc(value: &str) -> Option<PathBuf> {
    let value = if let Some(uri) = value.strip_prefix("file://") {
        let (_, path) = uri.split_once('/')?;
        let mut bytes = Vec::with_capacity(path.len() + 1);
        bytes.push(b'/');
        let mut input = path.as_bytes().iter().copied();
        while let Some(byte) = input.next() {
            if byte == b'%' {
                let a = char::from(input.next()?).to_digit(16)?;
                let b = char::from(input.next()?).to_digit(16)?;
                bytes.push((a * 16 + b) as u8);
            } else {
                bytes.push(byte);
            }
        }
        String::from_utf8(bytes).ok()?
    } else {
        value.to_owned()
    };
    if !value.starts_with('/') || value.contains('\0') {
        return None;
    }
    Some(PathBuf::from(value))
}

fn show_tab_context_menu(
    response: &egui::Response,
    tab: &mut Tab,
    accent: [u8; 3],
    config: &Config,
) -> (bool, bool) {
    let mut changed = false;
    let mut close = false;
    egui::Popup::context_menu(response)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            let title_label = ui.label("Tab title");
            let mut title = tab.title.clone().unwrap_or_default();
            if ui
                .text_edit_singleline(&mut title)
                .labelled_by(title_label.id)
                .changed()
            {
                tab.title = (!title.is_empty()).then_some(title);
                changed = true;
            }
            ui.label("Tab color");
            ui.menu_button("Theme colors", |ui| {
                const NAMES: [&str; 18] = [
                    "Black",
                    "Red",
                    "Green",
                    "Yellow",
                    "Blue",
                    "Magenta",
                    "Cyan",
                    "White",
                    "Bright black",
                    "Bright red",
                    "Bright green",
                    "Bright yellow",
                    "Bright blue",
                    "Bright magenta",
                    "Bright cyan",
                    "Bright white",
                    "Background",
                    "Foreground",
                ];
                for (variation, target, amount) in [
                    ("Palette", config.background, 0.0),
                    ("Soft", config.background, 0.35),
                    ("Contrast", config.foreground, 0.25),
                ] {
                    ui.label(variation);
                    egui::Grid::new(("theme-tab-colors", variation))
                        .spacing(Vec2::splat(4.0))
                        .show(ui, |ui| {
                            for (index, base) in config.palette[..16]
                                .iter()
                                .copied()
                                .chain([config.background, config.foreground])
                                .enumerate()
                            {
                                let color = rgb(base).lerp_to_gamma(rgb(target), amount);
                                let value = [color.r(), color.g(), color.b()];
                                let label = format!(
                                    "{} · {variation} · #{:02X}{:02X}{:02X}",
                                    NAMES[index], value[0], value[1], value[2]
                                );
                                let selected = tab.color == Some(value);
                                let outline = TabAccent::new(Some(value), accent).foreground;
                                let response = ui.add(
                                    egui::Button::new("")
                                        .fill(color)
                                        .min_size(Vec2::splat(24.0))
                                        .stroke(egui::Stroke::new(
                                            if selected { 2.0 } else { 0.5 },
                                            Color32::from_rgb(outline[0], outline[1], outline[2]),
                                        )),
                                );
                                response.widget_info(|| {
                                    egui::WidgetInfo::selected(
                                        egui::WidgetType::Button,
                                        ui.is_enabled(),
                                        selected,
                                        &label,
                                    )
                                });
                                if response.clicked() {
                                    tab.color = Some(value);
                                    changed = true;
                                }
                                response.on_hover_text(label);
                                if index % 6 == 5 {
                                    ui.end_row();
                                }
                            }
                        });
                }
            });
            ui.menu_button("Choose color", |ui| {
                ui.spacing_mut().slider_width = 275.0;
                let color = tab.color.unwrap_or(accent);
                let mut selected = Color32::from_rgb(color[0], color[1], color[2]);
                if egui::color_picker::color_picker_color32(
                    ui,
                    &mut selected,
                    egui::color_picker::Alpha::Opaque,
                ) {
                    tab.color = Some([selected.r(), selected.g(), selected.b()]);
                    changed = true;
                }
            });
            if tab.color.is_some() && ui.button("Use system accent").clicked() {
                tab.color = None;
                changed = true;
            }
            if ui.button("Close tab").clicked() {
                close = true;
                ui.close();
            }
        });
    (changed, close)
}

fn show_layout_picker(ctx: &egui::Context, picker: &mut LayoutPicker) -> Option<LayoutCommand> {
    let mut open = true;
    let mut command = None;
    egui::Window::new("Open saved layout")
        .open(&mut open)
        .collapsible(false)
        .default_width(560.0)
        .show(ctx, |ui| {
            ui.label("Open saved tabs and panes in new windows.");
            ui.label("New shells start in the saved directories.");
            ui.add_space(8.0);
            for choice in &picker.choices {
                let selected = Path::new(&picker.path) == choice.path;
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            choice.available,
                            egui::Button::selectable(selected, &choice.name),
                        )
                        .on_hover_text(choice.path.display().to_string())
                        .clicked()
                    {
                        picker.path = choice.path.to_string_lossy().into_owned();
                        picker.error = None;
                    }
                    if !choice.available {
                        ui.weak("No saved layout found");
                    }
                });
            }
            ui.separator();
            ui.label("Saved layout file or folder");
            ui.horizontal(|ui| {
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut picker.path)
                            .desired_width(ui.available_width() - 80.0),
                    )
                    .changed()
                {
                    picker.error = None;
                }
                if ui.button("Browse…").clicked() {
                    command = Some(LayoutCommand::Browse);
                }
            });
            if let Some(error) = &picker.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    command = Some(LayoutCommand::Close);
                }
                if ui
                    .add_enabled(!picker.path.trim().is_empty(), egui::Button::new("Open"))
                    .clicked()
                {
                    command = Some(LayoutCommand::Open(PathBuf::from(picker.path.trim())));
                }
            });
        });
    if !open || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        command = Some(LayoutCommand::Close);
    }
    command
}

fn messages_input(ctx: &egui::Context) -> bool {
    ctx.memory(egui::Memory::focused)
        .and_then(|id| ctx.read_response(id))
        .is_some_and(|response| {
            response.layer_id
                == egui::LayerId::new(egui::Order::Middle, egui::Id::new("Rustty messages"))
        })
}

fn show_messages(
    ctx: &egui::Context,
    errors: &mut Vec<String>,
    keyboard_input: bool,
) -> Option<egui::Response> {
    if errors.is_empty() {
        return None;
    }
    let mut open = true;
    let dismiss = egui::Window::new("Rustty messages")
        .id(egui::Id::new("Rustty messages"))
        .open(&mut open)
        .collapsible(false)
        .default_size([600.0, 240.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    for error in errors.iter() {
                        let label = ui.add(
                            egui::Label::new(error)
                                .selectable(true)
                                .sense(Sense::click_and_drag()),
                        );
                        if (label.clicked() || label.drag_started()) && !label.has_focus() {
                            label.request_focus();
                        }
                    }
                });
            ui.button("Dismiss")
        })
        .and_then(|window| window.inner);
    if !open
        || dismiss.as_ref().is_some_and(egui::Response::clicked)
        || keyboard_input && ctx.input(|input| input.key_pressed(egui::Key::Escape))
    {
        errors.clear();
        // The current frame still contains the window's shapes.
        ctx.request_repaint();
    }
    dismiss
}

fn repaint_is_current(requested_pass: u64, current_pass: u64) -> bool {
    // Match egui's native runner: one completed pass still needs its requested
    // follow-up, but later passes have already superseded the request.
    current_pass == requested_pass || requested_pass.checked_add(1) == Some(current_pass)
}

fn software_repaint_deadline(
    requested: Instant,
    last_frame: Option<Instant>,
    refresh_millihertz: Option<u32>,
) -> Instant {
    // Some remote displays report no refresh rate. Bound unreasonable values
    // and use 60 Hz in that case; do not schedule any independent idle timer.
    let rate = refresh_millihertz
        .filter(|rate| (10_000..=240_000).contains(rate))
        .unwrap_or(60_000);
    let interval = Duration::from_secs_f64(1000.0 / f64::from(rate));
    last_frame.map_or(requested, |last| requested.max(last + interval))
}

fn load_config(
    loader: &mut config::ConfigLoader,
    args: &[String],
    theme: Option<Theme>,
) -> LoadedConfig {
    if let Some(theme) = theme {
        loader.dark_mode = theme == Theme::Dark;
    }
    loader.load_with_args(args)
}

fn color_scheme(dark_mode: bool) -> vt::query::ColorScheme {
    if dark_mode {
        vt::query::ColorScheme::Dark
    } else {
        vt::query::ColorScheme::Light
    }
}

fn window_visible_panes(
    window: &WindowState,
    visible: bool,
    occluded: bool,
    peek: bool,
) -> Vec<Id> {
    if !visible || occluded {
        return Vec::new();
    }
    window
        .tabs
        .get(window.active_tab)
        .map_or_else(Vec::new, |tab| tab.visible_panes(peek))
}

fn startup_panes(window: &WindowState, tmuxx: bool) -> BTreeMap<Id, PathBuf> {
    window
        .tabs
        .iter()
        .enumerate()
        .filter(|(index, _)| tmuxx || *index == window.active_tab)
        .flat_map(|(_, tab)| tab.panes.iter())
        .map(|(&id, pane)| (id, pane.working_directory.clone()))
        .collect()
}

fn initial_visible_panes(window: &WindowState, native: Option<(bool, bool, bool)>) -> Vec<Id> {
    // Normal windows are shown during creation; quick windows start hidden.
    let (visible, occluded, peek) = native.unwrap_or((!window.quick, false, false));
    window_visible_panes(window, visible, occluded, peek)
}

fn configure_ui_fonts(context: &egui::Context) {
    #[cfg(target_os = "macos")]
    let path = PathBuf::from("/System/Library/Fonts/SFNS.ttf");
    #[cfg(target_os = "windows")]
    let path = PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into()))
        .join("Fonts/segoeui.ttf");
    // Use the native UI face; egui's embedded fonts remain fallbacks.
    if let Ok(bytes) = std::fs::read(path) {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "system-ui".into(),
            Arc::new(egui::FontData::from_owned(bytes)),
        );
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(0, "system-ui".into());
        context.set_fonts(fonts);
    }
}

#[cfg(target_os = "macos")]
fn paint_window_controls_background(
    ui: &mut egui::Ui,
    buttons: Option<egui::Rect>,
    rows: &[outliner::Row],
    max_width: f32,
    background: Color32,
    foreground: Color32,
) -> Option<outliner::Target> {
    let shape = ui.painter().add(egui::Shape::Noop);
    let origin = buttons.map_or(Pos2::new(10.0, 4.0), |buttons| {
        Pos2::new(buttons.right() + 12.0, buttons.center().y - 13.0)
    });
    let mut crumb_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("window-controls-breadcrumb")
            .max_rect(egui::Rect::from_min_size(
                origin,
                Vec2::new(max_width, 26.0),
            ))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let selected = outliner_ui::breadcrumb(&mut crumb_ui, rows, max_width);
    let right = if crumb_ui.min_rect().is_positive() {
        crumb_ui.min_rect().right()
    } else {
        buttons.map_or(origin.x, |buttons| buttons.right())
    };
    ui.painter().set(
        shape,
        egui::Shape::rect_filled(
            egui::Rect::from_min_max(
                Pos2::ZERO,
                Pos2::new(
                    right + 12.0,
                    buttons.map_or(34.0, |buttons| buttons.bottom() + 10.0),
                ),
            ),
            egui::CornerRadius {
                nw: 14,
                se: 14,
                ..Default::default()
            },
            background
                .lerp_to_gamma(foreground, 0.12)
                .gamma_multiply(0.85),
        ),
    );
    selected
}

fn centered_tab_strip_rect(window: egui::Rect, available: egui::Rect, width: f32) -> egui::Rect {
    let width = width.min(available.width().max(0.0));
    let left = (window.center().x - width * 0.5).clamp(
        available.left(),
        (available.right() - width).max(available.left()),
    );
    egui::Rect::from_min_size(
        Pos2::new(left, available.top()),
        Vec2::new(width, available.height()),
    )
}

fn paint_tab_bleed(
    painter: &egui::Painter,
    tab: egui::Rect,
    mut strip: egui::Rect,
    accent: Color32,
    frame: Color32,
) {
    let rect = egui::Rect::from_min_max(tab.left_bottom(), Pos2::new(tab.right(), strip.bottom()));
    if !rect.is_positive() {
        return;
    }
    let radius = 4.0_f32.min(rect.height() * 0.5);
    let pixels_per_point = painter.ctx().pixels_per_point();
    let feather = painter.ctx().tessellation_options(|options| {
        if options.feathering {
            options.feathering_size_in_pixels * 0.5 / pixels_per_point
        } else {
            0.0
        }
    });
    // Fully visible tabs can flare past the scroll area's edges.
    if tab.left() >= strip.left() {
        strip.min.x -= radius + feather;
    }
    if tab.right() <= strip.right() {
        strip.max.x += radius + feather;
    }
    if !rect
        .expand2(Vec2::new(radius + feather, feather))
        .intersects(strip)
    {
        return;
    }
    let mut rows = vec![(rect.left_top(), rect.right_top(), Vec2::X)];
    for step in 0..=4 {
        let (sin, cos) = (std::f32::consts::FRAC_PI_2 * step as f32 / 4.0).sin_cos();
        let inward = Vec2::new(cos, sin);
        rows.push((
            Pos2::new(rect.left() - radius, rect.bottom() - radius) + inward * radius,
            Pos2::new(rect.right() + radius, rect.bottom() - radius)
                + Vec2::new(-cos, sin) * radius,
            inward,
        ));
    }
    let mut mesh = egui::Mesh::default();
    for (row, (left, right, inward)) in rows.into_iter().enumerate() {
        let right_inward = Vec2::new(-inward.x, inward.y);
        for (pos, color) in [
            (left - inward * feather, Color32::TRANSPARENT),
            (left + inward * feather, Color32::WHITE),
            (right + right_inward * feather, Color32::WHITE),
            (right - right_inward * feather, Color32::TRANSPARENT),
        ] {
            mesh.colored_vertex(pos, color);
        }
        // Bands keep triangles out of the concave corner cutouts.
        if row > 0 {
            let base = (row as u32 - 1) * 4;
            for column in 0..3 {
                let index = base + column;
                mesh.add_triangle(index, index + 1, index + 4);
                mesh.add_triangle(index + 4, index + 1, index + 5);
            }
        }
    }
    for vertex in &mut mesh.vertices {
        let fade = ((vertex.pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        vertex.color = accent
            .lerp_to_gamma(frame, fade)
            .gamma_multiply_u8(vertex.color.a());
    }
    painter.with_clip_rect(strip).add(mesh);
}

fn paint_pane_gutter(
    painter: &egui::Painter,
    outer: egui::Rect,
    inner: egui::Rect,
    color: Color32,
) {
    // Paint only the gaps: filling beneath translucent terminal panes would
    // apply background opacity twice. The four strips do not overlap.
    for (min, max) in [
        (outer.min, Pos2::new(inner.left(), outer.bottom())),
        (Pos2::new(inner.right(), outer.top()), outer.max),
        (
            Pos2::new(inner.left(), outer.top()),
            Pos2::new(inner.right(), inner.top()),
        ),
        (
            Pos2::new(inner.left(), inner.bottom()),
            Pos2::new(inner.right(), outer.bottom()),
        ),
    ] {
        painter.rect_filled(egui::Rect::from_min_max(min, max), 0.0, color);
    }
}

fn uncovered_rects(clip: egui::Rect, cover: egui::Rect) -> Vec<egui::Rect> {
    let cover = clip.intersect(cover);
    if !cover.is_positive() {
        return vec![clip];
    }
    [
        egui::Rect::from_min_max(clip.min, Pos2::new(cover.left(), clip.bottom())),
        egui::Rect::from_min_max(Pos2::new(cover.right(), clip.top()), clip.max),
        egui::Rect::from_min_max(Pos2::new(cover.left(), clip.top()), cover.right_top()),
        egui::Rect::from_min_max(cover.left_bottom(), Pos2::new(cover.right(), clip.bottom())),
    ]
    .into_iter()
    .filter(egui::Rect::is_positive)
    .collect()
}

fn clip_zoom_chrome(
    ctx: &egui::Context,
    layer: egui::LayerId,
    start: egui::layers::ShapeIdx,
    panes: &[zoom_animation::Pane],
    id: Id,
    frame_color: Color32,
) {
    let Some(visual) = panes.iter().find(|pane| pane.id == id) else {
        return;
    };
    let clip = !visual.moving && panes.iter().any(|pane| pane.moving);
    if visual.background_opacity == 1.0 && !clip {
        return;
    }
    ctx.graphics_mut(|graphics| {
        let list = graphics.entry(layer);
        if visual.background_opacity < 1.0 {
            let mix = 1.0 - visual.background_opacity;
            for index in start.0..list.next_idx().0 {
                list.mutate_shape(egui::layers::ShapeIdx(index), |shape| {
                    // The backdrop is already mixed; tint chrome without fading it twice.
                    egui::epaint::shape_transform::adjust_colors(&mut shape.shape, move |color| {
                        if *color != Color32::PLACEHOLDER {
                            *color =
                                color.lerp_to_gamma(frame_color.gamma_multiply_u8(color.a()), mix);
                        }
                    });
                });
            }
        }
        if clip {
            // Background labels must stay behind the enlarging pane, too.
            let shapes: Vec<_> = list.all_entries().skip(start.0).cloned().collect();
            for (offset, clipped) in shapes.into_iter().enumerate() {
                list.reset_shape(egui::layers::ShapeIdx(start.0 + offset));
                let mut clips = vec![clipped.clip_rect];
                for pane in panes.iter().filter(|pane| pane.moving) {
                    clips = clips
                        .into_iter()
                        .flat_map(|clip| uncovered_rects(clip, pane.rect.expand(1.0)))
                        .collect();
                }
                for rect in clips {
                    list.add(rect, clipped.shape.clone());
                }
            }
        }
    });
}

fn zoom_chrome<T>(
    ui: &mut egui::Ui,
    panes: &[zoom_animation::Pane],
    id: Id,
    bounds: egui::Rect,
    frame_color: Color32,
    paint: impl FnOnce(&mut egui::Ui) -> T,
) -> T {
    if panes.iter().all(|pane| !pane.moving && pane.opacity == 1.0) {
        return paint(ui);
    }
    let mut child = ui.new_child(egui::UiBuilder::new().id(ui.id()));
    child.set_clip_rect(ui.clip_rect().intersect(bounds));
    let visual = panes.iter().find(|pane| pane.id == id);
    if let Some(visual) = visual {
        child.multiply_opacity(if visual.background_opacity > 0.0 {
            visual.opacity / visual.background_opacity
        } else {
            0.0
        });
    }
    let start = child
        .ctx()
        .graphics_mut(|graphics| graphics.entry(child.layer_id()).next_idx());
    let result = paint(&mut child);
    clip_zoom_chrome(child.ctx(), child.layer_id(), start, panes, id, frame_color);
    result
}

fn paint_pane_frame(painter: &egui::Painter, bounds: egui::Rect, color: Color32) {
    painter.rect_stroke(
        bounds,
        0.0,
        egui::Stroke::new(1.0, color),
        egui::StrokeKind::Inside,
    );
}

#[cfg(target_os = "macos")]
struct ProgressOverlay {
    bar: native_progress::Bar,
    fallback: egui::Shape,
}

/// Replace only unobscured bars, so native layers cannot cover later UI.
#[cfg(target_os = "macos")]
fn resolve_progress_overlays(
    shapes: &mut [egui::epaint::ClippedShape],
    viewport: egui::Rect,
    present: impl FnOnce(&[native_progress::Bar]) -> bool,
) -> bool {
    let overlays: Vec<_> = shapes
        .iter()
        .enumerate()
        .filter_map(|(index, clipped)| {
            let egui::Shape::Callback(callback) = &clipped.shape else {
                return None;
            };
            let overlay = callback.callback.downcast_ref::<ProgressOverlay>()?;
            let bounds = overlay.bar.rect;
            let uncovered = clipped.clip_rect.contains_rect(bounds)
                && viewport.contains_rect(bounds)
                && shapes[index + 1..].iter().all(|later| {
                    !later
                        .shape
                        .visual_bounding_rect()
                        .intersect(later.clip_rect)
                        .intersect(bounds)
                        .is_positive()
                });
            Some((index, overlay.bar, overlay.fallback.clone(), uncovered))
        })
        .collect();
    let bars: Vec<_> = overlays
        .iter()
        .filter(|(_, _, _, uncovered)| *uncovered)
        .map(|(_, bar, _, _)| *bar)
        .collect();
    let presented = present(&bars);
    let mut repaint = false;
    for (index, _, fallback, uncovered) in overlays {
        shapes[index].shape = if presented && uncovered {
            egui::Shape::Noop
        } else {
            repaint = true;
            fallback
        };
    }
    repaint
}

/// Returns whether a visible indeterminate bar is animating.
fn paint_progress(
    ui: &mut egui::Ui,
    pane: Id,
    bounds: egui::Rect,
    progress: Progress,
    accent: Color32,
    offset: f32,
    native_started: Option<Instant>,
) -> bool {
    // Keep a static fill separate from the focused split's same-color outline.
    let bounds = bounds.shrink(2.0);
    let bar = egui::Rect::from_min_size(bounds.min, Vec2::new(bounds.width(), 2.0));
    if !ui.is_rect_visible(bar) {
        return false;
    }
    let color = match progress.state {
        2 => Color32::from_rgb(255, 59, 48),
        4 => Color32::from_rgb(255, 149, 0),
        _ => accent,
    };
    let percentage = progress.percentage();
    let (offset, fraction) = if let Some(value) = percentage {
        (0.0, f32::from(value) / 100.0)
    } else {
        ui.painter()
            .rect_filled(bar, 0.0, color.gamma_multiply(0.3));
        (offset, 0.25)
    };
    let fill = egui::Shape::from(
        egui::epaint::RectShape::filled(
            egui::Rect::from_min_size(
                bar.min + Vec2::new(offset * bar.width(), 0.0),
                Vec2::new(fraction * bar.width(), bar.height()),
            ),
            0.0,
            color,
        )
        .with_round_to_pixels(percentage.is_some()),
    );
    #[cfg(target_os = "macos")]
    let fill = if let Some(started) = native_started.filter(|_| percentage.is_none()) {
        egui::Shape::Callback(egui::epaint::PaintCallback {
            rect: bar,
            callback: Arc::new(ProgressOverlay {
                bar: native_progress::Bar {
                    pane,
                    rect: bar,
                    color,
                    started,
                },
                fallback: fill,
            }),
        })
    } else {
        fill
    };
    if percentage.is_none() && (!cfg!(target_os = "macos") || native_started.is_none()) {
        // GPU presentation supplies vsync backpressure; the CPU backend paces these requests.
        ui.ctx().request_repaint();
    }
    ui.painter().add(fill);
    let response = ui.interact(
        bar,
        egui::Id::new(("terminal-progress", pane)),
        Sense::empty(),
    );
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(egui::accesskit::Role::ProgressIndicator);
        node.set_label(match progress.state {
            2 => "Terminal progress — Error",
            4 => "Terminal progress — Paused",
            3 => "Terminal progress — In progress",
            _ => "Terminal progress",
        });
        if let Some(value) = percentage {
            node.set_min_numeric_value(0.0);
            node.set_max_numeric_value(100.0);
            node.set_numeric_value(f64::from(value));
        }
    });
    percentage.is_none()
}

fn ui_theme(config: &Config) -> egui::ThemePreference {
    match config.window_theme {
        config::WindowTheme::System => egui::ThemePreference::System,
        config::WindowTheme::Light => egui::ThemePreference::Light,
        config::WindowTheme::Dark => egui::ThemePreference::Dark,
        config::WindowTheme::Auto => {
            let color = config.background;
            if 299 * u32::from(color.r) + 587 * u32::from(color.g) + 114 * u32::from(color.b)
                < 128000
            {
                egui::ThemePreference::Dark
            } else {
                egui::ThemePreference::Light
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starting_pane_input_is_ordered_and_bounded() {
        let mut queue = VecDeque::new();
        let mut bytes = 0;
        queue_input(
            &mut queue,
            &mut bytes,
            SessionInput::Text(b"first".to_vec()),
        )
        .unwrap();
        queue_input(
            &mut queue,
            &mut bytes,
            SessionInput::Paste(b"second".to_vec()),
        )
        .unwrap();
        queue_input(&mut queue, &mut bytes, SessionInput::Text(Vec::new())).unwrap();
        assert_eq!(bytes, 11);
        assert_eq!(queue.len(), 2);
        assert!(matches!(&queue[0], SessionInput::Text(data) if data == b"first"));
        assert!(matches!(&queue[1], SessionInput::Paste(data) if data == b"second"));
        assert!(
            queue_input(
                &mut queue,
                &mut bytes,
                SessionInput::Text(vec![0; INPUT_BUDGET])
            )
            .is_err()
        );
        assert_eq!(bytes, 11);
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn pane_minimum_includes_scaled_cells_and_both_padding_edges() {
        let mut config = Config::default();
        config.window_padding_x = config::Padding {
            start: 3.25,
            end: 4.5,
        };
        config.window_padding_y = config::Padding {
            start: 1.0,
            end: 2.0,
        };
        let minimum = pane_minimum_size([15, 31], 1.5, &config);
        assert_eq!(minimum, [20.0, 26.0]);
        assert_eq!(
            pane_grid_size(
                Vec2::new(minimum[0] - 2.0, minimum[1] - 2.0),
                1.5,
                [15, 31],
                &config,
            ),
            (1, 1)
        );
        config.window_padding_x.end += 10.0;
        assert_eq!(pane_minimum_size([15, 31], 1.5, &config)[0], 30.0);
        assert_eq!(pane_minimum_size([30, 62], 1.5, &config), [40.0, 47.0]);
        assert_eq!(
            pane_grid_size(Vec2::new(100.0, 103.0), 1.5, [15, 30], &config),
            (8, 5)
        );
        config.window_padding_y.end += 20.0;
        assert_eq!(
            pane_grid_size(Vec2::new(100.0, 103.0), 1.5, [15, 30], &config),
            (8, 4)
        );
    }

    #[test]
    fn quit_dump_policy_forces_option_only_when_enabled_and_preserves_normal_close() {
        use config::SessionDumpOnQuit::{Checked, Disabled, Unchecked};
        use winit::keyboard::ModifiersState;
        assert!(quit_option_modifier(ModifiersState::ALT));
        assert!(!quit_option_modifier(ModifiersState::CONTROL));
        assert_eq!(
            quit_option_modifier(ModifiersState::ALT | ModifiersState::CONTROL),
            !cfg!(target_os = "windows")
        );
        for setting in [Disabled, Unchecked, Checked] {
            assert!(Confirmation::for_quit(None, Action::Quit, setting, true, false).is_none());
            let forced = Confirmation::for_quit(None, Action::Quit, setting, true, true);
            let natural = Confirmation::for_quit(
                Some(Confirmation::Close(Action::Quit)),
                Action::Quit,
                setting,
                true,
                false,
            );
            if setting == Disabled {
                assert!(forced.is_none());
                assert!(matches!(natural, Some(Confirmation::Close(Action::Quit))));
            } else {
                for modal in [forced, natural] {
                    assert!(
                        matches!(modal, Some(Confirmation::Quit { save_dump, keep_across_reboots: false, error: None, .. }) if save_dump == (setting == Checked))
                    );
                }
            }
            assert!(matches!(
                Confirmation::for_quit(
                    Some(Confirmation::Close(Action::CloseTab)),
                    Action::CloseTab,
                    setting,
                    false,
                    true,
                ),
                Some(Confirmation::Close(Action::CloseTab))
            ));
            assert!(Confirmation::for_quit(None, Action::CloseTab, setting, false, true).is_none());
        }
    }

    #[test]
    fn tab_bleed_keeps_edge_flares_visible_and_clips_scrolled_tabs() {
        let context = egui::Context::default();
        let viewport = egui::Rect::from_min_max(Pos2::new(80.0, 0.0), Pos2::new(300.0, 38.0));
        for (left, right, visible) in [
            (80.0, 180.0, true),
            (70.0, 170.0, false),
            (200.0, 300.0, true),
            (210.0, 310.0, false),
        ] {
            let tab = egui::Rect::from_min_max(Pos2::new(left, 6.0), Pos2::new(right, 28.0));
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        Pos2::ZERO,
                        Vec2::new(400.0, 100.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    paint_tab_bleed(
                        ui.painter(),
                        tab,
                        viewport,
                        Color32::YELLOW,
                        Color32::DARK_GRAY,
                    );
                },
            );
            output.textures_delta.clear();
            let flare_visible = output.shapes.iter().any(|shape| {
                let egui::Shape::Mesh(mesh) = &shape.shape else {
                    return false;
                };
                mesh.vertices.iter().any(|vertex| {
                    vertex.color.a() == 255
                        && shape.clip_rect.contains(vertex.pos)
                        && (vertex.pos.x < viewport.left() || vertex.pos.x > viewport.right())
                })
            });
            assert_eq!(flare_visible, visible, "tab bounds {left}..{right}");
        }
    }

    #[test]
    fn tab_bleed_stays_below_the_selected_tab_without_allocating_space() {
        let context = egui::Context::default();
        let strip = egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(400.0, 38.0));
        let accent = Color32::YELLOW;
        let frame = Color32::from_rgb(30, 30, 20);
        for (left, right) in [
            (80.0, 180.0),
            (-20.0, 60.0),
            (350.0, 430.0),
            (450.0, 500.0),
            (-100.0, -20.0),
        ] {
            let tab = egui::Rect::from_min_max(Pos2::new(left, 6.0), Pos2::new(right, 28.0));
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(strip),
                    ..Default::default()
                },
                |ui| {
                    let before = ui.available_rect_before_wrap();
                    paint_tab_bleed(ui.painter(), tab, strip, accent, frame);
                    assert_eq!(ui.available_rect_before_wrap(), before);
                },
            );
            output.textures_delta.clear();
            let meshes: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Mesh(mesh) => Some((shape.clip_rect, mesh)),
                    _ => None,
                })
                .collect();
            if left >= strip.right() || right <= strip.left() {
                assert!(meshes.is_empty());
                continue;
            }
            assert_eq!(meshes.len(), 1);
            let (clip, mesh) = meshes[0];
            assert_eq!(clip, strip);
            let rect =
                egui::Rect::from_min_max(tab.left_bottom(), Pos2::new(right, strip.bottom()));
            assert!(
                rect.expand2(Vec2::new(5.0, 1.0))
                    .contains_rect(mesh.calc_bounds())
            );
            assert!(mesh.vertices.iter().any(|vertex| vertex.color.a() == 0));
            let opaque: Vec<_> = mesh
                .vertices
                .iter()
                .filter(|vertex| vertex.color.a() == 255)
                .collect();
            assert!(!opaque.is_empty());
            assert!(opaque.iter().any(|vertex| vertex.pos.x < left - 2.0));
            assert!(opaque.iter().any(|vertex| vertex.pos.x > right + 2.0));
            let coverage = |point: Pos2| {
                mesh.indices
                    .chunks_exact(3)
                    .filter(|indices| {
                        let vertices = [indices[0], indices[1], indices[2]]
                            .map(|index| mesh.vertices[index as usize]);
                        if vertices.iter().any(|vertex| vertex.color.a() != 255) {
                            return false;
                        }
                        let cross = |a: Pos2, b: Pos2, c: Pos2| {
                            let [ab, ac] = [b - a, c - a];
                            ab.x * ac.y - ab.y * ac.x
                        };
                        let [a, b, c] = vertices.map(|vertex| vertex.pos);
                        let signs = [cross(a, b, point), cross(b, c, point), cross(c, a, point)];
                        signs.iter().all(|value| *value >= 0.0)
                            || signs.iter().all(|value| *value <= 0.0)
                    })
                    .count()
            };
            for point in [
                Pos2::new(left - 0.5, rect.bottom() - 0.75),
                Pos2::new(right + 0.5, rect.bottom() - 0.75),
                Pos2::new(left + rect.width() * 0.41, rect.top() + 1.0),
            ] {
                assert_eq!(coverage(point), 1, "inverted corner fill at {point:?}");
            }
            for point in [
                Pos2::new(left - 3.0, rect.bottom() - 2.0),
                Pos2::new(right + 3.0, rect.bottom() - 2.0),
            ] {
                assert_eq!(coverage(point), 0, "concave corner cutout at {point:?}");
            }
            for (y, color) in [(rect.top(), accent), (rect.bottom(), frame)] {
                let vertex = opaque
                    .iter()
                    .min_by(|a, b| (a.pos.y - y).abs().total_cmp(&(b.pos.y - y).abs()))
                    .unwrap();
                assert!(
                    vertex.color.to_array()[..3]
                        .iter()
                        .zip(color.to_array())
                        .all(|(actual, expected)| { actual.abs_diff(expected) <= 15 })
                );
            }
        }
    }

    #[test]
    fn server_selection_marker_is_accessible_and_leaves_room_for_labels() {
        for pinned in [false, true] {
            let context = egui::Context::default();
            context.enable_accesskit();
            let bounds = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0));
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(bounds),
                    ..Default::default()
                },
                |ui| {
                    if pinned {
                        paint_panel_pin(ui, 1, bounds);
                    } else {
                        DirectoryBadge {
                            label: DirectoryLabel::new(Some("rustty".into()), false, true, false)
                                .unwrap(),
                            bounds,
                            centered: false,
                            shared: false,
                            active: false,
                            attention: false,
                            pinned: false,
                            flash: 0.0,
                            accent: TabAccent::new(None, [0; 3]),
                        }
                        .paint(ui);
                    }
                    paint_server_selection(ui, 1, bounds, Color32::LIGHT_BLUE);
                },
            );
            output.textures_delta.clear();
            let update = output.platform_output.accesskit_update.unwrap();
            let marker = update
                .nodes
                .iter()
                .find_map(|(_, node)| {
                    (node.value() == Some("Selected on tmuxx server")).then_some(node)
                })
                .expect("accessible server selection marker");
            assert!(!marker.supports_action(egui::accesskit::Action::Focus));
            let marker_bounds = marker.bounds().unwrap();
            assert!(marker_bounds.x0 >= f64::from(bounds.left()));
            assert!(marker_bounds.x1 <= f64::from(bounds.right()));
            assert!(marker_bounds.y0 >= f64::from(bounds.top()));
            assert!(marker_bounds.y1 <= f64::from(bounds.bottom()));
            let label = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) => Some(rect.rect),
                    _ => None,
                })
                .unwrap();
            assert!(f64::from(label.bottom()) <= marker_bounds.y0);
            if pinned {
                let pin = update
                    .nodes
                    .iter()
                    .find_map(|(_, node)| {
                        (node.value() == Some("Pinned panel"))
                            .then(|| node.bounds())
                            .flatten()
                    })
                    .expect("accessible pin");
                assert!(pin.y1 <= marker_bounds.y0);
            }
        }
    }

    #[test]
    fn tab_context_menu_keeps_title_and_color_editors_open() {
        fn frame(
            context: &egui::Context,
            tab: &mut Tab,
            events: Vec<egui::Event>,
        ) -> egui::FullOutput {
            let mut raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    Vec2::new(800.0, 600.0),
                )),
                focused: true,
                events,
                ..Default::default()
            };
            input::filter_egui_events(&mut raw, egui::Popup::is_any_open(context));
            let mut output = context.run_ui(raw, |root| {
                egui::Panel::top("tabs").show(root, |ui| {
                    let response = ui.button("Terminal");
                    let mut config = Config::default();
                    config.palette[1] = config::Rgb::new(180, 40, 65);
                    config.background = config::Rgb::new(20, 25, 30);
                    config.foreground = config::Rgb::new(230, 235, 240);
                    assert!(!show_tab_context_menu(&response, tab, [80, 120, 160], &config).1);
                });
                egui::CentralPanel::default().show(root, |_| {});
            });
            output.textures_delta.clear();
            output
        }
        fn point(output: &egui::FullOutput, label: &str) -> Pos2 {
            let update = output.platform_output.accesskit_update.as_ref().unwrap();
            let bounds = update
                .nodes
                .iter()
                .find_map(|(_, node)| {
                    (node.label().is_some_and(|text| text.starts_with(label))
                        || label == "Tab title" && node.role() == egui::accesskit::Role::TextInput)
                        .then(|| node.bounds())
                        .flatten()
                })
                .unwrap_or_else(|| panic!("missing {label}"));
            Pos2::new(
                ((bounds.x0 + bounds.x1) / 2.0) as f32,
                ((bounds.y0 + bounds.y1) / 2.0) as f32,
            )
        }
        fn click(
            context: &egui::Context,
            tab: &mut Tab,
            pos: Pos2,
            button: egui::PointerButton,
        ) -> egui::FullOutput {
            let mut output = frame(context, tab, vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                output = frame(
                    context,
                    tab,
                    vec![egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    }],
                );
            }
            output
        }
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut tab = Tab::new(1, 2, "/tmp".into());
        let output = frame(&context, &mut tab, vec![]);
        click(
            &context,
            &mut tab,
            point(&output, "Terminal"),
            egui::PointerButton::Secondary,
        );
        frame(&context, &mut tab, vec![]);
        let output = frame(&context, &mut tab, vec![]);
        click(
            &context,
            &mut tab,
            point(&output, "Tab title"),
            egui::PointerButton::Primary,
        );
        assert!(egui::Popup::is_any_open(&context));
        assert!(context.text_edit_focused());
        frame(
            &context,
            &mut tab,
            vec![egui::Event::Text("work 日誌".into())],
        );
        assert_eq!(tab.title.as_deref(), Some("work 日誌"));
        let output = frame(&context, &mut tab, vec![]);
        click(
            &context,
            &mut tab,
            point(&output, "Theme colors"),
            egui::PointerButton::Primary,
        );
        let output = frame(&context, &mut tab, vec![]);
        click(
            &context,
            &mut tab,
            point(&output, "Red · Palette"),
            egui::PointerButton::Primary,
        );
        assert_eq!(tab.color, Some([180, 40, 65]));
        assert!(egui::Popup::is_any_open(&context));
        let output = frame(&context, &mut tab, vec![]);
        click(
            &context,
            &mut tab,
            point(&output, "Red · Soft"),
            egui::PointerButton::Primary,
        );
        assert_eq!(tab.color, Some([124, 35, 53]));
        let output = frame(&context, &mut tab, vec![]);
        click(
            &context,
            &mut tab,
            point(&output, "Red · Contrast"),
            egui::PointerButton::Primary,
        );
        assert_eq!(tab.color, Some([193, 89, 109]));
        let output = frame(&context, &mut tab, vec![]);
        click(
            &context,
            &mut tab,
            point(&output, "Choose color"),
            egui::PointerButton::Primary,
        );
        assert!(egui::Popup::is_any_open(&context));
        let output = frame(&context, &mut tab, vec![]);
        let picker = output
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Mesh(mesh) = &shape.shape {
                    let bounds = mesh.calc_bounds();
                    if bounds.width() > 200.0 && bounds.height() > 200.0 {
                        return Some(bounds);
                    }
                }
                None
            })
            .expect("color picker");
        click(
            &context,
            &mut tab,
            picker.center(),
            egui::PointerButton::Primary,
        );
        assert!(egui::Popup::is_any_open(&context));
        assert!(tab.color.is_some_and(|color| color != [80, 120, 160]));
        click(
            &context,
            &mut tab,
            Pos2::new(790.0, 590.0),
            egui::PointerButton::Primary,
        );
        assert!(!egui::Popup::is_any_open(&context));
    }

    #[test]
    fn binding_dispatch_runs_every_action_in_order_with_broadcast_flags() {
        for prefix in ["", "all:", "global:"] {
            let mut binding = config::KeyBinding::parse(&format!("{prefix}ctrl+a=ignore")).unwrap();
            let actions = vec![
                Action::Ignore,
                Action::Text(b"first".to_vec()),
                Action::Text(b"second".to_vec()),
            ];
            binding.actions = actions.clone();
            let mut dispatched = Vec::new();
            assert!(dispatch_binding(binding.clone(), |action, all| {
                dispatched.push((action, all));
                // A successful first action must not skip later actions.
                dispatched.len() == 1
            }));
            assert_eq!(
                dispatched,
                actions
                    .into_iter()
                    .map(|action| (action, !prefix.is_empty()))
                    .collect::<Vec<_>>()
            );
            assert!(!dispatch_binding(binding, |_, _| false));
        }
    }

    #[test]
    fn hovered_link_bounds_cover_wrapped_text_and_the_whole_wide_cell() {
        let origin = Pos2::new(8.0, 20.0);
        let cell = Vec2::new(7.0, 16.0);
        let mut terminal = vt::Terminal::new(10, 4, 0);
        terminal.feed(b"xx https://example.org/path");
        let mut matcher = vt::search::LinkMatcher::default();
        let link = matcher.links(terminal.screen()).pop().unwrap();
        assert_eq!(
            link_bounds(terminal.screen(), &link, origin, cell),
            [
                egui::Rect::from_min_size(Pos2::new(29.0, 20.0), Vec2::new(49.0, 16.0)),
                egui::Rect::from_min_size(Pos2::new(8.0, 36.0), Vec2::new(70.0, 16.0)),
                egui::Rect::from_min_size(Pos2::new(8.0, 52.0), Vec2::new(49.0, 16.0)),
            ],
        );
        terminal.feed("\x1b[2J\x1b[H\x1b]8;;https://target.example\x07go你\x1b]8;;\x07".as_bytes());
        let link = matcher.links(terminal.screen()).pop().unwrap();
        assert_eq!(
            link_bounds(terminal.screen(), &link, origin, cell),
            [egui::Rect::from_min_size(origin, Vec2::new(28.0, 16.0))],
        );
    }

    #[test]
    fn synchronized_output_releases_on_end_and_restarts_the_bounded_timeout() {
        let mut terminal = vt::Terminal::new(20, 3, 0);
        let mut sync = SynchronizedOutput::default();
        let start = Instant::now();
        let later = start + Duration::from_millis(500);
        assert_eq!(sync.update(&mut terminal, start), None);
        terminal.feed(b"\x1b[?2026h\x1b[2J\x1b[H");
        assert_eq!(
            sync.update(&mut terminal, start),
            Some(start + Duration::from_secs(1))
        );
        terminal.feed(b"partial");
        assert_eq!(
            sync.update(&mut terminal, later),
            Some(start + Duration::from_secs(1))
        );
        terminal.feed(b"\x1b[?2026h");
        assert_eq!(
            sync.update(&mut terminal, later),
            Some(later + Duration::from_secs(1))
        );
        terminal.feed(b" complete\x1b[?2026l");
        assert_eq!(sync.update(&mut terminal, later), None);

        terminal.feed(b"\x1b[?2026h");
        let deadline = sync.update(&mut terminal, later).unwrap();
        assert!(
            sync.update(&mut terminal, deadline - Duration::from_nanos(1))
                .is_some()
        );
        assert_eq!(sync.update(&mut terminal, deadline), None);
        assert!(!terminal.modes.dec(2026));
        terminal.feed(b"\x1b[?2026h");
        sync.update(&mut terminal, later);
        terminal.resize(21, 3);
        assert_eq!(sync.update(&mut terminal, later), None);
        terminal.feed(b"\x1b[?2026h");
        sync.update(&mut terminal, start);
        terminal.feed(b"\x1bc\x1b[?2026h");
        assert_eq!(
            sync.update(&mut terminal, later),
            Some(later + Duration::from_secs(1))
        );
    }

    #[test]
    fn floating_composition_clips_without_moving_origin_and_hit_testing_uses_stack_order() {
        let content = egui::Rect::from_min_size(Pos2::new(0.0, 10.0), Vec2::new(100.0, 100.0));
        let rects = BTreeMap::from([
            (90, content),
            (
                2,
                egui::Rect::from_min_size(Pos2::new(-5.0, 5.0), Vec2::new(30.0, 30.0)),
            ),
            (
                1,
                egui::Rect::from_min_size(Pos2::new(5.0, 15.0), Vec2::new(30.0, 30.0)),
            ),
        ]);
        let order = [90, 1, 2];
        assert_eq!(
            topmost_pane(&order, &rects, content, Pos2::new(10.0, 20.0), None),
            Some(2)
        );
        assert_eq!(
            topmost_pane(&order, &rects, content, Pos2::new(30.0, 20.0), None),
            Some(1)
        );
        assert_eq!(
            topmost_pane(&order, &rects, content, Pos2::new(-1.0, 20.0), None),
            None
        );
        assert_eq!(
            topmost_pane(&order, &rects, content, Pos2::new(70.0, 50.0), Some(2)),
            None
        );
        let mut frame = Frame::empty([30, 30]);
        frame.quads.push(rustty_render::Quad::solid(
            [0.0, 0.0, 30.0, 30.0],
            rustty_render::Color::rgb([255; 3]),
        ));
        let composed = ComposedFrame::new(
            [100, 110],
            vec![ComposedPane {
                transition: None,
                frame: Arc::new(frame),
                rect: [-5.0, 5.0, 30.0, 30.0],
                clip: [0.0, 10.0, 25.0, 25.0],
                dim: None,
            }],
        )
        .unwrap();
        assert_eq!(composed.frame.quads[0].rect, [0.0, 10.0, 25.0, 25.0]);
    }

    #[test]
    fn modal_key_and_outside_click_policies_reach_the_owner() {
        use winit::keyboard::{Key, ModifiersState as Mods, NamedKey};
        let mut modal = workspace::Overlay {
            pane: 7,
            x: 2,
            y: 2,
            cols: 4,
            rows: 4,
            z: 0,
            modal: true,
            capture_keys: false,
            close_click: true,
            close_cancel: true,
        };
        assert!(modal_captures_key(
            &modal,
            &Key::Named(NamedKey::Escape),
            Mods::empty()
        ));
        assert!(modal_captures_key(
            &modal,
            &Key::Character("c".into()),
            Mods::CONTROL
        ));
        assert!(!modal_captures_key(
            &modal,
            &Key::Named(NamedKey::Escape),
            Mods::ALT
        ));
        assert!(!modal_captures_key(
            &modal,
            &Key::Character("c".into()),
            Mods::CONTROL | Mods::SHIFT
        ));
        assert!(!modal_captures_key(
            &modal,
            &Key::Character("x".into()),
            Mods::CONTROL
        ));
        modal.capture_keys = true;
        assert!(modal_captures_key(
            &modal,
            &Key::Character("x".into()),
            Mods::CONTROL
        ));
        let content = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let rect = egui::Rect::from_min_size(Pos2::new(20.0, 20.0), Vec2::new(40.0, 40.0));
        let outside = Pos2::new(70.0, 70.0);
        assert!(modal_outside_click(
            Some(&rect),
            content,
            outside,
            vt::MouseAction::Press,
            Some(vt::MouseButton::Left)
        ));
        assert!(!modal_outside_click(
            Some(&rect),
            content,
            Pos2::new(30.0, 30.0),
            vt::MouseAction::Press,
            Some(vt::MouseButton::Left)
        ));
        assert!(!modal_outside_click(
            Some(&rect),
            content,
            outside,
            vt::MouseAction::Release,
            Some(vt::MouseButton::Left)
        ));
        assert!(!modal_outside_click(
            Some(&rect),
            content,
            outside,
            vt::MouseAction::Press,
            Some(vt::MouseButton::WheelUp)
        ));
        assert!(!modal_outside_click(
            Some(&rect),
            content,
            Pos2::new(110.0, 110.0),
            vt::MouseAction::Press,
            Some(vt::MouseButton::Left)
        ));
    }

    #[test]
    fn zoom_composition_moves_and_fades_glyphs_without_scaling_them() {
        use rustty_render::{Color, Paint, Quad};
        let background = Color::rgb([0; 3]).opacity(0.8);
        let frame_background = Color::rgb([255; 3]);
        let mut frame = Frame::empty([100, 80]);
        frame
            .quads
            .push(Quad::solid([0.0, 0.0, 100.0, 80.0], background));
        let mut glyph = Quad::solid([10.0, 20.0, 8.0, 16.0], Color::rgb([255; 3]));
        glyph.paint = Paint::Color;
        glyph.uv = [0.1, 0.2, 0.3, 0.4];
        frame.quads.push(glyph.clone());
        for opacity in [1.0, 0.5, 0.0] {
            let pane = ComposedPane {
                frame: Arc::new(frame.clone()),
                rect: [25.0, 30.0, 100.0, 80.0],
                clip: [25.0, 30.0, 200.0, 160.0],
                dim: None,
                transition: Some(ZoomPaint {
                    background,
                    frame_background,
                    opacity,
                    background_opacity: 1.0,
                    moving: true,
                }),
            };
            let composed = ComposedFrame::new([300, 240], vec![pane]).unwrap();
            let quads = &composed.frame.quads;
            assert_eq!(
                quads.len(),
                2,
                "replace the background instead of doubling its opacity"
            );
            assert_eq!(quads[0].rect, [25.0, 30.0, 200.0, 160.0]);
            assert_eq!(quads[0].color, background);
            assert_eq!(quads[1].rect, [35.0, 50.0, 8.0, 16.0]);
            assert_eq!(quads[1].uv, glyph.uv);
            assert_eq!(quads[1].color.0[3], opacity);
        }
    }

    #[test]
    fn zoom_background_mix_applies_uniformly_to_content_and_background() {
        use rustty_render::{Color, Quad};
        let bounds = [0.0, 0.0, 2.0, 1.0];
        let background = Color::rgb([0; 3]).opacity(0.8);
        let mut frame = Frame::empty([2, 1]);
        frame.quads.push(Quad::solid(bounds, background));
        frame
            .quads
            .push(Quad::solid([0.0, 0.0, 1.0, 1.0], Color::rgb([255, 0, 0])));
        for (retained, expected) in [
            (1.0, [255, 0, 0, 255, 0, 0, 0, 204]),
            (0.5, [128, 0, 128, 255, 0, 0, 128, 230]),
            (0.0, [0, 0, 255, 255, 0, 0, 255, 255]),
        ] {
            let composed = ComposedFrame::new(
                [2, 1],
                vec![ComposedPane {
                    frame: Arc::new(frame.clone()),
                    rect: bounds,
                    clip: bounds,
                    dim: None,
                    transition: Some(ZoomPaint {
                        background,
                        frame_background: Color::rgb([0, 0, 255]),
                        opacity: retained,
                        background_opacity: retained,
                        moving: false,
                    }),
                }],
            )
            .unwrap();
            let rect = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(2.0, 1.0));
            let primitives = [egui::ClippedPrimitive {
                clip_rect: rect,
                primitive: egui::epaint::Primitive::Callback(egui::PaintCallback {
                    rect,
                    callback: Arc::new(rustty_render_software::TerminalPaint {
                        window: 1,
                        frame: composed.frame,
                    }),
                }),
            }];
            let mut renderer = rustty_render_software::Renderer::default();
            let pixels = renderer
                .render([2, 1], 1.0, &primitives, &egui::TexturesDelta::default())
                .unwrap();
            for (actual, expected) in pixels.iter().zip(expected) {
                assert!(actual.abs_diff(expected) <= 1, "{retained}: {pixels:?}");
            }
        }
    }

    #[test]
    fn zoom_slides_over_translucent_neighbors_without_stacking_their_backgrounds() {
        let background = rustty_render::Color::rgb([20, 30, 40]).opacity(0.5);
        let mut frame = Frame::empty([100, 80]);
        frame.quads.push(rustty_render::Quad::solid(
            [0.0, 0.0, 100.0, 80.0],
            background,
        ));
        let frame = Arc::new(frame);
        let pane = |x, moving| ComposedPane {
            frame: Arc::clone(&frame),
            rect: [x, 0.0, 100.0, 80.0],
            clip: [x, 0.0, 100.0, 80.0],
            dim: None,
            transition: Some(ZoomPaint {
                background,
                frame_background: background,
                opacity: 0.5,
                background_opacity: 1.0,
                moving,
            }),
        };
        let composed =
            ComposedFrame::new([150, 80], vec![pane(0.0, false), pane(50.0, true)]).unwrap();
        let quads = &composed.frame.quads;
        assert_eq!(quads.len(), 2);
        assert_eq!(quads[0].rect, [0.0, 0.0, 50.0, 80.0]);
        assert_eq!(quads[1].rect, [50.0, 0.0, 100.0, 80.0]);
        assert!(quads.iter().all(|quad| quad.color.0[3] == 0.5));
    }

    #[test]
    fn zoom_chrome_mixes_once_on_parent_and_search_layers_preserving_alpha() {
        let bounds = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 80.0));
        for separate_layer in [false, true] {
            for (opacity, background_opacity, moving, expected_opacity, expected) in [
                (1.0, 1.0, false, 1.0, [128, 0, 0, 128]),
                (0.5, 0.5, false, 1.0, [64, 0, 64, 128]),
                (0.0, 0.0, false, 0.0, [0, 0, 0, 0]),
                (0.5, 1.0, true, 0.5, [64, 0, 0, 64]),
                (0.25, 0.5, true, 0.5, [32, 0, 32, 64]),
            ] {
                let context = egui::Context::default();
                let panes = [zoom_animation::Pane {
                    id: 1,
                    layout: bounds,
                    label_quadrant: None,
                    rect: bounds,
                    opacity,
                    background_opacity,
                    moving,
                }];
                let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                    let layer = if separate_layer {
                        egui::LayerId::new(egui::Order::Middle, egui::Id::new("search-overlay"))
                    } else {
                        ui.layer_id()
                    };
                    zoom_chrome(ui, &panes, 1, bounds, Color32::BLUE, |ui| {
                        assert_eq!(ui.opacity(), expected_opacity);
                        let painter = ui.painter().clone().with_layer_id(layer);
                        painter.rect_filled(
                            bounds,
                            0.0,
                            Color32::from_rgba_premultiplied(128, 0, 0, 128),
                        );
                        painter.rect_filled(bounds, 0.0, Color32::PLACEHOLDER);
                    });
                    if layer != ui.layer_id() {
                        clip_zoom_chrome(
                            &context,
                            layer,
                            egui::layers::ShapeIdx(0),
                            &panes,
                            1,
                            Color32::BLUE,
                        );
                    }
                    context.graphics_mut(|graphics| {
                        let colors = graphics
                            .entry(layer)
                            .all_entries()
                            .filter_map(|shape| match &shape.shape {
                                egui::Shape::Rect(rect) => Some(rect.fill),
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        if expected_opacity == 0.0 {
                            assert!(colors.is_empty());
                        } else {
                            assert_eq!(
                                colors,
                                [
                                    Color32::from_rgba_premultiplied(
                                        expected[0],
                                        expected[1],
                                        expected[2],
                                        expected[3],
                                    ),
                                    Color32::PLACEHOLDER,
                                ],
                                "layer={separate_layer}, b={background_opacity}"
                            );
                        }
                    });
                });
                output.textures_delta.clear();
            }
        }
    }

    #[test]
    fn rendered_directory_badges_mix_text_and_fill_uniformly() {
        let context = egui::Context::default();
        configure_ui_fonts(&context);
        let size = [320, 160];
        let bounds = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 160.0));
        let background = Color32::from_rgb(12, 20, 28);
        let frame_color = Color32::from_rgb(20, 80, 140);
        let mut renderer = rustty_render_software::Renderer::default();
        let mut baseline = Vec::new();
        let mut probes = Vec::new();
        for retained in [1.0, 0.5, 0.0, 1.0] {
            let panes = [zoom_animation::Pane {
                id: 1,
                layout: bounds,
                label_quadrant: None,
                rect: bounds,
                opacity: retained,
                background_opacity: retained,
                moving: false,
            }];
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(bounds),
                    ..Default::default()
                },
                |ui| {
                    ui.visuals_mut().override_text_color = Some(Color32::WHITE);
                    ui.painter().rect_filled(
                        bounds,
                        0.0,
                        background.lerp_to_gamma(frame_color, 1.0 - retained),
                    );
                    zoom_chrome(ui, &panes, 1, bounds, frame_color, |ui| {
                        DirectoryBadge {
                            label: DirectoryLabel::new(
                                Some("Background".into()),
                                false,
                                true,
                                false,
                            )
                            .unwrap(),
                            bounds,
                            centered: true,
                            shared: false,
                            active: false,
                            attention: false,
                            pinned: false,
                            flash: 0.0,
                            accent: TabAccent::new(None, [200, 80, 20]),
                        }
                        .paint(ui);
                    });
                },
            );
            let badge = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect) if rect.rect != bounds && rect.fill.a() == 255 => {
                    Some(rect.rect)
                }
                _ => None,
            });
            let primitives = context.tessellate(output.shapes, output.pixels_per_point);
            let pixels = renderer
                .render(
                    size,
                    output.pixels_per_point,
                    &primitives,
                    &output.textures_delta,
                )
                .unwrap();
            output.textures_delta.clear();
            if baseline.is_empty() {
                let glyph = pixels
                    .chunks_exact(4)
                    .position(|pixel| pixel == [255; 4])
                    .expect("native badge font must cover at least one glyph pixel fully");
                let badge = badge.expect("missing badge fill");
                let badge_fill =
                    (badge.center().y as usize * size[0] as usize) + (badge.left() + 4.0) as usize;
                probes = vec![glyph, badge_fill, 10 * size[0] as usize + 10];
                baseline = pixels.to_vec();
                assert_ne!(
                    &baseline[badge_fill * 4..badge_fill * 4 + 4],
                    &background.to_array()
                );
            }
            for index in &probes {
                let start = index * 4;
                for (channel, frame) in frame_color.to_array().into_iter().enumerate() {
                    let expected = (f32::from(baseline[start + channel]) * retained
                        + f32::from(frame) * (1.0 - retained))
                        .round() as u8;
                    assert!(
                        pixels[start + channel].abs_diff(expected) <= 2,
                        "retained={retained}, pixel={index}, channel={channel}: got {}, expected {expected}",
                        pixels[start + channel]
                    );
                }
            }
        }
    }

    #[test]
    fn zoom_background_chrome_is_clipped_around_the_moving_panel() {
        let bounds = egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(100.0, 80.0));
        let cover = egui::Rect::from_min_max(Pos2::new(20.0, 15.0), Pos2::new(75.0, 60.0));
        let uncovered = uncovered_rects(bounds, cover);
        assert_eq!(
            uncovered.iter().map(egui::Rect::area).sum::<f32>() + cover.area(),
            bounds.area()
        );
        for (i, rect) in uncovered.iter().enumerate() {
            assert!(!rect.intersect(cover).is_positive());
            for other in &uncovered[i + 1..] {
                assert!(!rect.intersect(*other).is_positive());
            }
        }
        assert!(uncovered_rects(bounds, bounds.expand(10.0)).is_empty());
        assert_eq!(
            uncovered_rects(bounds, bounds.translate(Vec2::new(200.0, 0.0))),
            [bounds]
        );
    }

    #[test]
    fn retained_window_frames_follow_panes_geometry_and_split_dimming() {
        let mut frame = Frame::empty([20, 20]);
        frame.quads.push(rustty_render::Quad::solid(
            [0.0, 0.0, 20.0, 20.0],
            rustty_render::Color::rgb([255; 3]),
        ));
        let frame = Arc::new(frame);
        let pane = || ComposedPane {
            transition: None,
            frame: Arc::clone(&frame),
            rect: [10.0, 15.0, 20.0, 20.0],
            clip: [10.0, 15.0, 20.0, 20.0],
            dim: None,
        };
        let retained = ComposedFrame::new([60, 60], vec![pane()]).unwrap();
        assert_eq!(retained.frame.quads[0].rect, pane().rect);
        assert!(retained.matches([60, 60], &[pane()]));
        assert!(!retained.matches([61, 60], &[pane()]));
        assert!(!retained.matches([60, 60], &[]));
        assert!(!retained.matches([60, 60], &[pane(), pane()]));

        let mut changed = pane();
        changed.rect[0] += 1.0;
        assert!(!retained.matches([60, 60], &[changed]));
        let mut changed = pane();
        changed.dim = Some(rustty_render::Color::rgb([0; 3]).opacity(0.5));
        assert!(!retained.matches([60, 60], &[changed]));
        let mut changed = pane();
        changed.frame = Arc::new((*frame).clone());
        assert!(!retained.matches([60, 60], &[changed]));
    }

    #[test]
    fn prepared_panes_follow_terminal_view_and_atlas_changes() {
        let mut fonts = rustty_render::Renderer::new(font_config(&Config::default(), 1.0)).unwrap();
        let rect = egui::Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(320.0, 160.0));
        let options = RenderOptions {
            size: [320, 160],
            ..Default::default()
        };
        let key =
            |terminal: &vt::Terminal| PaneRenderKey::new(terminal, options.clone(), rect, 1.0);
        let mut terminal = vt::Terminal::new(20, 3, 64);
        terminal.feed(b"first\r\nsecond\r\nthird\r\nfourth");
        let mut prepared = PreparedPane::new(
            key(&terminal),
            terminal.screen().snapshot_viewport(),
            &mut fonts,
        )
        .unwrap();
        assert!(
            prepared.matches(&key(&terminal), &fonts),
            "an unchanged pane reuses its prepared content"
        );

        // Host scrolling and selection mutate the screen without advancing the
        // terminal's stream generation, so both must participate in reuse.
        terminal.screen_mut().scroll_viewport(1);
        assert!(!prepared.matches(&key(&terminal), &fonts));
        terminal.screen_mut().scroll_viewport(-1);
        let point = vt::GridPoint {
            row: terminal.screen().row(0).id,
            col: 0,
        };
        terminal.screen_mut().selection = Some(vt::Selection {
            start: point,
            end: point,
            rectangular: false,
        });
        assert!(!prepared.matches(&key(&terminal), &fonts));
        terminal.screen_mut().selection = None;
        assert!(prepared.matches(&key(&terminal), &fonts));

        let mut searching = key(&terminal);
        searching
            .options
            .search_highlights
            .push(rustty_render::SearchHighlight {
                row: point.row,
                columns: 0..=2,
                selected: false,
            });
        assert!(!prepared.matches(&searching, &fonts));

        let mut composition = key(&terminal);
        composition.options.preedit = Some(rustty_render::Preedit {
            text: "日本".into(),
            selection: Some((3, 6)),
        });
        assert!(!prepared.matches(&composition, &fonts));
        let moved = rect.translate(Vec2::new(30.0, 0.0));
        let moved_key = PaneRenderKey::new(&terminal, options.clone(), moved, 1.0);
        assert!(!prepared.matches(&moved_key, &fonts));
        let moved_pane =
            PreparedPane::new(moved_key, terminal.screen().snapshot_viewport(), &mut fonts)
                .unwrap();
        assert_eq!(
            moved_pane.ime_rect,
            prepared.ime_rect.translate(Vec2::new(30.0, 0.0))
        );
        terminal.feed(b"!");
        assert!(!prepared.matches(&key(&terminal), &fonts));
        let mut prepared = PreparedPane::new(
            key(&terminal),
            terminal.screen().snapshot_viewport(),
            &mut fonts,
        )
        .unwrap();
        fonts.clear_cache();
        assert!(
            !prepared.matches(&key(&terminal), &fonts),
            "an atlas reset invalidates even unchanged panes"
        );

        terminal.feed(b"\x1b_Gi=1,s=1,v=1,f=32;/wAA/w==\x1b\\\x1b_Ga=f,i=1,s=1,v=1,f=32,z=50;AP8A/w==\x1b\\\x1b_Ga=a,i=1,r=1,z=50,s=3\x1b\\\x1b_Ga=p,i=1,C=1\x1b\\");
        assert_eq!(terminal.tick_graphics(100), Some(150));
        let mut prepared = PreparedPane::new(
            key(&terminal),
            terminal.screen().snapshot_viewport(),
            &mut fonts,
        )
        .unwrap();
        assert_eq!(terminal.tick_graphics(125), Some(150));
        assert!(prepared.matches(&key(&terminal), &fonts));
        assert_eq!(terminal.tick_graphics(150), Some(200));
        assert!(
            !prepared.matches(&key(&terminal), &fonts),
            "a Kitty animation frame must invalidate retained graphics"
        );
    }

    #[test]
    fn output_metadata_distinguishes_titles_and_pointer_shapes_from_terminal_changes() {
        let mut terminal = vt::Terminal::new(20, 3, 64);
        let key = |terminal: &vt::Terminal| {
            PaneRenderKey::new(terminal, RenderOptions::default(), egui::Rect::ZERO, 1.0)
        };
        let displayed = key(&terminal);
        terminal.feed(b"\x1b]2;new title\x07\x1b]22;pointer\x07");
        assert!(displayed.matches_terminal(&terminal));
        assert_eq!(terminal.mouse_shape(), "pointer");
        for bytes in [
            b"\x1b]2;title plus text\x07text".as_slice(),
            b"\x1b[?5h",                      // reverse video
            b"\x1b]4;1;#123456\x07",          // palette
            b"\x1b[?25l",                     // cursor visibility
            b"\x1b[2 q",                      // cursor style
            b"\x1b[?2026hpartial\x1b[?2026l", // completed batch
        ] {
            let displayed = key(&terminal);
            terminal.feed(bytes);
            assert!(!displayed.matches_terminal(&terminal), "{bytes:?}");
        }
        let displayed = key(&terminal);
        terminal.screen_mut().cursor.col += 1;
        assert!(!displayed.matches_terminal(&terminal));
        let displayed = key(&terminal);
        let point = vt::GridPoint {
            row: terminal.screen().row(0).id,
            col: 0,
        };
        terminal.screen_mut().selection = Some(vt::Selection {
            start: point,
            end: point,
            rectangular: false,
        });
        assert!(!displayed.matches_terminal(&terminal));
        terminal.feed(b"\r\na\r\nb\r\nc\r\nd");
        let displayed = key(&terminal);
        terminal.screen_mut().scroll_viewport(1);
        assert!(!displayed.matches_terminal(&terminal));
    }

    #[test]
    fn prepared_panes_ignore_blink_phase_only_for_steady_content() {
        let mut fonts = rustty_render::Renderer::new(font_config(&Config::default(), 1.0)).unwrap();
        let rect = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 160.0));
        let key = |terminal: &vt::Terminal, blink_visible| {
            PaneRenderKey::new(
                terminal,
                RenderOptions {
                    size: [320, 160],
                    blink_visible,
                    ..Default::default()
                },
                rect,
                1.0,
            )
        };
        let mut terminal = vt::Terminal::new(20, 3, 64);
        // A steady cursor and a hidden blinking cursor both ignore the phase.
        for cursor in [b"\x1b[2 q".as_slice(), b"\x1b[1 q\x1b[?25l"] {
            terminal.feed(cursor);
            let mut prepared = PreparedPane::new(
                key(&terminal, true),
                terminal.screen().snapshot_viewport(),
                &mut fonts,
            )
            .unwrap();
            assert!(prepared.matches(&key(&terminal, false), &fonts));
            assert!(prepared.matches(&key(&terminal, true), &fonts));
        }
        terminal.feed(b"\x1b[?25h");
        let mut prepared = PreparedPane::new(
            key(&terminal, true),
            terminal.screen().snapshot_viewport(),
            &mut fonts,
        )
        .unwrap();
        assert!(!prepared.matches(&key(&terminal, false), &fonts));

        terminal.feed(b"a\r\nb\r\nc\r\nd\x1b[H");
        terminal.screen_mut().scroll_viewport(1);
        let mut prepared = PreparedPane::new(
            key(&terminal, true),
            terminal.screen().snapshot_viewport(),
            &mut fonts,
        )
        .unwrap();
        assert!(prepared.screen.cursor.visible);
        assert!(!prepared.matches(&key(&terminal, false), &fonts));
        terminal.screen_mut().scroll_viewport(-1);
        terminal.feed(b"\x1b[?25l");
        let mut prepared = PreparedPane::new(
            key(&terminal, true),
            terminal.screen().snapshot_viewport(),
            &mut fonts,
        )
        .unwrap();
        // New blinking text must use the current phase even if the previous
        // frame was steady. Matching must not normalize the incoming options.
        terminal.feed(b"\x1b[5mblink\x1b[0m");
        let hidden = key(&terminal, false);
        assert!(!prepared.matches(&hidden, &fonts));
        let mut prepared =
            PreparedPane::new(hidden, terminal.screen().snapshot_viewport(), &mut fonts).unwrap();
        assert!(prepared.frame.blinking_text);
        assert!(prepared.matches(&key(&terminal, false), &fonts));
        assert!(!prepared.matches(&key(&terminal, true), &fonts));
        let shown = PreparedPane::new(
            key(&terminal, true),
            terminal.screen().snapshot_viewport(),
            &mut fonts,
        )
        .unwrap();
        assert_ne!(prepared.frame.quads, shown.frame.quads);
    }

    #[test]
    fn accessibility_is_lazy_and_uses_the_displayed_snapshot() {
        use egui::accesskit::{self as ak, Node, TextPosition, TextSelection};

        let mut fonts = rustty_render::Renderer::new(font_config(&Config::default(), 2.0)).unwrap();
        let metrics = fonts.metrics();
        let mut terminal = vt::Terminal::new(12, 2, 0);
        terminal.feed("A界e\u{301}B".as_bytes());
        let row = terminal.screen().row(0).id;
        let key = PaneRenderKey::new(
            &terminal,
            RenderOptions {
                size: [400, 160],
                padding: [8.0, 12.0],
                ..Default::default()
            },
            egui::Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(200.0, 80.0)),
            2.0,
        );
        let mut prepared =
            PreparedPane::new(key, terminal.screen().snapshot_viewport(), &mut fonts).unwrap();
        assert!(prepared.text.is_none());
        let frame = Arc::clone(&prepared.frame);

        terminal.feed(b"\x1b[?2026h\x1b[2J\x1b[Hpartial frame");
        let text = prepared.accessibility(7);
        let mut update = ak::TreeUpdate {
            nodes: vec![(text.id.accesskit_id(), Node::new(ak::Role::Terminal))],
            tree: None,
            tree_id: ak::TreeId::ROOT,
            focus: text.id.accesskit_id(),
        };
        text.append_to(&mut update);
        let (node_id, node) = &update.nodes[1];
        assert!(node.value().unwrap().starts_with("A界e\u{301}B"));
        assert_eq!(node.bounds().unwrap().x0, 14.0);
        assert_eq!(node.bounds().unwrap().y0, 26.0);
        assert_eq!(
            node.character_positions().unwrap()[1],
            metrics.cell_width as f32 / 2.0
        );
        assert!(text.contains(*node_id));
        assert_eq!(
            text.selection(TextSelection {
                anchor: TextPosition {
                    node: *node_id,
                    character_index: 1,
                },
                focus: TextPosition {
                    node: *node_id,
                    character_index: 3,
                },
            }),
            Some(Some(vt::Selection {
                start: vt::GridPoint { row, col: 1 },
                end: vt::GridPoint { row, col: 3 },
                rectangular: false,
            }))
        );
        assert!(Arc::ptr_eq(&frame, &prepared.frame));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn window_controls_background_is_translucent_and_rounds_opposite_corners() {
        let context = egui::Context::default();
        let buttons = egui::Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(54.0, 14.0));
        for (background, foreground) in [
            (Color32::BLACK, Color32::WHITE),
            (Color32::WHITE, Color32::BLACK),
        ] {
            let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                paint_window_controls_background(
                    ui,
                    Some(buttons),
                    &[],
                    420.0,
                    background,
                    foreground,
                );
            });
            output.textures_delta.clear();
            let shape = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) => Some(rect),
                    _ => None,
                })
                .unwrap();
            assert_eq!(shape.rect.min, Pos2::ZERO);
            assert_eq!(shape.rect.max, Pos2::new(76.0, 34.0));
            assert!(shape.rect.contains_rect(buttons));
            assert_eq!(shape.corner_radius.nw, shape.corner_radius.se);
            assert_eq!(shape.corner_radius.nw, 14);
            assert_eq!(shape.corner_radius.ne, 0);
            assert_eq!(shape.corner_radius.sw, 0);
            assert_eq!(shape.fill.a(), 217);
            assert_ne!(shape.fill, background);
        }
    }

    #[test]
    fn tab_strip_centers_until_the_breadcrumb_needs_space_and_fits_narrow_windows() {
        let window = egui::Rect::from_min_size(Pos2::new(10.0, 6.0), Vec2::new(1000.0, 26.0));
        for (left, right, width, expected_left, expected_width) in [
            (90.0, 1004.0, 200.0, 410.0, 200.0),
            (350.0, 1004.0, 200.0, 410.0, 200.0),
            (450.0, 1004.0, 200.0, 450.0, 200.0),
            (850.0, 1004.0, 300.0, 850.0, 154.0),
            (90.0, 550.0, 200.0, 350.0, 200.0),
            (90.0, 1004.0, 300.0, 360.0, 300.0),
        ] {
            let available = egui::Rect::from_min_max(Pos2::new(left, 6.0), Pos2::new(right, 32.0));
            let rect = centered_tab_strip_rect(window, available, width);
            assert_eq!(rect.left(), expected_left);
            assert_eq!(rect.width(), expected_width);
            assert!(available.contains_rect(rect));
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn window_breadcrumb_stays_clickable_with_hidden_controls_and_narrow_width() {
        let buttons = egui::Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(54.0, 14.0));
        let accent = Color32::from_rgb(255, 200, 0);
        let rows = outliner::breadcrumb(
            &Tab::new(1, 2, "/tmp".into()),
            &HashMap::from([(2, "Long panel directory name".repeat(8))]),
        );
        for (buttons, width) in [
            (Some(buttons), 80.0),
            (Some(buttons), 420.0),
            (None, 80.0),
            (None, 420.0),
        ] {
            let context = egui::Context::default();
            configure_ui_fonts(&context);
            let draw = |events| {
                let mut selected = None;
                let mut output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            Pos2::ZERO,
                            Vec2::new(600.0, 240.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        ui.visuals_mut().selection.bg_fill = accent;
                        let available = ui.available_rect_before_wrap();
                        selected = paint_window_controls_background(
                            ui,
                            buttons,
                            &rows,
                            width,
                            Color32::BLACK,
                            Color32::WHITE,
                        );
                        assert_eq!(ui.available_rect_before_wrap(), available);
                    },
                );
                output.textures_delta.clear();
                (output, selected)
            };
            draw(Vec::new());
            let (output, _) = draw(Vec::new());
            let egui::Shape::Rect(background) = &output.shapes[0].shape else {
                panic!("background must precede the breadcrumb");
            };
            let left = buttons.map_or(10.0, |buttons| buttons.right() + 12.0);
            if let Some(buttons) = buttons {
                assert!(background.rect.contains_rect(buttons));
            }
            assert_eq!(background.rect.right(), left + width + 12.0);
            assert_eq!(background.fill.a(), 217);
            let text = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text),
                    _ => None,
                })
                .unwrap();
            assert!(text.pos.x >= left);
            assert_eq!(
                text.galley.job.sections[0].format.font_id.family,
                egui::FontFamily::Proportional
            );
            assert_eq!(text.galley.job.sections[0].format.color, accent);
            assert!(output.shapes.iter().any(|shape| matches!(
                &shape.shape,
                egui::Shape::Rect(rect) if rect.fill == accent.gamma_multiply(0.15)
                    && rect.rect.contains_rect(text.visual_bounding_rect())
            )));
            assert!(background.rect.contains_rect(text.visual_bounding_rect()));
            let pos = Pos2::new(
                background.rect.right() - 14.0,
                buttons.map_or(17.0, |buttons| buttons.center().y),
            );
            draw(vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                let (_, selected) = draw(vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }]);
                assert_eq!(selected, (!pressed).then_some(rows[0].target));
            }
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn native_progress_keeps_accessibility_without_requesting_window_frames() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let bounds = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(204.0, 100.0));
        let started = Instant::now();
        for frame in 0..3 {
            let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                assert!(paint_progress(
                    ui,
                    1,
                    bounds,
                    Progress {
                        state: 3,
                        value: None
                    },
                    Color32::BLUE,
                    0.375,
                    Some(started)
                ));
            });
            assert!(!resolve_progress_overlays(
                &mut output.shapes,
                bounds,
                |bars| {
                    assert_eq!(bars.len(), 1);
                    assert_eq!(bars[0].pane, 1);
                    assert_eq!(bars[0].started, started);
                    assert_eq!(bars[0].rect.size(), Vec2::new(200.0, 2.0));
                    true
                }
            ));
            assert!(
                output
                    .platform_output
                    .accesskit_update
                    .as_ref()
                    .unwrap()
                    .nodes
                    .iter()
                    .any(|(_, node)| node.role() == egui::accesskit::Role::ProgressIndicator)
            );
            assert!(
                output
                    .shapes
                    .iter()
                    .all(|shape| !matches!(shape.shape, egui::Shape::Callback(_)))
            );
            if frame == 2 {
                assert_eq!(
                    output.viewport_output[&ViewportId::ROOT].repaint_delay,
                    Duration::MAX
                );
            }
            output.textures_delta.clear();
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn native_progress_falls_back_when_covered_clipped_or_unavailable() {
        let context = egui::Context::default();
        let bounds = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(204.0, 100.0));
        for (covered, clipped, available) in [
            (true, false, true),
            (false, true, true),
            (false, false, false),
        ] {
            let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                if clipped {
                    ui.set_clip_rect(bounds.shrink(3.0));
                }
                assert!(paint_progress(
                    ui,
                    1,
                    bounds,
                    Progress {
                        state: 3,
                        value: None
                    },
                    Color32::BLUE,
                    0.375,
                    Some(Instant::now())
                ));
                if covered {
                    ui.painter().rect_filled(bounds, 0.0, Color32::BLACK);
                }
            });
            assert!(resolve_progress_overlays(
                &mut output.shapes,
                bounds,
                |bars| {
                    assert_eq!(bars.len(), usize::from(!covered && !clipped));
                    available
                }
            ));
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill == Color32::BLUE && rect.rect.left() == 77.0 && rect.rect.width() == 50.0)));
            assert!(
                output
                    .shapes
                    .iter()
                    .all(|shape| !matches!(shape.shape, egui::Shape::Callback(_)))
            );
            output.textures_delta.clear();
        }
        assert!(!resolve_progress_overlays(&mut [], bounds, |bars| {
            assert!(
                bars.is_empty(),
                "disappearing bars must remove their native layers"
            );
            true
        }));
    }

    #[test]
    fn determinate_progress_does_not_blend_into_the_focused_split_outline() {
        let context = egui::Context::default();
        let bounds = egui::Rect::from_min_size(Pos2::new(20.0, 30.0), Vec2::new(200.0, 100.0));
        let accent = Color32::from_rgb(200, 90, 230);
        for percentage in [25, 50, 100] {
            let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                paint_pane_frame(ui.painter(), bounds, accent);
                assert!(!paint_progress(
                    ui,
                    1,
                    bounds,
                    Progress {
                        state: 1,
                        value: Some(percentage)
                    },
                    accent,
                    0.0,
                    None,
                ));
            });
            output.textures_delta.clear();
            let rectangles: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) => Some(rect),
                    _ => None,
                })
                .collect();
            let (outline, fill) = (rectangles[0], rectangles[1]);
            assert!(
                fill.rect.top() >= outline.rect.top() + outline.stroke.width,
                "a static fill with the same accent must not cover the split outline"
            );
            assert!(fill.rect.left() >= outline.rect.left() + outline.stroke.width);
            assert!(fill.rect.right() <= outline.rect.right() - outline.stroke.width);
            assert_eq!(
                fill.rect.width(),
                (bounds.width() - 4.0) * f32::from(percentage) / 100.0
            );
        }
    }

    #[test]
    fn indefinite_progress_matches_swiftui_motion_and_requests_the_next_frame() {
        let context = egui::Context::default();
        let bounds = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(204.0, 100.0));
        let started = Instant::now();
        let mut activity = Activity::default();
        activity.progress_reported(3, None, started);
        // Values sampled from SwiftUI's UnitCurve.easeInOut. Ghostty traverses
        // 75% of the pane in 1.2 seconds, then automatically reverses.
        for (millis, position) in [
            (0, 0.0),
            (150, 0.0311136246),
            (300, 0.1291618347),
            (600, 0.5),
            (900, 0.8708381653),
            (1200, 1.0),
            (1500, 0.8708381653),
            (1800, 0.5),
            (2400, 0.0),
        ] {
            let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                assert!(paint_progress(
                    ui,
                    1,
                    bounds,
                    Progress {
                        state: 3,
                        value: None
                    },
                    Color32::BLUE,
                    activity.progress_offset(started + Duration::from_millis(millis)),
                    None,
                ));
            });
            output.textures_delta.clear();
            let fill = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) => Some(rect),
                    _ => None,
                })
                .next_back()
                .unwrap();
            assert!(
                (f64::from(fill.rect.left()) - (2.0 + 150.0 * position)).abs() < 0.001,
                "position at {millis}ms differs from SwiftUI: {:?}",
                fill.rect
            );
            assert_eq!(fill.rect.width(), 50.0);
            assert_eq!(
                output.viewport_output[&ViewportId::ROOT].repaint_delay,
                Duration::ZERO
            );
        }
        // Stop requesting frames after the bar becomes determinate or is clipped.
        // Allow egui's already queued repaint to drain before checking idleness.
        for visible in [true, false] {
            for frame in 0..3 {
                let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                    if !visible {
                        ui.set_clip_rect(egui::Rect::NOTHING);
                    }
                    assert!(!paint_progress(
                        ui,
                        1,
                        bounds,
                        Progress {
                            state: 3,
                            value: visible.then_some(50)
                        },
                        Color32::BLUE,
                        0.0,
                        None,
                    ));
                });
                output.textures_delta.clear();
                if frame == 2 {
                    assert_eq!(
                        output.viewport_output[&ViewportId::ROOT].repaint_delay,
                        Duration::MAX
                    );
                }
            }
        }
    }

    #[test]
    fn progress_bars_render_percentages_and_only_animate_without_a_value() {
        let context = egui::Context::default();
        let bounds = egui::Rect::from_min_size(Pos2::new(20.0, 30.0), Vec2::new(200.0, 100.0));
        for (state, value, width, animated) in [
            (1, Some(40), 78.4, false),
            (2, Some(75), 147.0, false),
            (4, None, 196.0, false),
            (3, None, 49.0, true),
            (2, None, 49.0, true),
        ] {
            let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                assert_eq!(
                    paint_progress(
                        ui,
                        1,
                        bounds,
                        Progress { state, value },
                        Color32::BLUE,
                        0.0,
                        None
                    ),
                    animated,
                );
            });
            output.textures_delta.clear();
            let fill = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) => Some(rect),
                    _ => None,
                })
                .next_back()
                .unwrap();
            assert_eq!(
                fill.rect,
                egui::Rect::from_min_size(bounds.min + Vec2::splat(2.0), Vec2::new(width, 2.0))
            );
            assert_eq!(
                fill.fill,
                match state {
                    2 => Color32::from_rgb(255, 59, 48),
                    4 => Color32::from_rgb(255, 149, 0),
                    _ => Color32::BLUE,
                }
            );
        }
    }

    #[test]
    fn unshared_directory_badges_use_large_type_when_it_fits_and_stay_top_right() {
        let context = egui::Context::default();
        configure_ui_fonts(&context);
        for (name, size, font_size, pinned) in [
            ("gitime", Vec2::new(740.0, 330.0), 22.0 * 0.7, false),
            ("timesheets", Vec2::new(370.0, 330.0), 22.0 * 0.7, false),
            // Exceed the available width with both Segoe UI and San Francisco.
            (
                "gitoxide.stable-gix-with-a-long-directory-name",
                Vec2::new(180.0, 200.0),
                12.0,
                false,
            ),
            ("gitime", Vec2::new(740.0, 38.0), 12.0, false),
            ("gitime", Vec2::new(180.0, 200.0), 22.0 * 0.7, true),
            (
                "gitoxide.stable-gix-with-a-long-directory-name",
                Vec2::new(180.0, 200.0),
                12.0,
                true,
            ),
        ] {
            let bounds = egui::Rect::from_min_size(Pos2::new(100.0, 50.0), size);
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(bounds),
                    ..Default::default()
                },
                |ui| {
                    DirectoryBadge {
                        label: DirectoryLabel::new(Some(name.into()), false, true, false).unwrap(),
                        bounds,
                        centered: false,
                        shared: false,
                        active: true,
                        attention: true,
                        pinned,
                        flash: 0.0,
                        accent: TabAccent::new(None, [230, 185, 60]),
                    }
                    .paint(ui);
                },
            );
            output.textures_delta.clear();
            let text = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text),
                    _ => None,
                })
                .unwrap();
            let format = &text.galley.job.sections[0].format;
            assert_eq!(format.font_id.size, font_size, "{name} in {size:?}");
            assert_eq!(format.underline.width, 1.0);
            if font_size > 12.0 {
                assert!(!text.galley.elided);
            }
            let badge = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) => Some(rect.rect),
                    _ => None,
                })
                .unwrap();
            assert_eq!(badge.right(), bounds.right() - 8.0);
            assert_eq!(badge.top(), bounds.top() + 8.0);
            assert!(bounds.contains_rect(badge));
            let padding = if font_size > 12.0 {
                Vec2::new(16.0, 10.0) * 0.7
            } else {
                Vec2::new(8.0, 4.0)
            };
            let pin_offset = Vec2::new(if pinned { 20.0 } else { 0.0 }, 0.0);
            assert!((text.pos - badge.min - padding - pin_offset).length() < 0.01);
        }
    }

    #[test]
    fn directory_badges_are_centered_and_readable_with_the_native_font() {
        let context = egui::Context::default();
        configure_ui_fonts(&context);
        let bounds = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 240.0));
        let accent = TabAccent::new(Some([230, 185, 60]), [0; 3]);
        for (selected, window_focused, shared) in [
            (false, true, true),
            (true, true, true),
            (true, false, true),
            (true, true, false),
            (true, false, false),
            (false, true, false),
        ] {
            for theme in [egui::ThemePreference::Dark, egui::ThemePreference::Light] {
                context.set_theme(theme);
                let mut output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(bounds),
                        ..Default::default()
                    },
                    |ui| {
                        let label = DirectoryLabel::new(
                            Some("rustty".into()),
                            selected,
                            window_focused,
                            true,
                        )
                        .unwrap();
                        DirectoryBadge {
                            centered: label.large(true),
                            shared,
                            label,
                            bounds,
                            active: false,
                            attention: false,
                            pinned: false,
                            flash: 0.0,
                            accent,
                        }
                        .paint(ui);
                    },
                );
                output.textures_delta.clear();
                let text = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) => Some(text),
                        _ => None,
                    })
                    .unwrap();
                assert!((text.pos + text.galley.size() / 2.0 - bounds.center()).length() < 1.0);
                assert_eq!(
                    text.galley.job.sections[0].format.font_id.size,
                    if shared { 22.0 } else { 22.0 * 0.7 }
                );
                assert_eq!(
                    text.galley.job.sections[0].format.color,
                    if selected && window_focused || theme == egui::ThemePreference::Light {
                        Color32::BLACK
                    } else {
                        Color32::WHITE
                    }
                );
                let badge = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Rect(rect) => Some(rect),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(badge.rect.center(), bounds.center());
                assert_eq!(
                    badge.fill,
                    if selected && window_focused {
                        Color32::from_rgb(230, 185, 60)
                    } else {
                        context.global_style().visuals.widgets.inactive.bg_fill
                    }
                );
            }
        }
    }

    #[test]
    fn singleton_directory_badges_keep_their_size_and_position_on_focus() {
        let context = egui::Context::default();
        configure_ui_fonts(&context);
        let bounds = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 240.0));
        let mut original = None;
        for (selected, window_focused) in [(false, true), (true, true), (true, false)] {
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(bounds),
                    ..Default::default()
                },
                |ui| {
                    DirectoryBadge {
                        label: DirectoryLabel::new(
                            Some("rustty".into()),
                            selected,
                            window_focused,
                            true,
                        )
                        .unwrap(),
                        bounds,
                        centered: true,
                        shared: true,
                        active: false,
                        attention: false,
                        pinned: false,
                        flash: 0.0,
                        accent: TabAccent::new(Some([230, 185, 60]), [0; 3]),
                    }
                    .paint(ui);
                },
            );
            output.textures_delta.clear();
            let text = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text),
                    _ => None,
                })
                .unwrap();
            let badge = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) => Some(rect.rect),
                    _ => None,
                })
                .unwrap();
            assert_eq!(text.galley.job.sections[0].format.font_id.size, 22.0);
            assert_eq!(badge.center(), bounds.center());
            let geometry = (badge, text.pos, text.galley.size());
            assert_eq!(*original.get_or_insert(geometry), geometry);
        }
    }

    #[test]
    fn saved_layout_picker_opens_the_selected_path_and_cancels() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let path = PathBuf::from("/tmp/saved-rustty-workspace.json");
        let mut picker = LayoutPicker {
            choices: vec![workspace::SavedLayout {
                name: "Rustty".into(),
                path: path.clone(),
                available: true,
            }],
            path: path.display().to_string(),
            error: None,
        };
        let mut frame = |events| {
            let mut command = None;
            let mut raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    Vec2::new(800.0, 600.0),
                )),
                focused: true,
                events,
                ..Default::default()
            };
            input::filter_egui_events(&mut raw, true);
            let mut output = context.run_ui(raw, |_| {
                command = show_layout_picker(&context, &mut picker).or(command.take());
            });
            let open = output
                .platform_output
                .accesskit_update
                .as_ref()
                .and_then(|update| {
                    update.nodes.iter().find_map(|(_, node)| {
                        if node.label() != Some("Open") {
                            return None;
                        }
                        let rect = node.bounds()?;
                        Some(Pos2::new(
                            ((rect.x0 + rect.x1) / 2.0) as f32,
                            ((rect.y0 + rect.y1) / 2.0) as f32,
                        ))
                    })
                });
            output.textures_delta.clear();
            (command, open)
        };
        frame(vec![]);
        let point = frame(vec![]).1.expect("accessible Open button");
        let mut command = None;
        for pressed in [true, false] {
            command = frame(vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                },
            ])
            .0;
        }
        assert!(matches!(command, Some(LayoutCommand::Open(selected)) if selected == path));
        let command = frame(vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }])
        .0;
        assert!(matches!(command, Some(LayoutCommand::Close)));
        let command = frame(vec![egui::Event::Key {
            key: egui::Key::OpenBracket,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        }])
        .0;
        assert!(matches!(command, Some(LayoutCommand::Close)));
    }

    #[test]
    fn messages_take_keyboard_input_only_after_explicit_focus() {
        fn frame(
            context: &egui::Context,
            errors: &mut Vec<String>,
            events: Vec<egui::Event>,
        ) -> (Option<egui::Response>, egui::PlatformOutput) {
            let ui_input = !errors.is_empty() && messages_input(context);
            let mut raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    Vec2::new(800.0, 600.0),
                )),
                focused: true,
                events,
                ..Default::default()
            };
            input::filter_egui_events(&mut raw, ui_input);
            let mut dismiss = None;
            let mut output = context.run_ui(raw, |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    let terminal = ui.interact(
                        ui.max_rect(),
                        egui::Id::new("terminal"),
                        Sense::click_and_drag(),
                    );
                    input::terminal_input(&terminal, None, ui_input);
                });
                dismiss = show_messages(context, errors, ui_input);
            });
            output.textures_delta.clear();
            (dismiss, output.platform_output)
        }

        fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
            egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }
        }

        fn click(context: &egui::Context, errors: &mut Vec<String>, point: Pos2) {
            for pressed in [true, false] {
                frame(
                    context,
                    errors,
                    vec![
                        egui::Event::PointerMoved(point),
                        egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
        }

        for dismissal in ["click", "escape", "control-bracket", "enter"] {
            let context = egui::Context::default();
            context.enable_accesskit();
            let mut errors = vec![];
            frame(&context, &mut errors, vec![]);
            for event in [
                key(egui::Key::A, egui::Modifiers::NONE),
                key(egui::Key::Escape, egui::Modifiers::NONE),
                key(egui::Key::OpenBracket, egui::Modifiers::CTRL),
            ] {
                errors.push("sending on a closed channel".to_owned());
                frame(&context, &mut errors, vec![event]);
                assert!(!messages_input(&context));
                assert!(context.memory(|memory| memory.has_focus(egui::Id::new("terminal"))));
            }
            assert_eq!(errors.len(), 3, "terminal keys must not dismiss messages");
            errors.truncate(1);
            let (dismiss, output) = frame(&context, &mut errors, vec![]);
            let label = output
                .accesskit_update
                .unwrap()
                .nodes
                .into_iter()
                .find_map(|(_, node)| {
                    (node.role() == egui::accesskit::Role::Label
                        && node.value() == Some(errors[0].as_str()))
                    .then(|| node.bounds())
                    .flatten()
                })
                .expect("accessible message label");
            let label = Pos2::new(
                ((label.x0 + label.x1) / 2.0) as f32,
                ((label.y0 + label.y1) / 2.0) as f32,
            );
            click(&context, &mut errors, label);
            assert!(
                messages_input(&context),
                "message click must allow copying: {:?}",
                context
                    .memory(egui::Memory::focused)
                    .and_then(|id| context.read_response(id))
                    .map(|r| (r.id, r.layer_id))
            );
            frame(
                &context,
                &mut errors,
                vec![key(egui::Key::A, egui::Modifiers::COMMAND)],
            );
            let (_, output) = frame(&context, &mut errors, vec![egui::Event::Copy]);
            assert!(
                output
                    .commands
                    .contains(&egui::OutputCommand::CopyText(errors[0].clone()))
            );

            click(&context, &mut errors, Pos2::new(750.0, 550.0));
            frame(
                &context,
                &mut errors,
                vec![key(egui::Key::A, egui::Modifiers::NONE)],
            );
            assert!(!messages_input(&context));
            assert!(context.memory(|memory| memory.has_focus(egui::Id::new("terminal"))));
            assert_eq!(errors.len(), 1);
            click(&context, &mut errors, label);
            assert!(messages_input(&context));

            let dismiss = dismiss.unwrap();
            match dismissal {
                "click" => click(&context, &mut errors, dismiss.rect.center()),
                "escape" => {
                    frame(
                        &context,
                        &mut errors,
                        vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
                    );
                }
                "control-bracket" => {
                    frame(
                        &context,
                        &mut errors,
                        vec![key(egui::Key::OpenBracket, egui::Modifiers::CTRL)],
                    );
                }
                "enter" => {
                    frame(
                        &context,
                        &mut errors,
                        vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
                    );
                    assert!(context.memory(|memory| memory.has_focus(dismiss.id)));
                    frame(
                        &context,
                        &mut errors,
                        vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
                    );
                }
                _ => unreachable!(),
            }
            assert!(errors.is_empty(), "dismiss by {dismissal}");
            assert!(frame(&context, &mut errors, vec![]).0.is_none());
            assert!(context.memory(|memory| memory.has_focus(egui::Id::new("terminal"))));
        }
    }

    #[test]
    fn appearance_selects_dual_themes_and_reload_preserves_cli_overrides() {
        let directory = std::env::temp_dir().join(format!(
            "rustty-theme-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let own = directory.join("config/rustty");
        std::fs::create_dir_all(own.join("themes")).unwrap();
        let source = "theme = light:Day,dark:Night\nfont-size = 12\n";
        std::fs::write(own.join("config.rustty"), source).unwrap();
        std::fs::write(own.join("themes/Day"), "background = ffffff\n").unwrap();
        std::fs::write(own.join("themes/Night"), "background = 101010\n").unwrap();
        let mut loader = config::ConfigLoader {
            home: directory.clone(),
            app_config_home: None,
            xdg_config_home: directory.join("config"),
            resources_dir: None,
            dark_mode: true,
            working_directory: directory.clone(),
        };
        let args = [
            "--font-size=19",
            "--window-theme=light",
            "-e",
            "/bin/echo",
            "two words",
        ]
        .map(str::to_owned);
        let light = load_config(&mut loader, &args, Some(Theme::Light));
        assert!(light.diagnostics.is_empty(), "{:?}", light.diagnostics);
        assert_eq!(light.config.background, config::Rgb::new(255, 255, 255));
        assert_eq!(
            color_scheme(loader.dark_mode),
            vt::query::ColorScheme::Light
        );
        let dark = load_config(&mut loader, &args, Some(Theme::Dark));
        assert_eq!(dark.config.background, config::Rgb::new(16, 16, 16));
        assert_eq!(color_scheme(loader.dark_mode), vt::query::ColorScheme::Dark);
        let reloaded = load_config(&mut loader, &args, None);
        assert_eq!(reloaded.config.background, dark.config.background);
        assert_eq!(reloaded.config.font_size, 19.0);
        assert_eq!(ui_theme(&reloaded.config), egui::ThemePreference::Light);
        assert_eq!(
            reloaded.config.initial_command,
            Some(config::Command::Direct(vec![
                "/bin/echo".into(),
                "two words".into()
            ]))
        );
        assert_eq!(
            std::fs::read_to_string(own.join("config.rustty")).unwrap(),
            source
        );
        assert!(!directory.join("Library").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn last_window_close_accounts_for_quick_terminals_tabs_and_splits() {
        let mut workspace = Workspace::default();
        workspace.windows.push(WindowState {
            id: 100,
            tabs: vec![Tab::new(10, 1, PathBuf::from("/tmp"))],
            active_tab: 0,
            frame: [0.0, 0.0, 800.0, 600.0],
            quick: false,
        });
        for action in [Action::CloseWindow, Action::CloseTab, Action::CloseSurface] {
            assert!(closes_last_window(&workspace, 100, &action));
        }
        let mut second = workspace.windows[0].clone();
        second.id = 200;
        second.tabs = vec![Tab::new(20, 2, PathBuf::from("/tmp"))];
        workspace.windows.push(second);
        for action in [Action::CloseWindow, Action::CloseTab, Action::CloseSurface] {
            assert!(!closes_last_window(&workspace, 100, &action));
        }
        workspace.windows[1].quick = true;
        let quits_with_quick = cfg!(target_os = "windows");
        for action in [Action::CloseWindow, Action::CloseTab, Action::CloseSurface] {
            assert_eq!(
                closes_last_window(&workspace, 100, &action),
                quits_with_quick
            );
        }
        assert!(!closes_last_window(&workspace, 200, &Action::CloseWindow));
        workspace.windows[0]
            .tabs
            .push(Tab::new(30, 3, PathBuf::from("/tmp")));
        assert!(!closes_last_window(&workspace, 100, &Action::CloseTab));
        assert!(!closes_last_window(&workspace, 100, &Action::CloseSurface));
        workspace.windows[0].tabs.pop();
        workspace.windows[0].tabs[0]
            .root
            .split(1, 3, 4, Direction::Right);
        assert!(!closes_last_window(&workspace, 100, &Action::CloseSurface));
        assert_eq!(
            closes_last_window(&workspace, 100, &Action::CloseTab),
            quits_with_quick
        );
        assert_eq!(
            closes_last_window(&workspace, 100, &Action::CloseWindow),
            quits_with_quick
        );
    }

    #[test]
    fn local_tabs_start_lazily_but_tmuxx_attaches_every_pane() {
        let mut first = Tab::new(10, 1, PathBuf::from("/tmp"));
        first.split(2, 3, Direction::Right, PathBuf::from("/tmp"));
        let mut window = WindowState {
            id: 100,
            tabs: vec![first, Tab::new(20, 4, PathBuf::from("/tmp"))],
            active_tab: 0,
            frame: [0.0, 0.0, 800.0, 600.0],
            quick: false,
        };
        assert_eq!(
            startup_panes(&window, false)
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(
            startup_panes(&window, true)
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            [1, 2, 4]
        );
        window.active_tab = 1;
        assert_eq!(
            startup_panes(&window, false)
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            [4]
        );
    }

    #[test]
    fn visibility_follows_native_window_active_tab_zoom_and_peek() {
        let mut first = Tab::new(10, 1, PathBuf::from("/tmp"));
        first.root.split(1, 2, 3, Direction::Right);
        let mut window = WindowState {
            id: 100,
            tabs: vec![first, Tab::new(20, 4, PathBuf::from("/tmp"))],
            active_tab: 0,
            frame: [0.0, 0.0, 800.0, 600.0],
            quick: false,
        };
        assert_eq!(window_visible_panes(&window, true, false, false), [1, 2]);
        assert_eq!(initial_visible_panes(&window, None), [1, 2]);
        window.active_tab = 1;
        assert_eq!(window_visible_panes(&window, true, false, false), [4]);
        window.active_tab = 0;
        window.tabs[0].zoom = Some(1);
        assert_eq!(window_visible_panes(&window, true, false, false), [1]);
        assert_eq!(initial_visible_panes(&window, None), [1]);
        assert_eq!(window_visible_panes(&window, true, false, true), [1, 2]);
        assert!(window_visible_panes(&window, true, true, true).is_empty());
        assert!(initial_visible_panes(&window, Some((true, true, false))).is_empty());
        window.quick = true;
        assert!(window_visible_panes(&window, false, false, true).is_empty());
        assert!(initial_visible_panes(&window, None).is_empty());
        assert_eq!(
            initial_visible_panes(&window, Some((true, false, true))),
            [1, 2]
        );
    }

    #[test]
    fn software_animation_requests_are_paced_without_delaying_overdue_frames() {
        let now = Instant::now();
        let next = software_repaint_deadline(now, Some(now), Some(50_000));
        assert_eq!(next, now + Duration::from_millis(20));
        assert_eq!(software_repaint_deadline(now, None, Some(50_000)), now);
        let overdue = now + Duration::from_millis(30);
        assert_eq!(
            software_repaint_deadline(overdue, Some(now), Some(50_000)),
            overdue
        );
        let fallback = software_repaint_deadline(now, Some(now), None);
        assert_eq!(fallback, software_repaint_deadline(now, Some(now), Some(0)));
        assert_eq!(
            fallback,
            software_repaint_deadline(now, Some(now), Some(u32::MAX))
        );
        assert!((fallback - now).as_secs_f64() > 0.016);
        assert!((fallback - now).as_secs_f64() < 0.017);
    }

    #[test]
    fn repaint_callbacks_only_survive_their_own_pass_and_its_follow_up() {
        let context = egui::Context::default();
        let viewport = ViewportId::from_hash_of("terminal");
        let mut requests = Vec::new();
        for _ in 0..8 {
            requests.push(context.cumulative_pass_nr_for(viewport));
            let mut input = egui::RawInput {
                viewport_id: viewport,
                ..Default::default()
            };
            input.viewports.entry(viewport).or_default();
            let mut output = context.run_ui(input, |_| {});
            output.textures_delta.clear();
        }
        let current = context.cumulative_pass_nr_for(viewport);
        assert_eq!(
            requests
                .into_iter()
                .filter(|&pass| repaint_is_current(pass, current))
                .count(),
            1,
            "queued callbacks must not each render another unchanged frame"
        );
        assert!(repaint_is_current(current, current));
        assert!(!repaint_is_current(current + 1, current));
        assert!(!repaint_is_current(u64::MAX, 0));
    }
    #[test]
    fn clipboard_grants_and_metadata_queries_respect_explicit_denial() {
        use config::ClipboardAccess::{Allow, Ask, Deny};
        use vt::clipboard::{Location, Read, Terminator, Write};
        let mut config = Config::default();
        let read = Read::osc52(Location::Standard, Terminator::St);
        assert_eq!(
            clipboard_policy(&config, &vt::Effect::ClipboardRead(read.clone())),
            Ask
        );
        for metadata_only in [false, true] {
            let mut read = read.clone();
            if metadata_only {
                read.mimes.clear();
            } else {
                read.granted = true;
            }
            config.clipboard_read = Ask;
            assert_eq!(
                clipboard_policy(&config, &vt::Effect::ClipboardRead(read.clone())),
                Allow
            );
            config.clipboard_read = Deny;
            assert_eq!(
                clipboard_policy(&config, &vt::Effect::ClipboardRead(read)),
                Deny
            );
        }
        let mut write = Write::osc52(Location::Standard, Vec::new());
        write.granted = true;
        config.clipboard_write = Ask;
        assert_eq!(
            clipboard_policy(&config, &vt::Effect::ClipboardWrite(write.clone())),
            Allow
        );
        config.clipboard_write = Deny;
        assert_eq!(
            clipboard_policy(&config, &vt::Effect::ClipboardWrite(write)),
            Deny
        );
    }

    #[test]
    fn paste_confirmation_follows_protection_bracketed_trust_and_payload() {
        for bracketed in [false, true] {
            let mut terminal = vt::Terminal::new(20, 2, 0);
            if bracketed {
                terminal.feed(b"\x1b[?2004h");
            }
            for protection in [false, true] {
                for trust_brackets in [false, true] {
                    let mut config = Config::default();
                    config.clipboard_paste_protection = protection;
                    config.clipboard_paste_bracketed_safe = trust_brackets;
                    for data in [b"".as_slice(), b"plain text", "日誌".as_bytes()] {
                        assert!(!paste_needs_confirmation(&config, Some(&terminal), data));
                        assert!(!paste_needs_confirmation(&config, None, data));
                    }
                    for data in [b"one\ntwo".as_slice(), b"one\rtwo", b"one\r\ntwo"] {
                        assert_eq!(
                            paste_needs_confirmation(&config, Some(&terminal), data),
                            protection && !(bracketed && trust_brackets)
                        );
                        assert_eq!(paste_needs_confirmation(&config, None, data), protection);
                    }
                    assert_eq!(
                        paste_needs_confirmation(&config, Some(&terminal), b"one\x1b[201~two"),
                        protection
                    );
                }
            }
        }
    }

    #[test]
    fn kitty_paste_lists_without_payload_reads_and_grants_still_respect_explicit_denial() {
        use base64::Engine;
        for location in [
            vt::clipboard::Location::Standard,
            vt::clipboard::Location::Selection,
        ] {
            let request = paste_listing_request(location);
            assert!(request.list && request.mimes.is_empty());
            assert_eq!(request.location, location);
            let mimes = [b"text/plain".to_vec(), b"image/png".to_vec()];
            let mut terminal = vt::Terminal::new(20, 2, 0);
            let mut output = Vec::new();
            assert!(
                !emit_paste_event(
                    &mut terminal,
                    location,
                    &mimes,
                    &mut |_| panic!("text fallback must not generate a grant"),
                    &mut output
                )
                .unwrap()
            );
            assert!(output.is_empty());
            terminal.feed(b"\x1b[?5522;2004h");
            let mut random = |bytes: &mut [u8]| {
                bytes.fill(0);
                Ok(())
            };
            // The host's event reader fails if called: success requires MIME-only handling.
            assert!(
                emit_paste_event(&mut terminal, location, &mimes, &mut random, &mut output)
                    .unwrap()
            );
            let event = String::from_utf8(output).unwrap();
            assert!(event.starts_with("\x1b]5522;type=read:status=OK"));
            assert_eq!(
                event.contains(":loc=primary"),
                location == vt::clipboard::Location::Selection
            );
            let password = base64::engine::general_purpose::STANDARD.encode([b'2'; 22]);
            let request =
                format!("\x1b]5522;type=read:name=cHJvZ3JhbQ==:pw={password};dGV4dC9wbGFpbg==\x07");
            let read = terminal
                .feed(request.as_bytes())
                .into_iter()
                .find_map(|effect| match effect {
                    vt::Effect::ClipboardRead(read) => Some(read),
                    _ => None,
                })
                .expect("follow-up clipboard read");
            assert!(read.granted);
            let mut config = Config::default();
            let effect = vt::Effect::ClipboardRead(read);
            assert_eq!(
                clipboard_policy(&config, &effect),
                config::ClipboardAccess::Allow
            );
            config.clipboard_read = config::ClipboardAccess::Deny;
            assert_eq!(
                clipboard_policy(&config, &effect),
                config::ClipboardAccess::Deny
            );
            terminal.feed(b"\x1b[?5522l");
            let mut output = Vec::new();
            assert!(
                !emit_paste_event(
                    &mut terminal,
                    location,
                    &mimes,
                    &mut |_| panic!("mode changed while native types were listed"),
                    &mut output
                )
                .unwrap()
            );
            assert!(output.is_empty());
        }
    }

    #[test]
    fn pending_paste_keeps_its_destination_when_tabs_change_and_rejects_closed_panes() {
        let paste = PendingPaste {
            pane: 1,
            data: b"one\ntwo".to_vec(),
        };
        let mut window = WindowState {
            id: 100,
            tabs: vec![
                Tab::new(10, 1, PathBuf::from("/tmp")),
                Tab::new(20, 2, PathBuf::from("/tmp")),
            ],
            active_tab: 0,
            frame: [0.0, 0.0, 800.0, 600.0],
            quick: false,
        };
        assert!(window_contains_pane(&window, paste.pane));
        window.active_tab = 1;
        assert!(window_contains_pane(&window, paste.pane));
        window.tabs.remove(0);
        assert!(!window_contains_pane(&window, paste.pane));
    }
    #[test]
    fn shell_exit_policy_holds_fast_failures_and_respects_wait_setting() {
        let mut config = Config::default();
        assert!(hold_after_exit(&config, Duration::from_millis(250)));
        assert!(!hold_after_exit(&config, Duration::from_secs(1)));
        config.wait_after_command = true;
        assert!(hold_after_exit(&config, Duration::from_secs(100)));
        config.wait_after_command = false;
        config.abnormal_command_exit_runtime = 0;
        assert!(!hold_after_exit(&config, Duration::from_millis(1)));
    }
    #[test]
    fn cwd_reports_become_absolute_paths_before_restoration() {
        assert_eq!(
            directory_from_osc("file://host/Users/me/My%20Project"),
            Some(PathBuf::from("/Users/me/My Project"))
        );
        assert_eq!(directory_from_osc("/tmp"), Some(PathBuf::from("/tmp")));
        for invalid in [
            "file://host",
            "file://host/%0",
            "file://host/%00bad",
            "https://host/tmp",
            "relative",
        ] {
            assert_eq!(directory_from_osc(invalid), None);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cursor_candidate_matches_only_ignore_the_blink_phase() {
        let terminal = vt::Terminal::new(12, 3, 0);
        let on = CursorCandidate {
            pane: 1,
            key: PaneRenderKey::new(
                &terminal,
                RenderOptions {
                    size: [320, 160],
                    ..Default::default()
                },
                egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 160.0)),
                1.0,
            ),
            frame: Arc::new(Frame::empty([320, 160])),
        };
        let off = || {
            let mut key = on.key.clone();
            key.options.blink_visible = false;
            CursorCandidate {
                pane: on.pane,
                key,
                frame: Arc::clone(&on.frame),
            }
        };
        assert!(on.key.options.blink_visible);
        assert!(on.matches(&off()) && off().matches(&on));
        type Mutation = (&'static str, fn(&mut CursorCandidate));
        let mutations: &[Mutation] = &[
            ("pane", |c| c.pane += 1),
            ("terminal", |c| c.key.generation += 1),
            ("scroll", |c| c.key.viewport_offset += 1),
            ("cursor position", |c| c.key.cursor.0 += 1),
            ("cursor shape", |c| {
                c.key.cursor.2 = vt::screen::CursorShape::Bar
            }),
            ("cursor visibility", |c| c.key.cursor.3 = !c.key.cursor.3),
            ("cursor blinking", |c| c.key.cursor.4 = !c.key.cursor.4),
            ("selection", |c| {
                let point = vt::GridPoint { row: 1, col: 0 };
                c.key.selection = Some(vt::Selection {
                    start: point,
                    end: point,
                    rectangular: false,
                });
            }),
            ("geometry", |c| {
                c.key.rect = c.key.rect.translate(Vec2::new(1.0, 0.0))
            }),
            ("scale", |c| c.key.scale = 2.0),
            ("size", |c| c.key.options.size[0] += 1),
            ("focus", |c| c.key.options.focused = !c.key.options.focused),
            ("opacity", |c| c.key.options.background_opacity = 0.5),
            ("palette", |c| c.key.options.palette[1][0] ^= 1),
            ("preedit", |c| {
                c.key.options.preedit = Some(rustty_render::Preedit {
                    text: "x".into(),
                    selection: None,
                });
            }),
            ("atlas generation", |c| {
                Arc::make_mut(&mut c.frame).generation += 1
            }),
        ];
        for (name, mutate) in mutations {
            let mut changed = off();
            mutate(&mut changed);
            assert!(
                !on.matches(&changed),
                "{name} must invalidate the retained cursor"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cursor_patch_reconstructs_split_frames_and_rejects_overlapping_panes() {
        fn raster(frame: Frame) -> Vec<u8> {
            let rect = egui::Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(frame.size[0] as f32, frame.size[1] as f32),
            );
            rustty_render_software::Renderer::default()
                .render(
                    frame.size,
                    1.0,
                    &[egui::ClippedPrimitive {
                        clip_rect: rect,
                        primitive: egui::epaint::Primitive::Callback(egui::PaintCallback {
                            rect,
                            callback: Arc::new(rustty_render_software::TerminalPaint {
                                window: 1,
                                frame: Arc::new(frame),
                            }),
                        }),
                    }],
                    &egui::TexturesDelta::default(),
                )
                .unwrap()
                .to_vec()
        }
        let mut fonts = rustty_render::Renderer::new(font_config(&Config::default(), 2.0)).unwrap();
        let mut terminal = vt::Terminal::new(12, 3, 0);
        terminal.feed(b"top row\r\n\x1b[3moffice AV\x1b[0m\r\nbottom row\x1b[2;3H");
        for shape in [1, 3, 5] {
            terminal.feed(format!("\x1b[{shape} q").as_bytes());
            let mut candidate = |blink_visible| {
                let options = RenderOptions {
                    size: [320, 160],
                    padding: [6.0, 8.0],
                    focused: true,
                    cursor_visible: true,
                    blink_visible,
                    background: [20, 30, 40],
                    cursor_color: [200, 100, 20],
                    cursor_text: [20, 40, 60],
                    ..Default::default()
                };
                CursorCandidate {
                    pane: 1,
                    key: PaneRenderKey::new(
                        &terminal,
                        options.clone(),
                        egui::Rect::from_min_size(Pos2::new(176.0, 16.0), Vec2::new(160.0, 80.0)),
                        2.0,
                    ),
                    frame: Arc::new(fonts.prepare(terminal.screen(), &options).unwrap()),
                }
            };
            let on = candidate(true);
            let off = candidate(false);
            let pane = |frame: &Arc<Frame>| ComposedPane {
                frame: Arc::clone(frame),
                rect: [352.0, 32.0, 320.0, 160.0],
                clip: [354.0, 32.0, 300.0, 160.0],
                dim: None,
                transition: None,
            };
            let neighbor = Arc::new((*off.frame).clone());
            let panes = |frame: &Arc<Frame>| {
                let neighbor = |x| ComposedPane {
                    frame: Arc::clone(&neighbor),
                    rect: [x, 32.0, 320.0, 160.0],
                    clip: [x, 32.0, 320.0, 160.0],
                    dim: Some(rustty_render::Color::rgb([10, 20, 30]).opacity(0.4)),
                    transition: None,
                };
                vec![neighbor(24.0), pane(frame), neighbor(680.0)]
            };
            let expected = ComposedFrame::new([1024, 224], panes(&on.frame)).unwrap();
            let base = ComposedFrame::new([1024, 224], panes(&off.frame)).unwrap();
            let (pixels, patch) = on.patch(&off, &base).unwrap();
            assert!(pixels[0] >= 354 && pixels[1] >= 32 && pixels[3] < 160);
            assert!(off.patch(&on, &expected).is_none());
            assert!(
                on.patch(&off, &expected).is_none(),
                "the OFF frame must be composed"
            );
            let mut fractional = panes(&off.frame);
            fractional[1].clip[0] = pixels[0] as f32 + 0.5;
            let fractional = ComposedFrame::new([1024, 224], fractional).unwrap();
            assert!(on.patch(&off, &fractional).is_none());
            let expected = raster((*expected.frame).clone());
            assert!(
                expected != raster((*base.frame).clone()),
                "cursor must change pixels"
            );
            let mut restored = (*base.frame).clone();
            restored
                .append_clipped(
                    &patch,
                    [pixels[0] as f32, pixels[1] as f32],
                    pixels.map(|v| v as f32),
                )
                .unwrap();
            let restored = raster(restored);
            assert_eq!(
                expected.iter().zip(&restored).position(|(a, b)| a != b),
                None,
                "cursor style {shape}: the retained patch must reproduce every pixel",
            );
            for index in [0, 2] {
                let mut overlapping = panes(&off.frame);
                overlapping[index].rect = [pixels[0] as f32 + 1.0, pixels[1] as f32, 320.0, 160.0];
                overlapping[index].clip = [pixels[0] as f32 + 1.0, pixels[1] as f32, 10.0, 10.0];
                let mut overlapping = ComposedFrame::new([1024, 224], overlapping).unwrap();
                assert!(
                    on.patch(&off, &overlapping).is_none(),
                    "overlapping pane {index}"
                );
                // A pane can overlap the cursor row while its visible clip stays outside it.
                overlapping.panes[index].clip[1] += pixels[3] as f32 + 1.0;
                assert!(
                    on.patch(&off, &overlapping).is_some(),
                    "clipped pane {index}"
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cursor_patch_checks_visible_triangles_after_the_terminal_callback() {
        use egui::epaint::{Mesh, Primitive, Vertex};
        let terminal = || egui::ClippedPrimitive {
            clip_rect: egui::Rect::EVERYTHING,
            primitive: Primitive::Callback(egui::PaintCallback {
                rect: egui::Rect::EVERYTHING,
                callback: Arc::new(()),
            }),
        };
        let triangle = |color, clip_rect| egui::ClippedPrimitive {
            clip_rect,
            primitive: Primitive::Mesh(Mesh {
                vertices: [
                    Pos2::new(10.0, 20.0),
                    Pos2::new(30.0, 20.0),
                    Pos2::new(10.0, 30.0),
                ]
                .map(|pos| Vertex {
                    pos,
                    uv: Pos2::ZERO,
                    color,
                })
                .to_vec(),
                indices: vec![0, 1, 2],
                ..Default::default()
            }),
        };
        let patch = [20, 40, 40, 20];
        let visible = || triangle(egui::Color32::WHITE, egui::Rect::EVERYTHING);
        assert!(!cursor_patch_uncovered(&[], patch, 2.0));
        assert!(cursor_patch_uncovered(&[terminal()], patch, 2.0));
        assert!(cursor_patch_uncovered(&[visible(), terminal()], patch, 2.0));
        assert!(!cursor_patch_uncovered(
            &[terminal(), visible()],
            patch,
            2.0
        ));
        assert!(!cursor_patch_uncovered(
            &[terminal(), terminal()],
            patch,
            2.0
        ));
        assert!(cursor_patch_uncovered(
            &[
                terminal(),
                triangle(egui::Color32::TRANSPARENT, egui::Rect::EVERYTHING)
            ],
            patch,
            2.0,
        ));
        assert!(cursor_patch_uncovered(
            &[
                terminal(),
                triangle(
                    egui::Color32::WHITE,
                    egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(9.0, 19.0))
                )
            ],
            patch,
            2.0,
        ));
        let mut outside = visible();
        let Primitive::Mesh(mesh) = &mut outside.primitive else {
            unreachable!()
        };
        for vertex in &mut mesh.vertices {
            vertex.pos.x += 20.0;
        }
        // Unused vertices must not make an otherwise disjoint UI mesh cover the patch.
        mesh.vertices.push(Vertex {
            pos: Pos2::new(15.0, 25.0),
            uv: Pos2::ZERO,
            color: egui::Color32::WHITE,
        });
        assert!(cursor_patch_uncovered(&[terminal(), outside], patch, 2.0));
    }
}
