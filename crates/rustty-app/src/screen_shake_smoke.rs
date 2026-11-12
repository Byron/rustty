//! Observe real per-panel bells and redraw retirement with idle disposable PTYs.
use super::*;

pub(super) const COMMAND: &str =
    r"stty -echo; printf '\033]2;SHAKE_READY\007'; while IFS= read -r line; do printf '\007'; done";

pub(super) struct Check {
    native: winit::window::WindowId,
    source: Id,
    neighbor: Id,
    stage: u8,
    started: Instant,
    first_bell: Option<Instant>,
    last_frame: u64,
    samples: usize,
    survived_first_duration: bool,
    captured_active: bool,
    prepared: BTreeMap<Id, Arc<Frame>>,
    rects: BTreeMap<Id, egui::Rect>,
    grids: Vec<(Id, u16, u16)>,
    baseline: Option<Arc<Frame>>,
}

impl Check {
    pub fn new(app: &mut App, event_loop: &ActiveEventLoop) -> Result<Self> {
        let native = *app.windows.keys().next().ok_or("missing shake window")?;
        let window = app.windows[&native].id;
        let source = app.focused(window).unwrap();
        let neighbor = app.workspace.id();
        let split = app.workspace.id();
        let tab = app.tab_mut(window).unwrap();
        tab.split(neighbor, split, Direction::Right, fixture_directory());
        tab.focus(source);
        app.spawn_pane(neighbor, fixture_directory(), None, None)?;
        app.reconcile(event_loop);
        let host = app.windows.get_mut(&native).unwrap();
        host.visible = true;
        host.occluded = false;
        host.focused = true;
        host.repaint();
        Ok(Self {
            native,
            source,
            neighbor,
            stage: 0,
            started: Instant::now(),
            first_bell: None,
            last_frame: host.frames,
            samples: 0,
            survived_first_duration: false,
            captured_active: false,
            prepared: BTreeMap::new(),
            rects: BTreeMap::new(),
            grids: Vec::new(),
            baseline: None,
        })
    }

    pub fn step(
        &mut self,
        app: &mut App,
        event_loop: &ActiveEventLoop,
        directory: &Path,
    ) -> Result<bool> {
        if !app.errors.is_empty() {
            return Err(format!("screen shake render errors: {:?}", app.errors).into());
        }
        if self.started.elapsed() > Duration::from_secs(5) {
            return Err(format!(
                "screen shake stalled at stage {} with {} samples",
                self.stage, self.samples
            )
            .into());
        }
        let mut host = app
            .windows
            .remove(&self.native)
            .ok_or("shake window closed")?;
        let result = self.step_host(app, event_loop, &mut host, directory);
        app.windows.insert(self.native, host);
        result
    }

    fn step_host(
        &mut self,
        app: &mut App,
        event_loop: &ActiveEventLoop,
        host: &mut Host,
        directory: &Path,
    ) -> Result<bool> {
        let now = Instant::now();
        if self.stage == 0 {
            if self.started.elapsed() < Duration::from_millis(300)
                || [self.source, self.neighbor]
                    .iter()
                    .any(|id| app.panes[id].title != "SHAKE_READY")
            {
                return Ok(false);
            }
            for (id, color, text) in [
                (self.source, 36, "Bell source"),
                (self.neighbor, 33, "Stationary neighbor"),
            ] {
                app.panes[&id].session.terminal()?.feed(format!("\x1b[2J\x1b[H\x1b[?25l\x1b[{color}m{text}\x1b[0m\r\nQuiet terminal pixels\r\n").as_bytes());
            }
            host.focus_hint.dismiss();
            capture_window(app, event_loop, host, &directory.join("shake-before.png"))?;
            assert_eq!(host.rects.len(), 2);
            self.rects = host.rects.clone();
            self.baseline = Some(Arc::clone(&host.composed.as_ref().unwrap().frame));
            for id in [self.source, self.neighbor] {
                self.prepared
                    .insert(id, Arc::clone(&host.prepared[&id].frame));
                let terminal = app.panes[&id].session.terminal()?;
                self.grids.push((id, terminal.cols, terminal.rows));
            }
            self.last_frame = host.frames;
            app.panes[&self.source].session.write(b"shake\n")?;
            self.stage = 1;
            return Ok(false);
        }

        let composed = host.composed.as_ref().ok_or("missing shake composition")?;
        assert_eq!(host.rects, self.rects, "shake changed panel layout");
        for &(id, cols, rows) in &self.grids {
            let terminal = app.panes[&id].session.terminal()?;
            assert_eq!(
                (terminal.cols, terminal.rows),
                (cols, rows),
                "shake resized a PTY"
            );
            assert!(
                Arc::ptr_eq(&self.prepared[&id], &host.prepared[&id].frame),
                "shake rerasterized terminal text"
            );
        }
        let source = composed
            .panes
            .iter()
            .find(|pane| Arc::ptr_eq(&pane.frame, &self.prepared[&self.source]))
            .ok_or("missing source panel composition")?;
        let neighbor = composed
            .panes
            .iter()
            .find(|pane| Arc::ptr_eq(&pane.frame, &self.prepared[&self.neighbor]))
            .ok_or("missing neighbor panel composition")?;
        assert_eq!(neighbor.offset, [0.0; 2], "bell shook its neighbor");

        if self.stage <= 2 {
            if app.panes[&self.source].screen_shake.is_some() {
                let first = *self.first_bell.get_or_insert(now);
                if host.frames != self.last_frame && source.offset[0].abs() > 0.01 {
                    self.samples += 1;
                    self.last_frame = host.frames;
                }
                if self.samples >= 4 && !self.captured_active {
                    capture_window(app, event_loop, host, &directory.join("shake-active.png"))?;
                    self.last_frame = host.frames;
                    self.captured_active = true;
                }
                if self.stage == 1 && now.duration_since(first) >= Duration::from_millis(250) {
                    app.panes[&self.source].session.write(b"again\n")?;
                    self.stage = 2;
                }
                if self.stage == 2 && now.duration_since(first) > Duration::from_millis(500) {
                    self.survived_first_duration = true;
                }
                return Ok(false);
            }
            if self.first_bell.is_none() {
                return Ok(false);
            }
            assert!(
                self.samples >= 4,
                "idle bells did not schedule animation frames"
            );
            assert!(
                self.survived_first_duration,
                "a repeated bell did not restart the shader"
            );
            assert_eq!(source.offset, [0.0; 2]);
            assert_eq!(
                composed.frame.quads,
                self.baseline.as_ref().unwrap().quads,
                "shader retirement did not restore terminal pixels"
            );
            capture_window(app, event_loop, host, &directory.join("shake-after.png"))?;
            self.stage = 3;
            self.started = now;
            return Ok(false);
        }
        if self.stage == 3 {
            if self.started.elapsed() < Duration::from_millis(150) {
                return Ok(false);
            }
            self.last_frame = host.frames;
            self.started = now;
            self.stage = 4;
            return Ok(false);
        }
        if self.started.elapsed() < Duration::from_millis(250) {
            return Ok(false);
        }
        let idle_frames = host.frames - self.last_frame;
        // The last animation pass may already have queued one native follow-up.
        assert!(
            idle_frames <= 1,
            "retired shader kept repainting idle panels"
        );
        assert!(app.panes[&self.source].screen_shake.is_none());
        assert!(app.panes[&self.neighbor].screen_shake.is_none());
        fs::write(
            directory.join("result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true, "native_focus_checked": false, "moving_samples": self.samples, "idle_frames": idle_frames,
                "checks": ["real-pty-bell", "per-panel-shader", "stationary-neighbor", "idle-animation-redraws", "repeat-bell-restarts", "prepared-text-retained", "pty-grids-unchanged", "layout-unchanged", "exact-retirement", "idle-after-retirement"],
            }))?,
        )?;
        eprintln!("Native screen shake passed: {}", directory.display());
        Ok(true)
    }
}
