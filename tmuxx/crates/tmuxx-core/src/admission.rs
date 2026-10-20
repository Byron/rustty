//! Admission to native UTF-8 storage; byte-oriented protocol data stays bytes.

pub(super) fn text(bytes: &[u8], role: &str) -> Result<String, String> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| format!("{role} is not valid UTF-8"))
}

pub(super) fn path(bytes: &[u8]) -> Result<String, String> {
    let value = text(bytes, "working directory")?;
    if value.contains('\0') {
        return Err("working directory contains NUL".into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use crate::{ClientContext, Engine, Outcome};
    use tmuxx_command::Command;

    fn run(
        engine: &mut Engine,
        context: &mut ClientContext,
        args: &[&[u8]],
    ) -> Result<Outcome, String> {
        engine.execute(
            &Command::new(args.iter().map(|arg| arg.to_vec()).collect()),
            context,
        )
    }

    #[test]
    fn malformed_working_directories_reject_before_graph_or_session_mutation() {
        let mut engine = Engine::default();
        let mut context = ClientContext {
            control: true,
            cwd: Some("C:\\kept�".into()),
            ..ClientContext::default()
        };
        run(
            &mut engine,
            &mut context,
            &[b"new-session", b"-d", b"-s", b"kept", b""],
        )
        .unwrap();
        let original = engine.snapshot();
        for malformed in [b"C:\\kept\xff".as_slice(), b"C:\\kept\0"] {
            for args in [
                vec![
                    b"new-session".as_slice(),
                    b"-d",
                    b"-s",
                    b"other",
                    b"-c",
                    malformed,
                    b"",
                ],
                vec![
                    b"new-window",
                    b"-a",
                    b"-t",
                    b"kept:0",
                    b"-c",
                    malformed,
                    b"",
                ],
                vec![b"split-window", b"-c", malformed, b""],
                vec![b"new-pane", b"-c", malformed, b""],
                vec![b"respawn-pane", b"-k", b"-c", malformed],
                vec![b"respawn-window", b"-k", b"-c", malformed],
                vec![b"attach-session", b"-t", b"kept", b"-c", malformed],
            ] {
                let error = run(&mut engine, &mut context, &args).unwrap_err();
                assert!(error.contains("working directory"), "{args:?}: {error}");
                assert_eq!(engine.snapshot(), original, "{args:?}");
                assert_eq!(engine.sessions[&0].cwd.as_deref(), Some("C:\\kept�"));
                assert_eq!(engine.panes[&0].cwd.as_deref(), Some("C:\\kept�"));
            }
        }
        context
            .formats
            .insert(b"bad".to_vec(), b"C:\\kept\xff".to_vec());
        for args in [
            vec![b"new-session".as_slice(), b"-d", b"-c", b"#{bad}", b""],
            vec![b"attach-session", b"-t", b"kept", b"-c", b"#{bad}"],
            vec![b"respawn-pane", b"-k", b"-c", b"#{bad}"],
        ] {
            assert!(
                run(&mut engine, &mut context, &args)
                    .unwrap_err()
                    .contains("working directory")
            );
            assert_eq!(engine.snapshot(), original);
        }
    }

    #[test]
    fn malformed_option_and_environment_bytes_never_alias_literal_replacement_character() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        run(
            &mut engine,
            &mut context,
            &[b"set-option", b"-g", "@name�".as_bytes(), b"kept"],
        )
        .unwrap();
        run(
            &mut engine,
            &mut context,
            &[b"setenv", b"-g", "NAME�".as_bytes(), "value�".as_bytes()],
        )
        .unwrap();
        context.formats.insert(b"bad".to_vec(), b"\xff".to_vec());
        let options = engine.options.clone();
        let environment = engine.environment.clone();
        for args in [
            vec![b"set-option".as_slice(), b"-gu", b"@name\xff"],
            vec![b"set-option", b"-g", b"@new", b"\xff"],
            vec![b"set-option", b"-gF", b"@new", b"#{bad}"],
            vec![b"set-option", b"-gu", b"@name#{bad}"],
            vec![b"show-options", b"-g", b"@name\xff"],
            vec![b"setenv", b"-gF", "NAME�".as_bytes(), b"#{bad}"],
            vec![b"setenv", b"-g", b"NAME\xff", b"replacement"],
            vec![b"showenv", b"-g", b"NAME\xff"],
        ] {
            assert!(run(&mut engine, &mut context, &args).is_err(), "{args:?}");
            assert_eq!(engine.options, options, "{args:?}");
            assert_eq!(engine.environment, environment, "{args:?}");
        }
        assert_eq!(
            engine
                .expand(b"#{@name\xff}", None, None, None, &context)
                .unwrap(),
            b""
        );
        assert_eq!(
            engine
                .expand("#{@name�}".as_bytes(), None, None, None, &context)
                .unwrap(),
            b"kept"
        );
    }

    #[test]
    fn osc_path_formats_keep_raw_bytes_until_native_path_admission() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        run(&mut engine, &mut context, &[b"new-session", b"-d", b""]).unwrap();
        engine.feed(0, b"\x1b]7;file://host/C:/kept\xff\x07");
        let raw = engine.panes[&0].terminal.cwd().to_vec();
        assert!(raw.contains(&0xff));
        assert_eq!(
            engine
                .expand(b"#{pane_path}", Some(0), Some(0), Some(0), &context)
                .unwrap(),
            raw
        );
        assert_eq!(engine.panes[&0].terminal.format_value("pane_path"), None);
        let before = engine.snapshot();
        assert!(
            run(
                &mut engine,
                &mut context,
                &[b"respawn-pane", b"-k", b"-c", b"#{pane_path}"]
            )
            .unwrap_err()
            .contains("working directory")
        );
        assert_eq!(engine.snapshot(), before);
    }

    #[cfg(windows)]
    #[test]
    fn glob_resource_errors_preserve_environment_graph_and_client_state() {
        let mut engine = Engine::default();
        let mut context = ClientContext {
            control: true,
            cwd: Some("C:\\kept".into()),
            ..ClientContext::default()
        };
        run(
            &mut engine,
            &mut context,
            &[b"new-session", b"-d", b"-s", b"kept", b""],
        )
        .unwrap();
        engine
            .sessions
            .get_mut(&0)
            .unwrap()
            .environment
            .insert("MATCH".into(), "original".into());
        context.environment.insert("MATCH".into(), "changed".into());
        let pattern = b"?".repeat(65537);
        let update = [b"MATCH ".as_slice(), &pattern].concat();
        run(
            &mut engine,
            &mut context,
            &[b"set", b"-g", b"update-environment", &update],
        )
        .unwrap();
        let before = engine.snapshot();
        let env = engine.sessions[&0].environment.clone();
        for args in [
            vec![
                b"attach-session".as_slice(),
                b"-c",
                b"C:\\changed",
                b"-t",
                b"kept",
            ],
            vec![b"switch-client", b"-t", b"kept"],
            vec![b"new-session", b"-d", b"-s", b"new", b""],
            vec![b"new-session", b"-A", b"-s", b"kept"],
            vec![b"kill-session", b"-t", &pattern],
        ] {
            let flags = context.client_flags.clone();
            let selected = context.session;
            assert!(
                run(&mut engine, &mut context, &args)
                    .unwrap_err()
                    .contains("complexity limit"),
                "{args:?}"
            );
            assert_eq!(engine.snapshot(), before);
            assert_eq!(engine.sessions[&0].environment, env);
            assert_eq!(engine.sessions[&0].cwd.as_deref(), Some("C:\\kept"));
            assert_eq!(context.client_flags, flags);
            assert_eq!(context.session, selected);
        }
        context.client_flags = "read-only,ignore-size".into();
        assert!(
            run(
                &mut engine,
                &mut context,
                &[b"switch-client", b"-r", b"-t", b"kept"]
            )
            .unwrap_err()
            .contains("complexity limit")
        );
        assert_eq!(context.client_flags, "read-only,ignore-size");
        assert_eq!(engine.snapshot(), before);
    }

    #[test]
    fn pipe_and_buffer_format_names_never_alias_malformed_bytes() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        run(&mut engine, &mut context, &[b"new-session", b"-d", b""]).unwrap();
        run(
            &mut engine,
            &mut context,
            &[b"set-option", b"-g", "@name�".as_bytes(), b"exact"],
        )
        .unwrap();
        for (format, expected) in [
            (b"#{@name\xff}".as_slice(), b"".as_slice()),
            ("#{@name�}".as_bytes(), b"exact"),
        ] {
            let pipe = run(&mut engine, &mut context, &[b"pipe-pane", format]).unwrap();
            assert!(
                matches!(pipe.effects.as_slice(), [crate::Effect::Pipe { command: Some(command), .. }] if command == expected)
            );
            run(
                &mut engine,
                &mut context,
                &[b"set-buffer", b"-b", b"kept", b"data"],
            )
            .unwrap();
            let output = run(&mut engine, &mut context, &[b"list-buffers", b"-F", format])
                .unwrap()
                .output;
            assert_eq!(output, [expected, b"\n"].concat());
        }
    }
}
