use super::*;

#[derive(Clone, Copy)]
struct ReflowRow {
    id: u64,
    wrapped: bool,
    continuation: bool,
    semantic: SemanticContent,
}

#[derive(Clone, Copy)]
struct ReflowStyle {
    page: (u64, u64),
    source: u16,
    destination: u16,
}

impl Screen {
    // Reflow exposes fresh slots. Copy ordinary cells sharing a style as a
    // run, admitting that style once, like Ghostty's ReflowCursor::copyRun.
    // Resource growth and splitting stay in the scalar fallback.
    fn copy_reflow_run(
        &mut self,
        source: &Page,
        cells: &[Cell],
        col: usize,
        style_cache: &mut Option<ReflowStyle>,
    ) -> bool {
        let page = self.pages.pages.back_mut().unwrap();
        let source_id = cells[0].style_id();
        let id = if source_id == 0 {
            0
        } else if let Some(cached) = style_cache.filter(|cached| {
            cached.page == (page.serial, page.layout_generation) && cached.source == source_id
        }) {
            page.styles
                .retain_many(cached.destination, cells.len() as u16);
            cached.destination
        } else {
            let bytes = page.styles.storage_bytes();
            let Ok(id) = page
                .styles
                .acquire_with_id(*source.styles.get(source_id), source_id)
            else {
                return false;
            };
            page.styles.retain_many(id, (cells.len() - 1) as u16);
            if page.styles.storage_bytes() != bytes {
                page.refresh_charge();
            }
            *style_cache = Some(ReflowStyle {
                page: (page.serial, page.layout_generation),
                source: source_id,
                destination: id,
            });
            id
        };
        let row = usize::from(page.rows) - 1;
        let slot = page.slot(row, col);
        let destination = &mut page.cells[slot..slot + cells.len()];
        destination.copy_from_slice(cells);
        if id != source_id {
            for cell in destination {
                cell.set_style_id(id);
            }
        }
        page.mark_cell(row, page.cells[slot]);
        true
    }

    fn reflow_row_state(&mut self) -> ReflowRow {
        ReflowRow {
            id: self.next_row_id(),
            wrapped: false,
            continuation: false,
            semantic: SemanticContent::Output,
        }
    }

    fn apply_reflow_row(&mut self, state: ReflowRow) {
        let (index, row) = self.pages.locate_from_end(0);
        let page = &mut self.pages.pages[index];
        page.row_ids[row] = state.id;
        page.headers[row].set(RowHeader::WRAPPED, state.wrapped);
        page.headers[row].set(RowHeader::CONTINUATION, state.continuation);
        page.headers[row].set_semantic(state.semantic);
        page.headers[row].set(RowHeader::DIRTY, true);
    }

    fn reflow_expose(&mut self, capacity: crate::PageCapacity, id: u64, spare: &mut Option<Page>) {
        if self
            .pages
            .pages
            .back()
            .is_none_or(|page| page.rows == page.capacity.rows)
        {
            let serial = self.pages.fresh_serial();
            let reusable = spare.as_ref().is_some_and(|page| {
                page.capacity.styles == capacity.styles
                    && page.capacity.grapheme_bytes == capacity.grapheme_bytes
                    && page.capacity.hyperlink_bytes == capacity.hyperlink_bytes
                    && page.capacity.string_bytes == capacity.string_bytes
            });
            let page = if reusable {
                let mut page = spare.take().unwrap();
                page.recycle(capacity, serial);
                page
            } else {
                Page::new(capacity, 0, serial)
            };
            self.pages.pages.push_back(page);
        }
        self.pages
            .pages
            .back_mut()
            .unwrap()
            .expose(id, Color::Default);
    }

    fn ensure_reflow_row(
        &mut self,
        output: &[ReflowRow],
        line: ReflowRow,
        capacity: crate::PageCapacity,
        spare: &mut Option<Page>,
        exposed_rows: &mut usize,
    ) {
        while *exposed_rows <= output.len() {
            let index = *exposed_rows;
            let state = output.get(index).copied().unwrap_or(line);
            self.reflow_expose(capacity, state.id, spare);
            self.apply_reflow_row(state);
            *exposed_rows += 1;
        }
        self.apply_reflow_row(line);
    }

    pub(crate) fn resize(&mut self, cols: usize, rows: usize, reflow: bool) {
        self.tracked.prune();
        let viewport_top = (self.viewport_offset > 0).then(|| self.viewport_top());
        if self.viewport_offset > 0 && self.viewport_offset < self.history_len() {
            self.viewport_pin = viewport_top;
        }
        if self.viewport_pin.is_none() {
            self.viewport_pin = self.all_rows().next().map(|row| GridPoint {
                row: row.id,
                col: 0,
            });
        }
        let viewport_pinned = viewport_top.is_some() && viewport_top == self.viewport_pin;
        let viewport_at_top = self.viewport_offset > 0 && !viewport_pinned;
        if viewport_top.is_none() {
            self.viewport_pin_column = 0;
        }
        self.release_cursor_style();
        self.release_cursor_link();
        let old_cols = self.columns;
        let old_rows = self.height;
        let cursor_y = self.cursor.row;
        let old_cursor = GridPoint {
            row: self.row(cursor_y).id,
            col: self.cursor.col,
        };
        let cursor_index = self.history_len() + cursor_y;
        let mut saved_point = self.saved_cursor.as_ref().and_then(|saved| {
            (saved.cursor.row < self.height).then(|| GridPoint {
                row: self.row(saved.cursor.row).id,
                col: saved.cursor.col,
            })
        });
        let height_first = reflow && cols <= old_cols;
        if height_first {
            self.resize_height(old_cols, old_rows, rows, old_cursor, saved_point);
        }
        self.columns = cols;
        let mut mapped_cursor = old_cursor;
        if cols != old_cols && reflow {
            let height = if height_first { rows } else { old_rows };
            let active_start = self.pages.total_rows().saturating_sub(height);
            let old_wrapped = self
                .all_rows()
                .enumerate()
                .filter(|(i, row)| {
                    *i >= active_start && *i <= cursor_index && row.wrap_continuation
                })
                .count();
            let mut source_pages = std::mem::take(&mut self.pages);
            let first_capacity = source_pages
                .pages
                .front()
                .unwrap()
                .adjusted_capacity(cols as u16, false);
            self.pages.append(first_capacity, 1);
            let mut graphics_points: Vec<_> = self
                .graphics
                .placements
                .iter()
                .filter(|p| !p.virtual_placement && p.parent.is_none())
                .map(|p| (p.row, p.col))
                .collect();
            graphics_points.sort_unstable();
            graphics_points.dedup();
            let mut map = HashMap::<GridPointKey, GridPoint>::new();
            let mut wanted = Vec::new();
            let mut output = Vec::new();
            let mut line = self.reflow_row_state();
            self.apply_reflow_row(line);
            let mut exposed_rows = 1;
            let (mut x, mut pin_x, mut written_rows) = (0usize, 0usize, 0usize);
            let mut spare = None;
            while let Some(source_page) = source_pages.pages.pop_front() {
                let capacity = source_page.adjusted_capacity(cols as u16, true);
                // Source IDs are page-local; destination rebuilds can remap IDs.
                let mut style_cache = None;
                for source_row in 0..usize::from(source_page.rows) {
                    let old = source_page.row(source_row);
                    let unmanaged = !source_page.headers[source_row].has(RowHeader::MANAGED);
                    wanted.clear();
                    let mut used = if old.wrapped {
                        old.cells.len()
                    } else {
                        old.used()
                    };
                    let mut keep_pin = |point: &mut GridPoint| {
                        if point.row == old.id {
                            if point.col >= used {
                                point.col = point.col.min(cols - 1 - pin_x);
                            }
                            used = used.max(point.col + 1);
                            wanted.push(point.col);
                        }
                    };
                    if let Some(point) = &mut self.viewport_pin {
                        keep_pin(point);
                    }
                    for point in self.tracked.0.values_mut().flatten() {
                        keep_pin(point);
                    }
                    if let Some(selection) = &mut self.selection {
                        keep_pin(&mut selection.start);
                        keep_pin(&mut selection.end);
                    }
                    if let Some(point) = &mut saved_point {
                        keep_pin(point);
                    }
                    if old.id == old_cursor.row {
                        used = used.max(old_cursor.col + 1);
                        wanted.push(old_cursor.col);
                    }
                    if old.semantic != SemanticContent::Output {
                        used = used.max(1);
                    }
                    if used == 0 && old.wrap_continuation {
                        continue;
                    }
                    let graphics_start = graphics_points.partition_point(|p| p.0 < old.id);
                    wanted.extend(
                        graphics_points[graphics_start..]
                            .iter()
                            .take_while(|p| p.0 == old.id)
                            .map(|p| p.1),
                    );
                    wanted.sort_unstable();
                    wanted.dedup();
                    let mut next_point = 0;
                    let mut record = |old_col, point| {
                        if wanted.get(next_point) == Some(&old_col) {
                            map.insert((old.id, old_col), point);
                            next_point += 1;
                        }
                    };
                    line.semantic = old.semantic;
                    if used > 0 {
                        self.ensure_reflow_row(
                            &output,
                            line,
                            capacity,
                            &mut spare,
                            &mut exposed_rows,
                        );
                    }
                    let mut wide_tail = None;
                    let bulk_copy = wanted.is_empty()
                        && !source_page.headers[source_row].has(RowHeader::PLACEHOLDER);
                    let mut cells = old.cells.iter().take(used).enumerate();
                    while let Some((old_col, cell)) = cells.next() {
                        if cell.width() == 0 {
                            record(
                                old_col,
                                wide_tail.unwrap_or(GridPoint {
                                    row: line.id,
                                    col: x.saturating_sub(1),
                                }),
                            );
                            continue;
                        }
                        if cell.spacer_head() {
                            record(
                                old_col,
                                GridPoint {
                                    row: line.id,
                                    col: x.min(cols - 1),
                                },
                            );
                            continue;
                        }
                        let width = usize::from(cell.width()).min(cols);
                        let mut spacer = None;
                        if x + width > cols {
                            if width == 2 && x < cols {
                                let (index, row) = self.pages.locate_from_end(0);
                                let page = &mut self.pages.pages[index];
                                let slot = page.slot(row, x);
                                page.cells[slot].set_spacer_head(true);
                                spacer = Some(GridPoint {
                                    row: line.id,
                                    col: x,
                                });
                            }
                            line.wrapped = true;
                            self.apply_reflow_row(line);
                            output.push(line);
                            line = self.reflow_row_state();
                            line.semantic = old.semantic;
                            line.continuation = true;
                            x = 0;
                            self.ensure_reflow_row(
                                &output,
                                line,
                                capacity,
                                &mut spare,
                                &mut exposed_rows,
                            );
                        }
                        if bulk_copy
                            && cell.width() == 1
                            && !cell.spacer_head()
                            && !cell.has_grapheme()
                            && !cell.has_hyperlink()
                            && (cell.background().is_none() || cell.style_id() == 0)
                        {
                            let window =
                                &old.cells[old_col..old_col + (used - old_col).min(cols - x)];
                            let count = crate::printing::destination_narrow(
                                window,
                                cell.bits() & crate::printing::DEST_MASK,
                            );
                            if self.copy_reflow_run(
                                &source_page,
                                &window[..count],
                                x,
                                &mut style_cache,
                            ) {
                                if count > 1 {
                                    cells.nth(count - 2);
                                }
                                x += count;
                                continue;
                            }
                        }
                        record(
                            old_col,
                            spacer.unwrap_or(GridPoint {
                                row: line.id,
                                col: x,
                            }),
                        );
                        if unmanaged {
                            // Reflow writes each fresh destination slot once.
                            let mut copy = *cell;
                            if cols == 1 && copy.width() == 2 {
                                copy.set_codepoint(None);
                            }
                            copy.set_width(width as u8);
                            let page = self.pages.pages.back_mut().unwrap();
                            let row = usize::from(page.rows) - 1;
                            let slot = page.slot(row, x);
                            page.cells[slot] = copy;
                            if width == 2 {
                                copy.set_codepoint(None);
                                copy.set_width(0);
                                page.cells[slot + 1] = copy;
                            }
                            page.mark_cell(row, page.cells[slot]);
                        } else {
                            let mut copy = old.copy_cell(old_col);
                            let source_link = copy.link_id;
                            if cols == 1 && copy.cell.width() == 2 {
                                copy.cell.set_codepoint(None);
                                copy.text = None;
                            }
                            copy.cell.set_width(width as u8);
                            let _ = self.install_cell(output.len(), x, copy, true);
                            if width == 2 {
                                let page = self.pages.pages.back().unwrap();
                                let mut tail = page.row(usize::from(page.rows) - 1).copy_cell(x);
                                tail.cell.set_codepoint(None);
                                tail.cell.set_width(0);
                                tail.text = None;
                                tail.link_id = source_link;
                                let _ = self.install_cell(output.len(), x + 1, tail, true);
                            }
                        }
                        wide_tail = Some(GridPoint {
                            row: line.id,
                            col: (x + 1).min(cols - 1),
                        });
                        x += width;
                    }
                    for old_col in wanted
                        .iter()
                        .copied()
                        .filter(|&col| col >= used && col < old.cells.len())
                    {
                        map.insert(
                            (old.id, old_col),
                            GridPoint {
                                row: line.id,
                                col: (x + old_col - used).min(cols - 1),
                            },
                        );
                    }
                    if used > 0 {
                        pin_x = x.min(cols - 1);
                        written_rows = output.len() + 1;
                    }
                    if !old.wrapped {
                        if output.len() < exposed_rows {
                            self.apply_reflow_row(line);
                        }
                        output.push(line);
                        line = self.reflow_row_state();
                        x = 0;
                    }
                }
                // Keep at most one exhausted source allocation for the next destination page.
                spare = Some(source_page);
            }
            if output.len() < written_rows {
                self.apply_reflow_row(line);
                output.push(line);
            }
            if let Some(p) = map.get(&(old_cursor.row, old_cursor.col)) {
                mapped_cursor = *p;
            }
            saved_point = saved_point.and_then(|p| map.get(&(p.row, p.col)).copied());
            self.viewport_pin = self
                .viewport_pin
                .and_then(|p| map.get(&(p.row, p.col)).copied());
            for point in self.tracked.0.values_mut() {
                *point = point.and_then(|p| map.get(&(p.row, p.col)).copied());
            }
            self.selection = self.selection.and_then(|s| {
                Some(Selection {
                    start: *map.get(&(s.start.row, s.start.col))?,
                    end: *map.get(&(s.end.row, s.end.col))?,
                    rectangular: s.rectangular,
                })
            });
            self.graphics.reflow(&map);
            output.truncate(written_rows);
            if self.pages.total_rows() > output.len() {
                self.pages.truncate(output.len());
            }
            while self.pages.total_rows() < height {
                let id = self.next_row_id();
                self.grow_row(id, Color::Default, height);
            }
            let start = self.pages.total_rows() - height;
            if let Some(cursor_index) = self
                .all_rows()
                .position(|row| row.id == mapped_cursor.row)
                .filter(|&i| i >= start)
            {
                let wrapped = self
                    .all_rows()
                    .skip(start)
                    .take(cursor_index - start + 1)
                    .filter(|r| r.wrap_continuation)
                    .count();
                let remaining = height.saturating_sub(cursor_y + 1);
                let current = self.pages.total_rows() - cursor_index - 1;
                let grow = remaining
                    .saturating_sub(wrapped.saturating_sub(old_wrapped))
                    .saturating_sub(current);
                for _ in 0..grow {
                    let id = self.next_row_id();
                    self.grow_row(id, Color::Default, height);
                }
            }
        } else if cols != old_cols {
            self.resize_columns(cols);
            mapped_cursor.col = mapped_cursor.col.min(cols - 1);
            if let Some(point) = &mut self.viewport_pin {
                point.col = point.col.min(cols - 1);
            }
        }
        if !height_first {
            self.resize_height(cols, old_rows, rows, mapped_cursor, saved_point);
        }
        self.height = rows;
        while self.pages.total_rows() < rows {
            let id = self.next_row_id();
            self.grow_row(id, Color::Default, rows);
        }
        let start = self.history_len();
        let cursor_index = self.all_rows().position(|row| row.id == mapped_cursor.row);
        self.cursor.row = cursor_index
            .unwrap_or(start)
            .saturating_sub(start)
            .min(rows - 1);
        self.cursor.col = mapped_cursor.col.min(cols - 1);
        if cursor_index.is_none_or(|i| i < start) {
            self.cursor.col = 0;
        }
        let saved_location = saved_point
            .and_then(|p| self.all_rows().position(|r| r.id == p.row).map(|i| (i, p)))
            .filter(|(i, _)| *i >= start);
        if let Some(saved) = &mut self.saved_cursor {
            if let Some((index, point)) = saved_location {
                saved.cursor.row = index - start;
                saved.cursor.col = point.col.min(cols - 1);
                if saved.cursor.pending_wrap && saved.cursor.col != cols - 1 {
                    saved.cursor.pending_wrap = false;
                    saved.cursor.col += 1;
                }
            } else {
                saved.cursor.row = 0;
                saved.cursor.col = 0;
                saved.cursor.pending_wrap = false;
            }
        }
        self.cursor.col = self
            .cursor
            .col
            .min(self.row(self.cursor.row).cells.len() - 1);
        if self.limits.bytes == Some(0) || self.memory_limit == Some(0) {
            self.clear_history();
        } else {
            let removed = self.pages.prune(
                rows,
                ScrollbackLimits {
                    bytes: None,
                    ..self.effective_limits()
                },
                self.memory_limit,
            );
            self.discard_ids(removed);
        }
        self.viewport_offset = self.viewport_offset.min(self.history_len());
        if viewport_pinned && let Some(point) = self.viewport_pin {
            let index = self.all_rows().position(|row| row.id == point.row);
            if let Some(index) = index {
                self.viewport_offset = self.history_len().saturating_sub(index);
                self.viewport_pin_column = if self.viewport_offset > 0 {
                    point.col.min(cols - 1)
                } else {
                    0
                };
            } else {
                self.viewport_offset = self.history_len();
                self.viewport_pin_column = 0;
            }
        } else if viewport_at_top {
            self.viewport_offset = self.history_len();
            self.viewport_pin_column = 0;
        }
        self.renew_cursor_implicit_link();
    }

    fn resize_height(
        &mut self,
        cols: usize,
        old_rows: usize,
        rows: usize,
        cursor: GridPoint,
        saved: Option<GridPoint>,
    ) {
        let mut trim = old_rows.saturating_sub(rows);
        while trim > 0
            && self.pages.total_rows() > rows
            && self.all_rows().next_back().is_some_and(|r| {
                r.id != cursor.row
                    && !saved.is_some_and(|p| p.row == r.id)
                    && !self.viewport_pin.is_some_and(|p| p.row == r.id)
                    && !self
                        .tracked
                        .0
                        .values()
                        .any(|p| p.is_some_and(|p| p.row == r.id))
                    && r.cells.iter().all(|c| c.codepoint().is_none())
            })
        {
            self.pages.truncate(self.pages.total_rows() - 1);
            trim -= 1;
        }
        self.height = rows;
        self.columns = cols;
        if rows > old_rows && self.cursor.row < old_rows - 1 {
            for _ in 0..rows - old_rows {
                let id = self.next_row_id();
                self.grow_row(id, Color::Default, rows);
            }
        }
        while self.pages.total_rows() < rows {
            let id = self.next_row_id();
            self.grow_row(id, Color::Default, rows);
        }
    }

    fn resize_columns(&mut self, columns: usize) {
        let mut old = std::mem::take(&mut self.pages);
        let mut spare = None;
        while let Some(mut page) = old.pages.pop_front() {
            let spacer = (0..usize::from(page.rows))
                .any(|r| page.row_cells(r).last().is_some_and(|c| c.spacer_head()));
            if columns <= usize::from(page.columns)
                || columns <= usize::from(page.capacity.cols) && !spacer
            {
                for row in 0..usize::from(page.rows) {
                    let start = page.slot(row, 0);
                    for col in columns..usize::from(page.columns) {
                        page.clear_cell(start + col, Color::Default);
                    }
                }
                page.columns = columns as u16;
                for row in 0..usize::from(page.rows) {
                    page.repair_wide(row, Color::Default);
                }
                page.refresh_charge();
                self.pages.push(page);
            } else {
                let capacity = page.adjusted_capacity(columns as u16, false);
                for row in 0..usize::from(page.rows) {
                    let mut copy = RowCopy::from_view(page.row(row));
                    copy.wrapped = false;
                    copy.wrap_continuation = false;
                    let absolute = self.pages.total_rows();
                    self.reflow_expose(capacity, copy.id, &mut spare);
                    self.install_row(absolute, copy, usize::MAX);
                    let (index, row) = self.locate(absolute);
                    self.pages.pages[index].repair_wide(row, Color::Default);
                }
                spare = Some(page);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Terminal, snapshot};

    #[test]
    fn reflow_style_cache_follows_resource_relocation() {
        let mut source = Terminal::new(8, 2, 0);
        source.feed(b"\x1b[31mx");
        let source_page = &source.screen().pages.pages[0];
        let cells = &source_page.row_cells(0)[..1];

        for split in [false, true] {
            let mut screen = Screen::new(8, 2, ScrollbackLimits::NONE);
            screen.set_cell_style(
                0,
                0,
                Style {
                    foreground: Color::Indexed(2),
                    ..Style::default()
                },
            );
            let mut cache = None;
            assert!(screen.copy_reflow_run(source_page, cells, 0, &mut cache));
            assert_eq!(cache.unwrap().destination, 2);

            // Both operations renumber the copied style; only splitting also
            // changes its owner. Neither may reuse the cached destination ID.
            if split {
                screen.split_resource_page(1).unwrap();
            } else {
                let page = &mut screen.pages.pages[0];
                page.clear_cell(page.slot(0, 0), Color::Default);
                page.rebuild(None).unwrap();
            }

            let page = screen.pages.pages.back().unwrap();
            assert_eq!(page.row(usize::from(page.rows) - 1).cells[0].style_id(), 1);
            assert!(screen.copy_reflow_run(source_page, cells, 1, &mut cache));
            let page = screen.pages.pages.back().unwrap();
            let row = page.row(usize::from(page.rows) - 1);
            for col in 0..2 {
                assert_eq!(row.cells[col].codepoint(), Some('x'));
                assert_eq!(row.style(col).foreground, Color::Indexed(1));
            }
            assert_eq!(page.styles.reference_count(1), 2);
        }
    }

    #[test]
    fn unmanaged_reflow_matches_resource_copying() {
        let mut source = Terminal::new(17, 4, 400);
        source.feed(b"\x1b[?2027h");
        for _ in 0..8 {
            source.feed("abcdefghijklmnopqr\r\nabc界界xyz\r\n".as_bytes());
            source.feed(b"\x1b[44m\x1b[2K\x1b[0m\r\n");
            source.feed("\x1b[31mstyled\x1b[0m a\u{301}👩\u{200d}💻\r\n".as_bytes());
        }
        for width in [1, 2, 3, 8, 32] {
            let mut actual = source.clone();
            let mut reference = source.clone();
            for target in [width, 17] {
                // Conservative hints force the existing resource-copy path.
                for page in &mut reference.screen_mut().pages.pages {
                    for header in &mut page.headers[..usize::from(page.rows)] {
                        header.set(RowHeader::STYLED, true);
                    }
                }
                actual.resize(target, 4);
                reference.resize(target, 4);
                assert_eq!(
                    snapshot::encode_to_vec(&actual).unwrap(),
                    snapshot::encode_to_vec(&reference).unwrap(),
                    "width={width}, target={target}",
                );
            }
        }
    }
}
