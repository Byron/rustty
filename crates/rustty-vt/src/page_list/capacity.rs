//! Carry recent resource demand without retaining page allocations.
use crate::{
    page::{Page, PageResource, hyperlink_bytes_for_count, set_capacity_for_count},
    page_layout::PageCapacity,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct GrowHint {
    need: PageCapacity,
    used: u8,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct GrowHints {
    recent: [Option<GrowHint>; 3],
    next: usize,
}

pub(super) struct Growth {
    pub capacity: PageCapacity,
    pub hint: GrowHint,
}

impl GrowHints {
    /// Only the owner calls this, after the next page has been installed.
    pub fn push(&mut self, hint: GrowHint) {
        self.recent[self.next] = Some(hint);
        self.next = (self.next + 1) % self.recent.len();
    }

    pub fn next(&self, columns: u16, page: &Page) -> Growth {
        let standard = PageCapacity::initial(columns).expect("validated screen dimensions");
        let usage = page.usage();
        let mut need = standard;
        let mut used = 0;
        for resource in PageResource::ALL {
            let amount = resource.usage(usage);
            if amount.used != 0 {
                used |= 1 << resource as u8;
            }
            let scaled = if page.rows == 0 {
                amount.used
            } else {
                (amount.used * u64::from(standard.rows) / u64::from(page.rows)).min(amount.capacity)
            };
            let headroom = scaled + scaled / 2;
            let request = match resource {
                PageResource::Styles => set_capacity_for_count(headroom),
                PageResource::Links if scaled != 0 => hyperlink_bytes_for_count(headroom.max(32)),
                PageResource::Links => 0,
                _ => headroom,
            };
            let value = if scaled * 3 < amount.capacity {
                request
            } else {
                resource.get(page.capacity)
            };
            resource.set(
                &mut need,
                value.max(resource.get(standard)).min(resource.maximum()),
            );
        }
        let hint = GrowHint { need, used };
        let mut capacity = need;
        if let Some(newest) = self.recent[(self.next + self.recent.len() - 1) % self.recent.len()] {
            for resource in PageResource::ALL {
                // One light/empty page may be part of steady output. Two
                // consecutive empty pages discard older bursts independently.
                let carry = if (used | newest.used) & (1 << resource as u8) != 0 {
                    self.recent
                        .iter()
                        .flatten()
                        .map(|hint| resource.get(hint.need))
                        .max()
                        .unwrap()
                } else {
                    resource.get(newest.need)
                };
                let value = resource.get(capacity).max(carry);
                resource.set(&mut capacity, value);
            }
        }
        let standard_bytes = PageCapacity::STANDARD.layout().unwrap().total_size;
        let Ok(layout) = capacity.layout() else {
            return Growth {
                capacity: standard,
                hint,
            };
        };
        if layout.total_size > standard_bytes
            && let Some(fitted) = capacity.fit_rows(standard_bytes)
            && u64::from(fitted.rows) * layout.total_size as u64 * 10
                >= u64::from(capacity.rows) * standard_bytes as u64 * 9
        {
            capacity = fitted;
        }
        Growth { capacity, hint }
    }
}

#[cfg(test)]
#[path = "capacity_tests.rs"]
mod tests;
