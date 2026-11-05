//! Native output actions with private editor exports and no external launches.
use super::*;

fn copy_commands(host: &mut Host) -> Vec<String> {
    host.egui.egui_ctx().output_mut(|output| {
        std::mem::take(&mut output.commands)
            .into_iter()
            .map(|command| {
                let egui::OutputCommand::CopyText(text) = command else {
                    panic!("unexpected output action")
                };
                text
            })
            .collect()
    })
}

pub(super) fn check(app: &mut App, directory: &Path) -> Result<()> {
    let window = *app.windows.keys().next().ok_or("no output smoke window")?;
    let mut host = app.windows.remove(&window).unwrap();
    let pane = app.focused(host.id).ok_or("no output smoke pane")?;
    let saved = {
        let mut terminal = app.panes[&pane].session.terminal()?;
        std::mem::replace(&mut *terminal, vt::Terminal::new(20, 4, 100))
    };
    let result = (|| -> Result<()> {
        host.egui.egui_ctx().copy_text("clipboard sentinel".into());
        for action in [
            Action::CopyLastCommandOutput,
            Action::OpenLastCommandOutputInEditor,
            Action::OpenSelectionInEditor,
        ] {
            assert!(!app.output_action(&mut host, &action, |_| panic!(
                "missing text must not launch an editor"
            ))?);
        }
        assert_eq!(copy_commands(&mut host), ["clipboard sentinel"]);
        let expected = "wide 界 output: abcdefghijklmnopqrstuvwxyz\nsecond line";
        let (selection, viewport, generation) = {
            let mut terminal = app.panes[&pane].session.terminal()?;
            terminal.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07old\r\n\x1b]133;C\x07old result\r\n\x1b]133;D;0\x07");
            terminal.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07build\r\n\x1b]133;C\x07\x1b[31m");
            terminal.feed(expected.replace('\n', "\r\n").as_bytes());
            terminal.feed(b"\x1b[0m\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07not run");
            let screen = terminal.screen_mut();
            let row = screen.row(screen.cursor.row).id;
            screen.selection = Some(vt::Selection {
                start: vt::GridPoint { row, col: 2 },
                end: vt::GridPoint { row, col: 8 },
                rectangular: false,
            });
            screen.viewport_offset = screen.history_len().min(2);
            assert!(screen.viewport_offset > 0);
            (
                screen.selection,
                screen.viewport_offset,
                terminal.generation,
            )
        };
        assert!(
            app.output_action(&mut host, &Action::CopyLastCommandOutput, |_| panic!(
                "copy must not launch an editor"
            ))?
        );
        assert_eq!(copy_commands(&mut host), [expected]);
        let expected_all = format!("$ old\nold result\n$ build\n{expected}\n$ not run");
        for (action, expected) in [
            (Action::OpenLastCommandOutputInEditor, expected),
            (Action::OpenSelectionInEditor, "not run"),
            (Action::OpenScrollbackInEditor, expected_all.as_str()),
        ] {
            let mut export = None;
            let mut contents = None;
            let performed = app.output_action(&mut host, &action, |path| {
                export = Some(path.to_owned());
                contents = Some(fs::read(path).map_err(|error| error.to_string())?);
                // The opener can acquire terminal state because extraction released its lock.
                let terminal = app.panes[&pane]
                    .session
                    .terminal()
                    .map_err(|error| error.to_string())?;
                assert_eq!(terminal.screen().selection, selection);
                assert_eq!(terminal.screen().viewport_offset, viewport);
                Ok(())
            });
            if let Some(path) = export {
                fs::remove_file(&path)?;
                fs::remove_dir(path.parent().unwrap())?;
            }
            assert!(performed?);
            assert_eq!(contents.unwrap(), expected.as_bytes());
        }
        let mut failed_export = None;
        let failure = app.output_action(&mut host, &Action::OpenSelectionInEditor, |path| {
            failed_export = Some(path.to_owned());
            Err("editor unavailable".into())
        });
        assert!(failure.unwrap_err().contains("editor unavailable"));
        let failed_export = failed_export.unwrap();
        assert!(!failed_export.exists());
        assert!(!failed_export.parent().unwrap().exists());
        let terminal = app.panes[&pane].session.terminal()?;
        assert_eq!(terminal.screen().selection, selection);
        assert_eq!(terminal.screen().viewport_offset, viewport);
        assert_eq!(terminal.generation, generation);
        assert!(copy_commands(&mut host).is_empty());
        fs::write(
            directory.join("output-actions-result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true,
                "checks": ["missing-output-and-selection-leave-clipboard-unchanged", "copy-last-command-output", "editor-last-command-output", "editor-selection", "editor-screen-and-scrollback", "soft-wraps-joined", "styles-omitted", "selection-and-viewport-preserved", "terminal-lock-released-before-opener", "opener-errors-returned-and-cleaned"],
                "copied_text": expected,
                "external_launches": 0,
                "native_clipboard_writes": 0,
                "test_exports_remaining": 0
            }))?,
        )?;
        eprintln!(
            "Native output actions smoke: copy and all three private editor exports, missing-content behavior, exact plain text and unchanged terminal state passed; no native clipboard writes or editor launches"
        );
        Ok(())
    })();
    *app.panes[&pane].session.terminal()? = saved;
    app.windows.insert(window, host);
    result
}
