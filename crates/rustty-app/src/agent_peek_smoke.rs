//! Disposable native picker check: real renderers and PTYs, no USB or foreground windows.
use super::*;
use rustty::vt::agent::{Event as AgentEvent, Snapshot, State};
use rustty_app::agent_peek::Filter;

/// Observe native redraws; no captures, synthetic frame times, or polling repaints.
pub(super) struct Timing {
    native: winit::window::WindowId,
    window: Id,
    source: Id,
    target: Id,
    target_tab: Id,
    parent: Id,
    stage: u8,
    started: Instant,
    frames: u64,
    last_frame: u64,
    samples: Vec<(f64, f32)>,
    opening: Option<serde_json::Value>,
}

impl Timing {
    pub fn new(app: &mut App, event_loop: &ActiveEventLoop) -> Result<Self> {
        let native = *app.windows.keys().next().ok_or("missing timing window")?;
        let window = app.windows[&native].id;
        let source = app.focused(window).unwrap();
        app.tab_mut(window).unwrap().quad_roots.insert(source);
        let target_tab = app.workspace.id();
        let target = app.workspace.id();
        let neighbor = app.workspace.id();
        let split = app.workspace.id();
        let mut tab = Tab::new(target_tab, target, fixture_directory());
        tab.split(neighbor, split, Direction::Right, fixture_directory());
        let parent = tab.root.id;
        tab.quad_roots.insert(parent);
        tab.focus(target);
        tab.zoom = Some(target);
        tab.quadrant_zoom = Some(parent);
        let index = app.index(window).unwrap();
        app.workspace.windows[index].tabs.push(tab);
        app.spawn_pane(target, fixture_directory(), None, None)?;
        app.spawn_pane(neighbor, fixture_directory(), None, None)?;
        app.reconcile(event_loop);
        app.loaded.config.window_animations = Some(true);
        app.loaded.config.quad_peek_animation_duration = Duration::ZERO;
        app.loaded.config.agent_peek_animation_duration = Duration::from_millis(500);
        app.loaded.config.agent_peek_select_animation_duration = Duration::from_millis(500);
        for (id, state) in [(source, State::Idle), (target, State::Done)] {
            let pane = app.panes.get_mut(&id).unwrap();
            pane.agent.apply(
                AgentEvent::Begin(Snapshot {
                    state,
                    label: None,
                    thread_id: Some(format!("timing-{id}")),
                    turn_id: (state == State::Done).then(|| "done-1".into()),
                }),
                id,
                false,
            );
            pane.session
                .terminal()?
                .feed(b"\x1b[2J\x1b[HQuiet timing fixture\r\n");
        }
        let host = app.windows.get_mut(&native).unwrap();
        host.visible = true;
        host.occluded = false;
        host.focused = false;
        host.repaint();
        Ok(Self {
            native,
            window,
            source,
            target,
            target_tab,
            parent,
            stage: 0,
            started: Instant::now(),
            frames: host.frames,
            last_frame: host.frames,
            samples: Vec::new(),
            opening: None,
        })
    }

    pub fn step(&mut self, app: &mut App, directory: &Path) -> Result<bool> {
        if !app.errors.is_empty() {
            return Err(format!("agent peek timing render errors: {:?}", app.errors).into());
        }
        let mut host = app
            .windows
            .remove(&self.native)
            .ok_or("timing window closed")?;
        let result = self.step_host(app, &mut host, directory);
        app.windows.insert(self.native, host);
        result
    }

    fn step_host(&mut self, app: &mut App, host: &mut Host, directory: &Path) -> Result<bool> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.started);
        if elapsed > Duration::from_secs(2) {
            return Err(format!(
                "agent peek timing stalled at stage {}: {} redraws, samples {:?}",
                self.stage,
                host.frames - self.frames,
                self.samples
            )
            .into());
        }
        if self.stage == 0 {
            if elapsed < Duration::from_millis(300) || host.frames == self.frames {
                return Ok(false);
            }
            let chord = config::Modifiers {
                super_key: true,
                ..Default::default()
            };
            host.modifiers = winit::event::Modifiers::from(winit::keyboard::ModifiersState::SUPER);
            host.peek = app.tab_mut(self.window).unwrap().begin_peek(chord);
            host.repaint();
            self.stage = 1;
            self.started = now;
            return Ok(false);
        }
        if self.stage == 1 {
            if elapsed < Duration::from_millis(300) {
                return Ok(false);
            }
            let Some((rect, _)) = host
                .agent_peek
                .footer
                .iter()
                .find(|(_, filter)| *filter == Filter::All)
            else {
                return Ok(false);
            };
            host.mouse = rect.center();
            app.mouse(host, vt::MouseAction::Press, Some(vt::MouseButton::Left));
            app.mouse(host, vt::MouseAction::Release, Some(vt::MouseButton::Left));
            self.stage = 2;
            self.started = now;
            self.frames = host.frames;
            self.last_frame = host.frames;
            return Ok(false);
        }
        let progress = if self.stage == 2 {
            host.agent_peek.state.dim
        } else {
            host.agent_peek
                .landing
                .as_ref()
                .map_or(1.0, |landing| landing.phase)
        };
        if self.last_frame != host.frames {
            self.last_frame = host.frames;
            self.samples
                .push((elapsed.as_secs_f64() * 1000.0, progress));
        }
        if self.stage == 3
            && host.peek.is_some()
            && (app.focused(self.window) != Some(self.source)
                || app
                    .tab(self.window)
                    .is_some_and(|tab| tab.id == self.target_tab)
                || app.panes[&self.target]
                    .deck_view(self.target, "")
                    .unwrap()
                    .state
                    != State::Done)
        {
            return Err("cross-tab landing changed focus or acknowledged before completion".into());
        }
        if progress < 1.0 || self.stage == 3 && host.peek.is_some() {
            return Ok(false);
        }
        let intermediate = self
            .samples
            .iter()
            .filter(|(_, phase)| *phase > 0.0 && *phase < 1.0)
            .count();
        if intermediate < 4
            || self.samples.first().is_none_or(|(ms, _)| *ms > 250.0)
            || !(Duration::from_millis(400)..Duration::from_millis(1200)).contains(&elapsed)
        {
            return Err(format!(
                "agent peek stage {} skipped its animation: {} redraws in {:?}, samples {:?}",
                self.stage,
                host.frames - self.frames,
                elapsed,
                self.samples
            )
            .into());
        }
        let report = serde_json::json!({
            "elapsed_ms": elapsed.as_secs_f64() * 1000.0,
            "frames": host.frames - self.frames,
            "intermediate_samples": intermediate,
            "first_frame_ms": self.samples[0].0,
        });
        if self.stage == 2 {
            self.opening = Some(report);
            host.mouse = host
                .agent_peek
                .geometry(
                    host.agent_peek
                        .state
                        .cards
                        .iter()
                        .find(|card| card.target.pane == self.target)
                        .ok_or("hidden-tab card missing")?,
                )
                .ok_or("hidden-tab card has no geometry")?
                .0
                .center();
            app.mouse(host, vt::MouseAction::Press, Some(vt::MouseButton::Left));
            app.mouse(host, vt::MouseAction::Release, Some(vt::MouseButton::Left));
            self.stage = 3;
            self.started = now;
            self.frames = host.frames;
            self.last_frame = host.frames;
            self.samples.clear();
            return Ok(false);
        }
        let tab = app.tab(self.window).unwrap();
        if tab.id != self.target_tab
            || tab.focused != self.target
            || tab.zoom != Some(self.parent)
            || tab.quadrant_zoom != Some(self.parent)
            || app.panes[&self.target]
                .deck_view(self.target, "")
                .unwrap()
                .state
                != State::Idle
        {
            return Err(
                "cross-tab landing missed its owning tab, parent zoom, or acknowledgement".into(),
            );
        }
        fs::write(
            directory.join("result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true, "native_focus_checked": false,
                "opening": self.opening, "cross_tab_landing": report,
            }))?,
        )?;
        eprintln!("Native agent peek timing passed: {}", directory.display());
        Ok(true)
    }
}

pub(super) fn check(app: &mut App, event_loop: &ActiveEventLoop, directory: &Path) -> Result<()> {
    let native = *app.windows.keys().next().ok_or("missing fixture window")?;
    let one = app.windows[&native].id;
    let first = app.focused(one).unwrap();
    let two = app.add_window(false);
    app.reconcile(event_loop);
    let second = app.focused(two).unwrap();
    let mut ids = vec![first, second];
    for direction in [
        Direction::Right,
        Direction::Down,
        Direction::Left,
        Direction::Up,
    ] {
        let pane = app.workspace.id();
        let split = app.workspace.id();
        app.tab_mut(two)
            .unwrap()
            .split(pane, split, direction, fixture_directory());
        ids.push(pane);
    }
    let hidden_tab = app.workspace.id();
    let hidden = app.workspace.id();
    let index = app.index(one).unwrap();
    app.workspace.windows[index]
        .tabs
        .push(Tab::new(hidden_tab, hidden, fixture_directory()));
    app.spawn_pane(hidden, fixture_directory(), None, None)?;
    ids.push(hidden);
    app.reconcile(event_loop);
    app.loaded.config.window_animations = Some(true);
    app.loaded.config.split_animation_duration = Duration::ZERO;
    app.loaded.config.quad_peek_animation_duration = Duration::ZERO;
    app.loaded.config.agent_peek_animation_duration = Duration::ZERO;
    app.panes[&hidden].session.resize(48, 36, 0, 0)?;
    app.panes[&second].session.resize(120, 12, 0, 0)?;
    let states = [
        State::Idle,
        State::Working,
        State::Paused,
        State::NeedsInput,
        State::Error,
        State::Done,
        State::Unknown,
    ];
    for (&id, state) in ids.iter().zip(states) {
        let pane = app.panes.get_mut(&id).unwrap();
        pane.agent.apply(
            AgentEvent::Begin(Snapshot {
                state,
                label: None,
                thread_id: Some(format!("pane-{id}")),
                turn_id: (state == State::Done).then(|| "turn-1".into()),
            }),
            id,
            false,
        );
        pane.session.terminal()?.feed(format!("\x1b[2J\x1b[H\x1b[36mProjected {:?}\x1b[0m\r\nLive content from pane {id}\r\nwide 界 · emoji 🦀\r\n", state).as_bytes());
    }
    app.refresh_directory_labels();
    let mut host = app.windows.remove(&native).unwrap();
    host.visible = true;
    host.occluded = false;
    host.focused = false;
    smoke::capture_window(app, event_loop, &mut host, &directory.join("before.png"))?;
    let chord = config::Modifiers {
        super_key: true,
        ..Default::default()
    };
    app.loaded.config.keybinds.push(config::KeyBinding::parse(
        "super+h=goto_split:quadrant_left",
    )?);
    app.windows.insert(native, host);
    app.window_event(
        event_loop,
        native,
        WindowEvent::ModifiersChanged(winit::keyboard::ModifiersState::SUPER.into()),
    );
    host = app.windows.remove(&native).unwrap();
    assert!(!app.action(
        event_loop,
        &mut host,
        Action::GotoSplit(Direction::QuadrantLeft),
        true,
    ));
    host.navigation_warning = None;
    assert!(host.peek.is_none(), "single-pane tabs have no quads");
    assert_eq!(app.agent_peek_reports().len(), 7);
    smoke::capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("single-pane-prefix.png"),
    )?;
    assert!(host.agent_peek.footer.is_empty());
    assert!(!host.agent_peek.state.active());
    assert!(host.presented_peek.is_none());
    let neighbor = app.workspace.id();
    let split = app.workspace.id();
    let tab = app.tab_mut(one).unwrap();
    tab.split(neighbor, split, Direction::Right, fixture_directory());
    tab.focus(first);
    app.spawn_pane(neighbor, fixture_directory(), None, None)?;
    app.windows.insert(native, host);
    app.window_event(
        event_loop,
        native,
        WindowEvent::ModifiersChanged(winit::keyboard::ModifiersState::SUPER.into()),
    );
    host = app.windows.remove(&native).unwrap();
    assert!(!app.action(
        event_loop,
        &mut host,
        Action::GotoSplit(Direction::QuadrantLeft),
        true,
    ));
    host.navigation_warning = None;
    assert!(host.peek.is_none(), "two-pane tabs have no quads");
    smoke::capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("two-pane-prefix.png"),
    )?;
    assert_eq!(host.rects.len(), 2);
    assert!(host.agent_peek.footer.is_empty());
    assert!(!host.agent_peek.state.active());
    assert!(host.presented_peek.is_none());
    app.loaded.config.keybinds.pop();
    app.tab_mut(one).unwrap().quad_roots.insert(split);
    host.peek = app.tab_mut(one).unwrap().begin_peek(chord);
    assert!(
        host.peek.is_some(),
        "designated quad roots offer agent peek"
    );
    smoke::capture_window(app, event_loop, &mut host, &directory.join("selectors.png"))?;
    assert_eq!(host.agent_peek.footer.len(), 8);
    let unowned = PhysicalKey::Code(winit::keyboard::KeyCode::Digit9);
    assert!(!app.peek_key(&mut host, unowned, ElementState::Released, false));
    assert!(app.peek_key(&mut host, unowned, ElementState::Pressed, true));
    assert!(!app.consumed_global_keys.contains(&unowned));
    assert!(!app.peek_key(&mut host, unowned, ElementState::Released, false));
    assert_eq!(app.focused(one), Some(first));
    let grids: Vec<_> = ids
        .iter()
        .map(|id| {
            let terminal = app.panes[id].session.terminal().unwrap();
            (*id, terminal.cols, terminal.rows)
        })
        .collect();
    let all = host
        .agent_peek
        .footer
        .iter()
        .find(|(_, filter)| *filter == Filter::All)
        .unwrap()
        .0
        .center();
    host.mouse = all;
    app.mouse(
        &mut host,
        vt::MouseAction::Press,
        Some(vt::MouseButton::Left),
    );
    smoke::capture_window(app, event_loop, &mut host, &directory.join("all.png"))?;
    assert_eq!(host.agent_peek.state.cards.len(), 7);
    app.tab_mut(one).unwrap().quad_roots.remove(&split);
    smoke::capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("last-quad-removed.png"),
    )?;
    assert!(host.peek.is_none());
    assert!(host.agent_peek.footer.is_empty());
    assert!(!host.agent_peek.state.active());
    assert!(
        host.agent_peek
            .composed
            .as_ref()
            .unwrap()
            .frame
            .quads
            .is_empty()
    );
    assert!(host.presented_peek.is_none());
    app.tab_mut(one).unwrap().quad_roots.insert(split);
    host.peek = app.tab_mut(one).unwrap().begin_peek(chord);
    smoke::capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("quad-restored-selectors.png"),
    )?;
    host.mouse = all;
    app.mouse(
        &mut host,
        vt::MouseAction::Press,
        Some(vt::MouseButton::Left),
    );
    smoke::capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("quad-restored-agents.png"),
    )?;
    assert_eq!(host.agent_peek.state.cards.len(), 7);
    for card in &host.agent_peek.state.cards {
        let (outer, content) = host.agent_peek.geometry(card).unwrap();
        let [width, height] = host.agent_peek.prepared[&card.target.pane].frame.size;
        assert!((content.width() / content.height() - width as f32 / height as f32).abs() < 0.001);
        assert!((outer.width() - content.width() - 8.0).abs() < 0.001);
        assert!((outer.height() - content.height() - 30.0).abs() < 0.001);
    }
    let done = ids[5];
    assert_eq!(
        app.panes[&done].deck_view(done, "").unwrap().state,
        State::Done
    );
    let card = host
        .agent_peek
        .state
        .cards
        .iter()
        .find(|card| card.target.pane == hidden)
        .unwrap()
        .clone();
    let geometry = host.agent_peek.geometry(&card).unwrap();
    let (outer, _) = geometry;
    assert!(
        outer.width() < card.rect.width() - 8.0,
        "portrait fixture must expose the old unused card area"
    );
    host.mouse = Pos2::new(card.rect.left() + 1.0, outer.center().y);
    assert!(host.agent_peek.hit(host.mouse).is_none());
    app.mouse(
        &mut host,
        vt::MouseAction::Press,
        Some(vt::MouseButton::Left),
    );
    assert!(
        host.agent_peek.state.exiting.is_none(),
        "unused grid space selected a panel"
    );
    host.mouse = outer.center();
    app.mouse(&mut host, vt::MouseAction::Move, None);
    smoke::capture_window(app, event_loop, &mut host, &directory.join("hover.png"))?;
    assert_eq!(
        host.agent_peek
            .geometry(
                host.agent_peek
                    .state
                    .cards
                    .iter()
                    .find(|card| card.target.pane == hidden)
                    .unwrap()
            )
            .unwrap(),
        geometry,
        "hover changed preview geometry"
    );
    for (id, cols, rows) in &grids {
        let terminal = app.panes[id].session.terminal()?;
        assert_eq!(
            (terminal.cols, terminal.rows),
            (*cols, *rows),
            "projection resized a PTY"
        );
    }
    let old_frame = Arc::clone(&host.agent_peek.prepared[&hidden].frame);
    app.panes[&hidden]
        .session
        .terminal()?
        .feed(b"\r\n\x1b[32mUpdated hidden pane content\x1b[0m");
    smoke::capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("live-update.png"),
    )?;
    assert!(!Arc::ptr_eq(
        &old_frame,
        &host.agent_peek.prepared[&hidden].frame
    ));
    // Exact state changes stay in place with the same selector number.
    let number = host
        .agent_peek
        .state
        .cards
        .iter()
        .find(|card| card.target.pane == hidden)
        .unwrap()
        .number;
    app.panes.get_mut(&hidden).unwrap().agent.apply(
        AgentEvent::Update(Snapshot {
            state: State::Working,
            label: None,
            thread_id: Some(format!("pane-{hidden}")),
            turn_id: None,
        }),
        hidden,
        false,
    );
    smoke::capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("state-change.png"),
    )?;
    assert_eq!(
        host.agent_peek
            .state
            .cards
            .iter()
            .find(|card| card.target.pane == hidden)
            .unwrap()
            .number,
        number
    );
    assert_eq!(
        host.agent_peek
            .state
            .cards
            .iter()
            .find(|card| card.target.pane == hidden)
            .unwrap()
            .state,
        State::Working
    );
    assert!(
        !host
            .agent_peek
            .footer
            .iter()
            .any(|(_, filter)| *filter == Filter::State(State::Unknown))
    );
    // State filter -> all keeps the existing frame and animates from the current rectangle.
    for filter in [Filter::State(State::Working), Filter::All] {
        let rect = host
            .agent_peek
            .footer
            .iter()
            .find(|(_, f)| *f == filter)
            .unwrap()
            .0;
        host.mouse = rect.center();
        app.mouse(
            &mut host,
            vt::MouseAction::Press,
            Some(vt::MouseButton::Left),
        );
        smoke::capture_window(
            app,
            event_loop,
            &mut host,
            &directory.join(if filter == Filter::All {
                "all-again.png"
            } else {
                "running.png"
            }),
        )?;
    }
    assert_eq!(
        app.panes[&done].deck_view(done, "").unwrap().state,
        State::Done
    );
    for pane in app.panes.values() {
        assert!(pane.input.is_empty());
    }
    let source_active = app.active;
    for selecting in [false, true] {
        if selecting {
            let target = host
                .agent_peek
                .state
                .cards
                .iter()
                .find(|card| card.target.pane == hidden)
                .unwrap()
                .target;
            let card = host
                .agent_peek
                .state
                .cards
                .iter()
                .find(|card| card.target == target)
                .unwrap();
            let original = host.agent_peek.geometry(card).unwrap();
            let source_size = host.agent_peek.prepared[&hidden].frame.size;
            app.loaded.config.agent_peek_select_animation_duration = Duration::from_secs(1000);
            let now = Instant::now();
            host.agent_peek.state.select(target, now);
            app.update_agent_peek(&mut host, true, &app.loaded.config.clone(), now);
            assert!(host.agent_peek.state.exiting.is_some());
            let mut previous_distance = f32::INFINITY;
            for (permille, name) in [
                (10, "same-window-start.png"),
                (250, "same-window-floating.png"),
                (550, "same-window-background.png"),
                (850, "same-window-landing.png"),
                (999, "same-window-arriving.png"),
            ] {
                host.agent_peek.state.exiting.as_mut().unwrap().1 =
                    Instant::now() - Duration::from_secs(permille);
                smoke::capture_window(app, event_loop, &mut host, &directory.join(name))?;
                let landing = host.agent_peek.landing.as_ref().unwrap();
                assert_eq!(landing.target, target);
                assert_eq!(
                    landing.from, original,
                    "resizing the destination moved the source texture"
                );
                assert_eq!(
                    host.agent_peek.prepared[&hidden].frame.size, source_size,
                    "destination PTY resize replaced the floating texture"
                );
                let card = host
                    .agent_peek
                    .state
                    .cards
                    .iter()
                    .find(|card| card.target == target)
                    .unwrap();
                let (outer, content) = host.agent_peek.geometry(card).unwrap();
                let viewport = egui::Rect::from_min_size(
                    Pos2::new(host.content.x, host.content.y),
                    Vec2::new(host.content.width, host.content.height),
                );
                assert!(
                    viewport.contains_rect(outer),
                    "same-window selection flew outside the viewport"
                );
                let distance = content.min.distance(landing.destination.min)
                    + content.max.distance(landing.destination.max);
                assert!(
                    distance < previous_distance,
                    "floating pane did not approach its destination"
                );
                if permille == 10 {
                    assert!(
                        content.min.distance(original.1.min) + content.max.distance(original.1.max)
                            > 0.01,
                        "floating pane held still at the start"
                    );
                }
                if permille == 850 {
                    assert!(
                        distance > 1.0,
                        "floating pane settled before the animation ended"
                    );
                }
                if permille == 999 {
                    assert!(
                        distance < 0.02,
                        "floating pane missed its native terminal coordinates"
                    );
                }
                previous_distance = distance;
                let view = app.agent_peek_view(&host).unwrap();
                assert_eq!(view.id, hidden_tab);
                assert_eq!(view.focused, hidden);
                assert_eq!(view.zoom, None);
                assert!(
                    host.rects.contains_key(&hidden),
                    "destination background was not rendered from the start"
                );
                check_landing_frame(&host);
                assert_eq!(app.workspace.windows[app.index(one).unwrap()].active_tab, 0);
                assert_eq!(app.focused(one), Some(first));
                assert_eq!(app.active, source_active);
            }
        }
        app.windows.insert(native, host);
        app.window_event(
            event_loop,
            native,
            WindowEvent::ModifiersChanged(Default::default()),
        );
        host = app.windows.remove(&native).unwrap();
        assert!(host.peek.is_none());
        assert!(!host.agent_peek.state.active());
        assert!(host.agent_peek.state.exiting.is_none());
        assert!(host.agent_peek.landing.is_none());
        assert!(host.agent_peek.footer.is_empty());
        app.update_agent_peek(
            &mut host,
            true,
            &app.loaded.config.clone(),
            Instant::now() + Duration::from_secs(2),
        );
        assert_eq!(app.active, source_active);
        assert_eq!(app.workspace.windows[app.index(one).unwrap()].active_tab, 0);
        assert_eq!(app.focused(one), Some(first));
        host.modifiers = winit::event::Modifiers::from(winit::keyboard::ModifiersState::SUPER);
        host.peek = app.tab_mut(one).unwrap().begin_peek(chord);
        let bounds = egui::Rect::from_min_size(
            Pos2::new(host.content.x + 8.0, host.content.y + 8.0),
            Vec2::new(host.content.width - 16.0, host.content.height - 64.0),
        );
        host.agent_peek.state.toggle(
            Filter::All,
            &app.agent_peek_reports(),
            bounds,
            Instant::now(),
        );
        smoke::capture_window(
            app,
            event_loop,
            &mut host,
            &directory.join(if selecting {
                "after-flyout-cancel.png"
            } else {
                "after-prefix-release.png"
            }),
        )?;
    }
    // Instant selection reaches the same destination without a deferred background switch.
    let target = host
        .agent_peek
        .state
        .cards
        .iter()
        .find(|card| card.target.pane == hidden)
        .unwrap()
        .target;
    host.agent_peek.state.select(target, Instant::now());
    assert_eq!(app.workspace.windows[app.index(one).unwrap()].active_tab, 0);
    app.loaded.config.agent_peek_select_animation_duration = Duration::ZERO;
    app.update_agent_peek(&mut host, true, &app.loaded.config.clone(), Instant::now());
    assert_eq!(app.workspace.windows[app.index(one).unwrap()].active_tab, 1);
    assert_eq!(app.focused(one), Some(hidden));
    assert!(host.peek.is_none());
    smoke::capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("same-window-landed.png"),
    )?;
    assert!(host.rects.contains_key(&hidden));
    // Return and choose another native window's panel with its enclosing quad visible.
    let index = app.index(one).unwrap();
    app.workspace.windows[index].active_tab = 0;
    host.peek = app.tab_mut(one).unwrap().begin_peek(chord);
    smoke::capture_window(app, event_loop, &mut host, &directory.join("return.png"))?;
    let bounds = egui::Rect::from_min_size(
        Pos2::new(host.content.x + 8.0, host.content.y + 8.0),
        Vec2::new(host.content.width - 16.0, host.content.height - 64.0),
    );
    host.agent_peek.state.toggle(
        Filter::All,
        &app.agent_peek_reports(),
        bounds,
        Instant::now(),
    );
    smoke::capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("cross-window.png"),
    )?;
    let target = host
        .agent_peek
        .state
        .cards
        .iter()
        .find(|card| card.target.pane == done)
        .unwrap()
        .target;
    host.mouse = host
        .agent_peek
        .state
        .cards
        .iter()
        .find(|card| card.target == target)
        .unwrap()
        .rect
        .center();
    app.mouse(
        &mut host,
        vt::MouseAction::Press,
        Some(vt::MouseButton::Left),
    );
    app.update_agent_peek(&mut host, true, &app.loaded.config.clone(), Instant::now());
    assert_eq!(app.active, Some(two));
    assert_eq!(app.focused(two), Some(done));
    assert_eq!(
        app.panes[&done].deck_view(done, "").unwrap().state,
        State::Idle
    );
    let tab = app.tab(two).unwrap();
    assert_eq!(tab.zoom, tab.quad_path(done).last().copied());
    assert_eq!(tab.quadrant_zoom, tab.zoom);
    app.windows.insert(native, host);
    // A pane from the current tab lands in its enclosing quad instead of leaving the window.
    let native_two = *app
        .windows
        .iter()
        .find(|(_, host)| host.id == two)
        .unwrap()
        .0;
    let mut host = app.windows.remove(&native_two).unwrap();
    host.visible = true;
    host.occluded = false;
    host.focused = false;
    host.modifiers = winit::event::Modifiers::from(winit::keyboard::ModifiersState::SUPER);
    let tab = app.tab(two).unwrap();
    let (same_tab, parent) = ids
        .iter()
        .copied()
        .filter(|id| *id != done)
        .find_map(|id| {
            tab.quad_path(id)
                .into_iter()
                .rev()
                .find(|quad| *quad != id)
                .map(|parent| (id, parent))
        })
        .ok_or("fixture lacks a second pane inside a parent quad")?;
    let tab_id = tab.id;
    host.peek = app.tab_mut(two).unwrap().begin_peek(chord);
    smoke::capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("same-tab-before.png"),
    )?;
    let bounds = egui::Rect::from_min_size(
        Pos2::new(host.content.x + 8.0, host.content.y + 8.0),
        Vec2::new(host.content.width - 16.0, host.content.height - 64.0),
    );
    host.agent_peek.state.toggle(
        Filter::All,
        &app.agent_peek_reports(),
        bounds,
        Instant::now(),
    );
    smoke::capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("same-tab-all.png"),
    )?;
    let target = host
        .agent_peek
        .state
        .cards
        .iter()
        .find(|card| card.target.pane == same_tab)
        .unwrap()
        .target;
    app.loaded.config.agent_peek_select_animation_duration = Duration::from_secs(1000);
    host.agent_peek.state.select(target, Instant::now());
    app.update_agent_peek(&mut host, true, &app.loaded.config.clone(), Instant::now());
    host.agent_peek.state.exiting.as_mut().unwrap().1 = Instant::now() - Duration::from_secs(550);
    smoke::capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("same-tab-landing.png"),
    )?;
    let view = app.agent_peek_view(&host).unwrap();
    assert_eq!(view.id, tab_id);
    assert_eq!(view.focused, same_tab);
    assert_eq!(view.zoom, Some(parent));
    assert_eq!(view.quadrant_zoom, Some(parent));
    assert_eq!(app.focused(two), Some(done));
    assert_eq!(app.tab(two).unwrap().zoom, None);
    assert_eq!(app.active, Some(two));
    check_landing_frame(&host);
    let animation =
        std::env::var_os("RUSTTY_SMOKE_AGENT_PEEK_ANIMATION").map(|_| directory.join("animation"));
    if let Some(animation) = &animation {
        fs::create_dir_all(animation)?;
        for frame in 0..24 {
            host.agent_peek.state.exiting.as_mut().unwrap().1 =
                Instant::now() - Duration::from_secs_f32(1000.0 * frame as f32 / 24.0);
            smoke::capture_window(
                app,
                event_loop,
                &mut host,
                &animation.join(format!("{frame:02}.png")),
            )?;
            check_landing_frame(&host);
        }
    }
    host.agent_peek.state.exiting.as_mut().unwrap().1 = Instant::now() - Duration::from_secs(999);
    smoke::capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("same-tab-arriving.png"),
    )?;
    check_landing_frame(&host);
    let destination = host.agent_peek.landing.as_ref().unwrap().destination;
    host.agent_peek.state.exiting.as_mut().unwrap().1 = Instant::now() - Duration::from_secs(1000);
    smoke::capture_window(
        app,
        event_loop,
        &mut host,
        &directory.join("same-tab-landed.png"),
    )?;
    assert_eq!(app.focused(two), Some(same_tab));
    assert_eq!(app.tab(two).unwrap().zoom, Some(parent));
    assert_eq!(app.tab(two).unwrap().quadrant_zoom, Some(parent));
    assert!(host.peek.is_none());
    assert!(host.agent_peek.landing.is_none());
    let rect = host.rects[&same_tab];
    let config = app.window_config(two);
    assert_eq!(
        destination.min,
        rect.min + Vec2::new(config.window_padding_x.start, config.window_padding_y.start)
    );
    if let Some(animation) = &animation {
        smoke::capture_window(app, event_loop, &mut host, &animation.join("24.png"))?;
    }
    app.windows.insert(native_two, host);
    fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "passed": true, "native_focus_checked": false,
            "checks": ["all-stream-deck-states", "no-quads-no-peek-overlays", "designated-root-peek", "zero-count-hidden", "filter-toggles", "shared-panels-retained", "hover-keeps-geometry", "source-aspect-ratios", "tight-preview-hit-targets", "prefix-release-ends-overlay", "prefix-release-cancels-landing", "pty-grids-unchanged", "hidden-pane-live-update", "state-changes-retain-numbers", "no-input-or-preview-acknowledgement", "same-window-continuous-motion", "same-window-background", "same-window-frozen-texture", "same-window-opaque-float", "same-window-uniform-glyph-scale", "native-pixel-handoff", "same-tab-parent-quad-landing", "cross-tab-selection", "cross-window-selection", "parent-quad-zoom", "selection-acknowledgement"],
        }))?,
    )?;
    eprintln!("Native agent peek passed: {}", directory.display());
    Ok(())
}

fn check_landing_frame(host: &Host) {
    let landing = host.agent_peek.landing.as_ref().unwrap();
    let composed = host.agent_peek.composed.as_ref().unwrap();
    let floating = composed
        .panes
        .iter()
        .find(|pane| pane.transition.is_some_and(|paint| paint.moving))
        .expect("missing floating pane composition");
    let paint = floating.transition.unwrap();
    assert_eq!(paint.opacity, 1.0, "selected pane faded during landing");
    assert_eq!(
        paint.background_opacity, 1.0,
        "selected pane background faded during landing"
    );
    let scale = floating.rect[2] / floating.frame.size[0] as f32;
    assert!(
        (scale - floating.rect[3] / floating.frame.size[1] as f32).abs() < 0.0001,
        "floating pane stretched its glyphs"
    );
    let clip = egui::Rect::from_min_size(
        Pos2::new(floating.clip[0], floating.clip[1]),
        Vec2::new(floating.clip[2], floating.clip[3]),
    );
    let project = |quad: &rustty_render::Quad| {
        [
            floating.rect[0] + quad.rect[0] * scale,
            floating.rect[1] + quad.rect[1] * scale,
            quad.rect[2] * scale,
            quad.rect[3] * scale,
        ]
    };
    let glyph = floating
        .frame
        .quads
        .iter()
        .find(|quad| {
            let [x, y, width, height] = project(quad);
            quad.paint != rustty_render::Paint::Solid
                && width > 0.0
                && height > 0.0
                && clip.contains_rect(egui::Rect::from_min_size(
                    Pos2::new(x, y),
                    Vec2::new(width, height),
                ))
        })
        .expect("floating pane has no complete glyph to inspect");
    let expected = project(glyph);
    let distance = |quad: &rustty_render::Quad| {
        quad.rect
            .iter()
            .zip(expected)
            .map(|(actual, expected)| (*actual - expected).abs())
            .sum::<f32>()
    };
    let nearest = composed
        .frame
        .quads
        .iter()
        .filter(|quad| quad.paint == glyph.paint)
        .min_by(|left, right| distance(left).total_cmp(&distance(right)));
    assert!(
        composed
            .frame
            .quads
            .iter()
            .any(|quad| quad.paint == glyph.paint
                && quad.atlas == glyph.atlas
                && quad
                    .uv
                    .iter()
                    .zip(glyph.uv)
                    .all(|(actual, expected)| (*actual - expected).abs() < 0.000001)
                && quad
                    .rect
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| (*actual - expected).abs() < 0.02)),
        "floating glyph pixels did not use the uniform scale: phase={} scale={scale} clip={:?} expected={expected:?} source={glyph:?} nearest={nearest:?}",
        landing.phase,
        floating.clip,
    );
    if landing.phase >= 0.999 {
        let native = &host.prepared[&landing.target.pane];
        let origin = host.rects[&landing.target.pane].min * host.window.scale_factor() as f32;
        let target = native
            .frame
            .quads
            .iter()
            .find(|quad| {
                let [x, y, width, height] = quad.rect;
                quad.paint != rustty_render::Paint::Solid
                    && width > 0.0
                    && height > 0.0
                    && clip.shrink(1.0).contains_rect(egui::Rect::from_min_size(
                        origin + Vec2::new(x, y),
                        Vec2::new(width, height),
                    ))
            })
            .expect("native pane has no interior glyph to inspect");
        let expected = [
            origin.x + target.rect[0],
            origin.y + target.rect[1],
            target.rect[2],
            target.rect[3],
        ];
        assert!(
            composed
                .frame
                .quads
                .iter()
                .any(|quad| quad.paint == target.paint
                    && quad.atlas == target.atlas
                    && quad
                        .uv
                        .iter()
                        .zip(target.uv)
                        .all(|(actual, expected)| (*actual - expected).abs() < 0.000001)
                    && quad
                        .rect
                        .iter()
                        .zip(expected)
                        .all(|(actual, expected)| (*actual - expected).abs() < 0.02)),
            "near-end floating pixels do not line up with the native pane"
        );
    }
}
