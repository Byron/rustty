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
    workspace::{Axis, Id, Peek, Rect, Tab, WindowState, Workspace},
};
use rustty_font::{FontConfig, FontFeature};
use rustty_render::{Frame, RenderOptions};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
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
    event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::ModifiersState,
    platform::macos::WindowAttributesExtMacOS,
    window::{CursorIcon, Fullscreen, Window, WindowId},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const BLINK: Duration = Duration::from_millis(600);
const INPUT_BUDGET: usize = 16 * 1024 * 1024;

#[derive(Debug)]
enum Event {
    Output(Id),
    Platform(PlatformEvent),
    Repaint(ViewportId, Duration),
    Access(egui_winit::accesskit_winit::Event),
}
impl From<egui_winit::accesskit_winit::Event> for Event {
    fn from(value: egui_winit::accesskit_winit::Event) -> Self {
        Self::Access(value)
    }
}

struct Pane {
    session: Session,
    wake_pending: Arc<AtomicBool>,
    input: VecDeque<Vec<u8>>,
    input_bytes: usize,
    title: String,
    cwd: PathBuf,
    running: Option<Instant>,
    unseen: bool,
    exited: bool,
    started: Instant,
    exit_message: Option<String>,
}
impl Pane {
    fn write(&mut self, bytes: Vec<u8>) -> std::io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        if self.input.is_empty() {
            match self.session.write(&bytes) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
        }
        if self.input_bytes.saturating_add(bytes.len()) > INPUT_BUDGET {
            return Err(std::io::Error::other(
                "Input queue is full. Wait for the command to read its input.",
            ));
        }
        self.input_bytes += bytes.len();
        self.input.push_back(bytes);
        Ok(())
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
    modifiers: ModifiersState,
    sequence: Vec<usize>,
    sequence_len: usize,
    composing: bool,
    preedit: String,
    preedit_selection: Option<(usize, usize)>,
    peek: Option<Peek>,
    navigation_warning: Option<(Id, Instant)>,
    mouse: Pos2,
    mouse_button: Option<vt::MouseButton>,
    selection_anchor: Option<vt::GridPoint>,
    focused: bool,
    visible: bool,
    occluded: bool,
    deadline: Option<Instant>,
    search: Option<String>,
    search_index: usize,
    palette: bool,
    palette_query: String,
    confirm: Option<Action>,
    clipboard_request: Option<(Id, String, Option<Vec<u8>>)>,
    capture: bool,
    frames: u64,
}
impl Host {
    fn ui_input(&self) -> bool {
        self.search.is_some()
            || self.palette
            || self.confirm.is_some()
            || self.clipboard_request.is_some()
    }
    fn repaint(&self) {
        if self.visible && !self.occluded {
            self.window.request_redraw();
        }
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
    workspace: Workspace,
    state_path: PathBuf,
    resources: Option<PathBuf>,
    panes: HashMap<Id, Pane>,
    failed_panes: BTreeMap<Id, String>,
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
    primary_selection: Option<String>,
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
    let mut loaded = Config::load_with_args(&args)?;
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
    let resources = resource_dir();
    let event_loop = EventLoop::<Event>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let context = egui::Context::default();
    context.enable_accesskit();
    context.set_theme(ui_theme(&loaded.config));
    let repaint = proxy.clone();
    context.set_request_repaint_callback(move |info| {
        let _ = repaint.send_event(Event::Repaint(info.viewport_id, info.delay));
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
        workspace,
        state_path,
        resources,
        panes: HashMap::new(),
        failed_panes: BTreeMap::new(),
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
        primary_selection: None,
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
        self.workspace.windows.push(WindowState {
            id,
            tabs: vec![Tab::new(tab, pane, directory)],
            active_tab: 0,
            frame: [100.0, 100.0, 1000.0, 680.0],
            quick,
        });
        self.changed();
        id
    }
    fn spawn_pane(&mut self, id: Id, directory: PathBuf) -> Result<()> {
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
        let session = match Session::spawn(
            self.config(),
            SessionOptions {
                working_directory: Some(directory.clone()),
                command,
                resources: self.resources.clone(),
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
                wake_pending,
                input: VecDeque::new(),
                input_bytes: 0,
                title: String::new(),
                cwd: directory,
                running: None,
                unseen: false,
                exited: false,
                started,
                exit_message: None,
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
            && let Some(bounds) = self
                .platform
                .as_ref()
                .and_then(|platform| platform.quick_terminal_frame(self.config()))
        {
            frame = bounds;
        }
        let attributes = Window::default_attributes()
            .with_title("Rustty")
            .with_decorations(!quick)
            .with_visible(false)
            .with_inner_size(LogicalSize::new(frame[2].max(320.0), frame[3].max(180.0)))
            .with_position(LogicalPosition::new(frame[0], frame[1]))
            .with_min_inner_size(LogicalSize::new(240.0, 120.0))
            .with_transparent(self.config().background_opacity < 1.0)
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
            .with_title_hidden(true)
            .with_tabbing_identifier("rustty");
        let window = Arc::new(event_loop.create_window(attributes)?);
        if let Some(platform) = &self.platform {
            platform.configure_window(&window, quick, self.config())?;
        }
        window.set_ime_allowed(true);
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
                self.spawn_pane(pane, saved.working_directory.clone())?;
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
            modifiers: ModifiersState::empty(),
            sequence: Vec::new(),
            sequence_len: 0,
            composing: false,
            preedit: String::new(),
            preedit_selection: None,
            peek: None,
            navigation_warning: None,
            mouse: Pos2::ZERO,
            mouse_button: None,
            selection_anchor: None,
            focused: false,
            visible: !quick,
            occluded: false,
            deadline: None,
            search: None,
            search_index: 0,
            palette: false,
            palette_query: String::new(),
            confirm: None,
            clipboard_request: None,
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
                platform.hide_quick(&host.window, restore_focus)
            }
        } else {
            Err("macOS window services are unavailable".into())
        };
        match result {
            Ok(()) => {
                host.visible = visible;
                host.repaint();
            }
            Err(error) => self.errors.push(error),
        }
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
        let focused = self
            .windows
            .values()
            .any(|host| host.visible && host.focused && self.focused(host.id) == Some(id));
        let config = self.config().clone();
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
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
                SessionEvent::Effect(vt::Effect::CommandStart) => {
                    pane.running = Some(Instant::now())
                }
                SessionEvent::Effect(vt::Effect::CommandEnd { exit_code }) => {
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
                SessionEvent::Effect(vt::Effect::Clipboard { selection, data }) => {
                    let host = self.windows.values_mut().find(|host| {
                        self.workspace
                            .windows
                            .iter()
                            .find(|w| w.id == host.id)
                            .is_some_and(|w| w.tabs.iter().any(|t| t.panes.contains_key(&id)))
                    });
                    if let Some(host) = host {
                        let policy = if data.is_some() {
                            config.clipboard_write
                        } else {
                            config.clipboard_read
                        };
                        match policy {
                            config::ClipboardAccess::Deny => {}
                            config::ClipboardAccess::Ask => {
                                host.clipboard_request = Some((id, selection, data));
                                host.repaint();
                            }
                            config::ClipboardAccess::Allow => {
                                if let Some(data) = data {
                                    host.egui.set_clipboard_text(
                                        String::from_utf8_lossy(&data).into_owned(),
                                    );
                                } else if let Some(text) = host.egui.clipboard_text() {
                                    let _ = pane.write(clipboard_reply(&selection, &text));
                                }
                            }
                        }
                    }
                }
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
        let cwd = pane.cwd.clone();
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
        for host in self.windows.values() {
            if self
                .tab(host.id)
                .is_some_and(|tab| tab.panes.contains_key(&id))
            {
                host.repaint();
            }
        }
    }
}

fn clipboard_reply(selection: &str, text: &str) -> Vec<u8> {
    use base64::Engine;
    format!(
        "\x1b]52;{};{}\x1b\\",
        selection,
        base64::engine::general_purpose::STANDARD.encode(text)
    )
    .into_bytes()
}

impl App {
    fn action(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
        action: Action,
        approved: bool,
    ) -> bool {
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
                host.confirm = Some(action);
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
                if let Err(error) = self.spawn_pane(pane, directory) {
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
                if let Err(error) = self.spawn_pane(pane, directory) {
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
                        host.peek = tab.begin_peek(input::modifiers(host.modifiers));
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
                let text = if action == Action::PasteFromSelection {
                    self.primary_selection.clone()
                } else {
                    host.egui.clipboard_text()
                };
                if let (Some(id), Some(text)) = (focused, text) {
                    let bytes = self.panes.get(&id).and_then(|pane| {
                        pane.session.terminal().ok().map(|t| t.encode_paste(&text))
                    });
                    if let Some(bytes) = bytes {
                        self.write(id, bytes);
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
            Action::ReloadConfig => match Config::load() {
                Ok(loaded) => {
                    self.errors = loaded.diagnostics.iter().map(ToString::to_string).collect();
                    self.loaded = loaded;
                    self.failed_panes.clear();
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
                    self.update_fonts(host);
                    if let Some(platform) = &self.platform {
                        for window in std::iter::once(&*host).chain(self.windows.values()) {
                            let quick = self
                                .workspace
                                .windows
                                .iter()
                                .any(|state| state.id == window.id && state.quick);
                            if let Err(error) =
                                platform.configure_window(&window.window, quick, self.config())
                            {
                                self.errors.push(error);
                            }
                        }
                    }
                }
                Err(error) => self.errors.push(error.to_string()),
            },
            Action::IncreaseFontSize(amount) => {
                self.loaded.config.font_size = (self.config().font_size + amount).clamp(4.0, 200.0);
                self.update_fonts(host);
            }
            Action::DecreaseFontSize(amount) => {
                self.loaded.config.font_size = (self.config().font_size - amount).clamp(4.0, 200.0);
                self.update_fonts(host);
            }
            Action::ResetFontSize => {
                if let Ok(loaded) = Config::load() {
                    self.loaded.config.font_size = loaded.config.font_size;
                    self.update_fonts(host);
                }
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
    fn update_fonts(&mut self, host: &mut Host) {
        for current in std::iter::once(host).chain(self.windows.values_mut()) {
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
        let Ok(regex) = regex::Regex::new(&regex::escape(query)) else {
            return 0;
        };
        let matches = terminal.screen().search(&regex);
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
        let removed = self
            .windows
            .iter()
            .filter(|(_, host)| self.index(host.id).is_none())
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in removed {
            if let Some(host) = self.windows.remove(&id) {
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
            if let Err(error) = self.spawn_pane(id, directory) {
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
        if host.ui_input() {
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
                            .is_some_and(|trigger| input::matches(trigger, key, host.modifiers))
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
            self.action(event_loop, host, Action::CloseSurface, true);
            return;
        }
        let bytes = self.panes.get(&id).and_then(|pane| {
            let mut terminal = pane.session.terminal().ok()?;
            let event = input::terminal_key(key, host.modifiers, host.composing)?;
            if event.action != vt::KeyAction::Release {
                terminal.screen_mut().viewport_offset = 0;
                terminal.screen_mut().selection = None;
            }
            Some(terminal.encode_key(&event))
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
        if !host.ui_input() {
            raw.events.retain(|e| {
                !matches!(
                    e,
                    egui::Event::Key { .. }
                        | egui::Event::Text(_)
                        | egui::Event::Paste(_)
                        | egui::Event::Copy
                        | egui::Event::Cut
                        | egui::Event::Ime(_)
                )
            });
        }
        let context = self.context.clone();
        let mut commands = Vec::new();
        let mut tab_selection = None;
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
                            let running = tab
                                .panes
                                .keys()
                                .filter(|id| {
                                    self.panes.get(id).is_some_and(|p| p.running.is_some())
                                })
                                .count();
                            let attention = tab
                                .panes
                                .keys()
                                .any(|id| self.panes.get(id).is_some_and(|p| p.unseen));
                            let suffix = if attention {
                                " ●".into()
                            } else if running > 0 {
                                format!(" ▪ {running}")
                            } else {
                                String::new()
                            };
                            let label = format!(
                                "{}{suffix}{}",
                                title.chars().take(26).collect::<String>(),
                                if tab.zoom.is_some() { " ◩" } else { "" }
                            );
                            let response = ui.selectable_label(index == state.active_tab, label);
                            if response.clicked() {
                                tab_selection = Some(index);
                            }
                            response.context_menu(|ui| {
                                let window_index = self.index(host.id).unwrap();
                                let actual = &mut self.workspace.windows[window_index].tabs[index];
                                ui.label("Tab title");
                                ui.text_edit_singleline(
                                    actual.title.get_or_insert_with(String::new),
                                );
                                let color = actual.color.get_or_insert([100, 150, 230]);
                                ui.color_edit_button_srgb(color);
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
            if host.search.is_some() {
                egui::Panel::bottom("search").show(root_ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Find");
                        let response = ui.text_edit_singleline(host.search.as_mut().unwrap());
                        if response.changed() {
                            host.search_index = 0;
                            self.search(host);
                        }
                        if !response.has_focus() && !ctx.egui_wants_keyboard_input() {
                            response.request_focus();
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
                            if id == focused {
                                let cursor = &snapshot.screen.cursor;
                                let [x, y, width, height] = ime_cursor.unwrap_or([
                                    padding[0] + cursor.col as f32 * metrics.cell_width as f32,
                                    padding[1] + cursor.row as f32 * metrics.cell_height as f32,
                                    metrics.cell_width as f32,
                                    metrics.cell_height as f32,
                                ]);
                                host.window.set_ime_cursor_area(
                                    LogicalPosition::new(
                                        rect.left() + x / scale,
                                        rect.top() + y / scale,
                                    ),
                                    LogicalSize::new(width / scale, height / scale),
                                );
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
                        if id == focused && host.focused && !host.ui_input() {
                            response.request_focus();
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
                                Color32::from_rgb(112, 165, 245)
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
                            if host.peek.is_none()
                                && let Some(pane) = self.panes.get(&id)
                            {
                                let directory = pane
                                    .cwd
                                    .file_name()
                                    .unwrap_or(pane.cwd.as_os_str())
                                    .to_string_lossy();
                                let label = format!(
                                    "{}{}",
                                    directory,
                                    if pane.unseen {
                                        " ●"
                                    } else if pane.running.is_some() {
                                        " ▪"
                                    } else {
                                        ""
                                    }
                                );
                                let label_pos = rect.left_top() + Vec2::new(8.0, 4.0);
                                let galley = ui.painter().layout_no_wrap(
                                    label,
                                    egui::FontId::proportional(11.0),
                                    if selected {
                                        Color32::WHITE
                                    } else {
                                        Color32::LIGHT_GRAY
                                    },
                                );
                                ui.painter().rect_filled(
                                    egui::Rect::from_min_size(
                                        label_pos - Vec2::splat(2.0),
                                        galley.size() + Vec2::splat(4.0),
                                    ),
                                    3.0,
                                    rgb(config.background).gamma_multiply(0.9),
                                );
                                ui.painter().galley(label_pos, galley, Color32::WHITE);
                            }
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
                    if let Some(peek) = host.peek {
                        let mut quadrants = BTreeMap::<Id, egui::Rect>::new();
                        for (&pane, &rect) in &host.rects {
                            if let Some(quadrant) = active.root.quadrant(pane) {
                                quadrants
                                    .entry(quadrant)
                                    .and_modify(|bounds| *bounds = bounds.union(rect))
                                    .or_insert(rect);
                            }
                        }
                        let selected = active.root.quadrant(peek.target);
                        for (id, bounds) in quadrants {
                            if !peek.navigation_used && Some(id) != selected {
                                ui.painter().rect_filled(
                                    bounds,
                                    0.0,
                                    Color32::from_black_alpha(
                                        (config.quadrant_peek_opacity * 255.0) as u8,
                                    ),
                                );
                            }
                            if Some(id) == selected {
                                ui.painter().rect_stroke(
                                    bounds,
                                    0.0,
                                    egui::Stroke::new(2.0, Color32::from_rgb(112, 165, 245)),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            let pane =
                                active.activate_quadrant(active.root.node(id).unwrap().panes()[0]);
                            if let Some(pane) = pane.and_then(|id| self.panes.get(&id)) {
                                let label = pane
                                    .cwd
                                    .file_name()
                                    .unwrap_or(pane.cwd.as_os_str())
                                    .to_string_lossy();
                                let galley = ui.painter().layout_no_wrap(
                                    label.into_owned(),
                                    egui::FontId::proportional(18.0),
                                    Color32::WHITE,
                                );
                                let label_bounds =
                                    egui::Rect::from_center_size(bounds.center(), galley.size());
                                ui.painter().rect_filled(
                                    label_bounds.expand(7.0),
                                    5.0,
                                    Color32::from_black_alpha(220),
                                );
                                ui.painter()
                                    .galley(label_bounds.min, galley, Color32::WHITE);
                            }
                        }
                    }
                });
            if host.palette {
                egui::Window::new("Command palette")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_TOP, [0.0, 60.0])
                    .show(ctx, |ui| {
                        let response = ui.text_edit_singleline(&mut host.palette_query);
                        if !ctx.egui_wants_keyboard_input() {
                            response.request_focus();
                        }
                        for (label, action) in palette_actions() {
                            if label
                                .to_lowercase()
                                .contains(&host.palette_query.to_lowercase())
                                && ui.button(label).clicked()
                            {
                                commands.push(action);
                                host.palette = false;
                            }
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            host.palette = false;
                        }
                    });
            }
            if let Some(action) = host.confirm.clone() {
                egui::Window::new("Close running commands?")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label("Closing this terminal will stop its running commands.");
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                host.confirm = None;
                            }
                            if ui.button("Close").clicked() {
                                host.confirm = None;
                                self.action(event_loop, host, action, true);
                            }
                        });
                    });
            }
            if let Some((pane, selection, data)) = host.clipboard_request.clone() {
                egui::Window::new("Terminal clipboard access")
                    .collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(if data.is_some() {
                            "A terminal program wants to replace the clipboard."
                        } else {
                            "A terminal program wants to read the clipboard."
                        });
                        ui.horizontal(|ui| {
                            if ui.button("Deny").clicked() {
                                host.clipboard_request = None;
                            }
                            if ui.button("Allow once").clicked() {
                                if let Some(data) = data {
                                    host.egui.set_clipboard_text(
                                        String::from_utf8_lossy(&data).into_owned(),
                                    );
                                } else if let Some(text) = host.egui.clipboard_text() {
                                    self.write(pane, clipboard_reply(&selection, &text));
                                }
                                host.clipboard_request = None;
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
        let Some(pane) = self.panes.get(&id) else {
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
            modifiers: input::terminal_modifiers(host.modifiers),
        };
        if terminal.mouse_mode != 0 && !host.modifiers.shift_key() {
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
                if host.modifiers.super_key() && self.config().link_url {
                    let mut matcher = vt::search::LinkMatcher::default();
                    let links = matcher.links(&screen.snapshot_viewport());
                    if let Some(link) = links.iter().find(|link| {
                        (link.start.row, link.start.col) <= (point.row, point.col)
                            && (link.end.row, link.end.col) >= (point.row, point.col)
                    }) && let Some(platform) = &self.platform
                        && let Err(error) = platform.open_url(&link.uri)
                    {
                        self.errors.push(error);
                    }
                } else {
                    host.selection_anchor = Some(point);
                    screen.selection = None;
                }
            } else if action == vt::MouseAction::Move
                && host.mouse_button == Some(vt::MouseButton::Left)
            {
                if let Some(start) = host.selection_anchor {
                    screen.selection = Some(vt::Selection {
                        start,
                        end: point,
                        rectangular: host.modifiers.alt_key(),
                    });
                }
            } else if action == vt::MouseAction::Release {
                host.selection_anchor = None;
                if matches!(
                    self.config().copy_on_select,
                    config::CopyOnSelect::Primary | config::CopyOnSelect::Both
                ) {
                    self.primary_selection = screen.selection_text();
                }
                if matches!(
                    self.config().copy_on_select,
                    config::CopyOnSelect::Clipboard | config::CopyOnSelect::Both
                ) && let Some(text) = screen.selection_text()
                {
                    host.egui.set_clipboard_text(text);
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
                self.drain(id);
                self.reconcile(event_loop);
            }
            Event::Repaint(viewport, delay) => {
                if let Some(host) = self
                    .windows
                    .values_mut()
                    .find(|host| host.viewport == viewport)
                {
                    if delay.is_zero() {
                        host.repaint();
                    } else if let Some(deadline) = Instant::now().checked_add(delay) {
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
                }
            }
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, window: WindowId, event: WindowEvent) {
        let Some(mut host) = self.windows.remove(&window) else {
            return;
        };
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
                if let Some(index) = self.index(host.id) {
                    let size = size.to_logical::<f64>(host.window.scale_factor());
                    self.workspace.windows[index].frame[2] = size.width.max(1.0);
                    self.workspace.windows[index].frame[3] = size.height.max(1.0);
                    self.changed();
                }
                host.repaint();
            }
            WindowEvent::Moved(position) => {
                if let Some(index) = self.index(host.id) {
                    let pos = position.to_logical::<f64>(host.window.scale_factor());
                    self.workspace.windows[index].frame[0] = pos.x;
                    self.workspace.windows[index].frame[1] = pos.y;
                    self.changed();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.update_fonts(&mut host);
                host.repaint();
            }
            WindowEvent::Focused(focused) => {
                host.focused = focused;
                if focused {
                    self.active = Some(host.id);
                    if let Some(pane) = self.focused(host.id) {
                        self.focus_pane(host.id, pane);
                    }
                } else {
                    host.peek = None;
                    host.modifiers = ModifiersState::empty();
                    host.divider_drag = None;
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
                        if let Some(platform) = &self.platform
                            && let Err(error) = platform.quick_resigned_focus(&host.window)
                        {
                            self.errors.push(error);
                        }
                        if self.config().quick_terminal_autohide && !host.ui_input() {
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
                host.modifiers = modifiers.state();
                let current = input::modifiers(host.modifiers);
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
                    if mouse && !host.modifiers.shift_key() {
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
