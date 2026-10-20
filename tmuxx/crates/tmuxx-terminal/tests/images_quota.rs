use tmuxx_terminal::{Effect, ImageQuota, Terminal};

fn image(action: char, id: u32) -> Vec<u8> {
    format!("\x1b_Ga={action},i={id},f=32,s=1,v=1,C=1;AAAA/w==\x1b\\").into_bytes()
}
fn ids(terminal: &mut Terminal) -> Vec<u32> {
    terminal.prune_images();
    terminal
        .snapshot()
        .primary_graphics
        .images
        .iter()
        .map(|image| image.id)
        .collect()
}
fn reply(effects: Vec<Effect>) -> Vec<u8> {
    effects
        .into_iter()
        .flat_map(|effect| {
            if let Effect::Write(bytes) = effect {
                bytes
            } else {
                Vec::new()
            }
        })
        .collect()
}
fn terminals(count: usize, bytes: usize) -> (Terminal, Terminal, ImageQuota) {
    let quota = ImageQuota::default();
    quota.set_limits(count, bytes);
    let mut a = Terminal::new(80, 24, 100);
    let mut b = a.clone();
    a.set_image_quota(quota.clone());
    b.set_image_quota(quota.clone());
    (a, b, quota)
}
#[test]
fn query_admission_and_rejected_placements_keep_real_evictions() {
    let (mut a, mut b, quota) = terminals(2, 8);
    a.feed(&image('t', 1));
    b.feed(&image('t', 2));
    assert_eq!(reply(b.feed(&image('q', 3))), b"\x1b_Gi=3;OK\x1b\\");
    assert!(ids(&mut a).is_empty());
    assert_eq!(ids(&mut b), [2]);
    quota.set_limits(1, 8);
    assert_eq!(
        reply(b.feed(b"\x1b_Ga=p,i=2;\x1b\\")),
        b"\x1b_Gi=2;EINVAL\x1b\\"
    );
    assert_eq!(ids(&mut b), [2]);
    assert_eq!(reply(b.feed(&image('T', 4))), b"\x1b_Gi=4;EINVAL\x1b\\");
    assert!(ids(&mut b).is_empty());
}
#[test]
fn size_pressure_uses_lru_sources_and_placement_replacement_is_counted() {
    let (mut a, mut b, quota) = terminals(10, 8);
    a.feed(&image('T', 1));
    b.feed(&image('t', 2));
    a.feed(b"\x1b_Ga=p,i=1,p=9,C=1;\x1b\\");
    b.feed(&image('t', 3));
    assert_eq!(ids(&mut a), [1]);
    assert_eq!(ids(&mut b), [3]);
    assert_eq!(a.snapshot().primary_graphics.placements.len(), 2);
    quota.set_limits(2, 8);
    a.feed(b"\x1b_Ga=p,i=1,p=9,C=1;\x1b\\");
    assert!(ids(&mut b).is_empty());
    assert_eq!(a.snapshot().primary_graphics.placements.len(), 1);
    assert_eq!(ids(&mut a), [1]);
}
#[test]
fn invalid_transmit_and_place_preserves_previous_mapping_and_reset_releases_placements() {
    let (mut a, mut b, _) = terminals(3, 12);
    a.feed(&image('t', 1));
    assert_eq!(
        reply(a.feed(b"\x1b_Ga=T,i=1,f=32,s=1,v=1,r=4294967295;AAAA/w==\x1b\\")),
        b"\x1b_Gi=1;EINVAL\x1b\\"
    );
    assert_eq!(ids(&mut a), [1]);
    b.feed(&image('T', 2));
    let batch = [
        b"\x1bc".as_slice(),
        image('t', 3).as_slice(),
        image('q', 4).as_slice(),
    ]
    .concat();
    a.feed(&batch);
    assert_eq!(ids(&mut a), [3]);
    assert_eq!(ids(&mut b), [2]);
    assert!(b.snapshot().primary_graphics.placements.is_empty());
}
#[test]
fn reset_and_screen_switch_retain_one_shared_source_mapping() {
    let (mut a, mut b, quota) = terminals(3, 12);
    a.feed(
        &[
            image('t', 1),
            b"\x1b[?1049h".to_vec(),
            b"\x1b_Ga=p,i=1,C=1;\x1b\\".to_vec(),
        ]
        .concat(),
    );
    assert_eq!(a.snapshot().alternate_graphics.unwrap().placements.len(), 1);
    a.feed(b"\x1bc");
    assert_eq!(ids(&mut a), [1]);
    assert!(a.snapshot().primary_graphics.placements.is_empty());
    quota.set_limits(2, 12);
    b.feed(b"\x1b_Ga=t,f=32,s=1,v=1;AAAA/w==\x1b\\");
    assert!(ids(&mut b).is_empty());
    b.feed(&image('t', 2));
    assert_eq!(ids(&mut a), [1]);
    assert_eq!(ids(&mut b), [2]);
}
#[test]
fn anonymous_placements_have_private_ids_and_release_their_last_source_reference() {
    let (mut a, _, quota) = terminals(4, 16);
    a.feed(b"\x1b_Ga=T,f=32,s=1,v=1,C=1;AAAA/w==\x1b\\");
    for id in [1, u32::MAX] {
        assert_eq!(
            reply(a.feed(format!("\x1b_Ga=p,i={id},C=1;\x1b\\").as_bytes())),
            format!("\x1b_Gi={id};ENOENT\x1b\\").as_bytes()
        );
    }
    a.feed(&image('t', u32::MAX));
    let snapshot = a.snapshot();
    let placement = &snapshot.primary_graphics.placements[0];
    assert_ne!(placement.image_id, u32::MAX);
    assert!(
        snapshot
            .primary_graphics
            .images
            .iter()
            .find(|image| image.id == placement.image_id)
            .unwrap()
            .tmux_unmapped
    );
    let mut restored = snapshot.restore().unwrap();
    assert_eq!(
        reply(restored.feed(format!("\x1b_Ga=p,i={},C=1;\x1b\\", placement.image_id).as_bytes())),
        format!("\x1b_Gi={};ENOENT\x1b\\", placement.image_id).as_bytes()
    );
    a.feed(b"\x1b[2J\x1b[3J");
    assert_eq!(ids(&mut a), [u32::MAX]);
    quota.set_limits(2, 16);
    a.feed(&image('t', 1));
    assert_eq!(ids(&mut a), [1, u32::MAX]);
}

#[test]
fn image_wire_is_compact_and_reads_legacy_arrays_without_mutating_live_state() {
    let mut terminal = Terminal::new(40, 16, 100);
    terminal.feed(&image('T', 7));
    let snapshot = terminal.snapshot();
    let mut wire = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(wire["primary_graphics"]["images"][0]["pixels"], "AAAA/w==");
    wire["primary_graphics"]["images"][0]["pixels"] = serde_json::json!([0, 0, 0, 255]);
    let legacy: tmuxx_terminal::Snapshot = serde_json::from_value(wire).unwrap();
    assert!(legacy.restore_with_image_limit(3).is_err());
    assert_eq!(
        legacy.restore().unwrap().snapshot().primary_graphics.images[0]
            .pixels
            .as_slice(),
        [0, 0, 0, 255]
    );
    assert_eq!(
        terminal.snapshot().primary_graphics.images[0]
            .pixels
            .as_slice(),
        [0, 0, 0, 255]
    );
}
