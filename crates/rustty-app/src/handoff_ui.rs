//! macOS visual-state copying. The selector owns no terminal sessions.

use super::*;
use rustty_app::handoff::{Instance, Registry, Request, RequestStatus};
use serde::{Deserialize, Serialize};

const SELECTOR_WINDOW: Id = 0;

#[path = "handoff_picker.rs"]
mod picker;
#[cfg(debug_assertions)]
#[path = "handoff_smoke.rs"]
mod smoke;

pub(super) struct State {
    pub registry: Registry,
    instances: Vec<Instance>,
    requests: BTreeMap<String, Request>,
    selected: Option<String>,
    incoming: VecDeque<Request>,
    error: Option<String>,
    next_refresh: Instant,
    #[cfg(debug_assertions)]
    smoke: Option<smoke::Smoke>,
}

pub(super) struct Prompt {
    request: Request,
    ignore_further: bool,
}

#[derive(Serialize, Deserialize)]
struct Panel {
    #[serde(with = "serde_bytes")]
    visual: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct Dump {
    version: u32,
    workspace: Workspace,
    panels: BTreeMap<Id, Panel>,
    #[serde(default)]
    active_window: Option<Id>,
    #[serde(default)]
    outliner_visible: bool,
}

impl Dump {
    fn decode(bytes: &[u8]) -> Result<Self> {
        let dump: Self = rmp_serde::from_slice(bytes)?;
        if dump.version != 1 {
            return Err("Unsupported Rustty visual-state version".into());
        }
        dump.workspace.validate()?;
        let panes: HashSet<_> = dump
            .workspace
            .windows
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| t.panes.keys().copied())
            .collect();
        if panes.is_empty()
            || panes.len() != dump.panels.len()
            || dump
                .workspace
                .windows
                .iter()
                .any(|window| window.id == SELECTOR_WINDOW)
            || !dump.workspace.tmuxx.is_empty()
            || dump.panels.keys().any(|id| !panes.contains(id))
        {
            return Err("The visual state has missing or unexpected panels".into());
        }
        for panel in dump.panels.values() {
            Session::validate_visual_state(&panel.visual)?;
        }
        Ok(dump)
    }
}

pub(super) fn import_requested(args: &mut Vec<String>) -> bool {
    let command = args
        .iter()
        .position(|arg| arg == "-e" || arg == "--")
        .unwrap_or(args.len());
    args.extract_if(..command, |arg| arg == "--import-state")
        .count()
        != 0
}

pub(super) fn smoke_directory() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    return std::env::var_os("RUSTTY_HANDOFF_SMOKE_DIR").map(PathBuf::from);
    #[cfg(not(debug_assertions))]
    None
}

impl State {
    pub fn new(proxy: EventLoopProxy<Event>) -> std::io::Result<Self> {
        let label = format!("Rustty {}", env!("CARGO_PKG_VERSION"));
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = proxy.send_event(Event::Handoff);
        });
        #[cfg(debug_assertions)]
        let registry = if let Some(directory) = smoke_directory() {
            Registry::register_isolated(directory.join("registry"), label, wake)?
        } else {
            Registry::register(label, wake)?
        };
        #[cfg(not(debug_assertions))]
        let registry = Registry::register(label, wake)?;
        let instances = registry.instances()?;
        Ok(Self {
            registry,
            instances,
            requests: BTreeMap::new(),
            selected: None,
            incoming: VecDeque::new(),
            error: None,
            next_refresh: Instant::now(),
            #[cfg(debug_assertions)]
            smoke: smoke::Smoke::new(),
        })
    }

    fn refresh(&mut self) {
        self.next_refresh = Instant::now() + Duration::from_secs(1);
        match self.registry.instances() {
            Ok(mut instances) => {
                for request in self.requests.values() {
                    if !instances
                        .iter()
                        .any(|instance| instance.id == request.source.id)
                    {
                        instances.push(request.source.clone());
                    }
                }
                self.instances = instances;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    pub(super) fn requeue(&mut self, prompt: Prompt) {
        self.incoming.push_front(prompt.request);
    }

    fn rows(&self) -> Vec<picker::Row> {
        let mut rows: Vec<_> = self
            .instances
            .iter()
            .map(|instance| {
                let status = if let Some(request) = self.requests.get(&instance.id) {
                    match self.registry.status(request) {
                        Ok(RequestStatus::Waiting) => picker::Status::Waiting,
                        Ok(RequestStatus::Ready { path, bytes }) => picker::Status::Ready {
                            file: path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into(),
                            bytes,
                        },
                        Ok(RequestStatus::Declined) => picker::Status::Declined,
                        Ok(RequestStatus::Ignored) => picker::Status::Ignored,
                        Ok(RequestStatus::Unavailable) | Err(_) => picker::Status::Unavailable,
                    }
                } else if instance.ignoring_requests {
                    picker::Status::Ignored
                } else {
                    picker::Status::Available
                };
                picker::Row {
                    id: instance.id.clone(),
                    label: format!("{} · PID {}", instance.label, instance.pid),
                    detail: instance.executable.display().to_string(),
                    status,
                }
            })
            .collect();
        // Older bundles can be shown, but only advertised peers may receive a signal.
        use objc2_app_kit::NSRunningApplication;
        use objc2_foundation::ns_string;
        for app in NSRunningApplication::runningApplicationsWithBundleIdentifier(ns_string!(
            "app.rustty.Rustty"
        )) {
            let pid = app.processIdentifier();
            if pid <= 0
                || pid as u32 == std::process::id()
                || self
                    .instances
                    .iter()
                    .any(|instance| instance.pid == pid as u32)
                || !same_user(pid)
            {
                continue;
            }
            rows.push(picker::Row {
                id: format!("unsupported-{pid}"),
                label: format!("Rustty · PID {pid}"),
                detail: "This version does not support exporting terminal state.".into(),
                status: picker::Status::Unavailable,
            });
        }
        rows
    }
}

fn same_user(pid: libc::pid_t) -> bool {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    // A PID from AppKit is only displayed when libproc confirms its owner.
    unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        ) == size
            && info.pbi_uid == libc::geteuid()
    }
}

impl App {
    #[cfg(debug_assertions)]
    pub(super) fn tick_handoff_smoke(&mut self, event_loop: &ActiveEventLoop) {
        let Some(mut smoke) = self.handoff.as_mut().and_then(|state| state.smoke.take()) else {
            return;
        };
        match smoke.step(self, event_loop) {
            Ok(false) => self.handoff.as_mut().unwrap().smoke = Some(smoke),
            Ok(true) => event_loop.exit(),
            Err(error) => {
                self.smoke_error = Some(error.to_string());
                event_loop.exit();
            }
        }
    }

    #[cfg(debug_assertions)]
    pub(super) fn handoff_smoke_deadline(&self) -> Option<Instant> {
        self.handoff
            .as_ref()
            .and_then(|state| state.smoke.as_ref())
            .map(|_| Instant::now() + Duration::from_millis(25))
    }

    pub(super) fn tick_import_selector(&mut self) {
        if self.import_selector
            && let Some(state) = &mut self.handoff
            && Instant::now() >= state.next_refresh
        {
            state.refresh();
            for host in self.windows.values() {
                host.repaint();
            }
        }
    }

    pub(super) fn import_refresh_deadline(&self) -> Option<Instant> {
        self.handoff
            .as_ref()
            .filter(|_| self.import_selector)
            .map(|state| state.next_refresh)
    }

    pub(super) fn open_import_selector(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        if !self.windows.is_empty() {
            return Ok(());
        }
        self.open_host(
            event_loop,
            WindowState {
                id: SELECTOR_WINDOW,
                tabs: Vec::new(),
                active_tab: 0,
                frame: [120.0, 120.0, 700.0, 460.0],
                quick: false,
            },
        )?;
        if let Some(host) = self.windows.values().next() {
            host.window.set_title("Import Terminal State — Rustty");
        }
        Ok(())
    }

    pub(super) fn poll_handoff(&mut self) {
        if let Some(state) = &mut self.handoff {
            state.refresh();
            match state.registry.incoming() {
                Ok(requests) => state.incoming.extend(requests),
                Err(error) => self
                    .errors
                    .push(format!("Could not read state requests: {error}")),
            }
            for host in self.windows.values_mut() {
                if host.handoff_prompt.as_ref().is_some_and(|prompt| {
                    !state.registry.is_pending(&prompt.request).unwrap_or(false)
                }) {
                    host.handoff_prompt = None;
                }
                host.repaint();
            }
        }
        self.present_handoff_prompt();
    }

    pub(super) fn present_handoff_prompt(&mut self) {
        if self.import_selector
            || self
                .windows
                .values()
                .any(|host| host.handoff_prompt.is_some())
        {
            return;
        }
        let Some(state) = &mut self.handoff else {
            return;
        };
        while state
            .incoming
            .front()
            .is_some_and(|request| !state.registry.is_pending(request).unwrap_or(false))
        {
            state.incoming.pop_front();
        }
        if state.incoming.is_empty() {
            return;
        }
        if self.workspace.windows.is_empty() {
            while let Some(request) = state.incoming.pop_front() {
                if let Err(error) = state.registry.respond(&request, None) {
                    self.errors
                        .push(format!("Could not decline an empty export: {error}"));
                }
            }
            return;
        }
        let target = self
            .windows
            .iter()
            .find(|(_, host)| !host.modal_input() && Some(host.id) == self.active)
            .or_else(|| self.windows.iter().find(|(_, host)| !host.modal_input()))
            .map(|(id, _)| *id);
        if let Some(key) = target {
            let request = self.handoff.as_mut().unwrap().incoming.pop_front().unwrap();
            let host = self.windows.get_mut(&key).unwrap();
            host.handoff_prompt = Some(Prompt {
                request,
                ignore_further: false,
            });
            host.visible = true;
            if !self.background_smoke {
                host.window.set_visible(true);
                host.window.set_minimized(false);
                host.window.focus_window();
            }
            host.repaint();
        }
    }

    pub(super) fn show_handoff_prompt(&mut self, host: &mut Host, ctx: &egui::Context) {
        let Some(prompt) = &mut host.handoff_prompt else {
            return;
        };
        let requester = format!(
            "{} (PID {})",
            prompt.request.requester.label, prompt.request.requester.pid
        );
        let Some(approval) = picker::show_approval(ctx, &requester, &mut prompt.ignore_further)
        else {
            return;
        };
        let request = host.handoff_prompt.take().unwrap().request;
        let result = if approval.export {
            // shortcut: export runs on the UI thread; use a worker if large histories stall interaction.
            self.capture_handoff().and_then(|bytes| {
                self.handoff
                    .as_ref()
                    .ok_or("State export is unavailable")?
                    .registry
                    .respond(&request, Some(&bytes))?;
                Ok(())
            })
        } else {
            self.handoff
                .as_ref()
                .unwrap()
                .registry
                .respond(&request, None)
                .map_err(Into::into)
        };
        if let Err(error) = result {
            self.errors
                .push(format!("Could not export terminal state: {error}"));
            let _ = self
                .handoff
                .as_ref()
                .unwrap()
                .registry
                .respond(&request, None);
        }
        if approval.ignore_further
            && let Err(error) = self
                .handoff
                .as_mut()
                .unwrap()
                .registry
                .ignore_requests(true)
        {
            self.errors.push(error.to_string());
        }
        host.repaint();
    }

    fn capture_handoff(&self) -> Result<Vec<u8>> {
        let mut workspace = self.workspace.clone();
        workspace.tmuxx.clear();
        let mut panels = BTreeMap::new();
        let mut bytes = 0usize;
        for window in &mut workspace.windows {
            for tab in &mut window.tabs {
                for (id, saved) in &mut tab.panes {
                    let pane = self
                        .panes
                        .get(id)
                        .ok_or("A panel has no terminal state to export")?;
                    pane.update_saved(saved);
                    let visual = pane.session.capture_visual_state()?;
                    bytes = bytes.saturating_add(visual.len());
                    if bytes > rustty_app::handoff::MAX_DUMP_BYTES as usize {
                        return Err("The exported terminal state exceeds 512 MiB".into());
                    }
                    panels.insert(*id, Panel { visual });
                }
            }
        }
        workspace.validate()?;
        if panels.is_empty() {
            return Err("This instance has no panels to export".into());
        }
        Ok(rmp_serde::to_vec_named(&Dump {
            version: 1,
            workspace,
            panels,
            active_window: self.active,
            outliner_visible: self.outliner.as_ref().is_some_and(|host| host.visible),
        })?)
    }

    fn import_handoff(&mut self, event_loop: &ActiveEventLoop, source: &str) -> Result<()> {
        if !self.import_selector {
            return Err("Open the instance selector before importing terminal state".into());
        }
        let state = self.handoff.as_ref().ok_or("State import is unavailable")?;
        let request = state
            .requests
            .get(source)
            .ok_or("No export was requested from this instance")?
            .clone();
        let bytes = state.registry.read_dump(&request)?;
        let mut dump = Dump::decode(&bytes)?;
        // The source server's input and dismissal rules do not apply to fresh local shells.
        for window in &mut dump.workspace.windows {
            for tab in &mut window.tabs {
                tab.server_view = None;
                for overlay in &mut tab.overlays {
                    overlay.modal = false;
                    overlay.capture_keys = false;
                    overlay.close_click = false;
                    overlay.close_cancel = false;
                }
            }
        }
        self.workspace = dump.workspace;
        let window_ids: Vec<_> = self
            .workspace
            .windows
            .iter()
            .map(|window| window.id)
            .collect();
        let created = (|| {
            for (id, panel) in dump.panels {
                let directory = self
                    .workspace
                    .windows
                    .iter()
                    .flat_map(|w| &w.tabs)
                    .find_map(|tab| tab.panes.get(&id))
                    .unwrap()
                    .working_directory
                    .clone();
                self.spawn_pane_with_visual(id, directory, None, None, Some(&panel.visual))?;
            }
            for id in &window_ids {
                self.open_window(event_loop, *id)?;
            }
            if self.config().window_save_state != config::WindowSaveState::Never {
                self.handoff
                    .as_ref()
                    .unwrap()
                    .registry
                    .claim_workspace(&self.state_path)?;
            }
            Ok::<_, Box<dyn std::error::Error>>(())
        })();
        if let Err(error) = created {
            for (_, host) in self
                .windows
                .extract_if(|_, host| window_ids.contains(&host.id))
            {
                if let Some(platform) = &self.platform {
                    platform.forget_window(&host.window);
                }
            }
            // The selector may be temporarily borrowed by its redraw callback.
            let retained = self
                .windows
                .values()
                .map(|host| host.viewport)
                .chain([ViewportId::from_hash_of(SELECTOR_WINDOW)])
                .collect();
            for id in window_ids {
                self.painter
                    .remove_window(ViewportId::from_hash_of(id), id, &retained);
            }
            self.panes.clear();
            self.failed_panes.clear();
            self.workspace = Workspace::default();
            self.active = None;
            self.initial_command_pane = None;
            return Err(error);
        }
        self.import_selector = false;
        self.active = dump.active_window;
        if let Some(state) = &self.handoff
            && let Err(error) = state.registry.cancel_all()
        {
            self.errors.push(format!(
                "Imported state, but could not remove temporary files: {error}"
            ));
        }
        if let Some(state) = &mut self.handoff {
            state.requests.clear();
            state.selected = None;
        }
        self.reconcile(event_loop);
        if !self.background_smoke
            && let Some(host) = self
                .windows
                .values()
                .find(|host| Some(host.id) == dump.active_window)
        {
            host.window.focus_window();
        }
        if dump.outliner_visible {
            self.toggle_outliner(event_loop);
        }
        let config = self.config().clone();
        if let Some(platform) = &mut self.platform
            && let Err(error) = platform.update_config(&config)
        {
            self.errors.push(error);
        }
        self.changed();
        Ok(())
    }

    pub(super) fn import_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: WindowId,
        event: WindowEvent,
    ) {
        let Some(mut host) = self.windows.remove(&id) else {
            return;
        };
        let response = host.egui.on_window_event(&host.window, &event);
        if response.repaint {
            host.repaint();
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.draw_import_selector(event_loop, &mut host)
                    && let Some(state) = &mut self.handoff
                {
                    state.error = Some(error.to_string());
                }
            }
            WindowEvent::Resized(size) => {
                if let (Some(width), Some(height)) =
                    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    self.painter.on_window_resized(host.viewport, width, height);
                }
                host.repaint();
            }
            WindowEvent::Focused(focused) => host.focused = focused,
            _ => {}
        }
        if self.import_selector {
            self.windows.insert(id, host);
        } else {
            if let Some(platform) = &self.platform {
                platform.forget_window(&host.window);
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

    fn draw_import_selector(
        &mut self,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
    ) -> Result<()> {
        let size = host.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let mut raw = host.egui.take_egui_input(&host.window);
        raw.viewport_id = host.viewport;
        input::filter_egui_events(&mut raw, true);
        if host.accesskit_active {
            self.context.enable_accesskit();
        } else {
            self.context.disable_accesskit();
        }
        let mut command = None;
        let mut output = self.context.run_ui(raw, |ui| {
            if let Some(state) = &mut self.handoff {
                let rows = state.rows();
                command = picker::show(ui, &rows, &mut state.selected, state.error.as_deref())
                    .or(command.take());
            } else {
                command = picker::show(ui, &[], &mut None, Some(&self.errors.join("\n")))
                    .or(command.take());
            }
        });
        if !self.background_smoke {
            host.egui.handle_platform_output_with_event_loop(
                &host.window,
                event_loop,
                output.platform_output,
            );
        }
        let primitives = self
            .context
            .tessellate(output.shapes, output.pixels_per_point);
        if self.background_smoke {
            let render_state = self.painter.render_state().ok_or("GPU unavailable")?;
            #[cfg(debug_assertions)]
            if let Some(directory) = smoke_directory() {
                let path = directory.join("selector.png");
                if !path.exists() {
                    super::smoke::capture(
                        &render_state,
                        &primitives,
                        &output.textures_delta,
                        [size.width, size.height],
                        output.pixels_per_point,
                        &path,
                    )?;
                }
            }
            super::smoke::submit_offscreen(
                &render_state,
                &primitives,
                &output.textures_delta,
                [size.width, size.height],
                output.pixels_per_point,
                None,
            );
            output.textures_delta.clear();
        } else {
            self.painter.paint(
                host.viewport,
                output.pixels_per_point,
                &primitives,
                &mut output.textures_delta,
                false,
                &host.window,
            )?;
        }
        match command {
            Some(picker::Command::Cancel) => {
                if let Some(state) = &self.handoff {
                    state.registry.cancel_all()?;
                }
                event_loop.exit();
            }
            Some(picker::Command::Refresh) => {
                if let Some(state) = &mut self.handoff {
                    state.refresh();
                }
                host.repaint();
            }
            Some(picker::Command::Ask(id)) => {
                let state = self.handoff.as_mut().ok_or("State import is unavailable")?;
                let instance = state
                    .instances
                    .iter()
                    .find(|instance| instance.id == id)
                    .ok_or("The selected instance is no longer available")?;
                if let Some(request) = state.requests.remove(&id) {
                    state.registry.cancel(&request)?;
                }
                let request = state.registry.ask(instance)?;
                state.requests.insert(id, request);
                state.error = None;
                host.repaint();
            }
            Some(picker::Command::Import(id)) => self.import_handoff(event_loop, &id)?,
            None => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_flag_is_removed_without_changing_child_arguments() {
        for separator in ["-e", "--"] {
            let mut args: Vec<String> = [
                "--import-state",
                "--config-default-files=false",
                "--import-state",
                separator,
                "echo",
                "--import-state",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect();
            assert!(import_requested(&mut args));
            assert_eq!(
                args,
                [
                    "--config-default-files=false",
                    separator,
                    "echo",
                    "--import-state"
                ]
            );
            assert!(!import_requested(&mut args));
        }
        assert!(!import_requested(&mut Vec::new()));
    }

    fn dump() -> Dump {
        let mut workspace = Workspace::default();
        let window = workspace.id();
        let tab = workspace.id();
        let pane = workspace.id();
        workspace.windows.push(WindowState {
            id: window,
            tabs: vec![Tab::new(tab, pane, PathBuf::from("/tmp"))],
            active_tab: 0,
            frame: [0., 0., 800., 600.],
            quick: false,
        });
        let mut terminal = vt::Terminal::new(80, 24, 100);
        terminal.feed(b"retained display");
        let visual =
            serde_json::to_vec(&vt::visual::VisualSnapshot::capture(&terminal).unwrap()).unwrap();
        Dump {
            version: 1,
            workspace,
            panels: BTreeMap::from([(pane, Panel { visual })]),
            active_window: Some(window),
            outliner_visible: true,
        }
    }

    fn validates(dump: &Dump) -> bool {
        Dump::decode(&rmp_serde::to_vec_named(dump).unwrap()).is_ok()
    }

    #[test]
    fn validates_the_complete_workspace_before_importing() {
        let mut state = dump();
        assert!(validates(&state));
        state.panels.clear();
        assert!(!validates(&state));
        let mut state = dump();
        state.panels.values_mut().next().unwrap().visual = b"corrupt display".to_vec();
        assert!(!validates(&state));
        let mut state = dump();
        state.workspace.windows[0].active_tab = 99;
        assert!(!validates(&state));
        let mut state = dump();
        state.version += 1;
        assert!(!validates(&state));
    }

    #[test]
    fn optional_import_metadata_can_be_absent() {
        #[derive(Serialize)]
        struct Required {
            version: u32,
            workspace: Workspace,
            panels: BTreeMap<Id, Panel>,
        }
        let state = dump();
        let bytes = rmp_serde::to_vec_named(&Required {
            version: state.version,
            workspace: state.workspace,
            panels: state.panels,
        })
        .unwrap();
        let restored = Dump::decode(&bytes).unwrap();
        assert!(restored.active_window.is_none());
        assert!(!restored.outliner_visible);
    }
}
