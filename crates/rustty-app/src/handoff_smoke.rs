//! Opt-in, offscreen quit/startup import check using only fixture-owned state.
use super::*;
use rustty_app::handoff::SavedDump;
use std::fs;

const SOURCE_PROMPT: &str = "HANDOFF SOURCE PROMPT>";
const FRESH_OUTPUT: &str = "HANDOFF FRESH SHELL OUTPUT";
const ALT_OUTPUT: &str = "HANDOFF ALTERNATE DISPLAY";
const INVALID_STARTUP_SESSION: &[u8] = b"invalid startup fixture session";

pub(super) fn configure(loaded: &mut LoadedConfig, directory: PathBuf) {
    #[cfg(target_os = "windows")]
    let command = vec!["cmd.exe".into(), "/d".into(), "/q".into()];
    #[cfg(not(target_os = "windows"))]
    let command = vec!["/bin/sh".into(), "-i".into()];
    let command = config::Command::Direct(command);
    loaded.config.command = Some(command.clone());
    loaded.config.initial_command = Some(command);
    loaded.config.shell_integration = config::ShellIntegration::None;
    loaded.config.working_directory = Some(directory);
    loaded.config.stream_deck = false;
    loaded.config.window_save_state = config::WindowSaveState::Always;
    loaded
        .config
        .keybinds
        .retain(|binding| !binding.flags.global);
}

pub(super) fn save_software_capture(painter: &mut painter::Painter, path: &Path) -> Result<()> {
    let mut events = Vec::new();
    painter.handle_screenshots(&mut events);
    let image = events
        .into_iter()
        .find_map(|event| match event {
            egui::Event::Screenshot { image, .. } => Some(image),
            _ => None,
        })
        .ok_or("missing software selector screenshot")?;
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

pub(super) struct Smoke {
    directory: PathBuf,
    role: String,
    stage: u8,
    deadline: Instant,
    pane: Option<Id>,
    older: Option<SavedDump>,
    imported: Option<SavedDump>,
}

impl Smoke {
    pub fn new() -> Option<Self> {
        Some(Self {
            directory: PathBuf::from(std::env::var_os("RUSTTY_HANDOFF_SMOKE_DIR")?),
            role: std::env::var("RUSTTY_HANDOFF_SMOKE_ROLE").unwrap_or_default(),
            stage: 0,
            deadline: Instant::now() + Duration::from_secs(30),
            pane: None,
            older: None,
            imported: None,
        })
    }

    pub fn step(&mut self, app: &mut App, event_loop: &ActiveEventLoop) -> Result<bool> {
        let result = (|| {
            ensure(
                Instant::now() < self.deadline,
                format!(
                    "{} handover fixture timed out in stage {}",
                    self.role, self.stage,
                ),
            )?;
            ensure(app.background_smoke, "handover fixture must run offscreen")?;
            ensure(
                app.windows
                    .values()
                    .map(|host| &host.window)
                    .chain(app.outliner.iter().map(|host| &host.window))
                    .all(|window| window.is_visible() != Some(true) && !window.has_focus()),
                "handover fixture exposed a window or took keyboard focus",
            )?;
            ensure(
                app.state_path.starts_with(&self.directory),
                "workspace path escaped fixture directory",
            )?;
            ensure(
                app.tmuxx.is_empty(),
                "fixture must not connect to a tmuxx server",
            )?;
            match self.role.as_str() {
                "source" => self.source(app, event_loop),
                "cancel" => self.cancel(app, event_loop),
                "requester" => self.requester(app, event_loop),
                "single" => self.single(app),
                "failed" => self.failed(app, event_loop),
                _ => Err(
                    "RUSTTY_HANDOFF_SMOKE_ROLE must be source, cancel, requester, single, or failed"
                        .into(),
                ),
            }
        })();
        if let Err(error) = &result {
            let _ = self.report(serde_json::json!({"passed": false, "stage": self.stage, "error": error.to_string()}));
        }
        result
    }

    fn source(&mut self, app: &mut App, event_loop: &ActiveEventLoop) -> Result<bool> {
        match self.stage {
            0 => {
                if app.panes.is_empty() {
                    return Ok(false);
                }
                ensure(app.panes.len() == 1, "source fixture needs one fresh shell")?;
                let (&id, pane) = app.panes.iter().next().unwrap();
                #[cfg(target_os = "windows")]
                pane.session.write(b"prompt HANDOFF SOURCE PROMPT$G\r")?;
                #[cfg(not(target_os = "windows"))]
                pane.session.write(b"stty -echo; PS1='HANDOFF SOURCE PROMPT> '; printf '\\nHANDOFF SOURCE READY\\n'\n")?;
                self.pane = Some(id);
                self.stage = 1;
            }
            1 => {
                let pane = self.source_pane(app)?;
                let mut terminal = pane.session.terminal()?;
                if !has_line(&terminal, SOURCE_PROMPT) {
                    return Ok(false);
                }
                terminal.reset_from_host();
                for row in 0..128 {
                    terminal.feed(format!("HANDOFF HISTORY {row:04}\r\n").as_bytes());
                }
                terminal.feed(b"\x1b[1;38;2;10;180;90m\x1b]8;;https://example.org/rustty-handoff\x07HANDOFF LINK\x1b]8;;\x07\x1b[0m\r\n");
                terminal
                    .feed(b"HANDOFF PRIMARY PROMPT\x1b[?1049h\x1b[2J\x1b[H\x1b[38;2;200;120;40m");
                terminal.feed(ALT_OUTPUT.as_bytes());
                terminal.feed(b"\x1b[?25l\x1b[?2004h\x1b[?1003h");
                drop(terminal);
                // egui records widget labels only while its interaction debugger is enabled.
                app.context
                    .all_styles_mut(|style| style.debug.show_interactive_widgets = true);
                let dumped = self.dump_with_consent(app, event_loop);
                app.context
                    .all_styles_mut(|style| style.debug.show_interactive_widgets = false);
                let older = dumped?;
                let state = app.handoff.as_ref().ok_or("source dump store missing")?;
                ensure(
                    state.store.dumps()? == [older.clone()],
                    "source needs an empty dump directory",
                )?;
                self.older = Some(older);
                self.stage = 2;
            }
            2 => {
                self.verify_source(app)?;
                let bytes = self.capture_with_tmuxx_window(app)?;
                let state = app.handoff.as_ref().ok_or("source dump store missing")?;
                let latest = state.store.save_dump(&bytes)?;
                let older = self.older.as_ref().unwrap();
                ensure(
                    latest.created_at > older.created_at,
                    "fixture dumps need distinct creation times",
                )?;
                ensure(
                    state.store.dumps()? == [latest.clone(), older.clone()],
                    "saved sessions were not listed newest first",
                )?;
                self.verify_source(app)?;
                app.save();
                let workspace = Workspace::load(&app.state_path)?
                    .ok_or("source did not save its normal workspace")?;
                ensure(
                    workspace.windows.len() == 1 && workspace.windows[0].tabs[0].panes.len() == 1,
                    "source saved the dump's synthetic split layout as its normal workspace",
                )?;
                fs::write(
                    self.directory.join("source-ready.json"),
                    serde_json::to_vec(&Fixture {
                        source_pid: std::process::id(),
                        older: older.clone(),
                        latest,
                    })?,
                )?;
                self.report(serde_json::json!({
                    "passed": true,
                    "renderer": app.painter.description(),
                    "checks": ["session-dump-menu-consent", "dump-cancel-and-escape-save-nothing", "dump-failure-keeps-confirmation", "dump-retry-saves-once", "compressed-saved-dumps", "newest-first", "tmuxx-windows-excluded", "source-shell-alive-until-quit", "source-alternate-display-unchanged", "normal-workspace-saved"],
                }))?;
                return Ok(true);
            }
            _ => unreachable!(),
        }
        Ok(false)
    }

    fn dump_with_consent(&self, app: &mut App, event_loop: &ActiveEventLoop) -> Result<SavedDump> {
        for choice in ["Cancel", "Escape", "Dump Sessions"] {
            app.user_event(event_loop, Event::Platform(PlatformEvent::DumpSessions));
            ensure(
                app.handoff.as_ref().unwrap().store.dumps()?.is_empty(),
                "requesting a session dump saved before confirmation",
            )?;
            let (key, mut host) = app.take_active_host().ok_or("missing dump dialog host")?;
            let result = (|| {
                ensure(
                    matches!(
                        host.confirm,
                        Some(Confirmation::DumpSessions { error: None })
                    ),
                    "session dump menu did not request confirmation",
                )?;
                host.focused = true;
                host.egui.egui_input_mut().focused = true;
                for _ in 0..2 {
                    app.draw(event_loop, &mut host)?;
                }
                for text in [
                    "personally identifiable information (PII)",
                    "remains on disk until another Rustty instance successfully imports it",
                    "Opening Rustty without importing does not remove it",
                ] {
                    let displayed = app.context.viewport_for(host.viewport, |state| {
                        let widgets = &state.prev_pass.widgets;
                        widgets
                            .get_layer(egui::LayerId::new(
                                egui::Order::Middle,
                                egui::Id::new(Some("Dump all sessions?")),
                            ))
                            .any(|widget| {
                                widgets.info(widget.id).is_some_and(|info| {
                                    info.label
                                        .as_ref()
                                        .is_some_and(|label| label.contains(text))
                                })
                            })
                    });
                    ensure(
                        displayed,
                        format!("session dump confirmation omitted {text}"),
                    )?;
                }
                if choice == "Escape" {
                    for pressed in [true, false] {
                        host.egui.egui_input_mut().events.push(egui::Event::Key {
                            key: egui::Key::Escape,
                            physical_key: None,
                            pressed,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        });
                    }
                    app.draw(event_loop, &mut host)?;
                } else {
                    if choice == "Dump Sessions" {
                        let state = app.handoff.take();
                        let clicked = click_dump_button(app, event_loop, &mut host, choice);
                        app.handoff = state;
                        clicked?;
                        ensure(
                            matches!(&host.confirm, Some(Confirmation::DumpSessions { error: Some(error) }) if !error.is_empty()),
                            "failed session dump dismissed its confirmation or lost its error",
                        )?;
                        ensure(
                            app.handoff.as_ref().unwrap().store.dumps()?.is_empty(),
                            "failed session dump created saved state",
                        )?;
                        for _ in 0..2 {
                            app.draw(event_loop, &mut host)?;
                        }
                    }
                    click_dump_button(app, event_loop, &mut host, choice)?;
                }
                ensure(
                    host.confirm.is_none(),
                    "session dump confirmation stayed open",
                )?;
                ensure(
                    app.handoff.as_ref().unwrap().store.dumps()?.len()
                        == usize::from(choice == "Dump Sessions"),
                    "session dump confirmation saved without consent or saved more than once",
                )
            })();
            app.windows.insert(key, host);
            result?;
            self.verify_source(app)?;
        }
        app.handoff
            .as_ref()
            .unwrap()
            .store
            .dumps()?
            .pop()
            .ok_or_else(|| "confirmed session dump was not saved".into())
    }

    fn capture_with_tmuxx_window(&self, app: &mut App) -> Result<Vec<u8>> {
        let window = app.workspace.id();
        let tab = app.workspace.id();
        let pane = app.workspace.id();
        // An offline attachment needs no server or terminal; export must skip it entirely.
        let mut entry = tmuxx_host::Attachment::new(
            tmuxx_host::Source::Ssh("handoff-fixture.invalid".into()),
            None,
            false,
        );
        entry.mapping.sessions.insert(window, 1);
        app.workspace.windows.push(WindowState {
            id: window,
            tabs: vec![Tab::new(tab, pane, self.directory.clone())],
            active_tab: 0,
            frame: [0., 0., 800., 600.],
            quick: false,
        });
        app.tmuxx.push(entry);
        let active = app.active.replace(window);
        let cold_tab = app.workspace.id();
        let cold_pane = app.workspace.id();
        let cold_directory = self.directory.join("unopened-empty");
        fs::create_dir_all(&cold_directory)?;
        app.workspace.windows[0]
            .tabs
            .push(Tab::new(cold_tab, cold_pane, cold_directory));
        let bytes = app.capture_handoff();
        app.tmuxx[0]
            .mapping
            .sessions
            .insert(app.workspace.windows[0].id, 2);
        let empty = app.capture_handoff();
        app.active = active;
        app.tmuxx.pop();
        app.workspace.windows.pop();
        app.workspace.windows[0].tabs.pop();

        let bytes = bytes?;
        let mut dump = Dump::decode(&bytes)?;
        ensure(
            dump.workspace.windows.len() == 1
                && dump.workspace.windows[0].id == app.workspace.windows[0].id
                && dump.panels.len() == 2
                && dump.panels.contains_key(&self.pane.unwrap())
                && dump.panels.contains_key(&cold_pane)
                && dump.active_window.is_none(),
            "export retained a tmuxx window, panel, or active-window reference",
        )?;
        ensure(
            empty.is_err_and(|error| error.to_string() == "This instance has no panels to export"),
            "tmuxx-only export did not report an empty instance",
        )?;
        // Give the receiver differently sized panels without changing the source shell.
        let narrow = dump.workspace.id();
        let split = dump.workspace.id();
        let blank = dump.workspace.id();
        let second_split = dump.workspace.id();
        let narrow_directory = self.directory.join("narrow");
        let blank_directory = self.directory.join("empty");
        fs::create_dir_all(&narrow_directory)?;
        fs::create_dir_all(&blank_directory)?;
        let tab = &mut dump.workspace.windows[0].tabs[0];
        tab.split(narrow, split, Direction::Down, narrow_directory);
        tab.split(blank, second_split, Direction::Right, blank_directory);
        if let workspace::Node::Split { second, .. } = &mut tab.root.kind
            && let workspace::Node::Split { ratio, .. } = &mut second.kind
        {
            *ratio = 0.25;
        }
        let content = dump.panels[&self.pane.unwrap()].visual.clone();
        dump.panels.insert(narrow, Panel { visual: content });
        let empty = vt::Terminal::new(80, 24, 100);
        dump.panels.insert(
            blank,
            Panel {
                visual: serde_json::to_vec(&vt::visual::VisualSnapshot::capture(&empty)?)?,
            },
        );
        let dormant_tab = dump.workspace.id();
        let dormant_pane = dump.workspace.id();
        let dormant_split = dump.workspace.id();
        let dormant_second = dump.workspace.id();
        let dormant_directory = self.directory.join("unopened");
        fs::create_dir_all(&dormant_directory)?;
        let mut tab = Tab::new(dormant_tab, dormant_pane, dormant_directory.clone());
        tab.split(
            dormant_second,
            dormant_split,
            Direction::Right,
            dormant_directory,
        );
        dump.workspace.windows[0].tabs.insert(1, tab);
        for id in [dormant_pane, dormant_second] {
            dump.panels.insert(
                id,
                Panel {
                    visual: dump.panels[&self.pane.unwrap()].visual.clone(),
                },
            );
        }
        Ok(rmp_serde::to_vec_named(&dump)?)
    }

    fn cancel(&mut self, app: &mut App, event_loop: &ActiveEventLoop) -> Result<bool> {
        self.check_selector(app)?;
        let before = app.handoff.as_ref().unwrap().store.dumps()?;
        let workspace =
            Workspace::load(&app.state_path)?.ok_or("cancel fixture has no prior workspace")?;
        app.cancel_import_selector(event_loop);
        ensure(
            !app.import_selector
                && app.workspace.windows.len() == 1
                && app.workspace.windows[0].id == workspace.windows[0].id
                && app.panes.len() == 1,
            "cancel did not open the previous window configuration",
        )?;
        ensure(
            app.handoff.as_ref().unwrap().store.dumps()? == before,
            "cancel consumed saved sessions",
        )?;
        self.check_directories(app)?;
        self.report(serde_json::json!({
            "passed": true,
            "renderer": app.painter.description(),
            "checks": ["startup-selector-without-terminals", "cancel-restores-prior-workspace", "cancel-keeps-all-dumps", "restored-working-directory"],
        }))?;
        Ok(true)
    }

    fn requester(&mut self, app: &mut App, event_loop: &ActiveEventLoop) -> Result<bool> {
        match self.stage {
            0 => {
                self.check_selector(app)?;
                let state = app.handoff.as_mut().unwrap();
                let invalid = state.store.save_dump(b"invalid fixture session")?;
                state.refresh();
                ensure(
                    app.import_handoff(event_loop, &invalid.id).is_err(),
                    "malformed saved session imported successfully",
                )?;
                let state = app.handoff.as_mut().unwrap();
                ensure(
                    state.store.read_dump(&invalid)? == b"invalid fixture session",
                    "failed import consumed its dump",
                )?;
                state.store.remove_dump(&invalid)?;
                state.refresh();
                self.check_selector(app)?;
                let fixture = self.fixture()?;
                let previous_workspace = serde_json::to_vec(&app.workspace)?;
                let missing_command = config::Command::Direct(vec![
                    self.directory
                        .join("missing-shell")
                        .to_string_lossy()
                        .into_owned(),
                ]);
                let command = app.loaded.config.command.replace(missing_command.clone());
                let initial_command = app.loaded.config.initial_command.replace(missing_command);
                let failed = app.import_handoff(event_loop, &fixture.latest.id);
                app.loaded.config.command = command;
                app.loaded.config.initial_command = initial_command;
                ensure(failed.is_err(), "import accepted a missing shell")?;
                ensure(
                    serde_json::to_vec(&app.workspace)? == previous_workspace
                        && app.failed_panes.is_empty()
                        && app.windows.len() == 1
                        && app.windows.values().all(|host| host.id == SELECTOR_WINDOW),
                    "failed shell startup left imported windows or lost the previous workspace",
                )?;
                self.check_selector(app)?;
                self.older = Some(fixture.older);
                self.imported = Some(fixture.latest);
                self.stage = 1;
            }
            1 => {
                // Hidden Windows windows need not receive WM_PAINT. Render the
                // real selector directly before import removes its viewport.
                let key = app
                    .windows
                    .iter()
                    .find(|(_, host)| host.id == SELECTOR_WINDOW)
                    .map(|(key, _)| *key)
                    .ok_or("missing selector window")?;
                let mut host = app.windows.remove(&key).unwrap();
                let drawn = app.draw_import_selector(event_loop, &mut host);
                app.windows.insert(key, host);
                drawn?;
                let imported = self.imported.as_ref().unwrap();
                app.import_handoff(event_loop, &imported.id)?;
                ensure(
                    !app.import_selector && app.panes.len() == 3 && app.pending_visuals.len() == 3,
                    "import did not open the restored workspace",
                )?;
                let state = app.handoff.as_ref().unwrap();
                ensure(
                    state.store.read_dump(imported).is_err()
                        && state.store.dumps()? == [self.older.as_ref().unwrap().clone()],
                    "successful import did not consume only the selected dump",
                )?;
                self.stage = 2;
            }
            2 => {
                // Startup comparison must settle before inspecting the merged display.
                for pane in app.panes.values() {
                    if pane.cwd != self.directory.join("empty")
                        && !pane
                            .session
                            .terminal()?
                            .plain_text()
                            .contains(vt::visual::RESTORED_MARKER)
                    {
                        return Ok(false);
                    }
                }
                let untouched = app.pending_visuals.clone();
                for _ in 0..2 {
                    let exported = Dump::decode(&app.capture_handoff()?)?;
                    for (id, visual) in &untouched {
                        ensure(
                            exported.panels[id].visual.as_slice() == visual.as_slice(),
                            "export changed an unopened panel's saved contents",
                        )?;
                        let mut display = vt::Terminal::new(80, 24, 1000);
                        let snapshot: vt::visual::VisualSnapshot = serde_json::from_slice(visual)?;
                        snapshot.restore_into(&mut display)?;
                        ensure(
                            !display.plain_text().contains(vt::visual::RESTORED_MARKER),
                            "unopened panel accumulated a separator",
                        )?;
                    }
                    app.reconcile(event_loop);
                    ensure(
                        app.panes.len() == 3 && app.pending_visuals == untouched,
                        "export or reconciliation started an inactive tab",
                    )?;
                }
                self.check_directories(app)?;
                let mut widths = Vec::new();
                let mut blank_panels = 0;
                for pane in app.panes.values() {
                    let terminal = pane.session.terminal()?;
                    let mut formatter = terminal.formatter(vt::formatter::Format::Plain);
                    formatter.options.unwrap = true;
                    let text =
                        String::from_utf8(formatter.format().ok_or("missing restored display")?)?;
                    if text.contains(ALT_OUTPUT) {
                        widths.push(terminal.cols);
                        let banner = text
                            .lines()
                            .find(|line| line.contains(vt::visual::RESTORED_MARKER))
                            .ok_or("split panel lost its restored banner")?;
                        ensure(
                            banner.chars().count()
                                == usize::from(terminal.cols)
                                    .max(vt::visual::RESTORED_MARKER.len()),
                            "split panel banner used the wrong columns",
                        )?;
                    } else {
                        blank_panels += 1;
                        ensure(
                            !text.contains(vt::visual::RESTORED_MARKER),
                            "blank imported panel received a banner",
                        )?;
                    }
                }
                ensure(
                    blank_panels == 1
                        && widths
                            .iter()
                            .any(|cols| usize::from(*cols) < vt::visual::RESTORED_MARKER.len())
                        && widths
                            .iter()
                            .any(|cols| usize::from(*cols) > vt::visual::RESTORED_MARKER.len()),
                    "import fixture did not exercise wide, narrow, and blank panels",
                )?;
                let id = app.workspace.windows[0].tabs[0].root.panes()[0];
                let pane = &app.panes[&id];
                ensure(!pane.session.has_exited(), "new local shell exited")?;
                #[cfg(target_os = "windows")]
                // Escape one letter so a command-line echo cannot pass the output check.
                pane.session.write(b"echo HANDOFF FRESH SHELL OUT^PUT\r")?;
                #[cfg(not(target_os = "windows"))]
                pane.session
                    .write(b"printf '\\nHANDOFF FRESH SHELL OUTPUT\\n'\n")?;
                self.pane = Some(id);
                self.stage = 3;
            }
            3 => {
                let pane = app
                    .panes
                    .get(&self.pane.unwrap())
                    .ok_or("imported panel disappeared")?;
                let terminal = pane.session.terminal()?;
                if !has_line(&terminal, FRESH_OUTPUT) {
                    return Ok(false);
                }
                ensure(
                    !terminal.is_alternate_screen(),
                    "imported shell retained alternate-screen mode",
                )?;
                ensure(
                    !terminal.modes.dec(2004) && terminal.mouse_mode == 0,
                    "imported shell retained old keyboard or mouse modes",
                )?;
                ensure(
                    terminal.screen().cursor.visible
                        && terminal.screen().cursor.style == Default::default()
                        && terminal.screen().cursor.hyperlink.is_none(),
                    "imported shell retained old cursor or styling state",
                )?;
                let mut formatter = terminal.formatter(vt::formatter::Format::Plain);
                formatter.options.unwrap = true;
                let text = String::from_utf8(
                    formatter
                        .format()
                        .ok_or("could not format imported terminal")?,
                )?;
                let mut last = 0;
                for expected in [
                    "HANDOFF HISTORY 0000",
                    "HANDOFF HISTORY 0127",
                    "HANDOFF LINK",
                    "HANDOFF PRIMARY PROMPT",
                    ALT_OUTPUT,
                    vt::visual::RESTORED_MARKER,
                    FRESH_OUTPUT,
                ] {
                    let found = text
                        .find(expected)
                        .ok_or_else(|| format!("import lost {expected:?}"))?;
                    ensure(
                        found >= last,
                        "restored output and new child output were out of order",
                    )?;
                    last = found;
                }
                let link = terminal
                    .screen()
                    .all_rows()
                    .find(|row| terminal.screen().row_text(row).contains("HANDOFF LINK"))
                    .ok_or("missing styled link row")?;
                ensure(
                    link.style(0).bold && link.style(0).foreground == vt::Color::Rgb(10, 180, 90),
                    "import lost the old display style",
                )?;
                ensure(
                    link.hyperlink(0)
                        .is_some_and(|link| link.uri == "https://example.org/rustty-handoff"),
                    "import lost its hyperlink",
                )?;
                let banner = terminal
                    .screen()
                    .all_rows()
                    .map(|row| terminal.screen().row_text(row))
                    .find(|text| text.contains(vt::visual::RESTORED_MARKER))
                    .ok_or("restored banner did not fit the receiving panel")?;
                ensure(
                    banner.chars().count() == usize::from(terminal.cols),
                    format!(
                        "restored banner width {} did not match panel columns {}",
                        banner.chars().count(),
                        terminal.cols
                    ),
                )?;
                let (left, right) = banner.split_once(vt::visual::RESTORED_MARKER).unwrap();
                ensure(
                    left.chars().count().abs_diff(right.chars().count()) <= 1,
                    "restored banner was not centered",
                )?;
                drop(terminal);
                ensure(
                    self.directory.join("selector.png").is_file(),
                    "selector rendering did not produce its capture",
                )?;
                let untouched = app.pending_visuals.clone();
                let window = app.workspace.windows[0].id;
                let starting = app.workspace.windows[0].tabs[1].pane_ids();
                app.workspace.windows[0].active_tab = 1;
                app.reconcile(event_loop);
                ensure(
                    app.panes.len() == 3
                        && app.pending_visuals == untouched
                        && app.starting_panes.len() == starting.len()
                        && starting
                            .iter()
                            .all(|id| app.starting_panes.contains_key(id)),
                    "opening an inactive split tab did not defer its shells",
                )?;
                let workers: Vec<_> = starting
                    .iter()
                    .map(|id| (*id, app.starting_panes[id].0.wake_pending.clone()))
                    .collect();
                // Stay in this callback so queued worker completions cannot install panes yet.
                for active_tab in [1, 0, 1] {
                    app.workspace.windows[0].active_tab = active_tab;
                    app.reconcile(event_loop);
                    let expected = app.workspace.windows[0].tabs[active_tab].visible_panes(false);
                    let key = app
                        .windows
                        .iter()
                        .find_map(|(key, host)| (host.id == window).then_some(*key))
                        .ok_or("missing restored window")?;
                    let mut host = app.windows.remove(&key).unwrap();
                    let drawn = app.draw(event_loop, &mut host);
                    let displayed = host.pane_order == expected;
                    app.windows.insert(key, host);
                    drawn?;
                    ensure(displayed, "tab switch did not draw the selected panels")?;
                    ensure(
                        app.panes.len() == 3
                            && app.starting_panes.len() == starting.len()
                            && app.pending_visuals == untouched,
                        "tab switching or drawing waited for pending shells",
                    )?;
                    ensure(
                        workers.iter().all(|(id, wake)| {
                            app.starting_panes.get(id).is_some_and(|(startup, _)| {
                                Arc::ptr_eq(wake, &startup.wake_pending)
                            })
                        }),
                        "reconciliation replaced a pending shell startup",
                    )?;
                }
                let exported = Dump::decode(&app.capture_handoff()?)?;
                for (id, visual) in &untouched {
                    ensure(
                        exported.panels[id].visual.as_slice() == visual.as_slice(),
                        "export changed a starting panel's saved contents",
                    )?;
                }
                self.stage = 4;
            }
            4 => {
                let tab = &app.workspace.windows[0].tabs[1];
                for id in tab.panes.keys() {
                    let Some(pane) = app.panes.get(id) else {
                        return Ok(false);
                    };
                    let terminal = pane.session.terminal()?;
                    if !terminal.plain_text().contains(vt::visual::RESTORED_MARKER) {
                        return Ok(false);
                    }
                    ensure(
                        has_line(&terminal, ALT_OUTPUT),
                        "lazy startup lost saved display",
                    )?;
                }
                ensure(
                    app.panes.len() == 5
                        && app.pending_visuals.len() == 1
                        && app.starting_panes.is_empty(),
                    "opening an inactive split tab did not start only its shells",
                )?;
                let id = app.workspace.windows[0].tabs[2].focused;
                let command = app
                    .loaded
                    .config
                    .command
                    .replace(config::Command::Direct(vec![
                        self.directory
                            .join("missing-lazy-shell")
                            .to_string_lossy()
                            .into_owned(),
                    ]));
                app.write(id, Vec::new());
                app.loaded.config.command = command;
                self.stage = 5;
            }
            5 => {
                let id = app.workspace.windows[0].tabs[2].focused;
                if !app.failed_panes.contains_key(&id) {
                    return Ok(false);
                }
                ensure(
                    !app.panes.contains_key(&id)
                        && app.starting_panes.is_empty()
                        && app.pending_visuals.contains_key(&id),
                    "failed background startup lost its saved display",
                )?;
                let exported = Dump::decode(&app.capture_handoff()?)?;
                ensure(
                    exported.panels[&id].visual.as_slice() == app.pending_visuals[&id].as_slice(),
                    "export lost the failed panel's saved display",
                )?;
                app.failed_panes.remove(&id);
                #[cfg(target_os = "windows")]
                app.write(id, b"echo HANDOFF LAZY IN^PUT\r".to_vec());
                #[cfg(not(target_os = "windows"))]
                app.write(id, b"printf '\\nHANDOFF LAZY INPUT\\n'\n".to_vec());
                ensure(
                    app.panes.len() == 5
                        && app.pending_visuals.len() == 1
                        && app.starting_panes.len() == 1
                        && app.starting_panes.contains_key(&id),
                    "explicit input did not defer its unopened destination",
                )?;
                self.stage = 6;
            }
            6 => {
                let id = app.workspace.windows[0].tabs[2].focused;
                let Some(pane) = app.panes.get(&id) else {
                    return Ok(false);
                };
                if !has_line(&*pane.session.terminal()?, "HANDOFF LAZY INPUT") {
                    return Ok(false);
                }
                ensure(
                    app.panes.len() == 6
                        && app.pending_visuals.is_empty()
                        && app.starting_panes.is_empty(),
                    "completed lazy startup left pending panels",
                )?;
                self.check_directories(app)?;
                let history_rows = app.panes[&self.pane.unwrap()]
                    .session
                    .terminal()?
                    .screen()
                    .history_len();
                self.report(serde_json::json!({
                    "passed": true,
                    "renderer": app.painter.description(),
                    "source_pid": self.fixture()?.source_pid,
                    "imported_dump": self.imported.as_ref().unwrap().id,
                    "history_rows": history_rows,
                    "checks": ["automatic-startup-selector", "startup-without-terminals", "newest-first", "failed-import-keeps-dump", "failed-shell-startup-restores-selector", "lz4-dump", "selector-capture", "workspace-import", "working-directories", "primary-history", "alternate-display", "styles-and-hyperlinks", "fresh-shell-after-marker", "centered-panel-width-marker", "narrow-marker-wraps-without-padding", "blank-panel-omits-marker", "clean-terminal-modes", "import-consumes-only-selected-dump", "unopened-tabs-without-shells", "unchanged-unopened-panel-export", "lazy-tab-activation", "tab-switch-draws-before-shells-start", "switch-away-and-back-during-startup", "reconcile-keeps-pending-startups", "unchanged-starting-panel-export", "failed-background-startup-preserves-display", "retry-background-startup", "explicit-input-queued-for-unopened-pane"],
                }))?;
                return Ok(true);
            }
            _ => unreachable!(),
        }
        Ok(false)
    }

    fn single(&self, app: &App) -> Result<bool> {
        ensure(
            !app.import_selector
                && app.windows.len() == 1
                && app.windows.values().all(|host| host.id != SELECTOR_WINDOW)
                && app.workspace.windows.len() == 1
                && app.workspace.windows[0].tabs.len() == 1
                && app.workspace.windows[0].tabs[0].panes.len() == 1
                && app.panes.len() == 1
                && app.pending_visuals.is_empty(),
            "single saved session did not import at startup without a chooser",
        )?;
        let fixture = self.fixture()?;
        let state = app.handoff.as_ref().ok_or("dump store missing")?;
        ensure(
            state.dumps.is_empty()
                && state.store.dumps()?.is_empty()
                && state.store.read_dump(&fixture.older).is_err(),
            "automatic import did not consume the only remaining dump",
        )?;
        let pane = app.panes.values().next().unwrap();
        let terminal = pane.session.terminal()?;
        let text = terminal.plain_text();
        if !text.contains(vt::visual::RESTORED_MARKER) {
            return Ok(false);
        }
        for expected in ["HANDOFF HISTORY 0000", "HANDOFF HISTORY 0127", ALT_OUTPUT] {
            ensure(
                has_line(&terminal, expected),
                format!("automatic import lost the older dump's {expected:?}"),
            )?;
        }
        ensure(
            !pane.session.has_exited()
                && fs::canonicalize(&pane.cwd)? == fs::canonicalize(&self.directory)?,
            "automatic import did not start a shell in the older dump's directory",
        )?;
        drop(terminal);
        self.check_directories(app)?;
        let invalid = state.store.save_dump(INVALID_STARTUP_SESSION)?;
        ensure(
            state.store.dumps()? == [invalid.clone()],
            "failed-startup fixture needs exactly one malformed dump",
        )?;
        self.report(serde_json::json!({
            "passed": true,
            "renderer": app.painter.description(),
            "imported_dump": fixture.older.id,
            "seeded_invalid_dump": invalid.id,
            "checks": ["single-dump-auto-import", "single-dump-skips-selector", "older-display-and-directory", "single-dump-consumed"],
        }))?;
        Ok(true)
    }

    fn failed(&self, app: &mut App, event_loop: &ActiveEventLoop) -> Result<bool> {
        ensure(
            app.import_selector
                && app.panes.is_empty()
                && app.pending_visuals.is_empty()
                && app.windows.len() == 1
                && app.windows.values().all(|host| host.id == SELECTOR_WINDOW),
            "failed automatic import did not leave only the selector without shells",
        )?;
        let selector = *app.windows.keys().next().unwrap();
        let workspace = serde_json::to_vec(&app.workspace)?;
        let state = app.handoff.as_mut().ok_or("dump store missing")?;
        let invalid = state
            .dumps
            .first()
            .cloned()
            .ok_or("failed import lost its dump")?;
        ensure(
            state.dumps == [invalid.clone()]
                && state.store.read_dump(&invalid)? == INVALID_STARTUP_SESSION,
            "failed automatic import consumed or changed its dump",
        )?;
        let error = state
            .error
            .take()
            .ok_or("failed automatic import did not show its error")?;
        ensure(!error.is_empty(), "automatic import error was empty")?;
        // A repeated attempt would put the decode error back, even though the
        // unchanged malformed dump cannot produce another observable result.
        for _ in 0..2 {
            app.resumed(event_loop);
        }
        app.handoff.as_mut().unwrap().next_refresh = Instant::now();
        app.tick_import_selector();
        let state = app.handoff.as_mut().unwrap();
        ensure(
            state.error.is_none(),
            "resume or selector refresh retried the failed automatic import",
        )?;
        ensure(
            state.dumps == [invalid.clone()]
                && state.store.dumps()? == [invalid.clone()]
                && state.store.read_dump(&invalid)? == INVALID_STARTUP_SESSION
                && app.import_selector
                && app.panes.is_empty()
                && app.pending_visuals.is_empty()
                && app.windows.len() == 1
                && app.windows.contains_key(&selector)
                && serde_json::to_vec(&app.workspace)? == workspace,
            "failed startup retry checks changed the dump, chooser, or prior workspace",
        )?;
        state.error = Some(error);
        let mut host = app.windows.remove(&selector).unwrap();
        let drawn = app.draw_import_selector(event_loop, &mut host);
        app.windows.insert(selector, host);
        drawn?;
        self.report(serde_json::json!({
            "passed": true,
            "renderer": app.painter.description(),
            "preserved_dump": invalid.id,
            "checks": ["single-dump-failure-shows-selector-error", "failed-startup-without-shells", "failed-startup-keeps-dump", "resume-and-refresh-do-not-retry"],
        }))?;
        Ok(true)
    }

    fn fixture(&self) -> Result<Fixture> {
        Ok(serde_json::from_slice(&fs::read(
            self.directory.join("source-ready.json"),
        )?)?)
    }

    fn check_selector(&self, app: &App) -> Result<()> {
        ensure(
            app.import_selector,
            "startup did not automatically open the import selector",
        )?;
        ensure(
            app.panes.is_empty()
                && app.windows.len() == 1
                && app.windows.values().all(|host| host.id == SELECTOR_WINDOW),
            "startup opened terminal windows before import",
        )?;
        let fixture = self.fixture()?;
        let state = app.handoff.as_ref().ok_or("dump store missing")?;
        ensure(
            state.dumps == [fixture.latest.clone(), fixture.older],
            "selector did not offer persisted sessions newest first",
        )?;
        ensure(
            fixture.latest.bytes > 0,
            "saved session has no compressed content",
        )?;
        Dump::decode(&state.store.read_dump(&fixture.latest)?)?;
        Ok(())
    }

    fn check_directories(&self, app: &App) -> Result<()> {
        let directory = fs::canonicalize(&self.directory)?;
        for window in &app.workspace.windows {
            for tab in &window.tabs {
                for (id, saved) in &tab.panes {
                    let saved_directory = fs::canonicalize(&saved.working_directory)?;
                    ensure(
                        saved_directory.starts_with(&directory)
                            && if let Some(pane) = app.panes.get(id) {
                                fs::canonicalize(&pane.cwd)? == saved_directory
                            } else {
                                app.pending_visuals.contains_key(id)
                            },
                        "restoration lost a panel's working directory",
                    )?;
                }
            }
        }
        Ok(())
    }

    fn source_pane<'a>(&self, app: &'a App) -> Result<&'a Pane> {
        app.panes
            .get(&self.pane.ok_or("source panel not initialized")?)
            .ok_or_else(|| "source panel disappeared".into())
    }

    fn verify_source(&self, app: &App) -> Result<()> {
        let pane = self.source_pane(app)?;
        ensure(
            !pane.session.has_exited(),
            "source shell was terminated by handover",
        )?;
        let terminal = pane.session.terminal()?;
        ensure(
            terminal.is_alternate_screen(),
            "handover changed the source screen",
        )?;
        ensure(
            has_line(&terminal, ALT_OUTPUT),
            "handover changed the source display",
        )?;
        ensure(
            terminal.primary_screen().all_rows().any(|row| {
                terminal.primary_screen().row_text(row).trim_end() == "HANDOFF HISTORY 0000"
            }),
            "handover changed source scrollback",
        )
    }

    fn report(&self, mut report: serde_json::Value) -> Result<()> {
        report["role"] = self.role.clone().into();
        fs::write(
            self.directory.join(format!("{}-result.json", self.role)),
            serde_json::to_vec_pretty(&report)?,
        )?;
        Ok(())
    }
}

fn has_line(terminal: &vt::Terminal, expected: &str) -> bool {
    terminal
        .screen()
        .all_rows()
        .any(|row| terminal.screen().row_text(row).trim_end() == expected)
}

fn click_dump_button(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    host: &mut Host,
    label: &str,
) -> Result<()> {
    let point = app
        .context
        .viewport_for(host.viewport, |state| {
            let widgets = &state.prev_pass.widgets;
            widgets
                .get_layer(egui::LayerId::new(
                    egui::Order::Middle,
                    egui::Id::new(Some("Dump all sessions?")),
                ))
                .find(|widget| {
                    widgets.info(widget.id).is_some_and(|info| {
                        info.typ == egui::WidgetType::Button && info.label.as_deref() == Some(label)
                    })
                })
                .map(|widget| widget.interact_rect.center())
        })
        .ok_or_else(|| format!("session dump confirmation has no {label} button"))?;
    for event in [
        egui::Event::PointerMoved(point),
        egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        },
    ] {
        host.egui.egui_input_mut().events.push(event);
        app.draw(event_loop, host)?;
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Fixture {
    source_pid: u32,
    older: SavedDump,
    latest: SavedDump,
}

fn ensure(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into().into())
    }
}
