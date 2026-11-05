use super::*;

fn filled() -> Terminal {
    let mut terminal = Terminal::new(80, 4, 5000);
    terminal.feed(b"\x1b_Ga=T,i=7,f=32,s=1,v=1;/////w==\x1b\\");
    for _ in 0..2000 {
        terminal.feed(b"\x1b[1;32mBuild\x1b[m\t\x1b]8;id=test;https://example.org\x07wide \xe7\x95\x8c e\xcc\x81\x1b]8;;\x07\r\n");
    }
    terminal.feed(b"\x1b_Ga=T,i=8,f=32,s=1,v=1;/////w==\x1b\\\r\n");
    terminal
}

fn finish(terminal: &mut Terminal) {
    for _ in 0..1000 {
        if !terminal.compress_scrollback_step() {
            assert!(!terminal.scrollback_compression_pending());
            return;
        }
    }
    panic!("idle compression did not finish");
}

#[test]
fn owner_history_lifecycle_rearms_only_real_changes() {
    let mut terminal = filled();
    assert!(terminal.scrollback_compression_pending());
    let resident = terminal.inner.primary_screen().owned_bytes();
    let point = terminal.inner.primary_screen().point(2, 0).unwrap();
    terminal.inner.screen_mut().selection = Some(rustty_vt::Selection {
        start: point,
        end: rustty_vt::GridPoint { col: 3, ..point },
        rectangular: false,
    });
    let before = terminal.checkpoint_snapshot();
    let generation = terminal.inner.generation;
    finish(&mut terminal);
    let cold = terminal.inner.primary_screen().owned_bytes();
    assert!(cold < resident);
    assert_eq!(terminal.inner.generation, generation);
    assert_eq!(terminal.inner.screen().selection.unwrap().start, point);
    assert!(terminal.inner.screen().contains_point(point));
    assert_eq!(
        serde_json::to_value(terminal.checkpoint_snapshot()).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
    terminal.request_scrollback_compression();
    finish(&mut terminal);
    for _ in 0..3 {
        terminal.format_value("history_bytes").unwrap();
        terminal
            .active_screen()
            .row_text(terminal.active_screen().row(0));
        terminal.resize(80, 4);
        terminal.set_history_limit(5000);
        assert!(!terminal.scrollback_compression_pending());
        assert_eq!(terminal.inner.primary_screen().owned_bytes(), cold);
    }
    let restored = before.restore().unwrap();
    assert!(!restored.inner.scrollback_compression());
    assert!(!restored.scrollback_compression_pending());
    assert_eq!(restored.capture(), terminal.capture());
    terminal.request_scrollback_compression();
    finish(&mut terminal);
    assert_eq!(terminal.inner.primary_screen().owned_bytes(), cold);

    terminal.resize(60, 4);
    assert!(terminal.scrollback_compression_pending());
    finish(&mut terminal);
    terminal.set_history_limit(1000);
    assert!(terminal.scrollback_compression_pending());
    assert!(terminal.history_len() <= 1000);
    finish(&mut terminal);
    terminal.feed(b"\x1b[1;1Hactive");
    finish(&mut terminal);
    terminal.set_history_limit(5000);
    let old_history = terminal.history_len();
    terminal.reset_from_host();
    assert!(terminal.scrollback_compression_pending());
    assert!(terminal.history_len() >= old_history);
    finish(&mut terminal);
    terminal.trim_history_below_cursor();
    assert!(terminal.scrollback_compression_pending());
    finish(&mut terminal);
    terminal.feed(b"more\r\n");
    assert!(terminal.scrollback_compression_pending());
}

#[test]
fn owner_capture_reclaims_shared_reads_even_after_allocation_metadata_error() {
    let mut terminal = filled();
    // Imported history can lack native allocation metadata. Its capture error
    // occurs after the shared row traversal has restored compressed cells.
    let mut metadata =
        serde_json::to_value(terminal.inner.primary_screen().view_metadata()).unwrap();
    metadata["tmux_capture_known"] = false.into();
    terminal
        .inner
        .restore_view_metadata(&serde_json::from_value(metadata).unwrap(), None)
        .unwrap();
    finish(&mut terminal);
    let cold = terminal.inner.primary_screen().owned_bytes();
    assert!(
        terminal
            .capture_for_owner(CaptureOptions {
                start: Some(i32::MIN),
                preserve_trailing: true,
                ..Default::default()
            })
            .unwrap_err()
            .contains("allocation metadata")
    );
    assert!(terminal.scrollback_compression_pending());
    assert!(terminal.inner.primary_screen().owned_bytes() > cold);
    finish(&mut terminal);
    assert_eq!(terminal.inner.primary_screen().owned_bytes(), cold);
    let captured = terminal
        .capture_for_owner(CaptureOptions {
            start: Some(i32::MIN),
            ..Default::default()
        })
        .unwrap();
    assert!(captured.windows(5).any(|bytes| bytes == b"Build"));
    assert!(terminal.scrollback_compression_pending());
    finish(&mut terminal);
    assert_eq!(terminal.inner.primary_screen().owned_bytes(), cold);
    terminal
        .capture_for_owner(CaptureOptions {
            pending: true,
            ..Default::default()
        })
        .unwrap();
    assert!(!terminal.scrollback_compression_pending());
}
