use super::*;

pub(super) struct SessionPolicy {
    id: Id,
    name: String,
    detach: String,
}

impl Host {
    pub(super) fn has_attached_clients(&self) -> bool {
        self.hooks.attached.iter().any(|id| {
            self.clients.get(id).is_some_and(|client| {
                client
                    .context
                    .session
                    .is_some_and(|sid| self.engine.sessions.contains_key(&sid))
            })
        })
    }
    pub(super) fn session_policies(&mut self) -> Vec<SessionPolicy> {
        self.engine
            .sessions
            .values()
            .map(|session| SessionPolicy {
                id: session.id,
                name: session.name.clone(),
                detach: self
                    .engine
                    .session_option(session.id, "detach-on-destroy")
                    .unwrap_or("on")
                    .into(),
            })
            .collect()
    }

    fn session_attached(&self, session: Id) -> bool {
        self.hooks.attached.iter().any(|id| {
            self.clients
                .get(id)
                .is_some_and(|client| client.context.session == Some(session))
        })
    }

    pub(super) fn reconcile_destroyed_sessions(
        &mut self,
        before: Vec<SessionPolicy>,
        current: Option<(u64, &mut ClientContext)>,
    ) -> bool {
        let mut current = current;
        let mut detach_current = false;
        for old in before
            .into_iter()
            .filter(|old| !self.engine.sessions.contains_key(&old.id))
            .collect::<Vec<_>>()
        {
            let newest = |detached: bool| {
                self.engine
                    .sessions
                    .keys()
                    .copied()
                    .filter(|id| !detached || !self.session_attached(*id))
                    .max_by_key(|id| (self.engine.sessions[id].activity, std::cmp::Reverse(*id)))
            };
            let fallback = newest(false);
            let target = match old.detach.as_str() {
                "off" => fallback,
                "no-detached" => newest(true),
                "previous" | "next" => {
                    let mut sessions = self.engine.sessions.values().collect::<Vec<_>>();
                    sessions.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
                    if old.detach == "next" {
                        sessions
                            .iter()
                            .find(|session| session.name > old.name)
                            .or_else(|| sessions.first())
                            .map(|session| session.id)
                    } else {
                        sessions
                            .iter()
                            .rev()
                            .find(|session| session.name < old.name)
                            .or_else(|| sessions.last())
                            .map(|session| session.id)
                    }
                }
                _ => None,
            };
            let clients = self
                .hooks
                .attached
                .iter()
                .copied()
                .filter(|id| {
                    self.clients
                        .get(id)
                        .is_some_and(|client| client.context.session == Some(old.id))
                })
                .collect::<Vec<_>>();
            for id in clients {
                let previous = self.clients[&id].context.clone();
                let fallback_allowed = matches!(old.detach.as_str(), "on" | "no-detached")
                    && previous
                        .client_flags
                        .split(',')
                        .any(|flag| flag == "no-detach-on-destroy");
                let target = target.or_else(|| fallback_allowed.then_some(fallback).flatten());
                let mut context = previous.clone();
                context.set_session(target);
                context.current_pane = target
                    .and_then(|sid| self.engine.sessions.get(&sid))
                    .and_then(|session| session.windows.get(&session.active))
                    .map(|wid| self.engine.windows[wid].active);
                self.clients.get_mut(&id).unwrap().context = context.clone();
                if let Some((active, active_context)) = &mut current
                    && *active == id
                {
                    **active_context = context.clone();
                    detach_current |= target.is_none();
                }
                if let Some(sid) = target {
                    self.client_session_destroyed(&context);
                    self.engine.session_attached(sid);
                    self.send(
                        id,
                        ServerMessage::Control {
                            data: format!(
                                "%session-changed ${sid} {}",
                                self.engine.sessions[&sid].name
                            )
                            .into_bytes(),
                        },
                    );
                } else if current.as_ref().is_none_or(|(active, _)| *active != id) {
                    self.send(
                        id,
                        ServerMessage::Exit {
                            reason: "session destroyed".into(),
                        },
                    );
                    self.disconnect(id);
                }
            }
        }
        self.refresh_format_clients();
        detach_current
    }

    pub(super) fn check_unattached_sessions(&mut self) {
        if self.checking_sessions {
            return;
        }
        self.checking_sessions = true;
        let ids = self.engine.sessions.keys().copied().collect::<Vec<_>>();
        for id in ids {
            let Some(session) = self.engine.sessions.get(&id) else {
                continue;
            };
            if self.session_attached(id) {
                continue;
            }
            let group_size = session.group.as_ref().map(|group| {
                self.engine
                    .sessions
                    .values()
                    .filter(|session| session.group.as_ref() == Some(group))
                    .count()
            });
            let destroy = match self.engine.session_option(id, "destroy-unattached") {
                Some("on") => true,
                Some("keep-last") => group_size.is_some_and(|count| count > 1),
                Some("keep-group") => group_size != Some(1),
                _ => false,
            };
            if !destroy {
                continue;
            }
            let before = self.session_policies();
            let graph = self.hook_snapshot();
            let command = tmuxx_command::Command {
                blocks: Default::default(),
                args: vec![
                    b"kill-session".to_vec(),
                    b"-t".to_vec(),
                    format!("${id}").into_bytes(),
                ],
            };
            if let Ok(out) = self.engine.execute(&command, &mut ClientContext::default()) {
                self.reconcile_destroyed_sessions(before, None);
                if let Some(graph) = graph {
                    self.graph_hooks(graph, false);
                }
                if let Err(error) = self.effects(out.effects) {
                    eprintln!("tmuxx: session cleanup: {error}");
                }
                for data in out.notifications {
                    self.broadcast_notification(data);
                }
                self.broadcast(ServerMessage::Changed {
                    sequence: self.engine.sequence,
                });
            }
        }
        self.checking_sessions = false;
        self.refresh_format_clients();
    }
}
