//! Shared desktop visual-state copying. The selector owns no terminal sessions.

use super::*;
use rustty_app::handoff::{SavedDump, Store};
use serde::{Deserialize, Serialize};

const SELECTOR_WINDOW: Id = 0;

#[path = "handoff_picker.rs"]
mod picker;
#[cfg(debug_assertions)]
#[path = "handoff_smoke.rs"]
mod smoke;

pub(super) struct State {
    pub store: Store,
    pub dumps: Vec<SavedDump>,
    selected: Option<String>,
    error: Option<String>,
    next_refresh: Instant,
    #[cfg(debug_assertions)]
    smoke: Option<smoke::Smoke>,
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

#[cfg(debug_assertions)]
pub(super) fn configure_smoke(loaded: &mut LoadedConfig) {
    if let Some(directory) = smoke_directory() {
        smoke::configure(loaded, directory);
    }
}

pub(super) fn smoke_directory() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    return std::env::var_os("RUSTTY_HANDOFF_SMOKE_DIR").map(PathBuf::from);
    #[cfg(not(debug_assertions))]
    None
}

impl State {
    pub fn new() -> std::io::Result<Self> {
        let label = format!("Rustty {}", env!("CARGO_PKG_VERSION"));
        #[cfg(debug_assertions)]
        let store = if let Some(directory) = smoke_directory() {
            Store::open_isolated(directory.join("dumps"), label)?
        } else {
            Store::open(label)?
        };
        #[cfg(not(debug_assertions))]
        let store = Store::open(label)?;
        let dumps = store.dumps()?;
        Ok(Self {
            selected: dumps.first().map(|dump| dump.id.clone()),
            store,
            dumps,
            error: None,
            next_refresh: Instant::now(),
            #[cfg(debug_assertions)]
            smoke: smoke::Smoke::new(),
        })
    }

    fn refresh(&mut self) {
        self.next_refresh = Instant::now() + Duration::from_secs(1);
        match self.store.dumps() {
            Ok(dumps) => self.dumps = dumps,
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn rows(&self) -> Vec<picker::Row> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        self.dumps
            .iter()
            .map(|dump| {
                let seconds = now.saturating_sub(dump.created_at.into()) / 1000;
                let age = match seconds {
                    0..60 => format!("{seconds}s"),
                    60..3600 => format!("{}m", seconds / 60),
                    3600..86400 => format!("{}h", seconds / 3600),
                    _ => format!("{}d", seconds / 86400),
                };
                picker::Row {
                    id: dump.id.clone(),
                    label: format!("{} · saved {age} ago", dump.label),
                    detail: format!("{} KiB", dump.bytes.div_ceil(1024)),
                }
            })
            .collect()
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

    pub(super) fn cancel_import_selector(&mut self, event_loop: &ActiveEventLoop) {
        self.import_selector = false;
        if let Err(error) = self.start_sessions() {
            self.errors
                .push(format!("Could not start sessions: {error}"));
        }
        if self.config().window_save_state != config::WindowSaveState::Never
            && let Some(state) = &self.handoff
            && let Err(error) = state.store.claim_workspace(&self.state_path)
        {
            self.errors
                .push(format!("Could not claim workspace saving: {error}"));
        }
        let config = self.config().clone();
        if let Some(platform) = &mut self.platform
            && let Err(error) = platform.update_config(&config)
        {
            self.errors.push(error);
        }
        self.resumed(event_loop);
    }

    pub(super) fn dump_session_state(&self) -> Result<SavedDump> {
        // shortcut: capture/compression runs on the UI thread; use a worker if large histories stall quitting.
        let bytes = self.capture_handoff()?;
        Ok(self
            .handoff
            .as_ref()
            .ok_or("Session dumping is unavailable")?
            .store
            .save_dump(&bytes)?)
    }

    fn capture_handoff(&self) -> Result<Vec<u8>> {
        let mut workspace = self.workspace.clone();
        workspace.tmuxx.clear();
        workspace
            .windows
            .retain(|window| self.tmuxx_window(window.id).is_none());
        let mut panels = BTreeMap::new();
        let mut bytes = 0usize;
        for window in &mut workspace.windows {
            for tab in &mut window.tabs {
                for (id, saved) in &mut tab.panes {
                    let visual = if let Some(pane) = self.panes.get(id) {
                        pane.update_saved(saved);
                        pane.session.capture_visual_state()?
                    } else if let Some(visual) = self.pending_visuals.get(id) {
                        visual.clone()
                    } else {
                        let empty = vt::Terminal::new(80, 24, 0);
                        serde_json::to_vec(&vt::visual::VisualSnapshot::capture(&empty)?)?
                    };
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
            active_window: self.active.filter(|id| self.tmuxx_window(*id).is_none()),
            outliner_visible: self.outliner.as_ref().is_some_and(|host| host.visible),
        })?)
    }

    fn import_handoff(&mut self, event_loop: &ActiveEventLoop, source: &str) -> Result<()> {
        if !self.import_selector {
            return Err("Open the saved-session selector before importing terminal state".into());
        }
        let state = self.handoff.as_ref().ok_or("State import is unavailable")?;
        let saved = state
            .dumps
            .iter()
            .find(|dump| dump.id == source)
            .ok_or("The selected session dump is no longer available")?
            .clone();
        let bytes = state.store.read_dump(&saved)?;
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
        let previous_workspace = std::mem::replace(&mut self.workspace, dump.workspace);
        self.pending_visuals = dump
            .panels
            .into_iter()
            .map(|(id, panel)| (id, panel.visual))
            .collect();
        let window_ids: Vec<_> = self
            .workspace
            .windows
            .iter()
            .map(|window| window.id)
            .collect();
        let created = (|| {
            // Measure the receiving layout before inserting history and starting shells.
            for id in &window_ids {
                self.open_window(event_loop, *id)?;
            }
            for window in self.workspace.windows.clone() {
                let key = self
                    .windows
                    .iter()
                    .find_map(|(key, host)| (host.id == window.id).then_some(*key))
                    .unwrap();
                let mut host = self.windows.remove(&key).unwrap();
                let spawned = (|| {
                    self.draw(event_loop, &mut host)?;
                    for (id, directory) in startup_panes(&window, false) {
                        self.spawn_pane(id, directory, Some(&host), None)?;
                    }
                    Ok::<_, Box<dyn std::error::Error>>(())
                })();
                host.repaint();
                self.windows.insert(key, host);
                spawned?;
            }
            if self.config().window_save_state != config::WindowSaveState::Never {
                self.handoff
                    .as_ref()
                    .unwrap()
                    .store
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
            self.pending_visuals.clear();
            self.failed_panes.clear();
            self.workspace = previous_workspace;
            self.active = None;
            self.initial_command_pane = None;
            return Err(error);
        }
        self.import_selector = false;
        self.active = dump.active_window;
        self.startup_tmuxx = None;
        if let Some(state) = &mut self.handoff {
            if let Err(error) = state.store.remove_dump(&saved) {
                self.errors.push(format!(
                    "Imported state, but could not remove its saved dump: {error}"
                ));
            }
            state.refresh();
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
            WindowEvent::CloseRequested => self.cancel_import_selector(event_loop),
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
        let capture = self
            .background_smoke
            .then(smoke_directory)
            .flatten()
            .map(|directory| directory.join("selector.png"))
            .filter(|path| !path.exists());
        if self.background_smoke && !self.painter.is_software() {
            let render_state = self.painter.render_state().ok_or("GPU unavailable")?;
            #[cfg(debug_assertions)]
            if let Some(path) = &capture {
                super::smoke::capture(
                    &render_state,
                    &primitives,
                    &output.textures_delta,
                    [size.width, size.height],
                    output.pixels_per_point,
                    path,
                )?;
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
                capture.is_some(),
                &host.window,
            )?;
            #[cfg(debug_assertions)]
            if let Some(path) = &capture {
                smoke::save_software_capture(&mut self.painter, path)?;
            }
        }
        host.frames += 1;
        host.last_frame_started = Some(Instant::now());
        match command {
            Some(picker::Command::Cancel) => self.cancel_import_selector(event_loop),
            Some(picker::Command::Refresh) => {
                if let Some(state) = &mut self.handoff {
                    state.refresh();
                }
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
