use super::*;
use tmuxx_command::{Args, Command};

impl Host {
    pub(super) fn new_session_client(
        &mut self,
        id: u64,
        command: &Command,
        context: &mut ClientContext,
    ) -> Result<Outcome, String> {
        let entry = tmuxx_command::registry::validate(command)?;
        let args = Args::parse(&command.args[1..], entry.flags)?;
        if args.has(b'A')
            && let Some(name) = args.get(b's')
        {
            let expanded = self.engine.expand(name, None, None, None, context)?;
            let name = tmuxx_core::clean_name(
                std::str::from_utf8(&expanded).map_err(|_| "invalid session name")?,
                false,
            );
            if let Some(session) = self
                .engine
                .sessions
                .values()
                .find(|session| session.name == name)
            {
                let mut forwarded = Command {
                    blocks: Default::default(),
                    args: vec![
                        b"attach-session".to_vec(),
                        b"-t".to_vec(),
                        format!("${}", session.id).into_bytes(),
                    ],
                };
                for (source, dest) in [
                    (b'D', b'd'),
                    (b'X', b'x'),
                    (b'E', b'E'),
                    (b'c', b'c'),
                    (b'f', b'f'),
                ] {
                    if args.has(source) {
                        forwarded.args.push(vec![b'-', dest]);
                        if let Some(value) = args.get(source) {
                            forwarded.args.push(value.to_vec());
                        }
                    }
                }
                return self.client_command(id, &forwarded, context);
            }
        }
        let out = self.execute(command, context)?;
        if !args.has(b'd')
            && let Some(flags) = args.get(b'f')
        {
            self.control_flags(id, flags, context);
        }
        Ok(out)
    }

    fn command_client(
        &self,
        id: u64,
        target: Option<&[u8]>,
        context: &ClientContext,
    ) -> Result<u64, String> {
        if let Some(target) = target {
            let target = String::from_utf8_lossy(target);
            let name = target.strip_suffix(':').unwrap_or(&target);
            return self
                .clients
                .iter()
                .find(|(_, client)| {
                    client.context.session.is_some()
                        && (client.context.client_name == name
                            || client
                                .context
                                .identity
                                .get("client_tty")
                                .is_some_and(|tty| {
                                    tty == name || tty.strip_prefix("/dev/") == Some(name)
                                }))
                })
                .map(|(id, _)| *id)
                .ok_or_else(|| format!("can't find client: {target}"));
        }
        if self.hooks.attached.contains(&id) {
            return Ok(id);
        }
        self.clients
            .iter()
            .filter(|(id, _)| self.hooks.attached.contains(id))
            .max_by_key(|(_, client)| {
                (
                    client.context.session == context.session,
                    client.context.activity,
                )
            })
            .map(|(id, _)| *id)
            .ok_or_else(|| "no current client".into())
    }

    pub(super) fn client_command(
        &mut self,
        id: u64,
        command: &Command,
        context: &mut ClientContext,
    ) -> Result<Outcome, String> {
        let entry = tmuxx_command::registry::validate(command)?;
        let args = Args::parse(&command.args[1..], entry.flags)?;
        let readonly = context
            .client_flags
            .split(',')
            .any(|flag| flag == "read-only");
        if entry.name == "detach-client" {
            // Replacing a GUI process with a shell command has no native-view equivalent.
            args.reject(b"E")?;
            let target = self.command_client(id, args.get(b't'), context)?;
            if readonly && (args.has(b's') || args.has(b'a') || target != id) {
                return Err("client is read-only".into());
            }
            let targets = if let Some(session) = args.get(b's') {
                let session = self
                    .hook_target(Some(session), context)
                    .ok()
                    .and_then(|target| target.session);
                self.clients
                    .iter()
                    .filter(|(id, client)| {
                        self.hooks.attached.contains(id)
                            && session.is_some()
                            && client.context.session == session
                    })
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>()
            } else if args.has(b'a') {
                self.hooks
                    .attached
                    .iter()
                    .copied()
                    .filter(|id| *id != target)
                    .collect()
            } else {
                vec![target]
            };
            let mut out = Outcome::default();
            for target in targets {
                if target == id {
                    out.detach = true;
                    out.detach_hangup = args.has(b'P');
                } else {
                    self.detach_client(target, args.has(b'P'));
                }
            }
            return Ok(out);
        }
        if entry.name == "switch-client" {
            let target = self.command_client(id, args.get(b'c'), context)?;
            if readonly && target != id {
                return Err("client is read-only".into());
            }
            let previous = if target == id {
                context.clone()
            } else {
                self.clients[&target].context.clone()
            };
            let mut selected = previous.clone();
            let mut forwarded = command.clone();
            forwarded.args = vec![b"switch-client".to_vec()];
            for (flag, value) in &args.flags {
                if *flag == b'c' {
                    continue;
                }
                forwarded.args.push(vec![b'-', *flag]);
                if let Some(value) = value {
                    forwarded.args.push(value.clone());
                }
            }
            let mut out = self.execute(&forwarded, &mut selected)?;
            if args.has(b'r') {
                self.control_flags(target, b"", &mut selected);
            }
            self.clients.get_mut(&target).unwrap().context = selected.clone();
            self.client_command_hooks(target, &forwarded, &previous, &selected);
            if target == id {
                *context = selected;
            }
            self.deliver_session_change(target, &mut out);
            self.refresh_format_clients();
            return Ok(out);
        }
        if readonly && (args.has(b'd') || args.has(b'x') || args.has(b'c')) {
            return Err("client is read-only".into());
        }
        let mut forwarded = command.clone();
        forwarded.args = vec![b"attach-session".to_vec()];
        for (flag, value) in &args.flags {
            if matches!(*flag, b'd' | b'x') {
                continue;
            }
            forwarded.args.push(vec![b'-', *flag]);
            if let Some(value) = value {
                forwarded.args.push(value.clone());
            }
        }
        let mut out = self.execute(&forwarded, context)?;
        if args.has(b'r') || args.has(b'f') {
            self.control_flags(id, args.get(b'f').unwrap_or_default(), context);
        }
        if args.has(b'd') || args.has(b'x') {
            let targets = self
                .clients
                .iter()
                .filter(|(target, client)| {
                    **target != id
                        && self.hooks.attached.contains(target)
                        && client.context.session == context.session
                })
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            for target in targets {
                self.detach_client(target, args.has(b'x'));
            }
        }
        self.deliver_session_change(id, &mut out);
        Ok(out)
    }

    pub(super) fn deliver_session_change(&mut self, id: u64, out: &mut Outcome) {
        // The session-change record describes one client, so it is never broadcast.
        let name = self.clients[&id].context.client_name.clone();
        let mut broadcast = Vec::new();
        for data in out.notifications.drain(..) {
            match data {
                EngineNotification::Raw(data) if data.starts_with(b"%session-changed ") => {
                    out.client_notifications.push((name.clone(), data))
                }
                other => broadcast.push(other),
            }
        }
        out.notifications = broadcast;
    }

    pub(super) fn detach_client(&mut self, id: u64, hangup: bool) {
        if let Some(session) = self
            .clients
            .get(&id)
            .and_then(|client| client.context.session)
            .and_then(|session| self.engine.sessions.get(&session))
            .map(|session| session.name.clone())
        {
            if self.clients[&id].control {
                self.scheduler.lost(id);
                self.control.clients.entry(id).or_default().closing = Some(Instant::now());
                self.send(id, ServerMessage::Detached { session, hangup });
                return;
            }
            self.send(id, ServerMessage::Detached { session, hangup });
        } else {
            self.send(
                id,
                ServerMessage::Exit {
                    reason: "session destroyed".into(),
                },
            );
        }
        self.disconnect(id);
    }
}
