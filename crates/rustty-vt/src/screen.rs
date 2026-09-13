//! Owned screen storage. Rows retain identity when they enter scrollback.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

use crate::page_list::{Page, PageAllocationInfo, PageList};
use crate::page_resources::{SetFull, StyleAdmission};

/// Independent logical storage budgets. `None` means unlimited.
///
/// Bytes charge native page allocations, including active pages. Active rows
/// are always retained; pruning removes complete historical pages. Graphics
/// have their own budget. Limits have Ghostty's minimum page-size floor; an
/// explicit zero byte limit disables ordinary scrolling into history.
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

impl Style {
    /// Hash the native packed representation used by page style admission.
    pub(crate) fn native_hash(self) -> u64 {
        let mut packed = 0u128;
        for (i, color) in [self.foreground, self.background, self.underline_color]
            .into_iter()
            .enumerate()
        {
            let (tag, data): (u128, u128) = match color {
                Color::Default => (0, 0),
                Color::Indexed(index) => (1, index.into()),
                Color::Rgb(r, g, b) => (
                    2,
                    u128::from(r) | (u128::from(g) << 8) | (u128::from(b) << 16),
                ),
            };
            packed |= tag << (i * 8);
            packed |= data << (24 + i * 24);
        }
        let underline = match self.underline {
            Underline::None => 0u128,
            Underline::Single => 1,
            Underline::Double => 2,
            Underline::Curly => 3,
            Underline::Dotted => 4,
            Underline::Dashed => 5,
        };
        let mut flags = underline << 8;
        for (bit, set) in [
            self.bold,
            self.italic,
            self.faint,
            self.blink,
            self.inverse,
            self.invisible,
            self.strikethrough,
            self.overline,
        ]
        .into_iter()
        .enumerate()
        {
            flags |= u128::from(set) << bit;
        }
        packed |= flags << 96;
        let mut hash = packed as u64 ^ (packed >> 64) as u64;
        // Zig std.hash.int(u64).
        const MULTIPLIER: u64 = 0xbea225f9eb34556d;
        hash = (hash ^ (hash >> 32)).wrapping_mul(MULTIPLIER);
        hash = (hash ^ (hash >> 29)).wrapping_mul(MULTIPLIER);
        hash = (hash ^ (hash >> 32)).wrapping_mul(MULTIPLIER);
        hash ^ (hash >> 29)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticContent {
    #[default]
    Output,
    Prompt,
    Input,
}

/// Which prompt lines the shell can redraw after a resize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptRedraw {
    All,
    None,
    Last,
}

/// Cursor movement supported by the shell's OSC 133 `cl` option.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickMotion {
    Line,
    Multiple,
    ConservativeVertical,
    SmartVertical,
}

/// How the shell handles clicks inside its prompt and input area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticClick {
    None,
    Events { relative: bool },
    CursorKeys { motion: ClickMotion },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cell {
    #[serde(skip)]
    pub(crate) style_id: u16,
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

impl PartialEq for Cell {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
            && self.width == other.width
            && self.style == other.style
            && self.hyperlink == other.hyperlink
            && self.hyperlink_id == other.hyperlink_id
            && self.hyperlink_raw == other.hyperlink_raw
            && self.protected == other.protected
            && self.semantic == other.semantic
            && self.spacer_head == other.spacer_head
    }
}

impl Eq for Cell {}

impl Default for Cell {
    fn default() -> Self {
        Self {
            style_id: 0,
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
    #[serde(skip)]
    pub(crate) style_page: Option<u64>,
    pub id: u64,
    pub cells: Vec<Cell>,
    pub wrapped: bool,
    pub wrap_continuation: bool,
    /// Row prompt marker: Output is unmarked, Prompt begins a prompt, and
    /// Input denotes a prompt continuation. Cells keep their own content kind.
    pub semantic: SemanticContent,
    pub dirty: bool,
}

pub(crate) struct RowCopy {
    row: Row,
    styles: Vec<(Option<u64>, u16, Style)>,
}

impl Row {
    pub(crate) fn new(id: u64, cols: usize, background: Color) -> Self {
        Self {
            style_page: None,
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

/// Kitty's eight-entry cyclic keyboard flag stack. Overflow evicts the oldest
/// entry, while popping a full turn resets the stack to its disabled state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KittyKeyboard {
    pub(crate) flags: [u8; 8],
    pub(crate) index: u8,
}

impl KittyKeyboard {
    pub fn current(&self) -> u8 {
        self.flags[usize::from(self.index)]
    }

    pub fn push(&mut self, flags: u8) {
        self.index = (self.index + 1) % 8;
        self.flags[usize::from(self.index)] = flags & 31;
    }

    pub fn pop(&mut self, count: usize) {
        if count >= self.flags.len() {
            *self = Self::default();
            return;
        }
        for _ in 0..count {
            self.flags[usize::from(self.index)] = 0;
            self.index = self.index.wrapping_sub(1) % 8;
        }
    }

    pub fn set(&mut self, flags: u8, mode: u16) {
        let current = &mut self.flags[usize::from(self.index)];
        match mode {
            0 | 1 => *current = flags & 31,
            2 => *current |= flags & 31,
            3 => *current &= !(flags & 31),
            _ => {}
        }
    }
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

/// External handles belong to a live screen, never its copies or snapshots.
#[derive(Debug, Default)]
struct TrackedPoints(
    HashMap<u64, Option<GridPoint>>,
    HashMap<u64, std::sync::Weak<()>>,
);

impl TrackedPoints {
    fn prune(&mut self) {
        self.1.retain(|id, owner| {
            let alive = owner.strong_count() != 0;
            if !alive {
                self.0.remove(id);
            }
            alive
        });
    }
}

/// An internal pin whose registration expires when its owner is dropped.
/// Expired entries are reclaimed before tracking or moving points, so they
/// cannot affect reflow or keep otherwise empty rows alive.
pub(crate) struct OwnedTrackedPoint {
    point: TrackedPoint,
    _owner: std::sync::Arc<()>,
}

impl OwnedTrackedPoint {
    pub(crate) fn resolve(&self, screen: &Screen) -> Option<GridPoint> {
        screen.resolve(self.point)
    }
}

impl Clone for TrackedPoints {
    fn clone(&self) -> Self {
        Self::default()
    }
}

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
    /// Logical width. Restored physical rows retain their own width until a
    /// column resize reflows them or an edit needs additional cells.
    pub columns: usize,
    pub rows: Vec<Row>,
    pub history: VecDeque<Row>,
    pub cursor: Cursor,
    pub selection: Option<Selection>,
    pub viewport_offset: usize,
    /// Native viewport pins retain their column when a search scrolls to a
    /// match. This is an anchor coordinate, not horizontal scrolling.
    #[serde(skip)]
    pub(crate) viewport_pin_column: usize,
    pub kitty_keyboard: KittyKeyboard,
    pub(crate) saved_cursor: Option<SavedCursor>,
    pub(crate) charset: CharsetState,
    pub(crate) iso_protection: bool,
    pub(crate) limits: ScrollbackLimits,
    pub(crate) history_bytes: usize,
    pub(crate) pages: PageList,
    #[serde(skip)]
    pub(crate) cursor_style: Option<(u64, u16)>,
    pub(crate) next_row: u64,
    #[serde(skip)]
    tracked: TrackedPoints,
}

impl Screen {
    /// Whether explicit line feeds end the current OSC 133 input region.
    /// Soft wrapping preserves this region until an explicit line feed.
    pub fn input_clears_at_eol(&self) -> bool {
        self.metadata.cursor_clear_eol
    }

    pub fn semantic_click(&self) -> SemanticClick {
        match self.metadata.semantic_click {
            [1, relative @ 0..=1] => SemanticClick::Events {
                relative: relative != 0,
            },
            [2, value @ 0..=3] => SemanticClick::CursorKeys {
                motion: match value {
                    0 => ClickMotion::Line,
                    1 => ClickMotion::Multiple,
                    2 => ClickMotion::ConservativeVertical,
                    _ => ClickMotion::SmartVertical,
                },
            },
            _ => SemanticClick::None,
        }
    }

    pub(crate) fn new(cols: usize, rows: usize, limits: ScrollbackLimits) -> Self {
        Self {
            metadata: crate::snapshot::ScreenMetadata::default(),
            graphics: crate::graphics::Graphics::default(),
            columns: cols,
            rows: (0..rows)
                .map(|i| Row::new(i as u64, cols, Color::Default))
                .collect(),
            history: VecDeque::new(),
            cursor: Cursor::default(),
            selection: None,
            viewport_offset: 0,
            viewport_pin_column: 0,
            kitty_keyboard: KittyKeyboard::default(),
            saved_cursor: None,
            charset: CharsetState::default(),
            iso_protection: false,
            limits,
            history_bytes: 0,
            pages: PageList::new(cols as u16, rows),
            cursor_style: None,
            next_row: rows as u64,
            tracked: TrackedPoints::default(),
        }
    }

    pub fn all_rows(&self) -> impl DoubleEndedIterator<Item = &Row> {
        self.history.iter().chain(self.rows.iter())
    }

    /// Native allocation capacities and physical boundaries of the live pages.
    pub fn page_allocations(&self) -> impl Iterator<Item = PageAllocationInfo> + '_ {
        self.pages.allocations()
    }

    pub fn viewport(&self) -> impl Iterator<Item = &Row> {
        let start = self.history.len().saturating_sub(self.viewport_offset);
        self.all_rows().skip(start).take(self.rows.len())
    }

    /// Detached display and selection view of the visible rows. Resolved cell
    /// styles and page boundaries remain available without owning live resource
    /// tables, so shaping and GPU preparation need no session lock.
    pub fn snapshot_viewport(&self) -> Self {
        let mut cursor = self.cursor.clone();
        cursor.row = cursor.row.saturating_add(self.viewport_offset);
        cursor.visible &= cursor.row < self.rows.len();
        cursor.row = cursor.row.min(self.rows.len() - 1);
        Self {
            metadata: self.metadata.clone(),
            graphics: self.graphics.snapshot(self),
            columns: self.columns,
            rows: self
                .viewport()
                .map(|row| {
                    let mut row = row.clone();
                    row.style_page = None;
                    for cell in &mut row.cells {
                        cell.style_id = 0;
                    }
                    row
                })
                .collect(),
            history: VecDeque::new(),
            cursor,
            selection: self.selection,
            viewport_offset: 0,
            viewport_pin_column: 0,
            kitty_keyboard: self.kitty_keyboard.clone(),
            saved_cursor: None,
            charset: self.charset.clone(),
            iso_protection: self.iso_protection,
            limits: ScrollbackLimits::NONE,
            history_bytes: 0,
            pages: self.pages.clone_range(
                self.history.len().saturating_sub(self.viewport_offset),
                self.rows.len(),
            ),
            cursor_style: None,
            next_row: self.next_row,
            tracked: TrackedPoints::default(),
        }
    }

    pub fn scroll_viewport(&mut self, rows: isize) {
        if self.viewport_offset == 0
            || self.viewport_offset.saturating_add_signed(rows) > self.history.len()
        {
            self.viewport_pin_column = 0;
        }
        self.viewport_offset = self
            .viewport_offset
            .saturating_add_signed(rows)
            .min(self.history.len());
        if self.viewport_offset == 0 {
            self.viewport_pin_column = 0;
        }
    }

    /// The stored viewport anchor, including the column retained by a search
    /// scroll. Rows are still displayed starting at column zero.
    pub fn viewport_top(&self) -> GridPoint {
        GridPoint {
            row: self.viewport().next().unwrap().id,
            col: if self.viewport_offset == 0 {
                0
            } else {
                self.viewport_pin_column
            },
        }
    }

    pub fn row_by_id(&self, id: u64) -> Option<&Row> {
        self.all_rows().find(|r| r.id == id)
    }

    pub fn point(&self, row: usize, col: usize) -> Option<GridPoint> {
        let row = self.all_rows().nth(row)?;
        (col < row.cells.len()).then_some(GridPoint { row: row.id, col })
    }

    pub fn track(&mut self, point: GridPoint) -> TrackedPoint {
        use std::sync::atomic::{AtomicU64, Ordering};
        self.tracked.prune();
        // Handles must not alias across screens, terminal resets or clones.
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("tracked point IDs exhausted");
        let valid = self
            .row_by_id(point.row)
            .is_some_and(|r| point.col < r.cells.len());
        self.tracked.0.insert(id, valid.then_some(point));
        TrackedPoint(id)
    }

    pub(crate) fn track_owned(&mut self, point: GridPoint) -> OwnedTrackedPoint {
        let point = self.track(point);
        let owner = std::sync::Arc::new(());
        self.tracked
            .1
            .insert(point.0, std::sync::Arc::downgrade(&owner));
        OwnedTrackedPoint {
            point,
            _owner: owner,
        }
    }

    pub fn resolve(&self, point: TrackedPoint) -> Option<GridPoint> {
        self.tracked.0.get(&point.0).copied().flatten()
    }

    pub fn untrack(&mut self, point: TrackedPoint) {
        self.tracked.0.remove(&point.0);
        self.tracked.1.remove(&point.0);
    }

    pub fn selection_text(&self) -> Option<String> {
        let bytes = self.format_selection(self.selection?, crate::formatter::Options::default())?;
        Some(String::from_utf8(bytes).expect("plain cell formatting is valid UTF-8"))
    }

    fn physical_row_mut(&mut self, absolute: usize) -> &mut Row {
        if absolute < self.history.len() {
            &mut self.history[absolute]
        } else {
            &mut self.rows[absolute - self.history.len()]
        }
    }

    fn release_style(&mut self, serial: u64, id: u16) {
        if id != 0
            && let Some(page) = self
                .pages
                .pages
                .iter_mut()
                .find(|page| page.serial == serial)
        {
            page.styles.release(id);
        }
    }

    pub(crate) fn release_row_styles(&mut self, row: &Row) {
        if let Some(serial) = row.style_page {
            for cell in &row.cells {
                self.release_style(serial, cell.style_id);
            }
        }
    }

    /// Mutations confined to one page only change counts, never the set's
    /// buckets. Rotations can therefore apply their net count changes without
    /// transiently releasing a moved cell's final reference.
    pub(crate) fn edit_row<R>(&mut self, y: usize, edit: impl FnOnce(&mut Row) -> R) -> R {
        self.edit_physical_row(self.history.len() + y, edit)
    }

    fn edit_physical_row<R>(&mut self, absolute: usize, edit: impl FnOnce(&mut Row) -> R) -> R {
        let index = self.pages.page_index(absolute);
        let serial = self.pages.pages[index].serial;
        let row = self.physical_row_mut(absolute);
        row.style_page = Some(serial);
        let mut counts: HashMap<u16, isize> = HashMap::new();
        for cell in &row.cells {
            if cell.style_id != 0 {
                *counts.entry(cell.style_id).or_default() -= 1;
            }
        }
        let result = edit(row);
        for cell in &row.cells {
            if cell.style_id != 0 {
                *counts.entry(cell.style_id).or_default() += 1;
            }
        }
        let styles = &mut self.pages.pages[index].styles;
        for (id, count) in counts {
            if count < 0 {
                for _ in 0..-count {
                    styles.release(id);
                }
            } else {
                for _ in 0..count {
                    styles.retain(id);
                }
            }
        }
        result
    }

    /// Erase a native cell range, retaining background-only blanks inline.
    pub(crate) fn erase_row_cells(
        &mut self,
        y: usize,
        mut start: usize,
        mut end: usize,
        background: Color,
        protected: bool,
    ) {
        let absolute = self.history.len() + y;
        let index = self.pages.page_index(absolute);
        let serial = self.pages.pages[index].serial;
        let row = &mut self.rows[y];
        row.style_page = Some(serial);
        if start >= row.cells.len() || start >= end {
            return;
        }
        end = end.min(row.cells.len());
        if row.cells[start].width == 0 && start > 0 {
            start -= 1;
        }
        if end < row.cells.len() && row.cells[end - 1].width == 2 {
            end += 1;
        }
        for cell in &mut row.cells[start..end] {
            if !protected || !cell.protected {
                self.pages.pages[index].styles.release(cell.style_id);
                *cell = Cell::blank(background);
            }
        }
        row.dirty = true;
    }

    pub(crate) fn release_cursor_style(&mut self) {
        if let Some((serial, id)) = self.cursor_style.take() {
            self.release_style(serial, id);
        }
    }

    pub(crate) fn set_cursor_style(&mut self, value: Style) {
        if self.cursor.style == value {
            return;
        }
        let old = self.cursor.style;
        self.release_cursor_style();
        self.cursor.style = value;
        if !self.install_cursor_style() {
            self.cursor.style = old;
            if !self.install_cursor_style() {
                self.cursor.style = Style::default();
            }
        }
    }

    fn install_cursor_style(&mut self) -> bool {
        if self.cursor.style == Style::default() {
            return true;
        }
        let absolute = self.history.len() + self.cursor.row;
        match self.acquire_style(absolute, self.cursor.style, None) {
            Ok(id) => {
                self.cursor_style = Some((self.pages.page_at(absolute).0.serial, id));
                true
            }
            Err(_) => false,
        }
    }

    /// Cursor page changes own a reference independently of printed cells.
    /// Saved cursor values and detached renderer snapshots own none.
    pub(crate) fn sync_cursor_style(&mut self) {
        let serial = self
            .pages
            .page_at(self.history.len() + self.cursor.row)
            .0
            .serial;
        if let Some((owner, id)) = self.cursor_style
            && owner == serial
            && *self
                .pages
                .page_at(self.history.len() + self.cursor.row)
                .0
                .styles
                .get(id)
                == self.cursor.style
        {
            return;
        }
        self.release_cursor_style();
        if !self.install_cursor_style() {
            self.cursor.style = Style::default();
        }
    }

    pub(crate) fn retain_cursor_style_for_cell(&mut self) -> u16 {
        self.sync_cursor_style();
        let id = self.cursor_style.map_or(0, |(_, id)| id);
        let index = self.pages.page_index(self.history.len() + self.cursor.row);
        self.pages.pages[index].styles.retain(id);
        self.rows[self.cursor.row].style_page = Some(self.pages.pages[index].serial);
        id
    }

    fn acquire_style(
        &mut self,
        absolute: usize,
        style: Style,
        preferred: Option<u16>,
    ) -> Result<u16, SetFull> {
        let index = self.pages.page_index(absolute);
        let acquire = |set: &mut StyleAdmission| match preferred {
            Some(id) if id != 0 => set.acquire_with_id(style, id),
            _ => set.acquire(style),
        };
        match acquire(&mut self.pages.pages[index].styles) {
            Ok(id) => Ok(id),
            Err(error) => {
                if self
                    .grow_style_page(index, error == SetFull::OutOfMemory)
                    .is_err()
                {
                    self.split_style_page(absolute)?;
                }
                let index = self.pages.page_index(absolute);
                acquire(&mut self.pages.pages[index].styles)
            }
        }
    }

    fn exact_style_range_bytes(&self, start: usize, end: usize, columns: u16) -> usize {
        use crate::page_resources::BitmapAllocator;
        use std::collections::HashSet;
        let mut styles = HashSet::new();
        let mut links = HashSet::new();
        let mut linked_cells: usize = 0;
        let mut grapheme_bytes = 0;
        let mut string_bytes = 0;
        for row in self.all_rows().skip(start).take(end - start) {
            for cell in &row.cells {
                if cell.style_id != 0 {
                    styles.insert(cell.style_id);
                }
                let suffix = cell.text.chars().count().saturating_sub(1);
                if suffix != 0 {
                    grapheme_bytes += BitmapAllocator::<16>::bytes_required(suffix * 4).unwrap();
                }
                if let Some(uri) = &cell.hyperlink {
                    linked_cells += 1;
                    let uri = cell.hyperlink_raw.as_deref().unwrap_or(uri.as_bytes());
                    if links.insert((cell.hyperlink_id.as_ref(), uri)) {
                        string_bytes += BitmapAllocator::<32>::bytes_required(uri.len()).unwrap();
                        if let Some(HyperlinkId::Explicit(id)) = &cell.hyperlink_id {
                            string_bytes +=
                                BitmapAllocator::<32>::bytes_required(id.len()).unwrap();
                        }
                    }
                }
            }
        }
        let set_capacity = |count: usize| {
            if count == 0 {
                0
            } else {
                ((count + 1) * 16).div_ceil(13)
            }
        };
        let capacity = crate::page_layout::PageCapacity {
            cols: columns,
            rows: (end - start) as u16,
            styles: set_capacity(styles.len()).min(u16::MAX as usize) as u16,
            hyperlink_bytes: (set_capacity(links.len()).max(linked_cells.div_ceil(16))
                * crate::page_layout::HYPERLINK_ITEM_SIZE) as u16,
            grapheme_bytes: grapheme_bytes as u32,
            string_bytes: string_bytes as u32,
        };
        capacity
            .layout()
            .map_or(usize::MAX, |layout| layout.total_size)
    }

    fn split_style_page(&mut self, absolute: usize) -> Result<(), SetFull> {
        let index = self.pages.page_index(absolute);
        let (page, relative) = self.pages.page_at(absolute);
        let start = absolute - relative;
        let end = start + usize::from(page.rows);
        let above = self.exact_style_range_bytes(start, absolute + 1, page.columns);
        let below = self.exact_style_range_bytes(absolute, end, page.columns);
        let split = if above < below && absolute + 1 < end {
            relative + 1
        } else {
            relative
        };
        let referenced_cursor = self.cursor_style.is_some();
        if !self.pages.split(index, split as u16) {
            return Err(SetFull::OutOfMemory);
        }
        if split != 0 {
            for row in start + split..end {
                self.sync_style_row(row);
            }
        }
        if referenced_cursor {
            self.sync_cursor_style();
        }
        Ok(())
    }

    fn rebuild_style_page<'a>(
        page: &mut Page,
        rows: impl Iterator<Item = &'a mut Row>,
        grow: bool,
    ) -> Result<(), SetFull> {
        let mut capacity = page.capacity;
        if grow {
            let old = capacity.styles;
            if old == u16::MAX {
                return Err(SetFull::OutOfMemory);
            }
            capacity.styles = if old == 0 { 16 } else { old.saturating_mul(2) };
            capacity.layout().map_err(|_| SetFull::OutOfMemory)?;
            let used = page.styles.count() as u64;
            if used != 0 && page.rows != 0 {
                let density = used * u64::from(capacity.rows) / u64::from(page.rows);
                let projected = (density + density / 4)
                    .min(u64::from(old) * 32)
                    .min(u64::from(u16::MAX)) as u16;
                if projected > capacity.styles {
                    let projected_capacity = crate::page_layout::PageCapacity {
                        styles: projected,
                        ..capacity
                    };
                    if projected_capacity.layout().is_ok() {
                        capacity = projected_capacity;
                    }
                }
            }
        }
        let serial = page.serial;
        let mut styles = StyleAdmission::new(capacity.metadata().unwrap().styles_layout);
        let mut pending_ids = Vec::new();
        for row in rows.filter(|row| row.style_page == Some(serial)) {
            for cell in &mut row.cells {
                if cell.style_id != 0 {
                    let id =
                        styles.acquire_with_id(*page.styles.get(cell.style_id), cell.style_id)?;
                    pending_ids.push((&mut cell.style_id, id));
                }
            }
        }
        // Reordering live entries can itself exhaust a collision chain. Until
        // every admission succeeds, cell IDs must still address the old set.
        for (target, id) in pending_ids {
            *target = id;
        }
        page.capacity = capacity;
        page.styles = styles;
        Ok(())
    }

    fn grow_style_page(&mut self, index: usize, grow: bool) -> Result<(), SetFull> {
        let serial = self.pages.pages[index].serial;
        Self::rebuild_style_page(
            &mut self.pages.pages[index],
            self.history.iter_mut().chain(&mut self.rows),
            grow,
        )?;
        if self.cursor_style.is_some_and(|(owner, _)| owner == serial) {
            match self.pages.pages[index].styles.acquire(self.cursor.style) {
                Ok(id) => self.cursor_style = Some((serial, id)),
                Err(_) => {
                    self.cursor_style = None;
                    self.cursor.style = Style::default();
                }
            }
        }
        Ok(())
    }

    fn reflow_style(
        &mut self,
        output: &mut [Row],
        line: &mut Row,
        style: Style,
        source_id: u16,
    ) -> Result<u16, SetFull> {
        let index = self.pages.page_index(output.len());
        line.style_page = Some(self.pages.pages[index].serial);
        if source_id == 0 {
            return Ok(0);
        }
        match self.pages.pages[index]
            .styles
            .acquire_with_id(style, source_id)
        {
            Ok(id) => Ok(id),
            Err(error) => {
                Self::rebuild_style_page(
                    &mut self.pages.pages[index],
                    output.iter_mut().chain(std::iter::once(line)),
                    error == SetFull::OutOfMemory,
                )?;
                self.pages.pages[index]
                    .styles
                    .acquire_with_id(style, source_id)
            }
        }
    }

    /// Rehome complete rows after physical page movement. Same-page rotations
    /// keep their IDs; crossing a page copies with addWithId before releasing
    /// the source. The direction follows the native copy operation.
    pub(crate) fn sync_style_pages(&mut self, reverse: bool) {
        let count = self.rows.len();
        for offset in 0..count {
            let absolute = self.history.len() + if reverse { count - 1 - offset } else { offset };
            self.sync_style_row(absolute);
        }
        self.sync_cursor_style();
    }

    fn sync_style_row(&mut self, absolute: usize) {
        let serial = self.pages.page_at(absolute).0.serial;
        let row = self.all_rows().nth(absolute).unwrap();
        if row.style_page == Some(serial) {
            return;
        }
        let previous = row.style_page;
        let old_styles = previous
            .and_then(|serial| self.pages.pages.iter().find(|page| page.serial == serial))
            .map(|page| &page.styles);
        let styles: Vec<_> = row
            .cells
            .iter()
            .map(|cell| {
                let value = if cell.style_id != 0 {
                    old_styles.map_or(cell.style, |set| *set.get(cell.style_id))
                } else {
                    Style::default()
                };
                (cell.style_id, value)
            })
            .collect();
        let row = self.physical_row_mut(absolute);
        row.style_page = Some(serial);
        for cell in &mut row.cells {
            cell.style_id = 0;
        }
        for (col, (old_id, style)) in styles.into_iter().enumerate() {
            if old_id == 0 {
                continue;
            }
            let id = self
                .acquire_style(absolute, style, Some(old_id))
                .unwrap_or(0);
            self.physical_row_mut(absolute).cells[col].style_id = id;
            if id == 0 {
                self.physical_row_mut(absolute).cells[col].style = Style::default();
            }
            if let Some(previous) = previous {
                self.release_style(previous, old_id);
            }
        }
    }

    pub(crate) fn prepare_row_copy(
        &self,
        source: usize,
        recycled: Option<usize>,
        columns: usize,
        limit: usize,
    ) -> RowCopy {
        let source = &self.rows[source];
        let recycled = recycled.map(|index| &self.rows[index]);
        let mut row = source.clone();
        let end = row.cells.len().min(columns).min(limit);
        row.cells.truncate(end);
        if end < columns {
            if let Some(recycled) = recycled {
                row.cells.extend_from_slice(&recycled.cells[end..columns]);
            } else {
                row.cells.resize(columns, Cell::default());
            }
            row.wrapped = recycled.is_some_and(|row| row.wrapped);
            row.wrap_continuation = recycled.is_some_and(|row| row.wrap_continuation);
        }
        if columns > source.cells.len() {
            row.cells[source.cells.len() - 1].spacer_head = false;
        }
        let styles = row
            .cells
            .iter()
            .enumerate()
            .map(|(column, cell)| {
                let owner = if column < end { Some(source) } else { recycled }
                    .and_then(|row| row.style_page);
                let value = if cell.style_id == 0 {
                    Style::default()
                } else {
                    owner
                        .and_then(|serial| {
                            self.pages.pages.iter().find(|page| page.serial == serial)
                        })
                        .map_or(cell.style, |page| *page.styles.get(cell.style_id))
                };
                (owner, cell.style_id, value)
            })
            .collect();
        RowCopy { row, styles }
    }

    /// A mixed-width IND row combines two source pages. Keep recycled cells
    /// on their destination page before releasing displaced rows, then admit
    /// the copied prefix. Page growth can now remap every retained live cell.
    pub(crate) fn install_row_copies(
        &mut self,
        copies: Vec<(usize, RowCopy)>,
        discarded: Option<Row>,
    ) {
        let mut pending = Vec::new();
        let mut displaced = Vec::new();
        for (y, mut copy) in copies {
            let index = self.pages.page_index(self.history.len() + y);
            let serial = self.pages.pages[index].serial;
            copy.row.style_page = Some(serial);
            for (cell, &(owner, id, _)) in copy.row.cells.iter_mut().zip(&copy.styles) {
                if owner == Some(serial) {
                    self.pages.pages[index].styles.retain(id);
                } else {
                    cell.style_id = 0;
                }
            }
            displaced.push(std::mem::replace(&mut self.rows[y], copy.row));
            pending.push((y, serial, copy.styles));
        }
        for row in displaced.iter().chain(discarded.iter()) {
            self.release_row_styles(row);
        }
        for (y, serial, styles) in pending {
            for (column, (owner, source_id, style)) in styles.into_iter().enumerate() {
                if source_id == 0 || owner == Some(serial) {
                    continue;
                }
                let id = self
                    .acquire_style(self.history.len() + y, style, Some(source_id))
                    .unwrap_or(0);
                self.rows[y].cells[column].style_id = id;
                if id == 0 {
                    self.rows[y].cells[column].style = Style::default();
                }
            }
        }
    }

    pub(crate) fn copy_row_cells(
        &mut self,
        source: usize,
        destination: usize,
        start: usize,
        end: usize,
        background: Color,
    ) {
        let end = end.min(self.rows[destination].cells.len());
        if start >= end {
            return;
        }
        let source_index = self.pages.page_index(self.history.len() + source);
        let source_serial = self.pages.pages[source_index].serial;
        let destination_serial = self
            .pages
            .page_at(self.history.len() + destination)
            .0
            .serial;
        let cells: Vec<_> = (start..end)
            .map(|col| {
                let cell = self.rows[source]
                    .cells
                    .get(col)
                    .cloned()
                    .unwrap_or_else(|| Cell::blank(background));
                let style = if cell.style_id == 0 {
                    Style::default()
                } else {
                    *self.pages.pages[source_index].styles.get(cell.style_id)
                };
                (cell, style)
            })
            .collect();
        self.edit_row(destination, |row| {
            row.cells[start..end].fill(Cell::default())
        });
        for (offset, (mut cell, style)) in cells.into_iter().enumerate() {
            if cell.style_id != 0 {
                let index = self.pages.page_index(self.history.len() + destination);
                if source_serial == destination_serial {
                    self.pages.pages[index].styles.retain(cell.style_id);
                } else {
                    cell.style_id = self
                        .acquire_style(self.history.len() + destination, style, Some(cell.style_id))
                        .unwrap_or(0);
                    if cell.style_id == 0 {
                        cell.style = Style::default();
                    }
                }
            }
            self.rows[destination].cells[start + offset] = cell;
        }
        self.edit_row(destination, |row| row.repair_wide(background));
    }

    fn resize_owned_columns(
        &mut self,
        contents: &mut [Row],
        columns: usize,
        old_columns: usize,
        spacer_heads: &[bool],
    ) {
        let old_pages = self.pages.clone();
        for row in &mut *contents {
            let mut counts: HashMap<u16, isize> = HashMap::new();
            for cell in &row.cells {
                if cell.style_id != 0 {
                    *counts.entry(cell.style_id).or_default() -= 1;
                }
            }
            row.cells.resize(columns, Cell::default());
            row.repair_wide(Color::Default);
            if columns > old_columns {
                row.wrapped = false;
            }
            for cell in &row.cells {
                if cell.style_id != 0 {
                    *counts.entry(cell.style_id).or_default() += 1;
                }
            }
            if let Some(serial) = row.style_page {
                for (id, count) in counts {
                    for _ in 0..-count {
                        self.release_style(serial, id);
                    }
                }
            }
        }
        self.pages.resize_columns(columns as u16, spacer_heads);
        Self::rehome_contents(&mut self.pages, contents, &old_pages);
    }

    fn rehome_contents(pages: &mut PageList, contents: &mut [Row], old_pages: &PageList) {
        for absolute in 0..contents.len() {
            let index = pages.page_index(absolute);
            let serial = pages.pages[index].serial;
            let row = &mut contents[absolute];
            if row.style_page == Some(serial) {
                continue;
            }
            let previous = row.style_page;
            let source =
                previous.and_then(|owner| old_pages.pages.iter().find(|page| page.serial == owner));
            let styles: Vec<_> = row
                .cells
                .iter_mut()
                .map(|cell| {
                    let id = std::mem::take(&mut cell.style_id);
                    (
                        id,
                        if id == 0 {
                            Style::default()
                        } else {
                            source.map_or(cell.style, |page| *page.styles.get(id))
                        },
                    )
                })
                .collect();
            row.style_page = Some(serial);
            for (col, (source_id, style)) in styles.into_iter().enumerate() {
                if source_id == 0 {
                    continue;
                }
                let acquired = pages.pages[index].styles.acquire_with_id(style, source_id);
                let id = match acquired {
                    Ok(id) => id,
                    Err(error) => Self::rebuild_style_page(
                        &mut pages.pages[index],
                        contents.iter_mut(),
                        error == SetFull::OutOfMemory,
                    )
                    .and_then(|()| pages.pages[index].styles.acquire_with_id(style, source_id))
                    .unwrap_or(0),
                };
                contents[absolute].cells[col].style_id = id;
                if id == 0 {
                    contents[absolute].cells[col].style = Style::default();
                }
                if let Some(owner) = previous
                    && let Some(page) = pages.pages.iter_mut().find(|page| page.serial == owner)
                {
                    page.styles.release(source_id);
                }
            }
        }
    }

    pub(crate) fn blank_row(&mut self, cols: usize, background: Color) -> Row {
        let id = self.next_row;
        self.next_row = self.next_row.wrapping_add(1);
        Row::new(id, cols, background)
    }

    pub(crate) fn extend_physical_row(&mut self, row: usize, columns: usize) {
        let history = self.history.len();
        let absolute = history + row;
        let (page, page_row) = self.pages.page_at(absolute);
        if columns <= usize::from(page.columns) {
            return;
        }
        let spacer_head = self
            .all_rows()
            .skip(absolute - page_row)
            .take(usize::from(page.rows))
            .any(|row| row.cells.last().is_some_and(|cell| cell.spacer_head));
        let copy_rows = columns > usize::from(page.capacity.cols) || spacer_head;
        self.release_cursor_style();
        let old_pages = self.pages.clone();
        let range = self
            .pages
            .extend_page(absolute, columns as u16, spacer_head);
        let mut contents: Vec<_> = self.history.drain(..).chain(self.rows.drain(..)).collect();
        for row in &mut contents[range.clone()] {
            let old_ids: Vec<_> = row.cells.iter().map(|cell| cell.style_id).collect();
            row.cells.resize(columns, Cell::default());
            row.repair_wide(Color::Default);
            if copy_rows {
                // Native page copying into a wider blank row preserves the
                // destination's wrap flags because the copied range is partial.
                row.wrapped = false;
                row.wrap_continuation = false;
            }
            if let Some(serial) = row.style_page {
                for (old_id, cell) in old_ids.into_iter().zip(&row.cells) {
                    if old_id != cell.style_id {
                        self.release_style(serial, old_id);
                    }
                }
            }
        }
        Self::rehome_contents(&mut self.pages, &mut contents, &old_pages);
        self.rows = contents.split_off(history);
        self.history = contents.into();
        if range.start < history {
            self.history_bytes = self.history.iter().map(Row::storage_bytes).sum();
        }
        self.sync_cursor_style();
    }

    pub(crate) fn cursor_reset_wrap(&mut self) {
        self.cursor.pending_wrap = false;
        let y = self.cursor.row;
        if !self.rows[y].wrapped {
            return;
        }
        self.rows[y].wrapped = false;
        if let Some(next) = self.rows.get_mut(y + 1) {
            next.wrap_continuation = false;
        }
        let columns = self.rows[y].cells.len();
        if self.rows[y].cells[columns - 1].spacer_head {
            self.erase_row_cells(y, columns - 1, columns, self.cursor.style.background, false);
        }
    }

    pub(crate) fn split_cell_boundary(&mut self, col: usize) {
        let y = self.cursor.row;
        let cols = self.rows[y].cells.len();
        let background = self.cursor.style.background;
        if col >= cols {
            if col == cols && self.rows[y].wrapped && self.rows[y].cells[cols - 1].spacer_head {
                self.erase_row_cells(y, cols - 1, cols, background, false);
            }
            return;
        }
        if col <= 1 && self.rows[y].cells[0].width == 2 {
            let absolute = self.history.len() + y;
            if absolute > 0 {
                let previous = self.all_rows().nth(absolute - 1).unwrap();
                if previous.wrapped && previous.cells.last().is_some_and(|cell| cell.spacer_head) {
                    let before = previous.storage_bytes();
                    self.edit_physical_row(absolute - 1, |previous| {
                        let width = previous.cells.len();
                        previous.erase(width - 1, width, background, false);
                    });
                    if y == 0 {
                        self.history_bytes = self.history_bytes.saturating_sub(
                            before.saturating_sub(self.history.back().unwrap().storage_bytes()),
                        );
                    }
                }
            }
        }
        if col > 0 && self.rows[y].cells[col - 1].width == 2 {
            self.erase_row_cells(y, col - 1, col + 1, background, false);
        }
    }

    /// Move external pins and selection endpoints independently of row contents.
    /// Line edits retain physical coordinates; history scrolling moves them.
    pub(crate) fn remap_grid_rows(&mut self, rows: &HashMap<u64, u64>) {
        for point in self.grid_points_mut() {
            if let Some(&row) = rows.get(&point.row) {
                point.row = row;
            }
        }
    }

    pub(crate) fn grid_points_mut(&mut self) -> impl Iterator<Item = &mut GridPoint> {
        self.tracked.prune();
        self.tracked.0.values_mut().flatten().chain(
            self.selection
                .iter_mut()
                .flat_map(|selection| [&mut selection.start, &mut selection.end]),
        )
    }

    pub(crate) fn discard_row(&mut self, id: u64) {
        self.graphics.discard_row(id);
        self.tracked.prune();
        for point in self.tracked.0.values_mut() {
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
        if self.limits.bytes == Some(0) {
            self.release_row_styles(&row);
            self.discard_row(row.id);
            return;
        }
        self.retain_history(row);
    }

    // ED22 grows native page storage even when ordinary scrollback is disabled.
    pub(crate) fn retain_history(&mut self, row: Row) {
        self.history_bytes = self.history_bytes.saturating_add(row.storage_bytes());
        self.history.push_back(row);
        if self.viewport_offset > 0 {
            self.viewport_offset += 1;
        }
        let removed = self
            .pages
            .grow(self.columns as u16, self.rows.len(), self.limits);
        self.discard_history_prefix(removed);
    }

    /// Charged bytes in history rows; container spare capacity and graphics are
    /// excluded. Text and hyperlink allocations are charged at their capacity.
    pub fn history_bytes(&self) -> usize {
        self.history_bytes
    }

    /// Logical native page allocation charge, including active and history pages.
    pub fn storage_bytes(&self) -> usize {
        self.pages.allocation_bytes()
    }

    pub(crate) fn set_limits(&mut self, limits: ScrollbackLimits) {
        self.limits = limits;
        if limits.bytes == Some(0) {
            self.clear_history();
        } else {
            self.enforce_limits();
        }
        self.history.shrink_to_fit();
    }

    pub(crate) fn clear_history(&mut self) {
        self.pages.remove_prefix(self.history.len());
        self.discard_history_prefix(self.history.len());
        self.viewport_offset = 0;
        self.viewport_pin_column = 0;
    }

    pub(crate) fn effective_limits(&self) -> ScrollbackLimits {
        PageList::effective_limits(self.columns as u16, self.rows.len(), self.limits)
    }

    pub(crate) fn enforce_limits(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let removed = self.pages.prune(self.rows.len(), self.effective_limits());
        self.discard_history_prefix(removed);
    }

    fn discard_history_prefix(&mut self, count: usize) {
        if count > self.history.len().saturating_sub(self.viewport_offset) {
            self.viewport_pin_column = 0;
        }
        for _ in 0..count {
            let row = self.history.pop_front().unwrap();
            self.release_row_styles(&row);
            self.history_bytes = self.history_bytes.saturating_sub(row.storage_bytes());
            self.discard_row(row.id);
        }
        self.viewport_offset = self.viewport_offset.min(self.history.len());
    }

    pub(crate) fn clear_prompt_for_redraw(&mut self, redraw: PromptRedraw) {
        if self.cursor.semantic == SemanticContent::Output {
            return;
        }
        let cursor = self.history.len() + self.cursor.row;
        let (start, end) = match redraw {
            PromptRedraw::None => return,
            PromptRedraw::Last => (cursor, cursor + 1),
            PromptRedraw::All => {
                let mut previous = self
                    .all_rows()
                    .rev()
                    .skip(self.rows.len() - self.cursor.row - 1);
                let Some((offset, row)) = previous
                    .by_ref()
                    .enumerate()
                    .find(|(_, row)| row.semantic != SemanticContent::Output)
                else {
                    return;
                };
                let found = cursor - offset;
                let start = if row.semantic == SemanticContent::Input {
                    previous
                        .enumerate()
                        .find_map(|(offset, row)| match row.semantic {
                            SemanticContent::Prompt => Some(found - offset - 1),
                            SemanticContent::Output => Some(found - offset),
                            SemanticContent::Input => None,
                        })
                        // Native prompt iteration keeps its starting continuation
                        // when the entire preceding history is a continuation.
                        .unwrap_or(found)
                } else {
                    found
                };
                (start, self.history.len() + self.rows.len())
            }
        };
        for absolute in start..end {
            self.edit_physical_row(absolute, |row| {
                // Native resize releases the cursor style before clearing.
                row.cells.fill(Cell::default());
                row.dirty = true;
            });
        }
        if start < self.history.len() {
            self.history_bytes = self.history.iter().map(Row::storage_bytes).sum();
        }
    }

    pub(crate) fn resize(&mut self, cols: usize, rows: usize, reflow: bool) {
        self.tracked.prune();
        let mut viewport_point = (self.viewport_offset > 0).then(|| self.viewport_top());
        if viewport_point.is_none() {
            self.viewport_pin_column = 0;
        }
        self.release_cursor_style();
        let old_cols = self.columns;
        let columns_changed = cols != old_cols;
        self.columns = cols;
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
        let height_first = reflow && cols <= old_cols;
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
        if columns_changed && reflow {
            let source_pages = std::mem::take(&mut self.pages);
            let first_capacity = source_pages
                .pages
                .front()
                .unwrap()
                .adjusted_capacity(cols as u16, false);
            self.pages.append(first_capacity, 1);
            let height = if height_first { rows } else { old_rows };
            let active_start = contents.len().saturating_sub(height);
            let old_wrapped = contents
                [active_start..(cursor_index + 1).min(contents.len()).max(active_start)]
                .iter()
                .filter(|r| r.wrap_continuation)
                .count();
            let mut map = HashMap::<GridPointKey, GridPoint>::new();
            let mut output = Vec::new();
            let mut line = self.blank_row(cols, Color::Default);
            let mut x: usize = 0;
            let mut pin_x: usize = 0;
            for (old_index, old) in contents.iter().enumerate() {
                let source_page = source_pages.page_at(old_index).0;
                let capacity = source_page.adjusted_capacity(cols as u16, true);
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
                if let Some(point) = &mut viewport_point {
                    keep_pin(point);
                }
                for point in self.tracked.0.values_mut().flatten() {
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
                if used > 0 {
                    while self.pages.total_rows() <= output.len() {
                        self.pages.reflow_row(capacity);
                    }
                }
                line.semantic = old.semantic;
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
                        line.semantic = old.semantic;
                        line.wrap_continuation = true;
                        x = 0;
                        while self.pages.total_rows() <= output.len() {
                            self.pages.reflow_row(capacity);
                        }
                    }
                    map.insert(
                        (old.id, old_col),
                        spacer.unwrap_or(GridPoint {
                            row: line.id,
                            col: x,
                        }),
                    );
                    let source_style = if cell.style_id == 0 {
                        Style::default()
                    } else {
                        *source_page.styles.get(cell.style_id)
                    };
                    let native_id = self
                        .reflow_style(&mut output, &mut line, source_style, cell.style_id)
                        .unwrap_or(0);
                    let mut cell = cell.clone();
                    cell.style_id = native_id;
                    if native_id == 0 && source_style != Style::default() {
                        cell.style = Style::default();
                    }
                    if cols == 1 && cell.width == 2 {
                        cell.text.clear();
                    }
                    cell.width = width as u8;
                    line.cells[x] = cell.clone();
                    if width == 2 {
                        let index = self.pages.page_index(output.len());
                        self.pages.pages[index].styles.retain(cell.style_id);
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
            viewport_point = viewport_point.and_then(|p| map.get(&(p.row, p.col)).copied());
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
            // Reflow uses blank rows below the content before pushing live text
            // into history. Cursor and tracked references keep their blank rows.
            while output.len() > 1
                && output.last().is_some_and(|r| {
                    r.id != mapped_cursor.row
                        && !saved_point.is_some_and(|p| p.row == r.id)
                        && !viewport_point.is_some_and(|p| p.row == r.id)
                        && !self
                            .tracked
                            .0
                            .values()
                            .any(|p| p.is_some_and(|p| p.row == r.id))
                        && r.used() == 0
                        && r.semantic == SemanticContent::Output
                })
            {
                let row = output.pop().unwrap();
                self.release_row_styles(&row);
            }
            if self.pages.total_rows() > output.len() {
                self.pages.truncate(output.len());
            }
            while output.len() < height {
                self.grow_contents(&mut output, cols, height);
            }
            let start = output.len() - height;
            if let Some(cursor_index) = output
                .iter()
                .position(|r| r.id == mapped_cursor.row)
                .filter(|&i| i >= start)
            {
                let wrapped = output[start..=cursor_index]
                    .iter()
                    .filter(|r| r.wrap_continuation)
                    .count();
                let remaining = height.saturating_sub(cursor_y + 1);
                let current = output.len() - cursor_index - 1;
                let grow = remaining
                    .saturating_sub(wrapped.saturating_sub(old_wrapped))
                    .saturating_sub(current);
                for _ in 0..grow {
                    self.grow_contents(&mut output, cols, height);
                }
            }
            contents = output;
            self.graphics.reflow(&map);
        } else if columns_changed {
            let mut start = 0;
            let spacer_heads: Vec<_> = self
                .pages
                .pages
                .iter()
                .map(|page| {
                    let end = start + usize::from(page.rows);
                    let has_head = contents[start..end]
                        .iter()
                        .any(|row| row.cells.last().is_some_and(|cell| cell.spacer_head));
                    start = end;
                    has_head
                })
                .collect();
            self.resize_owned_columns(&mut contents, cols, old_cols, &spacer_heads);
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
            self.grow_contents(&mut contents, cols, rows);
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
        self.cursor.col = self
            .cursor
            .col
            .min(self.rows[self.cursor.row].cells.len() - 1);
        self.history_bytes = contents.iter().map(Row::storage_bytes).sum();
        self.history = contents.into();
        if self.limits.bytes == Some(0) {
            self.clear_history();
        } else {
            let removed = self.pages.prune(
                rows,
                ScrollbackLimits {
                    bytes: None,
                    ..self.effective_limits()
                },
            );
            self.discard_history_prefix(removed);
        }
        self.viewport_offset = self.viewport_offset.min(self.history.len());
        if let Some(point) = viewport_point {
            let index = self.all_rows().position(|row| row.id == point.row);
            if let Some(index) = index {
                self.viewport_offset = self.history.len().saturating_sub(index);
                self.viewport_pin_column = if self.viewport_offset > 0 {
                    point.col.min(cols - 1)
                } else {
                    0
                };
            } else {
                self.viewport_offset = self.history.len();
                self.viewport_pin_column = 0;
            }
        }
        // Native resize reattaches the cursor hyperlink to its new page,
        // assigning a new implicit identity while printed links keep theirs.
        if matches!(self.cursor.hyperlink_id, Some(HyperlinkId::Implicit(_))) {
            let id = self.metadata.hyperlink_implicit_id;
            self.cursor.hyperlink_id = Some(HyperlinkId::Implicit(id));
            self.metadata.hyperlink_implicit_id = id.wrapping_add(1);
        }
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
                        .0
                        .values()
                        .any(|p| p.is_some_and(|p| p.row == r.id))
                    && r.cells.iter().all(|c| c.text.is_empty())
            })
        {
            let row = contents.pop().unwrap();
            self.release_row_styles(&row);
            trim -= 1;
        }
        if self.pages.total_rows() > contents.len() {
            self.pages.truncate(contents.len());
        }
        if rows > old_rows && self.cursor.row < old_rows - 1 {
            for _ in 0..rows - old_rows {
                self.grow_contents(contents, cols, rows);
            }
        }
        while contents.len() < rows {
            self.grow_contents(contents, cols, rows);
        }
    }

    fn grow_contents(&mut self, contents: &mut Vec<Row>, columns: usize, active_rows: usize) {
        let removed = self.pages.grow(columns as u16, active_rows, self.limits);
        let width = usize::from(self.pages.pages.back().unwrap().columns);
        contents.push(self.blank_row(width, Color::Default));
        for row in contents.drain(..removed) {
            self.release_row_styles(&row);
            self.discard_row(row.id);
        }
    }
}

type GridPointKey = (u64, usize);

#[cfg(test)]
mod style_tests {
    use super::*;
    use crate::Terminal;
    use crate::page_layout::PageCapacity;

    #[test]
    fn failed_style_rebuild_preserves_original_ids_and_references() {
        let rgb = |value: u32| Style {
            foreground: Color::Rgb(value as u8, (value >> 8) as u8, (value >> 16) as u8),
            ..Style::default()
        };
        let ordinary = (0..)
            .map(rgb)
            .find(|style| style.native_hash() & 127 > 32)
            .unwrap();
        let colliding: Vec<_> = (0..)
            .map(rgb)
            .filter(|style| style.native_hash() & 255 == 0)
            .take(32)
            .collect();
        let capacity = PageCapacity {
            cols: 80,
            rows: 2,
            ..PageCapacity::STANDARD
        };
        let mut pages = PageList::default();
        pages.append(capacity, 2);
        let mut original = pages.pages.pop_front().unwrap();
        let ordinary_id = original.styles.acquire(ordinary).unwrap();
        let mut rows = [
            Row::new(0, 80, Color::Default),
            Row::new(1, 80, Color::Default),
        ];
        for row in &mut rows {
            row.style_page = Some(original.serial);
        }
        for (cell, style) in rows[0].cells.iter_mut().zip(colliding) {
            cell.style = style;
            cell.style_id = original.styles.acquire(style).unwrap();
        }
        rows[1].cells[0].style = ordinary;
        rows[1].cells[0].style_id = ordinary_id;
        let ids: Vec<_> = rows
            .iter()
            .flat_map(|row| row.cells.iter().map(|cell| cell.style_id))
            .collect();

        // The original admission order fits. Cloning in row order reaches
        // PSL31 before the final noncolliding style, at either table size.
        for grow in [false, true] {
            let mut page = original.clone();
            let mut copied_rows = rows.clone();
            assert_eq!(
                Screen::rebuild_style_page(&mut page, copied_rows.iter_mut(), grow),
                Err(SetFull::OutOfMemory)
            );
            assert_eq!(page.capacity, capacity);
            assert_eq!(
                copied_rows
                    .iter()
                    .flat_map(|row| row.cells.iter().map(|cell| cell.style_id))
                    .collect::<Vec<_>>(),
                ids
            );
            for cell in copied_rows
                .iter()
                .flat_map(|row| &row.cells)
                .filter(|cell| cell.style_id != 0)
            {
                assert_eq!(*page.styles.get(cell.style_id), cell.style);
                assert_eq!(page.styles.reference_count(cell.style_id), 1);
            }
        }
    }

    #[test]
    fn viewport_keeps_resolved_styles_without_allocating_live_tables() {
        let mut terminal = Terminal::new(8, 2, 20);
        let page = &mut terminal.screen_mut().pages.pages[0];
        page.capacity.styles = u16::MAX;
        page.styles = StyleAdmission::new(page.capacity.metadata().unwrap().styles_layout);
        terminal.feed(b"\x1b[1;31mred word\x1b[0m");
        let before = crate::snapshot::encode_to_vec(&terminal).unwrap();
        let source = terminal.screen();
        assert_eq!(source.pages.pages[0].styles.allocated_buckets(), 65536);

        let viewport = source.snapshot_viewport();
        assert_eq!(viewport.pages.pages[0].styles.allocated_buckets(), 0);
        assert_eq!(
            viewport.pages.pages[0].capacity,
            source.pages.pages[0].capacity
        );
        assert_eq!(viewport.pages.pages[0].rows, source.pages.pages[0].rows);
        for (row, original) in viewport.rows.iter().zip(&source.rows) {
            assert_eq!(row.cells, original.cells);
            assert_eq!(row.style_page, None);
            assert!(row.cells.iter().all(|cell| cell.style_id == 0));
        }
        let point = viewport.point(0, 1).unwrap();
        assert_eq!(
            viewport.select_word(point, crate::selection::DEFAULT_WORD_BOUNDARIES),
            source.select_word(point, crate::selection::DEFAULT_WORD_BOUNDARIES)
        );
        assert_eq!(
            viewport.search_literal(b"red"),
            source.search_literal(b"red")
        );
        assert_eq!(crate::snapshot::encode_to_vec(&terminal).unwrap(), before);
        assert_references(source);
    }

    fn assert_references(screen: &Screen) {
        let mut expected: HashMap<(u64, u16), usize> = HashMap::new();
        for (absolute, row) in screen.all_rows().enumerate() {
            let serial = screen.pages.page_at(absolute).0.serial;
            for cell in &row.cells {
                if cell.style_id != 0 {
                    assert_eq!(row.style_page, Some(serial));
                    *expected.entry((serial, cell.style_id)).or_default() += 1;
                }
            }
        }
        if let Some((serial, id)) = screen.cursor_style {
            assert_eq!(
                serial,
                screen
                    .pages
                    .page_at(screen.history.len() + screen.cursor.row)
                    .0
                    .serial
            );
            *expected.entry((serial, id)).or_default() += 1;
        }
        for page in &screen.pages.pages {
            for (id, _) in page.styles.iter() {
                assert_eq!(
                    usize::from(page.styles.reference_count(id)),
                    expected.remove(&(page.serial, id)).unwrap_or(0),
                    "page {} style {id}",
                    page.serial
                );
            }
        }
        assert!(expected.is_empty(), "cells referenced a dead style");
    }

    #[test]
    fn live_style_references_survive_grid_edits_and_resize() {
        for columns in [8, 80, 1024] {
            let mut terminal = Terminal::with_limits(columns, 4, ScrollbackLimits::default());
            for value in 0..160 {
                terminal.feed(format!("\x1b[38;2;{value};0;0mX").as_bytes());
            }
            for sequence in [
                b"\x1b[H\x1b[2@".as_slice(),
                b"\x1b[2P",
                b"\x1b[2X",
                b"\x1b[2S",
                b"\x1b[2T",
                b"\x1b[2;4r\x1b[4;1H\n",
                b"\x1b[r\x1b[H\x1b[44m\x1b[2J",
                b"\x1b#8",
                b"\x1b[22J",
                b"\x1b[3J",
            ] {
                terminal.feed(sequence);
                assert_references(terminal.screen());
            }
            for width in [12, 3, 32, columns] {
                terminal.resize(width, 5);
                assert_references(terminal.screen());
                terminal.feed(b"\x1b[1;31mwide:\xe7\x95\x8c\x1b[0m\r\n");
                assert_references(terminal.screen());
            }
            terminal.feed(b"\x1b[?7l");
            for width in [3, 20, columns] {
                terminal.resize(width, 3);
                assert_references(terminal.screen());
            }
        }
    }
}
