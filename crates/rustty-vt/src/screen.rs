//! Owned screen storage. Rows retain identity when they enter scrollback.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// Independent logical storage budgets. `None` means unlimited.
///
/// Bytes include active and history row storage, including cell, text, and
/// hyperlink allocations. Active rows are always retained. Graphics use their
/// own budget. Unlike Ghostty's page allocator, pruning here removes whole rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrollbackLimits {
    pub bytes: Option<usize>,
    pub lines: Option<usize>,
}

impl ScrollbackLimits {
    pub const NONE: Self = Self {
        bytes: Some(0),
        lines: Some(0),
    };
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HyperlinkId {
    Implicit(u32),
    Explicit(Vec<u8>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Style {
    pub foreground: Color,
    pub background: Color,
    pub underline_color: Color,
    pub bold: bool,
    pub faint: bool,
    pub italic: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: Underline,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticContent {
    #[default]
    Output,
    Prompt,
    Input,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    /// Empty for unwritten cells and the continuation of a wide glyph.
    pub text: String,
    /// Zero denotes the continuation of a two-cell glyph.
    pub width: u8,
    pub style: Style,
    pub hyperlink: Option<String>,
    pub hyperlink_id: Option<HyperlinkId>,
    /// Present only when the OSC 8 URI contains bytes that are not UTF-8.
    pub hyperlink_raw: Option<Vec<u8>>,
    pub protected: bool,
    pub semantic: SemanticContent,
    /// Padding before a wide glyph that wrapped at the right edge.
    pub spacer_head: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: String::new(),
            width: 1,
            style: Style::default(),
            hyperlink: None,
            hyperlink_id: None,
            hyperlink_raw: None,
            protected: false,
            semantic: SemanticContent::Output,
            spacer_head: false,
        }
    }
}

impl Cell {
    pub(crate) fn blank(background: Color) -> Self {
        Self {
            style: Style {
                background,
                ..Style::default()
            },
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.width != 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub id: u64,
    pub cells: Vec<Cell>,
    pub wrapped: bool,
    pub wrap_continuation: bool,
    pub semantic: SemanticContent,
    pub dirty: bool,
}

impl Row {
    pub(crate) fn new(id: u64, cols: usize, background: Color) -> Self {
        Self {
            id,
            cells: vec![Cell::blank(background); cols],
            wrapped: false,
            wrap_continuation: false,
            semantic: SemanticContent::Output,
            dirty: true,
        }
    }

    pub fn text(&self) -> String {
        let mut result = String::new();
        for cell in &self.cells {
            if cell.width == 0 || cell.spacer_head {
                continue;
            }
            if cell.text.is_empty() {
                result.push(' ');
            } else {
                result.push_str(&cell.text);
            }
        }
        result.truncate(result.trim_end_matches(' ').len());
        result
    }

    pub(crate) fn storage_bytes(&self) -> usize {
        size_of::<Self>()
            .saturating_add(self.cells.capacity().saturating_mul(size_of::<Cell>()))
            .saturating_add(self.cells.iter().fold(0usize, |bytes, cell| {
                bytes
                    .saturating_add(cell.text.capacity())
                    .saturating_add(cell.hyperlink.as_ref().map_or(0, String::capacity))
                    .saturating_add(cell.hyperlink_raw.as_ref().map_or(0, Vec::capacity))
                    .saturating_add(match &cell.hyperlink_id {
                        Some(HyperlinkId::Explicit(id)) => id.capacity(),
                        _ => 0,
                    })
            }))
    }

    pub(crate) fn used(&self) -> usize {
        self.cells
            .iter()
            .rposition(|c| {
                !c.text.is_empty() || c.width == 0 || c.style.background != Color::Default
            })
            .map_or(0, |i| i + 1)
    }

    pub(crate) fn repair_wide(&mut self, background: Color) {
        for col in 0..self.cells.len() {
            if self.cells[col].width == 0 && (col == 0 || self.cells[col - 1].width != 2)
                || self.cells[col].width == 2
                    && (col + 1 == self.cells.len() || self.cells[col + 1].width != 0)
                || self.cells[col].spacer_head && col + 1 != self.cells.len()
            {
                self.cells[col] = Cell::blank(background);
            }
        }
        self.dirty = true;
    }

    pub(crate) fn erase(
        &mut self,
        mut start: usize,
        mut end: usize,
        background: Color,
        protected: bool,
    ) {
        if start >= self.cells.len() || start >= end {
            return;
        }
        end = end.min(self.cells.len());
        if self.cells[start].width == 0 && start > 0 {
            start -= 1;
        }
        if end < self.cells.len() && self.cells[end - 1].width == 2 {
            end += 1;
        }
        for cell in &mut self.cells[start..end] {
            if !protected || !cell.protected {
                *cell = Cell::blank(background);
            }
        }
        self.dirty = true;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorShape {
    #[default]
    Block,
    HollowBlock,
    Bar,
    Underline,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub col: usize,
    pub row: usize,
    pub shape: CursorShape,
    pub visible: bool,
    pub blink: bool,
    pub pending_wrap: bool,
    pub style: Style,
    pub protected: bool,
    pub hyperlink: Option<String>,
    pub hyperlink_id: Option<HyperlinkId>,
    pub hyperlink_raw: Option<Vec<u8>>,
    pub semantic: SemanticContent,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            col: 0,
            row: 0,
            shape: CursorShape::Block,
            visible: true,
            blink: false,
            pending_wrap: false,
            style: Style::default(),
            protected: false,
            hyperlink: None,
            hyperlink_id: None,
            hyperlink_raw: None,
            semantic: SemanticContent::Output,
        }
    }
}

/// A reference to a cell by stable row identity, including scrollback rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridPoint {
    pub row: u64,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub start: GridPoint,
    pub end: GridPoint,
    pub rectangular: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TrackedPoint(u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Charset {
    #[default]
    Utf8,
    Ascii,
    British,
    DecSpecial,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CharsetState {
    pub slots: [Charset; 4],
    pub gl: usize,
    pub gr: usize,
    pub single: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedCursor {
    pub cursor: Cursor,
    pub origin: bool,
    pub charset: CharsetState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Screen {
    #[serde(skip)]
    pub(crate) metadata: crate::snapshot::ScreenMetadata,
    #[serde(skip)]
    pub graphics: crate::graphics::Graphics,
    pub rows: Vec<Row>,
    pub history: VecDeque<Row>,
    pub cursor: Cursor,
    pub selection: Option<Selection>,
    pub viewport_offset: usize,
    pub kitty_keyboard: Vec<u8>,
    pub(crate) saved_cursor: Option<SavedCursor>,
    pub(crate) charset: CharsetState,
    pub(crate) iso_protection: bool,
    pub(crate) limits: ScrollbackLimits,
    pub(crate) history_bytes: usize,
    pub(crate) next_row: u64,
    #[serde(skip)]
    tracked: HashMap<u64, Option<GridPoint>>,
    #[serde(skip)]
    next_track: u64,
}

impl Screen {
    pub(crate) fn new(cols: usize, rows: usize, limits: ScrollbackLimits) -> Self {
        Self {
            metadata: crate::snapshot::ScreenMetadata::default(),
            graphics: crate::graphics::Graphics::default(),
            rows: (0..rows)
                .map(|i| Row::new(i as u64, cols, Color::Default))
                .collect(),
            history: VecDeque::new(),
            cursor: Cursor::default(),
            selection: None,
            viewport_offset: 0,
            kitty_keyboard: Vec::new(),
            saved_cursor: None,
            charset: CharsetState::default(),
            iso_protection: false,
            limits,
            history_bytes: 0,
            next_row: rows as u64,
            tracked: HashMap::new(),
            next_track: 0,
        }
    }

    pub fn all_rows(&self) -> impl DoubleEndedIterator<Item = &Row> {
        self.history.iter().chain(self.rows.iter())
    }

    pub fn viewport(&self) -> impl Iterator<Item = &Row> {
        let start = self.history.len().saturating_sub(self.viewport_offset);
        self.all_rows().skip(start).take(self.rows.len())
    }

    /// Copy only visible rows so shaping and GPU preparation need no session lock.
    pub fn snapshot_viewport(&self) -> Self {
        let mut cursor = self.cursor.clone();
        cursor.row = cursor.row.saturating_add(self.viewport_offset);
        cursor.visible &= cursor.row < self.rows.len();
        cursor.row = cursor.row.min(self.rows.len() - 1);
        Self {
            metadata: self.metadata.clone(),
            graphics: self.graphics.snapshot(self),
            rows: self.viewport().cloned().collect(),
            history: VecDeque::new(),
            cursor,
            selection: self.selection,
            viewport_offset: 0,
            kitty_keyboard: self.kitty_keyboard.clone(),
            saved_cursor: None,
            charset: self.charset.clone(),
            iso_protection: self.iso_protection,
            limits: ScrollbackLimits::NONE,
            history_bytes: 0,
            next_row: self.next_row,
            tracked: HashMap::new(),
            next_track: 0,
        }
    }

    pub fn scroll_viewport(&mut self, rows: isize) {
        self.viewport_offset = self
            .viewport_offset
            .saturating_add_signed(rows)
            .min(self.history.len());
    }

    pub fn row_by_id(&self, id: u64) -> Option<&Row> {
        self.all_rows().find(|r| r.id == id)
    }

    pub fn point(&self, row: usize, col: usize) -> Option<GridPoint> {
        let row = self.all_rows().nth(row)?;
        (col < row.cells.len()).then_some(GridPoint { row: row.id, col })
    }

    pub fn track(&mut self, point: GridPoint) -> TrackedPoint {
        let id = self.next_track;
        self.next_track = self.next_track.wrapping_add(1);
        let valid = self
            .row_by_id(point.row)
            .is_some_and(|r| point.col < r.cells.len());
        self.tracked.insert(id, valid.then_some(point));
        TrackedPoint(id)
    }

    pub fn resolve(&self, point: TrackedPoint) -> Option<GridPoint> {
        self.tracked.get(&point.0).copied().flatten()
    }

    pub fn untrack(&mut self, point: TrackedPoint) {
        self.tracked.remove(&point.0);
    }

    pub fn selection_text(&self) -> Option<String> {
        let selection = self.selection?;
        let rows: Vec<_> = self.all_rows().collect();
        let mut first = rows.iter().position(|r| r.id == selection.start.row)?;
        let mut last = rows.iter().position(|r| r.id == selection.end.row)?;
        let mut left = selection.start.col;
        let mut right = selection.end.col;
        if (first, left) > (last, right) {
            std::mem::swap(&mut first, &mut last);
            std::mem::swap(&mut left, &mut right);
        }
        let mut result = String::new();
        for (i, row) in rows.iter().enumerate().take(last + 1).skip(first) {
            let start = if selection.rectangular {
                left.min(right)
            } else if i == first {
                left
            } else {
                0
            };
            let end = if selection.rectangular {
                left.max(right) + 1
            } else if i == last {
                right + 1
            } else {
                row.cells.len()
            };
            let line_start = result.len();
            for cell in &row.cells[start.min(row.cells.len())..end.min(row.cells.len())] {
                if cell.width == 0 || cell.spacer_head {
                    continue;
                }
                if cell.text.is_empty() {
                    result.push(' ');
                } else {
                    result.push_str(&cell.text);
                }
            }
            if selection.rectangular || !row.wrapped {
                while result.len() > line_start && result.ends_with(' ') {
                    result.pop();
                }
                if i < last {
                    result.push('\n');
                }
            }
        }
        Some(result)
    }

    pub(crate) fn blank_row(&mut self, cols: usize, background: Color) -> Row {
        let id = self.next_row;
        self.next_row = self.next_row.wrapping_add(1);
        Row::new(id, cols, background)
    }

    pub(crate) fn split_cell_boundary(&mut self, col: usize) {
        let y = self.cursor.row;
        let cols = self.rows[y].cells.len();
        let background = self.cursor.style.background;
        if col == cols {
            if self.rows[y].wrapped && self.rows[y].cells[cols - 1].spacer_head {
                self.rows[y].erase(cols - 1, cols, background, false);
            }
            return;
        }
        if col <= 1 && self.rows[y].cells[0].width == 2 {
            let previous = if y > 0 {
                self.rows.get_mut(y - 1)
            } else {
                // This is the only edit path that can mutate a historical row.
                self.history.back_mut()
            };
            if let Some(previous) = previous
                && previous.wrapped
                && previous.cells[cols - 1].spacer_head
            {
                let before = previous.storage_bytes();
                previous.erase(cols - 1, cols, background, false);
                if y == 0 {
                    self.history_bytes = self
                        .history_bytes
                        .saturating_sub(before.saturating_sub(previous.storage_bytes()));
                }
            }
        }
        if col > 0 && self.rows[y].cells[col - 1].width == 2 {
            self.rows[y].erase(col - 1, col + 1, background, false);
        }
    }

    pub(crate) fn discard_row(&mut self, id: u64) {
        self.graphics.discard_row(id);
        for point in self.tracked.values_mut() {
            if point.is_some_and(|p| p.row == id) {
                *point = None;
            }
        }
        if self
            .selection
            .is_some_and(|s| s.start.row == id || s.end.row == id)
        {
            self.selection = None;
        }
    }

    pub(crate) fn push_history(&mut self, row: Row) {
        if self.limits.bytes == Some(0) || self.limits.lines == Some(0) {
            self.discard_row(row.id);
            return;
        }
        self.history_bytes = self.history_bytes.saturating_add(row.storage_bytes());
        self.history.push_back(row);
        if self.viewport_offset > 0 {
            self.viewport_offset += 1;
        }
        self.enforce_limits();
    }

    /// Charged bytes in history rows; container spare capacity and graphics are
    /// excluded. Text and hyperlink allocations are charged at their capacity.
    pub fn history_bytes(&self) -> usize {
        self.history_bytes
    }

    pub fn storage_bytes(&self) -> usize {
        self.rows.iter().fold(self.history_bytes, |bytes, row| {
            bytes.saturating_add(row.storage_bytes())
        })
    }

    pub(crate) fn set_limits(&mut self, limits: ScrollbackLimits) {
        self.limits = limits;
        self.enforce_limits();
        self.history.shrink_to_fit();
    }

    pub(crate) fn clear_history(&mut self) {
        while let Some(row) = self.history.pop_front() {
            self.discard_row(row.id);
        }
        self.history_bytes = 0;
        self.viewport_offset = 0;
    }

    pub(crate) fn enforce_limits(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let byte_budget = self.limits.bytes.map(|bytes| {
            let active = self.rows.iter().fold(0usize, |bytes, row| {
                bytes.saturating_add(row.storage_bytes())
            });
            bytes.saturating_sub(active)
        });
        while self
            .limits
            .lines
            .is_some_and(|max| self.history.len() > max)
            || byte_budget.is_some_and(|max| self.history_bytes > max)
        {
            let row = self.history.pop_front().unwrap();
            self.history_bytes = self.history_bytes.saturating_sub(row.storage_bytes());
            self.discard_row(row.id);
        }
        self.viewport_offset = self.viewport_offset.min(self.history.len());
    }

    pub(crate) fn resize(&mut self, cols: usize, rows: usize, reflow: bool) {
        let old_cols = self.rows[0].cells.len();
        let old_rows = self.rows.len();
        let cursor_y = self.cursor.row;
        let old_cursor = GridPoint {
            row: self.rows[self.cursor.row].id,
            col: self.cursor.col,
        };
        let mut saved_point = self.saved_cursor.as_ref().and_then(|saved| {
            self.rows.get(saved.cursor.row).map(|row| GridPoint {
                row: row.id,
                col: saved.cursor.col,
            })
        });
        let mut contents: Vec<Row> = self.history.drain(..).chain(self.rows.drain(..)).collect();
        self.history_bytes = 0;
        let cursor_index = contents
            .iter()
            .position(|r| r.id == old_cursor.row)
            .unwrap();
        // Narrowing uses the new height while wrapping; widening unwraps
        // first, before changing the height. This preserves the active boundary.
        let height_first = !reflow || cols <= old_cols;
        if height_first {
            self.resize_height(
                &mut contents,
                old_cols,
                old_rows,
                rows,
                old_cursor,
                saved_point,
            );
        }
        let mut mapped_cursor = old_cursor;
        if cols != old_cols && reflow {
            let height = if height_first { rows } else { old_rows };
            let active_start = contents.len().saturating_sub(height);
            let old_wrapped = contents
                [active_start..cursor_index.min(contents.len()).max(active_start)]
                .iter()
                .filter(|r| r.wrapped)
                .count();
            let mut map = HashMap::<GridPointKey, GridPoint>::new();
            let mut output = Vec::new();
            let mut line = self.blank_row(cols, Color::Default);
            let mut x: usize = 0;
            let mut pin_x: usize = 0;
            for old in &contents {
                let mut used = if old.wrapped {
                    old.cells.len()
                } else {
                    old.used()
                };
                // Non-cursor pins in trailing blanks clamp to the remaining
                // destination width. The live cursor alone preserves all blanks.
                let mut keep_pin = |point: &mut GridPoint| {
                    if point.row == old.id {
                        if point.col >= used {
                            point.col = point.col.min(cols - 1 - pin_x);
                        }
                        used = used.max(point.col + 1);
                    }
                };
                if let Some(point) = &mut saved_point {
                    keep_pin(point);
                }
                for point in self.tracked.values_mut().flatten() {
                    keep_pin(point);
                }
                if let Some(selection) = &mut self.selection {
                    keep_pin(&mut selection.start);
                    keep_pin(&mut selection.end);
                }
                if old.id == old_cursor.row {
                    used = used.max(old_cursor.col + 1);
                }
                if old.semantic != SemanticContent::Output {
                    used = used.max(1);
                }
                let mut wide_tail = None;
                for (old_col, cell) in old.cells.iter().take(used).enumerate() {
                    if cell.width == 0 {
                        map.insert(
                            (old.id, old_col),
                            wide_tail.unwrap_or(GridPoint {
                                row: line.id,
                                col: x.saturating_sub(1),
                            }),
                        );
                        continue;
                    }
                    if cell.spacer_head {
                        map.insert(
                            (old.id, old_col),
                            GridPoint {
                                row: line.id,
                                col: x.min(cols - 1),
                            },
                        );
                        continue;
                    }
                    let width = usize::from(cell.width).min(cols);
                    let mut spacer = None;
                    if x + width > cols {
                        if width == 2 && x < cols {
                            line.cells[x].spacer_head = true;
                            spacer = Some(GridPoint {
                                row: line.id,
                                col: x,
                            });
                        }
                        line.wrapped = true;
                        output.push(line);
                        line = self.blank_row(cols, Color::Default);
                        line.wrap_continuation = true;
                        x = 0;
                    }
                    map.insert(
                        (old.id, old_col),
                        spacer.unwrap_or(GridPoint {
                            row: line.id,
                            col: x,
                        }),
                    );
                    let mut cell = cell.clone();
                    if cols == 1 && cell.width == 2 {
                        cell.text.clear();
                    }
                    cell.width = width as u8;
                    line.cells[x] = cell.clone();
                    if width == 2 {
                        cell.text.clear();
                        cell.width = 0;
                        line.cells[x + 1] = cell;
                    }
                    wide_tail = Some(GridPoint {
                        row: line.id,
                        col: (x + 1).min(cols - 1),
                    });
                    x += width;
                }
                // Empty rows and tracked blank columns still need a stable mapping.
                for old_col in used..old.cells.len() {
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
                }
                if !old.wrapped {
                    line.semantic = old.semantic;
                    output.push(line);
                    line = self.blank_row(cols, Color::Default);
                    x = 0;
                }
            }
            if x > 0 {
                output.push(line);
            }
            if let Some(p) = map.get(&(old_cursor.row, old_cursor.col)) {
                mapped_cursor = *p;
            }
            saved_point = saved_point.and_then(|p| map.get(&(p.row, p.col)).copied());
            for point in self.tracked.values_mut() {
                *point = point.and_then(|p| map.get(&(p.row, p.col)).copied());
            }
            self.selection = self.selection.and_then(|s| {
                Some(Selection {
                    start: *map.get(&(s.start.row, s.start.col))?,
                    end: *map.get(&(s.end.row, s.end.col))?,
                    rectangular: s.rectangular,
                })
            });
            // Reflow uses blank rows below the content before pushing live text
            // into history. Cursor and tracked references keep their blank rows.
            while output.len() > 1
                && output.last().is_some_and(|r| {
                    r.id != mapped_cursor.row
                        && !saved_point.is_some_and(|p| p.row == r.id)
                        && !self
                            .tracked
                            .values()
                            .any(|p| p.is_some_and(|p| p.row == r.id))
                        && r.used() == 0
                        && r.semantic == SemanticContent::Output
                })
            {
                output.pop();
            }
            while output.len() < height {
                output.push(self.blank_row(cols, Color::Default));
            }
            let start = output.len() - height;
            if let Some(cursor_index) = output
                .iter()
                .position(|r| r.id == mapped_cursor.row)
                .filter(|&i| i >= start)
            {
                let wrapped = output[start..cursor_index]
                    .iter()
                    .filter(|r| r.wrapped)
                    .count();
                let remaining = height.saturating_sub(cursor_y + 1);
                let current = output.len() - cursor_index - 1;
                let grow = remaining
                    .saturating_sub(wrapped.saturating_sub(old_wrapped))
                    .saturating_sub(current);
                for _ in 0..grow {
                    output.push(self.blank_row(cols, Color::Default));
                }
            }
            contents = output;
            self.graphics.reflow(&map);
        } else if cols != old_cols {
            for row in &mut contents {
                row.cells.resize(cols, Cell::default());
                row.repair_wide(Color::Default);
                if cols > old_cols {
                    row.wrapped = false;
                }
            }
            mapped_cursor.col = mapped_cursor.col.min(cols - 1);
        }
        if !height_first {
            self.resize_height(
                &mut contents,
                cols,
                old_rows,
                rows,
                mapped_cursor,
                saved_point,
            );
        }
        while contents.len() < rows {
            contents.push(self.blank_row(cols, Color::Default));
        }
        let start = contents.len() - rows;
        let cursor_index = contents.iter().position(|r| r.id == mapped_cursor.row);
        self.cursor.row = contents
            .iter()
            .position(|r| r.id == mapped_cursor.row)
            .unwrap_or(start)
            .saturating_sub(start)
            .min(rows - 1);
        self.cursor.col = mapped_cursor.col.min(cols - 1);
        if cursor_index.is_none_or(|i| i < start) {
            self.cursor.col = 0;
        }
        if let Some(saved) = &mut self.saved_cursor {
            if let Some((index, point)) = saved_point
                .and_then(|p| contents.iter().position(|r| r.id == p.row).map(|i| (i, p)))
                .filter(|(index, _)| *index >= start)
            {
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
        self.rows = contents.split_off(start);
        for row in contents {
            self.push_history(row);
        }
        self.viewport_offset = self.viewport_offset.min(self.history.len());
    }

    fn resize_height(
        &mut self,
        contents: &mut Vec<Row>,
        cols: usize,
        old_rows: usize,
        rows: usize,
        cursor: GridPoint,
        saved: Option<GridPoint>,
    ) {
        let mut trim = old_rows.saturating_sub(rows);
        while trim > 0
            && contents.len() > rows
            && contents.last().is_some_and(|r| {
                r.id != cursor.row
                    && !saved.is_some_and(|p| p.row == r.id)
                    && !self
                        .tracked
                        .values()
                        .any(|p| p.is_some_and(|p| p.row == r.id))
                    && r.cells.iter().all(|c| c.text.is_empty())
            })
        {
            contents.pop();
            trim -= 1;
        }
        if rows > old_rows && self.cursor.row < old_rows - 1 {
            for _ in 0..rows - old_rows {
                contents.push(self.blank_row(cols, Color::Default));
            }
        }
        while contents.len() < rows {
            contents.push(self.blank_row(cols, Color::Default));
        }
    }
}

type GridPointKey = (u64, usize);
