//! Native page accounting without allocating native backing memory.
//!
//! These are libghostty's 64-bit ABI sizes, not Rust object sizes. Keep the
//! layout arithmetic aligned with terminal/{Page,page,hash_map,bitmap_allocator,
//! ref_counted_set}.zig; the process oracle compares every offset and capacity.
use serde::{Deserialize, Serialize};

pub(crate) const PAGE_ALIGNMENT: usize = if cfg!(all(target_os = "macos", target_arch = "aarch64"))
{
    16 * 1024
} else {
    4096
};
pub(crate) const CELL_ALIGNMENT: usize = if cfg!(target_arch = "aarch64") {
    128
} else {
    64
};
pub(crate) const ROW_SIZE: usize = 8;
pub(crate) const CELL_SIZE: usize = 8;
pub(crate) const STYLE_ITEM_SIZE: usize = 36;
pub(crate) const STYLE_ITEM_ALIGNMENT: usize = 4;
pub(crate) const HYPERLINK_ITEM_SIZE: usize = 48;
pub(crate) const METADATA_ALIGNMENT: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LayoutError {
    InvalidDimensions,
    OutOfMemory,
    RowCountOverflow,
    PageTooLarge,
    ArithmeticOverflow,
    UnsupportedPlatform,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PageCapacity {
    pub cols: u16,
    pub rows: u16,
    pub styles: u16,
    pub hyperlink_bytes: u16,
    pub grapheme_bytes: u32,
    pub string_bytes: u32,
}

impl Default for PageCapacity {
    fn default() -> Self {
        Self {
            cols: 0,
            rows: 0,
            styles: 16,
            hyperlink_bytes: 4 * HYPERLINK_ITEM_SIZE as u16,
            grapheme_bytes: 64 * 16,
            string_bytes: 64 * 32,
        }
    }
}

impl PageCapacity {
    /// Production capacity. Zig unit tests use a smaller grapheme arena;
    /// Rust unit tests still exercise the production layout used by the app.
    pub(crate) const STANDARD: Self = Self {
        cols: 215,
        rows: 215,
        styles: 128,
        hyperlink_bytes: 192,
        grapheme_bytes: 8192,
        string_bytes: 2048,
    };

    pub(crate) fn initial(cols: u16) -> Result<Self, LayoutError> {
        match Self::STANDARD.adjust_columns(cols) {
            Ok(capacity) => Ok(capacity),
            // Native initialCapacity preserves the standard row count when
            // even one row cannot fit into the standard allocation.
            Err(LayoutError::OutOfMemory) => {
                let capacity = Self {
                    cols,
                    ..Self::STANDARD
                };
                capacity.layout()?;
                Ok(capacity)
            }
            Err(error) => Err(error),
        }
    }

    /// Change columns while preserving the original allocation size and all
    /// resource capacities. The initialized row count belongs to the page.
    pub(crate) fn adjust_columns(self, cols: u16) -> Result<Self, LayoutError> {
        if cols == 0 {
            return Err(LayoutError::InvalidDimensions);
        }
        let original = self.layout()?;
        let available = original.total_size - self.metadata()?.total_size;
        let row_bytes = add(ROW_SIZE, mul(CELL_SIZE, usize::from(cols))?)?;
        let mut rows =
            u16::try_from(available / row_bytes).map_err(|_| LayoutError::RowCountOverflow)?;
        while rows > 0 {
            let adjusted = Self { cols, rows, ..self };
            // Row-header padding can push the first estimate over a page
            // boundary. Native adjust trims rows until it fits.
            match adjusted.layout() {
                Ok(layout) if layout.total_size <= original.total_size => return Ok(adjusted),
                Ok(_) | Err(LayoutError::PageTooLarge) => rows -= 1,
                Err(error) => return Err(error),
            }
        }
        Err(LayoutError::OutOfMemory)
    }

    pub(crate) fn layout(self) -> Result<PageLayout, LayoutError> {
        if !cfg!(target_pointer_width = "64") {
            return Err(LayoutError::UnsupportedPlatform);
        }
        if self.cols == 0 || self.rows == 0 {
            return Err(LayoutError::InvalidDimensions);
        }
        let metadata = self.metadata()?;
        let rows_size = mul(usize::from(self.rows), ROW_SIZE)?;
        let cells_start = align(rows_size, CELL_ALIGNMENT)?;
        let cells_size = mul(
            mul(usize::from(self.cols), usize::from(self.rows))?,
            CELL_SIZE,
        )?;
        let meta_start = align(add(cells_start, cells_size)?, METADATA_ALIGNMENT)?;
        let total_size = align(add(meta_start, metadata.total_size)?, PAGE_ALIGNMENT)?;
        // Native page offsets are u32, even on a 64-bit host.
        if total_size > u32::MAX as usize {
            return Err(LayoutError::PageTooLarge);
        }
        Ok(PageLayout {
            total_size,
            rows_start: 0,
            rows_size,
            cells_start,
            cells_size,
            styles_start: add(meta_start, metadata.styles_start)?,
            styles_layout: metadata.styles_layout,
            grapheme_alloc_start: add(meta_start, metadata.grapheme_alloc_start)?,
            grapheme_alloc_layout: metadata.grapheme_alloc_layout,
            grapheme_map_start: add(meta_start, metadata.grapheme_map_start)?,
            grapheme_map_layout: metadata.grapheme_map_layout,
            string_alloc_start: add(meta_start, metadata.string_alloc_start)?,
            string_alloc_layout: metadata.string_alloc_layout,
            hyperlink_map_start: add(meta_start, metadata.hyperlink_map_start)?,
            hyperlink_map_layout: metadata.hyperlink_map_layout,
            hyperlink_set_start: add(meta_start, metadata.hyperlink_set_start)?,
            hyperlink_set_layout: metadata.hyperlink_set_layout,
            capacity: self,
        })
    }

    pub(crate) fn metadata(self) -> Result<MetaLayout, LayoutError> {
        let styles_layout = set_layout(
            usize::from(self.styles),
            STYLE_ITEM_SIZE,
            STYLE_ITEM_ALIGNMENT,
        )?;
        let grapheme_alloc_layout = bitmap_layout(self.grapheme_bytes as usize, 16)?;
        let grapheme_alloc_start = align(styles_layout.total_size, METADATA_ALIGNMENT)?;
        let grapheme_count = power_of_two((self.grapheme_bytes as usize).div_ceil(16))?;
        let grapheme_map_layout = map_layout(grapheme_count, 16, 8, 100)?;
        let grapheme_map_start = align(
            add(grapheme_alloc_start, grapheme_alloc_layout.total_size)?,
            8,
        )?;
        let string_alloc_layout = bitmap_layout(self.string_bytes as usize, 32)?;
        let string_alloc_start =
            align(add(grapheme_map_start, grapheme_map_layout.total_size)?, 8)?;
        let hyperlink_count = usize::from(self.hyperlink_bytes) / HYPERLINK_ITEM_SIZE;
        let hyperlink_set_layout = set_layout(hyperlink_count, HYPERLINK_ITEM_SIZE, 8)?;
        let hyperlink_set_start =
            align(add(string_alloc_start, string_alloc_layout.total_size)?, 8)?;
        let hyperlink_map_layout = map_layout(mul(hyperlink_count, 16)?, 2, 2, 80)?;
        let hyperlink_map_start = align(
            add(hyperlink_set_start, hyperlink_set_layout.total_size)?,
            4,
        )?;
        Ok(MetaLayout {
            total_size: add(hyperlink_map_start, hyperlink_map_layout.total_size)?,
            styles_start: 0,
            styles_layout,
            grapheme_alloc_start,
            grapheme_alloc_layout,
            grapheme_map_start,
            grapheme_map_layout,
            string_alloc_start,
            string_alloc_layout,
            hyperlink_set_start,
            hyperlink_set_layout,
            hyperlink_map_start,
            hyperlink_map_layout,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PageLayout {
    pub total_size: usize,
    pub rows_start: usize,
    pub rows_size: usize,
    pub cells_start: usize,
    pub cells_size: usize,
    pub styles_start: usize,
    pub styles_layout: SetLayout,
    pub grapheme_alloc_start: usize,
    pub grapheme_alloc_layout: BitmapLayout,
    pub grapheme_map_start: usize,
    pub grapheme_map_layout: MapLayout,
    pub string_alloc_start: usize,
    pub string_alloc_layout: BitmapLayout,
    pub hyperlink_map_start: usize,
    pub hyperlink_map_layout: MapLayout,
    pub hyperlink_set_start: usize,
    pub hyperlink_set_layout: SetLayout,
    pub capacity: PageCapacity,
}

impl PageLayout {
    pub fn pooled(self, exact: bool) -> bool {
        !exact && self.total_size <= PageCapacity::STANDARD.layout().unwrap().total_size
    }

    pub fn allocation_bytes(self, exact: bool) -> usize {
        if self.pooled(exact) {
            PageCapacity::STANDARD.layout().unwrap().total_size
        } else {
            self.total_size
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct MetaLayout {
    pub total_size: usize,
    pub styles_start: usize,
    pub styles_layout: SetLayout,
    pub grapheme_alloc_start: usize,
    pub grapheme_alloc_layout: BitmapLayout,
    pub grapheme_map_start: usize,
    pub grapheme_map_layout: MapLayout,
    pub string_alloc_start: usize,
    pub string_alloc_layout: BitmapLayout,
    pub hyperlink_set_start: usize,
    pub hyperlink_set_layout: SetLayout,
    pub hyperlink_map_start: usize,
    pub hyperlink_map_layout: MapLayout,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct SetLayout {
    pub cap: usize,
    pub table_cap: usize,
    pub table_mask: u16,
    pub table_start: usize,
    pub items_start: usize,
    pub total_size: usize,
}

fn set_layout(
    capacity: usize,
    item_size: usize,
    item_alignment: usize,
) -> Result<SetLayout, LayoutError> {
    if capacity == 0 {
        return Ok(SetLayout::default());
    }
    let table_cap = power_of_two(capacity)?;
    let table_mask = u16::try_from(table_cap - 1).map_err(|_| LayoutError::ArithmeticOverflow)?;
    let cap = mul(table_cap, 13)? / 16;
    let items_start = align(mul(table_cap, 2)?, item_alignment)?;
    Ok(SetLayout {
        cap,
        table_cap,
        table_mask,
        table_start: 0,
        items_start,
        total_size: add(items_start, mul(cap, item_size)?)?,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct BitmapLayout {
    pub total_size: usize,
    pub bitmap_count: usize,
    pub bitmap_start: usize,
    pub chunks_start: usize,
}

fn bitmap_layout(bytes: usize, chunk_size: usize) -> Result<BitmapLayout, LayoutError> {
    let chunks = align(align(bytes, chunk_size)? / chunk_size, 64)?;
    let bitmap_count = chunks / 64;
    let chunks_start = mul(bitmap_count, 8)?;
    Ok(BitmapLayout {
        total_size: add(chunks_start, mul(chunks, chunk_size)?)?,
        bitmap_count,
        bitmap_start: 0,
        chunks_start,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct MapLayout {
    pub total_size: usize,
    pub metadata_start: usize,
    pub keys_start: usize,
    pub vals_start: usize,
    pub capacity: u32,
}

fn map_layout(
    entries: usize,
    value_size: usize,
    value_alignment: usize,
    load: usize,
) -> Result<MapLayout, LayoutError> {
    let minimum = mul(entries, 100)?.div_ceil(load);
    let cap = power_of_two(minimum.min(1 << 31))?;
    let keys_start = align(add(4, cap)?, 4)?;
    let vals_start = align(add(keys_start, mul(cap, 4)?)?, value_alignment)?;
    Ok(MapLayout {
        total_size: align(
            add(vals_start, mul(cap, value_size)?)?,
            value_alignment.max(4),
        )?,
        metadata_start: 4,
        keys_start,
        vals_start,
        capacity: u32::try_from(cap).map_err(|_| LayoutError::ArithmeticOverflow)?,
    })
}

fn align(value: usize, alignment: usize) -> Result<usize, LayoutError> {
    Ok(add(value, alignment - 1)? & !(alignment - 1))
}

fn power_of_two(value: usize) -> Result<usize, LayoutError> {
    if value == 0 {
        return Ok(0);
    }
    value
        .checked_next_power_of_two()
        .ok_or(LayoutError::ArithmeticOverflow)
}

fn add(a: usize, b: usize) -> Result<usize, LayoutError> {
    a.checked_add(b).ok_or(LayoutError::ArithmeticOverflow)
}

fn mul(a: usize, b: usize) -> Result<usize, LayoutError> {
    a.checked_mul(b).ok_or(LayoutError::ArithmeticOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_resource_maps_retain_their_native_headers() {
        let capacity = PageCapacity {
            cols: 1,
            rows: 1,
            styles: 0,
            hyperlink_bytes: 0,
            grapheme_bytes: 0,
            string_bytes: 0,
        };
        let metadata = capacity.metadata().unwrap();
        assert_eq!(metadata.total_size, 12);
        assert_eq!(metadata.styles_layout.total_size, 0);
        assert_eq!(metadata.grapheme_alloc_layout.total_size, 0);
        assert_eq!(metadata.grapheme_map_layout.total_size, 8);
        assert_eq!(metadata.grapheme_map_layout.capacity, 0);
        assert_eq!(metadata.hyperlink_map_start, 8);
        assert_eq!(metadata.hyperlink_map_layout.total_size, 4);
        assert_eq!(capacity.layout().unwrap().total_size, PAGE_ALIGNMENT);
    }

    #[test]
    fn native_standard_resource_layout() {
        let metadata = PageCapacity::STANDARD.metadata().unwrap();
        assert_eq!(metadata.total_size, 26124);
        assert_eq!(metadata.styles_layout.cap, 104);
        assert_eq!(metadata.styles_layout.table_cap, 128);
        assert_eq!(metadata.styles_layout.total_size, 4000);
        assert_eq!(metadata.grapheme_alloc_layout.bitmap_count, 8);
        assert_eq!(metadata.grapheme_alloc_layout.total_size, 8256);
        assert_eq!(metadata.grapheme_map_layout.capacity, 512);
        assert_eq!(metadata.grapheme_map_layout.total_size, 10760);
        assert_eq!(metadata.string_alloc_layout.total_size, 2056);
        assert_eq!(metadata.hyperlink_set_layout.cap, 3);
        assert_eq!(metadata.hyperlink_set_layout.total_size, 152);
        assert_eq!(metadata.hyperlink_map_layout.capacity, 128);
        assert_eq!(metadata.hyperlink_map_layout.total_size, 900);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn macos_arm64_initial_capacity_matches_native_observation() {
        let capacity = PageCapacity::initial(80).unwrap();
        assert_eq!(capacity.rows, 591);
        assert_eq!(PageCapacity::initial(215).unwrap().rows, 221);
        let layout = capacity.layout().unwrap();
        assert_eq!(layout.rows_size, 4728);
        assert_eq!(layout.cells_start, 4736);
        assert_eq!(layout.cells_size, 378240);
        assert_eq!(layout.styles_start, 382976);
        assert_eq!(layout.total_size, 409600);
        assert_eq!(PageCapacity::initial(47918).unwrap().rows, 1);
        assert_eq!(PageCapacity::initial(47919).unwrap().rows, 215);
    }

    #[test]
    fn column_adjustment_preserves_resources_and_fills_available_rows() {
        let capacity = PageCapacity::STANDARD;
        let bytes = capacity.layout().unwrap().total_size;
        for cols in [1, 3, 80, 215, 1024, 4096] {
            let adjusted = capacity.adjust_columns(cols).unwrap();
            assert_eq!(adjusted.metadata(), capacity.metadata());
            assert!(adjusted.layout().unwrap().total_size <= bytes);
            let extra_row = PageCapacity {
                rows: adjusted.rows + 1,
                ..adjusted
            };
            assert!(extra_row.layout().unwrap().total_size > bytes);
        }
    }

    #[test]
    fn wide_initial_page_retains_standard_rows_when_pool_cannot_fit_one() {
        assert_eq!(
            PageCapacity::STANDARD.adjust_columns(u16::MAX),
            Err(LayoutError::OutOfMemory)
        );
        let capacity = PageCapacity::initial(u16::MAX).unwrap();
        assert_eq!(capacity.cols, u16::MAX);
        assert_eq!(capacity.rows, PageCapacity::STANDARD.rows);
        assert!(!capacity.layout().unwrap().pooled(false));
    }

    #[test]
    fn pooled_and_exact_pages_have_distinct_charges() {
        let layout = PageCapacity {
            cols: 1,
            rows: 1,
            ..PageCapacity::default()
        }
        .layout()
        .unwrap();
        assert!(layout.pooled(false));
        assert!(!layout.pooled(true));
        assert_eq!(
            layout.allocation_bytes(false),
            PageCapacity::STANDARD.layout().unwrap().total_size
        );
        assert_eq!(layout.allocation_bytes(true), layout.total_size);
        assert!(layout.allocation_bytes(false) > layout.allocation_bytes(true));
    }

    #[test]
    fn invalid_or_unrepresentable_capacity_is_rejected_without_allocation() {
        assert_eq!(
            PageCapacity::initial(0),
            Err(LayoutError::InvalidDimensions)
        );
        assert_eq!(
            PageCapacity::default().layout(),
            Err(LayoutError::InvalidDimensions)
        );
        assert_eq!(
            PageCapacity {
                cols: u16::MAX,
                rows: u16::MAX,
                ..PageCapacity::default()
            }
            .layout(),
            Err(LayoutError::PageTooLarge)
        );
        assert_eq!(
            PageCapacity {
                cols: 1,
                rows: 1,
                string_bytes: u32::MAX,
                ..PageCapacity::default()
            }
            .layout(),
            Err(LayoutError::PageTooLarge)
        );
        assert_eq!(
            PageCapacity {
                cols: 1024,
                rows: 512,
                ..PageCapacity::default()
            }
            .adjust_columns(1),
            Err(LayoutError::RowCountOverflow)
        );
    }

    #[test]
    fn arithmetic_overflows_are_explicit() {
        assert_eq!(add(usize::MAX, 1), Err(LayoutError::ArithmeticOverflow));
        assert_eq!(mul(usize::MAX, 2), Err(LayoutError::ArithmeticOverflow));
        assert_eq!(align(usize::MAX, 8), Err(LayoutError::ArithmeticOverflow));
        assert_eq!(
            power_of_two(usize::MAX),
            Err(LayoutError::ArithmeticOverflow)
        );
        assert_eq!(
            bitmap_layout(usize::MAX, 16),
            Err(LayoutError::ArithmeticOverflow)
        );
        assert_eq!(
            map_layout(usize::MAX, 16, 8, 100),
            Err(LayoutError::ArithmeticOverflow)
        );
    }
}
