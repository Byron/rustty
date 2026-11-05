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
    /// Pull the newest history row into the scrolling region. Whole rows keep
    /// their identities; partial-width edits retain the existing screen rows.
    pub(crate) fn unscroll_history_row(&mut self, margins: crate::Margins, background: Color) {
        let history = self.history_len();
        debug_assert!(history != 0);
        let total = self.pages.total_rows();
        let full_width = margins.left == 0 && margins.right + 1 == self.columns;
        if full_width {
            // Move the incoming row past the unchanged rows above the margin.
            if margins.top != 0 {
                let incoming = RowCopy::from_view(self.physical_row(history - 1));
                let blank = self.next_row_id();
                self.rotate_rows(
                    history - 1,
                    history + margins.top - 1,
                    true,
                    blank,
                    background,
                    usize::MAX,
                );
                self.install_row(history + margins.top - 1, incoming, usize::MAX);
            }
            // Removing the region's bottom row moves the history boundary back
            // one, without changing the position of rows below the margin.
            let removed = self.physical_row(history + margins.bottom).id;
            if margins.bottom + 1 != self.height {
                let blank = self.next_row_id();
                self.rotate_rows(
                    history + margins.bottom,
                    total - 1,
                    true,
                    blank,
                    background,
                    usize::MAX,
                );
            }
            self.pages.truncate(total - 1);
            self.discard_row(removed);
        } else {
            let incoming = RowCopy::from_view(self.physical_row(history - 1));
            let blank = self.next_row_id();
            // Remove the history row while leaving every active row unchanged.
            self.rotate_rows(history - 1, total - 1, true, blank, background, usize::MAX);
            self.pages.truncate(total - 1);
            for y in margins.top..=margins.bottom {
                self.prepare_row_shift(
                    y,
                    margins.left,
                    margins.right,
                    margins.right + 1 == self.columns,
                );
            }
            for y in (margins.top + 1..=margins.bottom).rev() {
                self.copy_row_cells(y - 1, y, margins.left, margins.right + 1, background);
            }
            let destination = self.history_len() + margins.top;
            let source_id = incoming.id;
            let mut spans = Vec::new();
            for col in margins.left..=margins.right {
                let copy = incoming.cells.get(col).filter(|copy| {
                    !(col == margins.left && copy.cell.width() == 0
                        || col == margins.right && copy.cell.width() == 2)
                        && copy.span.as_ref().is_none_or(|part| {
                            let start = col.saturating_sub(usize::from(part.offset));
                            start >= margins.left
                                && start + usize::from(part.width) <= margins.right + 1
                        })
                });
                let mut copy = copy
                    .cloned()
                    .unwrap_or_else(|| CellCopy::plain(Cell::blank(background)));
                if let Some(part) = copy.span.take() {
                    spans.push((col, part));
                }
                let _ = self.install_cell(destination, col, copy, false);
            }
            let (index, row) = self.locate(destination);
            for (col, part) in spans {
                self.pages.pages[index].set_span_part(row, col, part);
            }
            self.pages.pages[index].repair_wide(row, background);
            self.pages.pages[index].refresh_charge();
            self.tmux_capture_write(margins.top, margins.right + 1);
            let destination_id = self.row(margins.top).id;
            for point in self.grid_points_mut() {
                if point.row == source_id && (margins.left..=margins.right).contains(&point.col) {
                    point.row = destination_id;
                }
            }
            self.graphics.restore_row_segment(
                source_id,
                destination_id,
                margins.left..=margins.right,
            );
            self.discard_row(source_id);
        }
        self.viewport_offset = self
            .viewport_offset
            .saturating_sub(1)
            .min(self.history_len());
        let remaining = self.history_len();
        if let Some(history) = &mut self.tmux_history {
            history.scrolled = history.scrolled.saturating_sub(1).min(remaining);
        }
        // A restored history page can be narrower than the logical screen.
        // Preserve the cursor coordinate before reacquiring its page resources.
        if self.cursor.col >= self.row_columns(self.cursor.row) {
            self.extend_physical_row(self.cursor.row, self.columns);
        }
        self.sync_cursor_resources();
    }

    /// Native allocated row slots/bytes, cell slots/bytes, auxiliary arenas/bytes.
    /// Row bytes include stable row IDs and content revisions. Auxiliary arenas
    /// include page objects, styles, hyperlinks and graphemes. Graphics and
    /// Screen-owned maps are separate.
    pub fn owned_storage_parts(&self) -> [usize; 6] {
        let mut parts = [0; 6];
        for page in &self.pages.pages {
            let rows = page.headers.capacity() * size_of::<RowHeader>()
                + (page.row_ids.capacity() + page.row_revisions.capacity()) * size_of::<u64>();
            let cells = page.cells.storage_bytes();
            parts[0] += page.headers.capacity();
            parts[1] += rows;
            parts[2] += page.cells.slots();
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
