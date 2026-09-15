use rustty_vt::{Cell, HyperlinkId, Screen, Terminal, snapshot};
use std::sync::Arc;

#[test]
fn cells_share_links_through_wide_text_reflow_viewports_and_snapshots() {
    #[cfg(target_pointer_width = "64")]
    assert_eq!(size_of::<Cell>(), 56);

    let mut terminal = Terminal::new(80, 4, 1000);
    terminal.feed(b"\x1b]8;id=shared;https://example.org/\xff\x07");
    terminal.feed("a界b".as_bytes());
    let shared = terminal.screen().cursor.hyperlink.clone().unwrap();
    for cell in &terminal.screen().rows[0].cells[..4] {
        assert!(Arc::ptr_eq(cell.hyperlink.as_ref().unwrap(), &shared));
    }
    let viewport = terminal.screen().snapshot_viewport();
    assert!(Arc::ptr_eq(
        viewport.rows[0].cells[0].hyperlink.as_ref().unwrap(),
        &shared
    ));
    terminal.resize(40, 4);
    assert!(Arc::ptr_eq(
        terminal.screen().rows[0].cells[0]
            .hyperlink
            .as_ref()
            .unwrap(),
        &shared
    ));

    let wire = snapshot::encode_to_vec(&terminal).unwrap();
    let restored = snapshot::decode(wire.as_slice(), Default::default()).unwrap();
    let cells = &restored.screen().rows[0].cells;
    let link = cells[0].hyperlink.as_ref().unwrap();
    assert_eq!(link.uri_bytes(), b"https://example.org/\xff");
    assert_eq!(link.id, Some(HyperlinkId::Explicit(b"shared".to_vec())));
    for cell in &cells[1..4] {
        assert!(Arc::ptr_eq(cell.hyperlink.as_ref().unwrap(), link));
    }
    drop(terminal);
    assert_eq!(
        viewport.rows[0].cells[0]
            .hyperlink
            .as_ref()
            .unwrap()
            .uri_bytes(),
        b"https://example.org/\xff"
    );
}

#[test]
fn renewing_the_cursor_link_preserves_published_cell_identities() {
    let mut terminal = Terminal::new(80, 4, 1000);
    terminal.feed(b"\x1b]8;;https://example.org\x07ab");
    let shared = terminal.screen().cursor.hyperlink.clone().unwrap();
    let original_id = shared.id.clone();
    let viewport = terminal.screen().snapshot_viewport();
    terminal.resize(40, 4);
    assert_ne!(
        terminal.screen().cursor.hyperlink.as_ref().unwrap().id,
        original_id
    );
    assert_eq!(shared.id, original_id);
    assert_eq!(
        viewport.rows[0].cells[0].hyperlink.as_ref().unwrap().id,
        original_id
    );
    assert!(Arc::ptr_eq(
        terminal.screen().rows[0].cells[0]
            .hyperlink
            .as_ref()
            .unwrap(),
        &shared
    ));
}

#[test]
fn shared_links_preserve_flat_cell_and_cursor_json() {
    for cursor in [false, true] {
        let terminal = Terminal::new(1, 1, 0);
        let mut screen = serde_json::to_value(terminal.screen()).unwrap();
        let value = if cursor {
            &mut screen["cursor"]
        } else {
            &mut screen["rows"][0]["cells"][0]
        };
        for field in ["hyperlink", "hyperlink_id", "hyperlink_raw"] {
            assert_eq!(value.get(field), Some(&serde_json::Value::Null));
        }
        value["hyperlink"] = serde_json::json!("https://example.org/�");
        value["hyperlink_id"] = serde_json::json!({"Explicit": [105, 100]});
        value["hyperlink_raw"] = serde_json::json!([255]);
        let expected = value.clone();
        let decoded: Screen = serde_json::from_value(screen).unwrap();
        let link = if cursor {
            &decoded.cursor.hyperlink
        } else {
            &decoded.rows[0].cells[0].hyperlink
        };
        assert_eq!(link.as_ref().unwrap().uri_bytes(), &[255]);
        let encoded = serde_json::to_value(decoded).unwrap();
        let actual = if cursor {
            &encoded["cursor"]
        } else {
            &encoded["rows"][0]["cells"][0]
        };
        assert_eq!(actual, &expected);
    }
}
