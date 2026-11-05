//! Opportunistic cold-history cell storage. Resources and row identities stay native.
use std::{
    ops::{Deref, DerefMut},
    sync::OnceLock,
};

use crate::{Terminal, packed::Cell, page_list::PageList};

/// The common resident path retains an ordinary Vec. Cold reads initialize a
/// cache once, so borrowed row slices stay valid until exclusive page access.
#[derive(Clone, Debug, Default)]
pub(crate) struct CellStorage {
    resident: Vec<Cell>,
    cold: Option<Box<ColdCells>>,
}

#[derive(Clone, Debug)]
struct ColdCells {
    encoded: Box<[u8]>,
    cells: usize,
    restored: OnceLock<Vec<Cell>>,
}

fn restore(encoded: &[u8], cells: usize) -> Vec<Cell> {
    let mut result = vec![Cell::default(); cells];
    let size = lz4_flex::block::decompress_into(encoded, bytemuck::cast_slice_mut(&mut result))
        .expect("internally compressed cell buffer is valid");
    assert_eq!(size, cells * size_of::<Cell>());
    result
}

impl From<Vec<Cell>> for CellStorage {
    fn from(resident: Vec<Cell>) -> Self {
        Self {
            resident,
            cold: None,
        }
    }
}

impl Deref for CellStorage {
    type Target = Vec<Cell>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        if self.resident.is_empty() && self.cold.is_some() {
            return self.restore_shared();
        }
        &self.resident
    }
}

impl DerefMut for CellStorage {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        if self.resident.is_empty() && self.cold.is_some() {
            self.restore_exclusive();
        }
        &mut self.resident
    }
}

impl CellStorage {
    #[cold]
    #[inline(never)]
    fn restore_shared(&self) -> &Vec<Cell> {
        let cold = self.cold.as_ref().unwrap();
        cold.restored
            .get_or_init(|| restore(&cold.encoded, cold.cells))
    }

    #[cold]
    #[inline(never)]
    fn restore_exclusive(&mut self) {
        let ColdCells {
            encoded,
            cells,
            restored,
        } = *self.cold.take().unwrap();
        self.resident = restored
            .into_inner()
            .unwrap_or_else(|| restore(&encoded, cells));
    }

    pub(crate) fn storage_bytes(&self) -> usize {
        self.resident.capacity() * size_of::<Cell>()
            + self.cold.as_ref().map_or(0, |cold| {
                size_of::<ColdCells>()
                    + cold.encoded.len()
                    + cold
                        .restored
                        .get()
                        .map_or(0, |cells| cells.capacity() * size_of::<Cell>())
            })
    }

    pub(crate) fn slots(&self) -> usize {
        self.cold
            .as_ref()
            .map_or(self.resident.capacity(), |cold| cold.cells)
    }

    fn needs_compression(&self) -> bool {
        self.cold
            .as_ref()
            .is_none_or(|cold| cold.restored.get().is_some())
    }

    fn compress(&mut self) -> bool {
        if let Some(cold) = &mut self.cold {
            // Shared reads cannot mutate cells; their cache still matches the
            // retained encoding. Exclusive access lets us reclaim it directly.
            return cold.restored.take().is_some();
        }
        let raw = bytemuck::cast_slice(&self.resident);
        // Bound temporary memory and lock hold time even for extremely wide
        // restored pages. Ordinary native cell buffers are below 400 KiB.
        if raw.is_empty() || raw.len() > 1024 * 1024 {
            return false;
        }
        let encoded = lz4_flex::block::compress(raw);
        if encoded.len() + size_of::<ColdCells>() >= self.storage_bytes() {
            return false;
        }
        let cold = ColdCells {
            encoded: encoded.into_boxed_slice(),
            cells: self.resident.len(),
            restored: OnceLock::new(),
        };
        self.cold = Some(Box::new(cold));
        self.resident = Vec::new();
        true
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Compression {
    pending: bool,
    restart: bool,
    compressed: bool,
    cursor: usize,
    row: usize,
    layout: (u64, u64, Option<u64>),
}

impl Compression {
    /// Partial trims retain page serials but change the running row offset.
    pub(crate) fn invalidate(&mut self) {
        *self = Self {
            pending: true,
            ..Default::default()
        };
    }
}

impl PageList {
    pub(crate) fn request_compression(&mut self) {
        self.compression.pending = true;
        self.compression.restart = true;
    }

    fn compress_step(&mut self, active: usize, viewport_offset: usize) -> bool {
        if !self.compression.pending {
            return false;
        }
        let layout = (
            self.identity(),
            self.next_serial,
            self.pages.front().map(|page| page.serial),
        );
        let state = &mut self.compression;
        if state.layout != layout {
            *state = Compression {
                pending: true,
                layout,
                ..Default::default()
            };
        }
        if state.cursor == 0 {
            state.restart = false;
        }
        let history = self
            .pages
            .iter()
            .map(|page| usize::from(page.rows))
            .sum::<usize>()
            .saturating_sub(active);
        let viewport_start = history.saturating_sub(viewport_offset);
        let viewport_end = viewport_start.saturating_add(active);
        // The metadata sum above does not materialize cells. Inspect at most
        // eight candidates and attempt at most one codec call after that scan.
        for _ in 0..8 {
            let Some(page) = self.pages.get_mut(state.cursor) else {
                break;
            };
            let end = state.row + usize::from(page.rows);
            if end > history {
                break;
            }
            let visible = state.row < viewport_end && end > viewport_start;
            state.cursor += 1;
            state.row = end;
            if !visible && page.cells.needs_compression() {
                state.compressed |= page.cells.compress();
                return true;
            }
        }
        if self
            .pages
            .get(state.cursor)
            .is_some_and(|page| state.row + usize::from(page.rows) <= history)
        {
            return true;
        }
        if state.compressed || state.restart {
            state.cursor = 0;
            state.row = 0;
            state.compressed = false;
            state.restart = false;
            return true;
        }
        state.pending = false;
        state.cursor = 0;
        state.row = 0;
        false
    }
}

impl Terminal {
    /// Enable opportunistic in-memory compression. Hosts call the bounded idle
    /// step; the setting is preserved by reset but does not enter snapshots.
    /// Disabling stops new compression; existing cold cells restore on access.
    pub fn set_scrollback_compression(&mut self, enabled: bool) {
        self.scrollback_compression = enabled;
        if enabled {
            self.request_scrollback_compression();
        }
    }

    pub fn scrollback_compression(&self) -> bool {
        self.scrollback_compression
    }

    /// Arm another pass after output or a host operation that read cold history.
    pub fn request_scrollback_compression(&mut self) {
        if self.scrollback_compression {
            self.primary.pages.request_compression();
        }
    }

    /// Compress at most one cold historical cell buffer. Returns whether the
    /// host should schedule another idle step. It never changes terminal content.
    pub fn compress_scrollback_step(&mut self) -> bool {
        self.scrollback_compression
            && self
                .primary
                .pages
                .compress_step(self.primary.height, self.primary.viewport_offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScrollbackLimits, Selection, snapshot};

    fn filled() -> Terminal {
        let mut terminal = Terminal::with_limits(80, 4, ScrollbackLimits::default());
        terminal.set_scrollback_compression(true);
        for index in 0..2400 {
            terminal.feed(format!("\x1b[{}m\x1b]8;;https://example.com/{index}\x07line {index:04} 界e\u{301}\x1b]8;;\x07\x1b[m\r\n", 31 + index % 6).as_bytes());
        }
        terminal
    }

    fn compress(terminal: &mut Terminal) {
        terminal.request_scrollback_compression();
        for _ in 0..10000 {
            if !terminal.compress_scrollback_step() {
                return;
            }
        }
        panic!("compression did not finish");
    }

    fn cold(terminal: &Terminal) -> usize {
        terminal
            .primary
            .pages
            .pages
            .iter()
            .filter(|page| {
                page.cells
                    .cold
                    .as_ref()
                    .is_some_and(|cold| cold.restored.get().is_none())
            })
            .count()
    }

    #[test]
    fn viewport_reads_only_visible_rows_without_restoring_cold_history() {
        let mut terminal = filled();
        let expected = terminal
            .primary
            .all_rows()
            .map(|row| (row.id, terminal.primary.row_text(row)))
            .collect::<Vec<_>>();
        let history = terminal.primary.history_len();
        let height = terminal.primary.height();
        for offset in [0, history / 2, history, history + height] {
            terminal.primary.viewport_offset = offset;
            compress(&mut terminal);
            let start = history.saturating_sub(offset);
            let mut row = 0;
            let untouched = terminal
                .primary
                .pages
                .pages
                .iter()
                .filter_map(|page| {
                    let end = row + usize::from(page.rows);
                    let outside = end <= start || row >= start + height;
                    row = end;
                    (outside
                        && page
                            .cells
                            .cold
                            .as_ref()
                            .is_some_and(|cold| cold.restored.get().is_none()))
                    .then_some(page.serial)
                })
                .collect::<Vec<_>>();
            assert!(!untouched.is_empty());
            let bytes = terminal.primary.owned_bytes();
            let actual = terminal
                .primary
                .viewport()
                .map(|row| (row.id, terminal.primary.row_text(row)))
                .collect::<Vec<_>>();
            assert_eq!(actual, expected[start..start + height], "offset {offset}");
            for page in &terminal.primary.pages.pages {
                if untouched.contains(&page.serial) {
                    assert!(
                        page.cells.cold.as_ref().unwrap().restored.get().is_none(),
                        "viewport offset {offset} restored unrelated page {}",
                        page.serial
                    );
                }
            }
            if offset == 0 {
                assert_eq!(terminal.primary.owned_bytes(), bytes);
            }
        }
    }

    #[test]
    fn compression_preserves_words_and_reuses_immutable_restore_encoding() {
        let original: Vec<_> = (0..10000)
            .map(|n| Cell::from_bits((n % 100) << 2))
            .collect();
        let mut cells = CellStorage::from(original.clone());
        let before = cells.storage_bytes();
        assert!(cells.compress());
        assert!(cells.storage_bytes() < before / 2);
        let encoded = cells.cold.as_ref().unwrap().encoded.as_ptr();
        assert_eq!(cells.as_slice(), original);
        assert!(cells.storage_bytes() > before);
        assert!(cells.compress());
        assert_eq!(cells.cold.as_ref().unwrap().encoded.as_ptr(), encoded);
        cells[0] = Cell::from_bits(42 << 2);
        assert!(cells.cold.is_none());
        assert_eq!(cells[0].codepoint(), Some('*'));
        assert_eq!(&cells[1..], &original[1..]);
    }

    #[test]
    fn compression_leaves_incompressible_and_oversized_buffers_resident() {
        let mut random = 0x123456789abcdef0u64;
        let mut cells = CellStorage::from(
            (0..8192)
                .map(|_| {
                    random ^= random << 13;
                    random ^= random >> 7;
                    random ^= random << 17;
                    bytemuck::cast::<u64, Cell>(random)
                })
                .collect::<Vec<_>>(),
        );
        let original = cells.resident.clone();
        assert!(!cells.compress());
        assert_eq!(cells.as_slice(), original);
        let mut cells = CellStorage::from(vec![Cell::default(); 1024 * 1024 / 8 + 1]);
        assert!(!cells.compress());
    }

    #[test]
    fn compression_is_bounded_skips_viewport_and_preserves_snapshot_and_selection() {
        let mut terminal = filled();
        let first = terminal.primary.point(0, 0).unwrap();
        let last = terminal.primary.point(1, 15).unwrap();
        let tracked = terminal.primary.track(first);
        terminal.primary.selection = Some(Selection {
            start: first,
            end: last,
            rectangular: false,
        });
        let selected = terminal.primary.selection_text();
        let expected = snapshot::encode_to_vec(&terminal).unwrap();
        let identities = terminal
            .primary
            .pages
            .pages
            .iter()
            .map(|page| (page.serial, page.row_ids.clone()))
            .collect::<Vec<_>>();
        let generation = terminal.generation;
        let bytes = terminal.primary.owned_bytes();
        terminal.primary.viewport_offset = terminal.primary.history_len();
        let first_serial = terminal.primary.pages.pages[0].serial;
        let mut previous = 0;
        loop {
            let pending = terminal.compress_scrollback_step();
            let now = cold(&terminal);
            assert!(now <= previous + 1);
            previous = now;
            if !pending {
                break;
            }
        }
        assert!(cold(&terminal) > 0);
        assert!(terminal.primary.owned_bytes() < bytes);
        assert!(
            terminal
                .primary
                .pages
                .pages
                .iter()
                .find(|page| page.serial == first_serial)
                .unwrap()
                .cells
                .cold
                .is_none()
        );
        assert_eq!(terminal.primary.resolve(tracked), Some(first));
        assert_eq!(terminal.primary.selection_text(), selected);
        assert_eq!(terminal.generation, generation);
        assert_eq!(
            identities,
            terminal
                .primary
                .pages
                .pages
                .iter()
                .map(|page| (page.serial, page.row_ids.clone()))
                .collect::<Vec<_>>()
        );
        assert_eq!(snapshot::encode_to_vec(&terminal).unwrap(), expected);
        compress(&mut terminal);
        assert!(cold(&terminal) > 0);
    }

    #[test]
    fn compression_survives_clone_reflow_unscroll_and_history_lifecycle() {
        let mut terminal = filled();
        compress(&mut terminal);
        let mut plain = terminal.clone();
        plain.set_scrollback_compression(false);
        // Materialize one clone without changing the source's cold buffers.
        let expected = snapshot::encode_to_vec(&plain).unwrap();
        assert!(cold(&terminal) > 0);
        assert_eq!(snapshot::encode_to_vec(&terminal).unwrap(), expected);
        compress(&mut terminal);
        terminal.resize(37, 7);
        plain.resize(37, 7);
        assert_eq!(
            snapshot::encode_to_vec(&terminal).unwrap(),
            snapshot::encode_to_vec(&plain).unwrap()
        );
        compress(&mut terminal);
        for input in [
            b"\x1b[3+T".as_slice(),
            b"\x1b[?1049hother\x1b[?1049l",
            b"\x1b[3J",
        ] {
            terminal.feed(input);
            plain.feed(input);
            assert_eq!(
                snapshot::encode_to_vec(&terminal).unwrap(),
                snapshot::encode_to_vec(&plain).unwrap()
            );
            compress(&mut terminal);
        }
        terminal.feed(b"\x1bc");
        assert!(terminal.scrollback_compression());
        assert!(!terminal.compress_scrollback_step());
    }

    #[test]
    fn compression_disabled_policy_and_small_budgets_preserve_retention() {
        let mut terminal = filled();
        terminal.set_scrollback_compression(false);
        assert!(!terminal.compress_scrollback_step());
        assert_eq!(cold(&terminal), 0);
        let mut plain = terminal.clone();
        terminal.set_scrollback_compression(true);
        compress(&mut terminal);
        let limits = ScrollbackLimits {
            bytes: Some(1),
            lines: Some(1),
        };
        terminal.set_limits(limits);
        plain.set_limits(limits);
        for _ in 0..2000 {
            terminal.feed(b"new line\r\n");
            plain.feed(b"new line\r\n");
        }
        assert_eq!(
            snapshot::encode_to_vec(&terminal).unwrap(),
            snapshot::encode_to_vec(&plain).unwrap()
        );
    }

    #[test]
    fn compression_restarts_after_partial_page_trims_without_serial_changes() {
        let mut terminal = filled();
        assert!(terminal.compress_scrollback_step());
        let first_serial = terminal.primary.pages.pages[0].serial;
        terminal.primary.pages.remove_prefix(1);
        assert_eq!(terminal.primary.pages.pages[0].serial, first_serial);
        assert_eq!(terminal.primary.pages.compression.cursor, 0);
        assert!(terminal.compress_scrollback_step());
        let rows = terminal.primary.pages.total_rows();
        terminal.primary.pages.truncate(rows - 1);
        assert_eq!(terminal.primary.pages.compression.cursor, 0);
        while terminal.compress_scrollback_step() {
            let state = &terminal.primary.pages.compression;
            assert_eq!(
                state.row,
                terminal
                    .primary
                    .pages
                    .pages
                    .iter()
                    .take(state.cursor)
                    .map(|page| usize::from(page.rows))
                    .sum::<usize>()
            );
        }
    }
}
