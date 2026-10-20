use super::*;
use tmuxx_terminal::clipboard::{Content, Location, ReadResult, is_text_mime};

impl Engine {
    pub(super) fn clipboard_target(
        &self,
        target: Option<&str>,
        context: &ClientContext,
    ) -> Option<(String, Id)> {
        let client = self.display_client(target, context.session, context).ok()?;
        let session = self.sessions.get(&client.session?)?;
        let window = self.windows.get(session.windows.get(&session.active)?)?;
        Some((client.client_name, window.active))
    }

    fn clipboard_buffer(&mut self, pane: Option<Id>, data: Vec<u8>) -> Effect {
        let (buffer, deleted) = if data.is_empty() {
            (None, Vec::new())
        } else {
            let (name, deleted) = self
                .store_buffer(None, data)
                .expect("automatic buffer has no name to validate")
                .unwrap();
            (Some(name), deleted)
        };
        Effect::ClipboardChanged {
            pane,
            buffer,
            deleted,
        }
    }

    pub(super) fn terminal_effects(
        &mut self,
        pane: Id,
        effects: Vec<tmuxx_terminal::Effect>,
    ) -> Vec<Effect> {
        let mut result = Vec::new();
        for effect in effects {
            let action = match effect {
                tmuxx_terminal::Effect::ShellPrompt => {
                    self.panes
                        .get_mut(&pane)
                        .unwrap()
                        .shell_state
                        .last_prompt_time = Some(self.now / 1_000_000);
                    result.push(Effect::ShellPrompt { pane });
                    continue;
                }
                tmuxx_terminal::Effect::TmuxCommandStart { command_line } => {
                    let state = &mut self.panes.get_mut(&pane).unwrap().shell_state;
                    state.command_line = command_line
                        .and_then(|line| String::from_utf8(line).ok())
                        .map(|line| clean_name(&line, true));
                    state.command_start_time = Some(self.now / 1_000_000);
                    state.command_end_time = None;
                    state.command_running = true;
                    state.command_status = None;
                    result.push(Effect::ShellCommand {
                        pane,
                        started: true,
                        state: state.clone(),
                    });
                    continue;
                }
                tmuxx_terminal::Effect::CommandEnd { exit_code } => {
                    let state = &mut self.panes.get_mut(&pane).unwrap().shell_state;
                    state.command_end_time = Some(self.now / 1_000_000);
                    state.command_running = false;
                    state.command_status = exit_code;
                    result.push(Effect::ShellCommand {
                        pane,
                        started: false,
                        state: state.clone(),
                    });
                    continue;
                }
                tmuxx_terminal::Effect::Write(data) => {
                    result.push(Effect::Write { pane, data });
                    continue;
                }
                tmuxx_terminal::Effect::Title(title) => {
                    if let Ok(title) = std::str::from_utf8(&title) {
                        self.panes
                            .get_mut(&pane)
                            .unwrap()
                            .terminal
                            .set_title(clean_name(title, true).as_bytes());
                    }
                    continue;
                }
                tmuxx_terminal::Effect::WindowName(name) => {
                    if self.pane_option(pane, "allow-rename") == Some("on") {
                        let wid = self.panes[&pane].window;
                        let global_auto = self.global_option("automatic-rename") == Some("on");
                        let window = self.windows.get_mut(&wid).unwrap();
                        let name = if name.is_empty() {
                            window.options.remove("automatic-rename");
                            (!global_auto).then(String::new)
                        } else {
                            window
                                .options
                                .insert("automatic-rename".into(), "off".into());
                            std::str::from_utf8(&name)
                                .ok()
                                .map(|name| clean_name(name, true))
                        };
                        if let Some(name) = name {
                            window.name = name.clone();
                            result.push(Effect::WindowRenamed { window: wid, name });
                        }
                    }
                    continue;
                }
                tmuxx_terminal::Effect::Bell => HostAction::Bell,
                tmuxx_terminal::Effect::Notification { title, body } => {
                    HostAction::Notification { title, body }
                }
                tmuxx_terminal::Effect::ClipboardWrite(request) => {
                    if request.is_osc52() {
                        if self.server_options.get("set-clipboard").map(String::as_str)
                            != Some("on")
                        {
                            continue;
                        }
                        let data = request
                            .contents
                            .iter()
                            .find(|content| is_text_mime(&content.mime))
                            .map(|content| content.data.to_vec())
                            .unwrap_or_default();
                        result.push(self.clipboard_buffer(Some(pane), data));
                    }
                    HostAction::ClipboardWrite(request)
                }
                tmuxx_terminal::Effect::ClipboardRead(mut request) => {
                    if request.is_osc52() {
                        if self.server_options.get("set-clipboard").map(String::as_str)
                            != Some("on")
                        {
                            continue;
                        }
                        match self.server_options.get("get-clipboard").map(String::as_str) {
                            Some("buffer") => {
                                if let Some(data) = self
                                    .top_buffer_name()
                                    .and_then(|name| self.buffers.get(name))
                                {
                                    result.push(Effect::Write {
                                        pane,
                                        data: request.reply(&[Content {
                                            mime: b"text/plain".to_vec(),
                                            data: data.clone().into(),
                                        }]),
                                    });
                                }
                                continue;
                            }
                            Some("request" | "both") => {
                                // C asks the host's default clipboard; its reply chooses the selector.
                                request.location = Location::Standard;
                                request.tmux_selector = Some(b'c');
                            }
                            _ => continue,
                        }
                    }
                    HostAction::ClipboardRead(request)
                }
                _ => continue,
            };
            result.push(Effect::Host { pane, action });
        }
        result
    }

    pub fn host_response(
        &mut self,
        pane: Id,
        action: HostAction,
        response: HostResponse,
    ) -> Result<Vec<Effect>, String> {
        if !self.panes.contains_key(&pane) {
            return Err("pane closed".into());
        }
        let mut effects = Vec::new();
        if let (HostAction::ClipboardRead(request), HostResponse::Read(result)) =
            (&action, &response)
            && request.is_osc52()
        {
            let policy = self.server_options.get("get-clipboard").map(String::as_str);
            // C never manufactures a reply when no terminal supplied clipboard data.
            let ReadResult::Success(success) = result else {
                return Ok(effects);
            };
            if !matches!(policy, Some("request" | "both")) {
                return Ok(effects);
            }
            if policy == Some("both") {
                let data = success
                    .contents
                    .iter()
                    .find(|content| is_text_mime(&content.mime))
                    .map(|content| content.data.to_vec())
                    .unwrap_or_default();
                effects.push(self.clipboard_buffer(None, data));
                self.sequence = self.sequence.wrapping_add(1);
            }
        }
        if let Some(data) = self
            .panes
            .get_mut(&pane)
            .unwrap()
            .terminal
            .host_response(action, response)
            .map_err(|e| e.to_string())?
            && !data.is_empty()
        {
            effects.push(Effect::Write { pane, data });
        }
        Ok(effects)
    }
}
