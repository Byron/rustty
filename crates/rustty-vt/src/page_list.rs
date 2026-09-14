//! Authoritative native page allocation boundaries for owned screen rows.
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::page_layout::PageCapacity;
use crate::page_resources::{GraphemeAdmission, HyperlinkAdmission, StyleAdmission};
use crate::screen::ScrollbackLimits;

/// A live list differs from its clones and replacements even when page serials
/// repeat. This is transient; resource ownership still uses local page serials.
#[derive(Debug)]
struct ListIdentity(u64);

impl Default for ListIdentity {
    fn default() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(
            NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .expect("page list identities exhausted"),
        )
    }
}

impl Clone for ListIdentity {
    fn clone(&self) -> Self {
        Self::default()
    }
}

/// A live page's physical dimensions and charged native allocation.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct PageAllocationInfo {
    pub columns: u16,
    pub rows: u16,
    pub capacity: PageCapacity,
    pub pooled: bool,
    pub allocation_bytes: usize,
}

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
}

impl Page {
    fn allocation(&self) -> PageAllocationInfo {
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
        self.capacity.adjust_columns(columns).unwrap_or_else(|_| {
            let rows = if reflow {
                self.rows.min(PageCapacity::STANDARD.rows)
            } else {
                self.rows.min(self.capacity.rows)
            };
            PageCapacity {
                cols: columns,
                rows,
                ..self.capacity
            }
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct PageList {
    pub pages: VecDeque<Page>,
    next_serial: u64,
    #[serde(skip)]
    identity: ListIdentity,
}

impl PageList {
    pub fn identity(&self) -> u64 {
        self.identity.0
    }

    pub fn new(columns: u16, rows: usize) -> Self {
        let capacity = PageCapacity::initial(columns).expect("validated screen dimensions");
        let mut result = Self::default();
        let mut remaining = rows;
        while remaining > 0 {
            let count = remaining.min(usize::from(capacity.rows));
            result.append(capacity, count as u16);
            remaining -= count;
        }
        result
    }

    pub fn allocations(&self) -> impl Iterator<Item = PageAllocationInfo> + '_ {
        self.pages.iter().map(Page::allocation)
    }

    pub fn total_rows(&self) -> usize {
        self.pages.iter().map(|page| usize::from(page.rows)).sum()
    }

    pub fn allocation_bytes(&self) -> usize {
        self.allocations().map(|page| page.allocation_bytes).sum()
    }

    pub fn page_at(&self, mut row: usize) -> (&Page, usize) {
        for page in &self.pages {
            if row < usize::from(page.rows) {
                return (page, row);
            }
            row -= usize::from(page.rows);
        }
        panic!("row is outside the page list");
    }

    pub fn page_index(&self, mut row: usize) -> usize {
        for (index, page) in self.pages.iter().enumerate() {
            if row < usize::from(page.rows) {
                return index;
            }
            row -= usize::from(page.rows);
        }
        panic!("row is outside the page list");
    }

    /// Renew cached-coordinate generations without changing resource owners.
    pub fn invalidate_layout(&mut self, first: usize, last: usize) {
        let start = self.page_index(first);
        let end = self.page_index(last);
        for page in self.pages.range_mut(start..=end) {
            page.layout_generation = page.layout_generation.wrapping_add(1);
        }
    }

    pub fn append(&mut self, capacity: PageCapacity, rows: u16) {
        assert!(rows <= capacity.rows);
        let layout = capacity.metadata().unwrap();
        self.pages.push_back(Page {
            capacity,
            columns: capacity.cols,
            rows,
            serial: self.next_serial,
            layout_generation: 0,
            styles: StyleAdmission::new(layout.styles_layout),
            links: HyperlinkAdmission::new(
                layout.hyperlink_set_layout,
                layout.string_alloc_layout,
                layout.hyperlink_map_layout.capacity as usize * 80 / 100,
            ),
            graphemes: GraphemeAdmission::new(
                layout.grapheme_alloc_layout,
                layout.grapheme_map_layout.capacity as usize,
            ),
        });
        self.next_serial = self.next_serial.wrapping_add(1);
    }

    pub fn prepend(&mut self, capacity: PageCapacity, rows: u16) {
        assert!(rows <= capacity.rows);
        let layout = capacity.metadata().unwrap();
        self.pages.push_front(Page {
            capacity,
            columns: capacity.cols,
            rows,
            serial: self.next_serial,
            layout_generation: 0,
            styles: StyleAdmission::new(layout.styles_layout),
            links: HyperlinkAdmission::new(
                layout.hyperlink_set_layout,
                layout.string_alloc_layout,
                layout.hyperlink_map_layout.capacity as usize * 80 / 100,
            ),
            graphemes: GraphemeAdmission::new(
                layout.grapheme_alloc_layout,
                layout.grapheme_map_layout.capacity as usize,
            ),
        });
        self.next_serial = self.next_serial.wrapping_add(1);
    }

    pub fn split(&mut self, index: usize, row: u16) -> bool {
        let source = &self.pages[index];
        if source.rows <= 1 {
            return false;
        }
        if row == 0 {
            return true;
        }
        assert!(row < source.rows);
        let columns = source.columns;
        let count = source.rows - row;
        let capacity = source.capacity;
        self.append(capacity, count);
        let mut target = self.pages.pop_back().unwrap();
        target.columns = columns;
        self.pages[index].rows = row;
        self.pages[index].layout_generation = self.pages[index].layout_generation.wrapping_add(1);
        self.pages.insert(index + 1, target);
        true
    }

    pub fn effective_limits(
        columns: u16,
        active_rows: usize,
        limits: ScrollbackLimits,
    ) -> ScrollbackLimits {
        let capacity = PageCapacity::initial(columns).expect("validated screen dimensions");
        let minimum_lines = usize::from(capacity.rows);
        let standard_bytes = PageCapacity::STANDARD
            .layout()
            .expect("standard page layout")
            .allocation_bytes(false);
        let minimum_bytes = standard_bytes * (active_rows.max(1).div_ceil(minimum_lines) + 1);
        ScrollbackLimits {
            bytes: limits.bytes.map(|bytes| bytes.max(minimum_bytes)),
            lines: limits.lines.map(|lines| lines.max(minimum_lines)),
        }
    }

    /// Grow one physical row and return the number of recycled history rows.
    pub fn grow(&mut self, columns: u16, active_rows: usize, limits: ScrollbackLimits) -> usize {
        let effective = Self::effective_limits(columns, active_rows, limits);
        let last = self.pages.back_mut().expect("a screen has pages");
        if last.rows < last.capacity.rows {
            last.rows += 1;
            return self.prune(
                active_rows,
                ScrollbackLimits {
                    bytes: None,
                    ..effective
                },
            );
        }

        let capacity = PageCapacity::initial(columns).expect("validated screen dimensions");
        let standard_bytes = PageCapacity::STANDARD
            .layout()
            .unwrap()
            .allocation_bytes(false);
        let mut removed = 0;
        if self.pages.len() > 1
            && effective
                .bytes
                .is_some_and(|limit| self.allocation_bytes() + standard_bytes > limit)
        {
            let first_rows = usize::from(self.pages.front().unwrap().rows);
            if self.total_rows() - first_rows + 1 >= active_rows {
                self.pages.pop_front();
                removed += first_rows;
            }
        }
        self.append(capacity, 1);
        removed
            + self.prune(
                active_rows,
                ScrollbackLimits {
                    bytes: None,
                    ..effective
                },
            )
    }

    /// Remove only pages lying wholly before the active boundary.
    pub fn prune(&mut self, active_rows: usize, limits: ScrollbackLimits) -> usize {
        let mut removed = 0;
        loop {
            let history = self.total_rows().saturating_sub(active_rows);
            let exceeded = limits.lines.is_some_and(|limit| history > limit)
                || limits
                    .bytes
                    .is_some_and(|limit| self.allocation_bytes() > limit);
            let first = self.pages.front().expect("a screen has pages");
            if !exceeded || usize::from(first.rows) > history || self.pages.len() == 1 {
                break;
            }
            removed += usize::from(self.pages.pop_front().unwrap().rows);
        }
        removed
    }

    /// Physically erase a prefix, preserving the retained page's capacity.
    pub fn remove_prefix(&mut self, mut count: usize) {
        assert!(count < self.total_rows());
        while count > 0 {
            let page = self.pages.front_mut().unwrap();
            if count >= usize::from(page.rows) {
                count -= usize::from(self.pages.pop_front().unwrap().rows);
            } else {
                page.rows -= count as u16;
                page.layout_generation = page.layout_generation.wrapping_add(1);
                count = 0;
            }
        }
    }

    pub fn truncate(&mut self, rows: usize) {
        assert!(rows > 0 && rows <= self.total_rows());
        let mut remove = self.total_rows() - rows;
        while remove > 0 {
            let page = self.pages.back_mut().unwrap();
            if remove >= usize::from(page.rows) {
                remove -= usize::from(self.pages.pop_back().unwrap().rows);
            } else {
                page.rows -= remove as u16;
                page.layout_generation = page.layout_generation.wrapping_add(1);
                remove = 0;
            }
        }
    }

    /// Copy allocation metadata for a detached viewport, preserving its page
    /// boundaries without allocating the live page's resource tables.
    pub fn clone_range(&self, start: usize, rows: usize) -> Self {
        let mut result = Self::default();
        let mut offset = 0;
        for page in &self.pages {
            let end = offset + usize::from(page.rows);
            let count = end.min(start + rows).saturating_sub(offset.max(start));
            if count > 0 {
                result.pages.push_back(Page {
                    capacity: page.capacity,
                    columns: page.columns,
                    rows: count as u16,
                    serial: result.next_serial,
                    layout_generation: 0,
                    styles: StyleAdmission::default(),
                    links: HyperlinkAdmission::default(),
                    graphemes: GraphemeAdmission::default(),
                });
                result.next_serial = result.next_serial.wrapping_add(1);
            }
            offset = end;
        }
        assert_eq!(result.total_rows(), rows);
        result
    }

    /// Emit a reflow row using the current source page's native capacity.
    pub fn reflow_row(&mut self, capacity: PageCapacity) {
        if let Some(last) = self.pages.back_mut()
            && last.rows < last.capacity.rows
        {
            last.rows += 1;
        } else {
            self.append(capacity, 1);
        }
    }

    pub fn resize_columns(&mut self, columns: u16, spacer_heads: &[bool]) {
        let old = std::mem::take(&mut self.pages);
        for (index, mut page) in old.into_iter().enumerate() {
            if columns <= page.columns || (columns <= page.capacity.cols && !spacer_heads[index]) {
                page.columns = columns;
                self.pages.push_back(page);
                continue;
            }
            let capacity = page.adjusted_capacity(columns, false);
            let mut remaining = page.rows;
            if let Some(previous) = self.pages.back_mut() {
                // ponytail: backfill assumes resources fit; integrate managed
                // resource exhaustion with the live resource mutation hooks.
                let count = remaining.min(previous.capacity.rows - previous.rows);
                previous.rows += count;
                remaining -= count;
            }
            while remaining > 0 {
                let count = remaining.min(capacity.rows);
                self.append(capacity, count);
                remaining -= count;
            }
        }
    }

    /// Safely widen a restored narrow page when an edit reaches outside it.
    pub fn extend_page(
        &mut self,
        row: usize,
        columns: u16,
        spacer_head: bool,
    ) -> std::ops::Range<usize> {
        let mut start = 0;
        let index = self
            .pages
            .iter()
            .position(|page| {
                if row < start + usize::from(page.rows) {
                    true
                } else {
                    start += usize::from(page.rows);
                    false
                }
            })
            .expect("row is inside the page list");
        let page = &self.pages[index];
        let end = start + usize::from(page.rows);
        if columns > page.columns {
            let page = self.pages.remove(index).unwrap();
            let mut expanded = Self {
                pages: [page].into(),
                next_serial: self.next_serial,
                identity: ListIdentity::default(),
            };
            expanded.resize_columns(columns, &[spacer_head]);
            self.next_serial = expanded.next_serial;
            for page in expanded.pages.into_iter().rev() {
                self.pages.insert(index, page);
            }
        }
        start..end
    }
}
