//! Compare real pane-cache replacements with the uncached renderer in a hidden window.
use super::*;

fn compare(host: &mut Host, panes: &[Id]) -> Result<()> {
    for id in panes {
        let prepared = host
            .prepared
            .get(id)
            .ok_or("row smoke pane was not prepared")?;
        let reference = host
            .fonts
            .prepare(&prepared.screen, &prepared.key.options)?;
        let actual = &prepared.frame;
        assert_eq!(actual.generation, reference.generation);
        assert_eq!(actual.size, reference.size);
        assert_eq!(
            actual.quads, reference.quads,
            "pane {id} cached geometry differs"
        );
        assert_eq!(actual.ime_cursor, reference.ime_cursor);
        assert_eq!(actual.cursor_row_bounds, reference.cursor_row_bounds);
        assert_eq!(actual.blinking_text, reference.blinking_text);
    }
    Ok(())
}

pub(super) fn check(app: &mut App, event_loop: &ActiveEventLoop, directory: &Path) -> Result<()> {
    let window = *app
        .windows
        .keys()
        .next()
        .ok_or("no row-cache smoke window")?;
    let mut host = app.windows.remove(&window).unwrap();
    let focused = host.focused;
    host.focused = true;
    let first = app.focused(host.id).ok_or("no row-cache smoke pane")?;
    app.action(
        event_loop,
        &mut host,
        Action::NewSplit(Direction::Right),
        true,
    );
    let second = app.focused(host.id).ok_or("split did not create a pane")?;
    assert_ne!(first, second);
    app.draw(event_loop, &mut host)?;
    let panes = [first, second];
    let mut saved = Vec::new();
    for (index, id) in panes.into_iter().enumerate() {
        let mut terminal = app.panes[&id].session.terminal()?;
        let mut fixture = vt::Terminal::new(terminal.cols, terminal.rows, 100);
        fixture.feed(b"\x1b[?25l");
        for row in 0..fixture.rows {
            fixture.feed(
                format!(
                    "\x1b[{};1H\x1b[{}mPane {} row {row}: office 水 é\x1b[0m",
                    row + 1,
                    31 + index,
                    index + 1
                )
                .as_bytes(),
            );
        }
        saved.push((id, std::mem::replace(&mut *terminal, fixture)));
    }
    let result = (|| -> Result<()> {
        let ids = panes.map(|id| {
            app.panes[&id]
                .session
                .terminal()
                .unwrap()
                .screen()
                .row(0)
                .id
        });
        assert_eq!(ids[0], ids[1], "fixture must overlap row IDs across panes");
        app.draw(event_loop, &mut host)?;
        compare(&mut host, &panes)?;
        let second_frame = Arc::clone(&host.prepared[&second].frame);
        let snapshot_id = host.prepared[&first].screen.render_identity();
        let prepares = host.pane_prepares;
        app.panes[&first]
            .session
            .terminal()?
            .feed(b"\x1b[2;1Hchanged");
        app.draw(event_loop, &mut host)?;
        assert_eq!(host.pane_prepares, prepares + 1);
        assert!(Arc::ptr_eq(&second_frame, &host.prepared[&second].frame));
        assert_ne!(snapshot_id, host.prepared[&first].screen.render_identity());
        compare(&mut host, &panes)?;
        capture_window(app, event_loop, &mut host, &directory.join("row-cache.png"))?;

        // Selection changes without output still rebuild the selected row correctly.
        {
            let mut terminal = app.panes[&first].session.terminal()?;
            let row = terminal.screen().row(2).id;
            terminal.screen_mut().selection = Some(vt::Selection {
                start: vt::GridPoint { row, col: 2 },
                end: vt::GridPoint { row, col: 8 },
                rectangular: false,
            });
        }
        app.draw(event_loop, &mut host)?;
        compare(&mut host, &panes)?;
        // Replacing a source can repeat all row IDs and terminal generations.
        {
            let mut terminal = app.panes[&first].session.terminal()?;
            *terminal = terminal.clone();
        }
        app.draw(event_loop, &mut host)?;
        compare(&mut host, &panes)?;

        app.panes[&second]
            .session
            .terminal()?
            .feed(b"\x1b[?25h\x1b[1 q\x1b[3;4H");
        host.cursor_blink_started = Instant::now();
        app.draw(event_loop, &mut host)?;
        let on_key = host.prepared[&second].key.clone();
        let on_frame = Arc::clone(&host.prepared[&second].frame);
        host.cursor_blink_started = Instant::now() - Duration::from_millis(750);
        app.draw(event_loop, &mut host)?;
        assert!(!host.prepared[&second].key.options.blink_visible);
        compare(&mut host, &panes)?;
        // Native cursor promotion restores the ON frame/key while the row cache
        // still describes the last full OFF preparation.
        let prepared = host.prepared.get_mut(&second).unwrap();
        prepared.key = on_key;
        prepared.frame = on_frame;
        host.cursor_blink_started = Instant::now() - Duration::from_millis(750);
        app.draw(event_loop, &mut host)?;
        compare(&mut host, &panes)?;
        host.cursor_blink_started = Instant::now();
        app.draw(event_loop, &mut host)?;
        compare(&mut host, &panes)?;

        host.composing = true;
        for text in ["input 日", "changed 世"] {
            host.preedit = text.into();
            host.preedit_selection = Some((0, 5));
            app.draw(event_loop, &mut host)?;
            assert!(host.prepared[&second].frame.ime_cursor.is_some());
            compare(&mut host, &panes)?;
        }
        host.composing = false;
        host.preedit.clear();
        host.preedit_selection = None;
        app.draw(event_loop, &mut host)?;
        compare(&mut host, &panes)?;
        // Reuses the established disappearing-pane check, including an atlas reset
        // after its live session was removed from the application.
        check_closed_split_animation(app, event_loop, &mut host, directory)?;
        compare(&mut host, &panes)?;
        fs::write(
            directory.join("row-cache-result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true,
                "panes": 2,
                "oracle": "uncached Renderer::prepare with the same atlas",
                "checks": ["overlapping-row-ids", "fresh-snapshot-cache-transfer", "single-row-output", "unchanged-sibling-frame", "selection", "source-replacement", "cursor-phases-and-native-promotion", "fresh-preedit", "outgoing-pane-without-session", "atlas-invalidation"],
                "capture": "row-cache.png"
            }))?,
        )?;
        eprintln!(
            "Native row-cache smoke: two panes, fresh snapshots, exact uncached geometry, selection, cursor phases, preedit and outgoing-pane atlas recovery passed"
        );
        Ok(())
    })();
    for (id, terminal) in saved {
        *app.panes[&id].session.terminal()? = terminal;
    }
    host.focused = focused;
    host.composing = false;
    host.preedit.clear();
    host.preedit_selection = None;
    app.windows.insert(window, host);
    result
}
