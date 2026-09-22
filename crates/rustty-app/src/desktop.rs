#[path = "smoke.rs"]
mod smoke;
use egui::{Color32, Pos2, Sense, Vec2, ViewportId};
use rustty::{
    config::{self, Action, Config, Direction, LoadedConfig},
    session::{Session, SessionEvent, SessionOptions},
    vt,
};
use rustty_app::{
    accessibility::TerminalText,
    input,
    platform::{Platform, PlatformEvent},
    presentation::{
        Activity, CompletionFlash, DirectoryLabel, FocusHint, Progress, TabAccent,
        common_directory_name, cursor_blink_phase, directory_name,
    },
    search::{self, Search},
    workspace::{self, Axis, Id, Peek, Rect, SavedPane, Tab, WindowState, Workspace},
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
    },
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalPosition, LogicalSize},
    event::{ElementState, Ime, Modifiers, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::PhysicalKey,
    platform::macos::WindowAttributesExtMacOS,
    window::{CursorIcon, Fullscreen, Theme, Window, WindowId},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const INPUT_BUDGET: usize = 16 * 1024 * 1024;
const MIN_WINDOW_SIZE: LogicalSize<f64> = LogicalSize::new(240.0, 120.0);

#[derive(Debug)]
enum Event {
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

struct Pane {
    session: Session,
    host_state: (bool, bool, vt::query::ColorScheme),
    wake_pending: Arc<AtomicBool>,
    input: VecDeque<Vec<u8>>,
    input_bytes: usize,
    title: String,
    title_override: Option<String>,
    cwd: PathBuf,
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
impl Pane {
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

    fn write(&mut self, bytes: Vec<u8>) -> std::io::Result<()> {
        QueuedInput {
            session: &self.session,
            queue: &mut self.input,
            bytes: &mut self.input_bytes,
        }
        .enqueue(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        while let Some(bytes) = self.input.front() {
            match self.session.write(bytes) {
                Ok(()) => {
                    self.input_bytes -= self.input.pop_front().unwrap().len();
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    self.input.clear();
                    self.input_bytes = 0;
                    return Err(error);
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
    queue: &'a mut VecDeque<Vec<u8>>,
    bytes: &'a mut usize,
}
impl QueuedInput<'_> {
    fn enqueue(&mut self, bytes: Vec<u8>) -> std::io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        if self.queue.is_empty() {
            match self.session.write(&bytes) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
        }
        if self.bytes.saturating_add(bytes.len()) > INPUT_BUDGET {
            return Err(std::io::Error::other(
                "Input queue is full. Wait for the command to read its input.",
            ));
        }
        *self.bytes += bytes.len();
        self.queue.push_back(bytes);
        Ok(())
    }
}
impl std::io::Write for QueuedInput<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.enqueue(bytes.to_vec())?;
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
    Close(Action),
    Paste(PendingPaste),
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
#[derive(PartialEq)]
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
    // Accessibility may first be requested while synchronized output holds this frame.
    screen: vt::Screen,
    cell: [f32; 2],
    text: Option<TerminalText>,
    ime_rect: egui::Rect,
}

#[derive(PartialEq)]
struct HoveredLink {
    pane: Id,
    uri: String,
    bounds: Vec<egui::Rect>,
}

impl PreparedPane {
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

struct ComposedPane {
    frame: Arc<Frame>,
    rect: [f32; 4],
    dim: Option<rustty_render::Color>,
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
                Arc::ptr_eq(&old.frame, &new.frame) && old.rect == new.rect && old.dim == new.dim
            })
    }

    fn new(
        size: [u32; 2],
        panes: Vec<ComposedPane>,
    ) -> std::result::Result<Self, rustty_render::ComposeError> {
        let mut frame = Frame::empty(size);
        for pane in &panes {
            frame.append_clipped(&pane.frame, [pane.rect[0], pane.rect[1]], pane.rect)?;
            if let Some(color) = pane.dim {
                frame
                    .quads
                    .push(rustty_render::Quad::solid(pane.rect, color));
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
    viewport: ViewportId,
    window: Arc<Window>,
    egui: egui_winit::State,
    accesskit_active: bool,
    fonts: rustty_render::Renderer,
    rects: BTreeMap<Id, egui::Rect>,
    prepared: BTreeMap<Id, PreparedPane>,
    composed: Option<ComposedFrame>,
    pane_prepares: u64,
    content: Rect,
    divider_drag: Option<(Id, Axis, Rect)>,
    modifiers: Modifiers,
    consumed_keys: HashSet<PhysicalKey>,
    sequence: Vec<usize>,
    sequence_len: usize,
    composing: bool,
    preedit: String,
    preedit_selection: Option<(usize, usize)>,
    peek: Option<Peek>,
    navigation_warning: Option<(Id, Instant)>,
    mouse: Pos2,
    deferred_pointer: Option<Pos2>,
    link_hit: Option<(Id, u64, usize, vt::GridPoint, egui::Rect, Vec2)>,
    hovered_link: Option<HoveredLink>,
    mouse_button: Option<vt::MouseButton>,
    selection_drag: Option<input::SelectionDrag>,
    focused: bool,
    focus_hint: FocusHint,
    cursor_blink_started: Instant,
    visible: bool,
    occluded: bool,
    deadline: Option<Instant>,
    search_focus: Option<Id>,
    search_rects: BTreeMap<Id, egui::Rect>,
    focus_text_input: bool,
    popup_open: bool,
    messages_open: bool,
    layout_picker: Option<LayoutPicker>,
    palette: bool,
    palette_query: String,
    confirm: Option<Confirmation>,
    clipboard_request: VecDeque<(Id, vt::Effect)>,
    capture: bool,
    frames: u64,
}
impl Host {
    fn hovered_pane(&self) -> Option<Id> {
        if !self.egui.is_pointer_in_window() {
            return None;
        }
        self.pane_at(self.mouse)
    }
    fn pane_at(&self, position: Pos2) -> Option<Id> {
        self.rects
            .iter()
            .find(|(_, rect)| rect.contains(position))
            .map(|(&id, _)| id)
    }
    fn ui_input(&self) -> bool {
        self.search_focus.is_some() || self.modal_input()
    }
    fn modal_input(&self) -> bool {
        self.palette
            || self.popup_open
            || self.messages_open
            || self.layout_picker.is_some()
            || self.confirm.is_some()
            || !self.clipboard_request.is_empty()
    }
    fn repaint(&self) {
        if self.visible && !self.occluded {
            self.window.request_redraw();
        }
    }
}

struct DirectoryBadge {
    label: DirectoryLabel,
    bounds: egui::Rect,
    large: bool,
    active: bool,
    attention: bool,
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
        let mut job = egui::text::LayoutJob::simple(
            format!(
                "{}{}",
                self.label.name,
                if self.attention { " ●" } else { "" }
            ),
            egui::FontId::proportional(if self.large { 22.0 } else { 12.0 }),
            color,
            (self.bounds.width() * 0.75 - 24.0).max(1.0),
        );
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        job.sections[0]
            .format
            .coords
            .push("wght", if self.large { 600.0 } else { 500.0 });
        if self.label.shows_activity(self.active) {
            job.sections[0].format.underline = egui::Stroke::new(1.0, color);
        }
        let galley = ui.painter().layout_job(job);
        let padding = if self.large {
            Vec2::new(16.0, 10.0)
        } else {
            Vec2::new(8.0, 4.0)
        };
        let size = galley.size() + 2.0 * padding;
        let bounds = if self.large {
            egui::Rect::from_center_size(self.bounds.center(), size)
        } else {
            egui::Rect::from_min_size(
                self.bounds.right_top() + Vec2::new(-size.x - 8.0, 8.0),
                size,
            )
        };
        let painter = ui.painter().with_clip_rect(self.bounds);
        let rounding = if self.large { 10.0 } else { 6.0 };
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
        painter.galley(bounds.min + padding, galley, color);
    }
}

struct GpuRenderer {
    renderer: rustty_render_wgpu::Renderer,
    frame: Option<Arc<Frame>>,
    prepares: u64,
}
struct GpuRenderers(HashMap<Id, GpuRenderer>);
struct TerminalPaint {
    window: Id,
    frame: Arc<Frame>,
    format: wgpu::TextureFormat,
}
impl egui_wgpu::CallbackTrait for TerminalPaint {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _: &egui_wgpu::ScreenDescriptor,
        _: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if resources.get::<GpuRenderers>().is_none() {
            resources.insert(GpuRenderers(HashMap::new()));
        }
        let renderer = resources
            .get_mut::<GpuRenderers>()
            .unwrap()
            .0
            .entry(self.window)
            .or_insert_with(|| GpuRenderer {
                renderer: rustty_render_wgpu::Renderer::new(device, self.format),
                frame: None,
                prepares: 0,
            });
        if !renderer
            .frame
            .as_ref()
            .is_some_and(|frame| Arc::ptr_eq(frame, &self.frame))
        {
            // A failed upload can replace part of the GPU state. Retry it even
            // when the next UI frame still retains the same terminal content.
            renderer.frame = None;
            match renderer.renderer.prepare(device, queue, &self.frame) {
                Ok(()) => {
                    renderer.frame = Some(Arc::clone(&self.frame));
                    renderer.prepares += 1;
                }
                Err(error) => eprintln!("Rustty renderer: {error}"),
            }
        }
        Vec::new()
    }
    fn paint(
        &self,
        _: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(renderer) = resources
            .get::<GpuRenderers>()
            .and_then(|all| all.0.get(&self.window))
        {
            renderer.renderer.paint(pass);
        }
    }
}

struct App {
    loaded: LoadedConfig,
    config_loader: config::ConfigLoader,
    config_args: Vec<String>,
    workspace: Workspace,
    state_path: PathBuf,
    resources: Option<PathBuf>,
    panes: HashMap<Id, Pane>,
    failed_panes: BTreeMap<Id, String>,
    activity_flashes: HashMap<Id, CompletionFlash>,
    closing: Vec<Session>,
    windows: HashMap<WindowId, Host>,
    platform: Option<Platform>,
    context: egui::Context,
    painter: egui_wgpu::winit::Painter,
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
    smoke_error: Option<String>,
}

pub fn run() -> Result<()> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "Rustty — native Rust terminal\n\nUsage: rustty [--config-file=PATH] [--key=value] [-e COMMAND ...]\n       rustty --config-info\n\nRustty settings override Ghostty settings. See crates/rustty-app/README.md."
        );
        return Ok(());
    }
    let show_config = args.iter().any(|arg| arg == "--config-info");
    args.retain(|arg| arg != "--config-info");
    let event_loop = EventLoop::<Event>::with_user_event().build()?;
    let resources = resource_dir();
    let mut config_loader = config::ConfigLoader::from_env()?;
    config_loader.resources_dir = resources.clone().or(config_loader.resources_dir);
    let mut loaded = load_config(&mut config_loader, &args, Platform::system_theme());
    if show_config {
        println!(
            "Configuration: {:?}\nOwn settings: {}\nEdit settings: {}",
            loaded.family,
            loaded.own_config_path.display(),
            loaded.edit_config_path.display()
        );
        for source in &loaded.sources {
            println!("  {}", source.display());
        }
        for diagnostic in &loaded.diagnostics {
            eprintln!("{diagnostic}");
        }
        return Ok(());
    }
    let smoke = smoke::Smoke::from_env(&mut loaded)?;
    let mut state_path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is unavailable")?
        .join("Library/Application Support/com.rustty.app/workspace.json");
    let mut errors = loaded
        .diagnostics
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if let Some(smoke) = &smoke {
        state_path = smoke.directory.join("workspace.json");
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
    let mut gpu_config = egui_wgpu::WgpuConfiguration::default();
    gpu_config.surface = egui_wgpu::SurfaceConfig::LOW_LATENCY;
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
    let painter = pollster::block_on(egui_wgpu::winit::Painter::new(
        context.clone(),
        gpu_config,
        true,
        Default::default(),
    ));
    let now = Instant::now();
    let close_at = std::env::var("RUSTTY_SMOKE_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|s| now + Duration::from_secs(s));
    let mut app = App {
        loaded,
        config_loader,
        config_args: args,
        workspace,
        state_path,
        resources,
        panes: HashMap::new(),
        failed_panes: BTreeMap::new(),
        activity_flashes: HashMap::new(),
        closing: Vec::new(),
        windows: HashMap::new(),
        platform: None,
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
        smoke_error: None,
    };
    let result = event_loop.run_app(&mut app);
    app.shutdown();
    result?;
    if let Some(error) = app.smoke_error {
        return Err(error.into());
    }
    Ok(())
}

fn resource_dir() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let bundled = executable.parent()?.parent()?.join("Resources/rustty");
    if bundled.is_dir() {
        return Some(bundled);
    }
    let development = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/Rustty.app/Contents/Resources/rustty");
    development.is_dir().then_some(development)
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
        let scheme = color_scheme(self.config_loader.dark_mode);
        for (&id, pane) in &mut self.panes {
            let state = (visible.contains(&id), focused_panes.contains(&id), scheme);
            if pane.host_state.1 && !state.1 {
                pane.reset_selection_gesture();
            }
            if !state.0 {
                pane.activity.reset_progress_animation();
            }
            if !pane.exited && pane.host_state != state {
                match pane.session.set_host_state(state.0, state.1, state.2) {
                    Ok(()) => pane.host_state = state,
                    Err(error) => self.errors.push(error.to_string()),
                }
            }
        }
    }
    fn changed(&mut self) {
        self.save_at = Some(Instant::now() + Duration::from_millis(500));
    }
    fn remember(&mut self) {
        if !self.config().undo_timeout.is_zero() {
            self.history.push((
                Instant::now() + self.config().undo_timeout,
                self.workspace.clone(),
            ));
        }
        if self.history.len() > 50 {
            self.history.remove(0);
        }
        self.redo.clear();
    }
    fn directory(&self, window: Id) -> PathBuf {
        self.focused(window)
            .and_then(|id| self.panes.get(&id))
            .map(|pane| pane.cwd.clone())
            .or_else(|| self.config().working_directory.clone())
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/"))
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
    fn spawn_pane(&mut self, id: Id, directory: PathBuf, host: Option<&Host>) -> Result<()> {
        if self.panes.contains_key(&id) || self.failed_panes.contains_key(&id) {
            return Ok(());
        }
        let directory = directory_from_osc(&directory.to_string_lossy())
            .filter(|path| path.is_dir())
            .or_else(|| {
                self.config()
                    .working_directory
                    .clone()
                    .filter(|path| path.is_dir())
            })
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/"));
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
        let host_state = (visible, focused, color_scheme(self.config_loader.dark_mode));
        let session = match Session::spawn(
            self.config(),
            SessionOptions {
                working_directory: Some(directory.clone()),
                command,
                resources: self.resources.clone(),
                color_scheme: Some(host_state.2),
                visible: host_state.0,
                focused: host_state.1,
                ..Default::default()
            },
            wake,
        ) {
            Ok(session) => session,
            Err(error) => {
                self.failed_panes.insert(id, error.to_string());
                return Err(error.into());
            }
        };
        let saved = self
            .workspace
            .windows
            .iter()
            .flat_map(|window| &window.tabs)
            .find_map(|tab| tab.panes.get(&id));
        let title_override = saved.and_then(|pane| pane.title_override.clone());
        let title = title_override
            .clone()
            .or_else(|| saved.and_then(|pane| pane.title.clone()))
            .unwrap_or_default();
        self.panes.insert(
            id,
            Pane {
                session,
                host_state,
                wake_pending,
                input: VecDeque::new(),
                input_bytes: 0,
                title,
                title_override,
                cwd: directory,
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
        Ok(())
    }
    fn open_window(&mut self, event_loop: &ActiveEventLoop, id: Id) -> Result<()> {
        if self.windows.values().any(|host| host.id == id) {
            return Ok(());
        }
        let state = self.workspace.windows[self.index(id).ok_or("missing window")?].clone();
        let quick = state.quick;
        let mut frame = state.frame;
        if quick
            && let Some(bounds) = self.platform.as_ref().and_then(|platform| {
                platform.quick_terminal_frame(self.config(), Some([frame[2], frame[3]]))
            })
        {
            frame = bounds;
        }
        let attributes = Window::default_attributes()
            .with_title("Rustty")
            // AppKit consumes the activation click before any terminal/UI action.
            .with_accepts_first_mouse(false)
            .with_decorations(!quick)
            .with_nonactivating_panel(quick)
            .with_visible(false)
            .with_inner_size(LogicalSize::new(
                frame[2].max(MIN_WINDOW_SIZE.width),
                frame[3].max(MIN_WINDOW_SIZE.height),
            ))
            .with_position(LogicalPosition::new(frame[0], frame[1]))
            .with_min_inner_size(MIN_WINDOW_SIZE)
            .with_transparent(self.config().background_opacity < 1.0)
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
            .with_title_hidden(true);
        let window = Arc::new(event_loop.create_window(attributes)?);
        if let Some(platform) = &self.platform {
            platform.configure_window(&window, quick, self.config())?;
        }
        let viewport = ViewportId::from_hash_of(id);
        pollster::block_on(self.painter.set_window(viewport, Some(window.clone())))?;
        let fonts =
            rustty_render::Renderer::new(font_config(self.config(), window.scale_factor() as f32))?;
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
            self.painter
                .render_state()
                .map(|s| s.device.limits().max_texture_dimension_2d as usize),
        );
        egui.init_accesskit(event_loop, &window, self.proxy.clone());
        for tab in &state.tabs {
            for (&pane, saved) in &tab.panes {
                self.spawn_pane(pane, saved.working_directory.clone(), None)?;
            }
        }
        window.set_visible(!quick);
        let host = Host {
            id,
            viewport,
            window: window.clone(),
            egui,
            accesskit_active: false,
            fonts,
            rects: BTreeMap::new(),
            prepared: BTreeMap::new(),
            composed: None,
            pane_prepares: 0,
            content: Rect::UNIT,
            divider_drag: None,
            modifiers: Modifiers::default(),
            consumed_keys: HashSet::new(),
            sequence: Vec::new(),
            sequence_len: 0,
            composing: false,
            preedit: String::new(),
            preedit_selection: None,
            peek: None,
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
            visible: !quick,
            occluded: false,
            deadline: None,
            search_focus: None,
            search_rects: BTreeMap::new(),
            focus_text_input: false,
            popup_open: false,
            messages_open: false,
            layout_picker: None,
            palette: false,
            palette_query: String::new(),
            confirm: None,
            clipboard_request: VecDeque::new(),
            capture: false,
            frames: 0,
        };
        self.windows.insert(window.id(), host);
        if !quick {
            window.focus_window();
            self.active = Some(id);
        }
        window.request_redraw();
        Ok(())
    }
    fn quick_visible(&mut self, host: &mut Host, visible: bool, restore_focus: bool) {
        let result = if let Some(platform) = &self.platform {
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
                self.remember_quick_frame(host);
                host.repaint();
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
        if self.config().window_save_state != config::WindowSaveState::Never
            && let Err(error) = self.workspace.save(&self.state_path)
        {
            self.errors.push(format!("Could not save windows: {error}"));
        }
    }
    fn write(&mut self, pane: Id, bytes: Vec<u8>) {
        if let Some(pane) = self.panes.get_mut(&pane)
            && let Err(error) = pane.write(bytes)
        {
            self.errors.push(error.to_string());
        }
    }
    fn terminal_input(&self, host: &mut Host, pane: Id) {
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
            && self
                .panes
                .get(&pane)
                .is_some_and(|pane| !pane.exited && !pane.session.has_exited())
    }
    fn paste(&mut self, host: &mut Host, paste: PendingPaste, approved: bool) {
        if paste.data.is_empty() || !self.paste_target_exists(host.id, paste.pane) {
            return;
        }
        let Some(pane) = self.panes.get(&paste.pane) else {
            return;
        };
        let Ok(mut terminal) = pane.session.terminal() else {
            return;
        };
        if !approved && paste_needs_confirmation(self.config(), &terminal, &paste.data) {
            host.confirm = Some(Confirmation::Paste(paste));
            host.repaint();
            return;
        }
        // Encode the captured data with the destination's current paste mode.
        let bytes = terminal.encode_paste(&paste.data);
        terminal.screen_mut().viewport_offset = 0;
        terminal.screen_mut().selection = None;
        drop(terminal);
        self.terminal_input(host, paste.pane);
        self.write(paste.pane, bytes);
        host.repaint();
    }
    fn drop_file(&mut self, host: &mut Host, position: Pos2, path: &Path) {
        if host.modal_input()
            || self
                .context
                .layer_id_at(position)
                .is_some_and(|layer| layer.order != egui::Order::Background)
        {
            return;
        }
        let Some(pane) = host.pane_at(position) else {
            return;
        };
        self.paste(
            host,
            PendingPaste {
                pane,
                data: format!("'{}' ", path.to_string_lossy().replace('\'', "'\\''")).into_bytes(),
            },
            true,
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
        if !self.panes[&pane]
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
        if let Some(previous) = self.focused(window).filter(|&id| id != pane)
            && let Some(state) = self.panes.get_mut(&previous)
        {
            state.reset_selection_gesture();
        }
        if let Some(tab) = self.tab_mut(window) {
            tab.focus(pane);
        }
        if let Some(state) = self.panes.get_mut(&pane) {
            state.unseen = false;
            if let Some(platform) = &self.platform {
                platform.clear_notifications(pane);
            }
        }
        self.changed();
    }
    fn tab_label(&self, tab: &Tab) -> (String, bool) {
        let pane = self.panes.get(&tab.focused);
        let title = tab
            .title
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| pane.map(|p| p.title.as_str()).filter(|s| !s.is_empty()))
            .unwrap_or("Terminal");
        let working = tab
            .panes
            .keys()
            .filter(|id| {
                self.panes
                    .get(id)
                    .is_some_and(|p| p.activity.reported_active())
            })
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
        let label = format!(
            "{}{suffix}{}",
            title.chars().take(26).collect::<String>(),
            if tab.zoom.is_some() { " ◩" } else { "" }
        );
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
        let previous_tab_label = self.pane_tab_label(id);
        let live = previous_tab_label.is_some();
        let previous_errors = self.errors.len();
        let mut content_changed = false;
        let mut title_changed = false;
        let mut close = false;
        let mut stopped = false;
        let mut clipboard = Vec::new();
        let focused = self
            .windows
            .values()
            .any(|host| host.visible && host.focused && self.focused(host.id) == Some(id));
        let config = &self.loaded.config;
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        pane.wake_pending.store(false, Ordering::Release);
        if let Err(error) = pane.flush()
            && !pane.session.has_exited()
        {
            self.errors.push(error.to_string());
        }
        if let Ok(mut terminal) = pane.session.terminal() {
            pane.sync_output.update(&mut terminal, Instant::now());
            if let Some(directory) = directory_from_osc(&terminal.working_directory) {
                content_changed |= pane.cwd != directory;
                pane.cwd = directory;
            }
        } else {
            pane.sync_output.deadline = None;
        }
        let events = pane.session.events().collect::<Vec<_>>();
        for event in events {
            content_changed |= !matches!(&event, SessionEvent::Effect(vt::Effect::Title(_)));
            match event {
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
                SessionEvent::Effect(vt::Effect::CommandStart) => {
                    pane.running = Some(Instant::now());
                    pane.activity.command_started();
                }
                SessionEvent::Effect(vt::Effect::CommandEnd { exit_code }) => {
                    stopped |= pane.activity.command_finished();
                    let elapsed = pane.running.take().map(|start| start.elapsed());
                    if elapsed
                        .is_some_and(|elapsed| elapsed >= config.notify_on_command_finish_after)
                        && (config.notify_on_command_finish
                            == config::NotifyOnCommandFinish::Always
                            || config.notify_on_command_finish
                                == config::NotifyOnCommandFinish::Unfocused
                                && !focused)
                    {
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
                SessionEvent::Effect(vt::Effect::Notification { title, body }) => {
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
                SessionEvent::Exited {
                    code,
                    signal,
                    runtime,
                } => {
                    pane.exited = true;
                    pane.running = None;
                    pane.title = format!(
                        "Exited {code}{}",
                        signal.map(|s| format!(" ({s})")).unwrap_or_default()
                    );
                    pane.exit_message = Some(format!("{} — press any key to close", pane.title));
                    close = live && !hold_after_exit(config, runtime);
                }
                SessionEvent::Error(error) => self.errors.push(error),
                _ => {}
            }
        }
        // The child waiter can report exit before the reader delivers its final effects.
        if pane.exited {
            pane.running = None;
            stopped |= pane.activity.clear();
        }
        // Only a change in the rendered tab label/underline warrants a hidden-tab
        // repaint. Raw titles can be masked by a custom title or another pane.
        let indicators_changed = stopped || previous_tab_label != self.pane_tab_label(id);
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
            if host.hovered_pane() == Some(id)
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
                        .is_some_and(|(pane, _)| *pane == id));
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
        let title = format!("{title} — Rustty");
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
                if let Some(quadrant) = tab.root.quadrant(pane)
                    && let Some(host) = self.windows.values().find(|host| host.id == window.id)
                    && self.quadrant_label(tab, host, quadrant).is_some()
                {
                    self.activity_flashes
                        .entry(quadrant)
                        .or_default()
                        .start_if_idle(now);
                }
            }
        }
    }

    fn quadrant_label(&self, tab: &Tab, host: &Host, quadrant: Id) -> Option<DirectoryLabel> {
        // A full-pane zoom does not display its containing quadrant's label.
        let node = tab.visible_tree(host.peek.is_some()).node(quadrant)?;
        let name = common_directory_name(
            node.panes()
                .iter()
                .map(|id| self.panes.get(id).map(|pane| pane.cwd.as_path())),
        );
        let focused = host.peek.map_or(tab.focused, |peek| peek.target);
        DirectoryLabel::new(
            name,
            node.contains(focused),
            host.focused,
            host.peek.is_some(),
        )
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
            self.finish_clipboard(pane, request, false, false);
            return;
        };
        let mut host = self.windows.remove(&key).unwrap();
        if policy == config::ClipboardAccess::Ask && supported && host.clipboard_request.len() < 16
        {
            host.clipboard_request.push_back((pane, request));
            host.repaint();
        } else {
            self.finish_clipboard(
                pane,
                request,
                policy != config::ClipboardAccess::Deny
                    && (policy == config::ClipboardAccess::Allow || !supported),
                false,
            );
        }
        self.windows.insert(key, host);
    }
    fn finish_clipboard(&mut self, pane: Id, request: vt::Effect, allow: bool, remember: bool) {
        use vt::clipboard::{ReadResult, WriteResult};
        let Some(session) = self.panes.get(&pane).map(|pane| &pane.session) else {
            return;
        };
        // Native pasteboard providers may block. Never hold the terminal mutex
        // during native access, or while queuing the reply to the PTY worker.
        let reply = match request {
            vt::Effect::ClipboardRead(read) => {
                let mut result = if allow {
                    self.platform
                        .as_ref()
                        .map_or(ReadResult::IoError, |p| p.clipboard_read(&read))
                } else {
                    ReadResult::Denied
                };
                if let ReadResult::Success(success) = &mut result {
                    success.remember = remember && read.can_remember;
                }
                session
                    .terminal()
                    .map(|mut terminal| Some(terminal.reply_clipboard_read(read, result)))
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
                session
                    .terminal()
                    .map(|mut terminal| terminal.reply_clipboard_write(write, result))
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
                Action::CloseSurface => focused.into_iter().collect::<Vec<_>>(),
                Action::CloseTab => self
                    .tab(host.id)
                    .map(|tab| tab.root.panes())
                    .unwrap_or_default(),
                Action::CloseWindow => self
                    .index(host.id)
                    .map(|i| {
                        self.workspace.windows[i]
                            .tabs
                            .iter()
                            .flat_map(|t| t.root.panes())
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
            if needs_confirmation {
                host.confirm = Some(Confirmation::Close(action));
                host.repaint();
                return true;
            }
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
                if let Err(error) = self.spawn_pane(pane, directory, Some(host)) {
                    self.errors.push(error.to_string());
                }
            }
            Action::NewSplit(direction) => {
                let directory = self.directory(host.id);
                let pane = self.workspace.id();
                let split = self.workspace.id();
                self.remember();
                if let Some(tab) = self.tab_mut(host.id) {
                    tab.split(pane, split, direction, directory.clone());
                }
                if let Err(error) = self.spawn_pane(pane, directory, Some(host)) {
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
                let target = from.and_then(|from| self.tab(host.id)?.target(from, direction));
                let target = target.map(|pane| {
                    if quadrant {
                        self.tab(host.id)
                            .and_then(|tab| tab.remembered_for(pane))
                            .unwrap_or(pane)
                    } else {
                        pane
                    }
                });
                let moved = target.is_some() && target != from;
                if quadrant {
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
            Action::ResizeSplit { direction, amount } => {
                self.remember();
                if let Some(tab) = self.tab_mut(host.id)
                    && tab
                        .root
                        .resize(tab.focused, direction, f32::from(amount), host.content)
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
                        Action::GotoTab(tab) => (tab - 1).min(count - 1),
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
                host.window
                    .set_fullscreen(if host.window.fullscreen().is_some() {
                        None
                    } else {
                        Some(Fullscreen::Borderless(None))
                    })
            }
            Action::ToggleCommandPalette => {
                host.palette = !host.palette;
                host.palette_query.clear();
                host.focus_text_input = host.palette || host.search_focus.is_some();
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
                if let Some(pane) = focused.and_then(|id| self.panes.get(&id))
                    && let Ok(mut terminal) = pane.session.terminal()
                {
                    let screen = terminal.screen_mut();
                    let first = screen
                        .all_rows()
                        .next()
                        .map(|r| vt::GridPoint { row: r.id, col: 0 });
                    let last = screen.all_rows().last().map(|r| vt::GridPoint {
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
                self.update_fonts(Some(host));
            }
            Action::DecreaseFontSize(amount) => {
                self.loaded.config.font_size = (self.config().font_size - amount).clamp(4.0, 200.0);
                self.update_fonts(Some(host));
            }
            Action::ResetFontSize => {
                let loaded = self.config_loader.load_with_args(&self.config_args);
                self.loaded.config.font_size = loaded.config.font_size;
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
        let Some((_, previous)) = previous else {
            return false;
        };
        let current = self.workspace.restore(previous);
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
        self.loaded = load_config(&mut self.config_loader, &self.config_args, None);
        if self.smoke.is_some() {
            smoke::Smoke::configure(&mut self.loaded);
        }
        self.errors = self
            .loaded
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .collect();
        self.failed_panes.clear();
        self.context.set_theme(ui_theme(&self.loaded.config));
        if let Some(platform) = &mut self.platform
            && let Err(error) = platform.update_config(&self.loaded.config)
        {
            self.errors.push(error);
        }
        for pane in self.panes.values_mut() {
            if let Err(error) = pane.session.apply_config(&self.loaded.config) {
                self.errors.push(error.to_string());
            }
            if !self.loaded.config.progress_style {
                pane.activity.progress_reported(0, None, Instant::now());
            }
        }
        self.update_fonts(host.as_deref_mut());
        if let Some(platform) = &self.platform {
            for window in host.into_iter().chain(self.windows.values_mut()) {
                let quick = self
                    .workspace
                    .windows
                    .iter()
                    .any(|state| state.id == window.id && state.quick);
                if let Err(error) =
                    platform.configure_window(&window.window, quick, &self.loaded.config)
                {
                    self.errors.push(error);
                }
            }
        }
    }
    fn update_fonts(&mut self, host: Option<&mut Host>) {
        for current in host.into_iter().chain(self.windows.values_mut()) {
            match rustty_render::Renderer::new(font_config(
                &self.loaded.config,
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
                for (pane, request) in host.clipboard_request {
                    self.finish_clipboard(pane, request, false, false);
                }
                self.painter
                    .gc_viewports(&self.windows.values().map(|host| host.viewport).collect());
                if let Some(state) = self.painter.render_state()
                    && let Some(renderers) = state
                        .renderer
                        .write()
                        .callback_resources
                        .get_mut::<GpuRenderers>()
                {
                    renderers.0.remove(&host.id);
                }
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
        for (id, directory) in required {
            if let Err(error) = self.spawn_pane(id, directory, None) {
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
        if self.workspace.windows.is_empty() && self.config().quit_after_last_window_closed {
            self.save();
            event_loop.exit();
        }
    }
    fn keyboard(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
        key: &winit::event::KeyEvent,
    ) -> bool {
        let ui_input = host.ui_input();
        let search_input = host.search_focus.is_some() && !host.modal_input();
        if input::key_is_consumed(
            &mut host.consumed_keys,
            key.physical_key,
            key.state,
            ui_input && !search_input,
        ) && !(search_input && key.repeat)
        {
            return !ui_input;
        }
        if key.state == ElementState::Pressed && !host.composing {
            let candidates = if host.sequence_len == 0 {
                (0..self.config().keybinds.len()).collect::<Vec<_>>()
            } else {
                host.sequence.clone()
            };
            let matched = candidates
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
            let complete = matched
                .iter()
                .rev()
                .copied()
                .find(|&i| self.config().keybinds[i].trigger.len() == host.sequence_len + 1);
            if let Some(index) = complete {
                host.sequence.clear();
                host.sequence_len = 0;
                let binding = self.config().keybinds[index].clone();
                let mut performed = false;
                for action in binding.actions {
                    if binding.flags.all
                        && let Action::Text(bytes) = action
                    {
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
                        performed = true;
                    } else {
                        performed |= self.action(event_loop, host, action, false);
                    }
                }
                if binding.flags.consumed && (!binding.flags.performable || performed) {
                    if ui_input || host.ui_input() {
                        host.consumed_keys.insert(key.physical_key);
                    }
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
        if input::key_is_consumed(
            &mut host.consumed_keys,
            key.physical_key,
            key.state,
            ui_input,
        ) {
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
        let bytes = self.panes.get(&id).and_then(|pane| {
            let mut terminal = pane.session.terminal().ok()?;
            let options = vt::KeyEncodeOptions {
                macos_option_as_alt: input::option_as_alt(
                    self.config().macos_option_as_alt,
                    host.modifiers.lalt_state(),
                    host.modifiers.ralt_state(),
                ),
            };
            let event = input::terminal_key(key, host.modifiers, host.composing, options)?;
            Some(input::encode_terminal_key(&mut terminal, &event, options))
        });
        if let Some(bytes) = bytes {
            if key.state == ElementState::Pressed && !bytes.is_empty() {
                self.terminal_input(host, id);
            }
            self.write(id, bytes);
        }
        host.repaint();
        // Discard the native translation now, before a later event can open Find.
        true
    }
}

fn reveal(screen: &mut vt::Screen, point: vt::GridPoint) {
    let index = screen.all_rows().position(|row| row.id == point.row);
    if let Some(index) = index {
        screen.viewport_offset = screen.history_len().saturating_sub(index);
    }
}

impl App {
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
            return Ok(());
        }
        let Some(index) = self.index(host.id) else {
            return Ok(());
        };
        let config = self.config().clone();
        let state = self.workspace.windows[index].clone();
        if let Some(Confirmation::Paste(paste)) = &host.confirm
            && !self.paste_target_exists(host.id, paste.pane)
        {
            host.confirm = None;
        }
        let active = &state.tabs[state.active_tab];
        let focused = active.focused;
        if host.search_focus.is_some_and(|id| {
            id != focused || self.panes.get(&id).is_none_or(|pane| pane.search.is_none())
        }) {
            host.search_focus = None;
        }
        let header_height = if active.panes.len() > 1 { 18.0 } else { 0.0 };
        let scale = host.window.scale_factor() as f32;
        let size = host.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let mut raw = host.egui.take_egui_input(&host.window);
        raw.viewport_id = host.viewport;
        if !self.passive_mouse_motion(host, host.mouse)
            && let Some(position) = host.deferred_pointer.take()
        {
            raw.events.push(egui::Event::PointerMoved(position));
        }
        input::filter_egui_events(&mut raw, host.ui_input());
        let context = self.context.clone();
        let now = Instant::now();
        let platform_accent = self
            .platform
            .as_ref()
            .and_then(Platform::accent_color)
            .unwrap_or_else(|| {
                let [r, g, b, _] = context.global_style().visuals.selection.bg_fill.to_array();
                [r, g, b]
            });
        let tab_accent = TabAccent::new(active.color, platform_accent);
        let accent = Color32::from_rgb(
            tab_accent.background[0],
            tab_accent.background[1],
            tab_accent.background[2],
        );
        self.activity_flashes.remove(&active.id);
        let mut commands = Vec::new();
        let mut search_commands = Vec::new();
        let mut layout_command = None;
        let mut tab_selection = None;
        let mut presentation_changed = false;
        let mut retry_pane = None;
        let mut render_error = None;
        let format = self
            .painter
            .render_state()
            .ok_or("GPU unavailable")?
            .target_format;
        let (blink_on, blink_deadline) = cursor_blink_phase(host.cursor_blink_started, now);
        let mut needs_blink = false;
        let mut animation_deadline: Option<Instant> = None;
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
        let mut output = context.run_ui(raw, |root_ui| {
            let ctx = &context;
            root_ui.visuals_mut().selection.bg_fill = accent;
            root_ui.visuals_mut().selection.stroke.color = Color32::from_rgb(
                tab_accent.foreground[0],
                tab_accent.foreground[1],
                tab_accent.foreground[2],
            );
            root_ui.visuals_mut().hyperlink_color = accent;
            egui::Panel::top("tabs")
                .exact_size(38.0)
                .frame(
                    egui::Frame::new()
                        .fill(rgb(config.background))
                        .inner_margin(6.0),
                )
                .show(root_ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(72.0);
                        for (index, tab) in state.tabs.iter().enumerate() {
                            let (label, is_active) = self.tab_label(tab);
                            let selected = index == state.active_tab;
                            let colors = TabAccent::new(tab.color, platform_accent);
                            let color = Color32::from_rgb(
                                colors.background[0],
                                colors.background[1],
                                colors.background[2],
                            );
                            let mut text = egui::RichText::new(label);
                            if !selected && is_active {
                                text = text.underline();
                            }
                            if selected {
                                text = text.color(Color32::from_rgb(
                                    colors.foreground[0],
                                    colors.foreground[1],
                                    colors.foreground[2],
                                ));
                            }
                            let flash =
                                self.flash_opacity(tab.id, now, &mut animation_deadline) * 0.45;
                            let fill = if selected {
                                color
                            } else {
                                color.gamma_multiply(flash.max(if tab.color.is_some() {
                                    0.12
                                } else {
                                    0.0
                                }))
                            };
                            let response =
                                ui.add(egui::Button::selectable(selected, text).fill(fill));
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
                            response.context_menu(|ui| {
                                let window_index = self.index(host.id).unwrap();
                                let actual = &mut self.workspace.windows[window_index].tabs[index];
                                ui.label("Tab title");
                                let mut title = actual.title.clone().unwrap_or_default();
                                if ui.text_edit_singleline(&mut title).changed() {
                                    actual.title = (!title.is_empty()).then_some(title);
                                    presentation_changed = true;
                                }
                                ui.label("Tab color");
                                let mut color = actual.color.unwrap_or(platform_accent);
                                if ui.color_edit_button_srgb(&mut color).changed() {
                                    actual.color = Some(color);
                                    presentation_changed = true;
                                }
                                if actual.color.is_some()
                                    && ui.button("Use system accent").clicked()
                                {
                                    actual.color = None;
                                    presentation_changed = true;
                                }
                                if ui.button("Close tab").clicked() {
                                    tab_selection = Some(index);
                                    commands.push(Action::CloseTab);
                                    ui.close();
                                }
                            });
                        }
                        if ui.button("+").on_hover_text("New tab").clicked() {
                            commands.push(Action::NewTab);
                        }
                    });
                });
            // Cache this viewport's popup state for native events between frames.
            host.popup_open = egui::Popup::is_any_open(ctx);
            if host.ui_input() {
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
                    let layout = active
                        .visible_tree(host.peek.is_some())
                        .layout(host.content);
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
                    host.prepared.retain(|id, _| host.rects.contains_key(id));
                    let mut composed = None;
                    let mut accessible = Vec::new();
                    let mut terminal_ime_rect = None;
                    // An atlas eviction can happen halfway through a multi-pane frame.
                    // Rebuild against its new generation before handing a frame to WGPU.
                    for attempt in 0..2 {
                        let mut composition = Vec::with_capacity(host.rects.len());
                        accessible.clear();
                        for (&id, &rect) in &host.rects {
                            let Some(pane) = self.panes.get_mut(&id) else {
                                continue;
                            };
                            let physical = rect.size() * scale;
                            let padding = [
                                config.window_padding_x.start * scale,
                                (config.window_padding_y.start + header_height) * scale,
                            ];
                            let metrics = host.fonts.metrics();
                            let cols = ((physical.x
                                - (config.window_padding_x.start + config.window_padding_x.end)
                                    * scale)
                                .max(metrics.cell_width as f32)
                                / metrics.cell_width as f32)
                                .floor()
                                .clamp(1.0, u16::MAX as f32)
                                as u16;
                            let rows = ((physical.y
                                - (config.window_padding_y.start
                                    + config.window_padding_y.end
                                    + header_height)
                                    * scale)
                                .max(metrics.cell_height as f32)
                                / metrics.cell_height as f32)
                                .floor()
                                .clamp(1.0, u16::MAX as f32)
                                as u16;
                            if let Err(error) = pane.session.resize(
                                cols,
                                rows,
                                physical.x.min(u16::MAX as f32) as u16,
                                physical.y.min(u16::MAX as f32) as u16,
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
                            let now_ms =
                                self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                            if !synchronized && let Some(next) = terminal.tick_graphics(now_ms) {
                                let deadline = Instant::now()
                                    + Duration::from_millis(next.saturating_sub(now_ms));
                                animation_deadline = Some(
                                    animation_deadline.map_or(deadline, |old| old.min(deadline)),
                                );
                            }
                            let is_focused = host.focused && id == focused && host.peek.is_none();
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
                                cursor_visible: !pane.exited && (id != focused || !host.composing),
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
                            .then(|| terminal.screen().snapshot_viewport());
                            drop(terminal);
                            if let Some(snapshot) = snapshot {
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
                            let Some(prepared) = host.prepared.get(&id) else {
                                continue;
                            };
                            needs_blink |= !synchronized
                                && is_focused
                                && !pane.exited
                                && prepared.frame.blinking_text;
                            composition.push(ComposedPane {
                                frame: Arc::clone(&prepared.frame),
                                rect: [
                                    rect.left() * scale,
                                    rect.top() * scale,
                                    physical.x,
                                    physical.y,
                                ],
                                dim: (id != focused && config.unfocused_split_opacity < 1.0).then(
                                    || {
                                        let color = config
                                            .unfocused_split_fill
                                            .unwrap_or(config.background);
                                        rustty_render::Color::rgb([color.r, color.g, color.b])
                                            .opacity(1.0 - config.unfocused_split_opacity)
                                    },
                                ),
                            });
                            accessible.push((id, rect));
                            if id == focused && !pane.exited {
                                terminal_ime_rect = Some(prepared.ime_rect);
                            }
                        }
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
                    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                        egui::Rect::from_min_size(
                            Pos2::ZERO,
                            Vec2::new(size.width as f32 / scale, size.height as f32 / scale),
                        ),
                        TerminalPaint {
                            window: host.id,
                            frame: composed.unwrap_or_else(|| {
                                Arc::new(Frame::empty([size.width, size.height]))
                            }),
                            format,
                        },
                    ));
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
                        if id == focused && host.focused {
                            input::terminal_input(&response, terminal_ime_rect, host.ui_input());
                        }
                        if host.navigation_warning.is_some_and(|(pane, _)| pane == id) {
                            ui.painter().rect_stroke(
                                rect.shrink(2.0),
                                0.0,
                                egui::Stroke::new(3.0, Color32::from_rgb(230, 125, 65)),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if active.root.panes().len() > 1 {
                            let selected = host
                                .peek
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
                            && !paint_progress(
                                ui,
                                id,
                                rect,
                                progress,
                                accent,
                                pane.activity.progress_offset(now),
                            )
                        {
                            pane.activity.reset_progress_animation();
                        }
                    }
                    for (&id, &rect) in &host.rects {
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
                    }
                    self.update_hover_link(host);
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
                    for (&pane, &rect) in &host.rects {
                        if let Some(quadrant) = active.root.quadrant(pane) {
                            quadrants
                                .entry(quadrant)
                                .and_modify(|bounds| *bounds = bounds.union(rect))
                                .or_insert(rect);
                        }
                    }
                    let mut shared_labels = HashSet::new();
                    for (id, bounds) in quadrants {
                        let members = active.root.node(id).unwrap().panes();
                        let attention = members
                            .iter()
                            .any(|id| self.panes.get(id).is_some_and(|pane| pane.unseen));
                        if let Some(peek) = host.peek {
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
                        if let Some(label) = self.quadrant_label(active, host, id) {
                            shared_labels.extend(members.iter().copied());
                            DirectoryBadge {
                                label,
                                bounds,
                                large: true,
                                attention,
                                accent: tab_accent,
                                active: members.iter().any(|id| {
                                    self.panes
                                        .get(id)
                                        .is_some_and(|pane| pane.activity.is_active())
                                }),
                                flash: self.flash_opacity(id, now, &mut animation_deadline),
                            }
                            .paint(ui);
                        }
                    }
                    for (&id, &bounds) in &host.rects {
                        if shared_labels.contains(&id) {
                            continue;
                        }
                        let Some(pane) = self.panes.get(&id) else {
                            continue;
                        };
                        let selected = host.peek.map_or(focused, |peek| peek.target) == id;
                        if let Some(label) = DirectoryLabel::new(
                            directory_name(&pane.cwd),
                            selected,
                            host.focused,
                            host.focus_hint.visible(id, now),
                        ) {
                            let large = label.large(
                                active.quadrant_zoom.is_some()
                                    && (host.peek.is_some() || active.zoom == active.quadrant_zoom),
                            );
                            let flash = if label.shows_flash(true, false) {
                                self.flash_opacity(id, now, &mut animation_deadline)
                            } else {
                                0.0
                            };
                            DirectoryBadge {
                                label,
                                bounds,
                                large,
                                flash,
                                accent: tab_accent,
                                active: pane.activity.is_active(),
                                attention: pane.unseen,
                            }
                            .paint(ui);
                        }
                    }
                });
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
                for (&id, &bounds) in &host.rects {
                    let Some(pane) = self.panes.get_mut(&id) else {
                        continue;
                    };
                    let Some(search) = &mut pane.search else {
                        continue;
                    };
                    let mut focus =
                        !modal_input && host.search_focus == Some(id) && host.focus_text_input;
                    let requested = focus;
                    let response = search.show(ui, id, bounds, id == focused, &mut focus, &config);
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
            if host.palette {
                egui::Window::new("Command palette")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_TOP, [0.0, 60.0])
                    .show(ctx, |ui| {
                        let focus =
                            !ui.is_sizing_pass() && std::mem::take(&mut host.focus_text_input);
                        input::text_edit(
                            ui,
                            &mut host.palette_query,
                            egui::Id::new(("palette", host.id)),
                            focus,
                        );
                        for (label, action) in palette_actions() {
                            if label
                                .to_lowercase()
                                .contains(&host.palette_query.to_lowercase())
                                && ui.button(label).clicked()
                            {
                                commands.push(action);
                                host.palette = false;
                                host.focus_text_input = host.search_focus.is_some();
                            }
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            host.palette = false;
                            host.focus_text_input = host.search_focus.is_some();
                        }
                    });
            }
            let mut confirmed = None;
            if let Some(confirmation) = &host.confirm {
                let is_paste = matches!(confirmation, Confirmation::Paste(_));
                egui::Window::new(if is_paste {
                    "Paste potentially unsafe text?"
                } else {
                    "Close running commands?"
                })
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    match confirmation {
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
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked()
                            || ui.input(|input| input.key_pressed(egui::Key::Escape))
                        {
                            confirmed = Some(false);
                        }
                        if ui
                            .button(if is_paste { "Paste" } else { "Close" })
                            .clicked()
                        {
                            confirmed = Some(true);
                        }
                    });
                });
            }
            if let Some(approved) = confirmed
                && let Some(confirmation) = host.confirm.take()
                && approved
            {
                match confirmation {
                    Confirmation::Close(action) => {
                        self.action(event_loop, host, action, true);
                    }
                    Confirmation::Paste(paste) => self.paste(host, paste, true),
                }
            }
            if let Some((pane, request)) = host.clipboard_request.front() {
                let write = matches!(request, vt::Effect::ClipboardWrite(_));
                let (name, can_remember) = match request {
                    vt::Effect::ClipboardRead(read) => (&read.name, read.can_remember),
                    vt::Effect::ClipboardWrite(write) => (&write.name, write.can_remember),
                    _ => unreachable!(),
                };
                let name = String::from_utf8_lossy(name).into_owned();
                let title = self
                    .panes
                    .get(pane)
                    .map(|pane| pane.title.clone())
                    .unwrap_or_default();
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
                        ui.horizontal(|ui| {
                            if ui.button("Deny").clicked()
                                && let Some((pane, request)) = host.clipboard_request.pop_front()
                            {
                                self.finish_clipboard(pane, request, false, false);
                            }
                            if ui.button("Allow once").clicked()
                                && let Some((pane, request)) = host.clipboard_request.pop_front()
                            {
                                self.finish_clipboard(pane, request, true, false);
                            }
                            if can_remember
                                && ui.button("Allow for this session").clicked()
                                && let Some((pane, request)) = host.clipboard_request.pop_front()
                            {
                                self.finish_clipboard(pane, request, true, true);
                            }
                        });
                    });
            }
            if let Some(picker) = &mut host.layout_picker {
                layout_command = show_layout_picker(ctx, picker).or(layout_command.take());
            }
            let _ = show_messages(ctx, &mut self.errors);
            host.messages_open = !self.errors.is_empty();
        });
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
        if let Some(id) = retry_pane {
            self.failed_panes.remove(&id);
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
                pane.accessibility(id).append_to(update);
            }
        }
        host.egui.handle_platform_output_with_event_loop(
            &host.window,
            event_loop,
            output.platform_output,
        );
        if let Some(cursor) = self.pointer_cursor(host) {
            host.window.set_cursor(cursor);
        }
        let primitives = self
            .context
            .tessellate(output.shapes, output.pixels_per_point);
        if host.capture
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
        if let Some(target) = offscreen {
            let state = self.painter.render_state().ok_or("GPU unavailable")?;
            smoke::submit_offscreen(
                &state,
                &primitives,
                &output.textures_delta,
                [size.width, size.height],
                output.pixels_per_point,
                target,
            );
        } else {
            self.painter.paint_and_update_textures(
                host.viewport,
                output.pixels_per_point,
                [0.0; 4],
                &primitives,
                &mut output.textures_delta,
                if std::mem::take(&mut host.capture) {
                    vec![egui::UserData::default()]
                } else {
                    Vec::new()
                },
                &host.window,
            );
        }
        host.frames += 1;
        host.deadline = animation_deadline;
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
                    rect.contains(position) && !self.failed_panes.contains_key(id)
                }) && self
                    .context
                    .layer_id_at(position)
                    .is_none_or(|layer| layer.order == egui::Order::Background)
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
    fn scroll(&mut self, host: &mut Host, delta: MouseScrollDelta) {
        if host.modal_input()
            || host.peek.is_some()
            || host.divider_drag.is_some()
            || self
                .context
                .layer_id_at(host.mouse)
                .is_some_and(|layer| layer.order != egui::Order::Background)
        {
            return;
        }
        if let Some(id) = host.hovered_pane() {
            let mut cursor_keys = None;
            let mouse = self
                .panes
                .get(&id)
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
                if terminal.is_alternate_screen()
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
                    if count != 0 {
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
        host.repaint();
    }
    /// Resolve the same target used by Command-click. Passive motion stays cheap:
    /// without Command there is no scan, and an unchanged cell reuses its hit.
    fn update_hover_link(&mut self, host: &mut Host) -> bool {
        let result = (|| {
            if !host.modifiers.state().super_key()
                || !self.config().link_url
                || host.modal_input()
                || host.peek.is_some()
                || host.divider_drag.is_some()
                || self.platform.is_none()
                || self
                    .context
                    .layer_id_at(host.mouse)
                    .is_some_and(|layer| layer.order != egui::Order::Background)
            {
                return None;
            }
            let tab = self.tab(host.id)?;
            if tab
                .visible_tree(false)
                .divider_at(host.content, [host.mouse.x, host.mouse.y], 3.0)
                .is_some()
            {
                return None;
            }
            let header = if tab.panes.len() > 1 { 18.0 } else { 0.0 };
            let id = host.hovered_pane()?;
            let rect = host.rects[&id];
            let config = &self.loaded.config;
            let pane = self.panes.get_mut(&id)?;
            let terminal = pane.session.terminal().ok()?;
            if input::mouse_reporting(
                &terminal,
                host.modifiers.state(),
                config.mouse_shift_capture,
            ) || terminal.modes.dec(2026)
            {
                return None;
            }
            let text = egui::Rect::from_min_max(
                rect.min
                    + Vec2::new(
                        config.window_padding_x.start,
                        config.window_padding_y.start + header,
                    ),
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
        if host.modal_input()
            || host.peek.is_some()
            || host
                .search_rects
                .values()
                .any(|rect| rect.contains(host.mouse))
        {
            return None;
        }
        let divider = host.divider_drag.or_else(|| {
            self.tab(host.id)?.visible_tree(false).divider_at(
                host.content,
                [host.mouse.x, host.mouse.y],
                3.0,
            )
        });
        if let Some((_, axis, _)) = divider {
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
        pane.session.terminal().ok()?.mouse_shape().parse().ok()
    }

    fn mouse(&mut self, host: &mut Host, action: vt::MouseAction, button: Option<vt::MouseButton>) {
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
            if self
                .context
                .layer_id_at(host.mouse)
                .is_none_or(|layer| layer.order == egui::Order::Background)
            {
                host.search_focus = None;
            }
        }
        if self.update_hover_link(host) {
            host.repaint();
        }
        if let Some(cursor) = self.pointer_cursor(host) {
            host.window.set_cursor(cursor);
        }
        if let Some((id, axis, bounds)) = host.divider_drag {
            if action == vt::MouseAction::Release {
                host.divider_drag = None;
            } else if action == vt::MouseAction::Move {
                let ratio = match axis {
                    Axis::Horizontal => (host.mouse.x - bounds.x) / bounds.width,
                    Axis::Vertical => (host.mouse.y - bounds.y) / bounds.height,
                };
                if let Some(tab) = self.tab_mut(host.id) {
                    tab.root.set_ratio(id, ratio);
                }
                self.changed();
            }
            host.repaint();
            return;
        }
        if self
            .context
            .layer_id_at(host.mouse)
            .is_some_and(|layer| layer.order != egui::Order::Background)
        {
            return;
        }
        if !host.modal_input() && host.peek.is_none() {
            let divider = self.tab(host.id).and_then(|tab| {
                tab.visible_tree(false)
                    .divider_at(host.content, [host.mouse.x, host.mouse.y], 3.0)
            });
            if let Some(divider) = divider {
                if action == vt::MouseAction::Press && button == Some(vt::MouseButton::Left) {
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
        if self
            .context
            .layer_id_at(host.mouse)
            .is_some_and(|layer| layer.order != egui::Order::Background)
        {
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
            self.config().window_padding_x.start * scale,
            (self.config().window_padding_y.start
                + if self.tab(host.id).is_some_and(|t| t.panes.len() > 1) {
                    18.0
                } else {
                    0.0
                })
                * scale,
            self.config().window_padding_x.end * scale,
            self.config().window_padding_y.end * scale,
        ];
        let position = surface - Vec2::new(padding[0], padding[1]);
        let config = &self.loaded.config;
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        let Ok(mut terminal) = pane.session.terminal() else {
            return;
        };
        if input::mouse_reporting(
            &terminal,
            host.modifiers.state(),
            config.mouse_shift_capture,
        ) {
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
            self.write(id, bytes);
            // Mouse reporting does not change the local screen; PTY output will repaint it.
            if action != vt::MouseAction::Move || !self.errors.is_empty() {
                host.repaint();
            }
            return;
        }
        if action == vt::MouseAction::Move
            && (host.mouse_button != Some(vt::MouseButton::Left) || host.selection_drag.is_none())
        {
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
            if action == vt::MouseAction::Press && button == Some(vt::MouseButton::Left) {
                if host.modifiers.state().super_key() && config.link_url {
                    pane.selection_gesture.reset(&mut terminal);
                    host.selection_drag = None;
                    if let Some(link) = &host.hovered_link
                        && link.pane == id
                        && let Some(platform) = &self.platform
                        && let Err(error) = platform.open_url(&link.uri)
                    {
                        self.errors.push(error);
                    }
                } else {
                    host.selection_drag = Some(input::SelectionDrag::new(point, host.mouse));
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
            } else if action == vt::MouseAction::Move
                && host.mouse_button == Some(vt::MouseButton::Left)
            {
                if let Some(drag) = &mut host.selection_drag
                    && let Some(selection) =
                        drag.update(host.mouse, point, host.modifiers.state().alt_key())
                {
                    // Ordinary dragging keeps its existing cell boundaries;
                    // repeated clicks extend by whole words or lines.
                    terminal.screen_mut().selection = if pane.selection_gesture.behavior()
                        == vt::selection_gesture::Behavior::Cell
                    {
                        Some(selection)
                    } else {
                        pane.selection_gesture.drag(
                            &terminal,
                            vt::selection_gesture::Drag {
                                point,
                                xpos: f64::from(surface.x),
                                ypos: f64::from(surface.y),
                                rectangle: host.modifiers.state().alt_key(),
                                word_boundaries: vt::selection::DEFAULT_WORD_BOUNDARIES,
                                geometry: vt::selection_gesture::Geometry {
                                    columns: u32::from(terminal.cols),
                                    cell_width: metrics.cell_width,
                                    padding_left: padding[0] as u32,
                                    screen_height: (rect.height() * scale) as u32,
                                },
                            },
                        )
                    };
                }
            } else if action == vt::MouseAction::Release && host.selection_drag.take().is_some() {
                pane.selection_gesture.release(&terminal, Some(point));
                let copy = config.copy_on_select;
                let text = terminal.screen().selection_text();
                drop(terminal);
                if let Some(text) = text {
                    if matches!(
                        copy,
                        config::CopyOnSelect::Clipboard | config::CopyOnSelect::Both
                    ) {
                        host.egui.set_clipboard_text(text.clone());
                    }
                    if matches!(
                        copy,
                        config::CopyOnSelect::Primary | config::CopyOnSelect::Both
                    ) {
                        self.set_selection_text(text);
                    }
                }
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
fn palette_actions() -> Vec<(&'static str, Action)> {
    vec![
        ("New window", Action::NewWindow),
        ("New tab", Action::NewTab),
        ("Split right", Action::NewSplit(Direction::Right)),
        ("Split down", Action::NewSplit(Direction::Down)),
        ("Zoom pane", Action::ToggleSplitZoom),
        ("Zoom quadrant", Action::ToggleQuadrantZoom),
        ("Equalize splits", Action::EqualizeSplits),
        ("Find", Action::StartSearch),
        ("Open configuration", Action::OpenConfig),
        ("Open saved layout", Action::OpenLayout),
        ("Reload configuration", Action::ReloadConfig),
        ("Toggle quick terminal", Action::ToggleQuickTerminal),
        ("Undo layout change", Action::Undo),
        ("Redo layout change", Action::Redo),
    ]
}

impl ApplicationHandler<Event> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.platform.is_none() {
            let proxy = self.proxy.clone();
            match Platform::new(
                Arc::new(move |event| {
                    let _ = proxy.send_event(Event::Platform(event));
                }),
                self.config(),
            ) {
                Ok(platform) => self.platform = Some(platform),
                Err(error) => self.errors.push(error),
            }
        }
        if self.workspace.windows.iter().all(|w| w.quick) {
            self.add_window(false);
        }
        self.reconcile(event_loop);
    }
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Event) {
        match event {
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
                    if deadline <= Instant::now() {
                        host.repaint();
                    } else {
                        host.deadline =
                            Some(host.deadline.map_or(deadline, |old| old.min(deadline)));
                    }
                }
            }
            Event::Platform(event) => {
                if let PlatformEvent::NotificationClicked(pane) = event {
                    let target = self.workspace.windows.iter().find_map(|window| {
                        window
                            .tabs
                            .iter()
                            .position(|tab| tab.panes.contains_key(&pane))
                            .map(|index| (window.id, index))
                    });
                    if let Some((id, index)) = target {
                        if let Some(window) = self.index(id) {
                            self.workspace.windows[window].active_tab = index;
                        }
                        self.focus_pane(id, pane);
                        if let Some(host) = self.windows.values_mut().find(|host| host.id == id) {
                            host.peek = None;
                            host.visible = true;
                            host.window.set_visible(true);
                            host.window.focus_window();
                            host.repaint();
                        }
                    }
                } else if let PlatformEvent::Action(action) = event {
                    if action == Action::OpenLayout && self.windows.is_empty() {
                        let id = self.add_window(false);
                        self.reconcile(event_loop);
                        self.active = Some(id);
                    }
                    let key = self
                        .windows
                        .iter()
                        .find(|(_, host)| Some(host.id) == self.active)
                        .or_else(|| self.windows.iter().next())
                        .map(|(id, _)| *id);
                    if let Some(key) = key
                        && let Some(mut host) = self.windows.remove(&key)
                    {
                        self.action(event_loop, &mut host, action, false);
                        self.windows.insert(key, host);
                        self.reconcile(event_loop);
                    } else if matches!(action, Action::Undo | Action::Redo) {
                        if self.undo_layout(action == Action::Redo) {
                            self.reconcile(event_loop);
                        }
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
                    } else if matches!(action, Action::NewWindow | Action::NewTab) {
                        self.add_window(false);
                        self.reconcile(event_loop);
                    } else if action == Action::Quit {
                        self.save();
                        event_loop.exit();
                    }
                }
                self.sync_host_state();
            }
            Event::Access(event) => {
                use egui_winit::accesskit_winit::WindowEvent as AccessEvent;
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
                                        host.window.focus_window();
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
            } => {
                if self.keyboard(event_loop, &mut host, &event) {
                    // Retain UI events queued by actions, but discard this key's native translation.
                    host.egui
                        .egui_input_mut()
                        .events
                        .drain(egui_event_start..egui_event_end);
                }
            }
            WindowEvent::Ime(ime) if !host.ui_input() => {
                host.egui
                    .egui_input_mut()
                    .events
                    .drain(egui_event_start..egui_event_end);
                match ime {
                    Ime::Preedit(text, selection) => {
                        if !text.is_empty()
                            && let Some(pane) = self.focused(host.id)
                        {
                            self.terminal_input(&mut host, pane);
                        }
                        host.composing = !text.is_empty();
                        host.preedit = text;
                        host.preedit_selection = selection;
                    }
                    Ime::Commit(text) => {
                        host.composing = false;
                        host.preedit.clear();
                        host.preedit_selection = None;
                        if let Some(pane) = self.focused(host.id)
                            && let Some(bytes) = self.panes.get(&pane).and_then(|pane| {
                                let terminal = pane.session.terminal().ok()?;
                                Some(input::terminal_text(&terminal, text))
                            })
                        {
                            if !bytes.is_empty() {
                                self.terminal_input(&mut host, pane);
                            }
                            self.write(pane, bytes);
                        }
                    }
                    Ime::Disabled => {
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
        if let Some(mut smoke) = self.smoke.take() {
            match smoke.step(self, event_loop) {
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
        let expired = self
            .panes
            .iter_mut()
            .filter_map(|(&id, pane)| {
                pane.activity
                    .deadline()
                    .filter(|&deadline| deadline <= now)
                    .map(|_| (id, pane.activity.expire(now)))
            })
            .collect::<Vec<_>>();
        for (id, stopped) in expired {
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
            .chain(self.panes.values().flat_map(|pane| {
                pane.activity
                    .deadline()
                    .into_iter()
                    .chain(pane.sync_output.deadline)
            }))
            .chain((!self.closing.is_empty()).then_some(now + Duration::from_millis(20)))
            .chain(
                self.history
                    .iter()
                    .chain(&self.redo)
                    .map(|(expires, _)| *expires),
            )
            .chain(self.smoke.as_ref().map(|_| now + Duration::from_millis(50)))
            .min();
        for host in self.windows.values_mut() {
            if let Some(deadline) = host.deadline {
                if deadline <= now {
                    host.deadline = None;
                    host.repaint();
                } else if host.visible && !host.occluded {
                    next = Some(next.map_or(deadline, |old| old.min(deadline)));
                }
            }
        }
        event_loop.set_control_flow(next.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }
    fn exiting(&mut self, _: &ActiveEventLoop) {
        self.save();
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
    fn shutdown(&mut self) {
        for host in self.windows.values() {
            host.window.set_visible(false);
        }
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
            && self
                .panes
                .values()
                .map(|pane| &pane.session)
                .chain(&self.closing)
                .any(|session| !session.has_exited())
        {
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

fn window_contains_pane(window: &WindowState, pane: Id) -> bool {
    window.tabs.iter().any(|tab| tab.panes.contains_key(&pane))
}

fn paste_needs_confirmation(config: &Config, terminal: &vt::Terminal, data: &[u8]) -> bool {
    config.clipboard_paste_protection
        && !vt::input::paste_is_safe(
            data,
            config.clipboard_paste_bracketed_safe && terminal.modes.dec(2004),
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

fn show_messages(ctx: &egui::Context, errors: &mut Vec<String>) -> Option<egui::Response> {
    if errors.is_empty() {
        return None;
    }
    let mut open = true;
    let dismiss = egui::Window::new("Rustty messages")
        .open(&mut open)
        .collapsible(false)
        .default_size([600.0, 240.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    for error in errors.iter() {
                        ui.label(error);
                    }
                });
            ui.button("Dismiss")
        })
        .and_then(|window| window.inner);
    if !open
        || dismiss.as_ref().is_some_and(egui::Response::clicked)
        || ctx.input(|input| input.key_pressed(egui::Key::Escape))
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
        .map_or_else(Vec::new, |tab| tab.visible_tree(peek).panes())
}

fn initial_visible_panes(window: &WindowState, native: Option<(bool, bool, bool)>) -> Vec<Id> {
    // Normal windows are shown during creation; quick windows start hidden.
    let (visible, occluded, peek) = native.unwrap_or((!window.quick, false, false));
    window_visible_panes(window, visible, occluded, peek)
}

fn configure_ui_fonts(context: &egui::Context) {
    // Use the installed macOS UI font; egui's bundled fonts remain fallbacks.
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNS.ttf") {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "macos-system".into(),
            Arc::new(egui::FontData::from_owned(bytes)),
        );
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(0, "macos-system".into());
        context.set_fonts(fonts);
    }
}

fn paint_pane_frame(painter: &egui::Painter, bounds: egui::Rect, color: Color32) {
    painter.rect_stroke(
        bounds,
        0.0,
        egui::Stroke::new(1.0, color),
        egui::StrokeKind::Inside,
    );
}

/// Returns whether a visible indeterminate bar needs another frame.
fn paint_progress(
    ui: &mut egui::Ui,
    pane: Id,
    bounds: egui::Rect,
    progress: Progress,
    accent: Color32,
    offset: f32,
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
        // Metal presentation already supplies vsync backpressure. Ask for its
        // next frame instead of imposing a timer that caps animation at 30 FPS.
        ui.ctx().request_repaint();
        ui.painter()
            .rect_filled(bar, 0.0, color.gamma_multiply(0.3));
        (offset, 0.25)
    };
    ui.painter().add(
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
    fn retained_window_frames_follow_panes_geometry_and_split_dimming() {
        let mut frame = Frame::empty([20, 20]);
        frame.quads.push(rustty_render::Quad::solid(
            [0.0, 0.0, 20.0, 20.0],
            rustty_render::Color::rgb([255; 3]),
        ));
        let frame = Arc::new(frame);
        let pane = || ComposedPane {
            frame: Arc::clone(&frame),
            rect: [10.0, 15.0, 20.0, 20.0],
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
    fn retained_gpu_frames_skip_preparation_and_retry_after_errors() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("GPU adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        let mut resources = egui_wgpu::CallbackResources::default();
        let mut encoder = device.create_command_encoder(&Default::default());
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [20, 20],
            pixels_per_point: 1.0,
        };
        let mut frame = Frame::empty([20, 20]);
        frame.quads.push(rustty_render::Quad::solid(
            [0.0, 0.0, 20.0, 20.0],
            rustty_render::Color::rgb([255; 3]),
        ));
        let original = Arc::new(frame);
        let mut prepare = |frame: Arc<Frame>| {
            let paint = TerminalPaint {
                window: 1,
                frame,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
            };
            egui_wgpu::CallbackTrait::prepare(
                &paint,
                &device,
                &queue,
                &screen,
                &mut encoder,
                &mut resources,
            );
            let renderer = &resources.get::<GpuRenderers>().unwrap().0[&1];
            (renderer.prepares, renderer.frame.is_some())
        };
        assert_eq!(prepare(Arc::clone(&original)), (1, true));
        assert_eq!(prepare(Arc::clone(&original)), (1, true));
        let replacement = Arc::new((*original).clone());
        assert_eq!(prepare(Arc::clone(&replacement)), (2, true));

        let mut invalid = (*original).clone();
        invalid.quads[0].rect[0] = f32::NAN;
        assert_eq!(prepare(Arc::new(invalid)), (2, false));
        assert_eq!(prepare(replacement), (3, true));
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
                    0.0
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
                    paint_progress(ui, 1, bounds, Progress { state, value }, Color32::BLUE, 0.0),
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
    fn directory_badges_are_centered_and_readable_with_the_native_font() {
        let context = egui::Context::default();
        configure_ui_fonts(&context);
        let bounds = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 240.0));
        let accent = TabAccent::new(Some([230, 185, 60]), [0; 3]);
        for focused in [false, true] {
            for theme in [egui::ThemePreference::Dark, egui::ThemePreference::Light] {
                context.set_theme(theme);
                let mut output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(bounds),
                        ..Default::default()
                    },
                    |ui| {
                        let label = DirectoryLabel::new(Some("rustty".into()), focused, true, true)
                            .unwrap();
                        DirectoryBadge {
                            large: label.large(true),
                            label,
                            bounds,
                            active: false,
                            attention: false,
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
                    text.galley.job.sections[0].format.color,
                    if focused || theme == egui::ThemePreference::Light {
                        Color32::BLACK
                    } else {
                        Color32::WHITE
                    }
                );
                if focused {
                    assert!(output.shapes.iter().any(|shape| {
                        matches!(&shape.shape, egui::Shape::Rect(rect)
                            if rect.fill == Color32::from_rgb(230, 185, 60)
                                && rect.rect.center() == bounds.center())
                    }));
                }
            }
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
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        Pos2::ZERO,
                        Vec2::new(800.0, 600.0),
                    )),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |_| {
                    command = show_layout_picker(&context, &mut picker).or(command.take());
                },
            );
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
    }

    #[test]
    fn messages_close_by_click_or_escape_without_terminal_focus() {
        fn frame(
            context: &egui::Context,
            errors: &mut Vec<String>,
            events: Vec<egui::Event>,
        ) -> Option<egui::Rect> {
            let ui_input = !errors.is_empty();
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
                dismiss = show_messages(context, errors).map(|button| button.rect);
            });
            output.textures_delta.clear();
            dismiss
        }

        for click in [true, false] {
            let context = egui::Context::default();
            let mut errors = vec!["The text editor could not open settings".to_owned()];
            frame(&context, &mut errors, vec![]);
            let button = frame(&context, &mut errors, vec![]).unwrap();
            if click {
                let point = button.center();
                for pressed in [true, false] {
                    frame(
                        &context,
                        &mut errors,
                        vec![
                            egui::Event::PointerMoved(point),
                            egui::Event::PointerButton {
                                pos: point,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::default(),
                            },
                        ],
                    );
                }
            } else {
                frame(
                    &context,
                    &mut errors,
                    vec![egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::default(),
                    }],
                );
            }
            assert!(errors.is_empty(), "dismiss by click={click}");
            assert!(frame(&context, &mut errors, vec![]).is_none());
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
                    for data in [
                        b"".as_slice(),
                        b"plain text",
                        b"one\rtwo",
                        "日誌".as_bytes(),
                    ] {
                        assert!(!paste_needs_confirmation(&config, &terminal, data));
                    }
                    assert_eq!(
                        paste_needs_confirmation(&config, &terminal, b"one\ntwo"),
                        protection && !(bracketed && trust_brackets)
                    );
                    assert_eq!(
                        paste_needs_confirmation(&config, &terminal, b"one\x1b[201~two"),
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
}
