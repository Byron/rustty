//! Opt-in, offscreen two-process handover check using only fixture-owned state.
use super::*;
use std::fs;

const SOURCE_PROMPT: &str = "HANDOFF SOURCE PROMPT>";
const FRESH_OUTPUT: &str = "HANDOFF FRESH SHELL OUTPUT";
const ALT_OUTPUT: &str = "HANDOFF ALTERNATE DISPLAY";

pub(super) struct Smoke {
    directory: PathBuf,
    role: String,
    stage: u8,
    deadline: Instant,
    pane: Option<Id>,
    request: Option<Request>,
    cancelled: Option<String>,
    exports: usize,
}

impl Smoke {
    pub fn new() -> Option<Self> {
        Some(Self {
            directory: PathBuf::from(std::env::var_os("RUSTTY_HANDOFF_SMOKE_DIR")?),
            role: std::env::var("RUSTTY_HANDOFF_SMOKE_ROLE").unwrap_or_default(),
            stage: 0,
            deadline: Instant::now() + Duration::from_secs(20),
            pane: None,
            request: None,
            cancelled: None,
            exports: 0,
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
                "requester" => self.requester(app, event_loop),
                _ => Err("RUSTTY_HANDOFF_SMOKE_ROLE must be source or requester".into()),
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
                let state = app.handoff.as_ref().ok_or("source registry missing")?;
                fs::write(
                    self.directory.join("source-ready.json"),
                    serde_json::to_vec(state.registry.instance())?,
                )?;
                self.stage = 2;
            }
            2 => {
                self.verify_source(app)?;
                if let Some(prompt) = app
                    .windows
                    .values_mut()
                    .find_map(|host| host.handoff_prompt.take())
                {
                    let bytes = app.capture_handoff()?;
                    let state = app.handoff.as_mut().ok_or("source registry missing")?;
                    state.registry.respond(&prompt.request, Some(&bytes))?;
                    self.exports += 1;
                    if self.exports == 2 {
                        state.registry.ignore_requests(true)?;
                    }
                    self.verify_source(app)?;
                    fs::write(
                        self.directory
                            .join(format!("source-export-{}.json", self.exports)),
                        serde_json::to_vec(&serde_json::json!({
                            "request": prompt.request.id, "source_alive": true, "alternate_active": true,
                        }))?,
                    )?;
                }
                if self.directory.join("requester-done").exists() {
                    ensure(self.exports == 2, "source did not confirm both requests")?;
                    ensure(
                        app.handoff
                            .as_ref()
                            .unwrap()
                            .registry
                            .instance()
                            .ignoring_requests,
                        "source did not retain its ignore-further-requests preference",
                    )?;
                    self.report(serde_json::json!({
                        "passed": true,
                        "checks": ["native-signal-request", "source-confirmation", "two-independent-exports", "source-shell-alive", "source-alternate-display-unchanged", "ignore-further-requests"],
                        "exports": self.exports,
                    }))?;
                    return Ok(true);
                }
            }
            _ => unreachable!(),
        }
        Ok(false)
    }

    fn requester(&mut self, app: &mut App, event_loop: &ActiveEventLoop) -> Result<bool> {
        match self.stage {
            0 => {
                ensure(
                    app.import_selector,
                    "requester did not open its import selector",
                )?;
                ensure(
                    app.workspace.windows.is_empty()
                        && app.workspace.tmuxx.is_empty()
                        && app.panes.is_empty(),
                    "requester spawned terminals or loaded an existing workspace before import",
                )?;
                let path = self.directory.join("source-ready.json");
                if !path.exists() {
                    return Ok(false);
                }
                let advertised: Instance = serde_json::from_slice(&fs::read(path)?)?;
                let state = app.handoff.as_mut().ok_or("requester registry missing")?;
                state.refresh();
                let source = state
                    .instances
                    .iter()
                    .find(|instance| instance.id == advertised.id)
                    .ok_or("source was not discovered in the isolated registry")?
                    .clone();
                self.ask(state, &source)?;
                self.stage = 1;
            }
            1 => {
                let request = self.request.as_ref().unwrap();
                let state = app.handoff.as_mut().unwrap();
                let RequestStatus::Ready { path, bytes } = state.registry.status(request)? else {
                    return self.waiting(state, request);
                };
                ensure(
                    bytes > 0 && path.to_string_lossy().ends_with(".rustty-state.lz4"),
                    "export was not a compressed ready file",
                )?;
                Dump::decode(&state.registry.read_dump(request)?)?;
                let source = request.source.clone();
                self.cancelled = Some(request.id.clone());
                state.registry.cancel_all()?;
                ensure(
                    !path.exists() && !request_file(&path, request).exists(),
                    "cancelled request left a dump or request behind",
                )?;
                ensure(
                    app.panes.is_empty() && app.workspace.windows.is_empty(),
                    "cancelled import created a workspace",
                )?;
                state.requests.clear();
                self.ask(state, &source)?;
                self.stage = 2;
            }
            2 => {
                let request = self.request.as_ref().unwrap().clone();
                let state = app.handoff.as_ref().unwrap();
                let RequestStatus::Ready { path, .. } = state.registry.status(&request)? else {
                    return self.waiting(state, &request);
                };
                ensure(
                    state.rows().iter().any(|row| {
                        row.id == request.source.id
                            && matches!(row.status, picker::Status::Ready { .. })
                    }),
                    "source selector row did not change to Import",
                )?;
                app.import_handoff(event_loop, &request.source.id)?;
                ensure(
                    !app.import_selector && app.panes.len() == 1,
                    "import did not open the restored workspace",
                )?;
                ensure(
                    !path.exists() && !request_file(&path, &request).exists(),
                    "successful import did not remove its files",
                )?;
                let (&id, pane) = app.panes.iter().next().unwrap();
                ensure(!pane.session.has_exited(), "new local shell exited")?;
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
                drop(terminal);
                let request = self.request.as_ref().unwrap();
                let state = app.handoff.as_ref().unwrap();
                let source = state
                    .registry
                    .instances()?
                    .into_iter()
                    .find(|instance| instance.id == request.source.id)
                    .ok_or("source process was closed by import")?;
                // The second approval also checked Ignore further requests.
                if !source.ignoring_requests {
                    return Ok(false);
                }
                ensure(
                    state.registry.ask(&source).is_err(),
                    "source accepted a request after choosing Ignore",
                )?;
                ensure(
                    state.requests.is_empty(),
                    "completed requests remain selected",
                )?;
                self.report(serde_json::json!({
                    "passed": true,
                    "source_pid": source.pid,
                    "cancelled_request": self.cancelled,
                    "imported_request": request.id,
                    "history_rows": history_rows,
                    "checks": ["import-flag-starts-empty", "same-user-discovery", "ask-confirm-ready", "lz4-dump", "cancel-removes-dump", "selector-import-state", "workspace-import", "primary-history", "alternate-display", "styles-and-hyperlinks", "fresh-shell-after-marker", "clean-terminal-modes", "import-removes-dump", "source-process-alive", "ignore-further-requests"],
                }))?;
                fs::write(self.directory.join("requester-done"), [])?;
                return Ok(true);
            }
            _ => unreachable!(),
        }
        Ok(false)
    }

    fn ask(&mut self, state: &mut State, source: &Instance) -> Result<()> {
        let request = state.registry.ask(source)?;
        state.selected = Some(source.id.clone());
        state.requests.insert(source.id.clone(), request.clone());
        self.request = Some(request);
        Ok(())
    }

    fn waiting(&self, state: &State, request: &Request) -> Result<bool> {
        match state.registry.status(request)? {
            RequestStatus::Waiting => Ok(false),
            other => Err(format!("unexpected export status: {other:?}").into()),
        }
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

fn request_file(dump: &Path, request: &Request) -> PathBuf {
    dump.with_file_name(format!("{}.request", request.id))
}

fn ensure(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into().into())
    }
}
