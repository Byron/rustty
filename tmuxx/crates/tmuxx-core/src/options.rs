use super::*;

type Map = BTreeMap<String, String>;
#[derive(Clone, Copy)]
enum Kind {
    String,
    Number(i64, i64),
    Flag,
    Choice(&'static [&'static str]),
    Array(&'static [char]),
    Key,
}
#[derive(Clone, Copy)]
struct Spec {
    name: &'static str,
    scope: u8,
    kind: Kind,
    default: &'static str,
    pane: bool,
}
struct Metadata {
    spec: Spec,
    pattern: &'static str,
    hook: bool,
    style: bool,
    colour: bool,
    value_type: &'static str,
    feature: &'static str,
    defaults: &'static [&'static str],
}
include!("options_metadata.rs");

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HookScope {
    Session,
    Window,
    Pane,
}

/// Registered hooks and their declared scopes, derived from the C option table.
pub fn hook_options() -> impl Iterator<Item = (&'static str, HookScope)> {
    SOURCE_METADATA
        .iter()
        .filter(available)
        .filter(|metadata| metadata.hook)
        .map(|metadata| {
            let scope = if metadata.spec.scope == b'c' {
                HookScope::Session
            } else if metadata.spec.pane {
                HookScope::Pane
            } else {
                HookScope::Window
            };
            (metadata.spec.name, scope)
        })
}

// A setting becomes writable only when its process or native-view effect exists.
const WRITABLE: &[&str] = &[
    "message-limit",
    "monitor-activity",
    "monitor-bell",
    "monitor-silence",
    "activity-action",
    "bell-action",
    "silence-action",
    "theme",
    "window-style",
    "window-active-style",
    "aggressive-resize",
    "codepoint-widths",
    "variation-selector-always-wide",
    "allow-set-title",
    "allow-rename",
    "destroy-unattached",
    "detach-on-destroy",
    "exit-unattached",
    "alternate-screen",
    "scroll-on-clear",
    "cursor-style",
    "cursor-colour",
    "pane-colours",
    "image-size-limit",
    "image-total-count-limit",
    "image-total-size-limit",
    "pane-border-status",
    "pane-border-lines",
    "set-clipboard",
    "get-clipboard",
    "main-pane-height",
    "main-pane-width",
    "other-pane-height",
    "other-pane-width",
    "tiled-layout-max-columns",
    "history-limit",
    "base-index",
    "pane-base-index",
    "default-shell",
    "default-command",
    "window-default-command",
    "default-size",
    "automatic-rename",
    "automatic-rename-format",
    "default-terminal",
    "remain-on-exit",
    "exit-empty",
    "window-size",
    "update-environment",
    "command-alias",
    "focus-events",
    "buffer-limit",
    "synchronize-panes",
    "renumber-windows",
    "backspace",
    "extended-keys",
    "extended-keys-format",
    "prefix",
    "prefix2",
];
fn available(metadata: &&Metadata) -> bool {
    // utmp is a platform integration excluded by the native-client profile.
    metadata.feature != "HAVE_UTEMPTER"
}
fn metadata(name: &str) -> Option<&'static Metadata> {
    SOURCE_METADATA
        .iter()
        .filter(available)
        .find(|m| m.spec.name == name)
}
fn spec(name: &str) -> Option<&'static Spec> {
    metadata(name).map(|m| &m.spec)
}
/// Match an option or hook name and normalize a numeric array key.
pub fn resolve_option_name(argument: &str) -> Result<(String, Option<String>), String> {
    let (name, key) = if let Some((name, key)) = argument.split_once('[') {
        let key = key
            .strip_suffix(']')
            .filter(|key| !key.is_empty() && !key.contains(']'))
            .ok_or_else(|| format!("invalid option: {argument}"))?;
        let key = if key.bytes().all(|b| b.is_ascii_digit()) {
            key.parse::<u32>()
                .map_err(|_| format!("invalid option: {argument}"))?
                .to_string()
        } else {
            key.to_owned()
        };
        (name, Some(key))
    } else {
        (argument, None)
    };
    let name = if name.starts_with('@') || spec(name).is_some() {
        name.to_owned()
    } else {
        let mut found = SOURCE_METADATA
            .iter()
            .filter(available)
            .map(|m| &m.spec)
            .filter(|spec| spec.name.starts_with(name));
        let first = found
            .next()
            .ok_or_else(|| format!("invalid option: {argument}"))?;
        if found.next().is_some() {
            return Err(format!("ambiguous option: {argument}"));
        }
        first.name.to_owned()
    };
    Ok((name, key))
}
fn array_key<'a>(key: &'a str, name: &str) -> Option<&'a str> {
    key.strip_prefix(name)?.strip_prefix('[')?.strip_suffix(']')
}
fn clear(map: &mut Map, name: &str) {
    map.retain(|key, _| key != name && array_key(key, name).is_none());
}
fn numeric_key(key: &str) -> Option<u32> {
    key.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| key.parse().ok())
        .flatten()
}
fn array(map: &Map, name: &str) -> Vec<(String, String)> {
    let mut values: Vec<_> = map
        .iter()
        .filter_map(|(key, value)| array_key(key, name).map(|key| (key.to_owned(), value.clone())))
        .collect();
    values.sort_by(|(a, _), (b, _)| match (numeric_key(a), numeric_key(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => a.cmp(b),
    });
    values
}

pub(super) fn format_value(map: &Map, key: &str) -> Option<String> {
    let (name, index) = if let Some((name, key)) = key.split_once('[') {
        let key = key.strip_suffix(']')?;
        let index = numeric_key(key)
            .map(|n| n.to_string())
            .unwrap_or_else(|| key.to_owned());
        (name, Some(index))
    } else {
        (key, None)
    };
    let value = map.get(name)?;
    if spec(name).is_some_and(|s| matches!(s.kind, Kind::Array(_))) {
        return Some(index.map_or_else(
            || {
                array(map, name)
                    .into_iter()
                    .map(|(_, value)| value)
                    .collect::<Vec<_>>()
                    .join(" ")
            },
            |index| {
                map.get(&format!("{name}[{index}]"))
                    .cloned()
                    .unwrap_or_default()
            },
        ));
    }
    if index.is_some() {
        return None;
    }
    Some(if matches!(spec(name).map(|s| s.kind), Some(Kind::Flag)) {
        u8::from(value == "on").to_string()
    } else {
        value.clone()
    })
}

pub(super) fn loop_fields(map: &Map) -> Vec<BTreeMap<Vec<u8>, Vec<u8>>> {
    let mut output = Vec::new();
    for (name, value) in map.iter().filter(|(name, _)| !name.contains('[')) {
        let is_array = spec(name).is_some_and(|spec| matches!(spec.kind, Kind::Array(_)));
        let mut values = if is_array {
            array(map, name)
        } else {
            Vec::new()
        };
        let count = values.len();
        if values.is_empty() {
            values.push((String::new(), value.clone()));
        }
        let entries = values.len();
        for (index, (key, value)) in values.into_iter().enumerate() {
            output.push(
                [
                    ("option_name", name.clone()),
                    ("option_value", value),
                    ("option_is_array", u8::from(is_array).to_string()),
                    ("option_array_key", key.clone()),
                    ("option_array_index", key),
                    ("option_array_count", count.to_string()),
                    (
                        "option_array_first",
                        u8::from(is_array && index == 0).to_string(),
                    ),
                    (
                        "option_array_last",
                        u8::from(is_array && index + 1 == entries).to_string(),
                    ),
                    (
                        "option_is_user",
                        u8::from(name.starts_with('@')).to_string(),
                    ),
                    (
                        "option_is_hook",
                        u8::from(metadata(name).is_some_and(|m| m.hook)).to_string(),
                    ),
                ]
                .into_iter()
                .map(|(key, value)| (key.as_bytes().to_vec(), value.into_bytes()))
                .collect(),
            );
        }
    }
    output
}
fn assign(map: &mut Map, name: &str, separators: &[char], value: &str) {
    map.insert(name.into(), String::new());
    let mut index = 0u64;
    for value in value.split(separators).filter(|value| !value.is_empty()) {
        while map.contains_key(&format!("{name}[{index}]")) {
            index += 1;
        }
        map.insert(format!("{name}[{index}]"), value.into());
        index += 1;
    }
}
pub(super) fn defaults(scope: u8) -> Map {
    let mut map = Map::new();
    for metadata in SOURCE_METADATA
        .iter()
        .filter(available)
        .filter(|m| m.spec.scope == scope)
    {
        let spec = &metadata.spec;
        if let Kind::Array(separator) = spec.kind {
            if metadata.defaults.is_empty() {
                assign(&mut map, spec.name, separator, spec.default);
            } else {
                map.insert(spec.name.into(), String::new());
                for (index, value) in metadata.defaults.iter().enumerate() {
                    map.insert(format!("{}[{index}]", spec.name), (*value).into());
                }
            }
        } else {
            map.insert(spec.name.into(), spec.default.into());
        }
    }
    if scope == b'c' {
        map.insert("mouse".into(), "on".into());
    }
    if scope == b's' {
        // This native profile advertises the same terminfo entry as Rustty.
        map.insert("default-terminal".into(), "tmux-256color".into());
    }
    map
}
#[derive(Clone, Copy)]
enum Target {
    Global(u8),
    Session(Id),
    Window(Id),
    Pane(Id),
}
impl Engine {
    fn user_option_is_hook(&self, target: Target, name: &str) -> bool {
        let scope = match target {
            Target::Global(b'c') => Some((HookScope::Session, None)),
            Target::Global(b'w') => Some((HookScope::Window, None)),
            Target::Session(id) => Some((HookScope::Session, Some(id))),
            Target::Window(id) => Some((HookScope::Window, Some(id))),
            Target::Pane(id) => Some((HookScope::Pane, Some(id))),
            _ => None,
        };
        self.user_hook_names.contains(name)
            || scope.is_some_and(|(scope, id)| {
                self.monitored_user_options
                    .contains(&(scope, id, name.to_owned()))
            })
    }
    pub fn global_option(&self, key: &str) -> Option<&str> {
        let scope = spec(key).map_or(b'c', |s| s.scope);
        self.option_map(Target::Global(scope))
            .get(key)
            .map(String::as_str)
    }
    pub fn window_option(&self, wid: Id, key: &str) -> Option<&str> {
        self.windows
            .get(&wid)
            .and_then(|window| window.options.get(key))
            .map(String::as_str)
            .or_else(|| self.global_option(key))
    }
    pub(super) fn default_size(&self, sid: Option<Id>) -> (u16, u16) {
        let (cols, rows) = self.default_size_raw(sid);
        (cols.clamp(1, 10000) as u16, rows.clamp(1, 10000) as u16)
    }
    pub(super) fn default_size_raw(&self, sid: Option<Id>) -> (u32, u32) {
        let value = sid
            .and_then(|sid| self.session_option(sid, "default-size"))
            .or_else(|| self.global_option("default-size"))
            .unwrap_or("80x24");
        parse_size_raw(value)
            .map(|(cols, rows)| (cols.max(1), rows.max(1)))
            .unwrap_or((80, 24))
    }
    pub fn pane_option(&self, pid: Id, key: &str) -> Option<&str> {
        self.panes
            .get(&pid)
            .and_then(|pane| {
                pane.options
                    .get(key)
                    .or_else(|| self.windows.get(&pane.window)?.options.get(key))
            })
            .map(String::as_str)
            .or_else(|| self.global_option(key))
    }
    pub(super) fn pane_option_array(&self, pid: Id, key: &str) -> Vec<(String, String)> {
        array(&self.effective_map(Target::Pane(pid)), key)
    }
    pub fn option_array(&self, sid: Option<Id>, key: &str) -> Vec<(String, String)> {
        let global = Target::Global(spec(key).map_or(b'c', |s| s.scope));
        let map = sid
            .and_then(|id| self.sessions.get(&id))
            .filter(|s| s.options.contains_key(key))
            .map_or_else(|| self.option_map(global), |s| &s.options);
        array(map, key)
    }
    fn option_target(
        &self,
        args: &Args,
        command: &str,
        key: &str,
        ctx: &ClientContext,
    ) -> Result<Target, String> {
        let window = args.has(b'w')
            || matches!(
                command,
                "setw" | "set-window-option" | "showw" | "show-window-options"
            );
        let scope = spec(key).map_or_else(
            || {
                if args.has(b's') {
                    b's'
                } else if window || args.has(b'p') {
                    b'w'
                } else {
                    b'c'
                }
            },
            |s| s.scope,
        );
        if scope == b's' {
            return Ok(Target::Global(scope));
        }
        let target = args.string(b't');
        let missing = |kind| {
            target.as_ref().map_or_else(
                || format!("no current {kind}"),
                |raw| format!("no such {kind}: {raw}"),
            )
        };
        if args.has(b'p') && spec(key).is_none_or(|spec| spec.pane) {
            self.pane_target(target.as_deref(), ctx)
                .map(Target::Pane)
                .map_err(|_| missing("pane"))
        } else if args.has(b'g') {
            Ok(Target::Global(scope))
        } else if scope == b'w' {
            self.window_target(target.as_deref(), ctx)
                .map(|(_, _, id)| Target::Window(id))
                .map_err(|_| missing("window"))
        } else {
            self.session_id(target.as_deref(), ctx)
                .map(Target::Session)
                .map_err(|_| missing("session"))
        }
    }
    fn option_map(&self, target: Target) -> &Map {
        match target {
            Target::Global(b's') => &self.server_options,
            Target::Global(b'w') => &self.global_window_options,
            Target::Global(_) => &self.options,
            Target::Session(id) => &self.sessions[&id].options,
            Target::Window(id) => &self.windows[&id].options,
            Target::Pane(id) => &self.panes[&id].options,
        }
    }
    fn option_map_mut(&mut self, target: Target) -> &mut Map {
        match target {
            Target::Global(b's') => &mut self.server_options,
            Target::Global(b'w') => &mut self.global_window_options,
            Target::Global(_) => &mut self.options,
            Target::Session(id) => &mut self.sessions.get_mut(&id).unwrap().options,
            Target::Window(id) => &mut self.windows.get_mut(&id).unwrap().options,
            Target::Pane(id) => &mut self.panes.get_mut(&id).unwrap().options,
        }
    }
    fn option_parent(&self, target: Target) -> Option<Target> {
        match target {
            Target::Session(_) => Some(Target::Global(b'c')),
            Target::Window(_) => Some(Target::Global(b'w')),
            Target::Pane(id) => Some(Target::Window(self.panes[&id].window)),
            Target::Global(_) => None,
        }
    }
    fn effective_map(&self, target: Target) -> Map {
        let mut result = self
            .option_parent(target)
            .map(|p| self.effective_map(p))
            .unwrap_or_default();
        for key in self
            .option_map(target)
            .keys()
            .filter(|key| !key.contains('['))
        {
            clear(&mut result, key);
        }
        result.extend(self.option_map(target).clone());
        result
    }
    fn option_format_context(&self, args: &Args, ctx: &ClientContext) -> ClientContext {
        let target = args.string(b't');
        let mut context = ctx.clone();
        let selected = self.display_context(target.as_deref(), ctx);
        context.session = selected.map(|selected| selected.0);
        context.current_pane = selected.map(|selected| selected.2);
        if let Ok((sid, index, wid)) = self.window_target(target.as_deref(), ctx)
            && selected.is_some_and(|selected| selected.0 == sid && selected.1 == wid)
        {
            context = self.winlink_context(&context, sid, index);
        }
        context
    }
    pub(super) fn set_option(
        &mut self,
        command: &str,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(args, "aFgopqsuUwt:")?;
        if a.rest.is_empty() || a.rest.len() > 2 {
            return Err("usage: set-option [-aFgopqsuUw] [-t target] option [value]".into());
        }
        let format_context = self.option_format_context(&a, ctx);
        let format_window = format_context
            .current_pane
            .and_then(|pane| self.panes.get(&pane).map(|pane| pane.window));
        let argument = admission::text(
            &self.expand(
                &a.rest[0],
                format_context.session,
                format_window,
                format_context.current_pane,
                &format_context,
            )?,
            "option name",
        )?;
        let (name, index) = match resolve_option_name(&argument) {
            Ok(value) => value,
            Err(_) if a.has(b'q') => return Ok(Outcome::default()),
            Err(error) => return Err(error),
        };
        let target = match self.option_target(&a, command, &name, ctx) {
            Ok(value) => value,
            Err(_) if a.has(b'q') => return Ok(Outcome::default()),
            Err(error) => return Err(error),
        };
        if index.is_some() && !spec(&name).is_some_and(|s| matches!(s.kind, Kind::Array(_))) {
            return Err(format!("not an array: {argument}"));
        }
        let full = index
            .as_ref()
            .map_or_else(|| name.clone(), |index| format!("{name}[{index}]"));
        if a.has(b'o') && !a.has(b'u') && self.option_map(target).contains_key(&full) {
            return if a.has(b'q') {
                Ok(Outcome::default())
            } else {
                Err(format!("already set: {argument}"))
            };
        }
        if !name.starts_with('@') && !WRITABLE.contains(&name.as_str()) {
            if !a.has(b'u') && !a.has(b'U') {
                validate(
                    spec(&name).unwrap().kind,
                    a.rest.get(1).map(|v| text(v)).as_deref(),
                    "",
                )?;
            }
            let category = if metadata(&name).is_some_and(|m| m.style) {
                "style option"
            } else {
                "option"
            };
            return Err(format!(
                "unsupported {category} in native-client profile: {name}"
            ));
        }
        if a.has(b'u') || a.has(b'U') {
            if a.has(b'U')
                && let Target::Window(id) = target
            {
                for pid in self.windows[&id].panes.clone() {
                    clear(&mut self.panes.get_mut(&pid).unwrap().options, &full);
                }
            }
            clear(self.option_map_mut(target), &full);
            if index.is_none()
                && let Target::Global(scope) = target
                && spec(&name).is_some()
            {
                self.option_map_mut(target).extend(
                    defaults(scope)
                        .into_iter()
                        .filter(|(key, _)| key == &name || array_key(key, &name).is_some()),
                );
            }
            if name == "monitor-silence" {
                self.reset_silence_all();
            }
            if name == "history-limit" {
                self.update_history_limits();
            }
            return Ok(self.option_changed(target, name == "pane-border-status"));
        }
        let inherited = self.effective_map(target);
        let old = inherited.get(&name).map(String::as_str).unwrap_or("");
        let mut value = a
            .rest
            .get(1)
            .map(|v| admission::text(v, "option value"))
            .transpose()?;
        if a.has(b'F')
            && let Some(raw) = value.as_ref()
        {
            value = Some(admission::text(
                &self.expand(
                    raw.as_bytes(),
                    format_context.session,
                    format_window,
                    format_context.current_pane,
                    &format_context,
                )?,
                "option value",
            )?);
        }
        let reset_silence = name == "monitor-silence";
        let update_history = name == "history-limit";
        let kind = spec(&name).map_or(Kind::String, |s| s.kind);
        let mut value = validate(kind, value.as_deref(), old)?;
        if name == "message-limit" && value.parse::<usize>().unwrap_or(0) > MAX_DIAGNOSTIC_MESSAGES
        {
            return Err(format!(
                "message-limit exceeds native limit: {MAX_DIAGNOSTIC_MESSAGES}"
            ));
        }
        if a.has(b'a')
            && matches!(kind, Kind::String)
            && let Some(local) = self.option_map(target).get(&name)
        {
            value = format!(
                "{local}{}{value}",
                if metadata(&name).is_some_and(|m| m.style) {
                    ","
                } else {
                    ""
                }
            );
        }
        let validation = (|| {
            if metadata(&name).is_some_and(|m| m.colour)
                && !value.is_empty()
                && !value.contains("#{")
                && !super::styles::valid_color(&value)
            {
                return Err(format!("invalid colour: {value}"));
            }
            if matches!(name.as_str(), "window-style" | "window-active-style") {
                super::styles::validate(&value)?;
            }
            if name == "pane-colours" {
                for color in value.split([' ', ',']).filter(|value| !value.is_empty()) {
                    if color != "default"
                        && color != "none"
                        && super::terminal_options::colour(color).is_none()
                    {
                        return Err(format!("bad colour: {color}"));
                    }
                }
            }
            if let Some(metadata) = metadata(&name)
                && !metadata.pattern.is_empty()
                && !tmuxx_format::glob_match(metadata.pattern.as_bytes(), value.as_bytes())
                    .map_err(str::to_owned)?
            {
                return Err(format!("value is invalid: {value}"));
            }
            Ok(())
        })();
        if let Err(error) = validation {
            if matches!(name.as_str(), "window-style" | "window-active-style")
                && !error.starts_with("invalid style:")
            {
                return Err(error);
            }
            if matches!(kind, Kind::String) && !name.starts_with('@') {
                // C restores the inherited value as a local entry after failed string validation.
                self.option_map_mut(target).insert(name, old.to_owned());
            }
            return Err(error);
        }
        let layout = name == "pane-border-status";
        let map = self.option_map_mut(target);
        if let Kind::Array(separator) = kind {
            map.entry(name.clone()).or_default();
            if index.is_some() {
                if a.has(b'a') {
                    map.entry(full).or_default().push_str(&value);
                } else {
                    map.insert(full, value);
                }
            } else {
                if !a.has(b'a') {
                    clear(map, &name);
                }
                assign(map, &name, separator, &value);
            }
        } else {
            map.insert(name, value);
        }
        if reset_silence {
            self.reset_silence_all();
        }
        if update_history {
            self.update_history_limits();
        }
        Ok(self.option_changed(target, layout))
    }
    fn update_history_limits(&mut self) {
        let sessions = self
            .sessions
            .iter()
            .map(|(&id, session)| {
                let limit = self.effective_map(Target::Session(id))["history-limit"]
                    .parse::<usize>()
                    .unwrap_or(2000);
                let panes = session
                    .windows
                    .values()
                    .flat_map(|window| self.windows[window].panes.iter().copied())
                    .collect::<Vec<_>>();
                (panes, limit)
            })
            .collect::<Vec<_>>();
        for (panes, limit) in sessions {
            for pane in panes {
                let pane = self.panes.get_mut(&pane).unwrap();
                pane.history_limit = limit;
                pane.terminal.set_history_limit(limit);
            }
        }
    }

    fn option_changed(&mut self, target: Target, layout: bool) -> Outcome {
        let mut out = Outcome {
            changed: true,
            ..Outcome::default()
        };
        if layout {
            let windows = match target {
                Target::Window(id) => vec![id],
                Target::Pane(id) => vec![self.panes[&id].window],
                _ => self.windows.keys().copied().collect(),
            };
            self.finish_pane_layouts(&windows, &mut out);
        }
        out
    }
    pub(super) fn show_options(
        &self,
        command: &str,
        args: &[Vec<u8>],
        ctx: &ClientContext,
    ) -> Result<Outcome, String> {
        let a = Args::parse(args, "AF:gHpqsvwt:")?;
        if a.rest.len() > 1 {
            return Err("too many arguments".into());
        }
        let format_context = self.option_format_context(&a, ctx);
        let format_window = format_context
            .current_pane
            .and_then(|pane| self.panes.get(&pane).map(|pane| pane.window));
        let argument = a
            .rest
            .first()
            .map(|v| {
                self.expand(
                    v,
                    format_context.session,
                    format_window,
                    format_context.current_pane,
                    &format_context,
                )
                .and_then(|v| admission::text(&v, "option name"))
            })
            .transpose()?;
        let requested = argument.as_deref().map(resolve_option_name).transpose();
        let requested = match requested {
            Ok(value) => value,
            Err(_) if a.has(b'q') => return Ok(Outcome::default()),
            Err(error) => return Err(error),
        };
        let target = match self.option_target(
            &a,
            command,
            requested.as_ref().map_or("@", |(name, _)| name),
            ctx,
        ) {
            Ok(value) => value,
            Err(_) if a.has(b'q') => return Ok(Outcome::default()),
            Err(error) => return Err(error),
        };
        let map = if a.has(b'A') {
            self.effective_map(target)
        } else {
            self.option_map(target).clone()
        };
        let mut out = Outcome::default();
        if let Some((name, _)) = &requested
            && name.starts_with('@')
            && !map.contains_key(name)
            && !a.has(b'q')
        {
            return Err(format!("invalid option: {name}"));
        }
        let names: Vec<&str> = if let Some((name, _)) = &requested {
            vec![name]
        } else {
            map.keys()
                .filter(|name| {
                    name.starts_with('@')
                        && !name.contains('[')
                        && (a.has(b'H') || !self.user_option_is_hook(target, name))
                })
                .map(String::as_str)
                .chain(
                    SOURCE_METADATA
                        .iter()
                        .filter(available)
                        .filter(|m| {
                            (!m.hook || a.has(b'H'))
                                && match target {
                                    Target::Global(scope) => m.spec.scope == scope,
                                    Target::Session(_) => m.spec.scope == b'c',
                                    Target::Window(_) => m.spec.scope == b'w',
                                    Target::Pane(_) => m.spec.pane,
                                }
                        })
                        .map(|m| m.spec.name),
                )
                .collect()
        };
        for name in names {
            let Some((name, value)) = map.get_key_value(name) else {
                continue;
            };
            if requested
                .as_ref()
                .is_some_and(|(requested, _)| requested != name)
            {
                continue;
            }
            let array_option = spec(name).is_some_and(|s| matches!(s.kind, Kind::Array(_)));
            let values = if let Some((_, Some(index))) = &requested {
                vec![(
                    Some(index.clone()),
                    if array_option {
                        map.get(&format!("{name}[{index}]"))
                            .cloned()
                            .unwrap_or_default()
                    } else {
                        value.clone()
                    },
                )]
            } else if array_option {
                let values = array(&map, name);
                if values.is_empty() {
                    vec![(None, String::new())]
                } else {
                    values.into_iter().map(|(k, v)| (Some(k), v)).collect()
                }
            } else {
                vec![(None, value.clone())]
            };
            for (key, value) in values {
                let has_value = !array_option || key.is_some();
                let parent = a.has(b'A')
                    && !self.option_map(target).contains_key(name)
                    && (has_value || requested.is_none());
                let string = metadata(name).is_none_or(|m| m.value_type == "STRING");
                if let Some(format) = a.get(b'F') {
                    let fields = BTreeMap::from([
                        ("option_name", name.clone()),
                        ("option_value", value.clone()),
                        ("option_value_only", u8::from(a.has(b'v')).to_string()),
                        ("option_is_parent", u8::from(parent).to_string()),
                        ("option_is_array", u8::from(array_option).to_string()),
                        ("option_is_string", u8::from(string).to_string()),
                        (
                            "option_is_user",
                            u8::from(name.starts_with('@')).to_string(),
                        ),
                        (
                            "option_is_hook",
                            u8::from(metadata(name).is_some_and(|m| m.hook)).to_string(),
                        ),
                        ("option_has_value", u8::from(has_value).to_string()),
                        ("option_has_array_key", u8::from(key.is_some()).to_string()),
                        ("option_array_key", key.clone().unwrap_or_default()),
                    ]);
                    let mut context = format_context.clone();
                    context.formats.extend(
                        fields
                            .into_iter()
                            .map(|(k, v)| (k.as_bytes().to_vec(), v.into_bytes())),
                    );
                    line(
                        &mut out.output,
                        self.expand(
                            format,
                            context.session,
                            format_window,
                            context.current_pane,
                            &context,
                        )?,
                    );
                } else if a.has(b'v') {
                    if has_value {
                        line(&mut out.output, value);
                    }
                } else {
                    let full = key.map_or_else(|| name.clone(), |key| format!("{name}[{key}]"));
                    let mut rendered =
                        format!("{full}{}", if parent { "*" } else { "" }).into_bytes();
                    if has_value {
                        rendered.push(b' ');
                        rendered.extend(if string {
                            tmuxx_command::display_argument(value.as_bytes())
                        } else {
                            value.into_bytes()
                        });
                    }
                    line(&mut out.output, rendered);
                }
            }
        }
        Ok(out)
    }
}
fn parse_size_raw(value: &str) -> Option<(u32, u32)> {
    fn unsigned(value: &str) -> Option<(u32, &str)> {
        let value = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let (negative, digits) = value
            .strip_prefix('-')
            .map_or((false, value.strip_prefix('+').unwrap_or(value)), |v| {
                (true, v)
            });
        let end = digits.bytes().take_while(u8::is_ascii_digit).count();
        if end == 0 {
            return None;
        }
        let number = digits[..end].bytes().fold(0u32, |n, b| {
            n.wrapping_mul(10).wrapping_add(u32::from(b - b'0'))
        });
        Some((
            if negative {
                number.wrapping_neg()
            } else {
                number
            },
            &digits[end..],
        ))
    }
    let (cols, rest) = unsigned(value)?;
    let (rows, _) = unsigned(rest.strip_prefix('x')?)?;
    Some((cols, rows))
}

fn validate(kind: Kind, value: Option<&str>, old: &str) -> Result<String, String> {
    if matches!(kind, Kind::Flag) {
        return match value.map(str::to_ascii_lowercase).as_deref() {
            None | Some("") => Ok(if old == "on" { "off" } else { "on" }.into()),
            Some("on" | "yes" | "1") => Ok("on".into()),
            Some("off" | "no" | "0") => Ok("off".into()),
            _ => Err(format!("bad value: {}", value.unwrap())),
        };
    }
    if let Kind::Choice(choices) = kind {
        let value =
            value.unwrap_or_else(|| match choices.iter().position(|choice| *choice == old) {
                Some(0) => choices.get(1).copied().unwrap_or(old),
                Some(1) => choices[0],
                _ => old,
            });
        if choices.contains(&value) {
            return Ok(value.into());
        }
        return Err(format!("unknown value: {value}"));
    }
    let value = value.ok_or("empty value")?;
    if value.contains('\0') {
        return Err("option contains NUL".into());
    }
    if let Kind::Number(min, max) = kind {
        let number = value
            .trim_start_matches(|c: char| c.is_ascii_whitespace())
            .parse::<i64>()
            .map_err(|error| {
                format!(
                    "value is {}: {value}",
                    match error.kind() {
                        std::num::IntErrorKind::PosOverflow => "too large",
                        std::num::IntErrorKind::NegOverflow => "too small",
                        _ => "invalid",
                    }
                )
            })?;
        if number < min {
            return Err(format!("value is too small: {value}"));
        }
        if number > max {
            return Err(format!("value is too large: {value}"));
        }
        return Ok(number.to_string());
    }
    if matches!(kind, Kind::Key) {
        return super::input::canonical_key(value).ok_or_else(|| format!("bad key: {value}"));
    }
    Ok(value.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(engine: &mut Engine, context: &mut ClientContext, args: &[&str]) -> Outcome {
        let command = tmuxx_command::parse_argv(args.iter().map(|s| s.as_bytes())).unwrap();
        engine.execute(&command[0], context).unwrap()
    }
    #[test]
    fn unsupported_native_settings_reject_without_mutating_defaults() {
        let mut e = Engine::default();
        let mut c = ClientContext::default();
        run(&mut e, &mut c, &["new", "-d"]);
        for metadata in SOURCE_METADATA
            .iter()
            .filter(available)
            .filter(|m| !m.hook && !WRITABLE.contains(&m.spec.name))
        {
            let before = (
                e.options.clone(),
                e.server_options.clone(),
                e.global_window_options.clone(),
            );
            let args = ["set", "-g", metadata.spec.name, metadata.spec.default];
            let command = tmuxx_command::parse_argv(args.iter().map(|s| s.as_bytes())).unwrap();
            let error = e
                .execute(&command[0], &mut c)
                .expect_err(metadata.spec.name);
            assert!(
                error.contains("unsupported"),
                "{}: {error}",
                metadata.spec.name
            );
            assert_eq!(
                before,
                (
                    e.options.clone(),
                    e.server_options.clone(),
                    e.global_window_options.clone()
                ),
                "{}",
                metadata.spec.name
            );
        }
    }
    #[test]
    fn default_size_scan_matches_unsigned_c_prefixes_and_clamps() {
        for (input, expected) in [
            ("80x24", Some((80, 24))),
            ("  +70x  +19rest", Some((70, 19))),
            ("0x0", Some((1, 1))),
            ("-1x7", Some((10000, 7))),
            ("4294967296x12", Some((1, 12))),
            ("80 x24", None),
            ("x3", None),
            ("2x", None),
        ] {
            assert_eq!(
                parse_size_raw(input)
                    .map(|(cols, rows)| (cols.clamp(1, 10000) as u16, rows.clamp(1, 10000) as u16)),
                expected,
                "{input}"
            );
        }
    }
    #[test]
    fn window_default_command_overrides_session_and_is_fixed_for_respawn() {
        let mut e = Engine::default();
        let mut c = ClientContext::default();
        run(
            &mut e,
            &mut c,
            &["set", "-g", "default-command", "session command"],
        );
        run(
            &mut e,
            &mut c,
            &["set", "-gw", "window-default-command", "global window"],
        );
        let out = run(&mut e, &mut c, &["new", "-d", "-n", "fixed"]);
        assert!(
            matches!(&out.effects[0], Effect::Spawn { argv, .. } if argv == &[b"global window".to_vec()])
        );
        run(
            &mut e,
            &mut c,
            &["set", "-w", "window-default-command", "local window"],
        );
        let out = run(&mut e, &mut c, &["splitw", "-h"]);
        assert!(
            matches!(&out.effects[0], Effect::Spawn { argv, .. } if argv == &[b"local window".to_vec()])
        );
        run(&mut e, &mut c, &["set", "-w", "window-default-command", ""]);
        let out = run(&mut e, &mut c, &["splitw", "-v", "explicit command"]);
        assert!(
            matches!(&out.effects[0], Effect::Spawn { argv, .. } if argv == &[b"explicit command".to_vec()])
        );
        let out = run(&mut e, &mut c, &["respawnp", "-k", "-t", "%1"]);
        assert!(out.effects.iter().any(|effect| matches!(effect, Effect::Spawn { argv, .. } if argv == &[b"local window".to_vec()])));
    }
    #[test]
    fn environment_array_matches_globs_and_hidden_removals_on_attach() {
        let mut e = Engine::default();
        let mut c = ClientContext {
            control: true,
            environment: BTreeMap::from([
                ("MATCH_A".into(), "first".into()),
                ("OTHER".into(), "ignored".into()),
            ]),
            ..ClientContext::default()
        };
        run(
            &mut e,
            &mut c,
            &["set", "-g", "update-environment", "MATCH_[A-Z] MISSING"],
        );
        run(&mut e, &mut c, &["new", "-d", "-s", "main"]);
        assert_eq!(
            e.sessions[&0]
                .environment
                .get("MATCH_A")
                .map(String::as_str),
            Some("first")
        );
        assert!(!e.sessions[&0].environment.contains_key("OTHER"));
        assert!(e.sessions[&0].removed_environment.contains("MISSING"));
        run(&mut e, &mut c, &["setenv", "-h", "MATCH_A", "hidden"]);
        c.environment.insert("MATCH_A".into(), "second".into());
        run(&mut e, &mut c, &["attach", "-E"]);
        assert!(!e.spawn_environment(0).contains_key("MATCH_A"));
        run(&mut e, &mut c, &["attach"]);
        assert_eq!(
            e.spawn_environment(0).get("MATCH_A").map(String::as_str),
            Some("second")
        );
    }
    #[test]
    fn synchronized_keys_use_each_application_mode_and_prefix_option() {
        let mut e = Engine::default();
        let mut c = ClientContext::default();
        run(&mut e, &mut c, &["new", "-d"]);
        run(&mut e, &mut c, &["splitw", "-h"]);
        e.feed(1, b"\x1b[?1h");
        run(&mut e, &mut c, &["set", "-w", "synchronize-panes", "on"]);
        let out = run(&mut e, &mut c, &["send", "-t", "%0", "Up"]);
        assert!(matches!(&out.effects[0], Effect::Write { pane: 0, data } if data == b"\x1b[A"));
        assert!(matches!(&out.effects[1], Effect::Write { pane: 1, data } if data == b"\x1bOA"));
        run(&mut e, &mut c, &["set", "prefix", "C-a"]);
        let out = run(&mut e, &mut c, &["send-prefix"]);
        assert_eq!(out.effects.len(), 2);
        assert!(
            out.effects
                .iter()
                .all(|e| matches!(e, Effect::Write { data, .. } if data == b"\x01"))
        );
        run(
            &mut e,
            &mut c,
            &["set", "-p", "-t", "%0", "synchronize-panes", "off"],
        );
        assert_eq!(
            run(&mut e, &mut c, &["send", "-t", "%1", "Up"])
                .effects
                .len(),
            1
        );
    }
    #[test]
    fn buffer_limit_evicts_only_automatic_buffers_and_respects_renames() {
        let mut e = Engine::default();
        let mut c = ClientContext::default();
        run(&mut e, &mut c, &["set", "-s", "buffer-limit", "2"]);
        e.store_buffer(Some("named".into()), b"kept".to_vec())
            .unwrap();
        let (old, _) = e.store_buffer(None, b"old".to_vec()).unwrap().unwrap();
        let (next, _) = e.store_buffer(None, b"next".to_vec()).unwrap().unwrap();
        let (newest, _) = e.store_buffer(None, b"newest".to_vec()).unwrap().unwrap();
        assert!(!e.buffers.contains_key(&old));
        assert!(
            e.buffers.contains_key("named")
                && e.buffers.contains_key(&next)
                && e.buffers.contains_key(&newest)
        );
        e.store_buffer(Some(next.clone().into_bytes()), b"now named".to_vec())
            .unwrap();
        e.store_buffer(None, b"last".to_vec()).unwrap();
        assert!(e.buffers.contains_key(&next) && e.buffers.contains_key(&newest));
    }
    #[test]
    fn history_budget_and_default_command_are_allocated_from_session() {
        let mut e = Engine::default();
        let mut c = ClientContext::default();
        run(&mut e, &mut c, &["new", "-d"]);
        run(&mut e, &mut c, &["set", "history-limit", "31"]);
        run(&mut e, &mut c, &["set", "default-command", "echo chosen"]);
        let out = run(&mut e, &mut c, &["neww"]);
        assert!(
            matches!(&out.effects[0], Effect::Spawn { argv, .. } if argv == &[b"echo chosen".to_vec()])
        );
        run(&mut e, &mut c, &["set", "history-limit", "99"]);
        assert_eq!(
            run(&mut e, &mut c, &["display", "-p", "#{history_limit}"]).output,
            b"99\n"
        );
        assert_eq!(e.panes[&0].history_limit, 99);
    }
}
