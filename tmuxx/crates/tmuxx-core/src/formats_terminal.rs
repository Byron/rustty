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

    #[test]
    fn program_status_formats_enumerate_rustty_extensions_without_changing_c_registry() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        let command = |source| tmuxx_command::parse(source).unwrap().remove(0);
        engine
            .execute(&command(b"new-session -d ''"), &mut context)
            .unwrap();
        engine.feed(0, b"\x1b]7501;state=working:app=builder\x07\x1b]7501;state=blocked:id=job:kind=permission:progress=47:title=Sm9i:msg=QXBwcm92ZT8\x07");
        let query = b"#{pane_program_state}|#{pane_program_progress}|#{pane_program_app}|#{pane_program_title}|#{pane_program_message}";
        assert_eq!(
            engine
                .expand(query, Some(0), Some(0), Some(0), &context)
                .unwrap(),
            b"blocked|47|builder|Job|Approve?"
        );
        assert_eq!(
            engine.panes[&0].terminal.program_status().activity(),
            serde_json::from_value(serde_json::json!("needs_input")).unwrap()
        );
        assert!(engine.panes[&0].terminal.agent_status().snapshot.is_none());
        let output = engine
            .execute(&command(b"display-message -a -t %0"), &mut context)
            .unwrap()
            .output;
        let output = String::from_utf8(output).unwrap();
        for key in tmuxx_terminal::PROGRAM_STATUS_FORMATS {
            assert_eq!(
                output
                    .lines()
                    .filter(|line| line.starts_with(&format!("{key}=")))
                    .count(),
                1,
                "{key}"
            );
            assert!(!crate::format_metadata::FORMAT_KEYS.contains(key));
        }
        let records: serde_json::Value = serde_json::from_str(
            output
                .lines()
                .find_map(|line| line.strip_prefix("pane_program_status="))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(records[1]["app"], "builder");
        assert_eq!(records[1]["kind"], "permission");
        engine.feed(0, b"\x1b]7501;state=clear\x07");
        assert_eq!(
            engine.panes[&0]
                .terminal
                .format_value("pane_program_status")
                .as_deref(),
            Some("[]")
        );
        assert_eq!(
            engine.panes[&0].terminal.format_value("pane_program_state"),
            None
        );
    }

    #[test]
    fn program_status_exit_keeps_unseen_results_until_acknowledged() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        for source in [
            b"new-session -d ''".as_slice(),
            b"set-option -w remain-on-exit on",
        ] {
            engine
                .execute(&tmuxx_command::parse(source).unwrap()[0], &mut context)
                .unwrap();
        }
        engine.feed(
            0,
            b"\x1b]7501;state=done:id=result\x07\x1b]7501;state=working:id=work\x07",
        );
        engine.record_exit(0, 0, None);
        assert_eq!(engine.panes[&0].terminal.program_status().len(), 2);
        engine.feed(0, b"\x1b]7501;state=blocked:id=last\x07");
        assert!(engine.exited_with_signal(0, 0, None).changed);
        assert_eq!(engine.panes[&0].terminal.program_status().len(), 1);
        assert!(
            engine.panes[&0]
                .terminal
                .program_status()
                .get(Some("result"))
                .is_some()
        );
        assert!(engine.acknowledge_native_input([0]));
        assert!(engine.panes[&0].terminal.program_status().is_empty());
    }
}
