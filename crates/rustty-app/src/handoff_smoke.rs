//! Opt-in, offscreen quit/startup import check using only fixture-owned state.
use super::*;
use rustty_app::handoff::SavedDump;
use std::fs;

const SOURCE_PROMPT: &str = "HANDOFF SOURCE PROMPT>";
const FRESH_OUTPUT: &str = "HANDOFF FRESH SHELL OUTPUT";
const ALT_OUTPUT: &str = "HANDOFF ALTERNATE DISPLAY";

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
                app.state_path.starts_with(&self.directory),
                "workspace path escaped fixture directory",
            )?;
            ensure(
                app.tmuxx.is_empty(),
                "fixture must not connect to a tmuxx server",
            )?;
            match self.role.as_str() {
                "source" => self.source(app),
                "cancel" => self.cancel(app, event_loop),
                "requester" => self.requester(app, event_loop),
                _ => Err("RUSTTY_HANDOFF_SMOKE_ROLE must be source, cancel, or requester".into()),
            }
        })();
        if let Err(error) = &result {
            let _ = self.report(serde_json::json!({"passed": false, "stage": self.stage, "error": error.to_string()}));
        }
        result
    }

    fn source(&mut self, app: &mut App) -> Result<bool> {
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
                let older = app.dump_session_state()?;
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
                    "checks": ["session-dump-hook", "compressed-saved-dumps", "newest-first", "tmuxx-windows-excluded", "source-shell-alive-until-quit", "source-alternate-display-unchanged", "normal-workspace-saved"],
                }))?;
                return Ok(true);
            }
            _ => unreachable!(),
        }
        Ok(false)
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
        let bytes = app.capture_handoff();
        app.tmuxx[0]
            .mapping
            .sessions
            .insert(app.workspace.windows[0].id, 2);
        let empty = app.capture_handoff();
        app.active = active;
        app.tmuxx.pop();
        app.workspace.windows.pop();

        let bytes = bytes?;
        let mut dump = Dump::decode(&bytes)?;
        ensure(
            dump.workspace.windows.len() == 1
                && dump.workspace.windows[0].id == app.workspace.windows[0].id
                && dump.panels.len() == 1
                && dump.panels.contains_key(&self.pane.unwrap())
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
                    !app.import_selector && app.panes.len() == 3,
                    "import did not open the restored workspace",
                )?;
                let state = app.handoff.as_ref().unwrap();
                ensure(
                    state.store.read_dump(imported).is_err()
                        && state.store.dumps()? == [self.older.as_ref().unwrap().clone()],
                    "successful import did not consume only the selected dump",
                )?;
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
                self.stage = 2;
            }
            2 => {
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
                let history_rows = terminal.screen().history_len();
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
                self.report(serde_json::json!({
                    "passed": true,
                    "renderer": app.painter.description(),
                    "source_pid": self.fixture()?.source_pid,
                    "imported_dump": self.imported.as_ref().unwrap().id,
                    "history_rows": history_rows,
                    "checks": ["automatic-startup-selector", "startup-without-terminals", "newest-first", "failed-import-keeps-dump", "failed-shell-startup-restores-selector", "lz4-dump", "selector-capture", "workspace-import", "working-directories", "primary-history", "alternate-display", "styles-and-hyperlinks", "fresh-shell-after-marker", "centered-panel-width-marker", "narrow-marker-wraps-without-padding", "blank-panel-omits-marker", "clean-terminal-modes", "import-consumes-only-selected-dump"],
                }))?;
                return Ok(true);
            }
            _ => unreachable!(),
        }
        Ok(false)
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
                            && fs::canonicalize(&app.panes[id].cwd)? == saved_directory,
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
