use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TmuxHistory {
    pub limit: usize,
    pub enabled: bool,
    pub added: u32,
    pub collected: u32,
    pub generation: u32,
    #[serde(default)]
    pub scrolled: usize,
}

impl Screen {
    /// Native allocated row slots/bytes, cell slots/bytes, auxiliary arenas/bytes.
    /// Row bytes include stable row IDs; auxiliary arenas include page objects,
    /// styles, hyperlinks and graphemes. Graphics and Screen-owned maps are separate.
    pub fn owned_storage_parts(&self) -> [usize; 6] {
        let mut parts = [0; 6];
        for page in &self.pages.pages {
            let rows = page.headers.capacity() * size_of::<RowHeader>()
                + page.row_ids.capacity() * size_of::<u64>();
            let cells = page.cells.capacity() * size_of::<Cell>();
            parts[0] += page.headers.capacity();
            parts[1] += rows;
            parts[2] += page.cells.capacity();
            parts[3] += cells;
            parts[4] += 1;
            parts[5] += page.storage_bytes() - rows - cells;
        }
        parts
    }

    pub(crate) fn history_enabled(&self) -> bool {
        self.limits.bytes != Some(0)
            && self.memory_limit != Some(0)
            && self.tmux_history.is_none_or(|history| history.enabled)
    }

    pub(crate) fn tmux_collect_history(&mut self, all: bool) {
        let Some(history) = self.tmux_history else {
            return;
        };
        let size = self.history_len();
        if size == 0 || size < history.limit {
            return;
        }
        let count = if all {
            size - history.limit
        } else {
            history.limit / 10
        }
        .max(1)
        .min(size);
        let ids = self.history().take(count).map(|row| row.id).collect();
        self.pages.remove_prefix(count);
        self.discard_ids(ids);
        let history = self.tmux_history.as_mut().unwrap();
        history.collected = history.collected.wrapping_add(count as u32);
        history.scrolled = history.scrolled.min(size - count);
    }

    pub(crate) fn tmux_history_added(&mut self) {
        if self.tmux_history.is_some() && self.history_len() > 0 {
            let row = self.physical_row(self.history_len() - 1).id;
            if self.tmux_current_time != 0 {
                self.tmux_row_times.insert(row, self.tmux_current_time);
            } else {
                self.tmux_row_times.remove(&row);
            }
        }
        if let Some(history) = &mut self.tmux_history {
            history.added = history.added.wrapping_add(1);
            history.scrolled += 1;
        }
    }
}

impl crate::Terminal {
    pub fn trim_tmux_history_below_cursor(&mut self) {
        let screen = self.screen_mut();
        let count = screen
            .height
            .saturating_sub(screen.cursor.row + 1)
            .min(screen.history_len());
        if count == 0 {
            return;
        }
        let end = screen.pages.total_rows() - count;
        let ids = screen.all_rows().skip(end).map(|row| row.id).collect();
        screen.pages.truncate(end);
        screen.discard_ids(ids);
        screen.cursor.row += count;
        let remaining = screen.history_len();
        if let Some(history) = &mut screen.tmux_history {
            history.scrolled = history.scrolled.min(remaining);
        }
        screen.sync_cursor_resources();
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn clear_tmux_history(&mut self, hyperlinks: bool) {
        self.primary.clear_history();
        if hyperlinks {
            for screen in std::iter::once(&mut self.primary).chain(self.alternate.iter_mut()) {
                screen.end_hyperlink();
                if let Some(saved) = &mut screen.saved_cursor {
                    saved.cursor.hyperlink = None;
                }
                for page in &mut screen.pages.pages {
                    page.clear_links();
                }
            }
        }
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn set_tmux_time(&mut self, seconds: u64) {
        self.primary.tmux_current_time = seconds;
    }

    /// tmux retains the original GRID_HISTORY flag when changing its live limit.
    pub fn set_tmux_history_limit(&mut self, limit: usize) {
        match &mut self.primary.tmux_history {
            Some(history) => {
                history.limit = limit;
                self.primary.tmux_collect_history(true);
            }
            None => {
                self.primary.tmux_history = Some(TmuxHistory {
                    limit,
                    enabled: limit != 0,
                    ..TmuxHistory::default()
                })
            }
        }
        self.primary.limits.lines = Some(limit);
    }
}

impl Screen {
    pub fn tmux_row_time(&self, row: u64) -> u64 {
        self.tmux_row_times.get(&row).copied().unwrap_or(0)
    }
}
