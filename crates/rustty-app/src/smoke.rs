//! Opt-in native integration check; all shells and saved state are test-owned.
use super::*;
use std::fs;

#[path = "../../../test/rustty/frame_workloads.rs"]
mod workload;

#[cfg(target_os = "windows")]
#[path = "smoke/windows_capture.rs"]
mod windows_capture;

fn queued_input(pane: &Pane) -> Result<Vec<Vec<u8>>> {
    let terminal = pane.session.terminal()?;
    Ok(pane
        .input
        .iter()
        .map(|input| input.encode(&terminal))
        .collect())
}

fn fixture_directory() -> PathBuf {
    if cfg!(target_os = "windows") {
        std::env::temp_dir()
    } else {
        PathBuf::from("/tmp")
    }
}

#[cfg(test)]
#[test]
fn hover_measurement_restarts_for_motion_and_leaving_but_not_duplicate_events() {
    let now = Instant::now();
    let mut smoke = Smoke {
        directory: PathBuf::new(),
        offscreen: false,
        interactive_deck: false,
        hover: true,
        pointer: None,
        stage: 4,
        deadline: now + Duration::from_secs(45),
        next: now,
        original: None,
        closed: None,
        idle_frames: 0,
        progress_pane: None,
        progress_refreshed: false,
        progress_started: now,
        progress_frames: 0,
        progress_seconds: 0.0,
        hidden_title_frames: 0,
        header_frames: 0,
        header_prepares: 0,
        active_title_frames: 0,
        active_title_prepares: 0,
        active_title_tab: None,
        active_title_accesskit: false,
        events: BTreeMap::new(),
        timing: None,
        report: None,
    };
    let position = Some(Pos2::new(100.0, 100.0));
    smoke.pointer(position, 5);
    assert_eq!(smoke.idle_frames, 5);
    let next = smoke.next;
    smoke.pointer(position, 10);
    assert_eq!((smoke.idle_frames, smoke.next), (5, next));
    smoke.pointer(None, 12);
    assert_eq!(smoke.idle_frames, 12);
    smoke.pointer(position, 15);
    assert_eq!(smoke.idle_frames, 15);
}

pub(super) struct Smoke {
    pub directory: PathBuf,
    pub offscreen: bool,
    interactive_deck: bool,
    hover: bool,
    pointer: Option<Pos2>,
    stage: u8,
    deadline: Instant,
    next: Instant,
    original: Option<Id>,
    closed: Option<(Id, Instant)>,
    idle_frames: u64,
    progress_pane: Option<Id>,
    progress_refreshed: bool,
    progress_started: Instant,
    progress_frames: u64,
    progress_seconds: f64,
    hidden_title_frames: u64,
    header_frames: u64,
    header_prepares: u64,
    active_title_frames: u64,
    active_title_prepares: u64,
    active_title_tab: Option<String>,
    active_title_accesskit: bool,
    events: BTreeMap<&'static str, u64>,
    timing: Option<Replay>,
    report: Option<serde_json::Value>,
}
impl Smoke {
    pub fn from_env(loaded: &mut LoadedConfig) -> Result<Option<Self>> {
        let Some(directory) = std::env::var_os("RUSTTY_SMOKE_DIR") else {
            return Ok(None);
        };
        let directory = PathBuf::from(directory);
        fs::create_dir_all(&directory)?;
        for name in [
            "window.png",
            "find-focused.png",
            "find-unfocused.png",
            "theme-picker.png",
            "tmuxx-secondary-selection.png",
            "outliner.png",
            "outliner-collapsed.png",
            "outliner-terminal.png",
            "outliner-quad.png",
            "outliner-quad-terminal.png",
            "outliner-nested.png",
            "outliner-nested-terminal.png",
            "outliner-pane.png",
            "outliner-pane-terminal.png",
            "outliner-narrow-terminal.png",
            "outliner-pinned.png",
            "outliner-pinned-terminal.png",
            "outliner-pin-find.png",
            "outliner-search.png",
            "outliner-search-empty.png",
            "outliner-search-navigation.png",
            "outliner-flat.png",
            "outliner-windows.png",
            "outliner-other-window.png",
            "outliner-quick.png",
            "result.json",
            "timing.json",
        ] {
            match fs::remove_file(directory.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let timing = std::env::var("RUSTTY_SMOKE_TIMING")
            .ok()
            .map(|name| Replay::new(&name))
            .transpose()?;
        Self::configure(loaded);
        Ok(Some(Self {
            directory,
            offscreen: std::env::var_os("RUSTTY_SMOKE_OFFSCREEN").is_some(),
            interactive_deck: std::env::var_os("RUSTTY_SMOKE_INTERACTIVE_DECK").is_some(),
            hover: std::env::var_os("RUSTTY_SMOKE_HOVER").is_some(),
            pointer: None,
            stage: 0,
            deadline: Instant::now() + Duration::from_secs(45),
            next: Instant::now(),
            original: None,
            closed: None,
            idle_frames: 0,
            progress_pane: None,
            progress_refreshed: false,
            progress_started: Instant::now(),
            progress_frames: 0,
            progress_seconds: 0.0,
            hidden_title_frames: 0,
            header_frames: 0,
            header_prepares: 0,
            active_title_frames: 0,
            active_title_prepares: 0,
            active_title_tab: None,
            active_title_accesskit: false,
            events: BTreeMap::new(),
            timing,
            report: None,
        }))
    }
    pub(super) fn configure(loaded: &mut LoadedConfig) {
        loaded.config.stream_deck = false;
        let timing = std::env::var_os("RUSTTY_SMOKE_TIMING").is_some();
        let deck_only = std::env::var("RUSTTY_SMOKE_DECK").as_deref() == Ok("only");
        let outliner = std::env::var_os("RUSTTY_SMOKE_OUTLINER").is_some();
        if timing {
            let renderer = loaded.config.renderer;
            loaded.config = Config::default();
            loaded.config.renderer = renderer;
            loaded.diagnostics.clear();
            loaded.config.font_family = vec![if cfg!(target_os = "windows") {
                "Consolas".into()
            } else {
                "Menlo".into()
            }];
            loaded.config.font_size = 13.0;
        }
        #[cfg(not(target_os = "windows"))]
        let command = if timing || deck_only || outliner {
            config::Command::Direct(vec!["/bin/sleep".into(), "120".into()])
        } else {
            config::Command::Direct(vec!["/bin/sh".into(),"-c".into(),r"printf '\033[2J\033[H\033[30;107m  ✔️\033[5G  > selected row\033[0m\n\033[1;36mRustty native smoke\033[0m\n\033]7;file://localhost/tmp\007\033]9;4;1;65\007'; exec /bin/sh -i".into()])
        };
        #[cfg(target_os = "windows")]
        let command = {
            // Use a deterministic native Git Bash child for the shared POSIX
            // fixtures; no WSL and no user startup files or history writes.
            // Disable readline: a concurrent initial resize can make it lose
            // the first input character with both system and bundled ConPTY.
            let shell = std::env::var_os("RUSTTY_SMOKE_SHELL")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(
                        std::env::var_os("ProgramFiles")
                            .unwrap_or_else(|| "C:\\Program Files".into()),
                    )
                    .join("Git/bin/bash.exe")
                });
            if timing || deck_only || outliner {
                config::Command::Direct(vec![
                    shell.to_string_lossy().into_owned(),
                    "--noprofile".into(),
                    "--norc".into(),
                    "-c".into(),
                    "sleep 120".into(),
                ])
            } else {
                config::Command::Direct(vec![shell.to_string_lossy().into_owned(),
                "--noprofile".into(), "--norc".into(), "-c".into(),
                r#"export HISTFILE=/dev/null INPUTRC=/dev/null PS1='RUSTTY_SMOKE_READY> ' LC_ALL=C.UTF-8; printf '\033[2J\033[H\033[30;107m  ✔️\033[5G  > selected row\033[0m\n\033[1;36mRustty native smoke\033[0m\n\033]7;kitty-shell-cwd://localhost%s\007\033]9;4;1;65\007' "$PWD"; exec bash --noprofile --norc --noediting -i"#.into()])
            }
        };
        loaded.config.command = Some(command.clone());
        loaded.config.initial_command = Some(command);
        loaded.config.working_directory = Some(fixture_directory());
        loaded.config.window_save_state = config::WindowSaveState::Always;
        loaded.config.window_animations = Some(false);
        loaded.config.cursor_style_blink = Some(false);
        loaded.config.grapheme_width_method = config::GraphemeWidthMethod::Unicode;
        loaded.config.mouse_shift_capture = config::MouseShiftCapture::False;
        loaded.config.link_url = true;
        loaded.config.progress_style = true;
        loaded.config.undo_timeout = Duration::from_secs(5);
        loaded.config.keybinds.retain(|b| !b.flags.global);
        if std::env::var_os("RUSTTY_SMOKE_INTERACTIVE_DECK").is_some()
            && let Some(directory) = std::env::var_os("RUSTTY_SMOKE_DIR")
        {
            let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../test/rustty/interactive-deck-demo.py");
            let command = config::Command::Direct(vec![
                "python3".into(),
                script.to_string_lossy().into_owned(),
                "--directory".into(),
                directory.to_string_lossy().into_owned(),
            ]);
            loaded.config.command = Some(command.clone());
            loaded.config.initial_command = Some(command);
            loaded.config.working_directory = Some(PathBuf::from(directory));
            loaded.config.stream_deck = true;
        }
    }
    pub fn record(&mut self, event: &'static str) {
        if matches!(self.stage, 4 | 8 | 11 | 12) {
            *self.events.entry(event).or_default() += 1;
        }
    }
    pub fn pointer(&mut self, position: Option<Pos2>, frames: u64) {
        if self.hover && self.pointer != position {
            self.input(frames);
        }
        self.pointer = position;
    }
    pub fn input(&mut self, frames: u64) {
        if self.stage == 4 {
            // A developer can keep using an unlocked Mac during this check.
            // Measure a full quiet interval, rather than calling input redraws idle.
            self.idle_frames = frames;
            self.events.clear();
            self.next = Instant::now() + Duration::from_millis(1500);
        }
    }
    pub fn check_background_windows(&self, app: &App) -> Result<()> {
        if app.background_smoke
            && app
                .windows
                .values()
                .map(|host| &host.window)
                .chain(app.outliner.iter().map(|host| &host.window))
                .any(|window| window.is_visible() == Some(true) || window.has_focus())
        {
            return Err("offscreen smoke exposed a native window or took keyboard focus".into());
        }
        Ok(())
    }
    pub fn step(&mut self, app: &mut App, event_loop: &ActiveEventLoop) -> Result<bool> {
        self.check_background_windows(app)?;
        if std::env::var_os("RUSTTY_SMOKE_OUTLINER").is_some() {
            let now = Instant::now();
            match self.stage {
                0 => {
                    check_outliner(app, event_loop, &self.directory)?;
                    app.toggle_outliner(event_loop);
                    self.stage = 1;
                    self.next = Instant::now() + Duration::from_millis(500);
                }
                1 if now >= self.next => {
                    self.idle_frames = app.outliner.as_ref().unwrap().frames;
                    self.stage = 2;
                    self.next = now + Duration::from_millis(1500);
                }
                2 if now >= self.next => {
                    let frames = app.outliner.as_ref().unwrap().frames - self.idle_frames;
                    if frames > 4 {
                        return Err(
                            format!("idle Outliner kept repainting: {frames} frames").into()
                        );
                    }
                    let path = self.directory.join("result.json");
                    let mut report: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
                    report["idle_frames"] = frames.into();
                    report["checks"]
                        .as_array_mut()
                        .unwrap()
                        .push("outliner-idle-rendering".into());
                    fs::write(path, serde_json::to_vec_pretty(&report)?)?;
                    eprintln!("Native Outliner idle: {frames} frames in 1.5 seconds");
                    return Ok(true);
                }
                _ => {}
            }
            return Ok(false);
        }
        if std::env::var("RUSTTY_SMOKE_DECK").as_deref() == Ok("only") {
            super::deck_host::check_native_deck(app, event_loop)?;
            return Ok(true);
        }
        if self.interactive_deck {
            if self.stage == 0 && !app.workspace.windows.is_empty() {
                app.add_window(false);
                for window in 0..2 {
                    let tab = app.workspace.id();
                    let pane = app.workspace.id();
                    app.workspace.windows[window].tabs.push(Tab::new(
                        tab,
                        pane,
                        self.directory.clone(),
                    ));
                    for tab in 0..2 {
                        for direction in [Direction::Right, Direction::Down] {
                            let pane = app.workspace.id();
                            let split = app.workspace.id();
                            app.workspace.windows[window].tabs[tab].split(
                                pane,
                                split,
                                direction,
                                self.directory.clone(),
                            );
                        }
                    }
                    app.workspace.windows[window].frame = [
                        100.0 + window as f64 * 150.0,
                        100.0 + window as f64 * 80.0,
                        1100.0,
                        720.0,
                    ];
                }
                app.changed();
                app.reconcile(event_loop);
                self.stage = 1;
                println!(
                    "Interactive Stream Deck fixture: 12 simulated reporters across two windows and four tabs; isolated workspace {}",
                    self.directory.display()
                );
            }
            return Ok(false);
        }
        if self.offscreen {
            // This mode exercises visible-host scheduling with offscreen Metal
            // output, even if macOS occludes its disposable test window.
            for host in app.windows.values_mut() {
                if host.occluded {
                    host.occluded = false;
                    host.repaint();
                    eprintln!(
                        "Native smoke: continuing offscreen rendering after native occlusion"
                    );
                }
            }
        }
        if Instant::now() > self.deadline {
            let windows: Vec<_> = app
                .windows
                .values()
                .map(|host| Platform::window_diagnostics(&host.window))
                .collect();
            let panes: Vec<_> = app
                .panes
                .iter()
                .map(|(&id, pane)| {
                    (
                        id,
                        pane.activity.progress(),
                        pane.input_bytes,
                        pane.session
                            .terminal()
                            .map(|terminal| terminal.plain_text()),
                    )
                })
                .collect();
            return Err(format!(
                "native smoke timed out at stage {}: {:?}; windows: {:?}; panes (id, progress, queued input bytes, text): {:?}",
                self.stage, app.errors, windows, panes
            )
            .into());
        }
        if Instant::now() < self.next {
            return Ok(false);
        }
        let Some(key) = app
            .windows
            .iter()
            .find(|(_, h)| {
                app.index(h.id)
                    .is_some_and(|i| !app.workspace.windows[i].quick)
            })
            .map(|(key, _)| *key)
        else {
            return Ok(false);
        };
        let stage = self.stage;
        let mut host = app.windows.remove(&key).unwrap();
        let result = self.step_window(app, event_loop, &mut host);
        app.windows.insert(key, host);
        app.reconcile(event_loop);
        let done = result?;
        if stage == 3 && self.stage == 7 {
            // Dispatch through the window handler with the host back in its map.
            check_passive_pointer_motion(app, event_loop, key)?;
            check_focus_hint_clicks(app, event_loop, key)?;
        }
        let complete = done;
        if complete && self.timing.is_none() {
            let mut host = app.windows.remove(&key).unwrap();
            let result = check_themes(app, event_loop, &mut host, &self.directory)
                .and_then(|_| check_tab_frame(app, event_loop, &mut host, &self.directory));
            app.windows.insert(key, host);
            result?;
            check_tmuxx_management(app, event_loop, &self.directory)?;
            if std::env::var_os("RUSTTY_SMOKE_DECK").is_some() {
                super::deck_host::check_native_deck(app, event_loop)?;
            }
            #[cfg(target_os = "windows")]
            check_last_window_exit(app, event_loop)?;
            let mut report = self.report.take().ok_or("missing smoke report")?;
            report["checks"].as_array_mut().unwrap().extend(
                [
                    "tab-color-preserves-panel-geometry",
                    "theme-preview",
                    "theme-cancel",
                    "theme-accept",
                    "theme-reload",
                    "tmuxx-theme-scope",
                    "tmuxx-theme-picker",
                    "tmuxx-server-management",
                    "tmuxx-fit-window",
                    "tmuxx-secondary-selection",
                    "tmuxx-disconnected-input",
                ]
                .map(serde_json::Value::from),
            );
            if cfg!(target_os = "windows") {
                report["checks"].as_array_mut().unwrap().extend(
                    ["last-window-exit", "layout-autosave", "layout-on-exit"]
                        .map(serde_json::Value::from),
                );
            }
            fs::write(
                self.directory.join("result.json"),
                serde_json::to_vec_pretty(&report)?,
            )?;
            println!("Native smoke passed: {}", self.directory.display());
        }
        Ok(complete)
    }
    fn step_window(
        &mut self,
        app: &mut App,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
    ) -> Result<bool> {
        if let Some(timing) = &mut self.timing {
            let result = timing.step(app, event_loop, host, &self.directory, self.offscreen);
            self.next = Instant::now() + Duration::from_millis(50);
            return result;
        }
        let text = |app: &App, id: Id| {
            app.panes
                .get(&id)
                .and_then(|p| p.session.terminal().ok().map(|t| t.plain_text()))
                .unwrap_or_default()
        };
        let ready = |app: &App, id: Id| {
            let text = text(app, id);
            // Windows prints the banner before exec starts interactive Bash.
            // Wait for its prompt before sending input or replacing the grid.
            text.contains("Rustty native smoke")
                && (!cfg!(target_os = "windows") || text.contains("RUSTTY_SMOKE_READY>"))
        };
        let pane = app.focused(host.id).ok_or("no active pane")?;
        match self.stage {
            0 => {
                if host.frames == 0 || !ready(app, pane) {
                    return Ok(false);
                }
                if host.content.y != 0.0 {
                    return Err("single-tab window reserved space for a tab bar".into());
                }
                {
                    let terminal = app.panes[&pane].session.terminal()?;
                    let screen = terminal.screen();
                    let row = screen.row(0);
                    let cells = row.cells;
                    if &*row.text(2) != "✔️"
                        || (cells[2].width(), cells[3].width()) != (2, 0)
                        || row.style(3).background != vt::Color::Indexed(15)
                    {
                        return Err("emoji checkmark lost its second cell's background".into());
                    }
                }
                self.original = Some(pane);
                app.write(pane, b"printf '\\122USTTY_INPUT_OK\\n'\r".to_vec());
                eprintln!("Native smoke: shell output received");
                self.stage = 1;
            }
            1 => {
                if !text(app, pane).contains("RUSTTY_INPUT_OK") {
                    return Ok(false);
                }
                app.action(event_loop, host, Action::NewSplit(Direction::Right), true);
                app.action(event_loop, host, Action::NewSplit(Direction::Down), true);
                app.focus_pane(host.id, self.original.unwrap());
                app.action(event_loop, host, Action::NewSplit(Direction::Down), true);
                if app.tab(host.id).unwrap().panes.len() != 4 {
                    return Err("split creation lost a pane".into());
                }
                app.action(event_loop, host, Action::NewSplit(Direction::Right), true);
                let animation_checks_started = Instant::now();
                check_zoom_animations(app, event_loop, host, &self.directory)?;
                self.deadline += animation_checks_started.elapsed();
                for (action, label, capture) in [
                    (Action::ToggleSplitZoom, "Zoom", "zoom.png"),
                    (Action::ToggleQuadrantZoom, "Quad-zoom", "quad-zoom.png"),
                    (
                        Action::ToggleSplitZoom,
                        "Zoom within quad-zoom",
                        "nested-zoom.png",
                    ),
                ] {
                    app.action(event_loop, host, action, true);
                    let tab = app.tab(host.id).unwrap();
                    if zoom_label(tab) != Some(label)
                        || app.tab_label(tab).0.ends_with(&format!(" [{label}]"))
                    {
                        return Err(
                            "zoom mode was lost or added text to its graphical tab indicator"
                                .into(),
                        );
                    }
                    app.draw(event_loop, host)?;
                    capture_window(app, event_loop, host, &self.directory.join(capture))?;
                    if !host
                        .window
                        .title()
                        .ends_with(&format!(" [{label}] — Rustty"))
                    {
                        return Err("window title did not reflect its zoom layer".into());
                    }
                }
                app.action(event_loop, host, Action::ToggleQuadrantZoom, true);
                if zoom_label(app.tab(host.id).unwrap()) != Some("Quad-zoom") {
                    return Err("nested zoom did not return to its quadrant".into());
                }
                app.action(event_loop, host, Action::CloseSurface, true);
                if app.tab(host.id).unwrap().panes.len() != 4
                    || zoom_label(app.tab(host.id).unwrap()).is_some()
                {
                    return Err("closing to one quadrant pane did not leave zoom".into());
                }
                app.action(event_loop, host, Action::ToggleQuadrantZoom, true);
                let before = app.focused(host.id).unwrap();
                host.peek = app.tab_mut(host.id).unwrap().begin_peek(config::Modifiers {
                    control: true,
                    super_key: true,
                    ..Default::default()
                });
                if !app.action(
                    event_loop,
                    host,
                    Action::GotoSplit(Direction::QuadrantRight),
                    true,
                ) {
                    return Err("quadrant navigation was blocked".into());
                }
                let peek = host.peek.take().unwrap();
                let target = app.tab_mut(host.id).unwrap().finish_peek(peek);
                if target == before {
                    return Err("quadrant navigation kept old focus".into());
                }
                app.focus_pane(host.id, target);
                app.action(event_loop, host, Action::ToggleQuadrantZoom, true);
                let before = app.panes.len();
                app.action(event_loop, host, Action::NewTab, true);
                if app.panes.len() != before + 1 {
                    return Err("tab creation did not start a fifth PTY".into());
                }
                app.draw(event_loop, host)?;
                if host.content.y != 38.0 {
                    return Err("second tab did not reveal the tab bar".into());
                }
                app.action(event_loop, host, Action::CloseTab, true);
                app.draw(event_loop, host)?;
                if host.content.y != 0.0 {
                    return Err("closing the second tab did not hide the tab bar".into());
                }
                if !app.action(event_loop, host, Action::Undo, true) {
                    return Err("closing the second tab could not be undone".into());
                }
                app.draw(event_loop, host)?;
                if host.content.y != 38.0 {
                    return Err("restoring the second tab did not reveal the tab bar".into());
                }
                app.action(event_loop, host, Action::PreviousTab, true);
                eprintln!("Native smoke: splits, tabs, and quadrant actions passed");
                self.stage = 2;
            }
            2 => {
                if app.panes.keys().any(|&id| !ready(app, id)) {
                    return Ok(false);
                }
                if app.panes.values().any(|p| p.cwd != fixture_directory()) {
                    return Err("OSC directory was not decoded before restoration".into());
                }
                if !self.progress_refreshed {
                    // Captures may outlive the startup report; exercise a fresh PTY report.
                    for id in app.panes.keys().copied().collect::<Vec<_>>() {
                        app.write(id, b"printf '\\033]9;4;1;65\\007'\r".to_vec());
                    }
                    self.progress_refreshed = true;
                    return Ok(false);
                }
                if app.panes.values().any(|p| {
                    p.activity.progress()
                        != Some(Progress {
                            state: 1,
                            value: Some(65),
                        })
                }) {
                    // The worker exposes terminal text before the UI drains
                    // its queued progress effects. Wait for both, bounded by
                    // the smoke deadline, rather than racing event delivery.
                    return Ok(false);
                }
                if self.closed.is_none() {
                    self.closed = Some((pane, app.panes[&pane].started));
                    app.action(event_loop, host, Action::CloseSurface, true);
                    self.stage = 5;
                    return Ok(false);
                }
                app.save();
                let restored =
                    Workspace::load(&app.state_path)?.ok_or("workspace was not saved")?;
                if restored.windows[0].tabs.len() != 2
                    || restored.windows[0].tabs[0].panes.len() != 4
                {
                    return Err("restoration lost tabs or splits".into());
                }
                eprintln!(
                    "Native smoke: requesting frame, visible={}, occluded={}, frames={}, format={:?}",
                    host.visible,
                    host.occluded,
                    host.frames,
                    app.painter.render_state().map(|s| s.target_format)
                );
                host.capture = true;
                host.repaint();
                self.stage = 3;
            }
            3 => {
                let capture_path = self.directory.join("window.png");
                if !capture_path.is_file() {
                    if let Some(state) = app.painter.render_state() {
                        state.device.poll(wgpu::PollType::Poll)?;
                    }
                    let mut events = Vec::new();
                    app.painter.handle_screenshots(&mut events);
                    if events.is_empty() {
                        host.capture = true;
                        host.repaint();
                        self.next = Instant::now() + Duration::from_millis(100);
                    }
                    for event in events {
                        if let egui::Event::Screenshot { image, .. } = event {
                            let file = fs::File::create(&capture_path)?;
                            let mut encoder = png::Encoder::new(
                                file,
                                image.width() as u32,
                                image.height() as u32,
                            );
                            encoder.set_color(png::ColorType::Rgba);
                            encoder.set_depth(png::BitDepth::Eight);
                            encoder.write_header()?.write_image_data(
                                &image
                                    .pixels
                                    .iter()
                                    .flat_map(|c| c.to_array())
                                    .collect::<Vec<_>>(),
                            )?;
                        }
                    }
                }
                if capture_path.is_file() {
                    #[cfg(target_os = "windows")]
                    if std::env::var_os("RUSTTY_SMOKE_NATIVE_CAPTURE").is_some() {
                        windows_capture::capture(
                            &host.window,
                            &self.directory.join("native-window.png"),
                        )?;
                    }
                    check_pointer_targets(app, host)?;
                    check_terminal_frames(app, event_loop, host)?;
                    check_find(app, event_loop, host, &self.directory)?;
                    self.idle_frames = host.frames;
                    self.progress_pane = Some(pane);
                    self.progress_started = Instant::now();
                    // Find captures can take long enough on software adapters
                    // that startup progress expires during the later idle check.
                    // Refresh each fixture report before the timed phases so
                    // completion flashes do not count as idle redraws.
                    for pane in app.panes.values_mut() {
                        if let Some(progress) = pane.activity.progress() {
                            pane.activity.progress_reported(
                                progress.state,
                                progress.value,
                                self.progress_started,
                            );
                        }
                    }
                    app.panes
                        .get_mut(&pane)
                        .unwrap()
                        .activity
                        .progress_reported(3, None, self.progress_started);
                    // Changing keyboard focus must not leave the pane whose
                    // animation we started running during the later idle check.
                    let other = *host.rects.keys().find(|&&id| id != pane).unwrap();
                    app.focus_pane(host.id, other);
                    host.repaint();
                    self.next = self.progress_started + Duration::from_secs(2);
                    self.stage = 7;
                }
            }
            4 => {
                if self.hover
                    && !self.pointer.is_some_and(|position| {
                        host.rects.values().any(|rect| rect.contains(position))
                    })
                {
                    self.input(host.frames);
                    return Ok(false);
                }
                if !app.errors.is_empty() {
                    return Err(format!("native app errors: {:?}", app.errors).into());
                }
                if host.frames.saturating_sub(self.idle_frames) > 4 {
                    return Err(format!(
                        "idle window kept repainting: {} frames; events: {:?}",
                        host.frames - self.idle_frames,
                        self.events
                    )
                    .into());
                }
                if self.events.get("about-to-wait").copied().unwrap_or(0) > 400 {
                    return Err(format!("idle event loop kept waking: {:?}", self.events).into());
                }
                let refresh_hz = host
                    .window
                    .current_monitor()
                    .and_then(|monitor| monitor.refresh_rate_millihertz())
                    .map(|rate| f64::from(rate) / 1000.0);
                let report = serde_json::json!({"passed":true,"renderer":app.painter.description(),"capture_mode":if self.offscreen { "offscreen" } else { "surface" },"checks":["native-window","native-rendered-frame","pty-input-output","unicode-grapheme-width","four-splits","tab-creation","quadrant-focus-and-zoom","zoom-animation-endpoint-grids","zoom-animation-disable","quad-peek-animation","cwd-uri-decoding","osc-progress","progress-animation","passive-pointer-motion","hover-scrolling","alternate-scrolling","file-drop-targeting","osc-pointer","command-hover-links","double-click-selection","drag-selection-scrolling","focus-hint-click-dismissal","reverse-video","dec-column-mode","text-blink","synchronized-output","per-pane-find","find-transparency","hidden-tab-titles","active-masked-titles","retained-pane-content","workspace-roundtrip","undo-keeps-pty","idle-rendering"],"frames":host.frames,"idle_frames":host.frames-self.idle_frames,"hidden_title_frames":self.hidden_title_frames,"header_updates":{"frames":self.header_frames,"pane_prepares":self.header_prepares},"active_title_updates":{"count":50,"rate_hz":25,"frames":self.active_title_frames,"pane_prepares":self.active_title_prepares},"progress_animation":{"frames":self.progress_frames,"seconds":self.progress_seconds,"fps":self.progress_frames as f64/self.progress_seconds,"monitor_refresh_hz":refresh_hz},"panes":app.panes.len(),"idle_phase_events":self.events,"hover_required":self.hover,"pointer":self.pointer.map(|position|[position.x,position.y])});
                self.report = Some(report);
                return Ok(true);
            }
            5 => {
                let (closed, started) = self.closed.unwrap();
                if app.tab(host.id).unwrap().panes.contains_key(&closed)
                    || app
                        .panes
                        .get(&closed)
                        .is_none_or(|pane| pane.started != started)
                {
                    return Err("closed terminal was not retained for undo".into());
                }
                if !app.action(event_loop, host, Action::Undo, true) {
                    return Err("close could not be undone".into());
                }
                self.stage = 6;
            }
            6 => {
                let (closed, started) = self.closed.unwrap();
                if !app.tab(host.id).unwrap().panes.contains_key(&closed)
                    || app
                        .panes
                        .get(&closed)
                        .is_none_or(|pane| pane.started != started)
                    || !text(app, closed).contains("Rustty native smoke")
                {
                    return Err("undo did not restore the same terminal process".into());
                }
                self.stage = 2;
            }
            7 => {
                self.progress_frames = host.frames.saturating_sub(self.idle_frames);
                self.progress_seconds = self.progress_started.elapsed().as_secs_f64();
                if !self.offscreen && self.progress_frames == 0 {
                    return Err("indefinite progress did not render any animation frames".into());
                }
                eprintln!(
                    "Native smoke: indefinite progress rendered {} frames in {:.3}s ({:.1} FPS)",
                    self.progress_frames,
                    self.progress_seconds,
                    self.progress_frames as f64 / self.progress_seconds
                );
                app.panes
                    .get_mut(&self.progress_pane.unwrap())
                    .unwrap()
                    .activity
                    .progress_reported(1, Some(65), Instant::now());
                let index = app.index(host.id).unwrap();
                let window = &mut app.workspace.windows[index];
                let hidden = window
                    .tabs
                    .iter_mut()
                    .enumerate()
                    .find(|(index, _)| *index != window.active_tab)
                    .map(|(_, tab)| tab)
                    .ok_or("no hidden tab for title checks")?;
                hidden.title = Some("Background agent".into());
                let id = hidden.focused;
                app.write(id, b"i=0; while [ \"$i\" -lt 20 ]; do printf '\\033]2;hidden-agent-%s\\007' \"$i\"; i=$((i+1)); sleep 0.1; done\r".to_vec());
                host.repaint();
                self.idle_frames = host.frames;
                self.next = Instant::now() + Duration::from_secs(3);
                self.stage = 8;
            }
            8 => {
                let window = &app.workspace.windows[app.index(host.id).unwrap()];
                let hidden = window
                    .tabs
                    .iter()
                    .find(|tab| tab.title.as_deref() == Some("Background agent"))
                    .unwrap();
                if app.panes[&hidden.focused].title != "hidden-agent-19"
                    || hidden.panes[&hidden.focused].title.as_deref() != Some("hidden-agent-19")
                {
                    // Spawning sleep through MSYS can take longer than on Unix.
                    // Await all reports under the smoke's overall deadline.
                    self.next = Instant::now() + Duration::from_millis(100);
                    return Ok(false);
                }
                self.hidden_title_frames = host.frames.saturating_sub(self.idle_frames);
                if self.hidden_title_frames > 4 {
                    let progress: Vec<_> = app
                        .panes
                        .iter()
                        .map(|(&id, pane)| (id, pane.activity.progress()))
                        .collect();
                    return Err(format!(
                        "masked hidden-tab titles caused {} redraws; events: {:?}; progress: {:?}; deadline: {:?}; repaint causes: {:?}",
                        self.hidden_title_frames, self.events, progress, host.deadline,
                        app.context.repaint_causes()
                    )
                    .into());
                }
                eprintln!(
                    "Native smoke: 20 masked hidden-tab titles caused {} settling frames",
                    self.hidden_title_frames
                );
                let index = app.index(host.id).unwrap();
                let window = &mut app.workspace.windows[index];
                let hidden = window
                    .tabs
                    .iter_mut()
                    .find(|tab| tab.title.as_deref() == Some("Background agent"))
                    .unwrap();
                hidden.title = None;
                let id = hidden.focused;
                self.header_frames = host.frames;
                self.header_prepares = host.pane_prepares;
                app.write(id, b"i=0; while [ \"$i\" -lt 20 ]; do printf '\\033]2;visible-agent-%s\\007' \"$i\"; i=$((i+1)); sleep 0.1; done\r".to_vec());
                host.repaint();
                self.next = Instant::now() + Duration::from_secs(3);
                self.stage = 9;
            }
            9 => {
                let window = &app.workspace.windows[app.index(host.id).unwrap()];
                let hidden = window
                    .tabs
                    .iter()
                    .enumerate()
                    .find(|(index, _)| *index != window.active_tab)
                    .unwrap()
                    .1;
                if app.panes[&hidden.focused].title != "visible-agent-19" {
                    self.next = Instant::now() + Duration::from_millis(100);
                    return Ok(false);
                }
                self.header_frames = host.frames.saturating_sub(self.header_frames);
                self.header_prepares = host.pane_prepares.saturating_sub(self.header_prepares);
                if self.header_frames < 10 {
                    return Err("visible tab-label updates stopped repainting".into());
                }
                // Native focus/resize events can invalidate retained panes; title
                // updates must otherwise keep reusing their terminal content.
                if self.header_prepares > 8 {
                    return Err(format!(
                        "{} tab-header frames rebuilt panes {} times",
                        self.header_frames, self.header_prepares
                    )
                    .into());
                }
                eprintln!(
                    "Native smoke: {} tab-header frames needed only {} pane preparations",
                    self.header_frames, self.header_prepares
                );
                self.active_title_tab = app
                    .tab_mut(host.id)
                    .unwrap()
                    .title
                    .replace("Foreground agent".into());
                self.active_title_accesskit = std::mem::replace(&mut host.accesskit_active, false);
                for pane in app.panes.values_mut() {
                    pane.activity.progress_reported(0, None, Instant::now());
                }
                app.activity_flashes.clear();
                // The reads keep shell setup and cleanup outside the measured interval.
                app.write(pane, b"stty -echo; printf '\\033[?25l\\033]2;active-title-ready\\007'; read rustty_smoke_go; sleep 0.2; i=0; while [ \"$i\" -lt 50 ]; do printf '\\033]2;active-agent-%s\\007' \"$i\"; i=$((i+1)); sleep 0.04; done; sleep 0.2; read rustty_smoke_done; printf '\\033[?25h'; stty echo\r".to_vec());
                host.repaint();
                self.next = Instant::now() + Duration::from_millis(500);
                self.stage = 10;
            }
            10 => {
                if app.panes[&pane].title != "active-title-ready" {
                    return Ok(false);
                }
                // The ready effect can arrive before its initial redraw or the
                // shell's final setup bytes. Settle before starting the counter.
                self.next = Instant::now() + Duration::from_millis(200);
                self.stage = 13;
            }
            13 => {
                if !host.prepared.get(&pane).is_some_and(|prepared| {
                    app.panes[&pane]
                        .session
                        .terminal()
                        .is_ok_and(|terminal| prepared.key.matches_terminal(&terminal))
                }) {
                    host.repaint();
                    return Ok(false);
                }
                self.active_title_frames = host.frames;
                self.active_title_prepares = host.pane_prepares;
                self.events.clear();
                app.write(pane, b"\n".to_vec());
                self.stage = 11;
            }
            11 => {
                if app.panes[&pane].title != "active-agent-49" {
                    return Ok(false);
                }
                self.next = Instant::now() + Duration::from_millis(300);
                self.stage = 12;
            }
            12 => {
                self.active_title_frames = host.frames.saturating_sub(self.active_title_frames);
                self.active_title_prepares = host
                    .pane_prepares
                    .saturating_sub(self.active_title_prepares);
                if self.active_title_frames != 0 || self.active_title_prepares != 0 {
                    return Err(format!(
                        "50 active masked titles at 25 Hz caused {} frames and {} pane preparations; events: {:?}",
                        self.active_title_frames, self.active_title_prepares, self.events
                    )
                    .into());
                }
                if host.window.title() != "active-agent-49 — Rustty"
                    || app.tab(host.id).unwrap().panes[&pane].title.as_deref()
                        != Some("active-agent-49")
                {
                    return Err("active titles did not update native and saved title state".into());
                }
                eprintln!(
                    "Native smoke: 50 active masked titles at 25 Hz updated native/saved titles with zero frames and pane preparations"
                );
                app.tab_mut(host.id).unwrap().title = self.active_title_tab.take();
                host.accesskit_active = self.active_title_accesskit;
                app.write(pane, b"\n".to_vec());
                host.repaint();
                self.idle_frames = host.frames;
                self.events.clear();
                self.next = Instant::now() + Duration::from_millis(1500);
                self.stage = 4;
            }
            _ => unreachable!(),
        }
        Ok(false)
    }
}

fn check_zoom_animations(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    host: &mut Host,
    directory: &Path,
) -> Result<()> {
    let original = app.tab(host.id).unwrap().clone();
    let animations = app.loaded.config.window_animations;
    let background = app.loaded.config.background;
    let background_mix = app.loaded.config.zoom_animation_background_mix;
    let durations = (
        app.loaded.config.quad_zoom_animation_duration,
        app.loaded.config.quad_peek_animation_duration,
        app.loaded.config.zoom_animation_duration,
        app.loaded.config.zoom_from_quad_animation_duration,
    );
    app.draw(event_loop, host)?;
    let mut saved = Vec::new();
    for id in original.pane_ids() {
        let mut terminal = app.panes[&id].session.terminal()?;
        let mut fixture = vt::Terminal::new(terminal.cols, terminal.rows, 64);
        fixture.background = [4, 12, 20];
        fixture.screen_mut().cursor.visible = false;
        fixture.feed(
            format!("\x1b[1;36mPane {id} zoom animation\x1b[0m\r\nStable terminal grid; sliding pane bounds.\r\n")
                .repeat(usize::from(terminal.rows) / 2)
                .as_bytes(),
        );
        saved.push((
            id,
            std::mem::replace(&mut *terminal, fixture),
            app.panes[&id].cwd.clone(),
        ));
        host.prepared.remove(&id);
    }
    let result = (|| -> Result<()> {
        app.loaded.config.background = config::Rgb::new(20, 30, 40);
        app.loaded.config.zoom_animation_background_mix = 0.5;
        app.tab_mut(host.id).unwrap().color = Some([210, 90, 230]);
        app.draw(event_loop, host)?;
        app.loaded.config.window_animations = Some(true);
        app.loaded.config.quad_zoom_animation_duration = Duration::from_millis(600);
        app.loaded.config.quad_peek_animation_duration = Duration::from_millis(320);
        app.loaded.config.zoom_animation_duration = Duration::from_millis(800);
        app.loaded.config.zoom_from_quad_animation_duration = Duration::from_millis(1000);
        for (name, action, duration) in [
            (
                "quad-in",
                Action::ToggleQuadrantZoom,
                app.loaded.config.quad_zoom_animation_duration,
            ),
            (
                "nested-in",
                Action::ToggleSplitZoom,
                app.loaded.config.zoom_from_quad_animation_duration,
            ),
            (
                "nested-out",
                Action::ToggleSplitZoom,
                app.loaded.config.zoom_from_quad_animation_duration,
            ),
            (
                "quad-out",
                Action::ToggleQuadrantZoom,
                app.loaded.config.quad_zoom_animation_duration,
            ),
            (
                "pane-in",
                Action::ToggleSplitZoom,
                app.loaded.config.zoom_animation_duration,
            ),
            (
                "pane-out",
                Action::ToggleSplitZoom,
                app.loaded.config.zoom_animation_duration,
            ),
        ] {
            check_zoom_transition(
                app,
                event_loop,
                host,
                directory,
                (name, duration),
                |app, event_loop, host| {
                    app.action(event_loop, host, action, true);
                    Ok(())
                },
            )?;
        }
        app.loaded.config.window_animations = Some(false);
        app.action(event_loop, host, Action::ToggleQuadrantZoom, true);
        app.draw(event_loop, host)?;
        if host.zoom_animation.is_running() {
            return Err("window-animations=false left a zoom animation running".into());
        }
        app.loaded.config.window_animations = Some(true);
        for (id, _, _) in &saved {
            app.panes.get_mut(id).unwrap().cwd =
                fixture_directory().join(format!("zoom-panel-{id}"));
        }
        app.draw(event_loop, host)?;
        for name in ["nested-labels-in", "nested-labels-out"] {
            check_zoom_transition(
                app,
                event_loop,
                host,
                directory,
                (name, app.loaded.config.zoom_from_quad_animation_duration),
                |app, event_loop, host| {
                    app.action(event_loop, host, Action::ToggleSplitZoom, true);
                    Ok(())
                },
            )?;
        }
        for (id, _, cwd) in &saved {
            app.panes.get_mut(id).unwrap().cwd = cwd.clone();
        }
        app.draw(event_loop, host)?;
        for enter in [true, false] {
            check_zoom_transition(
                app,
                event_loop,
                host,
                directory,
                (
                    if enter { "peek-in" } else { "peek-out" },
                    app.loaded.config.quad_peek_animation_duration,
                ),
                |app, _, host| {
                    if enter {
                        host.peek = app.tab_mut(host.id).unwrap().begin_peek(config::Modifiers {
                            control: true,
                            super_key: true,
                            ..Default::default()
                        });
                        if host.peek.is_none() {
                            return Err("animation fixture could not enter Quad Peek".into());
                        }
                    } else {
                        let peek = host.peek.take().ok_or("animation fixture lost Quad Peek")?;
                        let target = app.tab_mut(host.id).unwrap().finish_peek(peek);
                        app.focus_pane(host.id, target);
                    }
                    Ok(())
                },
            )?;
        }
        Ok(())
    })();
    app.loaded.config.window_animations = animations;
    app.loaded.config.background = background;
    app.loaded.config.zoom_animation_background_mix = background_mix;
    (
        app.loaded.config.quad_zoom_animation_duration,
        app.loaded.config.quad_peek_animation_duration,
        app.loaded.config.zoom_animation_duration,
        app.loaded.config.zoom_from_quad_animation_duration,
    ) = durations;
    *app.tab_mut(host.id).unwrap() = original;
    host.peek = None;
    host.zoom_animation.finish();
    for (id, terminal, cwd) in saved {
        let pane = app.panes.get_mut(&id).unwrap();
        pane.cwd = cwd;
        *pane.session.terminal()? = terminal;
        host.prepared.remove(&id);
    }
    app.draw(event_loop, host)?;
    result?;
    eprintln!(
        "Native smoke: configured pane, quad, nested zoom and Quad Peek animations kept endpoint grids and partially visible neighbors throughout the slide; quarter/midpoint/three-quarter captures saved"
    );
    Ok(())
}

fn check_zoom_transition(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    host: &mut Host,
    directory: &Path,
    (name, duration): (&str, Duration),
    trigger: impl FnOnce(&mut App, &ActiveEventLoop, &mut Host) -> Result<()>,
) -> Result<()> {
    let ids = app.tab(host.id).unwrap().pane_ids();
    let source = host.rects.keys().copied().collect::<HashSet<_>>();
    let sizes = |app: &App| -> Result<BTreeMap<Id, (u16, u16)>> {
        ids.iter()
            .map(|id| {
                let terminal = app.panes[id].session.terminal()?;
                Ok((*id, (terminal.cols, terminal.rows)))
            })
            .collect()
    };
    let before = sizes(app)?;
    trigger(app, event_loop, host)?;
    app.draw(event_loop, host)?;
    if !host.zoom_animation.is_running() {
        return Err(format!("{name} did not start its animation").into());
    }
    let target = host.rects.keys().copied().collect::<HashSet<_>>();
    let mut initial = before.clone();
    let mut after = before.clone();
    let tab = app.tab(host.id).unwrap();
    let scale = host.window.scale_factor() as f32;
    let metrics = host.fonts.metrics();
    for (id, rect) in tab.layout(
        host.content,
        host.peek.is_some(),
        metrics.cell_height as f32 / scale,
    ) {
        let size = pane_grid_size(
            Vec2::new(rect.width - 2.0, rect.height - 2.0),
            scale,
            [metrics.cell_width, metrics.cell_height],
            app.window_config(host.id),
            if tab.panes.len() > 1 { 18.0 } else { 0.0 },
        );
        after.insert(id, size);
        if !source.contains(&id) {
            initial.insert(id, size);
        }
    }
    if sizes(app)? != initial {
        return Err(format!("{name} resized moving terminals before its midpoint").into());
    }
    if before == after {
        return Err(format!("{name} fixture did not change any terminal dimensions").into());
    }
    let stationary = source
        .symmetric_difference(&target)
        .copied()
        .collect::<HashSet<_>>();
    let mut overview = app.tab(host.id).unwrap().clone();
    overview.zoom = None;
    overview.quadrant_zoom = None;
    let mut retained_labels = BTreeMap::new();
    if !name.starts_with("peek-") {
        for id in &stationary {
            let Some(quadrant) = overview.root.quadrant(*id) else {
                continue;
            };
            if overview
                .root
                .node(quadrant)
                .unwrap()
                .panes()
                .iter()
                .all(|id| stationary.contains(id))
                && let Some(label) = app.quadrant_label(&overview, host, quadrant, None, false)
            {
                retained_labels.insert(quadrant, label);
            }
        }
    }
    if matches!(name, "quad-in" | "quad-out") && retained_labels.is_empty() {
        return Err(format!("{name} fixture has no stationary shared quadrant labels").into());
    }
    for (part, elapsed) in [
        ("quarter", duration / 4),
        ("midpoint", duration / 2),
        ("threequarter", duration * 3 / 4),
    ] {
        host.zoom_animation.started = Some(Instant::now() - elapsed);
        capture_window(
            app,
            event_loop,
            host,
            &directory.join(format!("animation-{name}-{part}.png")),
        )?;
        let expected = if part == "quarter" { &initial } else { &after };
        let actual = sizes(app)?;
        if &actual != expected {
            return Err(format!(
                "{name} {part} used intermediate or mistimed terminal dimensions: {actual:?}, expected {expected:?}"
            )
            .into());
        }
        let composed = host.composed.as_ref().ok_or("missing zoom composition")?;
        if composed.panes.len() != source.union(&target).count() {
            return Err(format!("{name} {part} dropped panels during the slide").into());
        }
        let config = app.window_config(host.id);
        let window_frame = rustty_render::Color::rgb(config.background.to_array());
        let retained = 1.0 - config.zoom_animation_background_mix;
        let mut neighbors = 0;
        for pane in &composed.panes {
            let paint = pane.transition.ok_or("missing zoom paint")?;
            if paint.frame_background != window_frame {
                return Err(format!("{name} {part} faded toward the tab accent").into());
            }
            if paint.moving {
                if part == "midpoint" && paint.opacity > 0.02 {
                    return Err(format!("{name} left moving text visible at its midpoint").into());
                }
            } else {
                neighbors += 1;
                if !(retained..1.0).contains(&paint.opacity)
                    || !(retained..1.0).contains(&paint.background_opacity)
                {
                    return Err(format!(
                        "{name} {part} did not keep neighboring text and backgrounds partially visible"
                    )
                    .into());
                }
                if part == "midpoint"
                    && ((paint.opacity - retained).abs() > 0.02
                        || (paint.background_opacity - retained).abs() > 0.02)
                {
                    return Err(
                        format!("{name} did not reach its background mix by midpoint").into(),
                    );
                }
            }
        }
        if neighbors == 0 || neighbors != source.symmetric_difference(&target).count() {
            return Err(format!("{name} {part} did not retain its stationary neighbors").into());
        }
        let mut displayed = app.tab(host.id).unwrap().clone();
        if let Some((zoom, quadrant, _)) = host.zoom_animation.display_zoom() {
            displayed.zoom = zoom;
            displayed.quadrant_zoom = quadrant;
        }
        for (quadrant, expected) in &retained_labels {
            if app
                .quadrant_label(&displayed, host, *quadrant, None, true)
                .as_ref()
                != Some(expected)
            {
                return Err(
                    format!("{name} {part} changed a stationary shared quadrant label").into(),
                );
            }
        }
        if matches!(name, "nested-in" | "nested-out")
            && let Some(quadrant) = displayed.quadrant_zoom
            && displayed.zoom != Some(quadrant)
        {
            let members = displayed.root.node(quadrant).unwrap().panes();
            displayed.focused = ids
                .iter()
                .copied()
                .find(|id| !members.contains(id))
                .ok_or("nested zoom fixture has no pane outside its quadrant")?;
            // Make the label eligible so focus hiding cannot mask lost zoom suppression.
            if app
                .quadrant_label(&displayed, host, quadrant, None, true)
                .is_none()
            {
                return Err("nested zoom fixture has no shared quadrant label".into());
            }
            if app
                .quadrant_label(&displayed, host, quadrant, None, false)
                .is_some()
            {
                return Err(format!(
                    "{name} {part} showed the fullscreen pane's shared quadrant label"
                )
                .into());
            }
        }
        if name.starts_with("nested-labels-") {
            let tab = app.tab(host.id).unwrap();
            let config = app.window_config(host.id);
            // Reuse the drawn frame's time so this probe cannot cross the midpoint.
            let visuals = host.zoom_animation.update(
                tab,
                host.peek.is_some(),
                host.content,
                metrics.cell_height as f32 / scale,
                scale,
                config,
                true,
                host.last_frame_started.ok_or("missing zoom frame time")?,
            );
            let mut labels = 0;
            for visual in visuals.iter().filter(|pane| !pane.moving) {
                let label = DirectoryLabel::new(
                    directory_name(&app.panes[&visual.id].cwd),
                    false,
                    host.focused,
                    false,
                )
                .ok_or("missing distinct zoom panel label")?;
                if !label.centered_in_pane(tab, visual.id, visual.label_quadrant) {
                    return Err(format!(
                        "{name} {part} moved neighboring panel {} label out of the center",
                        visual.id
                    )
                    .into());
                }
                labels += 1;
            }
            if labels == 0 {
                return Err("nested label fixture has no stationary neighbor".into());
            }
        }
        if part != "midpoint" {
            let prepared = host.pane_prepares;
            host.zoom_animation.started = Some(Instant::now() - elapsed - duration / 20);
            app.draw(event_loop, host)?;
            if host.pane_prepares != prepared {
                return Err(format!("{name} {part} rebuilt unchanged terminal content").into());
            }
        }
    }
    host.zoom_animation.finish();
    app.draw(event_loop, host)?;
    if sizes(app)? != after || host.zoom_animation.is_running() {
        return Err(format!("{name} did not settle at its final dimensions").into());
    }
    Ok(())
}

fn check_tab_frame(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    host: &mut Host,
    directory: &Path,
) -> Result<()> {
    let original = app.tab(host.id).unwrap().color;
    let result = (|| {
        app.tab_mut(host.id).unwrap().color = None;
        capture_window(
            app,
            event_loop,
            host,
            &directory.join("tab-frame-system-accent.png"),
        )?;
        let geometry = host.rects.clone();
        let panels: BTreeMap<_, _> = host
            .prepared
            .iter()
            .map(|(id, pane)| {
                let options = &pane.key.options;
                (*id, (options.size, options.padding, options.background))
            })
            .collect();
        if panels.is_empty() {
            return Err("tab-frame fixture has no prepared panels".into());
        }
        for (color, name) in [
            ([240, 190, 20], "tab-frame-yellow.png"),
            ([45, 125, 210], "tab-frame-blue.png"),
        ] {
            app.tab_mut(host.id).unwrap().color = Some(color);
            capture_window(app, event_loop, host, &directory.join(name))?;
            let colored: BTreeMap<_, _> = host
                .prepared
                .iter()
                .map(|(id, pane)| {
                    let options = &pane.key.options;
                    (*id, (options.size, options.padding, options.background))
                })
                .collect();
            if host.rects != geometry || colored != panels {
                return Err("tab color changed panel geometry or terminal backgrounds".into());
            }
        }
        Ok(())
    })();
    app.tab_mut(host.id).unwrap().color = original;
    host.repaint();
    result
}

fn check_themes(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    host: &mut Host,
    directory: &Path,
) -> Result<()> {
    let directory = directory.join("theme-fixtures");
    let user = directory.join("config/rustty");
    let bundled = directory.join("resources/themes");
    fs::create_dir_all(user.join("themes"))?;
    fs::create_dir_all(&bundled)?;
    let files = [
        (
            user.join("config.rustty"),
            "theme = Saved\nstream-deck = false\nfont-size = 13\n",
        ),
        (
            user.join("themes/A Preview"),
            "background = 445566\nforeground = ddeeff\n",
        ),
        (
            user.join("themes/Saved"),
            "background = 112233\nforeground = aabbcc\n",
        ),
        (
            bundled.join("Bundled"),
            "background = 778899\nforeground = ffffff\n",
        ),
        (
            bundled.join("Z Scoped"),
            "background = eeeeee\nforeground = 111111\nfont-size = 20\ncommand = exit\nwindow-theme = light\n",
        ),
    ];
    for (path, source) in &files {
        fs::write(path, source)?;
    }
    let loader = config::ConfigLoader {
        home: directory.clone(),
        xdg_config_home: directory.join("config"),
        app_config_home: None,
        resources_dir: Some(directory.join("resources")),
        dark_mode: true,
        working_directory: directory.clone(),
    };
    let original_loader = std::mem::replace(&mut app.config_loader, loader);
    let original_args = std::mem::take(&mut app.config_args);
    let original_loaded = app.loaded.clone();
    let original_workspace = app.workspace.clone();
    let original_tmuxx = std::mem::take(&mut app.tmuxx);
    let original_tmuxx_themes = std::mem::take(&mut app.tmuxx_themes);
    let original_errors = std::mem::take(&mut app.errors);
    let original_palette = std::mem::take(&mut host.palette);
    let original_focus = host.focused;
    let original_text_focus = host.focus_text_input;
    host.focused = true;
    host.egui.egui_input_mut().focused = true;
    host.egui
        .egui_input_mut()
        .events
        .push(egui::Event::WindowFocused(true));
    let key = |host: &mut Host, key| {
        for pressed in [true, false] {
            host.egui.egui_input_mut().events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
    };
    let colors = |app: &App, background, foreground| -> Result<()> {
        for pane in app.panes.values() {
            let terminal = pane.session.terminal()?;
            if terminal.background != background || terminal.foreground != foreground {
                return Err("theme selection did not update every pane's default colors".into());
            }
        }
        Ok(())
    };
    let result = (|| -> Result<()> {
        app.loaded = app.config_loader.load();
        app.apply_theme_config(
            theme_preview::appearance(&original_loaded.config, app.loaded.config.clone()),
            Some(host),
            &theme_preview::Scope::Global,
        );
        let saved = app.config().clone();
        let preview_line = app
            .config_loader
            .themes(app.loaded.family)?
            .iter()
            .find(|theme| theme.name == "A Preview")
            .map(|theme| format!("\ntheme = {}\n", theme.config_value()))
            .ok_or("missing preview theme")?;
        colors(app, [0x11, 0x22, 0x33], [0xaa, 0xbb, 0xcc])?;
        app.action(event_loop, host, Action::Themes, false);
        if !host.palette.open || app.theme_preview.is_none() {
            return Err("Themes action did not open the theme palette".into());
        }
        app.draw(event_loop, host)?;
        app.draw(event_loop, host)?;
        colors(app, [0x44, 0x55, 0x66], [0xdd, 0xee, 0xff])?;
        capture_window(
            app,
            event_loop,
            host,
            &directory.parent().unwrap().join("theme-picker.png"),
        )?;
        key(host, egui::Key::ArrowDown);
        app.draw(event_loop, host)?;
        colors(app, [0x11, 0x22, 0x33], [0xaa, 0xbb, 0xcc])?;
        key(host, egui::Key::ArrowDown);
        app.draw(event_loop, host)?;
        colors(app, [0x77, 0x88, 0x99], [0xff; 3])?;
        key(host, egui::Key::Escape);
        app.draw(event_loop, host)?;
        if app.config() != &saved || app.theme_preview.is_some() || host.palette.open {
            return Err("Escape did not restore the appearance before theme selection".into());
        }
        colors(app, [0x11, 0x22, 0x33], [0xaa, 0xbb, 0xcc])?;
        app.action(event_loop, host, Action::Themes, false);
        app.draw(event_loop, host)?;
        key(host, egui::Key::Enter);
        app.draw(event_loop, host)?;
        if app.theme_preview.is_some()
            || host.palette.open
            || !app
                .errors
                .iter()
                .any(|message| message.contains(&preview_line))
        {
            return Err("accepting a theme did not keep it and show its configuration line".into());
        }
        app.draw(event_loop, host)?;
        if !host.messages_open {
            return Err("accepted theme did not open the messages popup".into());
        }
        if host.ui_input() {
            return Err("messages popup took terminal input".into());
        }
        colors(app, [0x44, 0x55, 0x66], [0xdd, 0xee, 0xff])?;
        app.errors.clear();
        app.draw(event_loop, host)?;
        app.action(event_loop, host, Action::Themes, false);
        app.draw(event_loop, host)?;
        app.reload_config(Some(host));
        if app.theme_preview.is_some() || host.palette.open {
            return Err("configuration reload left the theme preview active".into());
        }
        colors(app, [0x11, 0x22, 0x33], [0xaa, 0xbb, 0xcc])?;
        // Reuse the inactive tab's live panes to test an unrelated window.
        let index = app.index(host.id).ok_or("missing theme smoke window")?;
        let mut other = app.workspace.windows[index].clone();
        if other.tabs.len() < 2 {
            return Err("theme smoke requires two live tabs".into());
        }
        other.id = app.workspace.id();
        other.tabs = app.workspace.windows[index].tabs.split_off(1);
        other.active_tab = 0;
        app.workspace.windows[index].active_tab = 0;
        app.workspace.windows.push(other);
        let mut attachment =
            tmuxx_host::Attachment::new(tmuxx_host::Source::Ssh("windows".into()), None, false);
        attachment.mapping.sessions.insert(host.id, 1);
        attachment
            .mapping
            .session_names
            .insert(host.id, "work".into());
        app.tmuxx.push(attachment);
        let scoped_window = host.id;
        let scoped_colors = |app: &App, background, other_background| -> Result<()> {
            for (&id, pane) in &app.panes {
                let expected = if app.pane_window(id) == Some(scoped_window) {
                    background
                } else {
                    other_background
                };
                if pane.session.terminal()?.background != expected {
                    return Err("connection theme leaked across windows or missed a pane".into());
                }
            }
            Ok(())
        };
        let host_config = format!("{}tmuxx-theme-host = windows = Z Scoped\n", files[0].1,);
        fs::write(&files[0].0, &host_config)?;
        app.reload_config(Some(host));
        scoped_colors(app, [0xee; 3], [0x11, 0x22, 0x33])?;
        if app.window_config(host.id).font_size != 13.0
            || app.window_config(host.id).command != app.config().command
            || app.window_color_scheme(host.id) != vt::query::ColorScheme::Light
        {
            return Err("connection theme lost explicit settings, behavior, or light mode".into());
        }
        let global_ui = app.context.options(|options| options.theme_preference);
        app.errors.push("Light connection theme".into());
        app.draw(event_loop, host)?;
        std::thread::sleep(Duration::from_millis(200));
        capture_window(
            app,
            event_loop,
            host,
            &directory.parent().unwrap().join("tmuxx-theme-light.png"),
        )?;
        if app.context.options(|options| options.theme_preference) != global_ui {
            return Err("connection theme leaked into the shared UI theme".into());
        }
        app.errors.clear();
        for (saved_config, background, hint) in [
            (
                files[0].1.to_owned(),
                [0x11, 0x22, 0x33],
                "tmuxx-theme-session = work = ",
            ),
            (
                host_config.clone(),
                [0xee; 3],
                "tmuxx-theme-host = windows = ",
            ),
            (
                format!("{host_config}tmuxx-theme-session = work = Bundled\n"),
                [0x77, 0x88, 0x99],
                "tmuxx-theme-session = work = ",
            ),
        ] {
            fs::write(&files[0].0, &saved_config)?;
            app.reload_config(Some(host));
            scoped_colors(app, background, [0x11, 0x22, 0x33])?;
            // Unthemed remote panes can retain defaults different from local config.
            let mut remote_appearance = app.config().clone();
            remote_appearance.background = config::Rgb::new(0x21, 0x43, 0x65);
            for (&id, pane) in &app.panes {
                if app.pane_window(id) != Some(scoped_window) {
                    pane.session.apply_appearance(&remote_appearance)?;
                }
            }
            let saved_scoped = app.config().clone();
            app.action(event_loop, host, Action::Themes, false);
            app.draw(event_loop, host)?;
            app.draw(event_loop, host)?;
            scoped_colors(app, [0x44, 0x55, 0x66], [0x21, 0x43, 0x65])?;
            key(host, egui::Key::Escape);
            app.draw(event_loop, host)?;
            scoped_colors(app, background, [0x21, 0x43, 0x65])?;
            if app.config() != &saved_scoped {
                return Err("cancelling a scoped theme did not restore the original rules".into());
            }
            app.action(event_loop, host, Action::Themes, false);
            app.draw(event_loop, host)?;
            key(host, egui::Key::Enter);
            app.draw(event_loop, host)?;
            scoped_colors(app, [0x44, 0x55, 0x66], [0x21, 0x43, 0x65])?;
            if !app.errors.iter().any(|error| {
                error.contains(hint)
                    && error.contains(&app.loaded.edit_config_path.display().to_string())
            }) || fs::read_to_string(&files[0].0)? != saved_config
            {
                return Err(
                    "accepting a scoped theme omitted its hint or wrote configuration".into(),
                );
            }
            app.errors.clear();
        }
        fs::write(&files[0].0, &host_config)?;
        app.reload_config(Some(host));
        scoped_colors(app, [0xee; 3], [0x11, 0x22, 0x33])?;
        fs::write(
            &files[0].0,
            format!("{}tmuxx-theme-host = windows = Missing\n", files[0].1),
        )?;
        app.reload_config(Some(host));
        if !app.errors.iter().any(|error| error.contains("Missing")) {
            return Err("missing connection theme did not report a diagnostic".into());
        }
        colors(app, [0x11, 0x22, 0x33], [0xaa, 0xbb, 0xcc])?;
        fs::write(&files[0].0, files[0].1)?;
        app.reload_config(Some(host));
        colors(app, [0x11, 0x22, 0x33], [0xaa, 0xbb, 0xcc])?;
        for (path, source) in &files {
            if fs::read_to_string(path)? != *source {
                return Err("theme selection changed a configuration or theme file".into());
            }
        }
        Ok(())
    })();
    app.cancel_theme_preview(Some(host));
    app.config_loader = original_loader;
    app.config_args = original_args;
    app.workspace = original_workspace;
    app.tmuxx = original_tmuxx;
    app.tmuxx_themes = original_tmuxx_themes;
    app.apply_theme_config(
        original_loaded.config.clone(),
        Some(host),
        &theme_preview::Scope::Global,
    );
    app.loaded = original_loaded;
    if let Some(platform) = &mut app.platform {
        platform.update_config(&app.loaded.config)?;
    }
    app.errors = original_errors;
    host.palette = original_palette;
    host.focused = original_focus;
    host.focus_text_input = original_text_focus;
    host.messages_open = !app.errors.is_empty();
    host.egui.egui_input_mut().focused = original_focus;
    host.egui
        .egui_input_mut()
        .events
        .push(egui::Event::WindowFocused(original_focus));
    result?;
    eprintln!(
        "Native smoke: global and connection themes previewed, cancelled, accepted with scoped hints, and reloaded with window isolation and saved-setting preservation"
    );
    Ok(())
}

pub(super) fn check_tmuxx_management(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    directory: &Path,
) -> Result<()> {
    use rustty::tmuxx::Connection;

    let endpoint =
        tmuxx_host::temporary_endpoint(&format!("rustty-management-{}", std::process::id()))?;
    let configuration = directory.join("tmuxx-management.conf");
    fs::write(
        &configuration,
        if cfg!(windows) {
            "set-option -g default-shell cmd.exe\nset-option -g default-command 'cmd.exe /D /Q'\n"
        } else {
            "set-option -g default-shell /bin/sh\nset-option -g default-command '/bin/sleep 60'\n"
        },
    )?;
    let executable = std::env::current_exe()?;
    #[cfg(windows)]
    let executable = executable.with_file_name("tmuxx.exe");
    let prefix: &[&str] = if cfg!(windows) { &[] } else { &["tmux"] };
    struct Daemon(std::process::Child);
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut daemon = Daemon(
        tmuxx_cli::native_daemon_command(&executable, &endpoint, prefix)
            .arg("-f")
            .arg(configuration)
            .spawn()?,
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let connection = loop {
        if let Ok(connection) = Connection::native(&endpoint, Arc::new(|| {})) {
            break connection;
        }
        if daemon.0.try_wait()?.is_some() || Instant::now() >= deadline {
            return Err("management smoke daemon did not start".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let initial = if cfg!(windows) {
        vec![b"cmd.exe".to_vec(), b"/D".to_vec(), b"/Q".to_vec()]
    } else {
        vec![
            b"/bin/sh".to_vec(),
            b"-c".to_vec(),
            br"printf '\033]7;file://remote.invalid/rustty-cwd-before-attach\007'; exec /bin/sleep 60".to_vec(),
        ]
    };
    let mut command = vec![b"new-session".to_vec(), b"-d".to_vec()];
    command.extend(initial);
    connection.command_bytes(command.clone())?;
    connection.command_bytes(command)?;
    // This second real connection uses Embedded only to check temporary-menu labels.
    let mut owner = tmuxx_host::Attachment::new(
        tmuxx_host::Source::Embedded(endpoint.clone()),
        Some(connection),
        false,
    );
    let connection = Connection::native(&endpoint, Arc::new(|| {}))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let graph = loop {
        if let Some(graph) = connection.graph()
            && graph.sessions.len() == 2
        {
            break graph;
        }
        if connection.is_closed() || Instant::now() >= deadline {
            return Err(format!(
                "management smoke could not connect: {:?}",
                connection.errors()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if !cfg!(windows) {
        // Wait for the remote report before attaching the app to an idle pane.
        for pane in &graph.panes {
            let view = Session::attach_tmuxx(&connection, pane.id, Arc::new(|| {}))?;
            let deadline = Instant::now() + Duration::from_secs(10);
            while view.terminal()?.working_directory.is_empty() {
                if Instant::now() >= deadline {
                    return Err("management smoke did not receive the remote OSC 7".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    owner.instance = connection.instance();
    let mut entry =
        tmuxx_host::Attachment::new(tmuxx_host::Source::Local(endpoint), Some(connection), false);
    entry.instance = entry.connection.as_ref().unwrap().instance();
    entry.selected.insert(graph.sessions[0].id);
    let target = entry.target();
    let original = std::mem::replace(&mut app.tmuxx, vec![entry]);
    let workspace = app.workspace.clone();
    let active = app.active;
    let errors = std::mem::take(&mut app.errors);
    let background_smoke = std::mem::replace(&mut app.background_smoke, true);
    let result = (|| -> Result<()> {
        let sessions = [graph.sessions[0].id, graph.sessions[1].id];
        let windows = graph
            .sessions
            .iter()
            .map(|s| *s.windows.values().next().unwrap())
            .collect::<Vec<_>>();
        let wait_sizes = |fitted: (u16, u16)| -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if owner
                    .connection
                    .as_ref()
                    .unwrap()
                    .graph()
                    .is_some_and(|graph| {
                        windows.iter().zip([(90, 30), fitted]).all(|(id, size)| {
                            graph.windows.iter().any(|window| {
                                window.id == *id && (window.cols, window.rows) == size
                            })
                        })
                    })
                {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "Fit did not preserve the other session and reach {fitted:?}"
                    )
                    .into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let fit_command = |entry: &tmuxx_host::Attachment| {
            entry
                .menu_entry(None)
                .children
                .into_iter()
                .find(|item| item.label == graph.sessions[1].name)
                .and_then(|session| {
                    session
                        .children
                        .into_iter()
                        .find(|item| item.label == "Fit to This Window")
                })
                .and_then(|item| item.command)
        };
        if fit_command(&app.tmuxx[0]).is_some() {
            return Err("Fit was offered for a session without a local window".into());
        }
        // Synthetic views exercise routing without creating or focusing native windows.
        app.tmuxx[0].mapping.sessions =
            [(u64::MAX - 1, sessions[0]), (u64::MAX, sessions[1])].into();
        app.tmuxx[0].selected.extend(sessions);
        app.active = Some(u64::MAX - 1);
        let fit = fit_command(&app.tmuxx[0]).ok_or("Open native session omitted Fit")?;
        if fit != tmuxx_host::MenuCommand::Fit(target.clone(), sessions[1]) {
            return Err("Fit captured the wrong session or server".into());
        }
        app.tmuxx[0].source = tmuxx_host::Source::SystemSsh("unused".into());
        let stock_fit = fit_command(&app.tmuxx[0]);
        app.tmuxx[0].source = target.source.clone();
        if stock_fit.is_some() {
            return Err("Stock tmux offered native Fit".into());
        }
        for (session, cols, rows) in [(sessions[0], 90, 30), (sessions[1], 60, 20)] {
            app.tmuxx[0]
                .connection
                .as_ref()
                .unwrap()
                .view_size(session, cols, rows, 8, 16)?;
        }
        wait_sizes((60, 20))?;
        for _ in 0..2 {
            owner.connection.as_ref().unwrap().command([
                "resize-window".into(),
                "-t".into(),
                format!("@{}", windows[1]),
                "-x".into(),
                "100".into(),
                "-y".into(),
                "40".into(),
            ])?;
            wait_sizes((100, 40))?;
            let mut stale = target.clone();
            stale.instance.push_str("-replaced");
            app.tmuxx_menu_command(tmuxx_host::MenuCommand::Fit(stale, sessions[1]));
            if app.errors.len() != 1 || !app.errors[0].contains("connection changed") {
                return Err(format!("Stale Fit target was not rejected: {:?}", app.errors).into());
            }
            app.errors.clear();
            // Repeating Fit must work even though this view's cached dimensions are unchanged.
            app.native_tmuxx_command(event_loop, fit.clone());
            wait_sizes((60, 20))?;
        }
        app.tmuxx[0].mapping.sessions.clear();
        for session in sessions {
            app.tmuxx[0]
                .connection
                .as_ref()
                .unwrap()
                .release_view(session)?;
        }
        app.active = active;
        check_tmuxx_secondary_selection(
            app,
            event_loop,
            owner.connection.as_ref().unwrap(),
            sessions[0],
            directory,
        )?;
        check_tmuxx_disconnection(app, event_loop, directory)?;
        if !cfg!(windows) {
            for remote in graph.panes.iter().map(|pane| pane.id) {
                let view = app.tmuxx[0]
                    .mapping
                    .panes
                    .iter()
                    .find(|(_, pane)| **pane == remote)
                    .map(|(view, _)| view)
                    .ok_or("OSC 7 pane was not attached")?;
                if app.panes[view].cwd != Path::new("/rustty-cwd-before-attach") {
                    return Err("attached pane label did not use the remote snapshot CWD".into());
                }
            }
        }
        let graph = app.tmuxx[0]
            .connection
            .as_ref()
            .and_then(Connection::graph)
            .ok_or("secondary-selection smoke lost its server snapshot")?;
        // Closing disconnected views must not offer termination or start a monitor/retry.
        drop(app.tmuxx[0].connection.take());
        app.tmuxx[0].retry_at = Some(Instant::now());
        app.refresh_tmuxx(event_loop);
        let disconnected_windows = app.tmuxx[0].mapping.sessions.clone();
        if disconnected_windows.len() != 2 {
            return Err("disconnected-close smoke needs both session windows".into());
        }
        for (window, session) in disconnected_windows {
            let key = app
                .windows
                .iter()
                .find_map(|(key, host)| (host.id == window).then_some(*key))
                .ok_or("disconnected-close smoke lost its native window")?;
            app.window_event(event_loop, key, WindowEvent::CloseRequested);
            if app.windows.contains_key(&key)
                || app.workspace.windows.iter().any(|state| state.id == window)
                || app.tmuxx[0].selected.contains(&session)
                || app.tmuxx[0].connection.is_some()
                || !app.errors.is_empty()
            {
                return Err(format!(
                    "disconnected close retained a window/prompt or opened a connection: {:?}",
                    app.errors
                )
                .into());
            }
            if !app.tmuxx[0].selected.is_empty() {
                // The last view can be closed while an automatic replacement is in flight.
                app.retry_tmuxx(Instant::now());
                if app.tmuxx[0].connection.is_none() || app.tmuxx[0].view_connection().is_some() {
                    return Err(
                        "disconnected-close smoke did not start a pending replacement".into(),
                    );
                }
            }
        }
        app.retry_tmuxx(Instant::now() + Duration::from_secs(3));
        app.refresh_tmuxx(event_loop);
        if app.tmuxx[0].connection.is_some()
            || app.tmuxx[0].deadline().is_some()
            || app.tmuxx[0].waiting_for_window()
            || !app.tmuxx[0].selected.is_empty()
            || !app.saved_workspace().tmuxx.is_empty()
        {
            return Err("closing the last disconnected window retained automatic reconnect".into());
        }
        app.tmuxx_menu_command(tmuxx_host::MenuCommand::Reconnect(target.clone()));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.refresh_tmuxx(event_loop);
            if let Some(reconnected) = app.tmuxx[0].connection.as_ref().and_then(Connection::graph)
            {
                if !app.tmuxx[0].selected.is_empty()
                    || app.workspace.windows.len() != workspace.windows.len()
                    || reconnected.sessions.len() != graph.sessions.len()
                    || !reconnected
                        .panes
                        .iter()
                        .map(|pane| pane.id)
                        .eq(graph.panes.iter().map(|pane| pane.id))
                {
                    return Err("disconnected close killed a session or reopened a window".into());
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err("disconnected-close smoke could not manually reconnect".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        app.tmuxx_menu_command(tmuxx_host::MenuCommand::Disconnect(target.clone()));
        let deadline = Instant::now() + Duration::from_secs(10);
        let connections = loop {
            let entry = app
                .tmuxx
                .first()
                .ok_or("Disconnect removed the local server menu")?;
            if !entry.monitoring || !entry.selected.is_empty() || entry.retry_at.is_some() {
                return Err(
                    "Disconnect retained an active selection or automatic reconnect".into(),
                );
            }
            if let Some(connection) = &entry.connection {
                if connection.graph().is_some() {
                    return Err("Disconnected server monitor retained terminal snapshots".into());
                }
                if let Some(status) = connection.server_status()
                    && status.instance_id == target.instance
                    && status.connections == 1
                    && owner
                        .connection
                        .as_ref()
                        .unwrap()
                        .server_status()
                        .is_some_and(|status| status.connections == 1)
                {
                    for attachment in [entry, &owner] {
                        if !attachment
                            .menu_entry(None)
                            .children
                            .iter()
                            .any(|item| item.label == "Connections: 1")
                        {
                            return Err(
                                "local or temporary server menu omitted its connection count"
                                    .into(),
                            );
                        }
                    }
                    if !entry.menu_entry(None).children.iter().any(|item| {
                        item.label == "Stop Idle Server (connections remain)"
                            && item.command.is_none()
                    }) {
                        return Err("busy server monitor offered idle shutdown".into());
                    }
                    break status.connections;
                }
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "management smoke did not receive the owner client count: {:?}",
                    app.errors
                )
                .into());
            }
            app.refresh_tmuxx(event_loop);
            std::thread::sleep(Duration::from_millis(5));
        };
        app.reconcile(event_loop);
        if !app.tmuxx[0].mapping.sessions.is_empty()
            || app.workspace.windows.len() != workspace.windows.len()
            || app
                .windows
                .values()
                .any(|host| !workspace.windows.iter().any(|window| window.id == host.id))
        {
            return Err("Disconnect retained the selection fixture's native views".into());
        }
        app.tmuxx_menu_command(tmuxx_host::MenuCommand::Reconnect(target.clone()));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let entry = app
                .tmuxx
                .first()
                .ok_or("Reconnect removed the local server")?;
            if let Some(connection) = &entry.connection
                && let Some(reconnected) = connection.graph()
            {
                if entry.monitoring
                    || !entry.selected.is_empty()
                    || connection.instance() != target.instance
                    || !reconnected
                        .sessions
                        .iter()
                        .map(|session| (&session.name, session.id))
                        .eq(graph
                            .sessions
                            .iter()
                            .map(|session| (&session.name, session.id)))
                    || !reconnected
                        .panes
                        .iter()
                        .map(|pane| pane.id)
                        .eq(graph.panes.iter().map(|pane| pane.id))
                {
                    return Err(
                        "Reconnect changed the owner, sessions, panes, or opened windows".into(),
                    );
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err(
                    format!("management smoke reconnect timed out: {:?}", app.errors).into(),
                );
            }
            app.refresh_tmuxx(event_loop);
            std::thread::sleep(Duration::from_millis(5));
        }
        if owner.connection.as_ref().unwrap().is_closed() || !app.errors.is_empty() {
            return Err(format!(
                "management smoke stopped the owner or reported errors: {:?}",
                app.errors
            )
            .into());
        }
        owner.connection = None;
        app.tmuxx_menu_command(tmuxx_host::MenuCommand::Disconnect(target.clone()));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let entry = &app.tmuxx[0];
            if entry
                .connection
                .as_ref()
                .and_then(Connection::server_status)
                .is_some_and(|status| status.connections == 0)
            {
                if !entry.menu_entry(None).children.iter().any(|item| {
                    item.command == Some(tmuxx_host::MenuCommand::ShutdownIdle(target.clone()))
                }) {
                    return Err("idle server monitor omitted its stop command".into());
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err("management smoke server did not become idle".into());
            }
            app.refresh_tmuxx(event_loop);
            std::thread::sleep(Duration::from_millis(5));
        }
        app.tmuxx_menu_command(tmuxx_host::MenuCommand::ShutdownIdle(target.clone()));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.refresh_tmuxx(event_loop);
            if app.tmuxx[0].status.as_deref() == Some("Server stopped")
                && let Some(status) = daemon.0.try_wait()?
            {
                if !status.success() {
                    return Err(format!(
                        "management smoke daemon failed during idle stop: {status}"
                    )
                    .into());
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "management smoke idle shutdown failed: {:?}; {:?}",
                    app.tmuxx[0].status, app.errors
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        app.refresh_tmuxx(event_loop);
        if app.tmuxx[0].status.as_deref() != Some("Server stopped")
            || app.tmuxx[0].deadline().is_some()
            || !app.errors.is_empty()
        {
            return Err(format!(
                "successful idle shutdown lost its status or reported errors: {:?}",
                app.errors
            )
            .into());
        }
        fs::write(
            directory.join("tmuxx-management.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "instance": target.instance,
                "connections_after_disconnect": connections,
                "sessions": graph.sessions.len(),
                "panes": graph.panes.len(),
                "idle_stop": true,
                "fit_cached_size_twice": true,
                "fit_preserved_other_session": true,
                "fit_rejected_stale_target": true,
                "local_focus_sent_select_pane": true,
                "server_selection_kept_local_focus": true,
                "secondary_selection_rendered": true,
                "secondary_selection_suppressed_when_focused": true,
                "local_split_auto_focused": true,
                "local_floating_pane_auto_focused": true,
                "remote_split_preserved_local_focus": true,
                "disconnected_views_retain_content_without_sending_input": true,
                "reconnect_discards_stale_input": true,
                "disconnected_close_preserves_sessions_without_reconnecting": true,
            }))?,
        )?;
        Ok(())
    })();
    app.workspace = workspace;
    app.tmuxx = original;
    app.active = active;
    app.reconcile(event_loop);
    app.background_smoke = background_smoke;
    app.errors = errors;
    app.sync_tmuxx_menu();
    result?;
    eprintln!(
        "Native smoke: server menus retained live counts without snapshots; reconnect preserved sessions and idle stop reported a clean shutdown"
    );
    Ok(())
}

fn check_tmuxx_disconnection(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    directory: &Path,
) -> Result<()> {
    let target = app.tmuxx[0].target();
    let window = *app.tmuxx[0]
        .mapping
        .sessions
        .keys()
        .next()
        .ok_or("no tmuxx window")?;
    let key = app
        .windows
        .iter()
        .find(|(_, host)| host.id == window)
        .map(|(key, _)| *key)
        .ok_or("no tmuxx native host")?;
    let mut host = app.windows.remove(&key).unwrap();
    // The selection fixture leaves a small floating pane focused in its first tab.
    app.action(event_loop, &mut host, Action::GotoTab(2), true);
    let id = app.focused(window).ok_or("no focused tmuxx pane")?;
    let draw = app.draw(event_loop, &mut host);
    app.windows.insert(key, host);
    draw?;
    let contents = {
        let pane = app.panes.get_mut(&id).unwrap();
        pane.input
            .push_back(SessionInput::Text(b"stale input".to_vec()));
        pane.input_bytes = b"stale input".len();
        pane.mouse_cell = Some([1, 1]);
        let mut terminal = pane.session.terminal()?;
        let rows = usize::from(terminal.rows) + 8;
        terminal.feed("retained content\r\n".repeat(rows).as_bytes());
        terminal.feed(b"\x1b[?1003h\x1b[?1006h");
        terminal.plain_text()
    };
    let host = app.windows.get_mut(&key).unwrap();
    host.mouse_button = Some(vt::MouseButton::Left);
    host.composing = true;
    host.preedit = "unfinished".into();
    host.preedit_selection = Some((0, 1));
    // Drop only the client transport; the disposable server and shells stay alive.
    drop(app.tmuxx[0].connection.take());
    app.refresh_tmuxx(event_loop);
    let host = &app.windows[&key];
    let pane = &app.panes[&id];
    if pane.session.accepts_input()
        || pane.exited
        || !pane.input.is_empty()
        || pane.input_bytes != 0
        || pane.mouse_cell.is_some()
        || host.mouse_button.is_some()
        || host.composing
        || !host.preedit.is_empty()
        || host.preedit_selection.is_some()
        || pane.session.terminal()?.plain_text() != contents
        || !app.tmuxx[0]
            .window_tag(window, &BTreeMap::new())
            .0
            .contains("disconnected")
    {
        return Err("disconnect did not retain an inactive view and clear pending input".into());
    }
    let error_count = app.errors.len();
    let mut host = app.windows.remove(&key).unwrap();
    let pointer_in_window = host.egui.is_pointer_in_window();
    let result = (|| -> Result<()> {
        host.mouse = host.rects[&id].left_bottom() + Vec2::new(24.0, -32.0);
        host.modifiers = Modifiers::default();
        let _ = host.egui.on_window_event(
            &host.window,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: LogicalPosition::new(host.mouse.x, host.mouse.y)
                    .to_physical(host.window.scale_factor()),
            },
        );
        for _ in 0..4 {
            app.mouse(&mut host, vt::MouseAction::Move, None);
            app.scroll(&mut host, MouseScrollDelta::LineDelta(0.0, 1.0));
        }
        let offset = app.panes[&id].session.terminal()?.screen().viewport_offset;
        if offset == 0 {
            let terminal = app.panes[&id].session.terminal()?;
            return Err(format!(
                "disconnected application mouse mode blocked local scrolling: pane={id}, hovered={:?}, pointer={:?}, rect={:?}, modal={}, peek={}, divider={}, UI={}, history={}, alternate={}",
                host.hovered_pane(), host.mouse, host.rects[&id], host.modal_input(),
                host.peek.is_some(), host.divider_drag.is_some(), host.ui_areas.contains(host.mouse),
                terminal.screen().history_len(), terminal.is_alternate_screen(),
            ).into());
        }
        app.mouse(
            &mut host,
            vt::MouseAction::Press,
            Some(vt::MouseButton::Left),
        );
        if host.selection_drag.is_none() {
            return Err("disconnected application mouse mode blocked local selection".into());
        }
        app.mouse(
            &mut host,
            vt::MouseAction::Release,
            Some(vt::MouseButton::Left),
        );
        {
            let pane = app.panes.get_mut(&id).unwrap();
            for input in [
                SessionInput::Text(b"text".to_vec()),
                SessionInput::Mouse(b"mouse".to_vec()),
                SessionInput::Paste(b"paste".to_vec()),
                SessionInput::Reply(b"reply".to_vec()),
                SessionInput::Key {
                    event: vt::KeyEvent::new(vt::Key::Char('x')),
                    options: Default::default(),
                },
            ] {
                pane.input.push_back(SessionInput::Text(b"stale".to_vec()));
                pane.input_bytes = 5;
                pane.send_input(input)?;
                if !pane.input.is_empty() || pane.input_bytes != 0 {
                    return Err("disconnected input retained queued bytes".into());
                }
            }
            pane.input.push_back(SessionInput::Text(b"stale".to_vec()));
            pane.input_bytes = 5;
            pane.flush()?;
            let mut terminal = pane.session.terminal()?;
            let row = terminal
                .screen()
                .viewport()
                .next()
                .ok_or("no retained row")?
                .id;
            terminal.screen_mut().selection = Some(vt::Selection {
                start: vt::GridPoint { row, col: 0 },
                end: vt::GridPoint { row, col: 7 },
                rectangular: false,
            });
            if terminal.screen().selection_text().as_deref() != Some("retained") {
                return Err("disconnected content could not be copied".into());
            }
            let mut search = Search::default();
            search.query = "retained".into();
            search.refresh(&mut terminal);
            if search.highlights(&mut terminal).is_empty() {
                return Err("disconnected content could not be searched".into());
            }
        }
        app.action(event_loop, &mut host, Action::Text(b"text".to_vec()), true);
        app.paste(
            &mut host,
            PendingPaste {
                pane: id,
                data: b"paste".to_vec(),
            },
            true,
        );
        capture_window(
            app,
            event_loop,
            &mut host,
            &directory.join("tmuxx-disconnected.png"),
        )?;
        if host
            .prepared
            .get(&id)
            .is_none_or(|pane| pane.key.options.cursor_visible)
        {
            return Err("disconnected view retained an active terminal cursor".into());
        }
        Ok(())
    })();
    if !pointer_in_window {
        let _ = host.egui.on_window_event(
            &host.window,
            &WindowEvent::CursorLeft {
                device_id: winit::event::DeviceId::dummy(),
            },
        );
    }
    app.windows.insert(key, host);
    result?;
    for event in [
        Ime::Preedit("unfinished".into(), Some((0, 1))),
        Ime::Commit("input".into()),
    ] {
        app.window_event(event_loop, key, WindowEvent::Ime(event));
    }
    if app.errors.len() != error_count
        || !app.panes[&id].input.is_empty()
        || app.panes[&id].input_bytes != 0
        || app.windows[&key].composing
        || !app.windows[&key].preedit.is_empty()
    {
        return Err(format!(
            "disconnected input produced errors or retained input: {:?}",
            app.errors
        )
        .into());
    }
    app.tmuxx_menu_command(tmuxx_host::MenuCommand::Reconnect(target));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.refresh_tmuxx(event_loop);
        if app
            .panes
            .get(&id)
            .is_some_and(|pane| pane.session.accepts_input())
        {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!("disconnected view did not reconnect: {:?}", app.errors).into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if app.errors.len() != error_count
        || !app.panes[&id].input.is_empty()
        || app.panes[&id].input_bytes != 0
        || app.tmuxx[0]
            .window_tag(window, &BTreeMap::new())
            .0
            .contains("disconnected")
    {
        return Err("reconnect retained stale input or disconnected presentation".into());
    }
    app.write(id, b"fresh-after-reconnect".to_vec());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = app.panes[&id].session.terminal()?.plain_text();
        if text.contains("fresh-after-reconnect") {
            if text.contains("stale") {
                return Err("reconnect replayed discarded input".into());
            }
            break;
        }
        if Instant::now() >= deadline {
            return Err("reconnected pane did not receive fresh input".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn check_tmuxx_secondary_selection(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    owner: &rustty::tmuxx::Connection,
    session: u64,
    directory: &Path,
) -> Result<()> {
    fn wait(
        app: &mut App,
        event_loop: &ActiveEventLoop,
        label: &str,
        ready: impl Fn(&App) -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.refresh_tmuxx(event_loop);
            if !app.errors.is_empty() {
                return Err(format!("{label}: {:?}", app.errors).into());
            }
            if ready(app) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!("secondary-selection smoke timed out: {label}").into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let graph = owner.graph().ok_or("selection owner has no snapshot")?;
    let server_session = graph.sessions.iter().find(|s| s.id == session).unwrap();
    let index = u32::try_from(server_session.active)?;
    let server_window = server_session.windows[&index];
    let first = graph
        .windows
        .iter()
        .find(|window| window.id == server_window)
        .unwrap()
        .active;
    let split = || -> Result<u64> {
        let mut argv = [
            "split-window".into(),
            "-h".into(),
            "-P".into(),
            "-F".into(),
            "#{pane_id}".into(),
            "-t".into(),
            format!("%{first}"),
        ]
        .into_iter()
        .map(String::into_bytes)
        .collect::<Vec<_>>();
        let command: &[&str] = if cfg!(windows) {
            &["cmd.exe", "/D", "/Q"]
        } else {
            &["/bin/sleep", "60"]
        };
        argv.extend(command.iter().map(|arg| arg.as_bytes().to_vec()));
        let output = owner
            .command_with_reply(argv)?
            .recv_timeout(Duration::from_secs(10))??;
        Ok(std::str::from_utf8(&output)?
            .trim()
            .strip_prefix('%')
            .ok_or("split command omitted its pane ID")?
            .parse()?)
    };
    let second = split()?;
    wait(app, event_loop, "opening native session panes", |app| {
        app.tmuxx[0]
            .mapping
            .panes
            .values()
            .any(|pane| *pane == second)
    })?;
    let mapping = &app.tmuxx[0].mapping;
    let window = *mapping
        .sessions
        .iter()
        .find(|(_, sid)| **sid == session)
        .ok_or("selection session did not open a window")?
        .0;
    let local = |app: &App, remote| {
        app.tab(window)
            .unwrap()
            .panes
            .keys()
            .find(|pane| app.tmuxx[0].mapping.panes.get(pane) == Some(&remote))
            .copied()
            .unwrap()
    };
    let (first_view, second_view) = (local(app, first), local(app, second));
    let key = app
        .windows
        .iter()
        .find(|(_, host)| host.id == window)
        .map(|(key, _)| *key)
        .ok_or("selection session has no native host")?;
    if app.windows[&key].window.is_visible().unwrap_or(false)
        || app.windows[&key].window.has_focus()
    {
        return Err("selection fixture opened a visible or focused native window".into());
    }
    app.active = Some(window);
    wait(app, event_loop, "observing initial server focus", |_| {
        owner.graph().is_some_and(|graph| {
            graph
                .windows
                .iter()
                .any(|window| window.id == server_window && window.active == second)
        })
    })?;
    app.focus_pane(window, first_view);
    wait(app, event_loop, "local focus sent select-pane", |_| {
        owner.graph().is_some_and(|graph| {
            graph
                .windows
                .iter()
                .any(|window| window.id == server_window && window.active == first)
        })
    })?;
    owner.focus_pane(session, index, second)?;
    wait(
        app,
        event_loop,
        "receiving secondary server selection",
        |app| app.tmuxx[0].mapping.server_focused.contains(&second_view),
    )?;
    if app.focused(window) != Some(first_view) {
        return Err("incoming server selection stole the local keyboard focus".into());
    }

    let marker = egui::Id::new(("server-selection", second_view));
    let mut host = app.windows.remove(&key).unwrap();
    let capture = capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("tmuxx-secondary-selection.png"),
    );
    let visible = app.context.viewport_for(host.viewport, |viewport| {
        viewport.prev_pass.widgets.get(marker).is_some()
    });
    app.windows.insert(key, host);
    capture?;
    if !visible {
        return Err("secondary server selection was absent from the rendered frame".into());
    }
    app.focus_pane(window, second_view);
    let mut host = app.windows.remove(&key).unwrap();
    let draw = app.draw(event_loop, &mut host);
    let visible = app.context.viewport_for(host.viewport, |viewport| {
        viewport.prev_pass.widgets.get(marker).is_some()
    });
    app.windows.insert(key, host);
    draw?;
    if app.focused(window) != Some(second_view) || visible {
        return Err("locally focusing the selected pane did not suppress its marker".into());
    }

    for action in [Action::NewSplit(Direction::Right), Action::NewFloatingPane] {
        let before = app
            .tab(window)
            .unwrap()
            .panes
            .keys()
            .copied()
            .collect::<HashSet<_>>();
        let mut host = app.windows.remove(&key).unwrap();
        app.action(event_loop, &mut host, action, true);
        app.windows.insert(key, host);
        wait(app, event_loop, "focusing a locally created pane", |app| {
            app.tab(window).is_some_and(|tab| {
                tab.panes.len() == before.len() + 1 && !before.contains(&tab.focused)
            })
        })?;
    }
    let focused = app.focused(window);
    let remote = split()?;
    wait(
        app,
        event_loop,
        "receiving a remotely created pane",
        |app| {
            app.tmuxx[0]
                .mapping
                .server_focused
                .iter()
                .any(|pane| app.tmuxx[0].mapping.panes[pane] == remote)
        },
    )?;
    if app.focused(window) != focused {
        return Err("a remotely created pane stole local keyboard focus".into());
    }

    let mut command = vec![
        "new-window".into(),
        "-d".into(),
        "-t".into(),
        format!("${session}"),
    ];
    command.extend(if cfg!(windows) {
        ["cmd.exe", "/D", "/Q"].map(str::to_owned).to_vec()
    } else {
        ["/bin/sleep", "60"].map(str::to_owned).to_vec()
    });
    owner.command(command)?;
    wait(app, event_loop, "opening a second server tab", |app| {
        app.workspace.windows[app.index(window).unwrap()].tabs.len() == 2
    })?;
    let graph = app.tmuxx[0].connection.as_ref().unwrap().graph().unwrap();
    for (action, expected) in [
        (Action::NextTab, 1),
        (Action::NextTab, 0),
        (Action::GotoTab(2), 1),
        (Action::PreviousTab, 0),
        (Action::LastTab, 1),
        (Action::GotoTab(0), 0),
    ] {
        let mut host = app.windows.remove(&key).unwrap();
        app.action(event_loop, &mut host, action, true);
        app.windows.insert(key, host);
        if app.workspace.windows[app.index(window).unwrap()].active_tab != expected {
            return Err("tmuxx tab action waited for a server snapshot before switching".into());
        }
        let (_, target_index, _) = app.tmuxx[0]
            .mapping
            .focus_target(&app.workspace, window, app.focused(window).unwrap())
            .unwrap();
        wait(app, event_loop, "acknowledging tab navigation", |_| {
            owner.graph().is_some_and(|graph| {
                graph
                    .sessions
                    .iter()
                    .any(|s| s.id == session && s.active == u64::from(target_index))
            })
        })?;
    }
    // A distinct server selection is visible only after this client's pending focus clears.
    let local = app.tmuxx[0].mapping.panes[&app.focused(window).unwrap()];
    let marker = if local == first { second } else { first };
    owner.focus_pane(session, index, marker)?;
    wait(
        app,
        event_loop,
        "consuming the tab selection replies",
        |app| {
            app.tmuxx[0]
                .connection
                .as_ref()
                .unwrap()
                .graph()
                .is_some_and(|graph| {
                    graph
                        .windows
                        .iter()
                        .any(|window| window.id == server_window && window.active == marker)
                })
        },
    )?;
    let current = app.tmuxx[0].connection.as_ref().unwrap().graph().unwrap();
    if graph.panes.len() != current.panes.len()
        || graph
            .panes
            .iter()
            .zip(&current.panes)
            .any(|(before, after)| {
                before.id != after.id
                    || !before
                        .terminal
                        .as_ref()
                        .zip(after.terminal.as_ref())
                        .is_some_and(|(before, after)| Arc::ptr_eq(before, after))
            })
    {
        return Err("tmuxx tab navigation replaced the retained terminal snapshots".into());
    }
    Ok(())
}

struct Replay {
    case: &'static str,
    size: Option<[u16; 2]>,
    input: Vec<u8>,
    frame: usize,
    last_frame: Option<Instant>,
    process_start: Option<(Instant, u64)>,
    samples: Vec<[u64; 5]>,
    target: Option<wgpu::Texture>,
}

impl Replay {
    fn new(name: &str) -> Result<Self> {
        let name = if name == "1" { "mixed_unicode" } else { name };
        let case = workload::CASES
            .into_iter()
            .find(|&case| case == name)
            .ok_or_else(|| format!("unknown replay {name:?}; expected {:?}", workload::CASES))?;
        Ok(Self {
            case,
            size: None,
            input: Vec::new(),
            frame: 0,
            last_frame: None,
            process_start: None,
            samples: Vec::with_capacity(workload::SAMPLES),
            target: None,
        })
    }

    fn step(
        &mut self,
        app: &mut App,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
        directory: &Path,
        offscreen: bool,
    ) -> Result<bool> {
        if host.frames == 0 || !host.visible || host.occluded {
            return Ok(false);
        }
        let pane = app.focused(host.id).ok_or("replay has no pane")?;
        if self.size.is_none() {
            let wanted = winit::dpi::PhysicalSize::new(1200, 850);
            if host.window.inner_size() != wanted {
                let _ = host.window.request_inner_size(wanted);
                return Ok(false);
            }
            let mut terminal = app.panes[&pane].session.terminal()?;
            let size = [terminal.cols, terminal.rows];
            let mut fixture = vt::Terminal::new(size[0], size[1], 4096);
            fixture.set_pixel_size(terminal.width_px, terminal.height_px);
            self.input = workload::setup(&mut fixture, self.case);
            *terminal = fixture;
            self.size = Some(size);
            host.prepared.clear();
            host.composed = None;
            host.focus_hint.dismiss();
            if offscreen {
                let state = app.painter.render_state().ok_or("GPU unavailable")?;
                self.target = Some(offscreen_texture(&state, [1200, 850]));
            }
        }
        if self.frame == workload::WARMUP {
            self.process_start = Some((Instant::now(), cpu_time(CpuClock::Process)?));
        }
        let terminal_start = Instant::now();
        workload::advance(
            &mut *app.panes[&pane].session.terminal()?,
            self.case,
            &self.input,
            self.frame,
            self.size.unwrap(),
        );
        let terminal_ns = terminal_start.elapsed().as_nanos() as u64;
        let cpu_start = cpu_time(CpuClock::Thread)?;
        let frame_start = Instant::now();
        let before = host.frames;
        app.draw_frame(event_loop, host, self.target.as_ref())?;
        let frame_ns = frame_start.elapsed().as_nanos() as u64;
        let frame_cpu = cpu_time(CpuClock::Thread)? - cpu_start;
        if host.frames != before + 1 {
            return Err("replay did not draw a frame".into());
        }
        let interval = self
            .last_frame
            .replace(frame_start)
            .map_or(0, |last| frame_start.duration_since(last).as_nanos() as u64);
        if self.frame >= workload::WARMUP {
            self.samples.push([
                terminal_ns,
                frame_ns,
                frame_cpu,
                interval,
                resident_bytes()?,
            ]);
        }
        self.frame += 1;
        if self.samples.len() != workload::SAMPLES {
            return Ok(false);
        }
        let (started, cpu_started) = self.process_start.unwrap();
        let elapsed = started.elapsed().as_secs_f64();
        let process_cpu = (cpu_time(CpuClock::Process)? - cpu_started) as f64 / 1e9;
        let report = serde_json::json!({
            "case": self.case, "font": app.loaded.config.font_family[0], "font_size_points": 13,
            "renderer": app.painter.description(),
            "size_pixels": [host.window.inner_size().width, host.window.inner_size().height],
            "scale_factor": host.window.scale_factor(), "terminal_size": self.size.unwrap(),
            "warmup_frames": workload::WARMUP, "sample_frames": workload::SAMPLES,
            "minimum_replay_pause_ms": 50, "offscreen": offscreen,
            "offscreen_gpu_submission": self.target.is_some(),
            "gpu_completion_measured": false, "visible_presentation_measured": false,
            "columns": ["terminal_ns", "frame_wall_ns", "frame_thread_cpu_ns", "frame_interval_ns", "rss_bytes"],
            "statistics": {
                "terminal_ns": workload::stats(self.samples.iter().map(|s| s[0])),
                "frame_wall_ns": workload::stats(self.samples.iter().map(|s| s[1])),
                "frame_thread_cpu_ns": workload::stats(self.samples.iter().map(|s| s[2])),
                "frame_interval_ns": workload::stats(self.samples.iter().map(|s| s[3])),
                "rss_bytes": workload::stats(self.samples.iter().map(|s| s[4])),
            },
            "process_cpu_seconds": process_cpu, "elapsed_seconds": elapsed,
            "process_cpu_percent_of_one_core": 100.0 * process_cpu / elapsed,
            "samples": self.samples,
        });
        fs::write(
            directory.join("timing.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        Ok(true)
    }
}

enum CpuClock {
    Process,
    Thread,
}

#[cfg(target_os = "macos")]
fn cpu_time(clock: CpuClock) -> Result<u64> {
    let clock = match clock {
        CpuClock::Process => libc::CLOCK_PROCESS_CPUTIME_ID,
        CpuClock::Thread => libc::CLOCK_THREAD_CPUTIME_ID,
    };
    let mut time: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(clock, &mut time) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64)
}

#[cfg(target_os = "windows")]
fn cpu_time(clock: CpuClock) -> Result<u64> {
    use windows::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentProcess, GetCurrentThread, GetProcessTimes, GetThreadTimes},
    };
    let [mut created, mut exited, mut kernel, mut user] = [FILETIME::default(); 4];
    unsafe {
        match clock {
            CpuClock::Process => GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            ),
            CpuClock::Thread => GetThreadTimes(
                GetCurrentThread(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            ),
        }
    }?;
    let ticks =
        |time: FILETIME| u64::from(time.dwHighDateTime) << 32 | u64::from(time.dwLowDateTime);
    Ok((ticks(kernel) + ticks(user)) * 100)
}

#[cfg(target_os = "macos")]
fn resident_bytes() -> Result<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = std::mem::size_of_val(&info) as i32;
    let read = unsafe {
        libc::proc_pidinfo(
            std::process::id() as i32,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return Err(format!("proc_pidinfo returned {read} bytes, expected {size}").into());
    }
    Ok(unsafe { info.assume_init() }.pti_resident_size)
}

#[cfg(target_os = "windows")]
fn resident_bytes() -> Result<u64> {
    use windows::Win32::System::{
        ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::GetCurrentProcess,
    };
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let mut info = PROCESS_MEMORY_COUNTERS {
        cb: size,
        ..Default::default()
    };
    unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut info, size)? };
    Ok(info.WorkingSetSize as u64)
}

#[cfg(target_os = "windows")]
fn check_last_window_exit(app: &mut App, event_loop: &ActiveEventLoop) -> Result<()> {
    let check_saved = |app: &App| -> Result<()> {
        let restored = Workspace::load(&app.state_path)?.ok_or("workspace was not saved")?;
        if serde_json::to_value(restored)? != serde_json::to_value(&app.workspace)? {
            return Err(
                "saved workspace lost the current windows, tabs, splits or directories".into(),
            );
        }
        Ok(())
    };
    let window_key = |app: &App, id: Id| {
        app.windows
            .iter()
            .find(|(_, host)| host.id == id)
            .map(|(&key, _)| key)
            .ok_or("missing window for exit check")
    };
    let original = app.workspace.windows[0].id;
    let quick = app.add_window(true);
    let second = app.add_window(false);
    app.loaded.config.quit_after_last_window_closed = true;
    app.loaded.config.confirm_close_surface = config::ConfirmCloseSurface::True;
    app.reconcile(event_loop);
    let quick_key = window_key(app, quick)?;
    if app.windows[&quick_key].visible || event_loop.exiting() {
        return Err("hidden quick terminal changed the normal window lifecycle".into());
    }
    let second_key = window_key(app, second)?;
    app.window_event(event_loop, second_key, WindowEvent::CloseRequested);
    if event_loop.exiting() || app.index(second).is_some() {
        return Err("closing one of two regular windows quit or left it open".into());
    }
    // A due autosave must survive another layout change and run through the
    // event loop, including replacing an existing workspace file on Windows.
    app.save_at = Some(Instant::now());
    app.changed();
    app.about_to_wait(event_loop);
    check_saved(app)?;
    app.loaded.config.quit_after_last_window_closed = false;
    let original_key = window_key(app, original)?;
    app.window_event(event_loop, original_key, WindowEvent::CloseRequested);
    if event_loop.exiting() || app.index(original).is_some() || app.index(quick).is_none() {
        return Err("quit-after-last-window-closed=false did not keep the app running".into());
    }
    app.save_at = Some(Instant::now());
    app.about_to_wait(event_loop);
    check_saved(app)?;
    if !app.undo_layout(false) {
        return Err("could not restore the multi-tab layout for the exit check".into());
    }
    app.reconcile(event_loop);
    let last_key = window_key(app, original)?;
    let mut host = app.windows.remove(&last_key).unwrap();
    app.action(
        event_loop,
        &mut host,
        Action::NewSplit(Direction::Right),
        true,
    );
    let tab = app.tab_mut(original).unwrap();
    tab.title = Some("Saved before closing".into());
    tab.color = Some([50, 120, 180]);
    app.windows.insert(last_key, host);
    app.reconcile(event_loop);
    let expected = serde_json::to_value(&app.workspace)?;
    if app.save_at.is_none() || serde_json::to_value(Workspace::load(&app.state_path)?)? == expected
    {
        return Err("exit check did not have pending layout changes to save".into());
    }
    let quick_pane = app.focused(quick).ok_or("missing quick terminal pane")?;
    app.panes.get_mut(&quick_pane).unwrap().running = Some(Instant::now());
    app.loaded.config.quit_after_last_window_closed = true;
    app.window_event(event_loop, last_key, WindowEvent::CloseRequested);
    if event_loop.exiting()
        || !matches!(
            app.windows
                .get(&last_key)
                .and_then(|host| host.confirm.as_ref()),
            Some(Confirmation::Close(Action::CloseWindow))
        )
    {
        return Err(
            "last-window close skipped confirmation for a running quick-terminal job".into(),
        );
    }
    let mut host = app.windows.remove(&last_key).unwrap();
    host.confirm = None;
    app.action(event_loop, &mut host, Action::CloseWindow, true);
    app.windows.insert(last_key, host);
    app.reconcile(event_loop);
    if !event_loop.exiting() || serde_json::to_value(&app.workspace)? != expected {
        return Err("last-window close did not quit with its layout intact".into());
    }
    check_saved(app)?;
    // Shutdown also saves: it must not overwrite the intact layout with the
    // result of removing the last regular window. Session close is idempotent.
    app.exiting(event_loop);
    check_saved(app)?;
    fs::write(
        app.state_path.with_file_name("last-window-layout.json"),
        serde_json::to_vec_pretty(&expected)?,
    )?;
    if !app.errors.is_empty() {
        return Err(format!("window lifecycle errors: {:?}", app.errors).into());
    }
    eprintln!(
        "Native smoke: layouts autosaved during changes; last-window close preserved pending edits, honored the opt-out and confirmed the quick terminal's running job"
    );
    Ok(())
}

fn check_terminal_frames(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    host: &mut Host,
) -> Result<()> {
    let id = app
        .focused(host.id)
        .ok_or("no pane for synchronized-output check")?;
    let pane = app.panes.get_mut(&id).unwrap();
    let original = {
        let mut terminal = pane.session.terminal()?;
        let mut fixture = vt::Terminal::new(terminal.cols, terminal.rows, 0);
        fixture.set_pixel_size(terminal.width_px, terminal.height_px);
        fixture.screen_mut().cursor.blink = false;
        fixture.feed(b"\x1b[2;3Hcomplete frame");
        std::mem::replace(&mut *terminal, fixture)
    };
    let sync = std::mem::take(&mut pane.sync_output);
    let prepared = host.prepared.remove(&id);
    let focused = host.focused;
    let blink_started = host.cursor_blink_started;
    let focus_hint = host.focus_hint;
    let navigation_warning = host.navigation_warning.take();
    // A preceding progress timeout can leave a badge flash with its own redraw timer.
    let activity_flashes = std::mem::take(&mut app.activity_flashes);
    let accesskit_active = std::mem::replace(&mut host.accesskit_active, false);
    host.focus_hint.dismiss();
    let result = (|| -> Result<()> {
        app.draw(event_loop, host)?;
        if host.prepared[&id].text.is_some() {
            return Err("inactive accessibility built terminal text".into());
        }
        let normal = host.prepared[&id].key.options.clone();
        for (sequence, foreground, background) in [
            (b"\x1b[?5h".as_slice(), normal.background, normal.foreground),
            (b"\x1b[?5l", normal.foreground, normal.background),
        ] {
            app.panes[&id].session.terminal()?.feed(sequence);
            app.draw(event_loop, host)?;
            let prepared = &host.prepared[&id];
            if prepared.key.options.foreground != foreground
                || prepared.key.options.background != background
                || prepared.key.options.palette != normal.palette
                || prepared.frame.quads.first().map(|quad| quad.color)
                    != Some(
                        rustty_render::Color::rgb(background).opacity(normal.background_opacity),
                    )
            {
                return Err("reverse video did not change the rendered default colors".into());
            }
        }
        let columns = app.panes[&id].session.terminal()?.cols;
        for (sequence, expected) in [
            (b"\x1b[?40h\x1b[?3h".as_slice(), 132),
            (b"\x1b[?3l", 80),
            (b"\x1b[?40l", columns),
        ] {
            let generation = {
                let mut terminal = app.panes[&id].session.terminal()?;
                terminal.feed(sequence);
                terminal.generation
            };
            app.draw(event_loop, host)?;
            if app.panes[&id].session.terminal()?.cols != expected
                || host.prepared[&id].key.generation != generation
            {
                return Err("redraw undid DEC column mode or failed to render its grid".into());
            }
        }
        app.panes[&id]
            .session
            .terminal()?
            .feed(b"\x1b[2;3Hcomplete frame");
        app.draw(event_loop, host)?;
        let previous = &host.prepared[&id];
        let generation = previous.key.generation;
        let cursor = previous.key.cursor;
        let ime_rect = previous.ime_rect;
        let frame = Arc::clone(&previous.frame);
        for chunk in [
            b"\x1b[?2026h\x1b[2J\x1b[H".as_slice(),
            b"\x1b[3;1Hpartial frame",
        ] {
            app.panes[&id].session.terminal()?.feed(chunk);
            app.draw(event_loop, host)?;
            let held = &host.prepared[&id];
            if held.key.generation != generation
                || held.key.cursor != cursor
                || held.ime_rect != ime_rect
            {
                return Err("synchronized output displayed a partial frame or cursor".into());
            }
        }
        host.accesskit_active = true;
        app.draw(event_loop, host)?;
        let held = &host.prepared[&id];
        if held.text.is_none()
            || held.key.generation != generation
            || !Arc::ptr_eq(&held.frame, &frame)
        {
            return Err("accessibility activation did not retain the synchronized frame".into());
        }
        host.accesskit_active = false;
        // Another window can leave the shared context enabled between this window's draws.
        app.context.enable_accesskit();
        for chunk in [b"\x1b[Hfinished\x1b[?2026l".as_slice(), b"!"] {
            let generation = {
                let mut terminal = app.panes[&id].session.terminal()?;
                terminal.feed(chunk);
                terminal.generation
            };
            app.draw(event_loop, host)?;
            if host.prepared[&id].key.generation != generation
                || app.panes[&id].sync_output.deadline.is_some()
            {
                return Err("completed or ordinary output waited for another frame".into());
            }
            if host.prepared[&id].text.is_some() {
                return Err(
                    "inactive window inherited another window's accessibility state".into(),
                );
            }
        }
        app.panes[&id]
            .session
            .terminal()?
            .feed(b"\x1b[?2026h\x1b[2J");
        host.fonts.clear_cache();
        app.draw(event_loop, host)?;
        if app.panes[&id].session.terminal()?.modes.dec(2026)
            || host.prepared[&id].frame.generation != host.fonts.generation()
        {
            return Err("atlas replacement retained an invalid synchronized frame".into());
        }
        // Text blinking must keep working with a hidden cursor. Drive the
        // existing clock directly so the check needs no timed waits.
        host.focused = true;
        app.panes[&id]
            .session
            .terminal()?
            .feed(b"\x1b[H\x1b[2J\x1b[?25l\x1b[5mblinking\x1b[0m");
        host.cursor_blink_started = Instant::now();
        app.draw(event_loop, host)?;
        let shown = host.prepared[&id].frame.quads.clone();
        if host.deadline.is_none() || !host.prepared[&id].key.options.blink_visible {
            return Err("blinking text did not schedule a redraw with the cursor hidden".into());
        }
        host.cursor_blink_started = Instant::now() - Duration::from_millis(650);
        app.draw(event_loop, host)?;
        if host.deadline.is_none() || host.prepared[&id].frame.quads == shown {
            return Err("blinking text did not change at the next blink phase".into());
        }
        app.panes[&id].session.terminal()?.feed(b"\x1b[?2026h");
        app.draw(event_loop, host)?;
        if host.deadline.is_some() {
            return Err("synchronized output kept a text-blink timer running".into());
        }
        app.panes[&id].session.terminal()?.feed(b"\x1b[?2026l");
        host.focused = false;
        app.draw(event_loop, host)?;
        if host.deadline.is_some() || host.prepared[&id].frame.quads != shown {
            return Err("unfocused blinking text was hidden or kept waking the app".into());
        }
        host.focused = true;
        app.panes[&id].session.terminal()?.feed(b"\x1b[2J");
        app.draw(event_loop, host)?;
        if host.deadline.is_some() {
            return Err("erasing blinking text left a redraw timer running".into());
        }
        // Only the progress overlay changes during these frames. Keep the
        // terminal composition and GPU input alive across the animation.
        let activity = std::mem::take(&mut app.panes.get_mut(&id).unwrap().activity);
        app.panes
            .get_mut(&id)
            .unwrap()
            .activity
            .progress_reported(3, None, Instant::now());
        let result = (|| -> Result<()> {
            host.focused = false;
            app.draw(event_loop, host)?;
            let frame = host
                .composed
                .as_ref()
                .ok_or("missing terminal frame")?
                .frame
                .clone();
            let prepares = host.pane_prepares;
            for _ in 0..3 {
                app.draw(event_loop, host)?;
                if !Arc::ptr_eq(&frame, &host.composed.as_ref().unwrap().frame)
                    || host.pane_prepares != prepares
                {
                    return Err("progress animation rebuilt unchanged terminal content".into());
                }
            }
            let frames = host.frames;
            host.occluded = true;
            let hidden = app.draw(event_loop, host);
            host.occluded = false;
            hidden?;
            if host.frames != frames {
                return Err("occluded progress animation kept drawing".into());
            }
            app.panes[&id]
                .session
                .terminal()?
                .feed(b"changed behind cover");
            app.draw(event_loop, host)?;
            if host.frames != frames + 1
                || Arc::ptr_eq(&frame, &host.composed.as_ref().unwrap().frame)
            {
                return Err("revealing the window did not refresh terminal content".into());
            }
            Ok(())
        })();
        app.panes.get_mut(&id).unwrap().activity = activity;
        result?;
        Ok(())
    })();
    let pane = app.panes.get_mut(&id).unwrap();
    *pane.session.terminal()? = original;
    pane.sync_output = sync;
    host.prepared.remove(&id);
    if let Some(prepared) = prepared {
        host.prepared.insert(id, prepared);
    }
    host.focused = focused;
    host.cursor_blink_started = blink_started;
    host.focus_hint = focus_hint;
    host.navigation_warning = navigation_warning;
    app.activity_flashes = activity_flashes;
    host.accesskit_active = accesskit_active;
    host.repaint();
    result?;
    eprintln!(
        "Native smoke: terminal rendering and synchronized output passed; progress reused terminal content, stopped while occluded, and refreshed after reveal"
    );
    Ok(())
}

fn check_passive_pointer_motion(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    key: WindowId,
) -> Result<()> {
    use winit::event::DeviceId;

    let host = &app.windows[&key];
    let mut positions: Vec<_> = host.rects.values().map(egui::Rect::center).collect();
    let bounds = host.content;
    let workspace::Node::Split { axis, ratio, .. } = app.tab(host.id).unwrap().root.kind else {
        return Err("no divider for passive pointer check".into());
    };
    let mut divider = Pos2::from(bounds.center());
    match axis {
        Axis::Horizontal => divider.x = bounds.x + bounds.width * ratio,
        Axis::Vertical => divider.y = bounds.y + bounds.height * ratio,
    }
    positions.insert(1, divider);
    let scale = host.window.scale_factor();
    let pointer_in_window = host.egui.is_pointer_in_window();
    let motion = |position: Pos2| WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: LogicalPosition::new(position.x, position.y).to_physical(scale),
    };
    let host = app.windows.get_mut(&key).unwrap();
    let saved = (host.mouse, host.modifiers, host.deferred_pointer);
    let link_hit = host.link_hit.take();
    let hovered_link = host.hovered_link.take();
    let events = std::mem::take(&mut host.egui.egui_input_mut().events);
    host.mouse = positions[0];
    host.modifiers = Modifiers::default();
    let _ = host.egui.on_window_event(&host.window, &motion(host.mouse));
    host.egui.egui_input_mut().events.clear();
    let result = (|| -> Result<()> {
        // Crossing a split gap changes the native cursor, but no terminal pixels.
        for position in positions.into_iter().cycle().take(256) {
            app.window_event(event_loop, key, motion(position));
            let host = app.windows.get_mut(&key).unwrap();
            if host.deferred_pointer != Some(position)
                || !host.egui.egui_input_mut().events.is_empty()
            {
                return Err(format!("passive pointer motion at {position:?} reached egui").into());
            }
        }
        Ok(())
    })();
    let host = app.windows.get_mut(&key).unwrap();
    (host.mouse, host.modifiers, host.deferred_pointer) = saved;
    host.link_hit = link_hit;
    host.hovered_link = hovered_link;
    let restore_pointer = if pointer_in_window {
        motion(host.mouse)
    } else {
        WindowEvent::CursorLeft {
            device_id: DeviceId::dummy(),
        }
    };
    let _ = host.egui.on_window_event(&host.window, &restore_pointer);
    host.egui.egui_input_mut().events = events;
    result?;
    eprintln!("Native smoke: repeated pointer motion across panes and split gaps stayed deferred");
    Ok(())
}

fn check_focus_hint_clicks(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    key: WindowId,
) -> Result<()> {
    use winit::event::{
        DeviceId,
        ElementState::{Pressed, Released},
        MouseButton::{Back, Forward, Left, Middle, Other, Right},
        TouchPhase,
    };

    let host = &app.windows[&key];
    let window = host.id;
    let original_focus = app.focused(window).ok_or("no focus-hint test pane")?;
    let other = *host
        .rects
        .keys()
        .find(|&&id| id != original_focus)
        .ok_or("no second focus-hint test pane")?;
    let positions = [original_focus, other].map(|id| {
        // Focused directory labels are centered; click near the bottom instead.
        (id, host.rects[&id].left_bottom() + Vec2::new(24.0, -24.0))
    });
    let scale = host.window.scale_factor();
    let pointer_in_window = host.egui.is_pointer_in_window();
    let saved_host = (host.focused, host.focus_hint, host.mouse, host.modifiers);
    let bounds = host.content;
    let workspace::Node::Split { axis, ratio, .. } = app.tab(window).unwrap().root.kind else {
        return Err("no divider for focus-hint check".into());
    };
    let mut divider = Pos2::from(bounds.center());
    match axis {
        Axis::Horizontal => divider.x = bounds.x + bounds.width * ratio,
        Axis::Vertical => divider.y = bounds.y + bounds.height * ratio,
    }
    let host = app.windows.get_mut(&key).unwrap();
    let events = std::mem::take(&mut host.egui.egui_input_mut().events);
    host.modifiers = Modifiers::default();
    let mut saved = Vec::new();
    for id in [original_focus, other] {
        let pane = app.panes.get_mut(&id).unwrap();
        let mut terminal = pane.session.terminal()?;
        let mut fixture = vt::Terminal::new(terminal.cols, terminal.rows, 0);
        fixture.mouse_mode = 1000;
        fixture.mouse_format = 1006;
        saved.push((
            id,
            std::mem::replace(&mut *terminal, fixture),
            std::mem::take(&mut pane.input),
            std::mem::take(&mut pane.input_bytes),
            pane.mouse_cell.take(),
            std::mem::take(&mut pane.selection_gesture),
        ));
        // Capture reports instead of sending the simulated clicks to a shell.
        pane.input.push_back(SessionInput::Text(Vec::new()));
    }
    let motion = |position: Pos2| WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: LogicalPosition::new(position.x, position.y).to_physical(scale),
    };
    let button = |state, button| WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state,
        button,
    };
    let dispatch = |app: &mut App, event, pane, visible| -> Result<()> {
        app.window_event(event_loop, key, event);
        let hint = app.windows[&key].focus_hint;
        let now = Instant::now();
        if app.focused(window) != Some(pane)
            || hint.visible(pane, now) != visible
            || hint.deadline(now).is_some() != visible
        {
            return Err(
                format!("focus hint expected pane {pane}, visible={visible}: {hint:?}").into(),
            );
        }
        Ok(())
    };
    let result = (|| -> Result<()> {
        // AppKit consumes activation mouse-down; its release must retain the hint.
        dispatch(app, WindowEvent::Focused(false), original_focus, false)?;
        dispatch(app, WindowEvent::Focused(true), original_focus, true)?;
        dispatch(app, motion(positions[0].1), original_focus, true)?;
        dispatch(app, button(Released, Left), original_focus, true)?;
        dispatch(app, button(Pressed, Left), original_focus, false)?;
        dispatch(app, button(Released, Left), original_focus, false)?;

        for (index, mouse_button) in [Left, Middle, Right, Back, Forward, Other(4)]
            .into_iter()
            .enumerate()
        {
            let (previous, _) = positions[index % 2];
            let (pane, position) = positions[(index + 1) % 2];
            dispatch(app, motion(position), previous, false)?;
            dispatch(app, button(Pressed, mouse_button), pane, true)?;
            dispatch(app, button(Released, mouse_button), pane, true)?;
            dispatch(app, motion(position + Vec2::splat(1.0)), pane, true)?;
            dispatch(
                app,
                WindowEvent::MouseWheel {
                    device_id: DeviceId::dummy(),
                    delta: MouseScrollDelta::LineDelta(0.0, 1.0),
                    phase: TouchPhase::Moved,
                },
                pane,
                true,
            )?;
            let reports = app.panes[&pane].input.len();
            dispatch(app, button(Pressed, mouse_button), pane, false)?;
            if index < 3 && app.panes[&pane].input.len() != reports + 1 {
                return Err("dismissing the hint swallowed the terminal mouse report".into());
            }
            dispatch(app, button(Released, mouse_button), pane, false)?;
        }

        for (target, position) in [
            ("search", positions[0].1),
            ("divider", divider),
            ("modal", positions[0].1),
        ] {
            dispatch(app, WindowEvent::Focused(false), original_focus, false)?;
            dispatch(app, WindowEvent::Focused(true), original_focus, true)?;
            let host = app.windows.get_mut(&key).unwrap();
            if target == "search" {
                host.search_rects.insert(
                    original_focus,
                    egui::Rect::from_center_size(position, Vec2::splat(20.0)),
                );
            }
            host.palette.open = target == "modal";
            dispatch(app, motion(position), original_focus, true)?;
            dispatch(app, button(Pressed, Left), original_focus, false)?;
            let host = &app.windows[&key];
            if target == "search" && host.search_focus != Some(original_focus)
                || target == "divider" && host.divider_drag.is_none()
            {
                return Err(format!("dismissing the hint swallowed the {target} click").into());
            }
            if let Some((id, axis, _)) = host.divider_drag {
                let workspace::Node::Split { ratio, .. } =
                    app.tab(window).unwrap().root.node(id).unwrap().kind
                else {
                    unreachable!("dragging a split divider");
                };
                let offset = match axis {
                    Axis::Horizontal => Vec2::new(24.0, 0.0),
                    Axis::Vertical => Vec2::new(0.0, 24.0),
                };
                let moved = dispatch(app, motion(position + offset), original_focus, false);
                let changed = matches!(
                    app.tab(window).unwrap().root.node(id).unwrap().kind,
                    workspace::Node::Split { ratio: current, .. } if current != ratio
                );
                app.tab_mut(window).unwrap().root.set_ratio(id, ratio);
                moved?;
                if !changed {
                    return Err("held pointer motion did not resize the split".into());
                }
            }
            dispatch(app, button(Released, Left), original_focus, false)?;
            let host = app.windows.get_mut(&key).unwrap();
            host.search_rects.clear();
            host.search_focus = None;
            host.focus_text_input = false;
            host.palette.open = false;
        }
        dispatch(app, WindowEvent::Focused(false), original_focus, false)?;
        dispatch(app, WindowEvent::Focused(true), original_focus, true)?;
        dispatch(
            app,
            WindowEvent::Ime(Ime::Commit("x".into())),
            original_focus,
            false,
        )?;
        Ok(())
    })();
    for (id, terminal, input, input_bytes, mouse_cell, selection_gesture) in saved {
        let pane = app.panes.get_mut(&id).unwrap();
        *pane.session.terminal()? = terminal;
        pane.input = input;
        pane.input_bytes = input_bytes;
        pane.mouse_cell = mouse_cell;
        pane.selection_gesture = selection_gesture;
    }
    app.focus_pane(window, original_focus);
    let host = app.windows.get_mut(&key).unwrap();
    (host.focused, host.focus_hint, host.mouse, host.modifiers) = saved_host;
    host.search_rects.clear();
    host.search_focus = None;
    host.focus_text_input = false;
    host.palette.open = false;
    host.mouse_button = None;
    host.selection_drag = None;
    host.divider_drag = None;
    let restore_pointer = if pointer_in_window {
        motion(host.mouse)
    } else {
        WindowEvent::CursorLeft {
            device_id: DeviceId::dummy(),
        }
    };
    let _ = host.egui.on_window_event(&host.window, &restore_pointer);
    host.egui.egui_input_mut().events = events;
    host.egui.egui_input_mut().focused = host.focused;
    app.sync_host_state();
    result?;
    eprintln!(
        "Native smoke: focus labels survived focus clicks and dismissed on subsequent window clicks"
    );
    Ok(())
}

fn check_find(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    host: &mut Host,
    directory: &Path,
) -> Result<()> {
    let original_focus = app.focused(host.id).ok_or("no Find test pane")?;
    let other = *host
        .rects
        .keys()
        .find(|&&id| id != original_focus)
        .ok_or("no second Find test pane")?;
    let focused = host.focused;
    let mouse = host.mouse;
    let pointer_in_window = host.egui.is_pointer_in_window();
    // egui-winit queries AppKit's actual focus on macOS, so simulate both input
    // states directly when capturing an unfocused window without switching apps.
    let set_focus = |host: &mut Host, focused| {
        host.focused = focused;
        let raw = host.egui.egui_input_mut();
        raw.focused = focused;
        raw.events.push(egui::Event::WindowFocused(focused));
    };
    let mut saved = Vec::new();
    for id in [original_focus, other] {
        let pane = app.panes.get_mut(&id).unwrap();
        let mut terminal = pane.session.terminal()?;
        let mut fixture = vt::Terminal::new(terminal.cols, terminal.rows, 64);
        fixture.set_pixel_size(terminal.width_px, terminal.height_px);
        fixture.screen_mut().cursor.blink = false;
        fixture.feed(
            "Find overlay geometry\r\n"
                .repeat(usize::from(terminal.rows) + 6)
                .as_bytes(),
        );
        fixture.feed(b"alpha beta\r\nsecond alpha\r\nthird alpha beta");
        saved.push((
            id,
            std::mem::replace(&mut *terminal, fixture),
            pane.search.take(),
            std::mem::take(&mut pane.input),
            std::mem::take(&mut pane.input_bytes),
        ));
        pane.input.push_back(SessionInput::Text(Vec::new()));
    }
    let result = (|| -> Result<()> {
        set_focus(host, true);
        app.draw(event_loop, host)?;
        let rects = host.rects.clone();
        let sizes = |app: &App| -> Result<Vec<_>> {
            rects
                .keys()
                .map(|id| {
                    let terminal = app.panes[id].session.terminal()?;
                    Ok((
                        *id,
                        terminal.cols,
                        terminal.rows,
                        terminal.width_px,
                        terminal.height_px,
                        terminal.query_defaults.size,
                    ))
                })
                .collect()
        };
        let original_sizes = sizes(app)?;
        for (id, query) in [(original_focus, "alpha"), (other, "beta")] {
            app.focus_pane(host.id, id);
            app.action(event_loop, host, Action::StartSearch, false);
            app.draw(event_loop, host)?;
            app.draw(event_loop, host)?;
            host.egui
                .egui_input_mut()
                .events
                .push(egui::Event::Text(query.into()));
            app.draw(event_loop, host)?;
            if app.panes[&id]
                .search
                .as_ref()
                .map(|search| search.query.as_str())
                != Some(query)
                || app.panes[&id]
                    .session
                    .terminal()?
                    .screen()
                    .selection_text()
                    .as_deref()
                    != Some(query)
            {
                return Err(format!("Find did not search pane {id}: query={:?}, selection={:?}, editor={:?}, pending={}, modal={}",
                    app.panes[&id].search.as_ref().map(|search| &search.query),
                    app.panes[&id].session.terminal()?.screen().selection_text(),
                    host.search_focus, host.focus_text_input, host.modal_input()).into());
            }
            if host.rects != rects || sizes(app)? != original_sizes {
                return Err("Find changed pane geometry or terminal/PTY dimensions".into());
            }
        }
        app.action(event_loop, host, Action::StartSearch, false);
        app.draw(event_loop, host)?;
        if app.panes[&original_focus].search.as_ref().unwrap().query != "alpha"
            || app.panes[&other].search.as_ref().unwrap().query != "beta"
            || host.search_rects.len() != 2
        {
            return Err(
                "Find queries were shared, reset on reopening, or not drawn per pane".into(),
            );
        }
        for window_focused in [true, false] {
            set_focus(host, window_focused);
            capture_window(
                app,
                event_loop,
                host,
                &directory.join(if window_focused {
                    "find-focused.png"
                } else {
                    "find-unfocused.png"
                }),
            )?;
            for (id, expected) in [(original_focus, [10, 5]), (other, [4, 4])] {
                let frame = &host.prepared[&id].frame;
                let actual = [[255, 224, 130], [242, 165, 126]].map(|color| {
                    frame
                        .quads
                        .iter()
                        .filter(|quad| {
                            quad.paint == rustty_render::Paint::Solid
                                && quad.color == rustty_render::Color::rgb(color)
                        })
                        .count()
                });
                if actual != expected {
                    return Err(format!(
                        "Find highlights in pane {id}: {actual:?}, expected {expected:?}"
                    )
                    .into());
                }
            }
        }
        set_focus(host, true);
        app.action(event_loop, host, Action::StartSearch, false);
        app.draw(event_loop, host)?;
        app.draw(event_loop, host)?;
        let selection = app.panes[&other].session.terminal()?.screen().selection;
        let highlights = host.prepared[&other].key.options.search_highlights.clone();
        let raw = host.egui.egui_input_mut();
        raw.events
            .push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        for pressed in [true, false] {
            raw.events.push(egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        app.draw(event_loop, host)?;
        app.draw(event_loop, host)?;
        if host.ui_input()
            || app.focused(host.id) != Some(other)
            || host.search_rects.len() != 2
            || app.panes[&other].search.as_ref().unwrap().query != "beta"
            || app.panes[&other].session.terminal()?.screen().selection != selection
            || host.prepared[&other].key.options.search_highlights != highlights
            || !app
                .context
                .memory(|memory| memory.has_focus(egui::Id::new(("terminal", other))))
        {
            return Err(
                "Enter in Find did not restore terminal focus and preserve the current match"
                    .into(),
            );
        }
        host.mouse = host.search_rects[&original_focus].center();
        let _ = host.egui.on_window_event(
            &host.window,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: LogicalPosition::new(host.mouse.x, host.mouse.y)
                    .to_physical(host.window.scale_factor()),
            },
        );
        app.mouse(host, vt::MouseAction::Press, Some(vt::MouseButton::Left));
        if app.focused(host.id) != Some(original_focus) || host.search_focus != Some(original_focus)
        {
            return Err("clicking an unfocused Find overlay did not focus its pane".into());
        }
        app.draw(event_loop, host)?;
        host.mouse = host.rects[&other].center();
        app.panes[&other]
            .session
            .terminal()?
            .screen_mut()
            .viewport_offset = 0;
        app.scroll(host, MouseScrollDelta::LineDelta(0.0, 1.0));
        if app.panes[&other]
            .session
            .terminal()?
            .screen()
            .viewport_offset
            == 0
            || host.search_focus != Some(original_focus)
        {
            return Err("Find blocked scrolling another pane or stole keyboard focus".into());
        }
        app.mouse(host, vt::MouseAction::Press, Some(vt::MouseButton::Left));
        app.mouse(host, vt::MouseAction::Release, Some(vt::MouseButton::Left));
        app.draw(event_loop, host)?;
        if host.ui_input() || app.focused(host.id) != Some(other) {
            return Err("clicking terminal content did not restore terminal input".into());
        }
        app.action(event_loop, host, Action::StartSearch, false);
        app.action(event_loop, host, Action::NextTab, false);
        app.draw(event_loop, host)?;
        if host.ui_input() || !host.search_rects.is_empty() {
            return Err("a hidden tab's Find retained input or remained visible".into());
        }
        app.action(event_loop, host, Action::PreviousTab, false);
        app.draw(event_loop, host)?;
        app.action(event_loop, host, Action::ToggleSplitZoom, false);
        app.draw(event_loop, host)?;
        if host.search_rects.len() != 1 {
            return Err("zoom did not hide other pane overlays".into());
        }
        app.action(event_loop, host, Action::ToggleSplitZoom, false);
        app.draw(event_loop, host)?;
        for id in [original_focus, other] {
            app.search_action(host, id, Action::EndSearch);
            app.draw(event_loop, host)?;
            if app.panes[&id]
                .session
                .terminal()?
                .screen()
                .selection
                .is_some()
                || !host.prepared[&id].key.options.search_highlights.is_empty()
            {
                return Err("closing Find left its highlight behind".into());
            }
        }
        if host.rects != rects || sizes(app)? != original_sizes || host.ui_input() {
            return Err("closing Find changed geometry or retained editor input".into());
        }
        if [original_focus, other]
            .iter()
            .any(|id| app.panes[id].input.len() != 1 || app.panes[id].input_bytes != 0)
        {
            return Err("Find interaction leaked input to a terminal".into());
        }
        Ok(())
    })();
    for (id, terminal, search, input, input_bytes) in saved {
        let pane = app.panes.get_mut(&id).unwrap();
        *pane.session.terminal()? = terminal;
        pane.search = search;
        pane.input = input;
        pane.input_bytes = input_bytes;
        host.prepared.remove(&id);
    }
    app.focus_pane(host.id, original_focus);
    host.search_focus = None;
    host.search_rects.clear();
    set_focus(host, focused);
    host.mouse = mouse;
    if !pointer_in_window {
        let _ = host.egui.on_window_event(
            &host.window,
            &WindowEvent::CursorLeft {
                device_id: winit::event::DeviceId::dummy(),
            },
        );
    }
    app.draw(event_loop, host)?;
    result?;
    eprintln!(
        "Native smoke: per-pane Find preserved geometry, queries, focus, scrolling, tabs, and zoom; focused/unfocused captures saved"
    );
    Ok(())
}

fn check_pointer_targets(app: &mut App, host: &mut Host) -> Result<()> {
    let focused = app
        .focused(host.id)
        .ok_or("no focused pane for scrolling")?;
    let hovered = *host
        .rects
        .keys()
        .find(|&&id| id != focused)
        .ok_or("no unfocused pane for scrolling")?;
    let mouse = host.mouse;
    let modifiers = host.modifiers;
    let link_hit = host.link_hit.take();
    let hovered_link = host.hovered_link.take();
    let pointer_in_window = host.egui.is_pointer_in_window();
    let mut saved = Vec::new();
    for id in [focused, hovered] {
        let pane = app.panes.get_mut(&id).unwrap();
        let mut terminal = pane.session.terminal()?;
        let mut fixture = vt::Terminal::new(terminal.cols, terminal.rows, 64);
        fixture.feed("\r\n".repeat(usize::from(terminal.rows) + 12).as_bytes());
        saved.push((
            id,
            std::mem::replace(&mut *terminal, fixture),
            std::mem::take(&mut pane.input),
            std::mem::take(&mut pane.input_bytes),
            pane.mouse_cell.take(),
            std::mem::take(&mut pane.scroll),
            std::mem::take(&mut pane.selection_gesture),
        ));
        // Hold encoded reports in the existing queue instead of sending them to a shell.
        pane.input.push_back(SessionInput::Text(Vec::new()));
    }
    let result = (|| -> Result<()> {
        host.mouse = host.rects[&hovered].center();
        let _ = host.egui.on_window_event(
            &host.window,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: LogicalPosition::new(host.mouse.x, host.mouse.y)
                    .to_physical(host.window.scale_factor()),
            },
        );
        app.panes[&focused]
            .session
            .terminal()?
            .feed(b"\x1b]22;wait\x07");
        app.panes[&hovered]
            .session
            .terminal()?
            .feed(b"\x1b]22;hand\x07");
        if app.pointer_cursor(host) != Some(CursorIcon::Pointer) {
            return Err("OSC 22 pointer shape did not follow the hovered pane".into());
        }
        host.mouse = host.rects[&focused].center();
        if app.pointer_cursor(host) != Some(CursorIcon::Wait) {
            return Err("OSC 22 pointer shape leaked into another pane".into());
        }
        check_click_selection(app, host, focused, hovered)?;
        check_link_hover(app, host, focused, hovered)?;
        check_selection_scroll(app, host, focused, hovered)?;
        host.mouse = host.rects[&hovered].center();
        let pixels = f64::from(host.fonts.metrics().cell_height) * host.window.scale_factor() / 2.0;
        // Fractional movement, a reversal, then a decaying native momentum tail.
        // The final reverse scroll returns the viewport and remainder to zero.
        let trackpad = [
            (0.25, 0_isize),
            (0.25, 0),
            (0.0, 0),
            (-0.125, 0),
            (0.25, 0),
            (0.375, 1),
            (1.5, 1),
            (0.75, 1),
            (0.375, 0),
            (0.1875, 0),
            (0.125, 0),
            (0.0625, 1),
            (-4.0, -4),
        ];
        for (focused_mode, hovered_mode, shift, capture) in [
            (0, 0, false, false),
            (1000, 0, false, false),
            (0, 1000, false, false),
            (1000, 1000, false, false),
            (0, 1000, true, false),
            (0, 1000, true, true),
            (1000, 1000, true, true),
        ] {
            host.modifiers = if shift {
                winit::keyboard::ModifiersState::SHIFT
            } else {
                winit::keyboard::ModifiersState::empty()
            }
            .into();
            for (id, mode) in [(focused, focused_mode), (hovered, hovered_mode)] {
                let mut terminal = app.panes[&id].session.terminal()?;
                terminal.mouse_mode = mode;
                terminal.mouse_format = 1006;
                terminal.feed(if capture == (id == hovered) {
                    b"\x1b[>1s"
                } else {
                    b"\x1b[>0s"
                });
            }
            let reporting = hovered_mode != 0 && (!shift || capture);
            let mut offset = 0;
            for (delta, rows, reports, code) in [
                (MouseScrollDelta::LineDelta(0.0, 1.0), 3, 1, 64),
                (MouseScrollDelta::PixelDelta((0.0, pixels).into()), 1, 1, 64),
                (MouseScrollDelta::LineDelta(0.0, -1.0), -3, 1, 65),
                (
                    MouseScrollDelta::PixelDelta((0.0, -pixels).into()),
                    -1,
                    1,
                    65,
                ),
                (MouseScrollDelta::LineDelta(0.0, 0.1), 0, 1, 64),
                (MouseScrollDelta::LineDelta(0.0, -0.1), 0, 1, 65),
            ]
            .into_iter()
            .chain(trackpad.into_iter().map(|(fraction, rows)| {
                (
                    MouseScrollDelta::PixelDelta((0.0, fraction * pixels).into()),
                    rows,
                    rows.unsigned_abs(),
                    if rows > 0 { 64 } else { 65 },
                )
            })) {
                offset += rows;
                app.scroll(host, delta);
                let focused_pane = &app.panes[&focused];
                if app.focused(host.id) != Some(focused)
                    || focused_pane.session.terminal()?.screen().viewport_offset != 0
                    || focused_pane.input.len() != 1
                {
                    return Err(
                        "scrolling affected the focused pane instead of the hovered pane".into(),
                    );
                }
                let pane = app.panes.get_mut(&hovered).unwrap();
                if pane.session.terminal()?.screen().viewport_offset
                    != if reporting { 0 } else { offset as usize }
                    || pane.input.len() != if reporting { 1 + reports } else { 1 }
                    || reporting
                        && queued_input(pane)?.iter().skip(1).any(|bytes| {
                            !bytes.starts_with(
                                format!("\x1b[<{};", code + if shift { 4 } else { 0 }).as_bytes(),
                            )
                        })
                {
                    return Err(format!("hovered pane did not scroll correctly: {delta:?}, mouse modes {focused_mode}/{hovered_mode}, shift={shift}").into());
                }
                if reporting {
                    pane.input.truncate(1);
                    pane.input_bytes = 0;
                }
            }
            if reporting {
                for sign in [1.0, -1.0] {
                    app.scroll(
                        host,
                        MouseScrollDelta::PixelDelta((0.0, sign * pixels * 200.0).into()),
                    );
                    let pane = app.panes.get_mut(&hovered).unwrap();
                    if pane.input.len() != 129 {
                        return Err("precise scroll exceeded the mouse-report limit".into());
                    }
                    pane.input.truncate(1);
                    pane.input_bytes = 0;
                }
            }
        }
        // Alternate scrolling follows the hovered pane and DECCKM, even when
        // Kitty keyboard mode is active. Disabling mode 1007 sends nothing.
        app.panes[&hovered].session.terminal()?.mouse_mode = 0;
        app.panes[&hovered]
            .session
            .terminal()?
            .feed(b"\x1b[?1049h\x1b[>31u");
        for (enabled, application) in [(true, false), (true, true), (false, true)] {
            {
                let mut terminal = app.panes[&hovered].session.terminal()?;
                terminal.set_mode(true, 1007, enabled);
                terminal.set_mode(true, 1, application);
            }
            for (delta, rows) in [
                (MouseScrollDelta::LineDelta(0.0, 1.0), 3),
                (MouseScrollDelta::LineDelta(0.0, -1.0), -3),
                (
                    MouseScrollDelta::PixelDelta((0.0, pixels * 200.0).into()),
                    128,
                ),
                (
                    MouseScrollDelta::PixelDelta((0.0, -pixels * 200.0).into()),
                    -128,
                ),
            ]
            .into_iter()
            .chain(trackpad.into_iter().map(|(fraction, rows)| {
                (
                    MouseScrollDelta::PixelDelta((0.0, fraction * pixels).into()),
                    rows,
                )
            })) {
                app.scroll(host, delta);
                let pane = app.panes.get_mut(&hovered).unwrap();
                let expected = [
                    0x1b,
                    if application { b'O' } else { b'[' },
                    if rows > 0 { b'A' } else { b'B' },
                ]
                .repeat(rows.unsigned_abs());
                let emitted = enabled && rows != 0;
                if pane.input.len() != if emitted { 2 } else { 1 }
                    || emitted && queued_input(pane)?.last() != Some(&expected)
                {
                    return Err("alternate scroll ignored mode 1007 or cursor-key mode".into());
                }
                if emitted {
                    pane.input.pop_back();
                    pane.input_bytes = 0;
                }
                if app.focused(host.id) != Some(focused) || app.panes[&focused].input.len() != 1 {
                    return Err("alternate scroll changed the keyboard target".into());
                }
            }
        }
        app.panes[&hovered].session.terminal()?.feed(b"\x1b[?1049l");

        // A stale last position after leaving the window must not keep a target.
        let _ = host.egui.on_window_event(
            &host.window,
            &WindowEvent::CursorLeft {
                device_id: winit::event::DeviceId::dummy(),
            },
        );
        app.scroll(host, MouseScrollDelta::LineDelta(0.0, 1.0));
        host.mouse = Pos2::new(-1.0, -1.0);
        let _ = host.egui.on_window_event(
            &host.window,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (0.0, 0.0).into(),
            },
        );
        app.scroll(host, MouseScrollDelta::LineDelta(0.0, 1.0));
        for id in [focused, hovered] {
            let pane = &app.panes[&id];
            if pane.session.terminal()?.screen().viewport_offset != 0 || pane.input.len() != 1 {
                return Err("scrolling outside panes kept a terminal target".into());
            }
        }

        // A Finder drag can enter without updating the ordinary pointer state.
        let _ = Platform::cursor_position(&host.window)?;
        let _ = host.egui.on_window_event(
            &host.window,
            &WindowEvent::CursorLeft {
                device_id: winit::event::DeviceId::dummy(),
            },
        );
        let position = host.rects[&hovered].center();
        let (path, quoted) = if cfg!(target_os = "windows") {
            (
                Path::new("C:\\Temp\\rustty's dropped file.txt"),
                "'/c/Temp/rustty'\\''s dropped file.txt' ",
            )
        } else {
            (
                Path::new("/tmp/rustty's dropped file.txt"),
                "'/tmp/rustty'\\''s dropped file.txt' ",
            )
        };
        for bracketed in [false, true] {
            for (id, enabled) in [(focused, !bracketed), (hovered, bracketed)] {
                app.panes[&id].session.terminal()?.feed(if enabled {
                    b"\x1b[?2004h"
                } else {
                    b"\x1b[?2004l"
                });
            }
            app.drop_file(host, position, path);
            app.drop_file(host, Pos2::ZERO, path);
            let expected = if bracketed {
                format!("\x1b[200~{quoted}\x1b[201~").into_bytes()
            } else {
                quoted.as_bytes().to_vec()
            };
            if app.focused(host.id) != Some(focused) || app.panes[&focused].input.len() != 1 {
                return Err("file drop affected the focused pane".into());
            }
            let pane = app.panes.get_mut(&hovered).unwrap();
            if pane.input.len() != 2 || queued_input(pane)?.last() != Some(&expected) {
                return Err(
                    "file drop did not use the target pane's paste mode and quoted path".into(),
                );
            }
            pane.input.pop_back();
            pane.input_bytes = 0;
        }
        Ok(())
    })();
    for (id, terminal, input, input_bytes, mouse_cell, scroll, selection_gesture) in saved {
        let pane = app.panes.get_mut(&id).unwrap();
        pane.reset_selection_gesture();
        *pane.session.terminal()? = terminal;
        pane.input = input;
        pane.input_bytes = input_bytes;
        pane.mouse_cell = mouse_cell;
        pane.scroll = scroll;
        pane.selection_gesture = selection_gesture;
    }
    host.mouse = mouse;
    host.modifiers = modifiers;
    host.link_hit = link_hit;
    host.hovered_link = hovered_link;
    let event = if pointer_in_window {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: LogicalPosition::new(mouse.x, mouse.y)
                .to_physical(host.window.scale_factor()),
        }
    } else {
        WindowEvent::CursorLeft {
            device_id: winit::event::DeviceId::dummy(),
        }
    };
    let _ = host.egui.on_window_event(&host.window, &event);
    result?;
    eprintln!(
        "Native smoke: trackpad momentum, scrolling, and file drops followed the pointer without changing focus"
    );
    Ok(())
}

fn check_selection_scroll(app: &mut App, host: &mut Host, focused: Id, target: Id) -> Result<()> {
    let scale = host.window.scale_factor() as f32;
    let metrics = host.fonts.metrics();
    let cell = Vec2::new(metrics.cell_width as f32, metrics.cell_height as f32) / scale;
    let rect = host.rects[&target];
    let padding = host.prepared[&target].key.options.padding;
    let origin = rect.min + Vec2::new(padding[0], padding[1]) / scale;
    let saved_host = (host.focused, host.mouse, host.modifiers);
    let pane = app.panes.get_mut(&target).unwrap();
    pane.reset_selection_gesture();
    let saved_scroll = std::mem::take(&mut pane.scroll);
    let saved_terminal = {
        let mut terminal = pane.session.terminal()?;
        let mut fixture = vt::Terminal::new(terminal.cols, terminal.rows, 128);
        for row in 0..usize::from(terminal.rows) + 40 {
            fixture.feed(format!("row {row:03} text\r\n").as_bytes());
        }
        fixture.screen_mut().scroll_viewport(10);
        std::mem::replace(&mut *terminal, fixture)
    };
    host.focused = true;
    host.modifiers = Modifiers::default();
    let result = (|| -> Result<()> {
        let pointer_row = usize::from(app.panes[&target].session.terminal()?.rows) / 2;
        host.mouse = origin + Vec2::new(2.5 * cell.x, (pointer_row as f32 + 0.5) * cell.y);
        host.mouse_button = Some(vt::MouseButton::Left);
        app.mouse(host, vt::MouseAction::Press, host.mouse_button);
        host.mouse.x += 3.0 * cell.x;
        app.mouse(host, vt::MouseAction::Move, host.mouse_button);
        let initial = app.panes[&target]
            .session
            .terminal()?
            .screen()
            .selection
            .ok_or("drag did not create a selection")?;
        let pixels = f64::from(metrics.cell_height) * f64::from(scale) / 2.0;
        for (delta, offset) in [
            (MouseScrollDelta::LineDelta(0.0, 1.0), 13),
            (MouseScrollDelta::LineDelta(0.0, -1.0), 10),
            (MouseScrollDelta::PixelDelta((0.0, pixels / 4.0).into()), 10),
            (MouseScrollDelta::PixelDelta((0.0, pixels / 4.0).into()), 10),
            (MouseScrollDelta::PixelDelta((0.0, pixels / 4.0).into()), 10),
            (MouseScrollDelta::PixelDelta((0.0, pixels / 4.0).into()), 11),
            (MouseScrollDelta::PixelDelta((0.0, -pixels).into()), 10),
        ] {
            app.scroll(host, delta);
            let terminal = app.panes[&target].session.terminal()?;
            let screen = terminal.screen();
            let end = vt::GridPoint {
                row: screen.viewport().nth(pointer_row).unwrap().id,
                col: 5,
            };
            if screen.viewport_offset != offset
                || screen.selection != Some(vt::Selection { end, ..initial })
            {
                return Err(
                    format!("stationary drag did not follow wheel scroll: {delta:?}").into(),
                );
            }
        }

        let bounds = host.content;
        let workspace::Node::Split { axis, ratio, .. } = app.tab(host.id).unwrap().root.kind else {
            return Err("no divider for selection-scroll check".into());
        };
        let mut divider = Pos2::from(bounds.center());
        match axis {
            Axis::Horizontal => divider.x = bounds.x + bounds.width * ratio,
            Axis::Vertical => divider.y = bounds.y + bounds.height * ratio,
        }
        app.panes[&focused].session.terminal()?.mouse_mode = 1000;
        for position in [
            host.rects[&focused].center(),
            divider,
            Pos2::new(-8.0, -8.0),
        ] {
            host.mouse = position;
            app.mouse(host, vt::MouseAction::Move, host.mouse_button);
            let before = app.panes[&target]
                .session
                .terminal()?
                .screen()
                .viewport_offset;
            app.scroll(host, MouseScrollDelta::LineDelta(0.0, 1.0));
            if app.focused(host.id) != Some(target)
                || app.panes[&target]
                    .session
                    .terminal()?
                    .screen()
                    .viewport_offset
                    != before + 3
                || app.panes[&focused]
                    .session
                    .terminal()?
                    .screen()
                    .viewport_offset
                    != 0
                || app.panes[&focused].input.len() != 1
                || app.panes[&target].input.len() != 1
            {
                return Err(
                    "drag scrolling lost capture across a pane, divider, or window edge".into(),
                );
            }
        }
        app.panes[&focused].session.terminal()?.mouse_mode = 0;

        let now = Instant::now() + Duration::from_secs(1);
        for (position, rows, tick) in [
            (Pos2::new(rect.center().x, rect.top()), 1, now),
            (
                Pos2::new(rect.center().x, rect.bottom()),
                -1,
                now + Duration::from_secs(1),
            ),
        ] {
            host.mouse = position;
            app.mouse(host, vt::MouseAction::Move, host.mouse_button);
            let before = app.panes[&target]
                .session
                .terminal()?
                .screen()
                .viewport_offset;
            app.selection_scroll(host, tick);
            let terminal = app.panes[&target].session.terminal()?;
            if terminal.screen().viewport_offset != before.saturating_add_signed(rows)
                || terminal.screen().selection.is_none()
            {
                return Err("stationary edge drag did not auto-scroll one row".into());
            }
        }
        app.mouse(host, vt::MouseAction::Release, host.mouse_button);
        host.mouse_button = None;
        let offset = app.panes[&target]
            .session
            .terminal()?
            .screen()
            .viewport_offset;
        app.selection_scroll(host, now + Duration::from_secs(3));
        if host.selection_drag.is_some()
            || app.panes[&target]
                .session
                .terminal()?
                .screen()
                .viewport_offset
                != offset
        {
            return Err("selection auto-scroll continued after release".into());
        }

        // A release must finalize against live rows even while rendering waits
        // for synchronized output. Terminal input instead abandons the drag.
        for synchronized in [true, false] {
            app.panes
                .get_mut(&target)
                .unwrap()
                .reset_selection_gesture();
            host.mouse = origin + Vec2::new(2.5 * cell.x, (pointer_row as f32 + 0.5) * cell.y);
            host.mouse_button = Some(vt::MouseButton::Left);
            app.mouse(host, vt::MouseAction::Press, host.mouse_button);
            host.mouse.x += 3.0 * cell.x;
            app.mouse(host, vt::MouseAction::Move, host.mouse_button);
            if synchronized {
                let (selected, expected) = {
                    let mut terminal = app.panes[&target].session.terminal()?;
                    let selected = terminal.screen().selection.ok_or("no synchronized drag")?;
                    terminal.set_mode(true, 2026, true);
                    terminal.screen_mut().scroll_viewport(1);
                    let end = vt::GridPoint {
                        row: terminal.screen().viewport().nth(pointer_row).unwrap().id,
                        col: 5,
                    };
                    (selected, vt::Selection { end, ..selected })
                };
                app.selection_scroll(host, Instant::now());
                if app.panes[&target].session.terminal()?.screen().selection != Some(selected) {
                    return Err("periodic drag refresh ignored synchronized output".into());
                }
                app.mouse(host, vt::MouseAction::Release, host.mouse_button);
                if app.panes[&target].session.terminal()?.screen().selection != Some(expected) {
                    return Err("release copied a stale endpoint during synchronized output".into());
                }
                app.panes[&target]
                    .session
                    .terminal()?
                    .set_mode(true, 2026, false);
            } else {
                app.panes[&target]
                    .session
                    .terminal()?
                    .screen_mut()
                    .selection = None;
                app.terminal_input(host, target);
                app.selection_scroll(host, Instant::now() + Duration::from_secs(1));
                if app.panes[&target]
                    .session
                    .terminal()?
                    .screen()
                    .selection
                    .is_some()
                {
                    return Err(
                        "selection refresh restored a drag cancelled by terminal input".into(),
                    );
                }
            }
            if host.selection_drag.is_some() {
                return Err("release or terminal input retained the selection timer".into());
            }
            host.mouse_button = None;
        }

        app.panes
            .get_mut(&target)
            .unwrap()
            .reset_selection_gesture();
        app.panes[&target]
            .session
            .terminal()?
            .feed(b"\x1b[?1049h\x1b[?1007h\x1b[Halternate text");
        host.mouse = origin + Vec2::new(2.5 * cell.x, 0.5 * cell.y);
        host.mouse_button = Some(vt::MouseButton::Left);
        app.mouse(host, vt::MouseAction::Press, host.mouse_button);
        host.mouse.x += 3.0 * cell.x;
        app.mouse(host, vt::MouseAction::Move, host.mouse_button);
        let selected = app.panes[&target].session.terminal()?.screen().selection;
        app.scroll(host, MouseScrollDelta::LineDelta(0.0, -1.0));
        if selected.is_none()
            || app.panes[&target].session.terminal()?.screen().selection != selected
            || queued_input(&app.panes[&target])?.last().map(Vec::as_slice)
                != Some(b"\x1b[B\x1b[B\x1b[B")
        {
            return Err("alternate scrolling cleared a held selection or lost cursor keys".into());
        }
        app.cancel_selection_drag(host);
        app.panes[&target]
            .session
            .terminal()?
            .feed(b"\x1b[?1000h\x1b[?1006h");
        host.mouse_button = Some(vt::MouseButton::Left);
        app.mouse(host, vt::MouseAction::Press, host.mouse_button);
        host.mouse.y = rect.top();
        app.mouse(host, vt::MouseAction::Move, host.mouse_button);
        app.selection_scroll(host, now + Duration::from_secs(4));
        if host.selection_drag.is_some() || app.panes[&target].input.len() < 3 {
            return Err("application mouse capture became a local selection drag".into());
        }
        Ok(())
    })();
    app.cancel_selection_drag(host);
    let pane = app.panes.get_mut(&target).unwrap();
    *pane.session.terminal()? = saved_terminal;
    pane.scroll = saved_scroll;
    pane.input.truncate(1);
    pane.input_bytes = 0;
    app.panes[&focused].session.terminal()?.mouse_mode = 0;
    app.focus_pane(host.id, focused);
    (host.focused, host.mouse, host.modifiers) = saved_host;
    host.mouse_button = None;
    result?;
    eprintln!(
        "Native smoke: selection followed wheel scrolling and timed pane-edge scrolling through release"
    );
    Ok(())
}

fn check_click_selection(app: &mut App, host: &mut Host, focused: Id, target: Id) -> Result<()> {
    use winit::keyboard::ModifiersState;
    let scale = host.window.scale_factor() as f32;
    let metrics = host.fonts.metrics();
    let cell = Vec2::new(metrics.cell_width as f32, metrics.cell_height as f32) / scale;
    let padding = host.prepared[&target].key.options.padding;
    let origin = host.rects[&target].min + Vec2::new(padding[0], padding[1]) / scale;
    let cols = app.panes[&target].session.terminal()?.cols;
    let url = format!(
        "https://example.org:8443/{}?q=one#two",
        "path/".repeat(usize::from(cols) / 5)
    );
    let click = |app: &mut App, host: &mut Host| {
        host.mouse_button = Some(vt::MouseButton::Left);
        app.mouse(host, vt::MouseAction::Press, host.mouse_button);
        // AppKit repeats the cursor position immediately before mouse-up.
        app.mouse(host, vt::MouseAction::Move, host.mouse_button);
        app.mouse(host, vt::MouseAction::Release, host.mouse_button);
        host.mouse_button = None;
    };
    host.modifiers = ModifiersState::empty().into();
    for (text, row, expected) in [
        ("word next", 0, "word"),
        ("/tmp/source-file.rs next", 0, "/tmp/source-file.rs"),
        (url.as_str(), 1, url.as_str()),
        (
            "\x1b]8;;https://example.org\x07open link\x1b]8;;\x07",
            0,
            "open link",
        ),
    ] {
        let pane = app.panes.get_mut(&target).unwrap();
        pane.reset_selection_gesture();
        pane.session
            .terminal()?
            .feed(format!("\x1b[H\x1b[2J{text}").as_bytes());
        host.mouse = origin + Vec2::new(2.5 * cell.x, (row as f32 + 0.5) * cell.y);
        for count in 1..=2 {
            click(app, host);
            let selection = app.panes[&target]
                .session
                .terminal()?
                .screen()
                .selection_text();
            if selection.as_deref() != (count == 2).then_some(expected)
                || app.focused(host.id) != Some(target)
            {
                return Err(
                    format!("click {count} selected {selection:?}, expected {expected:?}").into(),
                );
            }
        }
        // A click in another pane breaks the repeated-click sequence.
        app.focus_pane(host.id, focused);
        click(app, host);
        if app.panes[&target]
            .session
            .terminal()?
            .screen()
            .selection
            .is_some()
        {
            return Err("pane focus change retained a repeated click".into());
        }
    }
    let pane = app.panes.get_mut(&target).unwrap();
    pane.reset_selection_gesture();
    pane.session.terminal()?.feed(b"\x1b[H\x1b[2Jword next");
    host.mouse = origin + Vec2::new(2.5 * cell.x, 0.5 * cell.y);
    click(app, host);
    host.mouse_button = Some(vt::MouseButton::Left);
    app.mouse(host, vt::MouseAction::Press, host.mouse_button);
    host.mouse.x += 4.0 * cell.x;
    app.mouse(host, vt::MouseAction::Move, host.mouse_button);
    app.mouse(host, vt::MouseAction::Release, host.mouse_button);
    host.mouse_button = None;
    if app.panes[&target]
        .session
        .terminal()?
        .screen()
        .selection_text()
        .as_deref()
        != Some("word next")
    {
        return Err("double-click dragging did not extend by whole words".into());
    }
    app.panes[&target]
        .session
        .terminal()?
        .feed(b"\x1b[H\x1b[2Jword next\x1b[?1000h\x1b[?1006h");
    host.mouse = origin + Vec2::new(2.5 * cell.x, 0.5 * cell.y);
    for shift in [false, true] {
        host.modifiers = if shift {
            ModifiersState::SHIFT
        } else {
            ModifiersState::empty()
        }
        .into();
        let reports = app.panes[&target].input.len();
        click(app, host);
        click(app, host);
        let pane = &app.panes[&target];
        if pane
            .session
            .terminal()?
            .screen()
            .selection_text()
            .as_deref()
            != Some(if shift { "word" } else { "word next" })
            || pane.input.len() != reports + if shift { 0 } else { 4 }
        {
            return Err(
                "double-click ignored application mouse capture or its Shift override".into(),
            );
        }
    }
    let pane = app.panes.get_mut(&target).unwrap();
    pane.session.terminal()?.feed(b"\x1b[?1000l\x1b[?1006l");
    pane.input.truncate(1);
    pane.input_bytes = 0;
    host.modifiers = ModifiersState::empty().into();
    app.focus_pane(host.id, focused);
    eprintln!(
        "Native smoke: double-click selected words, paths, and links without losing selections on mouse-up"
    );
    Ok(())
}

fn check_link_hover(app: &mut App, host: &mut Host, focused: Id, hovered: Id) -> Result<()> {
    use winit::keyboard::ModifiersState;
    let link_modifier = if cfg!(target_os = "windows") {
        ModifiersState::CONTROL
    } else {
        ModifiersState::SUPER
    };
    let scale = host.window.scale_factor() as f32;
    let metrics = host.fonts.metrics();
    let cell = Vec2::new(metrics.cell_width as f32, metrics.cell_height as f32) / scale;
    let padding = host.prepared[&hovered].key.options.padding;
    let origin = host.rects[&hovered].min + Vec2::new(padding[0], padding[1]) / scale;
    app.panes[&hovered].session.terminal()?.feed(
        "\x1b[H\x1b[2J\x1b]8;;https://target.example\x07go你\x1b]8;;\x07\r\nhttps://example.org"
            .as_bytes(),
    );
    // The final half of a wide glyph still belongs to the clickable label.
    host.mouse = origin + Vec2::new(3.5 * cell.x, 0.5 * cell.y);
    host.modifiers = ModifiersState::empty().into();
    app.update_hover_link(host);
    if host.hovered_link.is_some() || host.link_hit.is_some() {
        return Err("ordinary mouse hover started link detection".into());
    }
    host.modifiers = link_modifier.into();
    if !app.update_hover_link(host)
        || !host.hovered_link.as_ref().is_some_and(|link| {
            link.pane == hovered
                && link.uri == "https://target.example"
                && link.bounds.len() == 1
                && link.bounds[0].width() == 4.0 * cell.x
        })
        || app.pointer_cursor(host) != Some(CursorIcon::Pointer)
        || app.focused(host.id) != Some(focused)
    {
        return Err("Command-hover did not underline the hovered pane's OSC 8 link".into());
    }
    for offset in [3.6, 2.5, 1.5, 0.5] {
        host.mouse.x = origin.x + offset * cell.x;
        if app.update_hover_link(host) {
            return Err("moving within one link requested another redraw".into());
        }
    }
    host.mouse.y += cell.y;
    app.update_hover_link(host);
    if host
        .hovered_link
        .as_ref()
        .is_none_or(|link| link.uri != "https://example.org")
    {
        return Err("Command-hover did not recognize a plain URL".into());
    }
    host.modifiers = ModifiersState::empty().into();
    if !app.update_hover_link(host) || host.hovered_link.is_some() {
        return Err("releasing Command retained a link underline".into());
    }
    host.modifiers = link_modifier.into();
    app.update_hover_link(host);
    app.panes[&hovered]
        .session
        .terminal()?
        .feed(b"\x1b[?1000h\x1b[>1s");
    if !app.update_hover_link(host) || host.hovered_link.is_some() {
        return Err("mouse-captured text was advertised as Command-clickable".into());
    }
    host.modifiers = (link_modifier | ModifiersState::SHIFT).into();
    app.panes[&hovered].session.terminal()?.feed(b"\x1b[>0s");
    if !app.update_hover_link(host) || host.hovered_link.is_none() {
        return Err("Shift did not make the locally clickable URL discoverable".into());
    }
    app.panes[&hovered]
        .session
        .terminal()?
        .feed(b"\x1b[2;1H\x1b[2Kplain text");
    if !app.update_hover_link(host) || host.hovered_link.is_some() {
        return Err("changed terminal text retained a stale link target".into());
    }
    app.panes[&hovered].session.terminal()?.feed(b"\x1b[?1000l");
    host.modifiers = ModifiersState::empty().into();
    app.update_hover_link(host);
    eprintln!(
        "Native smoke: Command-hover links followed the pane, modifiers, wide cells, capture policy, and changed text without repeat redraws"
    );
    Ok(())
}

fn check_outliner(app: &mut App, event_loop: &ActiveEventLoop, directory: &Path) -> Result<()> {
    use outliner::Target;
    use winit::event::DeviceId;
    use winit::keyboard::KeyCode;

    let consumes_key_tail = |app: &mut App, key: KeyCode| {
        let key = PhysicalKey::Code(key);
        input::global_key_is_consumed(
            &mut app.consumed_global_keys,
            key,
            ElementState::Pressed,
            true,
        ) && input::global_key_is_consumed(
            &mut app.consumed_global_keys,
            key,
            ElementState::Released,
            false,
        ) && !input::global_key_is_consumed(
            &mut app.consumed_global_keys,
            key,
            ElementState::Pressed,
            false,
        )
    };

    if app.windows.is_empty() {
        app.add_window(false);
        app.reconcile(event_loop);
    }
    let key = *app.windows.keys().next().ok_or("missing Outliner host")?;
    let mut host = app.windows.remove(&key).unwrap();
    host.visible = true;
    host.occluded = false;
    host.focused = true;
    let window = host.id;
    let first = app.focused(window).ok_or("missing Outliner pane")?;
    let first_tab = app.tab(window).unwrap().id;
    app.panes[&first]
        .session
        .terminal()?
        .feed(b"\x1b]11;#123456\x07");
    for direction in [Direction::Right, Direction::Down] {
        app.action(event_loop, &mut host, Action::NewSplit(direction), true);
        let split = app.focused(window).unwrap();
        if app.panes[&split].session.terminal()?.background != [0x12, 0x34, 0x56] {
            return Err("split did not inherit the source pane's application background".into());
        }
        app.draw(event_loop, &mut host)?;
        if host.prepared[&split].key.options.background != [0x12, 0x34, 0x56] {
            return Err("split rendered the theme background before shell initialization".into());
        }
    }
    let bottom_right = app.focused(window).unwrap();
    let right = app.tab(window).unwrap().root.panes()[1];
    app.focus_pane(window, first);
    app.action(
        event_loop,
        &mut host,
        Action::NewSplit(Direction::Down),
        true,
    );
    let bottom_left = app.focused(window).unwrap();
    app.focus_pane(window, first);
    app.action(
        event_loop,
        &mut host,
        Action::NewSplit(Direction::Right),
        true,
    );
    let sibling = app.focused(window).unwrap();
    app.focus_pane(window, right);
    app.action(
        event_loop,
        &mut host,
        Action::NewSplit(Direction::Right),
        true,
    );
    let mixed = app.focused(window).unwrap();
    let tab = app.tab_mut(window).unwrap();
    tab.title = Some("Rustty".into());
    tab.color = Some([121, 96, 205]);
    app.action(event_loop, &mut host, Action::NewTab, true);
    let second = app.focused(window).unwrap();
    if app.panes[&second]
        .session
        .terminal()?
        .color_override(vt::color::Target::Background)
        .is_some()
    {
        return Err("new tab inherited a split's application background".into());
    }
    let second_tab = app.tab(window).unwrap().id;
    app.action(
        event_loop,
        &mut host,
        Action::NewSplit(Direction::Right),
        true,
    );
    let second_last = app.focused(window).unwrap();
    let tab = app.tab_mut(window).unwrap();
    tab.title = Some("Research".into());
    tab.color = Some([48, 149, 125]);
    tab.toggle_zoom();
    for (pane, label) in [
        (first, "rustty"),
        (sibling, "rustty"),
        (right, "tests"),
        (mixed, "docs"),
        (bottom_left, "build"),
        (bottom_right, "reviews"),
        (second, "notes"),
        (second_last, "reference"),
    ] {
        let pane = app.panes.get_mut(&pane).unwrap();
        pane.cwd = fixture_directory().join(label);
        pane.title = label.into();
        pane.session.terminal()?.feed(
            format!("\x1b[2J\x1b[H\x1b[1;36m{label}\x1b[0m\r\nOutliner navigation fixture\r\n")
                .as_bytes(),
        );
    }
    app.windows.insert(key, host);
    let action = |app: &mut App, action| {
        let mut host = app.windows.remove(&key).unwrap();
        app.action(event_loop, &mut host, action, true);
        app.windows.insert(key, host);
    };
    let capture_terminal = |app: &mut App, name: &str| -> Result<()> {
        let mut host = app.windows.remove(&key).unwrap();
        let capture = capture_window(app, event_loop, &mut host, &directory.join(name));
        let header_height = host.content.y;
        app.windows.insert(key, host);
        capture?;
        if header_height != 38.0 {
            return Err("breadcrumb consumed space below the single tab bar".into());
        }
        Ok(())
    };
    app.select_outline_target(Target::Pane {
        tab: first_tab,
        pane: first,
    });
    action(app, Action::ToggleOutliner);
    let utility_key = app
        .outliner
        .as_ref()
        .ok_or("Outliner window did not open")?
        .window
        .id();
    if app.windows.contains_key(&utility_key) || !app.outliner.as_ref().unwrap().visible {
        return Err("Outliner is not an independent visible native window".into());
    }
    if app
        .outline_rows()
        .iter()
        .any(|row| matches!(row.target, Target::Window(_)))
    {
        return Err("Outliner retained a redundant single-window level".into());
    }
    let quad = app.tab(window).unwrap().root.quadrant(first).unwrap();
    app.draw_outliner(event_loop, Some(&directory.join("outliner-collapsed.png")))?;
    let widget_bounds = |app: &App, id| {
        app.context
            .viewport_for(app.outliner.as_ref().unwrap().viewport, |state| {
                state
                    .prev_pass
                    .widgets
                    .get(id)
                    .map(|widget| widget.interact_rect)
            })
    };
    let disclosure = |target| egui::Id::new(("outliner-disclosure", target));
    for _ in 0..2 {
        app.draw_outliner(event_loop, Some(&directory.join("outliner-collapsed.png")))?;
    }
    if widget_bounds(
        app,
        disclosure(Target::Quad {
            tab: first_tab,
            node: quad,
        }),
    )
    .is_none()
        || widget_bounds(
            app,
            egui::Id::new((
                "outliner-row",
                Target::Pane {
                    tab: first_tab,
                    pane: first,
                },
            )),
        )
        .is_some()
        || widget_bounds(
            app,
            egui::Id::new((
                "outliner-row",
                Target::Pane {
                    tab: second_tab,
                    pane: second,
                },
            )),
        )
        .is_none()
    {
        return Err("Outliner did not initially show quads or ungrouped panels".into());
    }
    for pane in [sibling, first] {
        app.select_outline_target(Target::Pane {
            tab: first_tab,
            pane,
        });
        for _ in 0..2 {
            app.draw_outliner(event_loop, Some(&directory.join("outliner-collapsed.png")))?;
        }
        if widget_bounds(
            app,
            egui::Id::new((
                "outliner-row",
                Target::Pane {
                    tab: first_tab,
                    pane,
                },
            )),
        )
        .is_some()
        {
            return Err("Following Outliner focus expanded a collapsed quad".into());
        }
    }
    for (shift, open, name) in [
        (false, false, "outliner-collapsed.png"),
        (false, true, "outliner.png"),
        (true, false, "outliner-collapsed.png"),
        (true, true, "outliner.png"),
    ] {
        let before = (serde_json::to_vec(&app.workspace)?, app.active);
        let point = widget_bounds(app, disclosure(Target::Tab(first_tab)))
            .ok_or("missing native Outliner disclosure")?
            .center();
        let modifiers = egui::Modifiers {
            shift,
            ..Default::default()
        };
        for event in [
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers,
            },
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers,
            },
        ] {
            let input = app.outliner.as_mut().unwrap().egui.egui_input_mut();
            input.events.push(egui::Event::ModifiersChanged(modifiers));
            input.events.push(event);
            app.draw_outliner(event_loop, Some(&directory.join(name)))?;
        }
        app.outliner
            .as_mut()
            .unwrap()
            .egui
            .egui_input_mut()
            .events
            .push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        // Retire the previous pass's hit rectangles before checking hidden descendants.
        for _ in 0..2 {
            app.draw_outliner(event_loop, Some(&directory.join(name)))?;
        }
        if (serde_json::to_vec(&app.workspace)?, app.active) != before {
            return Err("Outliner disclosure changed terminal focus or zoom".into());
        }
        if widget_bounds(
            app,
            disclosure(Target::Quad {
                tab: first_tab,
                node: quad,
            }),
        )
        .is_some()
            != open
        {
            return Err("Outliner disclosure did not show or hide its children".into());
        }
    }
    if widget_bounds(
        app,
        egui::Id::new((
            "outliner-row",
            Target::Pane {
                tab: first_tab,
                pane: first,
            },
        )),
    )
    .is_none()
    {
        return Err("Shift-expand did not reveal nested Outliner branches".into());
    }
    capture_terminal(app, "outliner-terminal.png")?;
    for (command, expected, name) in [
        (Action::ToggleQuadrantZoom, "Quad-zoom", "outliner-quad.png"),
        (
            Action::ToggleSplitZoom,
            "Zoom within quad-zoom",
            "outliner-nested.png",
        ),
    ] {
        action(app, command);
        if zoom_label(app.tab(window).unwrap()) != Some(expected) {
            return Err("Outliner zoom fixture lost its zoom layer".into());
        }
        app.draw_outliner(event_loop, Some(&directory.join(name)))?;
        capture_terminal(app, &name.replace(".png", "-terminal.png"))?;
    }
    for ancestor in [
        Target::Window(window),
        Target::Tab(first_tab),
        Target::Quad {
            tab: first_tab,
            node: quad,
        },
    ] {
        app.select_outline_target(ancestor);
        let tab = app.tab(window).unwrap();
        if tab.focused != first || tab.zoom != Some(first) || tab.quadrant_zoom != Some(quad) {
            return Err("selecting an Outliner ancestor disturbed nested zoom".into());
        }
    }
    app.select_outline_target(Target::Pane {
        tab: first_tab,
        pane: sibling,
    });
    if app.focused(window) != Some(sibling)
        || zoom_label(app.tab(window).unwrap()) != Some("Quad-zoom")
    {
        return Err("Outliner did not reveal the sibling hidden by nested zoom".into());
    }
    action(app, Action::ToggleQuadrantZoom);
    action(app, Action::ToggleSplitZoom);
    if zoom_label(app.tab(window).unwrap()) != Some("Zoom") {
        return Err("Outliner pane zoom was not distinct".into());
    }
    app.draw_outliner(event_loop, Some(&directory.join("outliner-pane.png")))?;
    capture_terminal(app, "outliner-pane-terminal.png")?;
    let original_size = app.windows[&key].window.inner_size();
    let _ = app.windows[&key]
        .window
        .request_inner_size(LogicalSize::new(360.0, 500.0));
    let narrow_size = app.windows[&key].window.inner_size();
    if narrow_size
        .to_logical::<f64>(app.windows[&key].window.scale_factor())
        .width
        > 380.0
    {
        return Err("native window did not resize for the narrow tab-bar check".into());
    }
    app.window_event(event_loop, key, WindowEvent::Resized(narrow_size));
    capture_terminal(app, "outliner-narrow-terminal.png")?;
    let _ = app.windows[&key].window.request_inner_size(original_size);
    let restored_size = app.windows[&key].window.inner_size();
    app.window_event(event_loop, key, WindowEvent::Resized(restored_size));
    app.outliner
        .as_mut()
        .unwrap()
        .held_keys
        .insert(PhysicalKey::Code(KeyCode::Enter));
    app.select_outline_target(Target::Tab(second_tab));
    if !consumes_key_tail(app, KeyCode::Enter) {
        return Err("Outliner selection leaked its activation key into the terminal".into());
    }
    if app.focused(window) != Some(second) || app.tab(window).unwrap().zoom.is_some() {
        return Err("selecting another Outliner tab did not reveal its first pane".into());
    }

    for (tab, pane) in [(first_tab, first), (second_tab, second)] {
        app.select_outline_target(Target::Pane { tab, pane });
        action(app, Action::TogglePin);
    }
    if app.workspace.pinned_panes != [first, second] {
        return Err("Toggle Pin did not append focused panels in order".into());
    }
    let pin_shortcut = |app: &mut App, slot: usize| -> Result<()> {
        let binding = config::KeyBinding::parse(&format!("super+shift+{slot}=goto_pin:{slot}"))?;
        let mut host = app.windows.remove(&key).unwrap();
        let performed = app.host_binding(event_loop, &mut host, binding);
        app.windows.insert(key, host);
        if !performed {
            return Err("pin shortcut did not select an existing pin".into());
        }
        Ok(())
    };
    pin_shortcut(app, 1)?;
    if app.focused(window) != Some(first) {
        return Err("pin 1 did not focus its panel in another tab".into());
    }
    pin_shortcut(app, 2)?;
    if app.focused(window) != Some(second) {
        return Err("pin 2 did not focus its panel in another tab".into());
    }
    action(app, Action::StartSearch);
    capture_terminal(app, "outliner-pin-find.png")?;
    if app.windows[&key].search_focus != Some(second) {
        return Err("pin focus check did not open terminal Find".into());
    }
    pin_shortcut(app, 2)?;
    capture_terminal(app, "outliner-pin-find.png")?;
    if app.windows[&key].ui_input() || app.windows[&key].egui.egui_ctx().text_edit_focused() {
        return Err("activating the current pin left terminal Find focused".into());
    }
    action(app, Action::EndSearch);
    let before = (serde_json::to_vec(&app.workspace)?, app.active);
    if app.platform_action(event_loop, Action::GotoPin(9), false)
        || (serde_json::to_vec(&app.workspace)?, app.active) != before
    {
        return Err("a missing pin shortcut changed focus or layout".into());
    }
    app.select_outline_target(Target::Pane {
        tab: first_tab,
        pane: first,
    });
    action(app, Action::ToggleQuadrantZoom);
    action(app, Action::ToggleSplitZoom);
    app.window_event(event_loop, utility_key, WindowEvent::Focused(true));
    for _ in 0..2 {
        app.draw_outliner(event_loop, Some(&directory.join("outliner-pinned.png")))?;
    }
    capture_terminal(app, "outliner-pinned-terminal.png")?;
    let before = (serde_json::to_vec(&app.workspace.windows)?, app.active);
    let from = widget_bounds(app, egui::Id::new(("outliner-pin-number", second)))
        .ok_or("missing Outliner pin drag handle")?
        .center();
    let target = widget_bounds(app, egui::Id::new(("outliner-pin-drop", first)))
        .ok_or("missing Outliner pin drop target")?;
    let to = Pos2::new(target.center().x, target.top() + 2.0);
    let utility_scale = app.outliner.as_ref().unwrap().window.scale_factor();
    let pointer_move = |point: Pos2| WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: LogicalPosition::new(point.x, point.y).to_physical(utility_scale),
    };
    let pointer_button = |state| WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    };
    for event in [
        pointer_move(from),
        pointer_button(ElementState::Pressed),
        pointer_move(from - Vec2::new(0.0, 10.0)),
        pointer_move(to),
        pointer_button(ElementState::Released),
    ] {
        app.window_event(event_loop, utility_key, event);
        app.draw_outliner(event_loop, Some(&directory.join("outliner-pinned.png")))?;
    }
    if app.workspace.pinned_panes != [second, first] {
        return Err("dragging Outliner pins did not reorder them".into());
    }
    if (serde_json::to_vec(&app.workspace.windows)?, app.active) != before {
        return Err("reordering Outliner pins changed terminal focus or zoom".into());
    }
    pin_shortcut(app, 1)?;
    if app.focused(window) != Some(second) {
        return Err("pin shortcut did not follow the reordered list".into());
    }
    for _ in 0..2 {
        app.draw_outliner(event_loop, Some(&directory.join("outliner-pinned.png")))?;
    }
    let target = widget_bounds(app, egui::Id::new(("outliner-pin-drop", second)))
        .ok_or("reordered Outliner pin disappeared")?;
    let point = Pos2::new(target.right() - 12.0, target.center().y);
    for event in [
        pointer_move(point),
        pointer_button(ElementState::Pressed),
        pointer_button(ElementState::Released),
    ] {
        app.window_event(event_loop, utility_key, event);
        app.draw_outliner(event_loop, Some(&directory.join("outliner-pinned.png")))?;
    }
    if app.focused(window) != Some(second) || app.active != Some(window) {
        return Err("clicking an Outliner pin did not reveal its panel in another tab".into());
    }
    let before = (serde_json::to_vec(&app.workspace.windows)?, app.active);
    let point = widget_bounds(app, egui::Id::new(("outliner-pin", second)))
        .ok_or("missing Outliner pin button")?
        .center();
    for event in [
        pointer_move(point),
        pointer_button(ElementState::Pressed),
        pointer_button(ElementState::Released),
    ] {
        app.window_event(event_loop, utility_key, event);
        app.draw_outliner(event_loop, Some(&directory.join("outliner-pinned.png")))?;
    }
    if app.workspace.pinned_panes != [first] {
        return Err("clicking the Outliner pin button did not unpin the panel".into());
    }
    if (serde_json::to_vec(&app.workspace.windows)?, app.active) != before {
        return Err("unpinning from Outliner changed terminal focus or zoom".into());
    }

    let begin_search = |app: &mut App| -> Result<()> {
        if !app.background_smoke {
            app.outliner.as_ref().unwrap().window.focus_window();
        }
        app.window_event(event_loop, utility_key, WindowEvent::Focused(true));
        let host = app.outliner.as_mut().unwrap();
        host.search.start();
        host.egui.egui_input_mut().focused = true;
        host.egui
            .egui_input_mut()
            .events
            .push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        app.draw_outliner(
            event_loop,
            Some(&directory.join("outliner-search-empty.png")),
        )?;
        Ok(())
    };
    let type_query = |app: &mut App, query: &str| -> Result<()> {
        app.outliner
            .as_mut()
            .unwrap()
            .egui
            .egui_input_mut()
            .events
            .push(egui::Event::Text(query.into()));
        for _ in 0..2 {
            app.draw_outliner(event_loop, Some(&directory.join("outliner-search.png")))?;
        }
        Ok(())
    };
    let accept_search = |app: &mut App| -> Result<()> {
        for pressed in [true, false] {
            app.outliner
                .as_mut()
                .unwrap()
                .egui
                .egui_input_mut()
                .events
                .push(egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            app.draw_outliner(event_loop, None)?;
        }
        Ok(())
    };
    for (key, physical, modifiers) in [
        (egui::Key::Enter, KeyCode::Enter, egui::Modifiers::NONE),
        (egui::Key::Escape, KeyCode::Escape, egui::Modifiers::NONE),
        (
            egui::Key::OpenBracket,
            KeyCode::BracketLeft,
            egui::Modifiers::CTRL,
        ),
    ] {
        app.select_outline_target(Target::Pane {
            tab: first_tab,
            pane: first,
        });
        begin_search(app)?;
        if !app.outliner.as_ref().unwrap().search.query.is_empty()
            || widget_bounds(app, egui::Id::new(("outliner-row", Target::Tab(first_tab)))).is_none()
        {
            return Err("empty Outliner search did not retain the tree".into());
        }
        let field = widget_bounds(app, egui::Id::new("outliner-search-query"))
            .ok_or("missing Outliner search field")?;
        if app.outline_rows().iter().any(|row| {
            widget_bounds(app, egui::Id::new(("outliner-row", row.target)))
                .is_some_and(|rect| rect.is_positive() && rect.bottom() > field.top())
        }) {
            return Err("Outliner tree extended behind the search field".into());
        }
        type_query(app, "nts")?;
        if app.focused(window) != Some(first) {
            return Err("typing an Outliner query changed terminal focus".into());
        }
        app.outliner
            .as_mut()
            .unwrap()
            .egui
            .egui_input_mut()
            .events
            .push(egui::Event::Key {
                key: egui::Key::N,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL,
            });
        app.draw_outliner(event_loop, None)?;
        app.outliner
            .as_mut()
            .unwrap()
            .held_keys
            .insert(PhysicalKey::Code(physical));
        for pressed in [true, false] {
            app.outliner
                .as_mut()
                .unwrap()
                .egui
                .egui_input_mut()
                .events
                .push(egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers,
                });
            app.draw_outliner(event_loop, None)?;
        }
        let host = app.outliner.as_ref().unwrap();
        if host.search.open || !host.visible || app.focused(window) != Some(second) {
            return Err(
                "Outliner search did not focus its first result and exit without hiding".into(),
            );
        }
        if !consumes_key_tail(app, physical) {
            return Err("Outliner search leaked its activation key into the terminal".into());
        }
    }
    begin_search(app)?;
    type_query(app, "rty")?;
    let native_focus_available = !app.background_smoke;
    if !native_focus_available {
        eprintln!(
            "Native Outliner smoke: checking internal input isolation; native focus and minimization skipped"
        );
    } else if !app.outliner.as_ref().unwrap().window.has_focus() {
        return Err("Outliner could not acquire native focus for the surface smoke test".into());
    }
    for (key, physical, repeat, expected) in [
        (egui::Key::N, KeyCode::KeyN, false, first),
        (egui::Key::N, KeyCode::KeyN, true, sibling),
        (egui::Key::N, KeyCode::KeyN, true, first),
        (egui::Key::P, KeyCode::KeyP, false, sibling),
        (egui::Key::P, KeyCode::KeyP, true, first),
        (egui::Key::N, KeyCode::KeyN, false, sibling),
    ] {
        let host = app.outliner.as_mut().unwrap();
        host.held_keys.insert(PhysicalKey::Code(physical));
        host.egui.egui_input_mut().events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers: egui::Modifiers::CTRL,
        });
        app.draw_outliner(
            event_loop,
            Some(&directory.join("outliner-search-navigation.png")),
        )?;
        let host = app.outliner.as_ref().unwrap();
        if app.focused(window) != Some(expected)
            || !host.search.open
            || host.search.query != "rty"
            || native_focus_available && !host.window.has_focus()
            || !host.held_keys.contains(&PhysicalKey::Code(physical))
            || app
                .consumed_global_keys
                .contains(&PhysicalKey::Code(physical))
        {
            return Err("Outliner match navigation failed to preview a panel while retaining keyboard focus".into());
        }
    }
    accept_search(app)?;
    if app.outliner.as_ref().unwrap().search.open
        || app.focused(window) != Some(sibling)
        || !consumes_key_tail(app, KeyCode::KeyN)
        || !consumes_key_tail(app, KeyCode::KeyP)
    {
        return Err(
            "Outliner Enter did not accept the navigated match and isolate held keys".into(),
        );
    }
    begin_search(app)?;
    type_query(app, "rty")?;
    let point = widget_bounds(app, egui::Id::new(("outliner-search-result", sibling)))
        .ok_or("missing Outliner fuzzy result")?
        .center();
    for event in [
        pointer_move(point),
        pointer_button(ElementState::Pressed),
        pointer_button(ElementState::Released),
    ] {
        app.window_event(event_loop, utility_key, event);
        app.draw_outliner(event_loop, None)?;
    }
    if app.focused(window) != Some(sibling)
        || !app.outliner.as_ref().unwrap().search.open
        || app.outliner.as_ref().unwrap().search.query != "rty"
    {
        return Err("clicking an Outliner search result did not preserve search mode".into());
    }
    app.window_event(event_loop, utility_key, WindowEvent::Focused(false));
    app.panes
        .get_mut(&sibling)
        .unwrap()
        .input
        .push_back(SessionInput::Text(Vec::new()));
    app.window_event(
        event_loop,
        key,
        WindowEvent::Ime(Ime::Commit("search result typing".into())),
    );
    if queued_input(&app.panes[&sibling])?
        .last()
        .map(Vec::as_slice)
        != Some(b"search result typing".as_slice())
        || app.outliner.as_ref().unwrap().search.query != "rty"
    {
        return Err("an unfocused Outliner search captured terminal typing".into());
    }
    let pane = app.panes.get_mut(&sibling).unwrap();
    pane.input.clear();
    pane.input_bytes = 0;
    app.window_event(event_loop, utility_key, WindowEvent::Focused(true));
    app.draw_outliner(event_loop, None)?;
    type_query(app, "s")?;
    if app.outliner.as_ref().unwrap().search.query != "rtys" {
        return Err("returning to Outliner search did not restore editor focus".into());
    }
    app.platform_action(event_loop, Action::SelectAll, false);
    type_query(app, "nts")?;
    if app.outliner.as_ref().unwrap().search.query != "nts" {
        return Err("native Select All did not edit the Outliner search query".into());
    }
    let point = widget_bounds(app, egui::Id::new("outliner-search-close"))
        .ok_or("missing Outliner search close button")?
        .center();
    for event in [
        pointer_move(point),
        pointer_button(ElementState::Pressed),
        pointer_button(ElementState::Released),
    ] {
        app.window_event(event_loop, utility_key, event);
        app.draw_outliner(event_loop, None)?;
    }
    if app.outliner.as_ref().unwrap().search.open || app.focused(window) != Some(sibling) {
        return Err("closing Outliner search changed panel focus or left search open".into());
    }

    let other = app.add_window(false);
    app.reconcile(event_loop);
    app.tab_mut(other).unwrap().title = Some("Notes".into());
    let other_key = app
        .windows
        .iter()
        .find_map(|(&key, host)| (host.id == other).then_some(key))
        .unwrap();
    let other_pane = app.focused(other).unwrap();
    let navigate_search = |app: &mut App| -> Result<()> {
        app.outliner
            .as_mut()
            .unwrap()
            .egui
            .egui_input_mut()
            .events
            .push(egui::Event::Key {
                key: egui::Key::N,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL,
            });
        app.draw_outliner(event_loop, None)
    };
    app.select_outline_target(Target::Window(window));
    app.select_outline_target(Target::Window(other));
    if app.active != Some(other)
        || !app.windows[&other_key].visible
        || app.focused(other) != Some(other_pane)
    {
        return Err("Outliner did not select and reveal another terminal window".into());
    }
    let mut other_host = app.windows.remove(&other_key).unwrap();
    other_host.visible = true;
    other_host.occluded = false;
    app.search_action(&mut other_host, other_pane, Action::StartSearch);
    capture_window(
        app,
        event_loop,
        &mut other_host,
        &directory.join("outliner-other-window.png"),
    )?;
    let scale = other_host.window.scale_factor();
    let point = Pos2::new(40.0, 80.0);
    app.windows.insert(other_key, other_host);
    app.panes.get_mut(&other_pane).unwrap().cwd = fixture_directory().join("remote-search-target");
    app.select_outline_target(Target::Window(window));
    if !app.background_smoke {
        app.windows[&other_key].window.set_minimized(true);
    }
    app.workspace.toggle_pin(other_pane);
    app.outliner.as_mut().unwrap().search.start();
    pin_shortcut(app, 2)?;
    let mut other_host = app.windows.remove(&other_key).unwrap();
    capture_window(
        app,
        event_loop,
        &mut other_host,
        &directory.join("outliner-other-window.png"),
    )?;
    app.windows.insert(other_key, other_host);
    if app.active != Some(other)
        || app.focused(other) != Some(other_pane)
        || native_focus_available && app.windows[&other_key].window.is_minimized() == Some(true)
        || app.windows[&other_key].ui_input()
        || app.windows[&other_key].egui.egui_ctx().text_edit_focused()
    {
        return Err("pin shortcut did not focus its terminal in a minimized window".into());
    }
    app.platform_action(event_loop, Action::EndSearch, false);
    app.outliner
        .as_mut()
        .unwrap()
        .held_keys
        .insert(PhysicalKey::Code(KeyCode::Digit1));
    app.platform_binding(
        event_loop,
        config::KeyBinding::parse("super+shift+1=goto_pin:1")?,
    );
    if app.active != Some(window)
        || app.focused(window) != Some(first)
        || !consumes_key_tail(app, KeyCode::Digit1)
    {
        return Err(
            "pin shortcut from Outliner search failed to focus its panel and consume the held key"
                .into(),
        );
    }
    app.workspace.toggle_pin(other_pane);
    if !app.background_smoke {
        app.windows[&other_key].window.set_minimized(true);
    }
    begin_search(app)?;
    type_query(app, "remote-search")?;
    navigate_search(app)?;
    if app.active != Some(other)
        || !app.windows[&other_key].visible
        || native_focus_available && app.windows[&other_key].window.is_minimized() == Some(true)
        || native_focus_available && !app.outliner.as_ref().unwrap().window.has_focus()
    {
        return Err(
            "Outliner search did not reveal a minimized window without taking keyboard focus"
                .into(),
        );
    }
    accept_search(app)?;
    app.draw_outliner(event_loop, Some(&directory.join("outliner-windows.png")))?;
    let layout = serde_json::to_vec(&app.workspace)?;
    let terminal_size = app.windows[&other_key].window.inner_size();
    let utility = &app.outliner.as_ref().unwrap().window;
    if !utility.is_resizable() {
        return Err("Outliner native window cannot be resized independently".into());
    }
    utility.set_outer_position(LogicalPosition::new(80.0, 120.0));
    let _ = utility.request_inner_size(LogicalSize::new(420.0, 720.0));
    app.window_event(
        event_loop,
        utility_key,
        WindowEvent::Moved(winit::dpi::PhysicalPosition::new(80, 120)),
    );
    app.window_event(
        event_loop,
        utility_key,
        WindowEvent::Resized(winit::dpi::PhysicalSize::new(420, 720)),
    );
    app.window_event(event_loop, utility_key, WindowEvent::Focused(true));
    if serde_json::to_vec(&app.workspace)? != layout
        || app.windows[&other_key].window.inner_size() != terminal_size
        || app.active != Some(other)
    {
        return Err("moving, resizing, or focusing Outliner changed terminal state".into());
    }
    app.outliner
        .as_mut()
        .unwrap()
        .held_keys
        .insert(PhysicalKey::Code(KeyCode::Space));
    app.window_event(event_loop, utility_key, WindowEvent::Focused(false));
    if !consumes_key_tail(app, KeyCode::Space) {
        return Err("leaving Outliner leaked a held key into the terminal".into());
    }
    let pane = app.panes.get_mut(&other_pane).unwrap();
    pane.session.terminal()?.feed(b"\x1b[?1000h\x1b[?1006h");
    // Observe reports without sending test input to the child process.
    pane.input.push_back(SessionInput::Text(Vec::new()));
    app.window_event(
        event_loop,
        other_key,
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: LogicalPosition::new(point.x, point.y).to_physical(scale),
        },
    );
    for state in [ElementState::Pressed, ElementState::Released] {
        app.window_event(
            event_loop,
            other_key,
            WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state,
                button: MouseButton::Left,
            },
        );
    }
    let reports = queued_input(&app.panes[&other_pane])?;
    if reports.len() != 3
        || !reports[1].starts_with(b"\x1b[<0;")
        || !reports[1].ends_with(b"M")
        || !reports[2].starts_with(b"\x1b[<0;")
        || !reports[2].ends_with(b"m")
    {
        return Err("Outliner prevented ordinary terminal mouse input".into());
    }
    app.window_event(
        event_loop,
        other_key,
        WindowEvent::Ime(Ime::Commit("outliner typing".into())),
    );
    if queued_input(&app.panes[&other_pane])?
        .last()
        .map(Vec::as_slice)
        != Some(b"outliner typing".as_slice())
    {
        return Err("Outliner blocked terminal text input".into());
    }
    let pane = app.panes.get_mut(&other_pane).unwrap();
    pane.input.clear();
    pane.input_bytes = 0;
    pane.session.terminal()?.feed(b"\x1b[?1000l\x1b[?1006l");

    let quick = app.add_window(true);
    app.reconcile(event_loop);
    let quick_key = app
        .windows
        .iter()
        .find_map(|(&key, host)| (host.id == quick).then_some(key))
        .unwrap();
    let quick_listed = |app: &App| {
        app.outline_rows()
            .iter()
            .any(|row| row.target == Target::Window(quick))
    };
    if quick_listed(app) || app.windows[&quick_key].quick_used {
        return Err("unused quick terminal appeared in Outliner".into());
    }
    let mut quick_host = app.windows.remove(&quick_key).unwrap();
    app.quick_visible(&mut quick_host, true, false);
    app.windows.insert(quick_key, quick_host);
    if !quick_listed(app) || !app.windows[&quick_key].quick_used {
        return Err("first quick-terminal use did not add it to Outliner".into());
    }
    let mut quick_host = app.windows.remove(&quick_key).unwrap();
    app.quick_visible(&mut quick_host, false, false);
    app.windows.insert(quick_key, quick_host);
    if !quick_listed(app) {
        return Err("hiding the used quick terminal removed it from Outliner".into());
    }
    let quick_pane = app.focused(quick).unwrap();
    app.panes.get_mut(&quick_pane).unwrap().cwd = fixture_directory().join("quick-search-target");
    begin_search(app)?;
    type_query(app, "quick-search")?;
    navigate_search(app)?;
    if !app.windows[&quick_key].visible
        || app.active != Some(quick)
        || native_focus_available && !app.outliner.as_ref().unwrap().window.has_focus()
    {
        return Err("Outliner search did not preview the hidden quick terminal while retaining keyboard focus".into());
    }
    app.platform_action(event_loop, Action::SelectAll, false);
    type_query(app, "remote-search")?;
    navigate_search(app)?;
    if app.active != Some(other)
        || app.config().quick_terminal_autohide && app.windows[&quick_key].visible
        || native_focus_available && !app.outliner.as_ref().unwrap().window.has_focus()
    {
        return Err(format!(
            "leaving a quick-terminal preview failed: active={:?}, expected={other}, quick_visible={}, query={:?}",
            app.active,
            app.windows[&quick_key].visible,
            app.outliner.as_ref().unwrap().search.query,
        ).into());
    }
    accept_search(app)?;
    app.select_outline_target(Target::Window(quick));
    if !app.windows[&quick_key].visible || app.active != Some(quick) {
        return Err("Outliner did not reveal the hidden quick terminal".into());
    }
    app.draw_outliner(event_loop, Some(&directory.join("outliner-quick.png")))?;
    app.select_outline_target(Target::Tab(second_tab));
    let index = app.index(window).unwrap();
    app.workspace.windows[index].tabs.remove(0);
    app.workspace.windows[index].active_tab = 0;
    app.draw_outliner(event_loop, Some(&directory.join("outliner-flat.png")))?;
    let layout = serde_json::to_vec(&app.workspace)?;
    let terminals = app.panes.len();
    app.outliner
        .as_mut()
        .unwrap()
        .held_keys
        .insert(PhysicalKey::Code(KeyCode::Escape));
    app.window_event(event_loop, utility_key, WindowEvent::CloseRequested);
    if app.outliner.as_ref().is_some_and(|host| host.visible)
        || serde_json::to_vec(&app.workspace)? != layout
        || app.panes.len() != terminals
    {
        return Err("closing Outliner changed terminal windows or panes".into());
    }
    if !consumes_key_tail(app, KeyCode::Escape) {
        return Err("closing Outliner leaked a held key into the terminal".into());
    }
    fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "native_search_focus_checked": native_focus_available,
            "native_minimization_checked": native_focus_available,
            "background": app.background_smoke,
            "checks": ["outliner-native-window", "outliner-hierarchy", "outliner-disclosure", "outliner-navigation", "outliner-zoom", "outliner-single-tab-bar", "outliner-narrow-tab-bar", "outliner-pins", "outliner-pin-drag", "outliner-search", "outliner-search-navigation", "outliner-terminal-input", "outliner-quick-terminal", "outliner-close"],
            "captures": ["outliner.png", "outliner-collapsed.png", "outliner-terminal.png", "outliner-quad.png", "outliner-quad-terminal.png", "outliner-nested.png", "outliner-nested-terminal.png", "outliner-pane.png", "outliner-pane-terminal.png", "outliner-narrow-terminal.png", "outliner-pinned.png", "outliner-pinned-terminal.png", "outliner-search.png", "outliner-search-empty.png", "outliner-search-navigation.png", "outliner-windows.png", "outliner-other-window.png", "outliner-quick.png", "outliner-flat.png"]
        }))?,
    )?;
    eprintln!("Native Outliner smoke passed: {}", directory.display());
    Ok(())
}

/// Render the same host primitives to a texture when no drawable is available
/// (e.g. a locked CI Mac). This does not claim visible surface presentation.
fn capture_window(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    host: &mut Host,
    path: &Path,
) -> Result<()> {
    if let Some(state) = app.painter.render_state() {
        // Synchronous checks run while App.smoke is taken, so capture explicitly offscreen.
        let size = host.window.inner_size();
        let texture = offscreen_texture(&state, [size.width, size.height]);
        app.draw_frame(event_loop, host, Some(&texture))?;
        return save_texture(&state, &texture, path);
    }
    host.capture = true;
    app.draw(event_loop, host)?;
    let mut events = Vec::new();
    app.painter.handle_screenshots(&mut events);
    let image = events
        .into_iter()
        .find_map(|event| match event {
            egui::Event::Screenshot { image, .. } => Some(image),
            _ => None,
        })
        .ok_or("missing software screenshot")?;
    let mut encoder = png::Encoder::new(
        fs::File::create(path)?,
        image.width() as u32,
        image.height() as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(
        &image
            .pixels
            .iter()
            .flat_map(|color| color.to_array())
            .collect::<Vec<_>>(),
    )?;
    Ok(())
}

fn offscreen_texture(state: &egui_wgpu::RenderState, size: [u32; 2]) -> wgpu::Texture {
    state.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Rustty host capture"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: state.target_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn offscreen_commands(
    state: &egui_wgpu::RenderState,
    primitives: &[egui::ClippedPrimitive],
    delta: &egui::TexturesDelta,
    size: [u32; 2],
    scale: f32,
    texture: &wgpu::Texture,
) -> (wgpu::CommandEncoder, Vec<wgpu::CommandBuffer>) {
    let device = &state.device;
    let queue = &state.queue;
    let mut encoder = device.create_command_encoder(&Default::default());
    let descriptor = egui_wgpu::ScreenDescriptor {
        size_in_pixels: size,
        pixels_per_point: scale,
    };
    let mut renderer = state.renderer.write();
    for (id, deltas) in &delta.set {
        for delta in deltas {
            renderer.update_texture(device, queue, *id, delta);
        }
    }
    let commands = renderer.update_buffers(device, queue, &mut encoder, primitives, &descriptor);
    {
        let view = texture.create_view(&Default::default());
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Rustty host capture"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
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
        renderer.render(&mut pass.forget_lifetime(), primitives, &descriptor);
    }
    drop(renderer);
    (encoder, commands)
}

pub(super) fn submit_offscreen(
    state: &egui_wgpu::RenderState,
    primitives: &[egui::ClippedPrimitive],
    delta: &egui::TexturesDelta,
    size: [u32; 2],
    scale: f32,
    target: Option<&wgpu::Texture>,
) {
    let texture = target.is_none().then(|| offscreen_texture(state, size));
    let target = target.or(texture.as_ref()).unwrap();
    let (encoder, commands) = offscreen_commands(state, primitives, delta, size, scale, target);
    state
        .queue
        .submit(commands.into_iter().chain([encoder.finish()]));
    let mut renderer = state.renderer.write();
    for id in &delta.free {
        renderer.free_texture(id);
    }
}

pub(super) fn capture(
    state: &egui_wgpu::RenderState,
    primitives: &[egui::ClippedPrimitive],
    delta: &egui::TexturesDelta,
    size: [u32; 2],
    scale: f32,
    path: &Path,
) -> Result<()> {
    let texture = offscreen_texture(state, size);
    let (encoder, commands) = offscreen_commands(state, primitives, delta, size, scale, &texture);
    state
        .queue
        .submit(commands.into_iter().chain([encoder.finish()]));
    save_texture(state, &texture, path)
}

fn save_texture(
    state: &egui_wgpu::RenderState,
    texture: &wgpu::Texture,
    path: &Path,
) -> Result<()> {
    let device = &state.device;
    let queue = &state.queue;
    let size = [texture.width(), texture.height()];
    let stride = (size[0] * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Rustty host readback"),
        size: u64::from(stride) * u64::from(size[1]),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Rustty host readback"),
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: None,
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    // The channel timeout cannot protect an unbounded wait on the GPU.
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: Some(Duration::from_secs(5)),
    })?;
    rx.recv_timeout(Duration::from_secs(5))??;
    let mapped = buffer.slice(..).get_mapped_range()?;
    let mut pixels = Vec::with_capacity((size[0] * size[1] * 4) as usize);
    for row in mapped.chunks_exact(stride as usize) {
        for pixel in row[..size[0] as usize * 4].as_chunks::<4>().0 {
            if state.target_format == wgpu::TextureFormat::Bgra8Unorm {
                pixels.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
            } else {
                pixels.extend_from_slice(pixel);
            }
        }
    }
    drop(mapped);
    buffer.unmap();
    let mut encoder = png::Encoder::new(fs::File::create(path)?, size[0], size[1]);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&pixels)?;
    Ok(())
}
