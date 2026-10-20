use crate::{Args, Command};

/// C source metadata is a syntax contract, not a claim of implemented behavior.
#[derive(Debug)]
pub struct CommandSpec {
    pub name: &'static str,
    pub alias: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
    pub flags: &'static str,
    pub minimum: usize,
    pub maximum: Option<usize>,
    pub after_hook: bool,
    pub readonly: bool,
    pub starts_server: bool,
    pub source: &'static str,
    /// Flags whose effective values select an existing session, window, pane or client.
    pub selector_flags: &'static str,
}

include!("metadata.rs");

pub fn find(name: &[u8]) -> Result<&'static CommandSpec, String> {
    let mut found = None;
    let mut ambiguous = false;
    for entry in COMMANDS {
        if !entry.alias.is_empty() && name == entry.alias.as_bytes() {
            return Ok(entry);
        }
        if !entry.name.as_bytes().starts_with(name) {
            continue;
        }
        ambiguous |= found.is_some();
        found = Some(entry);
        if name == entry.name.as_bytes() {
            break;
        }
    }
    let printed = String::from_utf8_lossy(name);
    if ambiguous {
        let choices = COMMANDS
            .iter()
            .filter(|entry| entry.name.as_bytes().starts_with(name))
            .map(|entry| entry.name)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!("ambiguous command: {printed}, could be: {choices}"));
    }
    found.ok_or_else(|| format!("unknown command: {printed}"))
}

pub fn validate(command: &Command) -> Result<&'static CommandSpec, String> {
    validate_inner(command, false)
}

/// Validate commands for the Rust server's UTF-8 graph and client identities.
/// Wire clients must use `validate` so raw C-server selectors remain unchanged.
pub fn validate_server(command: &Command) -> Result<&'static CommandSpec, String> {
    validate_inner(command, true)
}

fn validate_inner(command: &Command, server: bool) -> Result<&'static CommandSpec, String> {
    let name = command.args.first().ok_or("no command")?;
    let entry = find(name)?;
    let args = Args::parse(&command.args[1..], entry.flags).map_err(|error| {
        if error == "usage requested" {
            format!("usage: {} {}", entry.name, entry.usage)
        } else {
            format!("command {}: {error}", entry.name)
        }
    })?;
    if args.rest.len() < entry.minimum {
        return Err(format!(
            "command {}: too few arguments (need at least {})",
            entry.name, entry.minimum
        ));
    }
    if let Some(maximum) = entry.maximum
        && args.rest.len() > maximum
    {
        return Err(format!(
            "command {}: too many arguments (need at most {maximum})",
            entry.name
        ));
    }
    if server {
        for flag in entry.selector_flags.bytes() {
            if args
                .get(flag)
                .is_some_and(|value| std::str::from_utf8(value).is_err())
            {
                return Err(format!(
                    "command {}: -{} target is not valid UTF-8",
                    entry.name, flag as char
                ));
            }
        }
    }
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_selectors_do_not_alias_malformed_bytes_or_restrict_wire_clients() {
        for (name, flag, rest) in [
            ("kill-session", "t", None),
            ("join-pane", "s", None),
            ("break-pane", "s", None),
            ("copy-mode", "s", None),
            ("display-panes", "s", None),
            ("detach-client", "s", None),
            ("switch-client", "c", None),
            ("refresh-client", "t", None),
            ("load-buffer", "t", Some("file")),
            ("send-keys", "c", Some("x")),
            ("attach-session", "t", None),
            ("switch-client", "t", None),
            ("link-window", "t", None),
            ("move-window", "t", None),
        ] {
            let mut command = Command::new(vec![
                name.as_bytes().to_vec(),
                format!("-{flag}").into_bytes(),
                b"replace\xff".to_vec(),
            ]);
            command
                .args
                .extend(rest.map(|value| value.as_bytes().to_vec()));
            assert!(validate(&command).is_ok(), "{name}");
            assert_eq!(
                validate_server(&command).unwrap_err(),
                format!("command {name}: -{flag} target is not valid UTF-8")
            );
            command.args[2] = "replace�".as_bytes().to_vec();
            assert!(validate_server(&command).is_ok(), "{name}");
        }
    }

    #[test]
    fn server_selector_validation_preserves_data_paths_and_effective_flag_values() {
        for args in [
            vec![&b"new-window"[..], b"-c", b"path\xff"],
            vec![&b"new-session"[..], b"-s", b"name\xff"],
            vec![&b"paste-buffer"[..], b"-s", b"separator\xff"],
            vec![&b"set-buffer"[..], b"-b", b"buffer\xff", b"data\xff"],
            vec![&b"kill-session"[..], b"-t", b"old\xff", b"-t", b"valid"],
        ] {
            let command = Command::new(args.into_iter().map(<[u8]>::to_vec).collect());
            assert!(validate_server(&command).is_ok(), "{:?}", command.args);
        }
    }

    #[test]
    fn names_aliases_unique_prefixes_and_argument_counts() {
        assert_eq!(find(b"new").unwrap().name, "new-session");
        assert_eq!(find(b"new-s").unwrap().name, "new-session");
        assert_eq!(
            find(b"list-c").unwrap_err(),
            "ambiguous command: list-c, could be: list-clients, list-commands"
        );
        assert_eq!(find(b"invalid").unwrap_err(), "unknown command: invalid");
        let command = |args: &[&str]| Command {
            blocks: Default::default(),
            args: args.iter().map(|s| s.as_bytes().to_vec()).collect(),
        };
        assert_eq!(
            validate(&command(&["rename-session"])).unwrap_err(),
            "command rename-session: too few arguments (need at least 1)"
        );
        assert_eq!(
            validate(&command(&["list-sessions", "extra"])).unwrap_err(),
            "command list-sessions: too many arguments (need at most 0)"
        );
        assert_eq!(
            validate(&command(&["list-sessions", "-!"])).unwrap_err(),
            "command list-sessions: invalid flag -!"
        );
        assert_eq!(
            validate(&command(&["list-sessions", "-F"])).unwrap_err(),
            "command list-sessions: -F expects an argument"
        );
        assert!(
            validate(&command(&["list-sessions", "-?"]))
                .unwrap_err()
                .starts_with("usage: list-sessions ")
        );
        assert!(validate(&command(&["new-session", "-d", "sh", "-c", "true"])).is_ok());
    }
}
