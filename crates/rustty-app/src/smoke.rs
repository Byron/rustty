//! Opt-in native integration check; all shells and saved state are test-owned.
use super::*;
use std::fs;

#[path = "../../../test/rustty/frame_workloads.rs"]
mod workload;

#[cfg(test)]
#[test]
fn hover_measurement_restarts_for_motion_and_leaving_but_not_duplicate_events() {
    let now = Instant::now();
    let mut smoke = Smoke {
        directory: PathBuf::new(),
        offscreen: false,
        hover: true,
        pointer: None,
        stage: 4,
        deadline: now + Duration::from_secs(45),
        next: now,
        original: None,
        closed: None,
        idle_frames: 0,
        progress_pane: None,
        progress_started: now,
        progress_frames: 0,
        progress_seconds: 0.0,
        hidden_title_frames: 0,
        header_frames: 0,
        header_prepares: 0,
        events: BTreeMap::new(),
        timing: None,
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
    hover: bool,
    pointer: Option<Pos2>,
    stage: u8,
    deadline: Instant,
    next: Instant,
    original: Option<Id>,
    closed: Option<(Id, Instant)>,
    idle_frames: u64,
    progress_pane: Option<Id>,
    progress_started: Instant,
    progress_frames: u64,
    progress_seconds: f64,
    hidden_title_frames: u64,
    header_frames: u64,
    header_prepares: u64,
    events: BTreeMap<&'static str, u64>,
    timing: Option<Replay>,
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
            hover: std::env::var_os("RUSTTY_SMOKE_HOVER").is_some(),
            pointer: None,
            stage: 0,
            deadline: Instant::now() + Duration::from_secs(45),
            next: Instant::now(),
            original: None,
            closed: None,
            idle_frames: 0,
            progress_pane: None,
            progress_started: Instant::now(),
            progress_frames: 0,
            progress_seconds: 0.0,
            hidden_title_frames: 0,
            header_frames: 0,
            header_prepares: 0,
            events: BTreeMap::new(),
            timing,
        }))
    }
    pub(super) fn configure(loaded: &mut LoadedConfig) {
        let timing = std::env::var_os("RUSTTY_SMOKE_TIMING").is_some();
        if timing {
            loaded.config = Config::default();
            loaded.diagnostics.clear();
            loaded.config.font_family = vec!["Menlo".into()];
            loaded.config.font_size = 13.0;
        }
        let command = if timing {
            config::Command::Direct(vec!["/bin/sleep".into(), "120".into()])
        } else {
            config::Command::Direct(vec!["/bin/sh".into(),"-c".into(),r"printf '\033[2J\033[H\033[30;107m  ✔️\033[5G  > selected row\033[0m\n\033[1;36mRustty native smoke\033[0m\n\033]7;file://localhost/tmp\007\033]9;4;1;65\007'; exec /bin/sh -i".into()])
        };
        loaded.config.command = Some(command.clone());
        loaded.config.initial_command = Some(command);
        loaded.config.working_directory = Some(PathBuf::from("/tmp"));
        loaded.config.window_save_state = config::WindowSaveState::Always;
        loaded.config.cursor_style_blink = Some(false);
        loaded.config.grapheme_width_method = config::GraphemeWidthMethod::Unicode;
        loaded.config.mouse_shift_capture = config::MouseShiftCapture::False;
        loaded.config.link_url = true;
        loaded.config.progress_style = true;
        loaded.config.undo_timeout = Duration::from_secs(5);
        loaded.config.keybinds.retain(|b| !b.flags.global);
    }
    pub fn record(&mut self, event: &'static str) {
        if matches!(self.stage, 4 | 8) {
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
            self.next = Instant::now() + Duration::from_millis(1500);
        }
    }
    pub fn step(&mut self, app: &mut App, event_loop: &ActiveEventLoop) -> Result<bool> {
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
            let progress: Vec<_> = app
                .panes
                .iter()
                .map(|(&id, pane)| (id, pane.activity.progress()))
                .collect();
            return Err(format!(
                "native smoke timed out at stage {}: {:?}; windows: {:?}; pane progress: {:?}",
                self.stage, app.errors, windows, progress
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
        let mut host = app.windows.remove(&key).unwrap();
        let result = self.step_window(app, event_loop, &mut host);
        app.windows.insert(key, host);
        app.reconcile(event_loop);
        result
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
        let pane = app.focused(host.id).ok_or("no active pane")?;
        match self.stage {
            0 => {
                if host.frames == 0 || !text(app, pane).contains("Rustty native smoke") {
                    return Ok(false);
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
                app.action(event_loop, host, Action::NewTab, true);
                if app.panes.len() != 5 {
                    return Err("tab creation did not start a fifth PTY".into());
                }
                app.action(event_loop, host, Action::PreviousTab, true);
                eprintln!("Native smoke: splits, tabs, and quadrant actions passed");
                self.stage = 2;
            }
            2 => {
                if app
                    .panes
                    .keys()
                    .any(|&id| !text(app, id).contains("Rustty native smoke"))
                {
                    return Ok(false);
                }
                if app.panes.values().any(|p| p.cwd != Path::new("/tmp")) {
                    return Err("OSC directory was not decoded before restoration".into());
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
                    check_pointer_targets(app, host)?;
                    check_terminal_frames(app, event_loop, host)?;
                    check_find(app, event_loop, host, &self.directory)?;
                    self.idle_frames = host.frames;
                    self.progress_pane = Some(pane);
                    self.progress_started = Instant::now();
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
                let refresh_hz = host
                    .window
                    .current_monitor()
                    .and_then(|monitor| monitor.refresh_rate_millihertz())
                    .map(|rate| f64::from(rate) / 1000.0);
                let report = serde_json::json!({"passed":true,"capture_mode":if self.offscreen { "offscreen" } else { "surface" },"checks":["native-window","metal-wgpu-frame","pty-input-output","unicode-grapheme-width","four-splits","tab-creation","quadrant-focus-and-zoom","cwd-uri-decoding","osc-progress","progress-animation","hover-scrolling","alternate-scrolling","file-drop-targeting","osc-pointer","command-hover-links","reverse-video","dec-column-mode","text-blink","synchronized-output","per-pane-find","find-transparency","hidden-tab-titles","retained-pane-content","workspace-roundtrip","undo-keeps-pty","idle-rendering"],"frames":host.frames,"idle_frames":host.frames-self.idle_frames,"hidden_title_frames":self.hidden_title_frames,"header_updates":{"frames":self.header_frames,"pane_prepares":self.header_prepares},"progress_animation":{"frames":self.progress_frames,"seconds":self.progress_seconds,"fps":self.progress_frames as f64/self.progress_seconds,"monitor_refresh_hz":refresh_hz},"panes":app.panes.len(),"idle_phase_events":self.events,"hover_required":self.hover,"pointer":self.pointer.map(|position|[position.x,position.y])});
                fs::write(
                    self.directory.join("result.json"),
                    serde_json::to_vec_pretty(&report)?,
                )?;
                println!("Native smoke passed: {}", self.directory.display());
                return Ok(true);
            }
            5 => {
                let (closed, started) = self.closed.unwrap();
                if app.tab(host.id).unwrap().panes.contains_key(&closed)
                    || !app
                        .panes
                        .get(&closed)
                        .is_some_and(|pane| pane.started == started)
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
                    || !app
                        .panes
                        .get(&closed)
                        .is_some_and(|pane| pane.started == started)
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
                    return Err("hidden-tab titles did not update live and saved state".into());
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
                self.header_frames = host.frames.saturating_sub(self.header_frames);
                self.header_prepares = host.pane_prepares.saturating_sub(self.header_prepares);
                let window = &app.workspace.windows[app.index(host.id).unwrap()];
                let hidden = window
                    .tabs
                    .iter()
                    .enumerate()
                    .find(|(index, _)| *index != window.active_tab)
                    .unwrap()
                    .1;
                if app.panes[&hidden.focused].title != "visible-agent-19" || self.header_frames < 10
                {
                    return Err("visible tab-label updates stopped repainting".into());
                }
                // The focused pane can rebuild as its blink phase changes; the
                // other panes must retain their content throughout title updates.
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
            self.process_start = Some((Instant::now(), cpu_time(libc::CLOCK_PROCESS_CPUTIME_ID)?));
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
        let cpu_start = cpu_time(libc::CLOCK_THREAD_CPUTIME_ID)?;
        let frame_start = Instant::now();
        let before = host.frames;
        app.draw_frame(event_loop, host, self.target.as_ref())?;
        let frame_ns = frame_start.elapsed().as_nanos() as u64;
        let frame_cpu = cpu_time(libc::CLOCK_THREAD_CPUTIME_ID)? - cpu_start;
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
        let process_cpu = (cpu_time(libc::CLOCK_PROCESS_CPUTIME_ID)? - cpu_started) as f64 / 1e9;
        let report = serde_json::json!({
            "case": self.case, "font": "Menlo", "font_size_points": 13,
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

fn cpu_time(clock: libc::clockid_t) -> Result<u64> {
    let mut time: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(clock, &mut time) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64)
}

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
    host.focus_hint.dismiss();
    let result = (|| -> Result<()> {
        app.draw(event_loop, host)?;
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
    host.repaint();
    result?;
    eprintln!(
        "Native smoke: terminal rendering and synchronized output passed; progress reused terminal content, stopped while occluded, and refreshed after reveal"
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
        pane.input.push_back(Vec::new());
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
            host.capture = true;
            app.draw(event_loop, host)?;
            app.painter
                .render_state()
                .ok_or("missing capture device")?
                .device
                .poll(wgpu::PollType::wait_indefinitely())?;
            let mut events = Vec::new();
            app.painter.handle_screenshots(&mut events);
            let image = events
                .into_iter()
                .find_map(|event| match event {
                    egui::Event::Screenshot { image, .. } => Some(image),
                    _ => None,
                })
                .ok_or("missing Find screenshot")?;
            let path = directory.join(if window_focused {
                "find-focused.png"
            } else {
                "find-unfocused.png"
            });
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
        }
        set_focus(host, true);
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
        ));
        // Hold encoded reports in the existing queue instead of sending them to a shell.
        pane.input.push_back(Vec::new());
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
        check_link_hover(app, host, focused, hovered)?;
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
                        && pane.input.iter().skip(1).any(|bytes| {
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
                    || emitted && pane.input.back() != Some(&expected)
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
        let path = Path::new("/tmp/rustty's dropped file.txt");
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
            let expected: &[u8] = if bracketed {
                b"\x1b[200~'/tmp/rustty'\\''s dropped file.txt' \x1b[201~"
            } else {
                b"'/tmp/rustty'\\''s dropped file.txt' "
            };
            if app.focused(host.id) != Some(focused) || app.panes[&focused].input.len() != 1 {
                return Err("file drop affected the focused pane".into());
            }
            let pane = app.panes.get_mut(&hovered).unwrap();
            if pane.input.len() != 2 || pane.input.back().unwrap() != expected {
                return Err(
                    "file drop did not use the target pane's paste mode and quoted path".into(),
                );
            }
            pane.input.pop_back();
            pane.input_bytes = 0;
        }
        Ok(())
    })();
    for (id, terminal, input, input_bytes, mouse_cell, scroll) in saved {
        let pane = app.panes.get_mut(&id).unwrap();
        *pane.session.terminal()? = terminal;
        pane.input = input;
        pane.input_bytes = input_bytes;
        pane.mouse_cell = mouse_cell;
        pane.scroll = scroll;
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

fn check_link_hover(app: &mut App, host: &mut Host, focused: Id, hovered: Id) -> Result<()> {
    use winit::keyboard::ModifiersState;
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
    host.modifiers = ModifiersState::SUPER.into();
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
    if !host
        .hovered_link
        .as_ref()
        .is_some_and(|link| link.uri == "https://example.org")
    {
        return Err("Command-hover did not recognize a plain URL".into());
    }
    host.modifiers = ModifiersState::empty().into();
    if !app.update_hover_link(host) || host.hovered_link.is_some() {
        return Err("releasing Command retained a link underline".into());
    }
    host.modifiers = ModifiersState::SUPER.into();
    app.update_hover_link(host);
    app.panes[&hovered]
        .session
        .terminal()?
        .feed(b"\x1b[?1000h\x1b[>1s");
    if !app.update_hover_link(host) || host.hovered_link.is_some() {
        return Err("mouse-captured text was advertised as Command-clickable".into());
    }
    host.modifiers = (ModifiersState::SUPER | ModifiersState::SHIFT).into();
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

/// Render the same host primitives to a texture when no drawable is available
/// (e.g. a locked CI Mac). This does not claim visible surface presentation.
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
    target: &wgpu::Texture,
) {
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
    let device = &state.device;
    let queue = &state.queue;
    let texture = offscreen_texture(state, size);
    let stride = (size[0] * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Rustty host readback"),
        size: u64::from(stride) * u64::from(size[1]),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let (mut encoder, commands) =
        offscreen_commands(state, primitives, delta, size, scale, &texture);
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
    queue.submit(commands.into_iter().chain([encoder.finish()]));
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    device.poll(wgpu::PollType::wait_indefinitely())?;
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
