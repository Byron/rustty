use super::*;
use crate::control::{Subscription, What, subscription};

pub(super) struct HookMonitor {
    subscription: Subscription,
    session: Option<Id>,
    only_true: bool,
    next_check: Instant,
    fire_count: u32,
    fire_time: u64,
}

impl Host {
    fn hook_option_map(&self, scope: Scope) -> Option<&BTreeMap<String, String>> {
        Some(match scope {
            Scope::GlobalSession => &self.engine.options,
            Scope::GlobalWindow => &self.engine.global_window_options,
            Scope::Session(id) => &self.engine.sessions.get(&id)?.options,
            Scope::Window(id) => &self.engine.windows.get(&id)?.options,
            Scope::Pane(id) => &self.engine.panes.get(&id)?.options,
        })
    }
    pub(super) fn monitor_option(&self, scope: Scope, name: &str) -> Option<&String> {
        self.hook_option_map(scope)?.get(name)
    }

    fn monitor_option_args(scope: Scope) -> Vec<Vec<u8>> {
        match scope {
            Scope::GlobalSession => vec![b"-g".to_vec()],
            Scope::GlobalWindow => vec![b"-gw".to_vec()],
            Scope::Session(id) => vec![b"-t".to_vec(), format!("${id}").into_bytes()],
            Scope::Window(id) => vec![
                b"-w".to_vec(),
                b"-t".to_vec(),
                format!("@{id}").into_bytes(),
            ],
            Scope::Pane(id) => vec![
                b"-p".to_vec(),
                b"-t".to_vec(),
                format!("%{id}").into_bytes(),
            ],
        }
    }

    pub(super) fn prune_hook_monitors(&mut self) {
        let removed = self
            .hooks
            .monitors
            .keys()
            .filter(|(scope, name)| self.monitor_option(*scope, name).is_none())
            .cloned()
            .collect::<Vec<_>>();
        for key in removed {
            self.hooks.monitors.remove(&key);
        }
        let removed = self
            .hooks
            .fires
            .keys()
            .filter(|(scope, name)| self.monitor_option(*scope, name).is_none())
            .cloned()
            .collect::<Vec<_>>();
        for key in removed {
            self.hooks.fires.remove(&key);
        }
        self.engine.monitored_user_options = self
            .hooks
            .monitors
            .keys()
            .map(|(scope, name)| {
                let (scope, id) = match scope {
                    Scope::GlobalSession => (HookScope::Session, None),
                    Scope::GlobalWindow => (HookScope::Window, None),
                    Scope::Session(id) => (HookScope::Session, Some(*id)),
                    Scope::Window(id) => (HookScope::Window, Some(*id)),
                    Scope::Pane(id) => (HookScope::Pane, Some(*id)),
                };
                (scope, id, name.clone())
            })
            .collect();
    }

    pub(super) fn record_hook_fire(&mut self, scope: Scope, name: &str) {
        let fire = self
            .hooks
            .fires
            .entry((scope, name.to_owned()))
            .or_default();
        fire.0 = fire.0.wrapping_add(1);
        fire.1 = self.engine.now / 1_000_000;
    }

    pub(super) fn show_hook_options(
        &mut self,
        args: &Args,
        context: &ClientContext,
        scope: Scope,
        requested: Option<(String, Option<Index>)>,
    ) -> Result<Outcome, String> {
        let names = if let Some((name, index)) = requested {
            vec![(name, index)]
        } else {
            let mut names = self
                .hook_option_map(scope)
                .into_iter()
                .flat_map(|map| map.keys())
                .filter(|name| {
                    name.starts_with('@')
                        && (self.engine.user_hook_names.contains(*name)
                            || self.hooks.monitors.contains_key(&(scope, (*name).clone())))
                })
                .cloned()
                .map(|name| (name, None))
                .collect::<Vec<_>>();
            names.extend(
                hook_options()
                    .filter(|(_, kind)| match scope {
                        Scope::GlobalSession | Scope::Session(_) => *kind == HookScope::Session,
                        Scope::GlobalWindow | Scope::Window(_) => *kind != HookScope::Session,
                        Scope::Pane(_) => *kind == HookScope::Pane,
                    })
                    .map(|(name, _)| (name.to_owned(), None)),
            );
            names
        };
        let mut output = Vec::new();
        for (name, index) in names {
            let mut command = vec![b"show-options".to_vec()];
            let mut scope_args = Self::monitor_option_args(scope);
            if let Some(target) = args.get(b't') {
                if let Some(index) = scope_args.iter().position(|arg| arg == b"-t") {
                    scope_args[index + 1] = target.to_vec();
                } else {
                    scope_args.extend([b"-t".to_vec(), target.to_vec()]);
                }
            }
            command.extend(scope_args);
            if let Some(format) = args.get(b'F') {
                command.extend([b"-F".to_vec(), format.to_vec()]);
            }
            command.push(
                index
                    .map_or_else(|| name.clone(), |index| format!("{name}[{}]", index.text()))
                    .into_bytes(),
            );
            let mut context = context.clone();
            let (count, time) = self
                .hooks
                .fires
                .get(&(scope, name))
                .copied()
                .unwrap_or_default();
            context
                .formats
                .insert(b"hook_fire_count".to_vec(), count.to_string().into_bytes());
            if time != 0 {
                context
                    .formats
                    .insert(b"hook_fire_time".to_vec(), time.to_string().into_bytes());
            }
            output.extend(
                self.engine
                    .execute(
                        &Command {
                            blocks: Default::default(),
                            args: command,
                        },
                        &mut context,
                    )?
                    .output,
            );
        }
        Ok(Outcome {
            output,
            ..Outcome::default()
        })
    }

    pub(super) fn hook_monitor_command(
        &mut self,
        showing: bool,
        args: &Args,
        context: &ClientContext,
    ) -> Result<Outcome, String> {
        if args.rest.len() > 1 {
            return Err("too many arguments".into());
        }
        let target = self.hook_target(args.get(b't'), context)?;
        if showing {
            let name = args
                .rest
                .first()
                .map(|name| self.expand_job(name, args.get(b't'), context, &[]))
                .transpose()?;
            let name = name
                .as_deref()
                .map(|name| std::str::from_utf8(name).map_err(|_| "invalid hook name"))
                .transpose()?;
            let scope = self.hook_scope(args, name.unwrap_or("@"), &target)?;
            return Ok(Outcome {
                output: self.monitor_listing(args, &target, scope, name)?,
                ..Outcome::default()
            });
        }
        let raw = args.get(b'B').unwrap();
        let parsed = subscription(raw);
        let (name, subscription) = if args.has(b'u') {
            parsed.map_or_else(|| (raw.to_vec(), None), |(name, sub)| (name, Some(sub)))
        } else {
            let (name, sub) = parsed
                .ok_or_else(|| format!("invalid subscription: {}", String::from_utf8_lossy(raw)))?;
            (name, Some(sub))
        };
        let name = std::str::from_utf8(&name).map_err(|_| "invalid hook name")?;
        if !name.starts_with('@') {
            return Err("monitor hook name must start with @".into());
        }
        let scope = self.hook_scope(args, name, &target)?;
        let key = (scope, name.to_owned());
        if args.has(b'u') {
            self.hooks.monitors.remove(&key);
            return Ok(Outcome::default());
        }
        if !args.rest.is_empty() || self.monitor_option(scope, name).is_none() {
            let mut command = vec![b"set-option".to_vec()];
            command.extend(Self::monitor_option_args(scope));
            if args.has(b'a') {
                command.push(b"-a".to_vec());
            }
            command.push(name.as_bytes().to_vec());
            command.push(args.rest.first().cloned().unwrap_or_default());
            self.engine.execute(
                &Command {
                    blocks: Default::default(),
                    args: command,
                },
                &mut context.clone(),
            )?;
        }
        let session = (!matches!(scope, Scope::GlobalSession | Scope::GlobalWindow))
            .then_some(target.session)
            .flatten();
        self.hooks.monitors.insert(
            key,
            HookMonitor {
                subscription: subscription.unwrap(),
                session,
                only_true: args.has(b'T'),
                next_check: Instant::now() + Duration::from_secs(1),
                fire_count: 0,
                fire_time: 0,
            },
        );
        Ok(Outcome {
            changed: true,
            ..Outcome::default()
        })
    }

    pub(super) fn monitor_listing(
        &mut self,
        args: &Args,
        context: &ClientContext,
        scope: Scope,
        name: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        let mut context = context.clone();
        let target_fields = self.expand_job(
            b"#{window_index}:#{window_active}:#{window_last_flag}",
            args.get(b't'),
            &context,
            &[],
        )?;
        for (name, value) in ["window_index", "window_active", "window_last_flag"]
            .into_iter()
            .zip(target_fields.split(|byte| *byte == b':'))
        {
            context
                .formats
                .insert(name.as_bytes().to_vec(), value.to_vec());
        }
        if let Some(name) = name.filter(|name| name.starts_with('@'))
            && self.monitor_option(scope, name).is_none()
        {
            return Err(format!("invalid option: {name}"));
        }
        let mut output = Vec::new();
        let Some(options) = self.hook_option_map(scope) else {
            return Ok(output);
        };
        for key in options.keys() {
            if !key.starts_with('@') || name.is_some_and(|name| name != key) {
                continue;
            }
            let Some(monitor) = self.hooks.monitors.get(&(scope, key.clone())) else {
                continue;
            };
            let selector = match monitor.subscription.what {
                What::Session => String::new(),
                What::Panes => "%*".into(),
                What::Windows => "@*".into(),
                What::Pane(id) => format!("%{id}"),
                What::Window(id) => format!("@{id}"),
            };
            let mut value = format!("{key}:{selector}:").into_bytes();
            let monitor_format = monitor.subscription.format.as_slice();
            value.extend(monitor_format);
            let line = if let Some(format) = args.get(b'F') {
                let mut context = context.clone();
                for (key, value) in [
                    ("option_name", key.as_bytes().to_vec()),
                    ("option_value", value.clone()),
                    ("hook_monitor_target", selector.into_bytes()),
                    ("hook_monitor_format", monitor_format.to_vec()),
                    (
                        "hook_fire_count",
                        monitor.fire_count.to_string().into_bytes(),
                    ),
                    ("option_is_hook", b"1".to_vec()),
                    ("option_is_user", b"1".to_vec()),
                    ("option_is_string", b"1".to_vec()),
                    ("option_has_value", b"1".to_vec()),
                    ("option_has_array_key", b"0".to_vec()),
                    ("option_is_array", b"0".to_vec()),
                    ("option_is_parent", b"0".to_vec()),
                    ("option_value_only", b"0".to_vec()),
                    ("option_array_key", Vec::new()),
                ] {
                    context.formats.insert(key.as_bytes().to_vec(), value);
                }
                if monitor.fire_time != 0 {
                    context.formats.insert(
                        b"hook_fire_time".to_vec(),
                        monitor.fire_time.to_string().into_bytes(),
                    );
                }
                let window = context
                    .current_pane
                    .and_then(|pane| self.engine.panes.get(&pane).map(|pane| pane.window));
                self.engine.expand(
                    format,
                    context.session,
                    window,
                    context.current_pane,
                    &context,
                )?
            } else {
                value
            };
            output.extend(line);
            output.push(b'\n');
        }
        Ok(output)
    }

    pub(crate) fn hook_monitor_tick(&mut self) {
        self.prune_hook_monitors();
        let now = Instant::now();
        let keys = self
            .hooks
            .monitors
            .iter()
            .filter(|(_, monitor)| monitor.next_check <= now)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            let mut monitor = self.hooks.monitors.remove(&key).unwrap();
            monitor.next_check = now + Duration::from_secs(1);
            let sid = monitor.session.or_else(|| {
                self.engine
                    .sessions
                    .values()
                    .min_by(|a, b| a.name.cmp(&b.name))
                    .map(|session| session.id)
            });
            if let Some(sid) = sid.filter(|sid| self.engine.sessions.contains_key(sid)) {
                let targets = monitor.subscription.targets(&self.engine, sid);
                if matches!(monitor.subscription.what, What::Panes | What::Windows) {
                    monitor
                        .subscription
                        .last
                        .retain(|key, _| targets.contains(key));
                }
                for (window, index, pane) in targets {
                    if matches!(monitor.subscription.what, What::Pane(_))
                        && pane.is_some_and(|pane| !self.processes.contains_key(&pane))
                    {
                        continue;
                    }
                    let current_window = window.or_else(|| {
                        self.engine.sessions[&sid]
                            .windows
                            .get(&self.engine.sessions[&sid].active)
                            .copied()
                    });
                    let mut context = ClientContext {
                        session: Some(sid),
                        current_pane: pane.or_else(|| {
                            current_window.map(|window| self.engine.windows[&window].active)
                        }),
                        no_format_jobs: true,
                        format_client: Some(false),
                        ..ClientContext::default()
                    };
                    if let Some(index) = index {
                        context
                            .formats
                            .insert(b"window_index".to_vec(), index.to_string().into_bytes());
                    }
                    let Ok(value) = self.engine.expand(
                        &monitor.subscription.format,
                        Some(sid),
                        window,
                        pane,
                        &context,
                    ) else {
                        continue;
                    };
                    let previous = monitor
                        .subscription
                        .last
                        .insert((window, index, pane), value.clone());
                    let Some(previous) = previous.filter(|previous| *previous != value) else {
                        continue;
                    };
                    if monitor.only_true && !tmuxx_format::truthy(&value) {
                        continue;
                    }
                    monitor.fire_count = monitor.fire_count.wrapping_add(1);
                    self.record_hook_fire(key.0, &key.1);
                    monitor.fire_time = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    context = self.hook_payload(&key.1, &context, None);
                    if pane.is_none() {
                        context.formats.remove(b"hook_pane".as_slice());
                    }
                    if window.is_none() {
                        context
                            .formats
                            .retain(|name, _| !name.starts_with(b"hook_window"));
                    }
                    if let Some(index) = index {
                        context
                            .formats
                            .insert(b"window_index".to_vec(), index.to_string().into_bytes());
                        context.formats.insert(
                            b"hook_window_index".to_vec(),
                            index.to_string().into_bytes(),
                        );
                    }
                    context.formats.insert(b"hook_value".to_vec(), value);
                    context.formats.insert(b"hook_last".to_vec(), previous);
                    self.signal_hook_event(&key.1, &context);
                    let command = self
                        .monitor_option(key.0, &key.1)
                        .cloned()
                        .unwrap_or_default();
                    if let Ok(expanded) = self.engine.expand(
                        command.as_bytes(),
                        context.session,
                        current_window,
                        context.current_pane,
                        &context,
                    ) && let Ok(commands) = self.prepare_commands(&expanded, &context)
                    {
                        context.no_hooks = true;
                        self.internal_queue(context, commands, 0, false);
                    }
                }
            }
            self.hooks.monitors.insert(key, monitor);
        }
    }
}
