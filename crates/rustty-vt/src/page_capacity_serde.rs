//! Keep full/compact Screen JSON compatible with the original coupled capacity.
//! Layout requests use PageCapacity directly and default the map to 64 cells.
use crate::page_layout::{HYPERLINK_ITEM_SIZE, PageCapacity};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// GHOSTSNP v1 and legacy Screen JSON retain one u16 hyperlink hint. It covers the set bytes and
// the map's equivalent request under the historical 16-cells-per-item rule.
pub(crate) fn hyperlink_capacity_hint(capacity: PageCapacity) -> u16 {
    let map_bytes = usize::from(capacity.hyperlink_cells).div_ceil(16) * HYPERLINK_ITEM_SIZE;
    usize::from(capacity.hyperlink_bytes)
        .max(map_bytes)
        .min(usize::from(u16::MAX)) as u16
}

pub(crate) fn hyperlink_cells_from_hint(bytes: u16, cols: u16, rows: u16) -> u16 {
    let item_size = HYPERLINK_ITEM_SIZE;
    let items = usize::from(bytes) / item_size;
    // 65520..65535 can represent a saturated hint. A map for all page cells
    // retains dense links that no longer fit the old byte-derived request.
    let cells = if items >= usize::from(u16::MAX) / item_size {
        usize::from(cols) * usize::from(rows)
    } else {
        items * 16
    };
    cells.min(usize::from(u16::MAX)) as u16
}

#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Capacity {
    cols: u16,
    rows: u16,
    styles: u16,
    hyperlink_bytes: u16,
    grapheme_bytes: u32,
    string_bytes: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    hyperlink_cells: Option<u16>,
}

impl Default for Capacity {
    fn default() -> Self {
        Self::from(PageCapacity::default())
    }
}

impl From<PageCapacity> for Capacity {
    fn from(capacity: PageCapacity) -> Self {
        Self {
            cols: capacity.cols,
            rows: capacity.rows,
            styles: capacity.styles,
            hyperlink_bytes: hyperlink_capacity_hint(capacity),
            grapheme_bytes: capacity.grapheme_bytes,
            string_bytes: capacity.string_bytes,
            // Old readers reject unknown capacity fields. The combined hint
            // preserves their shape and covers both independent resources.
            hyperlink_cells: None,
        }
    }
}

pub(crate) fn serialize<S: Serializer>(
    capacity: &PageCapacity,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    Capacity::from(*capacity).serialize(serializer)
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<PageCapacity, D::Error> {
    let capacity = Capacity::deserialize(deserializer)?;
    Ok(PageCapacity {
        cols: capacity.cols,
        rows: capacity.rows,
        styles: capacity.styles,
        hyperlink_bytes: capacity.hyperlink_bytes,
        hyperlink_cells: capacity.hyperlink_cells.unwrap_or_else(|| {
            hyperlink_cells_from_hint(capacity.hyperlink_bytes, capacity.cols, capacity.rows)
        }),
        grapheme_bytes: capacity.grapheme_bytes,
        string_bytes: capacity.string_bytes,
    })
}
