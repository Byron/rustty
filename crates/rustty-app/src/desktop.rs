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
        Activity, CompletionFlash, DirectoryLabel, TabAccent, common_directory_name, directory_name,
    },
    workspace::{Axis, Id, Peek, Rect, Tab, WindowState, Workspace},
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
const BLINK: Duration = Duration::from_millis(600);
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
    host_state: (bool, vt::query::ColorScheme),
    wake_pending: Arc<AtomicBool>,
    input: VecDeque<Vec<u8>>,
    input_bytes: usize,
    title: String,
    cwd: PathBuf,
    running: Option<Instant>,
    activity: Activity,
    unseen: bool,
    exited: bool,
    started: Instant,
    exit_message: Option<String>,
    links: vt::search::LinkMatcher,
}
impl Pane {
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

struct Host {
    id: Id,
    viewport: ViewportId,
    window: Arc<Window>,
    egui: egui_winit::State,
    fonts: rustty_render::Renderer,
    rects: BTreeMap<Id, egui::Rect>,
    accessibility: BTreeMap<Id, TerminalText>,
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
    mouse_button: Option<vt::MouseButton>,
    selection_drag: Option<input::SelectionDrag>,
    focused: bool,
    visible: bool,
    occluded: bool,
    deadline: Option<Instant>,
    search: Option<String>,
    search_index: usize,
    focus_text_input: bool,
    popup_open: bool,
    palette: bool,
    palette_query: String,
    confirm: Option<Confirmation>,
    clipboard_request: VecDeque<(Id, vt::Effect)>,
    capture: bool,
    frames: u64,
}
impl Host {
    fn ui_input(&self) -> bool {
        self.search.is_some()
            || self.palette
            || self.popup_open
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
    accent: Color32,
}
impl DirectoryBadge {
    fn paint(self, ui: &egui::Ui) {
        let color = if self.label.focused {
            Color32::WHITE
        } else {
            ui.visuals().text_color()
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
                Color32::from_black_alpha(235)
            } else {
                ui.visuals().widgets.inactive.bg_fill
            },
        );
        painter.rect_stroke(
            bounds,
            rounding,
            egui::Stroke::new(
                1.0,
                if self.label.focused {
                    self.accent
                } else {
                    ui.visuals().weak_text_color().gamma_multiply(0.35)
                },
            ),
            egui::StrokeKind::Inside,
        );
        if self.flash > 0.0 {
            painter.rect_filled(bounds, rounding, self.accent.gamma_multiply(self.flash));
        }
        painter.galley(bounds.min + padding, galley, color);
    }
}

struct GpuRenderers(HashMap<Id, rustty_render_wgpu::Renderer>);
struct TerminalPaint {
    window: Id,
    frame: Frame,
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
            .or_insert_with(|| rustty_render_wgpu::Renderer::new(device, self.format));
        if let Err(error) = renderer.prepare(device, queue, &self.frame) {
            eprintln!("Rustty renderer: {error}");
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
            renderer.paint(pass);
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
            "Configuration: {:?}\nOwn settings: {}",
            loaded.family,
            loaded.own_config_path.display()
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
    context.enable_accesskit();
    context.set_theme(ui_theme(&loaded.config));
    let repaint = proxy.clone();
    context.set_request_repaint_callback(move |info| {
        if let Some(deadline) = Instant::now().checked_add(info.delay) {
            let _ = repaint.send_event(Event::Repaint(info, deadline));
        }
    });
    let mut gpu_config = egui_wgpu::WgpuConfiguration::default();
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
        let visible = self.visible_panes();
        let scheme = color_scheme(self.config_loader.dark_mode);
        for (&id, pane) in &mut self.panes {
            let state = (visible.contains(&id), scheme);
            if !pane.exited && pane.host_state != state {
                match pane.session.set_host_state(state.0, state.1) {
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
        let host_state = (visible, color_scheme(self.config_loader.dark_mode));
        let session = match Session::spawn(
            self.config(),
            SessionOptions {
                working_directory: Some(directory.clone()),
                command,
                resources: self.resources.clone(),
                color_scheme: Some(host_state.1),
                visible: host_state.0,
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
        self.panes.insert(
            id,
            Pane {
                session,
                host_state,
                wake_pending,
                input: VecDeque::new(),
                input_bytes: 0,
                title: String::new(),
                cwd: directory,
                running: None,
                activity: Activity::default(),
                unseen: false,
                exited: false,
                started,
                exit_message: None,
                links: vt::search::LinkMatcher::default(),
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
            .with_title_hidden(true)
            .with_tabbing_identifier("rustty");
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
            fonts,
            rects: BTreeMap::new(),
            accessibility: BTreeMap::new(),
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
            mouse_button: None,
            selection_drag: None,
            focused: false,
            visible: !quick,
            occluded: false,
            deadline: None,
            search: None,
            search_index: 0,
            focus_text_input: false,
            popup_open: false,
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
        self.write(paste.pane, bytes);
        host.repaint();
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
        let previous = self.focused(window);
        if let Some(tab) = self.tab_mut(window) {
            tab.focus(pane);
        }
        for (id, focused) in previous
            .map(|id| (id, false))
            .into_iter()
            .chain([(pane, true)])
        {
            if let Some(pane) = self.panes.get_mut(&id) {
                if focused {
                    pane.unseen = false;
                    if let Some(platform) = &self.platform {
                        platform.clear_notifications(id);
                    }
                }
                let bytes = pane
                    .session
                    .terminal()
                    .ok()
                    .map(|terminal| terminal.encode_focus(focused));
                if let Some(bytes) = bytes {
                    let _ = pane.write(bytes);
                }
            }
        }
        self.changed();
    }
    fn drain(&mut self, id: Id) {
        let live = self
            .workspace
            .windows
            .iter()
            .any(|window| window.tabs.iter().any(|tab| tab.panes.contains_key(&id)));
        let mut close = false;
        let mut stopped = false;
        let mut clipboard = Vec::new();
        let focused = self
            .windows
            .values()
            .any(|host| host.visible && host.focused && self.focused(host.id) == Some(id));
        let config = self.config().clone();
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        let previous_title = pane.title.clone();
        let previous_status = (
            pane.activity.is_active(),
            pane.activity.reported_active(),
            pane.unseen,
        );
        pane.wake_pending.store(false, Ordering::Release);
        if let Err(error) = pane.flush()
            && !pane.session.has_exited()
        {
            self.errors.push(error.to_string());
        }
        if let Ok(terminal) = pane.session.terminal() {
            pane.title = terminal.title.clone();
            if let Some(directory) = directory_from_osc(&terminal.working_directory) {
                pane.cwd = directory;
            }
        }
        let events = pane.session.events().collect::<Vec<_>>();
        for event in events {
            match event {
                SessionEvent::Effect(vt::Effect::Title(title)) => {
                    pane.activity
                        .title_changed(&String::from_utf8_lossy(&title));
                }
                SessionEvent::Effect(vt::Effect::Progress { state, .. }) => {
                    stopped |= pane.activity.progress_reported(state, Instant::now());
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
                    close = live && !hold_after_exit(&config, runtime);
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
        let cwd = pane.cwd.clone();
        let indicators_changed = stopped
            || previous_title != pane.title
            || previous_status
                != (
                    pane.activity.is_active(),
                    pane.activity.reported_active(),
                    pane.unseen,
                );
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
                    && saved.working_directory != cwd
                {
                    saved.working_directory = cwd.clone();
                    changed = true;
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
        self.repaint_pane(id, indicators_changed);
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
                        // Ordinary navigation reveals the quadrant layer below full zoom.
                        tab.zoom = tab.quadrant_zoom;
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
                host.focus_text_input = host.palette || host.search.is_some();
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
                        Ok(true) => {}
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
                    self.write(id, b"\x0c".to_vec());
                }
            }
            Action::StartSearch | Action::SearchSelection => {
                let text = if matches!(action, Action::SearchSelection) {
                    focused
                        .and_then(|id| {
                            self.panes
                                .get(&id)?
                                .session
                                .terminal()
                                .ok()?
                                .screen()
                                .selection_text()
                        })
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                host.search = Some(text);
                host.search_index = 0;
                host.focus_text_input = true;
            }
            Action::EndSearch => host.search = None,
            Action::NavigateSearch { next } => {
                host.search_index = if next {
                    host.search_index.wrapping_add(1)
                } else {
                    host.search_index.wrapping_sub(1)
                };
                self.search(host);
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
                    let rows = screen.rows.len() as isize;
                    match action {
                        Action::ScrollToTop => screen.viewport_offset = screen.history.len(),
                        Action::ScrollToBottom => screen.viewport_offset = 0,
                        Action::ScrollPageUp => screen.scroll_viewport(rows),
                        Action::ScrollPageDown => screen.scroll_viewport(-rows),
                        Action::ScrollToSelection => {
                            if let Some(selection) = screen.selection {
                                reveal(screen, selection.start);
                            }
                        }
                        Action::JumpToPrompt(offset) => {
                            let start = screen.history.len().saturating_sub(screen.viewport_offset);
                            let prompts = screen
                                .all_rows()
                                .enumerate()
                                .filter(|(_, r)| {
                                    r.cells
                                        .iter()
                                        .any(|c| c.semantic == vt::SemanticContent::Prompt)
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
                                    screen.history.len().saturating_sub(target);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Action::OpenConfig => {
                if let Some(platform) = &self.platform
                    && let Err(error) = platform.open_config(&self.loaded.own_config_path)
                {
                    self.errors.push(error);
                }
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
        for pane in self.panes.values() {
            if let Err(error) = pane.session.apply_config(&self.loaded.config) {
                self.errors.push(error.to_string());
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
    fn search(&mut self, host: &mut Host) -> usize {
        let Some(query) = host.search.as_ref().filter(|q| !q.is_empty()) else {
            return 0;
        };
        let Some(pane) = self.focused(host.id).and_then(|id| self.panes.get(&id)) else {
            return 0;
        };
        let Ok(mut terminal) = pane.session.terminal() else {
            return 0;
        };
        let matches = terminal.screen().search_literal(query.as_bytes());
        if matches.is_empty() {
            return 0;
        }
        host.search_index %= matches.len();
        let found = &matches[host.search_index];
        let screen = terminal.screen_mut();
        screen.selection = Some(vt::Selection {
            start: found.start,
            end: found.end,
            rectangular: false,
        });
        reveal(screen, found.start);
        matches.len()
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
        for (_, pane) in self.panes.extract_if(|id, _| !retained.contains(id)) {
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
        let mut directories_changed = false;
        for window in &mut self.workspace.windows {
            for tab in &mut window.tabs {
                for (id, saved) in &mut tab.panes {
                    if let Some(pane) = self.panes.get(id)
                        && saved.working_directory != pane.cwd
                    {
                        saved.working_directory = pane.cwd.clone();
                        directories_changed = true;
                    }
                }
            }
        }
        if directories_changed {
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
    ) {
        let ui_input = host.ui_input();
        if input::key_is_consumed(
            &mut host.consumed_keys,
            key.physical_key,
            key.state,
            ui_input,
        ) {
            return;
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
                    if host.ui_input() {
                        host.consumed_keys.insert(key.physical_key);
                    }
                    return;
                }
            } else if !matched.is_empty() {
                host.sequence = matched;
                host.sequence_len += 1;
                return;
            } else {
                host.sequence.clear();
                host.sequence_len = 0;
            }
        }
        let Some(id) = self.focused(host.id) else {
            return;
        };
        if self.panes.get(&id).is_some_and(|p| p.exited) && key.state == ElementState::Pressed {
            host.consumed_keys.insert(key.physical_key);
            self.action(event_loop, host, Action::CloseSurface, true);
            return;
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
            if event.action != vt::KeyAction::Release {
                terminal.screen_mut().viewport_offset = 0;
                terminal.screen_mut().selection = None;
            }
            Some(terminal.encode_key_with_options(&event, options))
        });
        if let Some(bytes) = bytes {
            self.write(id, bytes);
        }
        host.repaint();
    }
}

fn reveal(screen: &mut vt::Screen, point: vt::GridPoint) {
    let index = screen.all_rows().position(|row| row.id == point.row);
    if let Some(index) = index {
        screen.viewport_offset = screen.history.len().saturating_sub(index);
    }
}

impl App {
    fn draw(&mut self, event_loop: &ActiveEventLoop, host: &mut Host) -> Result<()> {
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
        let header_height = if active.panes.len() > 1 { 18.0 } else { 0.0 };
        let scale = host.window.scale_factor() as f32;
        let size = host.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let mut raw = host.egui.take_egui_input(&host.window);
        raw.viewport_id = host.viewport;
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
        let mut tab_selection = None;
        let mut presentation_changed = false;
        let mut retry_pane = None;
        let mut render_error = None;
        let format = self
            .painter
            .render_state()
            .ok_or("GPU unavailable")?
            .target_format;
        let blink_on = (self.started.elapsed().as_millis() / BLINK.as_millis()).is_multiple_of(2);
        let mut needs_blink = false;
        let mut graphics_deadline: Option<Instant> = None;
        if host
            .navigation_warning
            .is_some_and(|(_, until)| until <= Instant::now())
        {
            host.navigation_warning = None;
        }

        let title = self
            .panes
            .get(&focused)
            .map(|pane| {
                if pane.title.is_empty() {
                    pane.cwd.display().to_string()
                } else {
                    pane.title.clone()
                }
            })
            .unwrap_or_else(|| "Rustty".into());
        host.window.set_title(&format!("{title} — Rustty"));
        host.accessibility.clear();
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
                            let pane = self.panes.get(&tab.focused);
                            let title = tab
                                .title
                                .as_deref()
                                .filter(|s| !s.is_empty())
                                .or_else(|| {
                                    pane.map(|p| p.title.as_str()).filter(|s| !s.is_empty())
                                })
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
                            let is_active = tab.panes.keys().any(|id| {
                                self.panes.get(id).is_some_and(|p| p.activity.is_active())
                            });
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
                                self.flash_opacity(tab.id, now, &mut graphics_deadline) * 0.45;
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
                host.composing = false;
                host.preedit.clear();
                host.preedit_selection = None;
            }
            if host.search.is_some() {
                egui::Panel::bottom("search").show(root_ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Find");
                        let focus = !host.palette
                            && !ui.is_sizing_pass()
                            && std::mem::take(&mut host.focus_text_input);
                        let response = input::text_edit(
                            ui,
                            host.search.as_mut().unwrap(),
                            egui::Id::new(("search", host.id)),
                            focus,
                        );
                        if response.changed() {
                            host.search_index = 0;
                            self.search(host);
                        }
                        if ui.button("Previous").clicked() {
                            commands.push(Action::NavigateSearch { next: false });
                        }
                        if ui.button("Next").clicked()
                            || response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            commands.push(Action::NavigateSearch { next: true });
                        }
                        if ui.button("Done").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Escape))
                        {
                            commands.push(Action::EndSearch);
                        }
                    });
                });
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
                    let mut composed = Frame::empty([size.width, size.height]);
                    let mut accessible = Vec::new();
                    let mut terminal_ime_rect = None;
                    // An atlas eviction can happen halfway through a multi-pane frame.
                    // Rebuild against its new generation before handing a frame to WGPU.
                    for attempt in 0..2 {
                        composed = Frame::empty([size.width, size.height]);
                        accessible.clear();
                        let mut retry = false;
                        for (&id, &rect) in &host.rects {
                            let Some(pane) = self.panes.get(&id) else {
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
                            if let Ok(mut terminal) = pane.session.terminal() {
                                let now_ms =
                                    self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                                if let Some(next) = terminal.tick_graphics(now_ms) {
                                    let deadline = Instant::now()
                                        + Duration::from_millis(next.saturating_sub(now_ms));
                                    graphics_deadline = Some(
                                        graphics_deadline.map_or(deadline, |old| old.min(deadline)),
                                    );
                                }
                            }
                            let snapshot = match pane.session.snapshot() {
                                Ok(snapshot) => snapshot,
                                Err(error) => {
                                    render_error = Some(error.to_string());
                                    continue;
                                }
                            };
                            let is_focused = host.focused && id == focused && host.peek.is_none();
                            needs_blink |= is_focused
                                && !pane.exited
                                && snapshot.screen.cursor.visible
                                && snapshot.screen.cursor.blink;
                            let cursor_color = snapshot.cursor_color.unwrap_or(snapshot.foreground);
                            let resolve = |color: config::TerminalColor| match color {
                                config::TerminalColor::Rgb(color) => [color.r, color.g, color.b],
                                config::TerminalColor::CellForeground => snapshot.foreground,
                                config::TerminalColor::CellBackground => snapshot.background,
                            };
                            let options = RenderOptions {
                                size: [physical.x.max(1.0) as u32, physical.y.max(1.0) as u32],
                                padding,
                                foreground: snapshot.foreground,
                                background: snapshot.background,
                                cursor_color,
                                cursor_text: config
                                    .cursor_text
                                    .map(resolve)
                                    .unwrap_or(snapshot.background),
                                selection_background: config
                                    .selection_background
                                    .map(resolve)
                                    .unwrap_or([65, 85, 120]),
                                selection_foreground: config.selection_foreground.map(resolve),
                                palette: snapshot
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
                            let mut ime_cursor = None;
                            match host.fonts.prepare(&snapshot.screen, &options) {
                                Ok(frame) => {
                                    ime_cursor = frame.ime_cursor;
                                    if composed
                                        .append_clipped(
                                            &frame,
                                            [rect.left() * scale, rect.top() * scale],
                                            [
                                                rect.left() * scale,
                                                rect.top() * scale,
                                                physical.x,
                                                physical.y,
                                            ],
                                        )
                                        .is_err()
                                    {
                                        retry = true;
                                        break;
                                    }
                                }
                                Err(error) => render_error = Some(error.to_string()),
                            }
                            if id != focused && config.unfocused_split_opacity < 1.0 {
                                let color =
                                    config.unfocused_split_fill.unwrap_or(config.background);
                                composed.quads.push(rustty_render::Quad::solid(
                                    [
                                        rect.left() * scale,
                                        rect.top() * scale,
                                        physical.x,
                                        physical.y,
                                    ],
                                    rustty_render::Color::rgb([color.r, color.g, color.b])
                                        .opacity(1.0 - config.unfocused_split_opacity),
                                ));
                            }
                            host.accessibility.insert(
                                id,
                                TerminalText::new(
                                    id,
                                    &snapshot.screen,
                                    [
                                        rect.left() + padding[0] / scale,
                                        rect.top() + padding[1] / scale,
                                    ],
                                    [
                                        metrics.cell_width as f32 / scale,
                                        metrics.cell_height as f32 / scale,
                                    ],
                                ),
                            );
                            accessible.push((id, rect));
                            if id == focused && !pane.exited {
                                let cursor = &snapshot.screen.cursor;
                                let [x, y, width, height] = ime_cursor.unwrap_or([
                                    padding[0] + cursor.col as f32 * metrics.cell_width as f32,
                                    padding[1] + cursor.row as f32 * metrics.cell_height as f32,
                                    metrics.cell_width as f32,
                                    metrics.cell_height as f32,
                                ]);
                                terminal_ime_rect = Some(egui::Rect::from_min_size(
                                    Pos2::new(rect.left() + x / scale, rect.top() + y / scale),
                                    Vec2::new(width / scale, height / scale),
                                ));
                            }
                        }
                        if !retry {
                            break;
                        }
                        if attempt == 1 {
                            render_error =
                                Some("Visible glyphs exceed the shared atlas budget".into());
                            composed = Frame::empty([size.width, size.height]);
                        }
                    }
                    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                        egui::Rect::from_min_size(
                            Pos2::ZERO,
                            Vec2::new(size.width as f32 / scale, size.height as f32 / scale),
                        ),
                        TerminalPaint {
                            window: host.id,
                            frame: composed,
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
                            ui.painter().rect_stroke(
                                rect,
                                0.0,
                                egui::Stroke::new(if selected { 1.5 } else { 0.5 }, color),
                                egui::StrokeKind::Inside,
                            );
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
                                ui.painter().rect_stroke(
                                    bounds,
                                    0.0,
                                    egui::Stroke::new(2.0, accent),
                                    egui::StrokeKind::Inside,
                                );
                            }
                        }
                        if let Some(label) = self.quadrant_label(active, host, id) {
                            shared_labels.extend(members.iter().copied());
                            DirectoryBadge {
                                label,
                                bounds,
                                large: true,
                                attention,
                                accent,
                                active: members.iter().any(|id| {
                                    self.panes
                                        .get(id)
                                        .is_some_and(|pane| pane.activity.is_active())
                                }),
                                flash: self.flash_opacity(id, now, &mut graphics_deadline),
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
                            false,
                        ) {
                            let large = label.large(
                                active.quadrant_zoom.is_some()
                                    && (host.peek.is_some() || active.zoom == active.quadrant_zoom),
                            );
                            let flash = if label.shows_flash(true, false) {
                                self.flash_opacity(id, now, &mut graphics_deadline)
                            } else {
                                0.0
                            };
                            DirectoryBadge {
                                label,
                                bounds,
                                large,
                                flash,
                                accent,
                                active: pane.activity.is_active(),
                                attention: pane.unseen,
                            }
                            .paint(ui);
                        }
                    }
                });
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
                                host.focus_text_input = host.search.is_some();
                            }
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            host.palette = false;
                            host.focus_text_input = host.search.is_some();
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
            if !self.errors.is_empty() {
                egui::Window::new("Rustty messages")
                    .default_size([600.0, 240.0])
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .show(ui, |ui| {
                                for error in &self.errors {
                                    ui.label(error);
                                }
                            });
                        if ui.button("Dismiss").clicked() {
                            self.errors.clear();
                        }
                    });
            }
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
            host.peek = None;
            host.repaint();
        }
        if let Some(id) = retry_pane {
            self.failed_panes.remove(&id);
        }
        for action in commands {
            self.action(event_loop, host, action, false);
        }
        if let Some(update) = &mut output.platform_output.accesskit_update {
            for text in host.accessibility.values() {
                text.append_to(update);
            }
        }
        host.egui.handle_platform_output_with_event_loop(
            &host.window,
            event_loop,
            output.platform_output,
        );
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
        host.frames += 1;
        host.deadline = graphics_deadline;
        if let Some((_, deadline)) = host.navigation_warning {
            host.deadline = Some(host.deadline.map_or(deadline, |old| old.min(deadline)));
        }
        if needs_blink {
            let elapsed = self.started.elapsed().as_millis();
            let deadline = Instant::now()
                + Duration::from_millis((BLINK.as_millis() - elapsed % BLINK.as_millis()) as u64);
            host.deadline = Some(host.deadline.map_or(deadline, |old| old.min(deadline)));
        }
        Ok(())
    }
    fn mouse(&mut self, host: &mut Host, action: vt::MouseAction, button: Option<vt::MouseButton>) {
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
        if !host.ui_input() && host.peek.is_none() {
            let divider = self.tab(host.id).and_then(|tab| {
                tab.visible_tree(false)
                    .divider_at(host.content, [host.mouse.x, host.mouse.y], 3.0)
            });
            host.window.set_cursor(match divider {
                Some((_, Axis::Horizontal, _)) => CursorIcon::ColResize,
                Some((_, Axis::Vertical, _)) => CursorIcon::RowResize,
                None => CursorIcon::Default,
            });
            if let Some(divider) = divider {
                if action == vt::MouseAction::Press && button == Some(vt::MouseButton::Left) {
                    self.remember();
                    host.divider_drag = Some(divider);
                }
                return;
            }
        }
        let hit = host
            .rects
            .iter()
            .find(|(_, rect)| rect.contains(host.mouse))
            .map(|(&id, _)| id);
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
        if action == vt::MouseAction::Press
            && let Some(id) = hit
        {
            self.focus_pane(host.id, id);
        }
        let Some(id) = self.focused(host.id) else {
            return;
        };
        let Some(rect) = host.rects.get(&id) else {
            return;
        };
        let scale = host.window.scale_factor() as f32;
        let metrics = host.fonts.metrics();
        let position = (host.mouse - rect.min) * scale
            - Vec2::new(
                self.config().window_padding_x.start * scale,
                (self.config().window_padding_y.start
                    + if self.tab(host.id).is_some_and(|t| t.panes.len() > 1) {
                        18.0
                    } else {
                        0.0
                    })
                    * scale,
            );
        let config = &self.loaded.config;
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        let Ok(mut terminal) = pane.session.terminal() else {
            return;
        };
        let col = (position.x.max(0.0) as usize / metrics.cell_width as usize)
            .min(terminal.cols as usize - 1);
        let row = (position.y.max(0.0) as usize / metrics.cell_height as usize)
            .min(terminal.rows as usize - 1);
        let event = vt::MouseEvent {
            action,
            button,
            col,
            row,
            x: f64::from(position.x),
            y: f64::from(position.y),
            modifiers: input::terminal_modifiers(host.modifiers.state()),
        };
        if terminal.mouse_mode != 0 && !host.modifiers.state().shift_key() {
            let bytes = terminal.encode_mouse(event);
            drop(terminal);
            self.write(id, bytes);
            host.repaint();
            return;
        }
        let screen = terminal.screen_mut();
        let point = screen
            .viewport()
            .nth(row)
            .map(|r| vt::GridPoint { row: r.id, col });
        if let Some(point) = point {
            if action == vt::MouseAction::Press && button == Some(vt::MouseButton::Left) {
                if host.modifiers.state().super_key() && config.link_url {
                    let links = pane.links.links(&screen.snapshot_viewport());
                    if let Some(link) = links.iter().find(|link| link.contains(screen, point))
                        && let Some(platform) = &self.platform
                        && let Err(error) = platform.open_url(&link.uri)
                    {
                        self.errors.push(error);
                    }
                } else {
                    host.selection_drag = Some(input::SelectionDrag::new(point, host.mouse));
                    screen.selection = None;
                }
            } else if action == vt::MouseAction::Move
                && host.mouse_button == Some(vt::MouseButton::Left)
            {
                if let Some(drag) = &mut host.selection_drag
                    && let Some(selection) =
                        drag.update(host.mouse, point, host.modifiers.state().alt_key())
                {
                    screen.selection = Some(selection);
                }
            } else if action == vt::MouseAction::Release && host.selection_drag.take().is_some() {
                let copy = config.copy_on_select;
                let text = screen.selection_text();
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
                self.drain(id);
                self.reconcile(event_loop);
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
                                .accessibility
                                .iter()
                                .find(|(_, text)| text.contains(request.target_node))
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
                                            && let Some(selection) =
                                                host.accessibility[&id].selection(*range)
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
                        AccessEvent::InitialTreeRequested => self.context.enable_accesskit(),
                        AccessEvent::AccessibilityDeactivated => {}
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
        let response = host.egui.on_window_event(&host.window, &event);
        if response.repaint && !matches!(event, WindowEvent::RedrawRequested) {
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
                    if let Some(pane) = self.focused(host.id).and_then(|id| self.panes.get_mut(&id))
                    {
                        let bytes = pane
                            .session
                            .terminal()
                            .ok()
                            .map(|terminal| terminal.encode_focus(false));
                        if let Some(bytes) = bytes {
                            let _ = pane.write(bytes);
                        }
                    }
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
            } => self.keyboard(event_loop, &mut host, &event),
            WindowEvent::Ime(ime) if !host.ui_input() => {
                match ime {
                    Ime::Preedit(text, selection) => {
                        host.composing = !text.is_empty();
                        host.preedit = text;
                        host.preedit_selection = selection;
                    }
                    Ime::Commit(text) => {
                        host.composing = false;
                        host.preedit.clear();
                        host.preedit_selection = None;
                        if let Some(pane) = self.focused(host.id) {
                            self.write(pane, text.into_bytes());
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
                if !host.ui_input() {
                    let button = host.mouse_button;
                    self.mouse(&mut host, vt::MouseAction::Move, button);
                }
            }
            WindowEvent::MouseInput { state, button, .. } if !host.ui_input() => {
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
            WindowEvent::MouseWheel { delta, .. } if !host.ui_input() => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(pos) => {
                        pos.y as f32 / host.fonts.metrics().cell_height as f32
                    }
                };
                if let Some(id) = self.focused(host.id) {
                    let mouse = self
                        .panes
                        .get(&id)
                        .and_then(|p| p.session.terminal().ok().map(|t| t.mouse_mode != 0))
                        .unwrap_or(false);
                    if mouse && !host.modifiers.state().shift_key() {
                        for _ in 0..lines.abs().ceil().min(128.0) as usize {
                            self.mouse(
                                &mut host,
                                vt::MouseAction::Press,
                                Some(if lines > 0.0 {
                                    vt::MouseButton::WheelUp
                                } else {
                                    vt::MouseButton::WheelDown
                                }),
                            );
                        }
                    } else if let Some(pane) = self.panes.get(&id)
                        && let Ok(mut terminal) = pane.session.terminal()
                    {
                        terminal
                            .screen_mut()
                            .scroll_viewport((lines * 3.0).round() as isize);
                    }
                }
                host.repaint();
            }
            WindowEvent::DroppedFile(path) => {
                if let Some(id) = self.focused(host.id) {
                    let escaped = format!("'{}' ", path.to_string_lossy().replace('\'', "'\\''"));
                    let bytes = self
                        .panes
                        .get(&id)
                        .and_then(|p| p.session.terminal().ok().map(|t| t.encode_paste(&escaped)));
                    if let Some(bytes) = bytes {
                        self.write(id, bytes);
                    }
                }
            }
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
            .chain(
                self.panes
                    .values()
                    .filter_map(|pane| pane.activity.deadline()),
            )
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
