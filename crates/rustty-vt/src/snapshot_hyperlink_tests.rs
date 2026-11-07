use super::*;
use crate::page_resources::HyperlinkKey;

fn linked_page(capacity: PageCapacity, count: usize) -> Page {
    let mut page = Page::new(capacity, capacity.rows, 0);
    let key = HyperlinkKey::new(b"https://example.test/dense", None, 1);
    assert!(count <= usize::from(capacity.cols) * usize::from(capacity.rows));
    for index in 0..count {
        let row = index / usize::from(capacity.cols);
        let col = index % usize::from(capacity.cols);
        let slot = page.slot(row, col);
        let id = page.links.copy_cell(key, 0).unwrap();
        page.link_map.insert(slot as u32, id);
        page.cells[slot].set_codepoint(Some('L'));
        page.cells[slot].set_hyperlink(true);
        page.mark_cell(row, page.cells[slot]);
    }
    page
}

fn encode(page: &Page) -> Vec<u8> {
    let rows: Vec<_> = (0..usize::from(page.rows))
        .map(|row| page.row(row))
        .collect();
    encode_page(&rows, page).unwrap()
}

fn decode(payload: &[u8]) -> Page {
    Decoder::new(std::io::empty(), DecodeOptions::default())
        .decode_page(payload)
        .unwrap()
}

#[test]
fn single_snapshot_hint_covers_independent_hyperlink_tables() {
    let base = PageCapacity {
        cols: 80,
        rows: 24,
        ..PageCapacity::default()
    };
    for capacity in [
        PageCapacity {
            hyperlink_bytes: 96,
            hyperlink_cells: 1500,
            ..base
        },
        PageCapacity {
            hyperlink_bytes: 300 * 48,
            hyperlink_cells: 8,
            ..base
        },
        PageCapacity {
            hyperlink_bytes: 0,
            hyperlink_cells: 0,
            ..base
        },
        base,
        PageCapacity {
            cols: 200,
            rows: 200,
            hyperlink_cells: 21841,
            ..base
        },
        PageCapacity {
            cols: 200,
            rows: 200,
            hyperlink_cells: u16::MAX,
            ..base
        },
    ] {
        let page = Page::new(capacity, capacity.rows, 0);
        let decoded = decode(&encode(&page));
        assert!(decoded.capacity.hyperlink_bytes >= capacity.hyperlink_bytes);
        let cells = usize::from(capacity.cols) * usize::from(capacity.rows);
        assert!(
            usize::from(decoded.capacity.hyperlink_cells)
                >= usize::from(capacity.hyperlink_cells).min(cells)
        );
        assert_eq!(decoded.capacity.styles, capacity.styles);
        assert_eq!(decoded.capacity.string_bytes, capacity.string_bytes);
        if capacity == base {
            assert_eq!(decoded.capacity, capacity);
        }
    }
}

#[test]
fn saturated_snapshot_hint_retains_more_links_than_the_old_map_could_hold() {
    // 30000 exceeds both the old request ceiling (21840) and the rounded
    // table's usable load (26214), so dropping the new decode rule loses links.
    let count = 30_000;
    let page = linked_page(
        PageCapacity {
            cols: 200,
            rows: 200,
            hyperlink_cells: count as u16,
            ..PageCapacity::default()
        },
        count,
    );
    let encoded = encode(&page);
    assert_eq!(
        u16::from_le_bytes(encoded[10..12].try_into().unwrap()),
        u16::MAX
    );
    let decoded = decode(&encoded);
    assert_eq!(decoded.capacity.hyperlink_cells, 40_000);
    assert_eq!(decoded.link_map.len(), count);
    decoded
        .links
        .assert_references(decoded.link_map.values().copied(), None);
    for index in 0..count {
        assert_eq!(
            decoded
                .row(index / 200)
                .hyperlink(index % 200)
                .unwrap()
                .uri_bytes(),
            b"https://example.test/dense"
        );
    }
    assert_eq!(decode(&encode(&decoded)).link_map.len(), count);
}

#[test]
fn old_snapshot_hints_keep_their_derived_map_and_saturate_at_the_item_boundary() {
    for (hint, cells) in [
        (0, 0),
        (47, 0),
        (48, 16),
        (192, 64),
        (65519, 21824),
        (65520, 40000),
        (65535, 40000),
    ] {
        assert_eq!(hyperlink_cells_from_hint(hint, 200, 200), cells);
    }
    assert_eq!(hyperlink_cells_from_hint(65535, 1000, 1000), u16::MAX);
    let page = linked_page(
        PageCapacity {
            cols: 80,
            rows: 1,
            hyperlink_cells: 512,
            ..PageCapacity::default()
        },
        80,
    );
    let mut payload = encode(&page);
    // An old writer's ordinary 192-byte hint still provides 64 requested
    // entries (102 usable), enough for all 80 linked cells of this fixture.
    payload[10..12].copy_from_slice(&192u16.to_le_bytes());
    let decoded = decode(&payload);
    assert_eq!(decoded.capacity.hyperlink_cells, 64);
    assert_eq!(decoded.link_map.len(), 80);
}
