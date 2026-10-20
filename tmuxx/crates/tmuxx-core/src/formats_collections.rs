use super::*;

impl Engine {
    pub(super) fn collection_format(
        &self,
        kind: u8,
        flags: &[u8],
        body: &[u8],
        target: (Option<Id>, Option<Id>, Option<Id>),
        context: &ClientContext,
        depth: usize,
    ) -> Result<Vec<u8>, String> {
        let (sid, wid, pid) = target;
        let mut contexts = Vec::new();
        match kind {
            b'O' => {
                let flags = if flags.is_empty() { b"s" } else { flags };
                let global = flags.contains(&b'g');
                let options = if flags.contains(&b'v') {
                    Some(&self.server_options)
                } else if flags.contains(&b'w') {
                    if global {
                        Some(&self.global_window_options)
                    } else {
                        wid.and_then(|id| self.windows.get(&id))
                            .map(|window| &window.options)
                    }
                } else if flags.contains(&b's') {
                    if global {
                        Some(&self.options)
                    } else {
                        sid.and_then(|id| self.sessions.get(&id))
                            .map(|session| &session.options)
                    }
                } else if flags.contains(&b'p') {
                    if global {
                        None
                    } else {
                        pid.and_then(|id| self.panes.get(&id))
                            .map(|pane| &pane.options)
                    }
                } else if global {
                    Some(&self.options)
                } else {
                    None
                };
                if let Some(options) = options {
                    for fields in options::loop_fields(options) {
                        let mut context = context.clone();
                        context.formats.extend(fields);
                        contexts.push(context);
                    }
                }
            }
            b'V' => {
                let empty = BTreeSet::new();
                let environment = match flags {
                    b"" | b"s" => sid.and_then(|id| self.sessions.get(&id)).map(|session| {
                        (
                            &session.environment,
                            &session.hidden_environment,
                            &session.removed_environment,
                        )
                    }),
                    b"g" => Some((
                        &self.environment,
                        &self.hidden_environment,
                        &self.removed_environment,
                    )),
                    b"c" => Some((&context.environment, &empty, &empty)),
                    _ => None,
                };
                if let Some((values, hidden, removed)) = environment {
                    let names = values.keys().chain(removed.iter()).collect::<BTreeSet<_>>();
                    for name in names {
                        let mut context = context.clone();
                        for (key, value) in [
                            ("environ_name", name.clone()),
                            (
                                "environ_value",
                                values.get(name).cloned().unwrap_or_default(),
                            ),
                            (
                                "environ_hidden",
                                u8::from(hidden.contains(name)).to_string(),
                            ),
                            (
                                "environ_removed",
                                u8::from(removed.contains(name)).to_string(),
                            ),
                        ] {
                            context
                                .formats
                                .insert(key.as_bytes().to_vec(), value.into_bytes());
                        }
                        contexts.push(context);
                    }
                }
            }
            b'L' => {
                contexts = self.clients.clone();
                if !flags.contains(&b'i') && (flags.contains(&b'n') || flags.contains(&b't')) {
                    contexts.sort_by(|a, b| {
                        let order = if flags.contains(&b'n') {
                            a.client_name.cmp(&b.client_name)
                        } else {
                            b.activity
                                .cmp(&a.activity)
                                .then(a.client_name.cmp(&b.client_name))
                        };
                        if flags.contains(&b'r') {
                            order.reverse()
                        } else {
                            order
                        }
                    });
                } else if flags.contains(&b'r') {
                    contexts.reverse();
                }
            }
            _ => unreachable!(),
        }
        let mut output = Vec::new();
        let count = contexts.len();
        for (index, mut context) in contexts.into_iter().enumerate() {
            context.format_type = Some(if pid.is_some() {
                b'P'
            } else if wid.is_some() {
                b'W'
            } else if sid.is_some() {
                b'S'
            } else {
                0
            });
            context
                .formats
                .insert(b"loop_index".to_vec(), index.to_string().into_bytes());
            context.formats.insert(
                b"loop_last_flag".to_vec(),
                u8::from(index + 1 == count).to_string().into_bytes(),
            );
            output.extend(self.expand_depth(body, target, &context, depth + 1, false)?);
            if output.len() > 1024 * 1024 {
                return Err("format loop output exceeds limit".into());
            }
        }
        Ok(output)
    }
}
