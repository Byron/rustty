//! Hidden native hint checks; clipboard commands are intercepted before native delivery.
use super::*;
use winit::keyboard::{Key, KeyCode, NamedKey};

fn key(app: &mut App, host: &mut Host, logical: Key) {
    let physical = PhysicalKey::Code(KeyCode::KeyA);
    assert!(app.hint_key(host, &logical, physical, ElementState::Pressed, false));
    assert!(input::global_key_is_consumed(
        &mut app.consumed_global_keys,
        physical,
        ElementState::Released,
        false
    ));
}
fn copied(host: &mut Host) -> Vec<String> {
    host.egui.egui_ctx().output_mut(|output| {
        std::mem::take(&mut output.commands)
            .into_iter()
            .map(|command| {
                let egui::OutputCommand::CopyText(text) = command else {
                    panic!("unexpected hint output")
                };
                text
            })
            .collect()
    })
}

fn ime_frame(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    host: &mut Host,
    directory: &Path,
) -> Result<()> {
    // Exercise egui-winit's native IME cache while rendering into a hidden offscreen target.
    let background = std::mem::replace(&mut app.background_smoke, false);
    let result = capture_window(
        app,
        event_loop,
        host,
        &directory.join("keyboard-hints-ime-restored.png"),
    );
    app.background_smoke = background;
    result?;
    assert!(!host.window.has_focus());
    assert_ne!(host.window.is_visible(), Some(true));
    Ok(())
}

pub(super) fn check(app: &mut App, event_loop: &ActiveEventLoop, directory: &Path) -> Result<()> {
    let window = *app.windows.keys().next().ok_or("no hint smoke window")?;
    let mut host = app.windows.remove(&window).unwrap();
    let focused = host.focused;
    host.focused = true;
    host.egui.egui_input_mut().focused = true;
    let pane = app.focused(host.id).ok_or("no hint smoke pane")?;
    app.draw(event_loop, &mut host)?;
    let state = app.panes.get_mut(&pane).unwrap();
    let (saved, selection) = {
        let mut terminal = state.session.terminal()?;
        let mut fixture = vt::Terminal::new(terminal.cols, terminal.rows, 100);
        fixture.feed(b"\x1b[?1003h\x1b[>3u");
        for index in 0..6 {
            fixture.feed(
                format!(
                    "src/main.rs:{}:3  https://example.com/{index}\r\n",
                    index + 10
                )
                .as_bytes(),
            );
        }
        fixture.feed(b"\x1b]8;;https://example.com/actual\x07documentation\x1b]8;;\x07");
        fixture.feed(format!("\x1b[{};1Hhttps://example.com/bottom", fixture.rows).as_bytes());
        let row = fixture.screen().row(0).id;
        let selection = Some(vt::Selection {
            start: vt::GridPoint { row, col: 0 },
            end: vt::GridPoint { row, col: 2 },
            rectangular: false,
        });
        fixture.screen_mut().selection = selection;
        (std::mem::replace(&mut *terminal, fixture), selection)
    };
    let saved_input = std::mem::take(&mut state.input);
    let saved_bytes = std::mem::take(&mut state.input_bytes);
    state.input.push_back(SessionInput::Text(Vec::new()));
    let result = (|| -> Result<()> {
        app.draw(event_loop, &mut host)?;
        ime_frame(app, event_loop, &mut host, directory)?;
        assert!(host.egui.allow_ime());
        assert!(app.show_link_hints(&mut host, false));
        assert!(!host.egui.allow_ime());
        key(app, &mut host, Key::Named(NamedKey::Escape));
        ime_frame(app, event_loop, &mut host, directory)?;
        assert!(
            host.egui.allow_ime(),
            "immediate cancel must restore IME on the next terminal frame"
        );
        assert!(app.action(event_loop, &mut host, Action::CopyLinkHints, false));
        assert!(host.hints.as_ref().unwrap().entries.len() >= 14);
        assert!(
            host.hints
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .all(|entry| entry.label.len() == 2)
        );
        assert!(app.hints_valid(&host));
        let mouse = host.mouse;
        host.mouse = host.rects[&pane].center();
        app.mouse(&mut host, vt::MouseAction::Move, None);
        host.mouse = mouse;
        capture_window(
            app,
            event_loop,
            &mut host,
            &directory.join("keyboard-hints.png"),
        )?;
        ime_frame(app, event_loop, &mut host, directory)?;
        assert!(!host.egui.allow_ime(), "hint frames must not re-enable IME");
        assert!(host.hints.is_some(), "ordinary drawing must retain hints");
        assert!(app.hint_ime(&mut host, &Ime::Preedit("あ".into(), Some((0, 3)))));
        assert!(app.hint_ime(&mut host, &Ime::Commit("あ".into())));
        let entry = host
            .hints
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .find(|entry| entry.target.link.uri == "src/main.rs:10:3")
            .unwrap();
        assert_eq!(entry.target.open_target, "src/main.rs");
        let label = entry.label.clone();
        key(app, &mut host, Key::Character(label[..1].into()));
        assert!(host.hints.is_some());
        key(app, &mut host, Key::Named(NamedKey::Backspace));
        for character in label.chars() {
            key(app, &mut host, Key::Character(character.to_string().into()));
        }
        assert!(host.hints.is_none());
        assert_eq!(copied(&mut host), ["src/main.rs:10:3"]);
        ime_frame(app, event_loop, &mut host, directory)?;
        assert!(host.egui.allow_ime(), "ordinary dismissal must restore IME");
        // A late native commit and the physical label release cannot reach the PTY.
        assert!(app.hint_ime(&mut host, &Ime::Commit("late".into())));
        assert!(!app.hint_ime(&mut host, &Ime::Enabled));
        assert_eq!(
            app.panes[&pane].session.terminal()?.screen().selection,
            selection
        );
        assert!(app.action(event_loop, &mut host, Action::ShowLinkHints, false));
        key(app, &mut host, Key::Named(NamedKey::Escape));
        assert!(host.hints.is_none());
        assert!(copied(&mut host).is_empty());

        assert!(app.search_action(&mut host, pane, Action::StartSearch));
        app.panes
            .get_mut(&pane)
            .unwrap()
            .search
            .as_mut()
            .unwrap()
            .query = "main".into();
        app.draw(event_loop, &mut host)?;
        let find_selection = app.panes[&pane].session.terminal()?.screen().selection;
        assert_eq!(host.search_focus, Some(pane));
        assert!(app.show_link_hints(&mut host, true));
        assert!(host.search_focus.is_none());
        assert_eq!(app.panes[&pane].search.as_ref().unwrap().query, "main");
        key(app, &mut host, Key::Named(NamedKey::Escape));
        assert_eq!(
            app.panes[&pane].session.terminal()?.screen().selection,
            find_selection
        );
        assert!(app.search_action(&mut host, pane, Action::EndSearch));
        app.panes[&pane].session.terminal()?.screen_mut().selection = selection;
        app.draw(event_loop, &mut host)?;

        // Cursor options and selection are not target changes.
        assert!(app.show_link_hints(&mut host, true));
        host.prepared
            .get_mut(&pane)
            .unwrap()
            .key
            .options
            .blink_visible ^= true;
        assert!(app.hints_valid(&host));
        let generation = app.panes[&pane].session.terminal()?.generation;
        app.panes[&pane]
            .session
            .terminal()?
            .screen_mut()
            .set_cell_text(0, 0, "x");
        assert_eq!(app.panes[&pane].session.terminal()?.generation, generation);
        key(app, &mut host, Key::Character("a".into()));
        assert!(
            host.hints.is_none(),
            "in-place mutation before drawing must cancel"
        );
        assert!(copied(&mut host).is_empty());
        app.draw(event_loop, &mut host)?;
        assert!(
            !app.show_link_hints(&mut host, true),
            "undisplayed in-place content must stay unavailable"
        );
        app.panes[&pane].session.terminal()?.feed(b"\x1b[1;1H");
        app.draw(event_loop, &mut host)?;
        assert!(app.show_link_hints(&mut host, true));
        host.focused = false;
        key(app, &mut host, Key::Character("a".into()));
        assert!(host.hints.is_none());
        host.focused = true;
        assert!(app.show_link_hints(&mut host, true));
        let rect = host.rects[&pane];
        host.rects.get_mut(&pane).unwrap().min.x += 1.0;
        key(app, &mut host, Key::Character("a".into()));
        assert!(host.hints.is_none());
        host.rects.insert(pane, rect);
        assert!(app.show_link_hints(&mut host, true));
        {
            let mut terminal = app.panes[&pane].session.terminal()?;
            *terminal = terminal.clone();
        }
        key(app, &mut host, Key::Character("a".into()));
        assert!(
            host.hints.is_none(),
            "replacement with the same generation must cancel"
        );
        app.draw(event_loop, &mut host)?;
        app.panes[&pane].session.terminal()?.feed(b"\x1b[?2026h");
        assert!(!app.show_link_hints(&mut host, true));
        app.panes[&pane].session.terminal()?.feed(b"\x1b[?2026l");
        assert_eq!(queued_input(&app.panes[&pane])?, [Vec::<u8>::new()]);
        assert_eq!(app.panes[&pane].input_bytes, 0);
        assert_eq!(
            app.panes[&pane].session.terminal()?.screen().selection,
            selection
        );
        assert!(copied(&mut host).is_empty());
        fs::write(
            directory.join("keyboard-hints-result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true,
            "checks": ["visible-labels-and-bottom-row", "fixed-width-prefix", "backspace", "escape", "copy-diagnostic-location", "selection-preserved", "find-focus-handoff", "ime-and-release-consumed", "ime-restored-after-immediate-and-drawn-dismissal", "no-terminal-input", "redraw-preserved", "in-place-mutation-cancel", "focus-cancel", "geometry-cancel", "source-replacement-cancel", "synchronized-output-rejected"],
                "clipboard": "CopyText intercepted before native delivery",
                "external_launches": 0,
                "capture": "keyboard-hints.png"
            }))?,
        )?;
        eprintln!(
            "Native keyboard hints smoke: overlay, prefixes, correction, copy, selection, input isolation and stale-target cancellation passed; no native clipboard writes or external launches"
        );
        Ok(())
    })();
    let state = app.panes.get_mut(&pane).unwrap();
    *state.session.terminal()? = saved;
    state.input = saved_input;
    state.input_bytes = saved_bytes;
    host.hints = None;
    host.hints_ime = false;
    host.focused = focused;
    app.windows.insert(window, host);
    result
}
