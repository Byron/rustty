use super::*;

pub fn parse_window_name(input: &str) -> String {
    let name = input
        .strip_prefix('"')
        .unwrap_or(input)
        .split('"')
        .next()
        .unwrap_or("");
    let name = name
        .strip_prefix("exec ")
        .unwrap_or(name)
        .trim_start_matches([' ', '-']);
    let name = name
        .split(' ')
        .next()
        .unwrap_or("")
        .trim_end_matches(|c: char| c.is_ascii_control() || c == ' ');
    let name = if name.starts_with('/') {
        name.rsplit('/').next().unwrap_or(name)
    } else {
        name
    };
    clean_name(name, false)
}

pub fn clean_name(name: &str, untrusted: bool) -> String {
    let mut output = String::new();
    let name = if untrusted {
        name.replace("#(", "_(")
    } else {
        name.to_owned()
    };
    for character in name.chars() {
        match character {
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{7}' => output.push_str("\\a"),
            '\u{8}' => output.push_str("\\b"),
            '\u{b}' => output.push_str("\\v"),
            '\u{c}' => output.push_str("\\f"),
            '\\' => output.push_str("\\\\"),
            c if c.is_ascii_control() => output.push_str(&format!("\\{:03o}", c as u8)),
            c => output.push(c),
        }
    }
    output
}

impl Pane {
    pub fn start_command(&self) -> String {
        text(
            &self
                .argv
                .iter()
                .map(|arg| tmuxx_command::display_argument(arg))
                .collect::<Vec<_>>()
                .join(&b' '),
        )
    }
    pub fn current_command(&self) -> String {
        let fallback = self.start_command();
        parse_window_name(
            self.current_command
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(if fallback.is_empty() {
                    &self.shell
                } else {
                    &fallback
                }),
        )
    }
}
impl Engine {
    /// Refresh automatic names after the host updates foreground process metadata.
    pub fn rename_automatic_windows(&mut self) -> Outcome {
        let changes: Vec<_> = self
            .windows
            .values()
            .filter(|w| self.window_option(w.id, "automatic-rename") == Some("on"))
            .filter_map(|w| {
                let format = self.window_option(w.id, "automatic-rename-format")?;
                let name = self
                    .expand(
                        format.as_bytes(),
                        None,
                        Some(w.id),
                        Some(w.active),
                        &ClientContext::default(),
                    )
                    .ok()?;
                let name = clean_name(&String::from_utf8(name).ok()?, true);
                (name != w.name).then_some((w.id, name))
            })
            .collect();
        let mut out = Outcome::default();
        for (wid, name) in changes {
            self.windows.get_mut(&wid).unwrap().name = name.clone();
            out.notifications.push(self.window_renamed(wid));
            out.changed = true;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_names_follow_tmux_prefix_and_path_rules() {
        for (input, expected) in [
            ("exec /bin/sleep 9", "sleep"),
            ("-zsh", "zsh"),
            ("  --python -u", "python"),
            ("\"/bin/sh\" -l", "sh"),
            ("sleep;", "sleep;"),
            ("a\tb", "a\\tb"),
            ("/bin/sh\n", "sh"),
        ] {
            assert_eq!(parse_window_name(input), expected);
        }
    }
}
