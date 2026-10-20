use super::*;

#[derive(Clone, Debug, Default)]
pub struct ShellState {
    pub last_prompt_time: Option<u64>,
    pub command_start_time: Option<u64>,
    pub command_end_time: Option<u64>,
    pub command_line: Option<String>,
    pub command_status: Option<i32>,
    pub command_running: bool,
}

impl Engine {
    pub(super) fn terminal_format(&self, key: &str, pane: &Pane) -> Option<String> {
        let shell = &pane.shell_state;
        Some(match key {
            "pane_synchronized" => {
                u8::from(self.pane_option(pane.id, "synchronize-panes") == Some("on")).to_string()
            }
            "pane_unseen_changes" => "0".into(), // Native views own selections; the server never enters copy mode.
            "pane_fg" => self.pane_color_name(pane.id, 0),
            "pane_bg" => self.pane_color_name(pane.id, 1),
            "cursor_colour" => pane
                .terminal
                .application_color(2)
                .map(styles::rgb_name)
                .unwrap_or_else(|| self.cursor_default_color(pane.id)),
            "pane_last_prompt_time" => shell.last_prompt_time?.to_string(),
            "pane_command_start_time" => shell.command_start_time?.to_string(),
            "pane_command_end_time" => shell.command_end_time?.to_string(),
            "pane_command_line" => shell.command_line.clone()?,
            "pane_command_status" => shell.command_status?.to_string(),
            "pane_command_running" => u8::from(shell.command_running).to_string(),
            "pane_command_duration" => (if shell.command_running {
                self.now / 1_000_000
            } else {
                shell.command_end_time.unwrap_or(0)
            })
            .saturating_sub(shell.command_start_time?)
            .to_string(),
            _ => return pane.terminal.format_value(key),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shell_time_fields_follow_owner_clock_and_repeated_commands() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        engine
            .execute(
                &tmuxx_command::parse(b"new -d 'sleep 30'").unwrap()[0],
                &mut context,
            )
            .unwrap();
        engine.now = 10_000_000;
        assert!(matches!(
            engine.feed(0, b"\x1b]133;A\x07").as_slice(),
            [Effect::ShellPrompt { pane: 0 }]
        ));
        engine.now = 12_000_000;
        assert!(matches!(
            engine
                .feed(0, b"\x1b]133;C;cmdline_url=echo%20hi\x07")
                .as_slice(),
            [Effect::ShellCommand {
                pane: 0,
                started: true,
                ..
            }]
        ));
        engine.now = 15_000_000;
        let query = |engine: &Engine, field| engine.terminal_format(field, &engine.panes[&0]);
        assert_eq!(
            query(&engine, "pane_command_duration").as_deref(),
            Some("3")
        );
        engine.feed(0, b"\x1b]133;D;7\x07");
        engine.now = 18_000_000;
        for (field, value) in [
            ("pane_last_prompt_time", "10"),
            ("pane_command_start_time", "12"),
            ("pane_command_end_time", "15"),
            ("pane_command_line", "echo hi"),
            ("pane_command_status", "7"),
            ("pane_command_running", "0"),
            ("pane_command_duration", "3"),
        ] {
            assert_eq!(query(&engine, field).as_deref(), Some(value), "{field}");
        }
        engine.feed(0, b"\x1b]133;C\x07");
        assert_eq!(query(&engine, "pane_command_end_time"), None);
        assert_eq!(query(&engine, "pane_command_status"), None);
        assert_eq!(query(&engine, "pane_command_line"), None);
        engine.now = 1_000_000;
        assert_eq!(
            query(&engine, "pane_command_duration").as_deref(),
            Some("0")
        );
    }
}
