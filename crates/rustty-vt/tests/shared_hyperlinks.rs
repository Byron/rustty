use rustty_vt::{Cell, HyperlinkId, Screen, Terminal, snapshot};

#[test]
fn json_preserves_distinct_display_strings_for_equal_opaque_native_links() {
    let mut terminal = Terminal::new(4, 2, 1000);
    let mut json = serde_json::to_value(terminal.screen()).unwrap();
    for col in 0..2 {
        let cell = &mut json["rows"][0]["cells"][col];
        cell["text"] = serde_json::json!(if col == 0 { "a" } else { "b" });
        cell["hyperlink"] = serde_json::json!(format!("display-{col}"));
        cell["hyperlink_raw"] = serde_json::json!([255]);
        cell["hyperlink_id"] = serde_json::json!({"Explicit": [105, 100]});
    }
    let screen: Screen = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(&screen).unwrap()["rows"], json["rows"]);
    *terminal.screen_mut() = screen;
    let detached = terminal.screen().snapshot_viewport();
    terminal.resize(2, 2);
    assert_eq!(
        terminal.screen().row(0).hyperlink(0).unwrap().uri,
        "display-0"
    );
    assert_eq!(
        terminal.screen().row(0).hyperlink(1).unwrap().uri,
        "display-1"
    );
    terminal.feed(b"\x1b[HX");
    assert_eq!(
        terminal.screen().row(0).hyperlink(1).unwrap().uri,
        "display-1"
    );
    assert_eq!(
        serde_json::to_value(detached).unwrap()["rows"],
        json["rows"]
    );
}

#[test]
fn cells_share_links_through_wide_text_reflow_viewports_and_snapshots() {
    #[cfg(target_pointer_width = "64")]
    assert_eq!(size_of::<Cell>(), 8);

    let mut terminal = Terminal::new(80, 4, 1000);
    terminal.feed(b"\x1b]8;id=shared;https://example.org/\xff\x07");
    terminal.feed("a界b".as_bytes());
    let original = terminal.screen().row(0).hyperlink(0).unwrap();
    for col in 0..4 {
        assert!(std::ptr::eq(
            terminal.screen().row(0).hyperlink(col).unwrap(),
            original
        ));
    }
    let viewport = terminal.screen().snapshot_viewport();
    assert!(std::ptr::eq(
        viewport.row(0).hyperlink(0).unwrap(),
        original
    ));
    terminal.resize(40, 4);
    assert_eq!(
        terminal.screen().row(0).hyperlink(0),
        viewport.row(0).hyperlink(0)
    );
    let wire = snapshot::encode_to_vec(&terminal).unwrap();
    let restored = snapshot::decode(wire.as_slice(), Default::default()).unwrap();
    let row = restored.screen().row(0);
    let link = row.hyperlink(0).unwrap();
    assert_eq!(link.uri_bytes(), b"https://example.org/\xff");
    assert_eq!(link.id, Some(HyperlinkId::Explicit(b"shared".to_vec())));
    for col in 1..4 {
        assert!(std::ptr::eq(row.hyperlink(col).unwrap(), link));
    }
    drop(terminal);
    assert_eq!(
        viewport.row(0).hyperlink(0).unwrap().uri_bytes(),
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
        viewport.row(0).hyperlink(0).as_ref().unwrap().id,
        original_id
    );
    assert_eq!(
        terminal.screen().row(0).hyperlink(0).unwrap().id,
        original_id
    );
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
            decoded.cursor.hyperlink.as_deref()
        } else {
            decoded.row(0).hyperlink(0)
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
