use super::*;

#[test]
fn hyperlink_map_and_set_have_independent_capacities() {
    let capacity = PageCapacity {
        cols: 10,
        rows: 10,
        hyperlink_bytes: 96,
        hyperlink_cells: 4000,
        ..PageCapacity::default()
    };
    let layout = capacity.metadata().unwrap();
    assert!(layout.hyperlink_set_layout.cap <= 2);
    assert!(layout.hyperlink_map_layout.capacity as usize * 80 / 100 >= 4000);
    assert!(layout.hyperlink_set_layout.total_size < layout.hyperlink_map_layout.total_size);

    let layout = PageCapacity {
        hyperlink_bytes: 512 * HYPERLINK_ITEM_SIZE as u16,
        hyperlink_cells: 8,
        ..capacity
    }
    .metadata()
    .unwrap();
    assert!(layout.hyperlink_set_layout.cap >= 400);
    assert_eq!(layout.hyperlink_map_layout.capacity, 16);

    let no_map = PageCapacity {
        hyperlink_cells: 0,
        ..capacity
    }
    .metadata()
    .unwrap();
    assert_eq!(no_map.hyperlink_map_layout.capacity, 0);
    assert!(no_map.hyperlink_set_layout.total_size > 0);
    let no_set = PageCapacity {
        hyperlink_bytes: 0,
        ..capacity
    }
    .metadata()
    .unwrap();
    assert_eq!(no_set.hyperlink_set_layout.total_size, 0);
    assert_eq!(
        no_set.hyperlink_map_layout,
        capacity.metadata().unwrap().hyperlink_map_layout
    );
}

#[test]
fn hyperlink_map_rounds_once_and_preserves_its_request_when_adjusting_columns() {
    let capacity = PageCapacity {
        cols: 80,
        rows: 24,
        hyperlink_cells: 48,
        ..PageCapacity::default()
    };
    // Scale 48 for 80% load, then round 60 to 64; do not round 48 first.
    assert_eq!(
        capacity.metadata().unwrap().hyperlink_map_layout.capacity,
        64
    );
    assert_eq!(PageCapacity::default().hyperlink_cells, 64);
    assert_eq!(
        PageCapacity::STANDARD
            .metadata()
            .unwrap()
            .hyperlink_map_layout
            .capacity,
        128
    );
    assert_eq!(
        PageCapacity {
            hyperlink_cells: u16::MAX,
            ..capacity
        }
        .metadata()
        .unwrap()
        .hyperlink_map_layout
        .capacity,
        131072
    );
    for columns in [1, 79, 160] {
        let adjusted = capacity.adjust_columns(columns).unwrap();
        assert_eq!(adjusted.hyperlink_cells, 48);
        assert_eq!(adjusted.metadata(), capacity.metadata());
        assert!(adjusted.layout().unwrap().total_size <= capacity.layout().unwrap().total_size);
    }
}

#[test]
fn old_capacity_json_defaults_the_new_map_field() {
    let old: PageCapacity =
        serde_json::from_str(r#"{"cols":80,"rows":24,"hyperlink_bytes":0}"#).unwrap();
    assert_eq!(old.hyperlink_cells, 64);
    assert_eq!(old.hyperlink_bytes, 0);
    let zero: PageCapacity =
        serde_json::from_str(r#"{"cols":80,"rows":24,"hyperlink_bytes":192,"hyperlink_cells":0}"#)
            .unwrap();
    assert_eq!(zero.hyperlink_cells, 0);
    let encoded = serde_json::to_value(zero).unwrap();
    assert_eq!(encoded["hyperlink_cells"], 0);
    assert_eq!(
        serde_json::from_value::<PageCapacity>(encoded).unwrap(),
        zero
    );
}
