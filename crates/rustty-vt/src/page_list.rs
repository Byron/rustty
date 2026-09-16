//! Authoritative native page allocation boundaries for owned screen rows.
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

pub(crate) use crate::page::Page;
use crate::page_layout::PageCapacity;
use crate::screen::Color;
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct PageList {
    pub pages: VecDeque<Page>,
    pub(crate) next_serial: u64,
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

    pub fn page_at(&self, row: usize) -> (&Page, usize) {
        let (index, row) = self.locate(row);
        (&self.pages[index], row)
    }

    pub fn page_index(&self, row: usize) -> usize {
        self.locate(row).0
    }

    pub fn locate(&self, mut row: usize) -> (usize, usize) {
        for (index, page) in self.pages.iter().enumerate() {
            if row < usize::from(page.rows) {
                return (index, row);
            }
            row -= usize::from(page.rows);
        }
        panic!("row is outside the page list");
    }

    /// Locate a row by its distance from the last row, skipping history pages.
    pub fn page_index_from_end(&self, mut distance: usize) -> usize {
        for (index, page) in self.pages.iter().enumerate().rev() {
            if distance < usize::from(page.rows) {
                return index;
            }
            distance -= usize::from(page.rows);
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
        let serial = self.fresh_serial();
        self.pages.push_back(Page::new(capacity, rows, serial));
    }

    pub fn fresh_serial(&mut self) -> u64 {
        let serial = self.next_serial;
        self.next_serial = self
            .next_serial
            .checked_add(1)
            .expect("page identities exhausted");
        serial
    }

    pub fn push(&mut self, mut page: Page) {
        page.serial = self.fresh_serial();
        self.pages.push_back(page);
    }

    pub fn prepend_page(&mut self, mut page: Page) {
        page.serial = self.fresh_serial();
        self.pages.push_front(page);
    }

    pub fn owned_history_bytes(&self, active_rows: usize) -> usize {
        let history = self.total_rows().saturating_sub(active_rows);
        let mut end = 0;
        self.pages
            .iter()
            .take_while(|page| {
                end += usize::from(page.rows);
                end <= history
            })
            .map(Page::storage_bytes)
            .sum()
    }

    pub fn effective_limits(
        columns: u16,
        active_rows: usize,
        limits: ScrollbackLimits,
    ) -> ScrollbackLimits {
        let capacity = PageCapacity::initial(columns).expect("validated screen dimensions");
        let minimum_lines = usize::from(capacity.rows);
        ScrollbackLimits {
            bytes: limits.bytes.map(|bytes| {
                let standard_bytes = PageCapacity::STANDARD
                    .layout()
                    .expect("standard page layout")
                    .total_size;
                let minimum_bytes =
                    standard_bytes * (active_rows.max(1).div_ceil(minimum_lines) + 1);
                bytes.max(minimum_bytes)
            }),
            lines: limits.lines.map(|lines| lines.max(minimum_lines)),
        }
    }

    /// Expose initialized storage, recycling one eligible historical page at growth.
    pub fn grow(
        &mut self,
        columns: u16,
        active_rows: usize,
        limits: ScrollbackLimits,
        memory: Option<usize>,
        id: u64,
        background: Color,
    ) -> Vec<u64> {
        let effective = Self::effective_limits(columns, active_rows, limits);
        let mut removed = Vec::new();
        if self
            .pages
            .back()
            .is_some_and(|page| page.rows < page.capacity.rows)
        {
            self.pages.back_mut().unwrap().expose(id, background);
        } else {
            let capacity = PageCapacity::initial(columns).expect("validated screen dimensions");
            let history = (self.total_rows() + 1).saturating_sub(active_rows);
            let recycle = self.pages.len() > 1
                && self.pages.front().is_some_and(|page| {
                    usize::from(page.rows) <= history
                        && (effective.bytes.is_some_and(|limit| {
                            self.allocation_bytes()
                                + capacity.layout().unwrap().allocation_bytes(false)
                                > limit
                        }) || effective.lines.is_some_and(|limit| history > limit)
                            || memory.is_some_and(|limit| {
                                self.owned_history_bytes(active_rows.saturating_sub(1)) > limit
                            }))
                });
            let mut spare = if recycle {
                let page = self.pages.pop_front().unwrap();
                removed.extend_from_slice(&page.row_ids[..usize::from(page.rows)]);
                page.recyclable(capacity).then_some(page)
            } else {
                None
            };
            let serial = self.fresh_serial();
            let mut page = spare
                .take()
                .map(|mut page| {
                    page.recycle(capacity, serial);
                    page
                })
                .unwrap_or_else(|| Page::new(capacity, 0, serial));
            page.expose(id, background);
            self.pages.push_back(page);
        }
        removed.extend(self.prune(
            active_rows,
            ScrollbackLimits {
                bytes: None,
                ..effective
            },
            memory,
        ));
        removed
    }

    /// Return discarded identities before releasing complete historical pages.
    pub fn prune(
        &mut self,
        active_rows: usize,
        limits: ScrollbackLimits,
        memory: Option<usize>,
    ) -> Vec<u64> {
        let mut removed = Vec::new();
        loop {
            let history = self.total_rows().saturating_sub(active_rows);
            let exceeded = limits.lines.is_some_and(|limit| history > limit)
                || limits
                    .bytes
                    .is_some_and(|limit| self.allocation_bytes() > limit)
                || memory.is_some_and(|limit| self.owned_history_bytes(active_rows) > limit);
            let Some(first) = self.pages.front() else {
                break;
            };
            if !exceeded || usize::from(first.rows) > history || self.pages.len() == 1 {
                break;
            }
            let page = self.pages.pop_front().unwrap();
            removed.extend_from_slice(&page.row_ids[..usize::from(page.rows)]);
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
                page.remove_prefix(count);
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
                page.truncate(usize::from(page.rows) - remove);
                remove = 0;
            }
        }
    }

    pub fn clone_range(&self, start: usize, rows: usize) -> Self {
        let mut result = Self {
            next_serial: self.next_serial,
            ..Self::default()
        };
        let mut offset = 0;
        for page in &self.pages {
            let end = offset + usize::from(page.rows);
            let count = end.min(start + rows).saturating_sub(offset.max(start));
            if count > 0 {
                let mut copy = page.clone();
                copy.remove_prefix(start.saturating_sub(offset));
                copy.truncate(count);
                result.pages.push_back(copy);
            }
            offset = end;
        }
        assert_eq!(result.total_rows(), rows);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growth_reuses_the_evicted_standard_allocation_without_a_cache() {
        let columns = 80;
        let capacity = PageCapacity::initial(columns).unwrap();
        let mut pages = PageList::new(columns, 2);
        let limits = ScrollbackLimits {
            bytes: Some(1),
            lines: None,
        };
        for id in 2..u64::from(capacity.rows) * 2 {
            pages.grow(columns, 2, limits, None, id, Color::Default);
        }
        assert_eq!(pages.pages.len(), 2);
        let pointer = pages.pages[0].cells.as_ptr();
        let old_serial = pages.pages[0].serial;
        let removed = pages.grow(columns, 2, limits, None, 100_000, Color::Default);
        assert_eq!(removed.len(), usize::from(capacity.rows));
        assert_eq!(pages.pages.len(), 2);
        let reused = pages.pages.back().unwrap();
        assert_eq!(reused.cells.as_ptr(), pointer);
        assert_ne!(reused.serial, old_serial);
        assert_eq!(reused.rows, 1);
        assert_eq!(reused.row_ids[0], 100_000);
        assert!(reused.cells.iter().all(|cell| cell.bits() == 0));
    }

    #[test]
    fn growth_preserves_native_line_and_byte_floors() {
        let columns = 1024;
        let minimum = usize::from(PageCapacity::initial(columns).unwrap().rows);
        for lines in [
            None,
            Some(0),
            Some(minimum - 1),
            Some(minimum),
            Some(minimum + 1),
        ] {
            let limits = ScrollbackLimits { bytes: None, lines };
            let mut pages = PageList::new(columns, 2);
            for _ in 0..minimum {
                assert_eq!(
                    pages
                        .grow(columns, 2, limits, None, 0, Color::Default)
                        .len(),
                    0
                );
            }
            assert_eq!(pages.total_rows(), minimum + 2);
            assert_eq!(
                pages
                    .grow(columns, 2, limits, None, 0, Color::Default)
                    .len(),
                if lines.is_some_and(|limit| limit <= minimum) {
                    minimum
                } else {
                    0
                },
            );
            assert_eq!(
                pages
                    .grow(columns, 2, limits, None, 0, Color::Default)
                    .len(),
                if lines == Some(minimum + 1) {
                    minimum
                } else {
                    0
                },
            );
            assert_eq!(
                pages.total_rows(),
                if lines.is_none() { minimum + 4 } else { 4 }
            );
        }

        let standard_bytes = PageCapacity::STANDARD.layout().unwrap().total_size;
        for bytes in [
            None,
            Some(0),
            Some(standard_bytes),
            Some(3 * standard_bytes),
        ] {
            let limits = ScrollbackLimits { bytes, lines: None };
            let mut pages = PageList::new(columns, 2);
            // The two-page floor permits filling both existing allocations;
            // only requesting a third page can recycle the oldest one.
            for _ in 2..2 * minimum {
                assert_eq!(
                    pages
                        .grow(columns, 2, limits, None, 0, Color::Default)
                        .len(),
                    0
                );
            }
            let recycled = bytes.is_some_and(|limit| limit < 3 * standard_bytes);
            assert_eq!(
                pages
                    .grow(columns, 2, limits, None, 0, Color::Default)
                    .len(),
                if recycled { minimum } else { 0 }
            );
            assert_eq!(pages.pages.len(), if recycled { 2 } else { 3 });
        }
        assert_eq!(
            PageList::effective_limits(
                columns,
                minimum + 1,
                ScrollbackLimits {
                    bytes: Some(0),
                    lines: None
                },
            )
            .bytes,
            Some(3 * standard_bytes),
        );
    }

    #[test]
    fn reverse_lookup_matches_forward_after_page_changes() {
        let mut pages = PageList::default();
        let capacity = PageCapacity::initial(128).unwrap();
        for rows in [2, 1, 4] {
            pages.append(capacity, rows);
        }
        let compare = |pages: &PageList| {
            let total = pages.total_rows();
            for row in 0..total {
                assert_eq!(
                    pages.page_index_from_end(total - row - 1),
                    pages.page_index(row),
                    "row={row}, total={total}"
                );
            }
        };
        compare(&pages);
        pages.append(capacity, 1);
        compare(&pages);
        pages.remove_prefix(3);
        compare(&pages);
        pages.remove_prefix(2);
        compare(&pages);
    }
}
