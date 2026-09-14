//! Read only Windows Terminal's selected shell, including fragment profiles.
use serde_json::Value;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(super) struct ResolvedShell {
    pub argv: Vec<String>,
    pub source: String,
}

#[cfg(windows)]
pub(super) fn resolve() -> io::Result<ResolvedShell> {
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| invalid("LOCALAPPDATA is unavailable"))?;
    let settings = [
        local.join("Packages/Microsoft.WindowsTerminal_8wekyb3d8bbwe/LocalState/settings.json"),
        local.join("Microsoft/Windows Terminal/settings.json"),
        local.join(
            "Packages/Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe/LocalState/settings.json",
        ),
    ];
    let mut fragments = Vec::new();
    if let Some(program_data) = std::env::var_os("ProgramData") {
        fragments.push(PathBuf::from(program_data).join("Microsoft/Windows Terminal/Fragments"));
    }
    // User fragments override machine fragments with the same identity.
    fragments.push(local.join("Microsoft/Windows Terminal/Fragments"));
    resolve_paths(&settings, &fragments)
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn read_json(path: &Path) -> io::Result<Value> {
    let mut text = String::new();
    fs::File::open(path)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_string(&mut text)?;
    if text.len() > 2 * 1024 * 1024 {
        return Err(invalid("Windows Terminal settings exceed 2 MiB"));
    }
    parse_jsonc(&text).map_err(|error| invalid(format!("{}: {error}", path.display())))
}

fn resolve_paths(settings: &[PathBuf], roots: &[PathBuf]) -> io::Result<ResolvedShell> {
    let mut selected = None;
    for path in settings {
        match read_json(path) {
            Ok(value) => {
                selected = Some((path, value));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let (path, value) =
        selected.ok_or_else(|| invalid("Windows Terminal settings were not found"))?;
    let default = value["defaultProfile"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("Windows Terminal has no defaultProfile"))?;
    let profiles = value["profiles"]["list"]
        .as_array()
        .or_else(|| value["profiles"].as_array());
    let matches = profiles
        .into_iter()
        .flatten()
        .filter(|profile| {
            profile["guid"]
                .as_str()
                .is_some_and(|guid| same_id(guid, default))
                || profile["name"].as_str() == Some(default)
        })
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        return Err(invalid(
            "Windows Terminal default profile name is ambiguous",
        ));
    }
    let profile = matches.first().copied();
    let guid = profile
        .and_then(|profile| profile["guid"].as_str())
        .unwrap_or(default);
    let source = profile.and_then(|profile| profile["source"].as_str());
    let name = profile
        .and_then(|profile| profile["name"].as_str())
        .unwrap_or(default);
    let explicit = profile.and_then(commandline);
    let command = if let Some(command) = explicit {
        command.to_owned()
    } else {
        let mut found = None;
        let mut updates = Vec::new();
        for root in roots {
            let entries = match fs::read_dir(root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let mut providers = entries.filter_map(Result::ok).collect::<Vec<_>>();
            providers.sort_by_key(|entry| entry.file_name());
            for provider in providers {
                if source.is_some_and(|source| {
                    !provider
                        .file_name()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(source)
                }) {
                    continue;
                }
                let Ok(files) = fs::read_dir(provider.path()) else {
                    continue;
                };
                let mut files = files.filter_map(Result::ok).collect::<Vec<_>>();
                files.sort_by_key(|entry| entry.file_name());
                for file in files {
                    if file
                        .path()
                        .extension()
                        .is_none_or(|extension| !extension.eq_ignore_ascii_case("json"))
                    {
                        continue;
                    }
                    let Ok(fragment) = read_json(&file.path()) else {
                        continue;
                    };
                    for entry in fragment["profiles"].as_array().into_iter().flatten() {
                        if entry["guid"].as_str().is_some_and(|id| same_id(id, guid))
                            && let Some(command) = commandline(entry)
                        {
                            found = Some(command.to_owned());
                        }
                        if entry["updates"]
                            .as_str()
                            .is_some_and(|id| same_id(id, guid))
                            && let Some(command) = commandline(entry)
                        {
                            updates.push(command.to_owned());
                        }
                    }
                }
            }
        }
        updates
            .pop()
            .or(found)
            .or_else(|| commandline(&value["profiles"]["defaults"]).map(str::to_owned))
            .ok_or_else(|| {
                invalid(format!(
                    "Windows Terminal profile {name:?} has no resolvable commandline (source {})",
                    source.unwrap_or("none")
                ))
            })?
    };
    let expanded = expand_environment(&command, |name| std::env::var(name).ok())?;
    let argv = split_command_line(&expanded)?;
    if argv.first().is_none_or(String::is_empty) {
        return Err(invalid("Windows Terminal default commandline is empty"));
    }
    Ok(ResolvedShell {
        argv,
        source: format!("Windows Terminal profile {name:?} ({})", path.display()),
    })
}

fn commandline(profile: &Value) -> Option<&str> {
    profile["commandline"]
        .as_str()
        .filter(|command| !command.trim().is_empty())
}

fn same_id(left: &str, right: &str) -> bool {
    left.trim_matches(['{', '}'])
        .eq_ignore_ascii_case(right.trim_matches(['{', '}']))
}

/// Windows command lines use backslash-before-quote rules, not POSIX shell quoting.
fn split_command_line(command: &str) -> io::Result<Vec<String>> {
    if command.contains('\0') {
        return Err(invalid("shell commandline contains NUL"));
    }
    let mut input = command.chars().peekable();
    let mut args = Vec::new();
    while input.peek().is_some() {
        while input.peek().is_some_and(|c| matches!(c, ' ' | '\t')) {
            input.next();
        }
        if input.peek().is_none() {
            break;
        }
        let mut arg = String::new();
        let mut quoted = false;
        while let Some(&ch) = input.peek() {
            if !quoted && matches!(ch, ' ' | '\t') {
                break;
            }
            if ch == '\\' {
                let mut count = 0;
                while input.peek() == Some(&'\\') {
                    input.next();
                    count += 1;
                }
                if input.peek() == Some(&'"') {
                    arg.extend(std::iter::repeat_n('\\', count / 2));
                    input.next();
                    if count % 2 == 1 {
                        arg.push('"');
                    } else {
                        quoted = !quoted;
                    }
                } else {
                    arg.extend(std::iter::repeat_n('\\', count));
                }
            } else if ch == '"' {
                input.next();
                if quoted && input.peek() == Some(&'"') {
                    input.next();
                    arg.push('"');
                } else {
                    quoted = !quoted;
                }
            } else {
                arg.push(ch);
                input.next();
            }
        }
        if quoted {
            return Err(invalid("shell commandline has an unmatched quote"));
        }
        args.push(arg);
    }
    Ok(args)
}

fn expand_environment(
    command: &str,
    mut lookup: impl FnMut(&str) -> Option<String>,
) -> io::Result<String> {
    let mut output = String::new();
    let mut rest = command;
    while let Some(start) = rest.find('%') {
        output.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let Some(end) = tail.find('%') else {
            output.push_str(&rest[start..]);
            return Ok(output);
        };
        let name = &tail[..end];
        if name.is_empty() {
            output.push_str("%%");
        } else {
            output.push_str(&lookup(name).ok_or_else(|| {
                invalid(format!("shell commandline references missing %{name}%"))
            })?);
        }
        rest = &tail[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

/// Strip JSONC comments and trailing commas without changing quoted strings.
fn parse_jsonc(text: &str) -> io::Result<Value> {
    let bytes = text.trim_start_matches('\u{feff}').as_bytes();
    let mut output = bytes.to_vec();
    let (mut index, mut quoted) = (0, false);
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if quoted => {
                index += 2;
                continue;
            }
            b'"' => quoted = !quoted,
            b'/' if !quoted && bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    output[index] = b' ';
                    index += 1;
                }
                continue;
            }
            b'/' if !quoted && bytes.get(index + 1) == Some(&b'*') => {
                output[index] = b' ';
                output[index + 1] = b' ';
                index += 2;
                while index + 1 < bytes.len() && &bytes[index..index + 2] != b"*/" {
                    if !matches!(bytes[index], b'\r' | b'\n') {
                        output[index] = b' ';
                    }
                    index += 1;
                }
                if index + 1 >= bytes.len() {
                    return Err(invalid("unterminated JSONC comment"));
                }
                output[index] = b' ';
                output[index + 1] = b' ';
                index += 2;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    index = 0;
    quoted = false;
    while index < output.len() {
        match output[index] {
            b'\\' if quoted => {
                index += 2;
                continue;
            }
            b'"' => quoted = !quoted,
            b',' if !quoted => {
                let next = output[index + 1..]
                    .iter()
                    .find(|byte| !byte.is_ascii_whitespace());
                if matches!(next, Some(b']' | b'}')) {
                    output[index] = b' ';
                }
            }
            _ => {}
        }
        index += 1;
    }
    serde_json::from_slice(&output).map_err(|error| invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "rustty-wt-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, text: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn sparse_git_profile_resolves_fragment_without_importing_other_settings() {
        let fixture = Fixture::new();
        let settings = fixture.write("settings.json", r#"{
            // Windows Terminal keeps the generated command in a fragment.
            "defaultProfile": "{2ECE5BFE-50ED-5F3A-AB87-5CD4BAAFED2B}",
            "profiles": {"list": [{"guid":"{2ece5bfe-50ed-5f3a-ab87-5cd4baafed2b}", "name":"Git Bash", "source":"Git", "commandline":null,}],},
        }"#);
        fixture.write(
            "fragments/Git/git-bash.json",
            r#"{"profiles":[{
            "guid":"{2ece5bfe-50ed-5f3a-ab87-5cd4baafed2b}",
            "commandline":"\"C:/Program Files/Git/bin/bash.exe\" -i -l",
            "startingDirectory":"Z:/must-not-import", "environment":{"BAD":"must-not-import"},
        }]}"#,
        );
        let shell = resolve_paths(&[settings], &[fixture.0.join("fragments")]).unwrap();
        assert_eq!(
            shell.argv,
            ["C:/Program Files/Git/bin/bash.exe", "-i", "-l"]
        );
        assert!(shell.source.contains("Git Bash"));
    }

    #[test]
    fn explicit_profile_command_overrides_fragments() {
        let fixture = Fixture::new();
        let settings = fixture.write("settings.json", r#"{"defaultProfile":"shell","profiles":{"list":[{"name":"shell","guid":"id","source":"Git","commandline":"cmd.exe /d"}]}}"#);
        fixture.write(
            "fragments/Git/shell.json",
            r#"{"profiles":[{"guid":"id","commandline":"wrong.exe"}]}"#,
        );
        assert_eq!(
            resolve_paths(&[settings], &[fixture.0.join("fragments")])
                .unwrap()
                .argv,
            ["cmd.exe", "/d"]
        );
    }

    #[test]
    fn windows_commandline_quotes_and_environment() {
        assert_eq!(
            split_command_line(r#""C:\Program Files\shell.exe" "" "a b" "C:\tail\\" "a\"b""#)
                .unwrap(),
            [
                r"C:\Program Files\shell.exe",
                "",
                "a b",
                r"C:\tail\",
                "a\"b"
            ]
        );
        assert!(split_command_line("\"unclosed").is_err());
        assert_eq!(
            expand_environment(r#""%ROOT%\bin\bash.exe" -l"#, |key| (key == "ROOT")
                .then(|| "C:/Git".into()))
            .unwrap(),
            r#""C:/Git\bin\bash.exe" -l"#
        );
        assert!(expand_environment("%MISSING%", |_| None).is_err());
    }

    #[test]
    fn jsonc_preserves_string_comments_and_rejects_broken_input() {
        let value =
            parse_jsonc(r#"/* header */ {"url":"https://example/*literal*/","list":[1,/*x*/],}"#)
                .unwrap();
        assert_eq!(value["url"], "https://example/*literal*/");
        assert_eq!(value["list"], serde_json::json!([1]));
        assert!(parse_jsonc("{/* incomplete").is_err());
        assert!(parse_jsonc("{broken}").is_err());
    }

    #[test]
    fn unresolved_dynamic_profile_is_an_error_not_a_guessed_shell() {
        let fixture = Fixture::new();
        let settings = fixture.write("settings.json", r#"{"defaultProfile":"id","profiles":{"list":[{"guid":"id","source":"Windows.Terminal.Wsl","name":"Ubuntu"}]}}"#);
        assert!(
            resolve_paths(&[settings], &[])
                .unwrap_err()
                .to_string()
                .contains("no resolvable commandline")
        );
    }
}
