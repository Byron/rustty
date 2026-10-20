use std::collections::BTreeMap;
use std::ffi::OsString;
use std::process::Command;

pub(super) fn command(host: &str, environment: &BTreeMap<String, String>) -> Command {
    let mut command = Command::new("ssh");
    command
        .args(["-T", "-o", "LogLevel=ERROR", "--", host])
        .envs(environment);
    let value = |name: &str| {
        environment
            .get(name)
            .map(OsString::from)
            .or_else(|| std::env::var_os(name))
    };
    if value("SSH_ASKPASS").is_some_and(|value| !value.is_empty())
        && value("SSH_ASKPASS_REQUIRE").is_none_or(|value| value.is_empty())
    {
        // SSH stdin carries tmux traffic; configured prompts belong in askpass.
        command.env("SSH_ASKPASS_REQUIRE", "force");
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(values: &[(&str, &str)]) -> BTreeMap<String, String> {
        values
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn ssh_uses_configured_environment_and_does_not_disable_authentication_prompts() {
        let command = command(
            "user@host",
            &environment(&[("SSH_AUTH_SOCK", "/configured/agent")]),
        );
        assert_eq!(command.get_program(), "ssh");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["-T", "-o", "LogLevel=ERROR", "--", "user@host"]
        );
        assert!(command.get_envs().any(|(key, value)| {
            key == "SSH_AUTH_SOCK" && value == Some("/configured/agent".as_ref())
        }));
    }

    #[test]
    fn configured_askpass_works_without_a_terminal_or_display() {
        let command = command(
            "host",
            &environment(&[("SSH_ASKPASS", "/helper"), ("SSH_ASKPASS_REQUIRE", "")]),
        );
        assert!(command.get_envs().any(|(key, value)| {
            key == "SSH_ASKPASS_REQUIRE" && value == Some("force".as_ref())
        }));
    }

    #[test]
    fn explicit_askpass_policy_and_missing_helper_are_preserved() {
        for (helper, requirement) in [("/helper", "never"), ("", "")] {
            let command = command(
                "host",
                &environment(&[
                    ("SSH_ASKPASS", helper),
                    ("SSH_ASKPASS_REQUIRE", requirement),
                ]),
            );
            assert!(command.get_envs().any(|(key, value)| {
                key == "SSH_ASKPASS_REQUIRE" && value == Some(requirement.as_ref())
            }));
        }
    }
}
