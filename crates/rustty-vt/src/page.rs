//! Typed page storage. Row rotations move headers, leaving physical cell keys stable.
use std::{
    collections::HashMap,
    hash::{BuildHasher, BuildHasherDefault, Hasher},
    ops::Range,
    sync::Arc,
};

#[cfg(feature = "allocation-probe")]
use crate::allocation_probe::{Kind, Scope};
use serde::{Deserialize, Serialize};

use crate::{
    packed::{Cell, RowHeader},
    page_layout::PageCapacity,
    page_list::PageAllocationInfo,
    page_resources::{
        GraphemeAdmission, GraphemeAllocation, HyperlinkAdmission, HyperlinkFull, SetFull,
        StyleAdmission,
    },
    screen::{Color, HyperlinkData, RowView, Style},
};

#[derive(Clone, Copy)]
pub(crate) enum PageResource {
    Styles,
    Graphemes,
    Links,
    Strings,
}

impl PageResource {
    pub fn for_link(error: HyperlinkFull) -> Option<Self> {
        match error {
            HyperlinkFull::Strings => Some(Self::Strings),
            HyperlinkFull::Set(SetFull::NeedsRehash) => None,
            HyperlinkFull::Set(SetFull::OutOfMemory) | HyperlinkFull::Map => Some(Self::Links),
        }
    }
}

/// Temporary ownership when a copy crosses pages or resource admission can relocate them.
#[derive(Clone, Debug)]
pub(crate) struct CellCopy {
    pub cell: Cell,
    pub style: Style,
    pub link: Option<Arc<HyperlinkData>>,
    pub link_id: u16,
    pub text: Option<(Arc<str>, u8)>,
}

impl CellCopy {
    pub fn plain(cell: Cell) -> Self {
        Self {
            cell,
            style: Style::default(),
            link: None,
            link_id: 0,
            text: None,
        }
    }
}

// Keys are bounded physical cell slots, never user-provided strings. Mix both
// consecutive and row-strided slots into bucket indices and fingerprints.
#[derive(Default)]
pub(crate) struct SlotHasher(u64);

impl Hasher for SlotHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write_u32(&mut self, slot: u32) {
        let mixed = u64::from(slot).wrapping_mul(0x9e3779b97f4a7c15);
        self.0 = mixed ^ (mixed >> 32);
    }

    fn write(&mut self, _: &[u8]) {
        unreachable!("physical slot maps only hash u32 keys");
    }
}

type SlotMap<T> = HashMap<u32, T, BuildHasherDefault<SlotHasher>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Page {
    pub capacity: PageCapacity,
    pub columns: u16,
    pub rows: u16,
    pub serial: u64,
    #[serde(skip)]
    pub layout_generation: u64,
    pub styles: StyleAdmission,
    #[serde(skip)]
    pub graphemes: GraphemeAdmission,
    #[serde(skip)]
    pub links: HyperlinkAdmission,
    #[serde(skip)]
    pub cells: Vec<Cell>,
    #[serde(skip)]
    pub headers: Vec<RowHeader>,
    #[serde(skip)]
    pub row_ids: Vec<u64>,
    #[serde(skip)]
    pub grapheme_map: SlotMap<GraphemeAllocation>,
    #[serde(skip)]
    pub link_map: SlotMap<u16>,
    // Legacy JSON can give equal native links different display strings or
    // omit their ID. Preserve those uncommon host values outside live words.
    #[serde(skip)]
    link_overrides: SlotMap<Arc<HyperlinkData>>,
    #[serde(skip)]
    override_bytes: usize,
    #[serde(skip)]
    map_bytes: [usize; 3],
    #[serde(skip)]
    owned_bytes: usize,
}

impl Page {
    pub fn new(capacity: PageCapacity, rows: u16, serial: u64) -> Self {
        assert!(rows <= capacity.rows);
        let layout = capacity.layout().expect("validated page capacity");
        let mut page = Self {
            capacity,
            columns: capacity.cols,
            rows,
            serial,
            layout_generation: 0,
            styles: StyleAdmission::new(layout.styles_layout),
            graphemes: GraphemeAdmission::new(
                layout.grapheme_alloc_layout,
                layout.grapheme_map_layout.capacity as usize,
            ),
            links: HyperlinkAdmission::new(
                layout.hyperlink_set_layout,
                layout.string_alloc_layout,
                layout.hyperlink_map_layout.capacity as usize * 80 / 100,
            ),
            cells: {
                #[cfg(feature = "allocation-probe")]
                let _scope = Scope::enter(Kind::PageBuffer);
                vec![Cell::default(); usize::from(capacity.cols) * usize::from(capacity.rows)]
            },
            headers: {
                #[cfg(feature = "allocation-probe")]
                let _scope = Scope::enter(Kind::PageBuffer);
                (0..capacity.rows)
                    .map(|row| RowHeader::new(u32::from(row) * u32::from(capacity.cols)))
                    .collect()
            },
            row_ids: {
                #[cfg(feature = "allocation-probe")]
                let _scope = Scope::enter(Kind::PageBuffer);
                vec![0; usize::from(capacity.rows)]
            },
            grapheme_map: SlotMap::default(),
            link_map: SlotMap::default(),
            link_overrides: SlotMap::default(),
            override_bytes: 0,
            map_bytes: [0; 3],
            owned_bytes: 0,
        };
        page.refresh_charge();
        page
    }

    pub fn allocation(&self) -> PageAllocationInfo {
        let layout = self.capacity.layout().expect("validated page capacity");
        PageAllocationInfo {
            columns: self.columns,
            rows: self.rows,
            capacity: self.capacity,
            pooled: layout.pooled(false),
            allocation_bytes: layout.allocation_bytes(false),
        }
    }

    pub fn adjusted_capacity(&self, columns: u16, reflow: bool) -> PageCapacity {
        self.capacity
            .adjust_columns(columns)
            .unwrap_or_else(|_| PageCapacity {
                cols: columns,
                rows: if reflow {
                    self.rows.min(PageCapacity::STANDARD.rows)
                } else {
                    self.rows.min(self.capacity.rows)
                },
                ..self.capacity
            })
    }

    pub fn storage_bytes(&self) -> usize {
        self.owned_bytes
    }

    pub fn refresh_charge(&mut self) {
        // HashMap::capacity excludes empty buckets; round up to charge the
        // bucket array, control bytes and the sentinel group conservatively.
        fn map_bytes<K, V, S: BuildHasher>(map: &HashMap<K, V, S>) -> usize {
            if map.capacity() == 0 {
                0
            } else {
                map.capacity().next_power_of_two() * (size_of::<(K, V)>() + 1) + 16
            }
        }
        // Deletions can reduce HashMap::capacity without releasing buckets.
        // Keep the largest charge until a rebuild replaces the allocation.
        self.map_bytes[0] = self.map_bytes[0].max(map_bytes(&self.grapheme_map));
        self.map_bytes[1] = self.map_bytes[1].max(map_bytes(&self.link_map));
        self.map_bytes[2] = self.map_bytes[2].max(map_bytes(&self.link_overrides));
        self.owned_bytes = size_of::<Self>()
            + self.cells.capacity() * size_of::<Cell>()
            + self.headers.capacity() * size_of::<RowHeader>()
            + self.row_ids.capacity() * size_of::<u64>()
            + self.styles.storage_bytes()
            + self.links.storage_bytes()
            + self.graphemes.storage_bytes()
            + self.override_bytes
            + self.map_bytes.iter().sum::<usize>();
    }

    #[inline]
    pub fn slot(&self, row: usize, col: usize) -> usize {
        self.headers[row].offset() + col
    }

    #[inline]
    pub fn row_cells(&self, row: usize) -> &[Cell] {
        let start = self.slot(row, 0);
        &self.cells[start..start + usize::from(self.columns)]
    }

    #[inline]
    pub fn row_cells_mut(&mut self, row: usize) -> &mut [Cell] {
        let start = self.slot(row, 0);
        &mut self.cells[start..start + usize::from(self.columns)]
    }

    #[inline]
    pub fn row(&self, row: usize) -> RowView<'_> {
        RowView::new(self, row)
    }

    #[inline]
    pub fn style(&self, slot: usize) -> Style {
        let cell = self.cells[slot];
        let mut style = if cell.style_id() == 0 {
            Style::default()
        } else {
            *self.styles.get(cell.style_id())
        };
        if let Some(background) = cell.background() {
            style.background = background;
        }
        style
    }

    #[inline]
    pub fn link_id(&self, slot: usize) -> u16 {
        if self.cells[slot].has_hyperlink() {
            self.link_map[&(slot as u32)]
        } else {
            0
        }
    }

    #[inline]
    pub fn grapheme(&self, slot: usize) -> Option<GraphemeAllocation> {
        self.cells[slot]
            .has_grapheme()
            .then(|| self.grapheme_map[&(slot as u32)])
    }

    pub fn hyperlink(&self, slot: usize) -> Option<&Arc<HyperlinkData>> {
        let id = self.link_id(slot);
        (id != 0).then(|| {
            self.link_overrides
                .get(&(slot as u32))
                .unwrap_or_else(|| self.links.data(id))
        })
    }

    pub fn set_link_data(&mut self, slot: usize, id: u16, data: Arc<HyperlinkData>) {
        if let Some(old) = self.link_overrides.remove(&(slot as u32)) {
            self.override_bytes -= old.storage_bytes();
        }
        if data != *self.links.data(id) {
            self.override_bytes += data.storage_bytes();
            self.link_overrides.insert(slot as u32, data);
        }
    }

    pub fn copy_cell(&self, slot: usize) -> CellCopy {
        let cell = self.cells[slot];
        let link_id = self.link_id(slot);
        CellCopy {
            cell,
            style: if cell.style_id() == 0 {
                Style::default()
            } else {
                *self.styles.get(cell.style_id())
            },
            link: self.hyperlink(slot).cloned(),
            link_id,
            text: self
                .grapheme(slot)
                .map(|allocation| (self.graphemes.text_arc(allocation), allocation.len)),
        }
    }

    #[inline]
    pub fn mark_cell(&mut self, row: usize, cell: Cell) {
        let header = &mut self.headers[row];
        header.set(RowHeader::DIRTY, true);
        if cell.style_id() != 0 {
            header.set(RowHeader::STYLED, true);
        }
        if cell.has_hyperlink() {
            header.set(RowHeader::HYPERLINK, true);
        }
        if cell.has_grapheme() {
            header.set(RowHeader::GRAPHEME, true);
        }
        if cell.codepoint() == Some(crate::graphics::unicode::PLACEHOLDER) {
            header.set(RowHeader::PLACEHOLDER, true);
        }
    }

    #[inline]
    pub fn replace_simple_styles(&mut self, old: u16, new: u16, count: usize) {
        if old != new && count != 0 {
            let count = u16::try_from(count).expect("run fits a physical row");
            self.styles.release_many(old, count);
            self.styles.retain_many(new, count);
        }
    }

    pub fn clear_cell(&mut self, slot: usize, background: Color) {
        let cell = self.cells[slot];
        self.styles.release(cell.style_id());
        if cell.has_hyperlink() {
            let id = self
                .link_map
                .remove(&(slot as u32))
                .expect("linked cell has a map entry");
            self.links.release_cell(id);
            if let Some(data) = self.link_overrides.remove(&(slot as u32)) {
                self.override_bytes -= data.storage_bytes();
            }
        }
        if cell.has_grapheme() {
            let allocation = self
                .grapheme_map
                .remove(&(slot as u32))
                .expect("grapheme cell has a map entry");
            self.graphemes.release(allocation);
        }
        self.cells[slot] = Cell::blank(background);
    }

    pub fn reset_row(&mut self, row: usize, id: u64, background: Color) {
        let start = self.slot(row, 0);
        let released_payload = self.headers[row].has(RowHeader::GRAPHEME | RowHeader::HYPERLINK);
        if self.headers[row].has(RowHeader::MANAGED) {
            for slot in start..start + usize::from(self.columns) {
                self.clear_cell(slot, background);
            }
        } else {
            self.row_cells_mut(row).fill(Cell::blank(background));
        }
        self.headers[row].reset();
        self.row_ids[row] = id;
        if released_payload {
            self.refresh_charge();
        }
    }

    pub fn expose(&mut self, id: u64, background: Color) {
        assert!(self.rows < self.capacity.rows);
        self.reset_row(usize::from(self.rows), id, background);
        self.rows += 1;
    }

    pub fn rotate_rows(&mut self, range: Range<usize>, up: bool) {
        for header in &mut self.headers[range.clone()] {
            header.set(RowHeader::DIRTY, true);
        }
        if up {
            self.headers[range.clone()].rotate_left(1);
            self.row_ids[range].rotate_left(1);
        } else {
            self.headers[range.clone()].rotate_right(1);
            self.row_ids[range].rotate_right(1);
        }
        self.layout_generation = self.layout_generation.wrapping_add(1);
    }

    pub fn remove_prefix(&mut self, count: usize) {
        for row in 0..count {
            self.reset_row(row, 0, Color::Default);
        }
        self.headers[..usize::from(self.rows)].rotate_left(count);
        self.row_ids[..usize::from(self.rows)].rotate_left(count);
        self.rows -= count as u16;
        self.layout_generation = self.layout_generation.wrapping_add(1);
    }

    pub fn truncate(&mut self, rows: usize) {
        for row in rows..usize::from(self.rows) {
            self.reset_row(row, 0, Color::Default);
        }
        self.rows = rows as u16;
        self.layout_generation = self.layout_generation.wrapping_add(1);
    }

    pub fn recyclable(&self, capacity: PageCapacity) -> bool {
        self.capacity == capacity
    }

    pub fn recycle(&mut self, capacity: PageCapacity, serial: u64) {
        // An exhausted reflow page can change width within the same allocation.
        // Vec retains its cell allocation whenever the new capacity fits.
        #[cfg(feature = "allocation-probe")]
        let _scope = Scope::enter(Kind::PageBuffer);
        self.cells.fill(Cell::default());
        self.cells.resize(
            usize::from(capacity.cols) * usize::from(capacity.rows),
            Cell::default(),
        );
        self.headers
            .resize(usize::from(capacity.rows), RowHeader::default());
        self.row_ids.resize(usize::from(capacity.rows), 0);
        self.row_ids.fill(0);
        for (row, header) in self.headers.iter_mut().enumerate() {
            *header = RowHeader::new((row * usize::from(capacity.cols)) as u32);
        }
        #[cfg(feature = "allocation-probe")]
        drop(_scope);
        self.styles.reset();
        self.graphemes.reset();
        self.links.reset();
        self.link_map.clear();
        self.link_overrides.clear();
        self.override_bytes = 0;
        self.grapheme_map.clear();
        self.capacity = capacity;
        self.columns = capacity.cols;
        self.rows = 0;
        self.serial = serial;
        self.layout_generation = self.layout_generation.wrapping_add(1);
        self.refresh_charge();
    }

    pub fn erase(
        &mut self,
        row: usize,
        mut start: usize,
        mut end: usize,
        background: Color,
        protected: bool,
    ) {
        let cells = self.row_cells(row);
        if start >= cells.len() || start >= end {
            return;
        }
        end = end.min(cells.len());
        if cells[start].width() == 0 && start > 0 {
            start -= 1;
        }
        if end < cells.len() && cells[end - 1].width() == 2 {
            end += 1;
        }
        let offset = self.slot(row, 0);
        let mut released_payload = false;
        for slot in offset + start..offset + end {
            if !protected || !self.cells[slot].protected() {
                released_payload |=
                    self.cells[slot].has_grapheme() || self.cells[slot].has_hyperlink();
                self.clear_cell(slot, background);
            }
        }
        self.headers[row].set(RowHeader::DIRTY, true);
        if released_payload {
            self.refresh_charge();
        }
    }

    fn swap_cells(&mut self, a: usize, b: usize) {
        let swap = |map: &mut SlotMap<_>| {
            let left = map.remove(&(a as u32));
            let right = map.remove(&(b as u32));
            if let Some(value) = left {
                map.insert(b as u32, value);
            }
            if let Some(value) = right {
                map.insert(a as u32, value);
            }
        };
        if self.cells[a].has_hyperlink() || self.cells[b].has_hyperlink() {
            swap(&mut self.link_map);
            let left = self.link_overrides.remove(&(a as u32));
            let right = self.link_overrides.remove(&(b as u32));
            if let Some(value) = left {
                self.link_overrides.insert(b as u32, value);
            }
            if let Some(value) = right {
                self.link_overrides.insert(a as u32, value);
            }
        }
        // Separate closure because the maps have different value types.
        if self.cells[a].has_grapheme() || self.cells[b].has_grapheme() {
            let left = self.grapheme_map.remove(&(a as u32));
            let right = self.grapheme_map.remove(&(b as u32));
            if let Some(value) = left {
                self.grapheme_map.insert(b as u32, value);
            }
            if let Some(value) = right {
                self.grapheme_map.insert(a as u32, value);
            }
        }
        self.cells.swap(a, b);
    }

    pub fn move_cells(&mut self, source: usize, destination: usize, len: usize, background: Color) {
        for i in 0..len {
            self.clear_cell(destination + i, background);
            self.swap_cells(source + i, destination + i);
        }
    }

    pub fn shift_cells(
        &mut self,
        row: usize,
        range: Range<usize>,
        count: usize,
        right: bool,
        background: Color,
    ) {
        let offset = self.slot(row, 0);
        if right {
            for i in (range.start + count..range.end).rev() {
                self.clear_cell(offset + i, background);
                self.swap_cells(offset + i - count, offset + i);
            }
            for i in range.start..range.start + count {
                self.clear_cell(offset + i, background);
            }
        } else {
            for i in range.start..range.end - count {
                self.clear_cell(offset + i, background);
                self.swap_cells(offset + i + count, offset + i);
            }
            for i in range.end - count..range.end {
                self.clear_cell(offset + i, background);
            }
        }
        self.repair_wide(row, background);
        self.refresh_charge();
    }

    pub fn repair_wide(&mut self, row: usize, background: Color) {
        let start = self.slot(row, 0);
        let len = usize::from(self.columns);
        for col in 0..len {
            let cell = self.cells[start + col];
            if cell.spacer_head() && col + 1 != len {
                self.cells[start + col].set_spacer_head(false);
            }
            if cell.width() == 0 && (col == 0 || self.cells[start + col - 1].width() != 2)
                || cell.width() == 2 && (col + 1 == len || self.cells[start + col + 1].width() != 0)
            {
                self.clear_cell(start + col, background);
            }
        }
        self.headers[row].set(RowHeader::DIRTY, true);
    }

    /// Rebuild admission transactionally: failures leave live words and maps intact.
    pub fn rebuild(&mut self, grow: Option<PageResource>) -> Result<(), SetFull> {
        #[cfg(feature = "allocation-probe")]
        let _scope = Scope::rebuild(grow.is_some());
        let mut capacity = self.capacity;
        if let Some(resource) = grow {
            let (old, default, maximum, used) = match resource {
                PageResource::Styles => (
                    u64::from(capacity.styles),
                    16,
                    u64::from(u16::MAX),
                    self.styles.count() as u64,
                ),
                PageResource::Graphemes => (
                    u64::from(capacity.grapheme_bytes),
                    1024,
                    u64::from(u32::MAX),
                    self.graphemes.used_bytes() as u64,
                ),
                PageResource::Links => (
                    u64::from(capacity.hyperlink_bytes),
                    192,
                    u64::from(u16::MAX),
                    0,
                ),
                PageResource::Strings => (
                    u64::from(capacity.string_bytes),
                    2048,
                    u64::from(u32::MAX),
                    0,
                ),
            };
            if old == maximum {
                return Err(SetFull::OutOfMemory);
            }
            let assign = |cap: &mut PageCapacity, value: u64| match resource {
                PageResource::Styles => cap.styles = value as u16,
                PageResource::Graphemes => cap.grapheme_bytes = value as u32,
                PageResource::Links => cap.hyperlink_bytes = value as u16,
                PageResource::Strings => cap.string_bytes = value as u32,
            };
            let increased = if old == 0 {
                default
            } else {
                (old * 2).min(maximum)
            };
            assign(&mut capacity, increased);
            capacity.layout().map_err(|_| SetFull::OutOfMemory)?;
            if used != 0 && self.rows != 0 {
                let density = used * u64::from(capacity.rows) / u64::from(self.rows);
                let projected = (density + density / 4).min(old * 32).min(maximum);
                if projected > increased {
                    let mut projected_capacity = capacity;
                    assign(&mut projected_capacity, projected);
                    if projected_capacity.layout().is_ok() {
                        capacity = projected_capacity;
                    }
                }
            }
        }
        let layout = capacity.metadata().unwrap();
        let mut styles = StyleAdmission::new(layout.styles_layout);
        let mut graphemes = GraphemeAdmission::new(
            layout.grapheme_alloc_layout,
            layout.grapheme_map_layout.capacity as usize,
        );
        let mut links = HyperlinkAdmission::new(
            layout.hyperlink_set_layout,
            layout.string_alloc_layout,
            layout.hyperlink_map_layout.capacity as usize * 80 / 100,
        );
        styles.reserve_entries(self.styles.count());
        links.reserve_entries(&self.links);
        let mut grapheme_map =
            SlotMap::with_capacity_and_hasher(self.grapheme_map.len(), Default::default());
        let mut link_map =
            SlotMap::with_capacity_and_hasher(self.link_map.len(), Default::default());
        let mut pending = Vec::new();
        for row in 0..usize::from(self.rows) {
            if !self.headers[row].has(RowHeader::MANAGED) {
                continue;
            }
            let start = self.slot(row, 0);
            for slot in start..start + usize::from(self.columns) {
                let cell = self.cells[slot];
                if let Some(allocation) = self.grapheme(slot) {
                    let next = graphemes.acquire(allocation.len)?;
                    graphemes.set_text(next, self.graphemes.text_arc(allocation));
                    grapheme_map.insert(slot as u32, next);
                }
                if cell.has_hyperlink() {
                    let id = self.link_id(slot);
                    let next = links
                        .copy_from(&self.links, id)
                        .map_err(|_| SetFull::OutOfMemory)?;
                    link_map.insert(slot as u32, next);
                }
                if cell.style_id() != 0 {
                    let id = styles
                        .acquire_with_id(*self.styles.get(cell.style_id()), cell.style_id())?;
                    pending.push((slot, id));
                }
            }
        }
        for (slot, id) in pending {
            self.cells[slot].set_style_id(id);
        }
        self.capacity = capacity;
        self.styles = styles;
        self.graphemes = graphemes;
        self.links = links;
        self.grapheme_map = grapheme_map;
        self.link_map = link_map;
        self.map_bytes = [0, 0, self.map_bytes[2]];
        self.layout_generation = self.layout_generation.wrapping_add(1);
        self.refresh_charge();
        Ok(())
    }
}
