use rustty_vt::{Terminal, graphics::PlacementId};

fn place(terminal: &mut Terminal, options: &str) {
    assert!(
        terminal
            .feed(format!("\x1b_Ga=p,{options},c=1,r=1,C=1,q=2\x1b\\").as_bytes())
            .is_empty()
    );
}

#[test]
fn placeholders_only_resolve_virtual_placements_even_with_an_exact_id() {
    let mut t = Terminal::new(12, 6, 0);
    t.feed(b"\x1b_Ga=t,i=1,f=32,s=1,v=1,q=2;AQID/w==\x1b\\");
    place(&mut t, "i=1,p=9,U=1");
    place(&mut t, "i=1,p=3");
    place(&mut t, "i=1,p=2,P=1,Q=9");
    assert!(t.graphics().placeholder_target(1, 3).is_none());
    assert!(t.graphics().placeholder_target(1, 2).is_none());
    assert_eq!(
        t.graphics().placeholder_target(1, 0).unwrap().placement_id,
        PlacementId::External(9)
    );
    assert!(
        t.graphics()
            .placeholder_target(1, 9)
            .unwrap()
            .virtual_placement
    );
}

#[test]
fn placing_after_no_history_shrink_uses_the_live_cursor_row() {
    let mut t = Terminal::new(12, 6, 0);
    let erased_row = t.screen().row(0).id;
    t.feed(b"\x1b[6;1Hx\x1b[1;1H");
    t.resize(12, 3);
    let live_row = t.screen().row(t.screen().cursor.row).id;
    assert_ne!(erased_row, live_row);
    t.feed(b"\x1b_Ga=t,i=1,f=32,s=1,v=1,q=2;AQID/w==\x1b\\");
    place(&mut t, "i=1,p=1");
    let p = t
        .graphics()
        .placements
        .get((1, PlacementId::External(1)))
        .unwrap();
    assert_eq!(p.row, live_row);
    assert_eq!(p.col, t.screen().cursor.col);
    assert_eq!(t.screen().cursor.row, 0);
}

#[test]
fn erased_history_removes_fixed_roots_and_descendants_only() {
    for history_limit in [0, 100] {
        let mut terminal = Terminal::new(10, 3, history_limit);
        for id in 1..=4 {
            terminal.feed(format!("\x1b_Ga=t,i={id},f=32,s=1,v=1,q=2;AQID/w==\x1b\\").as_bytes());
        }
        place(&mut terminal, "i=1,p=1");
        place(&mut terminal, "i=2,p=1,U=1");
        terminal.feed(b"\x1b[3;1H");
        place(&mut terminal, "i=3,p=1");
        place(&mut terminal, "i=4,p=1,P=1,Q=1");
        place(&mut terminal, "i=4,p=3,P=4,Q=1");
        terminal.feed(b"\x1b[1;1H");
        place(&mut terminal, "i=4,p=2,P=3,Q=1");
        assert_eq!(terminal.graphics().placements.len(), 6);
        let generation = terminal.graphics().generation;
        terminal.feed(b"\x1b[3;1H\n\x1b[3J");
        let graphics = terminal.graphics();
        let mut keys: Vec<_> = graphics
            .placements
            .iter()
            .map(|placement| (placement.image_id, placement.placement_id))
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                (2, PlacementId::External(1)),
                (3, PlacementId::External(1)),
                (4, PlacementId::External(2)),
            ],
            "history limit {history_limit}",
        );
        assert!(graphics.generation > generation);
        assert_eq!(graphics.images.len(), 4);
        assert_eq!(graphics.bytes_used(), 16);
    }
}
