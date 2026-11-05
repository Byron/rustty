//! Extra cursors use ordinary pane frames, including when the main cursor is hidden.
use super::*;

const COLOR: [u8; 3] = [230, 120, 45];

fn rectangles(host: &Host, pane: Id) -> Vec<[f32; 4]> {
    host.prepared[&pane]
        .frame
        .quads
        .iter()
        .filter(|quad| {
            quad.paint == rustty_render::Paint::Solid
                && quad.color == rustty_render::Color::rgb(COLOR)
        })
        .map(|quad| quad.rect)
        .collect()
}

fn compare(host: &mut Host, panes: &[Id]) -> Result<()> {
    for pane in panes {
        let prepared = &host.prepared[pane];
        let reference = host
            .fonts
            .prepare(&prepared.screen, &prepared.key.options)?;
        assert_eq!(prepared.frame.quads, reference.quads);
        assert_eq!(
            prepared.frame.cursor_row_bounds,
            reference.cursor_row_bounds
        );
        assert_eq!(prepared.frame.ime_cursor, reference.ime_cursor);
    }
    Ok(())
}

fn blink_deadline(host: &Host) {
    assert_eq!(
        host.deadline,
        Some(cursor_blink_phase(host.cursor_blink_started, Instant::now()).1),
        "visible extra cursors must schedule their next blink even when the main cursor is hidden"
    );
}

pub(super) fn check(app: &mut App, event_loop: &ActiveEventLoop, directory: &Path) -> Result<()> {
    let window = *app
        .windows
        .keys()
        .next()
        .ok_or("no multiple-cursor smoke window")?;
    let mut host = app.windows.remove(&window).unwrap();
    let focused = host.focused;
    host.focused = true;
    let first = app
        .focused(host.id)
        .ok_or("no multiple-cursor smoke pane")?;
    app.action(
        event_loop,
        &mut host,
        Action::NewSplit(Direction::Right),
        true,
    );
    let active = app.focused(host.id).ok_or("multiple-cursor split failed")?;
    assert_ne!(first, active);
    app.draw(event_loop, &mut host)?;
    host.focus_hint.dismiss();
    let panes = [first, active];
    let mut saved = Vec::new();
    let mut texts = Vec::new();
    for (index, id) in panes.into_iter().enumerate() {
        let mut terminal = app.panes[&id].session.terminal()?;
        let mut fixture = vt::Terminal::new(terminal.cols, terminal.rows, 100);
        for row in 0..usize::from(fixture.rows) + 8 {
            fixture
                .feed(format!("Pane {} row {row:02}: alpha beta gamma\r\n", index + 1).as_bytes());
        }
        texts.push(
            fixture
                .screen()
                .rows()
                .map(|row| fixture.screen().row_text(row))
                .collect::<Vec<_>>(),
        );
        fixture.feed(b"\x1b[1 q\x1b[?25l\x1b[>40;2:230:120:45 q\x1b[>30;2:10:20:30 q\x1b[>1;2:3:5 q\x1b[>2;2:5:9 q\x1b[>3;2:7:13 q\x1b[>29;2:9:17 q");
        assert_eq!(fixture.screen().extra_cursors().count(), 4);
        saved.push((id, std::mem::replace(&mut *terminal, fixture)));
    }
    let result = (|| -> Result<()> {
        host.cursor_blink_started = Instant::now();
        app.draw(event_loop, &mut host)?;
        compare(&mut host, &panes)?;
        assert!(!host.prepared[&active].screen.cursor.visible);
        let on = rectangles(&host, active);
        assert!(!on.is_empty());
        assert!(!rectangles(&host, first).is_empty());
        blink_deadline(&host);
        capture_window(
            app,
            event_loop,
            &mut host,
            &directory.join("multiple-cursors.png"),
        )?;
        let inactive = Arc::clone(&host.prepared[&first].frame);
        host.cursor_blink_started = Instant::now() - Duration::from_millis(750);
        app.draw(event_loop, &mut host)?;
        assert!(rectangles(&host, active).is_empty());
        assert!(
            Arc::ptr_eq(&inactive, &host.prepared[&first].frame),
            "an unfocused pane's extra cursors stay steady"
        );
        compare(&mut host, &panes)?;
        blink_deadline(&host);

        host.composing = true;
        host.preedit = "input 日".into();
        host.preedit_selection = Some((0, 0));
        host.cursor_blink_started = Instant::now();
        app.draw(event_loop, &mut host)?;
        assert!(!host.prepared[&active].key.options.cursor_visible);
        assert!(host.prepared[&active].key.options.extra_cursor_visible);
        assert_eq!(rectangles(&host, active), on);
        assert!(host.prepared[&active].frame.ime_cursor.is_some());
        compare(&mut host, &panes)?;
        blink_deadline(&host);
        host.cursor_blink_started = Instant::now() - Duration::from_millis(750);
        app.draw(event_loop, &mut host)?;
        assert!(rectangles(&host, active).is_empty());
        blink_deadline(&host);
        host.composing = false;
        host.preedit.clear();
        host.preedit_selection = None;

        app.panes[&active]
            .session
            .terminal()?
            .screen_mut()
            .scroll_viewport(3);
        host.cursor_blink_started = Instant::now();
        app.draw(event_loop, &mut host)?;
        assert_eq!(
            rectangles(&host, active),
            on,
            "extra cursors are viewport-fixed"
        );
        compare(&mut host, &panes)?;
        app.panes[&active]
            .session
            .terminal()?
            .screen_mut()
            .scroll_viewport(-3);
        app.draw(event_loop, &mut host)?;
        let retained = Arc::clone(&host.prepared[&active].frame);
        app.panes[&active]
            .session
            .terminal()?
            .feed(b"\x1b[?2026h\x1b[>0;4 q\x1b[>1;2:12:20 q");
        host.cursor_blink_started = Instant::now() - Duration::from_millis(750);
        app.draw(event_loop, &mut host)?;
        assert!(Arc::ptr_eq(&retained, &host.prepared[&active].frame));
        assert_eq!(rectangles(&host, active), on);
        assert!(
            host.deadline.is_none(),
            "synchronized output must freeze cursor blinking"
        );
        app.panes[&active].session.terminal()?.feed(b"\x1b[?2026l");
        host.cursor_blink_started = Instant::now();
        app.draw(event_loop, &mut host)?;
        assert_ne!(rectangles(&host, active), on);
        assert!(!rectangles(&host, active).is_empty());
        compare(&mut host, &panes)?;
        capture_window(
            app,
            event_loop,
            &mut host,
            &directory.join("multiple-cursors-updated.png"),
        )?;

        app.panes[&active]
            .session
            .terminal()?
            .feed(b"\x1b[1;1H\x1b[?25h");
        app.draw(event_loop, &mut host)?;
        #[cfg(target_os = "macos")]
        assert!(
            host.cursor_candidate(active).is_none(),
            "extra cursors disable the single-cursor native patch"
        );
        app.panes[&active].session.terminal()?.feed(b"\x1b[>0;4 q");
        app.draw(event_loop, &mut host)?;
        assert!(!host.prepared[&active].screen.has_extra_cursors());
        #[cfg(target_os = "macos")]
        assert!(
            host.cursor_candidate(active).is_some(),
            "clearing extras restores ordinary cursor eligibility"
        );
        app.panes[&active]
            .session
            .terminal()?
            .feed(b"\x1b[>1;2:12:20 q");
        app.panes.get_mut(&active).unwrap().exited = true;
        app.draw(event_loop, &mut host)?;
        assert!(!host.prepared[&active].key.options.extra_cursor_visible);
        assert!(rectangles(&host, active).is_empty());
        assert!(
            host.deadline.is_none(),
            "an exited pane must not keep a cursor timer running"
        );
        app.panes.get_mut(&active).unwrap().exited = false;
        for (index, id) in panes.into_iter().enumerate() {
            let mut terminal = app.panes[&id].session.terminal()?;
            assert_eq!(
                terminal
                    .screen()
                    .rows()
                    .map(|row| terminal.screen().row_text(row))
                    .collect::<Vec<_>>(),
                texts[index]
            );
            let row = terminal.screen().row(0).id;
            let last = texts[index][0].len().saturating_sub(1);
            terminal.screen_mut().selection = Some(vt::Selection {
                start: vt::GridPoint { row, col: 0 },
                end: vt::GridPoint { row, col: last },
                rectangular: false,
            });
            assert_eq!(
                terminal.screen().selection_text().as_deref(),
                Some(texts[index][0].as_str())
            );
        }
        fs::write(
            directory.join("multiple-cursors-result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true, "panes": 2,
                "checks": ["hidden-main-cursor", "focused-blink-deadline", "unfocused-static", "composition-preserves-extras", "viewport-fixed", "synchronized-output", "native-patch-ineligible-with-extras", "clear-restores-eligibility", "exited-pane-suppression", "text-and-selection-copy-unchanged", "cached-uncached-oracle"],
                "captures": ["multiple-cursors.png", "multiple-cursors-updated.png"]
            }))?,
        )?;
        eprintln!(
            "Native multiple-cursor smoke: hidden main cursor, blink, focus, preedit, viewport, sync and cache checks passed"
        );
        Ok(())
    })();
    for (id, terminal) in saved {
        *app.panes[&id].session.terminal()? = terminal;
    }
    app.panes.get_mut(&active).unwrap().exited = false;
    host.composing = false;
    host.preedit.clear();
    host.preedit_selection = None;
    host.focused = focused;
    app.windows.insert(window, host);
    result
}
