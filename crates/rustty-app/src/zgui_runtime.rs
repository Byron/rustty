//! The retained zgui application hosted by Rustty's existing native window loop.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};
use winit::{
    dpi::{LogicalPosition, LogicalSize},
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::Window,
};
use zgui::{
    geom::{Css, CssPx, Device, DevicePx, Point, Size},
    platform::*,
    reactive::FrameWaker,
    surface::{GpuShare, SurfaceConfig, SurfaceEvent as PanelEvent, SurfaceHandle},
    view::IntoView,
};
use zgui_platform_winit::{
    DesktopClipboard, SystemClock,
    app::{events::translate, window::WindowState},
};
use zgui_render_wgpu::{SharedGraphics, WgpuRenderer, wgpu};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const PANEL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

thread_local! {
    static ACCESSIBILITY: RefCell<HashMap<SurfaceId, Accessibility>> = RefCell::new(HashMap::new());
}

struct AccessiblePane {
    parent: accesskit_zgui::NodeId,
    base: Option<accesskit_zgui::Node>,
    frame: Arc<rustty_render::Frame>,
    text: rustty_app::accessibility::TerminalText,
    scale: f32,
    dirty: bool,
}

struct Accessibility {
    adapter: accesskit_winit_zgui::Adapter,
    panes: HashMap<u64, AccessiblePane>,
    focus: accesskit_zgui::NodeId,
}

impl Accessibility {
    fn publish(&mut self, build: &mut dyn FnMut() -> accesskit_zgui::TreeUpdate) {
        let Self {
            adapter,
            panes,
            focus,
        } = self;
        adapter.update_if_active(|| {
            let mut update = build();
            *focus = update.focus;
            for pane in panes.values_mut() {
                if let Some((_, parent)) = update.nodes.iter().find(|(id, _)| *id == pane.parent) {
                    pane.base = Some(parent.clone());
                    pane.text
                        .zgui_append_to(pane.parent, pane.scale, &mut update);
                    pane.dirty = false;
                }
            }
            update
        });
    }

    fn publish_terminal_changes(&mut self) {
        if !self
            .panes
            .values()
            .any(|pane| pane.dirty && pane.base.is_some())
        {
            return;
        }
        let Self {
            adapter,
            panes,
            focus,
        } = self;
        adapter.update_if_active(|| {
            let mut update = accesskit_zgui::TreeUpdate {
                nodes: Vec::new(),
                tree: None,
                tree_id: accesskit_zgui::TreeId::ROOT,
                focus: *focus,
            };
            for pane in panes.values_mut().filter(|pane| pane.dirty) {
                if let Some(base) = &pane.base {
                    update.nodes.push((pane.parent, base.clone()));
                    pane.text
                        .zgui_append_to(pane.parent, pane.scale, &mut update);
                    pane.dirty = false;
                }
            }
            update
        });
    }
}

pub enum TerminalAction {
    Focus {
        pane: u64,
    },
    Scroll {
        pane: u64,
        up: bool,
    },
    Select {
        pane: u64,
        selection: Option<rustty::vt::Selection>,
    },
}

/// zgui has one reactive executor per UI thread. All hosted runtimes share its wake edge.
struct WakeAll;

fn wakes() -> &'static Mutex<Vec<Weak<NativeWake>>> {
    static WAKES: OnceLock<Mutex<Vec<Weak<NativeWake>>>> = OnceLock::new();
    WAKES.get_or_init(Default::default)
}

impl FrameWaker for WakeAll {
    fn wake(&self) {
        let mut all = wakes()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        all.retain(|weak| {
            let Some(wake) = weak.upgrade() else {
                return false;
            };
            wake.wake(WakeReason::ReactiveWork {
                surfaces: Box::from([wake.id]),
            });
            true
        });
    }
}

struct NativeWake {
    id: SurfaceId,
    window: Weak<Window>,
    pending: Mutex<Vec<WakeReason>>,
    accessibility_active: AtomicBool,
    wake_loop: Arc<dyn Fn() + Send + Sync>,
}

impl Waker for NativeWake {
    fn wake(&self, reason: WakeReason) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let notify_loop = pending.is_empty();
        if !matches!(reason, WakeReason::ReactiveWork { .. })
            || !pending
                .iter()
                .any(|reason| matches!(reason, WakeReason::ReactiveWork { .. }))
        {
            pending.push(reason);
        }
        drop(pending);
        if notify_loop {
            // Windows does not deliver WM_PAINT to hidden windows; task wakes must reach the loop directly.
            (self.wake_loop)();
        }
        if let Some(window) = self.window.upgrade() {
            window.request_redraw();
        }
    }
}

struct Activation(Arc<NativeWake>);
impl accesskit_zgui::ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<accesskit_zgui::TreeUpdate> {
        self.0.accessibility_active.store(true, Ordering::Relaxed);
        self.0.wake(WakeReason::A11yTreeRequested(self.0.id));
        None
    }
}
struct Action(Arc<NativeWake>);
impl accesskit_zgui::ActionHandler for Action {
    fn do_action(&mut self, request: accesskit_zgui::ActionRequest) {
        self.0.wake(WakeReason::A11yAction(request));
    }
}
struct Deactivation(Arc<NativeWake>);
impl accesskit_zgui::DeactivationHandler for Deactivation {
    fn deactivate_accessibility(&mut self) {
        self.0.accessibility_active.store(false, Ordering::Relaxed);
    }
}

struct NativeSurface {
    id: SurfaceId,
    window: Arc<Window>,
    text_input: AtomicBool,
    redraw_requested: AtomicBool,
    offscreen: bool,
}

impl HasWindowHandle for NativeSurface {
    fn window_handle(&self) -> std::result::Result<WindowHandle<'_>, HandleError> {
        self.window.window_handle()
    }
}
impl HasDisplayHandle for NativeSurface {
    fn display_handle(&self) -> std::result::Result<DisplayHandle<'_>, HandleError> {
        self.window.display_handle()
    }
}

impl Surface for NativeSurface {
    fn id(&self) -> SurfaceId {
        self.id
    }
    fn size(&self) -> Size<DevicePx, Device> {
        let size = self.window.inner_size();
        Size::new(DevicePx(size.width as f32), DevicePx(size.height as f32))
    }
    fn scale_factor(&self) -> f64 {
        self.window.scale_factor()
    }
    fn refresh_rate_millihertz(&self) -> Option<u32> {
        self.window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz())
    }
    fn request_size(&self, size: Size<CssPx, Css>) -> Option<Size<DevicePx, Device>> {
        self.window
            .request_inner_size(LogicalSize::new(size.width.0, size.height.0))
            .map(|s| Size::new(DevicePx(s.width as f32), DevicePx(s.height as f32)))
    }
    fn set_min_size(&self, size: Option<Size<CssPx, Css>>) {
        self.window
            .set_min_inner_size(size.map(|s| LogicalSize::new(s.width.0, s.height.0)));
    }
    fn set_max_size(&self, size: Option<Size<CssPx, Css>>) {
        self.window
            .set_max_inner_size(size.map(|s| LogicalSize::new(s.width.0, s.height.0)));
    }
    fn request_redraw(&self) {
        self.redraw_requested.store(true, Ordering::Relaxed);
        self.window.request_redraw();
    }
    fn pre_present_notify(&self) {
        self.window.pre_present_notify();
    }
    fn set_title(&self, title: &str) {
        self.window.set_title(title);
    }
    fn set_visible(&self, visible: bool) {
        if !self.offscreen {
            self.window.set_visible(visible);
        }
    }
    fn set_decorations(&self, decorations: Decorations) {
        self.window
            .set_decorations(decorations != Decorations::None);
    }
    fn set_resizable(&self, resizable: bool) {
        self.window.set_resizable(resizable);
    }
    fn set_maximized(&self, maximized: bool) {
        self.window.set_maximized(maximized);
    }
    fn set_minimized(&self, minimized: bool) {
        self.window.set_minimized(minimized);
    }
    fn set_fullscreen(&self, mode: Option<FullscreenMode>) {
        let mode = mode.map(|mode| match mode {
            FullscreenMode::Exclusive => self
                .window
                .current_monitor()
                .and_then(|m| m.video_modes().next())
                .map_or(
                    winit::window::Fullscreen::Borderless(None),
                    winit::window::Fullscreen::Exclusive,
                ),
            _ => winit::window::Fullscreen::Borderless(None),
        });
        self.window.set_fullscreen(mode);
    }
    fn fullscreen(&self) -> Option<FullscreenMode> {
        self.window.fullscreen().map(|f| match f {
            winit::window::Fullscreen::Exclusive(_) => FullscreenMode::Exclusive,
            _ => FullscreenMode::Borderless,
        })
    }
    fn is_maximized(&self) -> bool {
        self.window.is_maximized()
    }
    fn is_minimized(&self) -> Option<bool> {
        self.window.is_minimized()
    }
    fn set_position(&self, pos: Point<CssPx, Css>) {
        self.window
            .set_outer_position(LogicalPosition::new(pos.x.0, pos.y.0));
    }
    fn position(&self) -> Option<Point<CssPx, Css>> {
        self.window.outer_position().ok().map(|p| {
            let p: LogicalPosition<f32> = p.to_logical(self.window.scale_factor());
            Point::new(CssPx(p.x), CssPx(p.y))
        })
    }
    fn focus(&self) {
        self.window.focus_window();
    }
    fn request_attention(&self, urgent: bool) {
        self.window.request_user_attention(Some(if urgent {
            winit::window::UserAttentionType::Critical
        } else {
            winit::window::UserAttentionType::Informational
        }));
    }
    fn begin_move_drag(&self) -> std::result::Result<(), Unsupported> {
        self.window.drag_window().map_err(|_| Unsupported)
    }
    fn begin_resize_drag(&self, edge: ResizeEdge) -> std::result::Result<(), Unsupported> {
        use winit::window::ResizeDirection as R;
        self.window
            .drag_resize_window(match edge {
                ResizeEdge::North => R::North,
                ResizeEdge::South => R::South,
                ResizeEdge::East => R::East,
                ResizeEdge::West => R::West,
                ResizeEdge::NorthEast => R::NorthEast,
                ResizeEdge::NorthWest => R::NorthWest,
                ResizeEdge::SouthEast => R::SouthEast,
                _ => R::SouthWest,
            })
            .map_err(|_| Unsupported)
    }
    fn set_cursor(&self, cursor: CursorStyle) {
        use winit::window::CursorIcon as C;
        self.window.set_cursor_visible(cursor != CursorStyle::None);
        self.window.set_cursor(match cursor {
            CursorStyle::Pointer => C::Pointer,
            CursorStyle::Text => C::Text,
            CursorStyle::VerticalText => C::VerticalText,
            CursorStyle::Crosshair => C::Crosshair,
            CursorStyle::Grab => C::Grab,
            CursorStyle::Grabbing => C::Grabbing,
            CursorStyle::Wait => C::Wait,
            CursorStyle::Progress => C::Progress,
            CursorStyle::NotAllowed => C::NotAllowed,
            CursorStyle::Move => C::Move,
            CursorStyle::ResizeColumn => C::ColResize,
            CursorStyle::ResizeRow => C::RowResize,
            CursorStyle::ResizeEastWest => C::EwResize,
            CursorStyle::ResizeNorthSouth => C::NsResize,
            CursorStyle::ResizeNorthEastSouthWest => C::NeswResize,
            CursorStyle::ResizeNorthWestSouthEast => C::NwseResize,
            _ => C::Default,
        });
    }
    fn set_text_input(&self, input: Option<TextInput>) {
        self.text_input.store(input.is_some(), Ordering::Relaxed);
        self.window.set_ime_allowed(input.is_some());
        if let Some(input) = input {
            self.window
                .set_ime_purpose(zgui_platform_winit::input::ime::purpose(input.purpose));
            self.window.set_ime_cursor_area(
                LogicalPosition::new(input.caret_origin.x.0, input.caret_origin.y.0),
                LogicalSize::new(input.caret_size.width.0, input.caret_size.height.0),
            );
        }
    }
    fn reset_dead_keys(&self) {
        self.window.reset_dead_keys();
    }
    fn push_a11y_update(&self, build: &mut dyn FnMut() -> accesskit_zgui::TreeUpdate) {
        ACCESSIBILITY.with_borrow_mut(|all| {
            if let Some(accessibility) = all.get_mut(&self.id) {
                accessibility.publish(build);
            }
        });
    }
    fn gpu(&self) -> Option<&dyn GpuSurface> {
        Some(self)
    }
    fn gpu_shared(self: Arc<Self>) -> Option<Arc<dyn GpuSurface>> {
        Some(self)
    }
}

struct Context {
    surface: Arc<NativeSurface>,
    wake: Arc<NativeWake>,
    clock: Arc<SystemClock>,
    clipboard: NativeClipboard,
    capabilities: PlatformCapabilities,
    exiting: Cell<bool>,
}

struct NativeClipboard {
    inner: DesktopClipboard,
    wake: Arc<NativeWake>,
    serials: ClipboardSerials,
}
impl Clipboard for NativeClipboard {
    fn read(&self, kind: ClipboardKind, format: ClipboardFormat) -> ClipboardSerial {
        let serial = self.serials.take();
        self.wake.wake(WakeReason::ClipboardRead {
            serial,
            result: self.read_blocking(kind, format),
        });
        serial
    }
    fn read_blocking(
        &self,
        kind: ClipboardKind,
        format: ClipboardFormat,
    ) -> std::result::Result<ClipboardData, ClipboardError> {
        self.inner.read_blocking(kind, format)
    }
    fn write(
        &self,
        kind: ClipboardKind,
        data: ClipboardData,
        options: ClipboardWriteOptions,
    ) -> std::result::Result<(), ClipboardError> {
        self.inner.write(kind, data, options)
    }
    fn clear(&self, kind: ClipboardKind) -> std::result::Result<(), ClipboardError> {
        self.inner.clear(kind)
    }
}

impl PlatformCx for Context {
    fn create_surface(
        &self,
        _: &SurfaceAttributes,
    ) -> std::result::Result<Arc<dyn Surface>, PlatformError> {
        Ok(self.surface.clone())
    }
    fn destroy_surface(&self, _: SurfaceId) {
        self.exiting.set(true);
    }
    fn surface(&self, id: SurfaceId) -> Option<Arc<dyn Surface>> {
        (id == self.surface.id).then(|| self.surface.clone() as Arc<dyn Surface>)
    }
    fn surfaces(&self) -> Vec<Arc<dyn Surface>> {
        vec![self.surface.clone()]
    }
    fn monitors(&self) -> Vec<MonitorInfo> {
        self.surface
            .window
            .available_monitors()
            .map(monitor_info)
            .collect()
    }
    fn primary_monitor(&self) -> Option<MonitorInfo> {
        self.surface.window.primary_monitor().map(monitor_info)
    }
    fn color_scheme(&self) -> Option<ColorScheme> {
        self.surface.window.theme().map(|theme| match theme {
            winit::window::Theme::Light => ColorScheme::Light,
            winit::window::Theme::Dark => ColorScheme::Dark,
        })
    }
    fn clipboard(&self) -> &dyn Clipboard {
        &self.clipboard
    }
    fn capabilities(&self) -> &PlatformCapabilities {
        &self.capabilities
    }
    fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }
    fn waker(&self) -> Arc<dyn Waker> {
        self.wake.clone()
    }
    fn request_exit(&self) {
        self.exiting.set(true);
    }
    fn is_exiting(&self) -> bool {
        self.exiting.get()
    }
}

fn monitor_info(monitor: winit::monitor::MonitorHandle) -> MonitorInfo {
    let position = monitor.position();
    let size = monitor.size();
    let mut info = MonitorInfo::new(
        Point::new(DevicePx(position.x as f32), DevicePx(position.y as f32)),
        Size::new(DevicePx(size.width as f32), DevicePx(size.height as f32)),
        monitor.scale_factor(),
    );
    if let Some(name) = monitor.name() {
        info = info.with_name(name);
    }
    if let Some(rate) = monitor.refresh_rate_millihertz() {
        info = info.with_refresh_rate_millihertz(rate);
    }
    info
}

/// A full zgui document, including native controls, text layout and panel surfaces.
pub struct NativeRuntime {
    runtime: zgui::runtime::Runtime,
    context: Context,
    input: WindowState,
    panels: HashMap<u64, TerminalSurface>,
    terminal_gpu: Option<TerminalGpu>,
    terminal_actions: Vec<TerminalAction>,
    frame_attempts: u64,
}

impl NativeRuntime {
    pub fn open<V: IntoView>(
        event_loop: &ActiveEventLoop,
        window: Arc<Window>,
        offscreen: bool,
        stylesheet: &str,
        view: impl FnMut() -> V + 'static,
        wake_loop: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = SurfaceId::new(NEXT.fetch_add(1, Ordering::Relaxed));
        let wake = Arc::new(NativeWake {
            id,
            window: Arc::downgrade(&window),
            pending: Mutex::new(Vec::new()),
            accessibility_active: AtomicBool::new(false),
            wake_loop: Arc::new(wake_loop),
        });
        let adapter = accesskit_winit_zgui::Adapter::with_direct_handlers(
            event_loop,
            &window,
            Activation(wake.clone()),
            Action(wake.clone()),
            Deactivation(wake.clone()),
        );
        ACCESSIBILITY.with_borrow_mut(|all| {
            all.insert(
                id,
                Accessibility {
                    adapter,
                    panes: HashMap::new(),
                    focus: accesskit_zgui::NodeId(0),
                },
            );
        });
        let surface = Arc::new(NativeSurface {
            id,
            window,
            text_input: AtomicBool::new(false),
            redraw_requested: AtomicBool::new(false),
            offscreen,
        });
        let mut capabilities = PlatformCapabilities::none();
        capabilities.ime = true;
        capabilities.ime_purpose_hints = true;
        capabilities.absolute_window_position = true;
        capabilities.system_color_scheme = true;
        capabilities.drop_mime_types.push("text/uri-list".into());
        let context = Context {
            surface,
            clock: Arc::new(SystemClock::new()),
            clipboard: NativeClipboard {
                inner: DesktopClipboard::new(),
                wake: wake.clone(),
                serials: ClipboardSerials::new(),
            },
            wake: wake.clone(),
            capabilities,
            exiting: Cell::new(false),
        };
        let mut app = zgui::app()
            .with_stylesheet(stylesheet)
            .with_exit_policy(zgui::runtime::ExitPolicy::Explicit);
        if offscreen {
            let graphics = SharedGraphics::new();
            app = app.with_renderer(Box::new(move |_, target| {
                let mut renderer = graphics.renderer_offscreen(target, PANEL_FORMAT, false)?;
                zgui_render_vector_vello::attach(&mut renderer, target.size);
                Ok(Box::new(renderer))
            }));
        }
        let mut runtime = app.into_handler(view)?.into_runtime();
        runtime.surfaces_available(&context);
        if let Some(error) = runtime.failure().take() {
            return Err(error.into());
        }
        wakes()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Arc::downgrade(&wake));
        zgui::reactive::set_frame_waker(Arc::new(WakeAll));
        Ok(Self {
            runtime,
            context,
            input: WindowState::default(),
            panels: HashMap::new(),
            terminal_gpu: None,
            terminal_actions: Vec::new(),
            frame_attempts: 0,
        })
    }

    /// Input runs through zgui's own winit translator and retained event router.
    pub fn event(&mut self, event: &WindowEvent) {
        ACCESSIBILITY.with_borrow_mut(|all| {
            if let Some(accessibility) = all.get_mut(&self.context.surface.id) {
                accessibility
                    .adapter
                    .process_event(&self.context.surface.window, event);
            }
        });
        if matches!(
            event,
            WindowEvent::RedrawRequested | WindowEvent::CloseRequested | WindowEvent::Destroyed
        ) {
            return;
        }
        let translated = translate(
            &mut self.input,
            self.context.surface.scale_factor(),
            || self.context.surface.size(),
            self.context.clock.timestamp(),
            event.clone(),
        );
        if let Some(event) = translated {
            self.queue(event);
        }
    }

    pub fn queue(&mut self, event: zgui::platform::SurfaceEvent) {
        self.runtime
            .surface_event(&self.context, self.context.surface.id, event);
    }

    pub fn panel_surface(&mut self, id: u64) -> SurfaceHandle {
        self.panels
            .entry(id)
            .or_insert_with(TerminalSurface::new)
            .handle
            .clone()
    }
    pub fn update_panel(&mut self, id: u64, frame: Arc<rustty_render::Frame>) {
        self.panels
            .entry(id)
            .or_insert_with(TerminalSurface::new)
            .update(frame);
    }
    pub fn retain_panels(&mut self, ids: &[u64]) {
        self.panels.retain(|id, _| ids.contains(id));
        ACCESSIBILITY.with_borrow_mut(|all| {
            if let Some(accessibility) = all.get_mut(&self.context.surface.id) {
                accessibility.panes.retain(|id, _| ids.contains(id));
            }
        });
    }
    /// Render entries, including ones zgui defers for presentation or resize pacing.
    pub fn frame_attempts(&self) -> u64 {
        self.frame_attempts
    }
    pub fn panel_paints(&self) -> u64 {
        self.panels.values().map(|p| p.paints).sum()
    }
    pub fn panel_paint_counts(&self) -> Vec<(u64, u64)> {
        let mut counts: Vec<_> = self
            .panels
            .iter()
            .map(|(&id, panel)| (id, panel.paints))
            .collect();
        counts.sort_unstable();
        counts
    }
    pub fn accessibility_active(&self) -> bool {
        self.context
            .wake
            .accessibility_active
            .load(Ordering::Relaxed)
    }
    pub fn update_accessibility(
        &mut self,
        pane: u64,
        parent: zgui::view::NodeId,
        frame: &Arc<rustty_render::Frame>,
        text: &rustty_app::accessibility::TerminalText,
        scale: f32,
    ) {
        let mut needs_parent = false;
        ACCESSIBILITY.with_borrow_mut(|all| {
            let Some(accessibility) = all.get_mut(&self.context.surface.id) else {
                return;
            };
            let parent = accesskit_zgui::NodeId(parent.as_u64());
            if let Some(current) = accessibility.panes.get_mut(&pane) {
                if current.parent != parent {
                    current.parent = parent;
                    current.base = None;
                }
                if Arc::ptr_eq(&current.frame, frame)
                    && current.scale == scale
                    && current.base.is_some()
                {
                    return;
                }
                current.frame = frame.clone();
                current.text = text.clone();
                current.scale = scale;
                current.dirty = true;
                needs_parent = current.base.is_none();
            } else {
                accessibility.panes.insert(
                    pane,
                    AccessiblePane {
                        parent,
                        base: None,
                        frame: frame.clone(),
                        text: text.clone(),
                        scale,
                        dirty: true,
                    },
                );
                needs_parent = true;
            }
        });
        if needs_parent {
            self.window_mut().publish_full_a11y_tree();
        }
    }
    pub fn take_terminal_actions(&mut self) -> Vec<TerminalAction> {
        std::mem::take(&mut self.terminal_actions)
    }

    fn terminal_action(&mut self, request: &accesskit_zgui::ActionRequest) -> bool {
        use accesskit_zgui::{Action, ActionData};
        let action = ACCESSIBILITY.with_borrow(|all| {
            let accessibility = all.get(&self.context.surface.id)?;
            let (&pane, text) = accessibility
                .panes
                .iter()
                .find(|(_, text)| text.text.zgui_contains(text.parent, request.target_node))?;
            match request.action {
                Action::Focus => Some(TerminalAction::Focus { pane }),
                Action::ScrollUp | Action::ScrollDown => Some(TerminalAction::Scroll {
                    pane,
                    up: request.action == Action::ScrollUp,
                }),
                Action::SetTextSelection => {
                    let Some(ActionData::SetTextSelection(range)) = &request.data else {
                        return None;
                    };
                    text.text
                        .zgui_selection(*range)
                        .map(|selection| TerminalAction::Select { pane, selection })
                }
                _ => None,
            }
        });
        if let Some(action) = action {
            self.terminal_actions.push(action);
            true
        } else {
            false
        }
    }
    pub fn wants_keyboard(&self) -> bool {
        self.context.surface.text_input.load(Ordering::Relaxed)
    }
    pub fn wants_widget_keys(&self) -> bool {
        use zgui::reactive::prelude::GetUntracked;
        if self.window().host().active_trap().is_some() {
            return true;
        }
        let Some(focused) = self.window().host_handle().focused().get_untracked() else {
            return false;
        };
        let document = self.window().document().borrow();
        let Some(index) = self.window().dom().live_index_of(focused) else {
            return false;
        };
        document.store().core(index).local_name().as_str() != "surface"
    }
    pub fn has_pointer_capture(&self) -> bool {
        self.window().has_pointer_capture()
    }
    pub fn edit_action(&mut self, action: &rustty::config::Action) -> bool {
        use rustty::config::Action;
        use zgui::vocab::{KeyEvent, KeyState, Modifiers};
        if !self.wants_keyboard() {
            return false;
        }
        let (key, shift) = match action {
            Action::CopyToClipboard => ("c", false),
            Action::PasteFromClipboard => ("v", false),
            Action::SelectAll => ("a", false),
            Action::Undo => ("z", false),
            Action::Redo => ("z", true),
            _ => return false,
        };
        self.runtime.surface_event(
            &self.context,
            self.context.surface.id,
            zgui::platform::SurfaceEvent::Key {
                state: KeyState::Pressed,
                event: KeyEvent::character(key),
                modifiers: Modifiers::PRIMARY.with(Modifiers::SHIFT, shift),
                timestamp: self.context.clock.timestamp(),
            },
        );
        true
    }
    pub fn paste_text(&mut self, text: String) -> bool {
        use zgui::reactive::prelude::GetUntracked;
        if !self.wants_keyboard() {
            return false;
        }
        let Some(focused) = self.window().host_handle().focused().get_untracked() else {
            return false;
        };
        let Some(index) = self.window().dom().live_index_of(focused) else {
            return false;
        };
        let key = self.window().document().borrow().store().key_of(index);
        self.window_mut().paste(key, text);
        self.window().request_frame();
        true
    }
    pub fn window(&self) -> &zgui::runtime::Window {
        &self.runtime.windows()[0]
    }
    pub fn window_mut(&mut self) -> &mut zgui::runtime::Window {
        &mut self.runtime.windows_mut()[0]
    }

    pub fn deadline(&mut self) -> Option<Instant> {
        self.runtime.idle(&self.context).deadline()
    }
    pub fn deadline_reached(&mut self) {
        self.runtime.deadline_reached(&self.context);
    }
    pub fn set_visible(&mut self, visible: bool) {
        let occluded = !visible;
        if self.window().is_occluded() != occluded {
            self.queue(zgui::platform::SurfaceEvent::Occluded(occluded));
            if !visible {
                for panel in self.panels.values_mut() {
                    panel.visible = false;
                }
            }
        }
    }
    pub fn service_hidden(&mut self) -> Result<()> {
        self.set_visible(false);
        self.deadline_reached();
        // idle() also promotes a deadline that elapsed before it could be parked.
        let _ = self.deadline();
        self.frame()
    }
    pub fn wake(&mut self, reason: WakeReason) {
        if let WakeReason::A11yAction(request) = &reason
            && self.terminal_action(request)
        {
            return;
        }
        self.runtime.wake(&self.context, reason);
    }

    /// Obtains current widget focus and geometry without entering either GPU renderer.
    pub fn settle(&mut self) -> Result<()> {
        self.drain_input();
        self.runtime.settle(&self.context, self.context.surface.id);
        self.drain_wakes();
        self.check_failure()
    }

    fn drain_input(&mut self) {
        self.drain_wakes();
        for drag in self.input.drag.take(self.input.pointer) {
            self.runtime.surface_event(
                &self.context,
                self.context.surface.id,
                zgui::platform::SurfaceEvent::Drag(drag),
            );
        }
    }

    pub fn frame(&mut self) -> Result<()> {
        self.drain_input();
        let mut painted = false;
        for panel in self.panels.values_mut() {
            painted |= panel.service(&mut self.terminal_gpu)?;
        }
        if painted {
            self.window().request_frame();
        }
        self.drain_wakes();
        self.draw_requested();
        let mut attached = false;
        for panel in self.panels.values_mut() {
            attached |= panel.service(&mut self.terminal_gpu)?;
        }
        if attached {
            // First layout supplies the device/size. Compose the freshly rendered panels before returning.
            self.window_mut().request_frame();
            self.drain_wakes();
            self.draw_requested();
        }
        self.check_failure()?;
        ACCESSIBILITY.with_borrow_mut(|all| {
            if let Some(accessibility) = all.get_mut(&self.context.surface.id) {
                accessibility.publish_terminal_changes();
            }
        });
        Ok(())
    }

    fn drain_wakes(&mut self) {
        let pending = std::mem::take(
            &mut *self
                .context
                .wake
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for reason in pending {
            self.wake(reason);
        }
    }

    fn check_failure(&self) -> Result<()> {
        if let Some(error) = self.runtime.failure().take() {
            return Err(error.into());
        }
        Ok(())
    }

    fn draw_requested(&mut self) {
        if self
            .context
            .surface
            .redraw_requested
            .swap(false, Ordering::Relaxed)
        {
            self.frame_attempts += 1;
            self.runtime.surface_event(
                &self.context,
                self.context.surface.id,
                zgui::platform::SurfaceEvent::RedrawRequested,
            );
        }
    }

    pub fn capture(&self) -> Result<([usize; 2], Vec<u8>)> {
        let renderer = self
            .window()
            .renderer()
            .as_any()
            .and_then(|any| any.downcast_ref::<WgpuRenderer>())
            .ok_or("zgui renderer does not support capture")?;
        let pixels = renderer.read_composed();
        let mut bytes = pixels.bytes().to_vec();
        if pixels.is_bgra() {
            for pixel in bytes.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }
        Ok((
            [pixels.size().width as usize, pixels.size().height as usize],
            bytes,
        ))
    }

    pub fn description(&self) -> String {
        self.window()
            .renderer()
            .as_any()
            .and_then(|any| any.downcast_ref::<WgpuRenderer>())
            .map_or_else(
                || "zgui".into(),
                |renderer| format!("zgui — {}", renderer.gpu().describe()),
            )
    }
}

impl Drop for NativeRuntime {
    fn drop(&mut self) {
        self.runtime.shutting_down(&self.context);
        ACCESSIBILITY.with_borrow_mut(|all| {
            all.remove(&self.context.surface.id);
        });
    }
}

struct TerminalSurface {
    handle: SurfaceHandle,
    events: Arc<Mutex<Vec<PanelEvent>>>,
    gpu: Option<GpuShare>,
    texture: Option<Arc<wgpu::Texture>>,
    frame: Option<Arc<rustty_render::Frame>>,
    dirty: bool,
    visible: bool,
    paints: u64,
}

struct TerminalGpu {
    generation: u64,
    renderer: rustty_render_wgpu::zgui::Renderer,
}

impl TerminalSurface {
    fn new() -> Self {
        let handle = SurfaceHandle::new(SurfaceConfig::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        handle.set_events(move |event| {
            sink.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(event)
        });
        Self {
            handle,
            events,
            gpu: None,
            texture: None,
            frame: None,
            dirty: false,
            visible: true,
            paints: 0,
        }
    }
    fn update(&mut self, frame: Arc<rustty_render::Frame>) {
        if self
            .frame
            .as_ref()
            .is_none_or(|old| !Arc::ptr_eq(old, &frame))
        {
            self.frame = Some(frame);
            self.dirty = true;
        }
    }
    fn service(&mut self, shared: &mut Option<TerminalGpu>) -> Result<bool> {
        for event in std::mem::take(
            &mut *self
                .events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        ) {
            match event {
                PanelEvent::Attached { gpu, .. } | PanelEvent::DeviceLost { gpu } => {
                    self.gpu = Some(gpu);
                    self.texture = None;
                    self.dirty = true;
                }
                PanelEvent::Visible(visible) => {
                    self.visible = visible;
                }
                PanelEvent::Detached => {
                    self.gpu = None;
                    self.texture = None;
                }
                // Controller layout already supplied a frame at the new size. This notification
                // arrives during composition; repainting it would draw that same frame twice.
                PanelEvent::Resized { .. } => {}
            }
        }
        if !self.dirty || !self.visible {
            return Ok(false);
        }
        let (Some(gpu), Some(frame)) = (&self.gpu, &self.frame) else {
            return Ok(false);
        };
        if shared
            .as_ref()
            .is_none_or(|shared| shared.generation != gpu.generation())
        {
            *shared = Some(TerminalGpu {
                generation: gpu.generation(),
                renderer: rustty_render_wgpu::zgui::Renderer::new(gpu.device(), PANEL_FORMAT),
            });
        }
        let renderer = &mut shared.as_mut().unwrap().renderer;
        let size = frame.size;
        let limit = gpu.device().limits().max_texture_dimension_2d;
        if size.contains(&0) {
            return Ok(false);
        }
        if size.iter().any(|s| *s > limit) {
            return Err("terminal panel exceeds zgui GPU texture size limit".into());
        }
        if u64::from(size[0])
            .checked_mul(u64::from(size[1]))
            .and_then(|pixels| pixels.checked_mul(4))
            .is_none_or(|bytes| bytes > 512 * 1024 * 1024)
        {
            return Err("terminal panel exceeds 512 MiB render target budget".into());
        }
        if self
            .texture
            .as_ref()
            .is_none_or(|texture| [texture.width(), texture.height()] != size)
        {
            self.texture = Some(Arc::new(gpu.device().create_texture(
                &wgpu::TextureDescriptor {
                    label: Some("Rustty retained terminal panel"),
                    size: wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: PANEL_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
            )));
        }
        renderer.prepare(gpu.device(), gpu.queue(), frame)?;
        let texture = self.texture.as_ref().unwrap();
        let view = texture.create_view(&Default::default());
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Rustty zgui terminal panel"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Rustty zgui terminal panel"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.paint(&mut pass);
        }
        gpu.queue().submit([encoder.finish()]);
        self.handle.present(texture.clone());
        self.dirty = false;
        self.paints += 1;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zgui::{
        elements::{row, surface},
        reactive::{
            RwSignal,
            prelude::{Get, Set},
        },
        surface::SurfaceElementExt,
    };

    fn solid(color: [u8; 3]) -> Arc<rustty_render::Frame> {
        solid_size(color, [32, 32])
    }

    fn solid_size(color: [u8; 3], size: [u32; 2]) -> Arc<rustty_render::Frame> {
        let mut frame = rustty_render::Frame::empty(size);
        frame.quads.push(rustty_render::Quad::solid(
            [0.0, 0.0, size[0] as f32, size[1] as f32],
            rustty_render::Color::rgb(color),
        ));
        Arc::new(frame)
    }

    #[test]
    fn retained_native_surfaces_repaint_only_the_changed_terminal() {
        let mut left = TerminalSurface::new();
        let mut right = TerminalSurface::new();
        let sources = (left.handle.clone(), right.handle.clone());
        let left_width = RwSignal::new(32);
        let graphics = SharedGraphics::new();
        let runtime = zgui::app()
            .with_size(64.0, 32.0)
            .with_stylesheet("row { width: 64px; height: 32px; gap: 0 } surface { width: 32px; height: 32px; flex-shrink: 0 }")
            .with_renderer(Box::new(move |_, target| Ok(Box::new(graphics.renderer_offscreen(target, PANEL_FORMAT, false)?))))
            .into_handler(move || row().child(surface().source(&sources.0).style_text(move || Some(format!("width: {}px", left_width.get())))).child(surface().source(&sources.1)))
            .expect("native app")
            .into_runtime();
        let mut app = zgui_platform_headless::Harness::new(runtime);
        app.settle(16);
        if let Some(error) = app.app().failure().take() {
            panic!("offscreen GPU: {error}");
        }
        let mut shared = None;
        let red = solid([255, 0, 0]);
        let blue = solid([0, 0, 255]);
        left.update(red.clone());
        right.update(blue.clone());
        assert!(left.service(&mut shared).expect("left renders"));
        assert!(right.service(&mut shared).expect("right renders"));
        app.settle(16);
        let pixels = |app: &zgui_platform_headless::Harness<zgui::runtime::Runtime>| {
            app.app().windows()[0]
                .renderer()
                .as_any()
                .unwrap()
                .downcast_ref::<WgpuRenderer>()
                .unwrap()
                .read_composed()
        };
        let before = pixels(&app);
        assert_eq!(before.rgba(8, 8), [255, 0, 0, 255]);
        assert_eq!(before.rgba(40, 8), [0, 0, 255, 255]);

        left.update(red);
        right.update(blue.clone());
        assert!(!left.service(&mut shared).unwrap());
        assert!(!right.service(&mut shared).unwrap());
        assert_eq!((left.paints, right.paints), (1, 1));

        left.update(solid([0, 255, 0]));
        right.update(blue);
        assert!(left.service(&mut shared).unwrap());
        assert!(!right.service(&mut shared).unwrap());
        let clock = app.platform().clock();
        for _ in 0..2 {
            app.app_mut().windows_mut()[0].settle(clock.as_ref());
        }
        assert_eq!(
            pixels(&app).bytes(),
            before.bytes(),
            "input/layout settlement must leave composition for the redraw"
        );
        app.settle(16);
        assert_eq!((left.paints, right.paints), (2, 1));
        let after = pixels(&app);
        assert_eq!(after.rgba(8, 8), [0, 255, 0, 255]);
        for y in 0..32 {
            for x in 32..64 {
                assert_eq!(
                    before.rgba(x, y),
                    after.rgba(x, y),
                    "unchanged panel pixel at {x},{y}"
                );
            }
        }

        // Layout supplies the new terminal frame before composition delivers its Resized event.
        // That late notification must not paint the same frame a second time.
        left_width.set(48);
        app.app_mut().windows_mut()[0].settle(clock.as_ref());
        left.update(solid_size([0, 255, 0], [48, 32]));
        assert!(left.service(&mut shared).unwrap());
        app.settle(16);
        assert!(!left.service(&mut shared).unwrap());
        assert!(!right.service(&mut shared).unwrap());
        assert_eq!((left.paints, right.paints), (3, 1));
        let resized = pixels(&app);
        assert_eq!(resized.rgba(40, 8), [0, 255, 0, 255]);
        assert_eq!(resized.rgba(56, 8), [0, 0, 255, 255]);
    }
}
