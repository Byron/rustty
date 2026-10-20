//! Pane process lifecycle decisions; the runtime owns descriptors and children.
use super::*;

pub(super) fn environment_overrides(args: &Args) -> Result<BTreeMap<String, String>, String> {
    let mut environment = BTreeMap::new();
    for value in args.repeated.get(&b'e').into_iter().flatten().flatten() {
        let value = std::str::from_utf8(value).map_err(|_| "invalid environment")?;
        if let Some((name, value)) = value.split_once('=') {
            environment.insert(name.into(), value.into());
        }
    }
    Ok(environment)
}

impl Engine {
    pub(super) fn pane_environment(
        &self,
        sid: Id,
        context: &ClientContext,
        overrides: BTreeMap<String, String>,
    ) -> BTreeMap<String, String> {
        let mut environment = self.spawn_environment(sid);
        environment.insert(
            "TERM".into(),
            self.server_options
                .get("default-terminal")
                .cloned()
                .unwrap_or_else(|| "tmux-256color".into()),
        );
        environment.insert("TERM_PROGRAM".into(), "tmuxx".into());
        environment.insert(
            "TERM_PROGRAM_VERSION".into(),
            env!("CARGO_PKG_VERSION").into(),
        );
        environment.insert("COLORTERM".into(), "truecolor".into());
        environment.extend(overrides);
        if !context.attached
            && let Some(path) = context.environment.get("PATH")
        {
            environment.insert("PATH".into(), path.clone());
        }
        let session = &self.sessions[&sid];
        if !environment.contains_key("PATH")
            && !self.environment.contains_key("PATH")
            && !self.removed_environment.contains("PATH")
            && !session.environment.contains_key("PATH")
            && !session.removed_environment.contains("PATH")
        {
            environment.insert("PATH".into(), self.default_child_path.clone());
        }
        environment
    }
    pub(super) fn empty_requested(args: &Args, input: bool) -> Result<bool, String> {
        let empty = args.has(b'E')
            || (input && args.has(b'I'))
            || args.rest.as_slice() == [Vec::<u8>::new()];
        if empty && !args.rest.is_empty() && args.rest.as_slice() != [Vec::<u8>::new()] {
            return Err("command cannot be given for empty pane".into());
        }
        Ok(empty)
    }

    pub(super) fn make_empty(&mut self, id: Id, out: &mut Outcome) {
        let pane = self.panes.get_mut(&id).unwrap();
        pane.empty = true;
        pane.pid = Some(0);
        pane.tty.clear();
        pane.status = None;
        pane.dead_status = None;
        pane.dead_signal = None;
        pane.dead_time = None;
        pane.terminal.feed(b"\x1b[?25l\x1b[20h");
        out.effects
            .retain(|effect| !matches!(effect, Effect::Spawn { pane, .. } if *pane == id));
        out.effects.push(Effect::Empty { pane: id });
    }

    pub(super) fn respawn(
        &mut self,
        args: &[Vec<u8>],
        ctx: &ClientContext,
        whole_window: bool,
    ) -> Result<Outcome, String> {
        let a = Args::parse(args, "c:e:Ekt:")?;
        let empty = a.has(b'E');
        let target = a.string(b't');
        let (sid, index, wid, id) = if whole_window {
            let (sid, index, wid) = self.window_target(target.as_deref(), ctx)?;
            (sid, index, wid, self.windows[&wid].panes[0])
        } else {
            let (sid, wid, id) = self.pane_context(target.as_deref(), ctx)?;
            let index = self.window_target(target.as_deref(), ctx)?.1;
            (sid, index, wid, id)
        };
        let alive = |pane: &Pane| !pane.empty && pane.status.is_none();
        if !a.has(b'k') {
            if whole_window
                && self.windows[&wid]
                    .panes
                    .iter()
                    .any(|id| alive(&self.panes[id]))
            {
                return Err(format!(
                    "respawn window failed: window {}:{index} still active",
                    self.sessions[&sid].name
                ));
            }
            if !whole_window && alive(&self.panes[&id]) {
                let pane_index = self.windows[&wid]
                    .panes
                    .iter()
                    .position(|pid| *pid == id)
                    .unwrap() as u32
                    + self
                        .pane_option(id, "pane-base-index")
                        .and_then(|value| value.parse::<u32>().ok())
                        .unwrap_or(0);
                return Err(format!(
                    "respawn pane failed: pane {}:{index}.{pane_index} still active",
                    self.sessions[&sid].name
                ));
            }
        }
        let cwd = a
            .get(b'c')
            .map(|cwd| self.expand(cwd, Some(sid), Some(wid), Some(id), ctx))
            .transpose()?
            .map(|cwd| admission::path(&cwd))
            .transpose()?
            .map(|cwd| {
                if std::path::Path::new(&cwd).is_absolute() {
                    Ok(cwd)
                } else {
                    std::path::Path::new(
                        ctx.cwd
                            .as_deref()
                            .or(self.panes[&id].cwd.as_deref())
                            .unwrap_or("/"),
                    )
                    .join(cwd)
                    .into_os_string()
                    .into_string()
                    .map_err(|_| "working directory is not valid UTF-8".to_owned())
                }
            })
            .transpose()?;
        let env = self.pane_environment(sid, ctx, environment_overrides(&a)?);
        let mut out = Outcome {
            changed: true,
            ..Outcome::default()
        };
        if whole_window {
            for other in self.windows[&wid].panes.clone() {
                if other == id {
                    continue;
                }
                self.panes.remove(&other);
                if self.marked.is_some_and(|m| m.pane == other) {
                    self.marked = None;
                }
                out.effects.push(Effect::Kill { pane: other });
            }
            let window = self.windows.get_mut(&wid).unwrap();
            window.panes = vec![id];
            window.active = id;
            window.last.clear();
            window.zoomed = false;
            window.zoomed_pane = None;
            window.zoom_geometry.clear();
            window.over_zoom.clear();
            window.modal = None;
            window.old_layout = None;
            window.last_layout = None;
            window.layout = Layout::pane(id as u32, window.cols.into(), window.rows.into());
            self.sync_layout(wid, &mut out);
            out.notifications.push(self.layout_event(wid));
        }
        let pane = self.panes.get_mut(&id).unwrap();
        if let Some(cwd) = cwd {
            pane.cwd = Some(cwd);
        }
        if !a.rest.is_empty() {
            pane.argv = a.rest;
        }
        pane.status = None;
        pane.dead_status = None;
        pane.dead_signal = None;
        pane.dead_time = None;
        pane.pid = None;
        pane.current_command = None;
        pane.current_path = None;
        pane.empty = false;
        let title = pane.terminal.title().to_vec();
        let cell_size = pane.terminal.cell_size();
        pane.terminal = Terminal::new(pane.cols, pane.rows, pane.history_limit);
        pane.terminal.set_cell_size(cell_size.0, cell_size.1);
        pane.terminal.set_title(&title);
        out.effects.push(Effect::Spawn {
            pane: id,
            shell: pane.shell.clone(),
            argv: pane.argv.clone(),
            cwd: pane.cwd.clone(),
            env,
            cols: pane.cols,
            rows: pane.rows,
        });
        if empty {
            self.make_empty(id, &mut out);
        }
        Ok(out)
    }

    pub(super) fn pipe_pane(
        &mut self,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(args, "IOot:")?;
        if a.rest.len() > 1 {
            return Err("too many arguments".into());
        }
        let (sid, wid, id) = self.pane_context(a.string(b't').as_deref(), ctx)?;
        if self.panes[&id].status.is_some() {
            return Err("target pane has exited".into());
        }
        let command = if a.has(b'o') && self.panes[&id].pipe {
            None
        } else {
            a.rest
                .first()
                .filter(|command| !command.is_empty())
                .map(|command| {
                    tmuxx_format::expand_time(command, |key| {
                        ctx.formats.get(key).cloned().or_else(|| {
                            let key = std::str::from_utf8(key).ok()?;
                            self.format_value(key, Some(sid), Some(wid), Some(id), ctx)
                                .map(String::into_bytes)
                        })
                    })
                    .map_err(|error| error.to_string())
                })
                .transpose()?
        };
        self.panes.get_mut(&id).unwrap().pipe = command.is_some();
        Ok(Outcome {
            changed: true,
            effects: vec![Effect::Pipe {
                pane: id,
                command,
                input: a.has(b'I'),
                output: !a.has(b'I') || a.has(b'O'),
            }],
            ..Outcome::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(
        engine: &mut Engine,
        ctx: &mut ClientContext,
        words: &[&str],
    ) -> Result<Outcome, String> {
        engine.execute(
            &Command {
                blocks: Default::default(),
                args: words.iter().map(|word| word.as_bytes().to_vec()).collect(),
            },
            ctx,
        )
    }
    #[test]
    fn empty_panes_skip_process_creation_and_window_respawn_keeps_first_identity() {
        let mut engine = Engine::default();
        let mut ctx = ClientContext::default();
        run(&mut engine, &mut ctx, &["new-session", "-d"]).unwrap();
        let empty = run(&mut engine, &mut ctx, &["split-window", "-E"]).unwrap();
        assert!(engine.panes[&1].empty);
        assert!(
            !empty
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::Spawn { .. }))
        );
        assert!(
            empty
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::Empty { pane: 1 }))
        );
        assert!(run(&mut engine, &mut ctx, &["split-window", "-E", "sleep 1"]).is_err());
        run(
            &mut engine,
            &mut ctx,
            &["respawn-pane", "-t", "%1", "sleep 30"],
        )
        .unwrap();
        assert!(!engine.panes[&1].empty);
        assert!(run(&mut engine, &mut ctx, &["respawn-window"]).is_err());
        let reset = run(&mut engine, &mut ctx, &["respawn-window", "-k", "-E"]).unwrap();
        assert_eq!(engine.windows[&0].panes, [0]);
        assert_eq!(engine.windows[&0].active, 0);
        assert!(engine.panes[&0].empty);
        assert_eq!((engine.panes[&0].cols, engine.panes[&0].rows), (80, 24));
        assert!(
            reset
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::Kill { pane: 1 }))
        );
        assert!(
            !reset
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::Spawn { .. }))
        );
    }
    #[test]
    fn respawn_preserves_default_and_negotiated_cell_pixels() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        run(&mut engine, &mut context, &["new-session", "-dE"]).unwrap();
        for size in [(16, 32), (9, 18)] {
            engine
                .panes
                .get_mut(&0)
                .unwrap()
                .terminal
                .set_cell_size(size.0, size.1);
            for command in ["split-window", "new-pane"] {
                run(&mut engine, &mut context, &[command, "-dE", "-t", "%0"]).unwrap();
                assert_eq!(
                    engine
                        .panes
                        .last_key_value()
                        .unwrap()
                        .1
                        .terminal
                        .cell_size(),
                    size,
                    "{command}"
                );
            }
            for command in ["respawn-pane", "respawn-window"] {
                run(&mut engine, &mut context, &[command, "-kE"]).unwrap();
                let terminal = &mut engine.panes.get_mut(&0).unwrap().terminal;
                assert_eq!(terminal.cell_size(), size, "{command}");
                assert_eq!(terminal.snapshot().restore().unwrap().cell_size(), size);
                let expected = format!("\x1b[6;{};{}t", size.1, size.0).into_bytes();
                assert_eq!(
                    terminal.feed(b"\x1b[16t"),
                    vec![tmuxx_terminal::Effect::Write(expected)]
                );
            }
        }
    }
    #[test]
    fn pipe_toggle_and_respawn_environment_have_explicit_effects() {
        let mut engine = Engine::default();
        engine.environment.insert("KEPT".into(), "parent".into());
        let mut ctx = ClientContext::default();
        run(&mut engine, &mut ctx, &["new-session", "-d"]).unwrap();
        let pipe = run(&mut engine, &mut ctx, &["pipe-pane", "-IO", "cat"]).unwrap();
        assert!(
            matches!(&pipe.effects[0], Effect::Pipe { command: Some(command), input: true, output: true, .. } if command == b"cat")
        );
        let toggle = run(&mut engine, &mut ctx, &["pipe-pane", "-o", "cat"]).unwrap();
        assert!(matches!(
            &toggle.effects[0],
            Effect::Pipe { command: None, .. }
        ));
        let reset = run(
            &mut engine,
            &mut ctx,
            &[
                "respawn-pane",
                "-k",
                "-e",
                "ADDED=value",
                "-e",
                "KEPT",
                "sleep 2",
            ],
        )
        .unwrap();
        assert!(
            matches!(&reset.effects[0], Effect::Spawn { env, argv, .. } if env.get("ADDED").map(String::as_str) == Some("value") && env.get("KEPT").map(String::as_str) == Some("parent") && argv == &[b"sleep 2".to_vec()])
        );
    }
    #[test]
    fn key_retention_waits_for_a_real_key_and_failed_key_retains_only_failure() {
        use tmuxx_terminal::{Input, Key, KeyAction, KeyEncodeOptions, KeyEvent};
        let mut engine = Engine::default();
        let mut ctx = ClientContext::default();
        run(&mut engine, &mut ctx, &["new-session", "-d"]).unwrap();
        run(&mut engine, &mut ctx, &["split-window", "-dk"]).unwrap();
        engine.feed(1, b"kept\x1b[");
        engine.exited(1, 0);
        assert_eq!(engine.panes[&1].status, Some(0));
        assert_eq!(engine.pane_option(1, "remain-on-exit"), Some("key"));
        assert!(!engine.panes[&1].terminal.mode(true, 25));
        for input in [
            Input::Paste(b"text".to_vec()),
            Input::Mouse(Vec::new()),
            Input::Text(b"synthetic".to_vec()),
        ] {
            assert!(engine.native_input(1, &input).is_err());
            assert!(engine.panes.contains_key(&1));
        }
        let mut event = KeyEvent::new(Key::Char('x'));
        event.action = KeyAction::Release;
        assert!(
            engine
                .native_input(
                    1,
                    &Input::Key {
                        event: event.clone(),
                        options: KeyEncodeOptions::default()
                    }
                )
                .is_err()
        );
        event.action = KeyAction::Press;
        assert!(
            engine
                .native_input(
                    1,
                    &Input::Key {
                        event,
                        options: KeyEncodeOptions::default()
                    }
                )
                .unwrap()
                .changed
        );
        assert!(!engine.panes.contains_key(&1));
        run(&mut engine, &mut ctx, &["split-window", "-d"]).unwrap();
        run(
            &mut engine,
            &mut ctx,
            &[
                "set-option",
                "-p",
                "-t",
                "%2",
                "remain-on-exit",
                "failed-key",
            ],
        )
        .unwrap();
        engine.exited(2, 0);
        assert!(!engine.panes.contains_key(&2));
        run(&mut engine, &mut ctx, &["new-pane", "-dk"]).unwrap();
        run(
            &mut engine,
            &mut ctx,
            &[
                "set-option",
                "-p",
                "-t",
                "%3",
                "remain-on-exit",
                "failed-key",
            ],
        )
        .unwrap();
        engine.exited(3, 7);
        assert_eq!(engine.panes[&3].status, Some(7));
    }

    #[test]
    fn exit_status_signal_and_dead_time_remain_distinct_through_respawn() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        run(&mut engine, &mut context, &["new-session", "-d"]).unwrap();
        run(
            &mut engine,
            &mut context,
            &["set-option", "-w", "remain-on-exit", "on"],
        )
        .unwrap();
        engine.now = 7_000_000;
        engine.record_exit(0, 137, Some("kill".into()));
        assert_eq!(engine.panes[&0].status, None);
        assert_eq!(engine.panes[&0].dead_signal.as_deref(), Some("kill"));
        assert_eq!(engine.panes[&0].dead_status, None);
        engine.exited_with_signal(0, 137, Some("kill".into()));
        assert_eq!(engine.panes[&0].dead_time, Some(7_000_000));
        run(&mut engine, &mut context, &["respawn-pane"]).unwrap();
        assert_eq!(engine.panes[&0].dead_time, None);
        assert_eq!(engine.panes[&0].dead_signal, None);
        engine.exited(0, 137);
        assert_eq!(engine.panes[&0].dead_status, Some(137));
        assert_eq!(engine.panes[&0].dead_signal, None);
    }
}
