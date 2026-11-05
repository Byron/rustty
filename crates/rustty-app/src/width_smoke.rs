//! Exercise explicit terminal widths through native presentation and input.
use super::*;
use winit::keyboard::{Key, KeyCode, ModifiersState};

fn copied(host: &mut Host) -> Vec<String> {
    host.egui.egui_ctx().output_mut(|output| {
        std::mem::take(&mut output.commands)
            .into_iter()
            .map(|command| {
                let egui::OutputCommand::CopyText(text) = command else {
                    panic!("unexpected width smoke output")
                };
                text
            })
            .collect()
    })
}

fn oracle(host: &mut Host, pane: Id) -> Result<()> {
    let prepared = &host.prepared[&pane];
    let reference = host
        .fonts
        .prepare(&prepared.screen, &prepared.key.options)?;
    assert_eq!(prepared.frame.quads, reference.quads);
    assert_eq!(
        prepared.frame.cursor_row_bounds,
        reference.cursor_row_bounds
    );
    assert_eq!(prepared.frame.ime_cursor, reference.ime_cursor);
    Ok(())
}

pub(super) fn check(app: &mut App, event_loop: &ActiveEventLoop, directory: &Path) -> Result<()> {
    let window = *app.windows.keys().next().ok_or("no width smoke window")?;
    let mut host = app.windows.remove(&window).unwrap();
    let focused = host.focused;
    host.focused = true;
    let pane = app.focused(host.id).ok_or("no width smoke pane")?;
    app.draw(event_loop, &mut host)?;
    let state = app.panes.get_mut(&pane).unwrap();
    let saved = {
        let mut terminal = state.session.terminal()?;
        let mut fixture = vt::Terminal::new(terminal.cols, terminal.rows, 100);
        fixture.feed(b"\x1b[?25lExplicit width: text occupies the requested columns");
        for (index, text) in ["🙂", "é", "猫🙂", "ffi", "hello", "👩🏽‍💻", "wide text"]
            .into_iter()
            .enumerate()
        {
            let width = index + 1;
            fixture.feed(
                format!(
                    "\x1b[{};1Hw={width}  |\x1b]66;w={width};{text}\x07|after",
                    index + 3
                )
                .as_bytes(),
            );
            assert_eq!(fixture.screen().cursor.col, 6 + width + 6);
            assert_eq!(fixture.screen().row(index + 2).text(6).as_str(), text);
            assert_eq!(
                fixture.screen().row(index + 2).cell_span(6).width,
                width as u8
            );
        }
        fixture.feed(b"\x1b[11;1HLink: \x1b]8;;https://example.com/width\x07\x1b]66;w=7;open me\x07\x1b]8;;\x07 tail");
        std::mem::replace(&mut *terminal, fixture)
    };
    let saved_search = state.search.take();
    let saved_input = std::mem::take(&mut state.input);
    let saved_bytes = std::mem::take(&mut state.input_bytes);
    state.input.push_back(SessionInput::Text(Vec::new()));
    let result = (|| -> Result<()> {
        app.draw(event_loop, &mut host)?;
        oracle(&mut host, pane)?;
        let (rect, cell) = app.hint_geometry(&host, pane).unwrap();
        let link_bounds = egui::Rect::from_min_size(
            rect.min + Vec2::new(6.0 * cell.x, 10.0 * cell.y),
            Vec2::new(7.0 * cell.x, cell.y),
        );
        host.modifiers = if cfg!(target_os = "windows") {
            ModifiersState::CONTROL
        } else {
            ModifiersState::SUPER
        }
        .into();
        for col in 6..13 {
            host.mouse = rect.min + Vec2::new((col as f32 + 0.5) * cell.x, 10.5 * cell.y);
            let _ = host.egui.on_window_event(
                &host.window,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: LogicalPosition::new(host.mouse.x, host.mouse.y)
                        .to_physical(host.window.scale_factor()),
                },
            );
            app.update_hover_link(&mut host);
            let link = host.hovered_link.as_ref().unwrap();
            assert_eq!(link.uri, "https://example.com/width");
            assert_eq!(link.bounds, [link_bounds]);
        }
        host.modifiers = ModifiersState::empty().into();
        app.update_hover_link(&mut host);
        // Both the first cell and the last narrow fallback tail select the same text.
        for col in [6, 12] {
            {
                let state = app.panes.get_mut(&pane).unwrap();
                state
                    .selection_gesture
                    .reset(&mut *state.session.terminal()?);
            }
            host.mouse = rect.min + Vec2::new((col as f32 + 0.5) * cell.x, 10.5 * cell.y);
            for count in 1..=2 {
                host.mouse_button = Some(vt::MouseButton::Left);
                app.mouse(
                    &mut host,
                    vt::MouseAction::Press,
                    Some(vt::MouseButton::Left),
                );
                app.mouse(
                    &mut host,
                    vt::MouseAction::Release,
                    Some(vt::MouseButton::Left),
                );
                host.mouse_button = None;
                assert_eq!(
                    app.panes[&pane]
                        .session
                        .terminal()?
                        .screen()
                        .selection_text()
                        .as_deref(),
                    (count == 2).then_some("open me")
                );
            }
        }
        app.draw(event_loop, &mut host)?;
        assert!(app.show_link_hints(&mut host, true));
        let hints = host.hints.as_ref().unwrap();
        assert_eq!(hints.entries.len(), 1);
        assert_eq!(hints.entries[0].bounds, [link_bounds]);
        let label = hints.entries[0].label.clone();
        capture_window(
            app,
            event_loop,
            &mut host,
            &directory.join("explicit-width-hints.png"),
        )?;
        let physical = PhysicalKey::Code(KeyCode::KeyA);
        assert!(app.hint_key(
            &mut host,
            &Key::Character(label.into()),
            physical,
            ElementState::Pressed,
            false
        ));
        assert_eq!(copied(&mut host), ["https://example.com/width"]);
        assert!(input::global_key_is_consumed(
            &mut app.consumed_global_keys,
            physical,
            ElementState::Released,
            false
        ));

        let mut search = Search::default();
        search.query = "open".into();
        search.refresh(&mut *app.panes[&pane].session.terminal()?);
        assert_eq!(
            app.panes[&pane]
                .session
                .terminal()?
                .screen()
                .selection_text()
                .as_deref(),
            Some("open me")
        );
        app.panes.get_mut(&pane).unwrap().search = Some(search);
        app.draw(event_loop, &mut host)?;
        oracle(&mut host, pane)?;
        let prepared = host.prepared.get_mut(&pane).unwrap();
        let text = prepared.accessibility(pane);
        let mut update = egui::accesskit::TreeUpdate {
            nodes: vec![(
                text.id.accesskit_id(),
                egui::accesskit::Node::new(egui::accesskit::Role::Terminal),
            )],
            tree: None,
            tree_id: egui::accesskit::TreeId::ROOT,
            focus: text.id.accesskit_id(),
        };
        text.append_to(&mut update);
        let row = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.value()
                    .is_some_and(|value| value.starts_with("Link: "))
            })
            .unwrap();
        assert_eq!(row.1.character_lengths()[6], 7);
        assert_eq!(row.1.character_widths().unwrap()[6], 7.0 * cell.x);
        app.panes
            .get_mut(&pane)
            .unwrap()
            .search
            .take()
            .unwrap()
            .clear_highlight(&mut *app.panes[&pane].session.terminal()?);

        // A cursor within the span still has its actual physical column for IME.
        app.panes[&pane]
            .session
            .terminal()?
            .feed(b"\x1b[11;10H\x1b[1 q\x1b[?25h");
        host.cursor_blink_started = Instant::now();
        app.draw(event_loop, &mut host)?;
        let expected_x = rect.left() + 9.0 * cell.x;
        assert_eq!(host.prepared[&pane].ime_rect.left(), expected_x);
        oracle(&mut host, pane)?;
        host.cursor_blink_started = Instant::now() - Duration::from_millis(750);
        app.draw(event_loop, &mut host)?;
        oracle(&mut host, pane)?;
        host.composing = true;
        host.preedit = "input 日".into();
        host.preedit_selection = Some((0, 0));
        app.draw(event_loop, &mut host)?;
        assert!(host.prepared[&pane].frame.ime_cursor.is_some());
        oracle(&mut host, pane)?;
        host.composing = false;
        host.preedit.clear();
        host.preedit_selection = None;
        host.cursor_blink_started = Instant::now();
        capture_window(
            app,
            event_loop,
            &mut host,
            &directory.join("explicit-width.png"),
        )?;

        assert!(app.show_link_hints(&mut host, true));
        app.panes[&pane]
            .session
            .terminal()?
            .feed(b"\x1b[11;7H\x1b]66;w=3;open me\x07");
        assert!(!app.hints_valid(&host));
        assert!(app.hint_key(
            &mut host,
            &Key::Character("a".into()),
            physical,
            ElementState::Pressed,
            false
        ));
        assert!(copied(&mut host).is_empty());
        assert!(input::global_key_is_consumed(
            &mut app.consumed_global_keys,
            physical,
            ElementState::Released,
            false
        ));
        assert_eq!(queued_input(&app.panes[&pane])?, [Vec::<u8>::new()]);
        fs::write(
            directory.join("explicit-width-result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true,
                "checks": ["widths-1-through-7", "full-logical-text", "continuation-hover-and-double-click", "span-sized-hints", "private-copy", "search-selection", "accessible-span-width", "physical-ime-anchor", "cursor-blink-phases", "preedit", "cached-uncached-oracle", "stale-width-cancel", "no-terminal-input"],
                "captures": ["explicit-width.png", "explicit-width-hints.png"]
            }))?,
        )?;
        eprintln!(
            "Native explicit-width smoke: widths1–7, links, selection, accessibility, cursor, IME and cache checks passed"
        );
        Ok(())
    })();
    let state = app.panes.get_mut(&pane).unwrap();
    *state.session.terminal()? = saved;
    state.search = saved_search;
    state.input = saved_input;
    state.input_bytes = saved_bytes;
    host.hints = None;
    host.hints_ime = false;
    host.hovered_link = None;
    host.link_hit = None;
    host.selection_drag = None;
    host.composing = false;
    host.preedit.clear();
    host.preedit_selection = None;
    host.focused = focused;
    app.windows.insert(window, host);
    result
}
