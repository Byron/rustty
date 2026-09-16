//! Owned screen storage. Rows retain identity when they enter scrollback.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[path = "screen/resize.rs"]
mod resize;
#[path = "screen/serde.rs"]
mod serde_impl;
#[cfg(test)]
#[path = "screen/tests.rs"]
mod tests;

use crate::page_list::{Page, PageAllocationInfo, PageList};
use crate::page_resources::{GraphemeAllocation, HyperlinkKey, SetFull, StyleAdmission};

pub use crate::packed::Cell;
use crate::packed::RowHeader;
use crate::page::{CellCopy, PageResource};

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

/// Immutable OSC 8 metadata shared by the cursor, cells and viewport copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HyperlinkData {
    pub uri: String,
    pub id: Option<HyperlinkId>,
    /// Present only when the URI contains bytes that are not UTF-8.
    pub raw: Option<Vec<u8>>,
}

impl HyperlinkData {
    pub fn new(uri: &[u8], id: Option<HyperlinkId>) -> Self {
        Self {
            uri: String::from_utf8_lossy(uri).into_owned(),
            id,
            raw: std::str::from_utf8(uri).is_err().then(|| uri.to_vec()),
        }
    }

    pub fn uri_bytes(&self) -> &[u8] {
        self.raw.as_deref().unwrap_or(self.uri.as_bytes())
    }

    pub(crate) fn storage_bytes(&self) -> usize {
        (size_of::<Self>() + 2 * size_of::<usize>())
            .saturating_add(self.uri.capacity())
            .saturating_add(self.raw.as_ref().map_or(0, Vec::capacity))
            .saturating_add(match &self.id {
                Some(HyperlinkId::Explicit(id)) => id.capacity(),
                _ => 0,
            })
    }
}

// Keep the existing flat Cell/Cursor JSON fields without cloning their payloads.
mod hyperlink_serde {
    use super::{Arc, HyperlinkData, HyperlinkId};
    use serde::{Deserialize, Deserializer, Serializer, ser::SerializeMap};

    pub fn serialize<S: Serializer>(
        link: &Option<Arc<HyperlinkData>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serialize_data(link.as_deref(), serializer)
    }

    pub fn serialize_data<S: Serializer>(
        link: Option<&HyperlinkData>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("hyperlink", &link.as_ref().map(|link| &link.uri))?;
        map.serialize_entry(
            "hyperlink_id",
            &link.as_ref().and_then(|link| link.id.as_ref()),
        )?;
        map.serialize_entry(
            "hyperlink_raw",
            &link.as_ref().and_then(|link| link.raw.as_ref()),
        )?;
        map.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Arc<HyperlinkData>>, D::Error> {
        #[derive(Deserialize)]
        struct Fields {
            hyperlink: Option<String>,
            hyperlink_id: Option<HyperlinkId>,
            hyperlink_raw: Option<Vec<u8>>,
        }
        let fields = Fields::deserialize(deserializer)?;
        Ok(fields.hyperlink.map(|uri| {
            Arc::new(HyperlinkData {
                uri,
                id: fields.hyperlink_id,
                raw: fields.hyperlink_raw,
            })
        }))
    }
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

/// A cell's text, borrowing page-owned clusters or encoding one inline scalar.
#[derive(Clone, Debug)]
pub struct CellText<'a>(CellTextStorage<'a>);

// Keep encoded bytes private: only `scalar` can construct them.
#[derive(Clone, Debug)]
enum CellTextStorage<'a> {
    Scalar {
        codepoint: Option<char>,
        bytes: [u8; 4],
        len: u8,
    },
    Grapheme(&'a str),
}

impl CellText<'_> {
    #[inline]
    fn scalar(codepoint: Option<char>) -> Self {
        let mut bytes = [0; 4];
        let len = codepoint.map_or(0, |cp| cp.encode_utf8(&mut bytes).len()) as u8;
        Self(CellTextStorage::Scalar {
            codepoint,
            bytes,
            len,
        })
    }

    /// Iterate inline scalars directly; only graphemes need UTF-8 decoding.
    #[inline]
    pub fn chars(&self) -> impl DoubleEndedIterator<Item = char> + Clone + '_ {
        let (scalar, text) = match &self.0 {
            CellTextStorage::Scalar { codepoint, .. } => (*codepoint, ""),
            CellTextStorage::Grapheme(text) => (None, *text),
        };
        scalar.into_iter().chain(text.chars())
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        match &self.0 {
            CellTextStorage::Scalar { bytes, len, .. } => {
                // SAFETY: private storage is built only by `scalar`, using
                // char::encode_utf8 (or an empty slice for an empty cell).
                unsafe { std::str::from_utf8_unchecked(&bytes[..usize::from(*len)]) }
            }
            CellTextStorage::Grapheme(text) => text,
        }
    }
}

impl std::ops::Deref for CellText<'_> {
    type Target = str;
    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

/// A borrowed row with one resolved resource owner. Cell storage is read-only.
#[derive(Clone, Copy, Debug)]
pub struct RowView<'a> {
    pub id: u64,
    pub cells: &'a [Cell],
    pub wrapped: bool,
    pub wrap_continuation: bool,
    pub semantic: SemanticContent,
    pub dirty: bool,
    pub(crate) page: &'a Page,
    pub(crate) offset: usize,
}

pub type Row<'a> = RowView<'a>;

pub struct Rows<'a> {
    pages: &'a PageList,
    front: (usize, usize),
    back: (usize, usize),
    remaining: usize,
}

impl<'a> Rows<'a> {
    fn new(pages: &'a PageList, start: usize, len: usize) -> Self {
        Self {
            pages,
            front: if len > 0 { pages.locate(start) } else { (0, 0) },
            back: if len > 0 {
                pages.locate(start + len - 1)
            } else {
                (0, 0)
            },
            remaining: len,
        }
    }
}

impl<'a> Iterator for Rows<'a> {
    type Item = Row<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let page = &self.pages.pages[self.front.0];
        let row = page.row(self.front.1);
        self.remaining -= 1;
        self.front.1 += 1;
        if self.front.1 == usize::from(page.rows) {
            self.front = (self.front.0 + 1, 0);
        }
        Some(row)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl DoubleEndedIterator for Rows<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let row = self.pages.pages[self.back.0].row(self.back.1);
        self.remaining -= 1;
        if self.back.1 > 0 {
            self.back.1 -= 1;
        } else if self.back.0 > 0 {
            self.back.0 -= 1;
            self.back.1 = usize::from(self.pages.pages[self.back.0].rows) - 1;
        }
        Some(row)
    }
}
impl ExactSizeIterator for Rows<'_> {}

impl<'a> From<&RowView<'a>> for RowView<'a> {
    #[inline]
    fn from(row: &RowView<'a>) -> Self {
        *row
    }
}

impl<'a> RowView<'a> {
    #[inline]
    pub(crate) fn new(page: &'a Page, row: usize) -> Self {
        let header = page.headers[row];
        Self {
            id: page.row_ids[row],
            cells: page.row_cells(row),
            wrapped: header.has(RowHeader::WRAPPED),
            wrap_continuation: header.has(RowHeader::CONTINUATION),
            semantic: header.semantic(),
            dirty: header.has(RowHeader::DIRTY),
            page,
            offset: header.offset(),
        }
    }
    #[inline]
    pub fn cells(self) -> &'a [Cell] {
        self.cells
    }
    #[inline]
    pub fn text(self, col: usize) -> CellText<'a> {
        let cell = self.cells[col];
        let codepoint = cell.codepoint();
        if cell.has_grapheme() && codepoint.is_some() {
            CellText(CellTextStorage::Grapheme(self.grapheme_text(col)))
        } else {
            CellText::scalar(codepoint)
        }
    }
    fn grapheme_text(self, col: usize) -> &'a str {
        let allocation = self.page.grapheme_map[&((self.offset + col) as u32)];
        self.page.graphemes.text(allocation)
    }
    #[inline]
    pub fn style(self, col: usize) -> Style {
        self.page.style(self.offset + col)
    }
    #[inline]
    pub fn hyperlink(self, col: usize) -> Option<&'a HyperlinkData> {
        self.page.hyperlink(self.offset + col).map(Arc::as_ref)
    }
    #[inline]
    pub(crate) fn grapheme(self, col: usize) -> Option<GraphemeAllocation> {
        self.page.grapheme(self.offset + col)
    }
    pub(crate) fn copy_cell(self, col: usize) -> CellCopy {
        self.page.copy_cell(self.offset + col)
    }
    pub(crate) fn used(self) -> usize {
        self.cells
            .iter()
            .enumerate()
            .rposition(|(col, c)| {
                c.codepoint().is_some()
                    || c.width() == 0
                    || self.style(col).background != Color::Default
            })
            .map_or(0, |i| i + 1)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RowCopy {
    pub id: u64,
    pub wrapped: bool,
    pub wrap_continuation: bool,
    pub semantic: SemanticContent,
    pub cells: Vec<CellCopy>,
}

impl RowCopy {
    fn from_view(row: RowView<'_>) -> Self {
        Self {
            id: row.id,
            wrapped: row.wrapped,
            wrap_continuation: row.wrap_continuation,
            semantic: row.semantic,
            cells: (0..row.cells.len()).map(|col| row.copy_cell(col)).collect(),
        }
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
    #[serde(flatten, with = "hyperlink_serde")]
    pub hyperlink: Option<Arc<HyperlinkData>>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CharsetState {
    pub slots: [Charset; 4],
    pub gl: usize,
    pub gr: usize,
    pub single: Option<usize>,
}

impl Default for CharsetState {
    fn default() -> Self {
        Self {
            slots: [Charset::Utf8; 4],
            gl: 0,
            gr: 2,
            single: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedCursor {
    pub cursor: Cursor,
    pub origin: bool,
    pub charset: CharsetState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct Screen {
    #[serde(skip)]
    pub(crate) metadata: crate::snapshot::ScreenMetadata,
    #[serde(skip)]
    pub graphics: crate::graphics::Graphics,
    /// Logical width. Restored physical rows retain their own width until a
    /// column resize reflows them or an edit needs additional cells.
    pub columns: usize,
    #[serde(skip)]
    pub(crate) height: usize,
    pub cursor: Cursor,
    pub selection: Option<Selection>,
    pub viewport_offset: usize,
    /// Native viewport pins retain their column when a search scrolls to a
    /// match. This is an anchor coordinate, not horizontal scrolling.
    #[serde(skip)]
    pub(crate) viewport_pin_column: usize,
    /// The native viewport anchor remains tracked when the viewport follows
    /// the active area or the top. Even then it keeps blank cells during reflow.
    #[serde(skip)]
    pub(crate) viewport_pin: Option<GridPoint>,
    pub kitty_keyboard: KittyKeyboard,
    pub(crate) saved_cursor: Option<SavedCursor>,
    pub(crate) charset: CharsetState,
    pub(crate) iso_protection: bool,
    pub(crate) limits: ScrollbackLimits,
    /// Host memory policy, separate from native page accounting and snapshots.
    #[serde(skip)]
    pub(crate) memory_limit: Option<usize>,
    pub(crate) pages: PageList,
    #[serde(skip)]
    pub(crate) cursor_style: Option<(u64, u16)>,
    #[serde(skip)]
    pub(crate) cursor_link: Option<(u64, u16)>,
    pub(crate) next_row: u64,
    #[serde(skip)]
    tracked: TrackedPoints,
}

impl Screen {
    pub(crate) fn new(cols: usize, rows: usize, limits: ScrollbackLimits) -> Self {
        let mut pages = PageList::new(cols as u16, rows);
        let mut id = 0;
        for page in &mut pages.pages {
            for row in &mut page.row_ids[..usize::from(page.rows)] {
                *row = id;
                id += 1;
            }
        }
        Self {
            metadata: crate::snapshot::ScreenMetadata::default(),
            graphics: crate::graphics::Graphics::default(),
            columns: cols,
            height: rows,
            cursor: Cursor::default(),
            selection: None,
            viewport_offset: 0,
            viewport_pin_column: 0,
            viewport_pin: Some(GridPoint { row: 0, col: 0 }),
            kitty_keyboard: KittyKeyboard::default(),
            saved_cursor: None,
            charset: CharsetState::default(),
            iso_protection: false,
            limits,
            memory_limit: None,
            pages,
            cursor_style: None,
            cursor_link: None,
            next_row: rows as u64,
            tracked: TrackedPoints::default(),
        }
    }

    pub fn height(&self) -> usize {
        self.height
    }
    pub fn history_len(&self) -> usize {
        self.pages.total_rows().saturating_sub(self.height)
    }
    pub fn all_rows(&self) -> Rows<'_> {
        Rows::new(&self.pages, 0, self.pages.total_rows())
    }
    pub fn rows(&self) -> Rows<'_> {
        Rows::new(&self.pages, self.history_len(), self.height)
    }
    pub fn history(&self) -> Rows<'_> {
        Rows::new(&self.pages, 0, self.history_len())
    }
    pub fn physical_row(&self, row: usize) -> Row<'_> {
        let (page, row) = self.pages.page_at(row);
        page.row(row)
    }
    pub fn row(&self, row: usize) -> Row<'_> {
        assert!(row < self.height);
        self.physical_row(self.history_len() + row)
    }
    #[inline]
    pub(crate) fn row_columns(&self, y: usize) -> usize {
        let (index, _) = self.pages.locate_from_end(self.height - 1 - y);
        usize::from(self.pages.pages[index].columns)
    }
    #[inline(always)]
    pub(crate) fn cursor_row(&self) -> Row<'_> {
        let (index, row) = self.cursor_location();
        self.pages.pages[index].row(row)
    }
    #[inline]
    pub fn cell_text<'a>(&self, row: impl Into<Row<'a>>, col: usize) -> CellText<'a> {
        row.into().text(col)
    }
    pub fn row_text<'a>(&self, row: impl Into<Row<'a>>) -> String {
        let row = row.into();
        let mut result = String::new();
        for (col, cell) in row.cells.iter().enumerate() {
            if cell.width() == 0 || cell.spacer_head() {
                continue;
            }
            if cell.codepoint().is_none() {
                result.push(' ');
            } else {
                result.push_str(&row.text(col));
            }
        }
        result.truncate(result.trim_end_matches(' ').len());
        result
    }
    pub fn viewport(&self) -> impl Iterator<Item = Row<'_>> {
        self.all_rows()
            .skip(self.history_len().saturating_sub(self.viewport_offset))
            .take(self.height)
    }
    pub fn page_allocations(&self) -> impl Iterator<Item = PageAllocationInfo> + '_ {
        self.pages.allocations()
    }

    pub fn snapshot_viewport(&self) -> Self {
        let mut result = Self {
            metadata: self.metadata.clone(),
            graphics: self.graphics.snapshot(self),
            columns: self.columns,
            height: self.height,
            cursor: self.cursor.clone(),
            selection: self.selection,
            viewport_offset: 0,
            viewport_pin_column: 0,
            viewport_pin: None,
            kitty_keyboard: self.kitty_keyboard.clone(),
            saved_cursor: None,
            charset: self.charset.clone(),
            iso_protection: self.iso_protection,
            limits: ScrollbackLimits::NONE,
            memory_limit: None,
            pages: self.pages.clone_range(
                self.history_len().saturating_sub(self.viewport_offset),
                self.height,
            ),
            cursor_style: self.cursor_style,
            cursor_link: self.cursor_link,
            next_row: self.next_row,
            tracked: TrackedPoints::default(),
        };
        result.cursor.row = result.cursor.row.saturating_add(self.viewport_offset);
        result.cursor.visible &= result.cursor.row < self.height;
        result.cursor.row = result.cursor.row.min(self.height - 1);
        result.release_cursor_style();
        result.release_cursor_link();
        result
    }

    fn locate(&self, absolute: usize) -> (usize, usize) {
        self.pages.locate(absolute)
    }

    #[inline]
    pub(crate) fn cursor_location(&self) -> (usize, usize) {
        self.pages
            .locate_from_end(self.height - 1 - self.cursor.row)
    }

    fn cursor_page_index(&self) -> usize {
        self.cursor_location().0
    }
    pub(crate) fn row_header_mut(&mut self, y: usize) -> &mut RowHeader {
        let (index, row) = self.locate(self.history_len() + y);
        &mut self.pages.pages[index].headers[row]
    }
    pub(crate) fn cell_mut(&mut self, y: usize, col: usize) -> &mut Cell {
        let (index, row) = self.locate(self.history_len() + y);
        let page = &mut self.pages.pages[index];
        page.headers[row].set(RowHeader::DIRTY, true);
        let slot = page.slot(row, col);
        &mut page.cells[slot]
    }

    pub(crate) fn erase_row_cells(
        &mut self,
        y: usize,
        start: usize,
        end: usize,
        background: Color,
        protected: bool,
    ) {
        let (index, row) = self.locate(self.history_len() + y);
        self.pages.pages[index].erase(row, start, end, background, protected);
    }
    pub(crate) fn reset_row(&mut self, y: usize, id: u64, background: Color) {
        let (index, row) = self.locate(self.history_len() + y);
        self.pages.pages[index].reset_row(row, id, background);
    }
    pub(crate) fn shift_cells(
        &mut self,
        y: usize,
        range: std::ops::Range<usize>,
        count: usize,
        right: bool,
        background: Color,
    ) {
        let (index, row) = self.locate(self.history_len() + y);
        self.pages.pages[index].shift_cells(row, range, count, right, background);
    }

    pub(crate) fn clear_grapheme(&mut self, y: usize, col: usize) {
        let (index, row) = self.locate(self.history_len() + y);
        let page = &mut self.pages.pages[index];
        let slot = page.slot(row, col);
        if let Some(allocation) = page.grapheme(slot) {
            page.graphemes.release(allocation);
            page.grapheme_map.remove(&(slot as u32));
            page.cells[slot].set_grapheme(false);
            page.refresh_charge();
        }
    }

    pub fn set_cell_text(&mut self, y: usize, col: usize, text: &str) {
        let mut chars = text.chars();
        let cp = chars.next();
        let suffix = chars.count();
        assert!(suffix <= 64, "cell grapheme is too long");
        let style = self.row(y).style(col);
        self.clear_grapheme(y, col);
        self.cell_mut(y, col).set_codepoint(cp);
        self.set_cell_style(y, col, style);
        if suffix > 0 {
            let absolute = self.history_len() + y;
            let mut location = self.locate(absolute);
            let allocation = self
                .acquire_grapheme(absolute, &mut location, suffix as u8)
                .expect("cell grapheme fits after growth");
            let (index, row) = location;
            let page = &mut self.pages.pages[index];
            let slot = page.slot(row, col);
            page.graphemes.set_text(allocation, Arc::from(text));
            page.grapheme_map.insert(slot as u32, allocation);
            page.cells[slot].set_grapheme(true);
            page.mark_cell(row, page.cells[slot]);
            page.refresh_charge();
        }
    }

    pub fn set_cell_style(&mut self, y: usize, col: usize, style: Style) {
        let absolute = self.history_len() + y;
        let mut location = self.locate(absolute);
        let page = &self.pages.pages[location.0];
        let cell = page.cells[page.slot(location.1, col)];
        let inline = cell.codepoint().is_none()
            && cell.width() == 1
            && !cell.spacer_head()
            && style
                == (Style {
                    background: style.background,
                    ..Style::default()
                });
        let id = if inline {
            0
        } else {
            self.acquire_style(absolute, &mut location, style, None)
                .expect("cell style fits after growth")
        };
        let (index, row) = location;
        let page = &mut self.pages.pages[index];
        let slot = page.slot(row, col);
        page.styles.release(page.cells[slot].style_id());
        page.cells[slot].set_style_id(id);
        if inline {
            page.cells[slot].set_background(style.background);
        } else if page.cells[slot].background().is_some() {
            page.cells[slot].set_codepoint(None);
        }
        page.mark_cell(row, page.cells[slot]);
        page.refresh_charge();
    }
    pub(crate) fn set_cell_cursor_hyperlink(&mut self, col: usize) {
        loop {
            let Some((_, id)) = self.cursor_link else {
                break;
            };
            let (index, row) = self.cursor_location();
            let page = &mut self.pages.pages[index];
            if page.links.retain_cell(id).is_ok() {
                let slot = page.slot(row, col);
                page.link_map.insert(slot as u32, id);
                page.set_link_data(slot, id, self.cursor.hyperlink.as_ref().unwrap().clone());
                page.cells[slot].set_hyperlink(true);
                page.mark_cell(row, page.cells[slot]);
                page.refresh_charge();
                return;
            }
            while let Some(data) = self.cursor.hyperlink.clone() {
                let link = HyperlinkKey::from_data(&data);
                if self.pages.pages[index].links.reserve_uri(link.uri.len()) {
                    break;
                }
                if self
                    .grow_resource_page(index, Some(PageResource::Strings))
                    .is_err()
                {
                    return;
                }
            }
            if self
                .grow_resource_page(index, Some(PageResource::Links))
                .is_err()
            {
                break;
            }
        }
    }

    pub(crate) fn install_cell(
        &mut self,
        absolute: usize,
        col: usize,
        copy: CellCopy,
        reflow: bool,
    ) -> Result<(), SetFull> {
        let mut location = self.locate(absolute);
        let (index, row) = location;
        let page = &mut self.pages.pages[index];
        let slot = page.slot(row, col);
        let payload_changed = page.cells[slot].has_grapheme()
            || page.cells[slot].has_hyperlink()
            || copy.text.is_some()
            || copy.link.is_some();
        page.clear_cell(slot, Color::Default);
        let mut result = Ok(());
        let mut cell = copy.cell;
        cell.set_style_id(0);
        cell.set_hyperlink(false);
        if cell.has_grapheme() {
            cell.set_grapheme(false);
        }
        page.cells[slot] = cell;
        page.mark_cell(row, cell);
        if let Some((text, len)) = copy.text {
            if let Ok(allocation) = self.acquire_grapheme(absolute, &mut location, len) {
                let (index, row) = location;
                let page = &mut self.pages.pages[index];
                let slot = page.slot(row, col);
                page.graphemes.set_text(allocation, text);
                page.grapheme_map.insert(slot as u32, allocation);
                page.cells[slot].set_grapheme(true);
                page.mark_cell(row, page.cells[slot]);
            } else {
                result = Err(SetFull::OutOfMemory);
            }
        }
        if let Some(data) = copy.link {
            let link = HyperlinkKey::from_data(&data);
            let id = if reflow {
                loop {
                    let index = location.0;
                    match self.pages.pages[index]
                        .links
                        .reflow_cell(link, copy.link_id)
                    {
                        Ok(id) => break id,
                        Err(error) => {
                            if self
                                .grow_resource_page(index, PageResource::for_link(error))
                                .is_err()
                            {
                                break 0;
                            }
                        }
                    }
                }
            } else {
                self.acquire_link_cell(location.0, link, copy.link_id)
                    .unwrap_or(0)
            };
            if id != 0 {
                let (index, row) = location;
                let page = &mut self.pages.pages[index];
                let slot = page.slot(row, col);
                page.set_link_data(slot, id, data);
                page.link_map.insert(slot as u32, id);
                page.cells[slot].set_hyperlink(true);
                page.mark_cell(row, page.cells[slot]);
            } else {
                result = Err(SetFull::OutOfMemory);
            }
        }
        if copy.cell.style_id() != 0 {
            let id = self
                .acquire_style(
                    absolute,
                    &mut location,
                    copy.style,
                    Some(copy.cell.style_id()),
                )
                .unwrap_or(0);
            let (index, row) = location;
            let page = &mut self.pages.pages[index];
            let slot = page.slot(row, col);
            page.cells[slot].set_style_id(id);
            page.mark_cell(row, page.cells[slot]);
            if id == 0 && copy.style != Style::default() {
                result = Err(SetFull::OutOfMemory);
            }
        }
        if payload_changed {
            self.pages.pages[location.0].refresh_charge();
        }
        result
    }

    pub(crate) fn append_grapheme(&mut self, col: usize, cp: char) -> Result<(), SetFull> {
        let mut location = self.cursor_location();
        let page = &mut self.pages.pages[location.0];
        let slot = page.slot(location.1, col);
        let previous = page.grapheme(slot);
        let mut bytes = [0; 4 * 65];
        let len = if let Some(previous) = previous {
            let text = page.graphemes.text(previous);
            bytes[..text.len()].copy_from_slice(text.as_bytes());
            text.len()
        } else {
            page.cells[slot]
                .codepoint()
                .map_or(0, |base| base.encode_utf8(&mut bytes).len())
        };
        let len = len + cp.encode_utf8(&mut bytes[len..]).len();
        let allocation = match page.graphemes.append(previous) {
            Ok(allocation) => allocation,
            Err(_) => {
                if self
                    .grow_resource_page(location.0, Some(PageResource::Graphemes))
                    .is_err()
                {
                    let absolute = self.history_len() + self.cursor.row;
                    self.split_resource_page(absolute)?;
                    location = self.cursor_location();
                }
                let (index, row) = location;
                let page = &mut self.pages.pages[index];
                let previous = page.grapheme(page.slot(row, col));
                page.graphemes.append(previous)?
            }
        };
        let (index, row) = location;
        let page = &mut self.pages.pages[index];
        let slot = page.slot(row, col);
        page.grapheme_map.insert(slot as u32, allocation);
        page.cells[slot].set_grapheme(true);
        page.graphemes.set_text(
            allocation,
            // The prefix is stored UTF-8; encode_utf8 appends one valid scalar.
            Arc::from(unsafe { std::str::from_utf8_unchecked(&bytes[..len]) }),
        );
        page.mark_cell(row, page.cells[slot]);
        page.refresh_charge();
        Ok(())
    }

    pub(crate) fn move_wrapped_grapheme(&mut self, source_col: usize, suffix: &str) {
        let absolute = self.history_len() + self.cursor.row;
        // Native transfer stops when scrolling leaves no preceding row. Only
        // host-budget pruning preserves active text through the saved suffix.
        if absolute == 0 && (self.memory_limit.is_none() || self.limits.bytes == Some(0)) {
            return;
        }
        let source = absolute.checked_sub(1).and_then(|source| {
            let (index, row) = self.locate(source);
            let page = &mut self.pages.pages[index];
            let slot = page.slot(row, source_col);
            let allocation = page.grapheme_map.remove(&(slot as u32))?;
            if page.cells[slot].has_grapheme() {
                page.cells[slot].set_grapheme(false);
            }
            Some((index, allocation))
        });
        let (index, row) = self.cursor_location();
        if let Some((source_index, allocation)) = source
            && source_index == index
        {
            let col = self.cursor.col;
            let page = &mut self.pages.pages[index];
            let slot = page.slot(row, col);
            page.grapheme_map.insert(slot as u32, allocation);
            page.cells[slot].set_grapheme(true);
            let base = page.cells[slot]
                .codepoint()
                .expect("wrapped grapheme has a base");
            // A pending character-set shift can remap the base while wrapping.
            // Otherwise the allocation already owns exactly the destination text.
            if !page.graphemes.text(allocation).starts_with(base) {
                let mut text = String::with_capacity(4 + suffix.len());
                text.push(base);
                text.push_str(suffix);
                page.graphemes.set_text(allocation, Arc::from(text));
            }
            page.mark_cell(row, page.cells[slot]);
            page.refresh_charge();
        } else {
            for cp in suffix.chars() {
                if self.append_grapheme(self.cursor.col, cp).is_err() {
                    break;
                }
            }
            if let Some((index, allocation)) = source {
                self.pages.pages[index].graphemes.release(allocation);
                self.pages.pages[index].refresh_charge();
            }
        }
    }

    fn cursor_template(&self, codepoint: Option<char>, width: u8, spacer_head: bool) -> Cell {
        let mut cell = Cell::default();
        cell.set_codepoint(codepoint);
        cell.set_style_id(self.cursor_style.map_or(0, |(_, id)| id));
        cell.set_width(width);
        cell.set_spacer_head(spacer_head);
        cell.set_protected(self.cursor.protected);
        cell.set_semantic(self.cursor.semantic);
        cell
    }

    pub(crate) fn fill_alignment_row(&mut self, y: usize) {
        let id = self.cursor_style.map_or(0, |(_, id)| id);
        let (index, row) = self.locate(self.history_len() + y);
        let page = &mut self.pages.pages[index];
        let mut cell = Cell::default();
        cell.set_codepoint(Some('E'));
        cell.set_style_id(id);
        page.styles.retain_many(id, page.columns);
        page.row_cells_mut(row).fill(cell);
        page.headers[row].reset();
        page.mark_cell(row, cell);
    }

    pub(crate) fn write_cursor_cell(
        &mut self,
        codepoint: Option<char>,
        width: u8,
        spacer_head: bool,
        location: (usize, usize),
    ) {
        debug_assert_eq!(location, self.cursor_location());
        let (index, row) = if self.cursor_resources_match(location.0) {
            location
        } else {
            self.sync_cursor_resources_slow();
            self.cursor_location()
        };
        let y = self.cursor.row;
        let col = self.cursor.col;
        let page = &self.pages.pages[index];
        let offset = page.slot(row, 0);
        let columns = usize::from(page.columns);
        let old = page.cells[offset + col];
        let old_width = old.width();
        if y > 0 && col <= 1 && old_width != width && old_width != 1 {
            let last = self.row_columns(y - 1) - 1;
            self.cell_mut(y - 1, last).set_spacer_head(false);
        }
        let template = self.cursor_template(codepoint, width, spacer_head);
        let page = &mut self.pages.pages[index];
        if width == 1 && old_width == 1 && !old.has_grapheme() && !old.has_hyperlink() {
            page.replace_simple_styles(old.style_id(), template.style_id(), 1);
            page.cells[offset + col] = template;
            page.mark_cell(row, template);
            if self.cursor.hyperlink.is_some() {
                self.set_cell_cursor_hyperlink(col);
            }
            return;
        }
        let end = (col + usize::from(width)).min(columns);
        let clear_start = col - usize::from(old_width == 0 && col > 0);
        let clear_end =
            end + usize::from(end < columns && page.cells[offset + end - 1].width() == 2);
        let mut released_payload = false;
        for x in clear_start..clear_end {
            let slot = offset + x;
            let has_payload = page.cells[slot].has_grapheme() || page.cells[slot].has_hyperlink();
            released_payload |= has_payload;
            let old_style = page.cells[slot].style_id();
            let replacement = if x < col || x >= end {
                Cell::blank(self.cursor.style.background)
            } else if x == col {
                template
            } else {
                let mut tail = template;
                tail.set_codepoint(None);
                tail.set_width(0);
                tail
            };
            // Preserve a homogeneous style's references without a release/retain pair.
            if has_payload {
                page.cells[slot].set_style_id(0);
                page.clear_cell(slot, Color::Default);
            }
            if old_style != replacement.style_id() {
                page.styles.release(old_style);
                page.styles.retain(replacement.style_id());
            }
            page.cells[slot] = replacement;
            page.mark_cell(row, replacement);
        }
        if released_payload {
            page.refresh_charge();
        }
        if self.cursor.hyperlink.is_some() {
            self.set_cell_cursor_hyperlink(col);
            if width == 2 && col + 1 < columns {
                self.set_cell_cursor_hyperlink(col + 1);
            }
        }
    }

    pub(crate) fn write_cursor_ascii(&mut self, bytes: &[u8]) -> usize {
        use crate::printing;
        let (index, row) = self.sync_cursor_resources();
        let template = self.cursor_template(None, 1, false);
        let page = &mut self.pages.pages[index];
        let offset = page.slot(row, self.cursor.col);
        let len = bytes.len().min(usize::from(page.columns) - self.cursor.col);
        let old = page.cells[offset];
        if old.width() != 1 || old.has_grapheme() || old.has_hyperlink() {
            return 0;
        }
        let count = printing::destination_narrow(
            &page.cells[offset..offset + len],
            old.bits() & printing::DEST_MASK,
        );
        page.replace_simple_styles(old.style_id(), template.style_id(), count);
        printing::store_ascii(
            &mut page.cells[offset..offset + count],
            &bytes[..count],
            template,
        );
        if count != 0 {
            page.mark_cell(row, template);
        }
        count
    }

    pub(crate) fn write_cursor_codepoints(
        &mut self,
        codepoints: &[char],
        properties: &[u32],
        right: usize,
        graphemes: bool,
        state: &mut u8,
    ) -> usize {
        use crate::{printing, unicode};
        if self.cursor.pending_wrap || self.cursor.col > right {
            return 0;
        }
        let Some(&first) = codepoints.first() else {
            return 0;
        };
        let width = (properties[0] & 3) as u8;
        if width == 0 {
            return 0;
        }
        let len = printing::printable_prefix(properties, width, graphemes)
            .min((right + 1 - self.cursor.col) / usize::from(width));
        if len == 0 {
            return 0;
        }
        let (index, row) = self.cursor_location();
        let page = &self.pages.pages[index];
        // The scalar fallback extends partial-reflow rows before any write.
        if usize::from(page.columns) <= right || self.cursor_link.is_some() {
            return 0;
        }
        match self.cursor_style {
            Some((owner, id))
                if owner == page.serial && *page.styles.get(id) == self.cursor.style => {}
            None if self.cursor.style == Style::default() => {}
            _ => return 0,
        }
        let offset = page.slot(row, self.cursor.col);
        let mut next_state = *state;
        if graphemes && first as u32 > 255 && self.cursor.col > 0 {
            let mut previous = offset - 1;
            if self.cursor.col > 1 && page.cells[previous].width() == 0 {
                previous -= 1;
            }
            let cell = page.cells[previous];
            if cell.has_grapheme() {
                return 0;
            }
            if let Some(cp) = cell.codepoint()
                && !unicode::grapheme_break_properties(
                    unicode::properties(cp).grapheme,
                    0,
                    &mut next_state,
                )
            {
                return 0;
            }
        }
        let cells = &page.cells[offset..offset + len * usize::from(width)];
        let old = cells[0];
        if old.has_grapheme() || old.has_hyperlink() {
            return 0;
        }
        let count = if old.width() == 1 {
            printing::destination_narrow(cells, old.bits() & printing::DEST_MASK)
                / usize::from(width)
        } else if width == 2
            && old.width() == 2
            && cells[1].width() == 0
            && cells[1].style_id() == old.style_id()
            && !cells[1].has_grapheme()
            && !cells[1].has_hyperlink()
        {
            printing::destination_wide(
                cells,
                [
                    old.bits() & printing::DEST_MASK,
                    cells[1].bits() & printing::DEST_MASK,
                ],
            ) / 2
        } else {
            0
        };
        if count == 0 {
            return 0;
        }
        // Once the leading boundary is checked, neighboring input characters
        // in this run have ordinary grapheme classes. Latin-1 skips segmentation.
        if graphemes
            && (1..=4).contains(&next_state)
            && codepoints[1..count].iter().any(|&cp| cp as u32 > 255)
        {
            next_state = 0;
        }
        let head = self.cursor_template(None, width, false);
        let tail = self.cursor_template(None, 0, false);
        let page = &mut self.pages.pages[index];
        let slots = count * usize::from(width);
        page.replace_simple_styles(old.style_id(), head.style_id(), slots);
        let cells = &mut page.cells[offset..offset + slots];
        if width == 1 {
            printing::store_narrow(cells, &codepoints[..count], head);
        } else {
            printing::store_wide(cells, &codepoints[..count], head, tail);
        }
        page.mark_cell(row, head);
        *state = next_state;
        let col = self.cursor.col + slots;
        self.cursor.col = col.min(right);
        self.cursor.pending_wrap = col > right;
        count
    }

    pub(crate) fn next_row_id(&mut self) -> u64 {
        let id = self.next_row;
        self.next_row = self
            .next_row
            .checked_add(1)
            .expect("row identities exhausted");
        id
    }

    fn install_row(&mut self, absolute: usize, copy: RowCopy, limit: usize) {
        let width = self.physical_row(absolute).cells.len();
        let count = width.min(copy.cells.len()).min(limit);
        let complete = count == width;
        let source_width = copy.cells.len();
        // Native row copies release the overwritten prefix before admitting
        // any incoming styles. Dead IDs must be available to the first cell.
        let (index, row) = self.locate(absolute);
        let page = &mut self.pages.pages[index];
        let offset = page.slot(row, 0);
        let released_payload = page.headers[row].has(RowHeader::GRAPHEME | RowHeader::HYPERLINK);
        for slot in offset..offset + count {
            page.clear_cell(slot, Color::Default);
        }
        if released_payload {
            page.refresh_charge();
        }
        for (col, mut cell) in copy.cells.into_iter().take(count).enumerate() {
            if col + 1 == source_width && width > source_width {
                cell.cell.set_spacer_head(false);
            }
            let _ = self.install_cell(absolute, col, cell, false);
        }
        let (index, row) = self.locate(absolute);
        let page = &mut self.pages.pages[index];
        page.row_ids[row] = copy.id;
        if complete {
            page.headers[row].set(RowHeader::WRAPPED, copy.wrapped);
            page.headers[row].set(RowHeader::CONTINUATION, copy.wrap_continuation);
        }
        page.headers[row].set_semantic(copy.semantic);
        page.headers[row].set(RowHeader::DIRTY, true);
    }

    /// Move rows in page-sized segments. Only a boundary row copies cells.
    fn rotate_rows(
        &mut self,
        start: usize,
        end: usize,
        up: bool,
        blank: u64,
        background: Color,
        copy_limit: usize,
    ) {
        if up {
            let mut first = start;
            while first <= end {
                let (index, row) = self.locate(first);
                let last = (first - row + usize::from(self.pages.pages[index].rows) - 1).min(end);
                let next = (last < end).then(|| RowCopy::from_view(self.physical_row(last + 1)));
                self.pages.pages[index].rotate_rows(row..row + last - first + 1, true);
                if let Some(copy) = next {
                    self.install_row(last, copy, copy_limit);
                } else {
                    let (index, row) = self.locate(last);
                    self.pages.pages[index].reset_row(row, blank, background);
                }
                first = last + 1;
            }
        } else {
            let mut last = end;
            loop {
                let (index, row) = self.locate(last);
                let first = (last - row).max(start);
                let previous =
                    (first > start).then(|| RowCopy::from_view(self.physical_row(first - 1)));
                self.pages.pages[index].rotate_rows(row - (last - first)..row + 1, false);
                if let Some(copy) = previous {
                    self.install_row(first, copy, copy_limit);
                } else {
                    let (index, row) = self.locate(first);
                    self.pages.pages[index].reset_row(row, blank, background);
                }
                if first == start {
                    break;
                }
                last = first - 1;
            }
        }
    }

    pub(crate) fn shift_rows(
        &mut self,
        top: usize,
        bottom: usize,
        up: bool,
        history: bool,
        blank: u64,
        background: Color,
        copy_limit: usize,
    ) {
        if history && self.limits.bytes != Some(0) && self.memory_limit != Some(0) {
            self.grow_row(blank, background, self.height);
            if bottom + 1 < self.height {
                let start = self.history_len() + bottom;
                self.rotate_rows(
                    start,
                    self.pages.total_rows() - 1,
                    false,
                    blank,
                    background,
                    copy_limit,
                );
            }
        } else {
            let start = self.history_len() + top;
            let end = self.history_len() + bottom;
            let erased = self.physical_row(if up { start } else { end }).id;
            self.rotate_rows(start, end, up, blank, background, copy_limit);
            self.discard_row(erased);
        }
        self.sync_cursor_resources();
    }

    fn grow_row(&mut self, id: u64, background: Color, active_rows: usize) {
        if self.viewport_offset > 0 {
            self.viewport_offset += 1;
        }
        let removed = self.pages.grow(
            self.columns as u16,
            active_rows,
            self.limits,
            self.memory_limit,
            id,
            background,
        );
        self.discard_ids(removed);
    }

    pub(crate) fn retain_history(&mut self) {
        let id = self.next_row_id();
        self.grow_row(id, Color::Default, self.height);
    }

    pub(crate) fn copy_row_cells(
        &mut self,
        source: usize,
        destination: usize,
        start: usize,
        end: usize,
        background: Color,
    ) {
        let source_absolute = self.history_len() + source;
        let destination_absolute = self.history_len() + destination;
        let (si, sr) = self.locate(source_absolute);
        let (di, dr) = self.locate(destination_absolute);
        let end = end.min(self.row(destination).cells.len());
        if start >= end {
            return;
        }
        if si == di {
            let page = &mut self.pages.pages[si];
            let src = page.slot(sr, start);
            let dst = page.slot(dr, start);
            page.move_cells(src, dst, end - start, background);
            for slot in dst..dst + end - start {
                page.mark_cell(dr, page.cells[slot]);
            }
            page.headers[sr].set(RowHeader::DIRTY, true);
            page.repair_wide(dr, background);
            page.refresh_charge();
        } else {
            let source = self.row(source);
            let copies: Vec<_> = (start..end)
                .map(|col| {
                    if col < source.cells.len() {
                        source.copy_cell(col)
                    } else {
                        CellCopy::plain(Cell::blank(background))
                    }
                })
                .collect();
            for (i, cell) in copies.into_iter().enumerate() {
                let _ = self.install_cell(destination_absolute, start + i, cell, false);
            }
            let (index, row) = self.locate(destination_absolute);
            self.pages.pages[index].repair_wide(row, background);
        }
    }

    pub(crate) fn change_grapheme_width(
        &mut self,
        y: usize,
        col: usize,
        width: u8,
        right: usize,
        background: Color,
    ) {
        self.cell_mut(y, col).set_width(width);
        if col < right {
            let mut copy = if width == 2 {
                self.row(y).copy_cell(col)
            } else {
                CellCopy::plain(Cell::blank(background))
            };
            if width == 2 {
                copy.cell.set_codepoint(None);
                copy.cell.set_width(0);
                copy.text = None;
            }
            let _ = self.install_cell(self.history_len() + y, col + 1, copy, false);
        }
    }

    pub(crate) fn prepare_row_shift(
        &mut self,
        y: usize,
        left: usize,
        right: usize,
        right_edge: bool,
    ) {
        if left == 0 && right_edge {
            self.row_header_mut(y)
                .set(RowHeader::WRAPPED | RowHeader::CONTINUATION, false);
        }
        if right_edge || left < 2 {
            let last = self.row(y).cells.len() - 1;
            self.cell_mut(y, last).set_spacer_head(false);
        }
        for boundary in [left, right + 1] {
            if boundary > 0
                && self
                    .row(y)
                    .cells
                    .get(boundary)
                    .is_some_and(|cell| cell.width() == 0)
            {
                self.clear_grapheme(y, boundary - 1);
                let cell = self.cell_mut(y, boundary - 1);
                cell.set_codepoint(None);
                cell.set_width(1);
                self.cell_mut(y, boundary).set_width(1);
            }
        }
        self.row_header_mut(y).set(RowHeader::DIRTY, true);
    }

    pub(crate) fn cursor_reset_wrap(&mut self) {
        self.cursor.pending_wrap = false;
        let y = self.cursor.row;
        if !self.row(y).wrapped {
            return;
        }
        self.row_header_mut(y).set(RowHeader::WRAPPED, false);
        if y + 1 < self.height {
            self.row_header_mut(y + 1)
                .set(RowHeader::CONTINUATION, false);
        }
        let columns = self.row(y).cells.len();
        if self.row(y).cells[columns - 1].spacer_head() {
            self.erase_row_cells(y, columns - 1, columns, self.cursor.style.background, false);
        }
    }

    pub(crate) fn split_cell_boundary(&mut self, col: usize) {
        let y = self.cursor.row;
        let cols = self.row(y).cells.len();
        let background = self.cursor.style.background;
        if col >= cols {
            if col == cols && self.row(y).wrapped && self.row(y).cells[cols - 1].spacer_head() {
                self.erase_row_cells(y, cols - 1, cols, background, false);
            }
            return;
        }
        if col <= 1 && self.row(y).cells[0].width() == 2 {
            let absolute = self.history_len() + y;
            if absolute > 0 {
                let previous = self.physical_row(absolute - 1);
                if previous.wrapped && previous.cells.last().is_some_and(|cell| cell.spacer_head())
                {
                    let width = previous.cells.len();
                    let (index, row) = self.locate(absolute - 1);
                    self.pages.pages[index].erase(row, width - 1, width, background, false);
                }
            }
        }
        if col > 0 && self.row(y).cells[col - 1].width() == 2 {
            self.erase_row_cells(y, col - 1, col + 1, background, false);
        }
    }

    fn split_resource_page(&mut self, absolute: usize) -> Result<(), SetFull> {
        let (index, relative) = self.locate(absolute);
        let page = &self.pages.pages[index];
        if page.rows <= 1 {
            return Err(SetFull::OutOfMemory);
        }
        let start = absolute - relative;
        let end = start + usize::from(page.rows);
        let above = self.exact_resource_range_bytes(start, absolute + 1, page.columns);
        let below = self.exact_resource_range_bytes(absolute, end, page.columns);
        let split = if above < below && absolute + 1 < end {
            relative + 1
        } else {
            relative
        };
        if split == 0 {
            return Ok(());
        }
        let capacity = page.capacity;
        let columns = page.columns;
        let copies: Vec<_> = (split..usize::from(page.rows))
            .map(|row| RowCopy::from_view(page.row(row)))
            .collect();
        self.pages.pages[index].truncate(split);
        let serial = self.pages.fresh_serial();
        let mut target = Page::new(capacity, copies.len() as u16, serial);
        target.columns = columns;
        self.pages.pages.insert(index + 1, target);
        for (i, copy) in copies.into_iter().enumerate() {
            self.install_row(start + split + i, copy, usize::MAX);
        }
        self.sync_cursor_resources();
        Ok(())
    }

    /// Capacity charges for pages that can be reclaimed without removing active rows.
    /// A page intersecting the active screen is the minimum storage allowance.
    pub fn history_bytes(&self) -> usize {
        self.pages.owned_history_bytes(self.height)
    }
    pub fn owned_bytes(&self) -> usize {
        self.pages.pages.iter().map(Page::storage_bytes).sum()
    }
    /// Native logical allocation charge, retained for snapshot and page compatibility.
    pub fn storage_bytes(&self) -> usize {
        self.pages.allocation_bytes()
    }

    fn discard_ids(&mut self, ids: Vec<u64>) {
        if ids.len() > self.history_len().saturating_sub(self.viewport_offset) {
            self.viewport_pin_column = 0;
        }
        for id in ids {
            self.discard_row(id);
        }
        self.viewport_offset = self.viewport_offset.min(self.history_len());
    }
    pub(crate) fn enforce_memory_limit(&mut self) {
        if let Some(limit) = self.memory_limit {
            if limit == 0 {
                self.clear_history();
                return;
            }
            let removed = self
                .pages
                .prune(self.height, ScrollbackLimits::default(), Some(limit));
            self.discard_ids(removed);
        }
    }
    pub(crate) fn effective_limits(&self) -> ScrollbackLimits {
        PageList::effective_limits(self.columns as u16, self.height, self.limits)
    }
    pub(crate) fn enforce_limits(&mut self) {
        let removed = self
            .pages
            .prune(self.height, self.effective_limits(), self.memory_limit);
        self.discard_ids(removed);
    }
    pub(crate) fn set_limits(&mut self, limits: ScrollbackLimits) {
        self.limits = limits;
        if limits.bytes == Some(0) {
            self.clear_history();
        } else {
            self.enforce_limits();
        }
    }
    pub(crate) fn clear_history(&mut self) {
        let count = self.history_len();
        let ids = self.history().map(|row| row.id).collect();
        self.pages.remove_prefix(count);
        self.discard_ids(ids);
        self.viewport_offset = 0;
        self.viewport_pin_column = 0;
    }

    pub(crate) fn extend_physical_row(&mut self, y: usize, columns: usize) {
        let absolute = self.history_len() + y;
        let (index, relative) = self.locate(absolute);
        let page = &self.pages.pages[index];
        if columns <= usize::from(page.columns) {
            return;
        }
        let spacer_head = (0..usize::from(page.rows)).any(|row| {
            page.row_cells(row)
                .last()
                .is_some_and(|cell| cell.spacer_head())
        });
        if columns <= usize::from(page.capacity.cols) && !spacer_head {
            self.pages.pages[index].columns = columns as u16;
            self.pages.pages[index].layout_generation =
                self.pages.pages[index].layout_generation.wrapping_add(1);
            return;
        }
        self.release_cursor_style();
        if self
            .cursor_link
            .is_some_and(|(owner, _)| owner == self.pages.pages[index].serial)
        {
            self.release_cursor_link();
        }
        let source = self.pages.pages.remove(index).unwrap();
        let capacity = source.adjusted_capacity(columns as u16, false);
        let start = absolute - relative;
        let mut remaining = usize::from(source.rows);
        let mut at = index;
        while remaining > 0 {
            let count = remaining.min(usize::from(capacity.rows));
            let serial = self.pages.fresh_serial();
            self.pages
                .pages
                .insert(at, Page::new(capacity, count as u16, serial));
            at += 1;
            remaining -= count;
        }
        for row in 0..usize::from(source.rows) {
            let mut copy = RowCopy::from_view(source.row(row));
            copy.wrapped = false;
            copy.wrap_continuation = false;
            // Copied row extension preserves the blank destination's wrap flags.
            self.install_row(start + row, copy, usize::MAX);
            let (index, row) = self.locate(start + row);
            self.pages.pages[index].repair_wide(row, Color::Default);
        }
        self.sync_cursor_resources();
        self.enforce_memory_limit();
    }

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

    pub fn scroll_viewport(&mut self, rows: isize) {
        let previous = self.viewport_offset;
        if self.viewport_offset == 0
            || self.viewport_offset.saturating_add_signed(rows) > self.history_len()
        {
            self.viewport_pin_column = 0;
        }
        self.viewport_offset = self
            .viewport_offset
            .saturating_add_signed(rows)
            .min(self.history_len());
        if self.viewport_offset == 0 {
            self.viewport_pin_column = 0;
        } else if self.viewport_offset < self.history_len()
            || previous > 0
                && previous < self.history_len()
                && previous.saturating_add_signed(rows) == self.history_len()
        {
            self.viewport_pin = Some(self.viewport_top());
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

    pub fn row_by_id(&self, id: u64) -> Option<Row<'_>> {
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

    pub(crate) fn release_cursor_style(&mut self) {
        if let Some((serial, id)) = self.cursor_style.take() {
            self.release_style(serial, id);
        }
    }

    fn release_cursor_link(&mut self) {
        if let Some((serial, id)) = self.cursor_link.take()
            && let Some(page) = self
                .pages
                .pages
                .iter_mut()
                .find(|page| page.serial == serial)
        {
            page.links.release(id);
        }
    }

    pub(crate) fn end_hyperlink(&mut self) {
        self.release_cursor_link();
        self.cursor.hyperlink = None;
    }

    fn renew_cursor_implicit_link(&mut self) {
        if let Some(link) = &mut self.cursor.hyperlink
            && matches!(link.id, Some(HyperlinkId::Implicit(_)))
        {
            let id = self.metadata.hyperlink_implicit_id;
            Arc::make_mut(link).id = Some(HyperlinkId::Implicit(id));
            self.metadata.hyperlink_implicit_id = id.wrapping_add(1);
        }
    }

    fn acquire_cursor_link(&mut self, link: HyperlinkKey<'_>) -> Option<(u64, u16)> {
        loop {
            let index = self.cursor_page_index();
            match self.pages.pages[index].links.insert(link) {
                Ok(id) => {
                    self.pages.pages[index].refresh_charge();
                    return Some((self.pages.pages[index].serial, id));
                }
                Err(error) => self
                    .grow_resource_page(index, PageResource::for_link(error))
                    .ok()?,
            }
        }
    }

    pub(crate) fn start_hyperlink(&mut self, uri: &[u8], explicit: Option<&[u8]>) {
        let implicit = self.metadata.hyperlink_implicit_id;
        if explicit.is_none() {
            self.metadata.hyperlink_implicit_id = implicit.wrapping_add(1);
        }
        let link = HyperlinkKey::new(uri, explicit, implicit);
        self.end_hyperlink();
        if let Some(reference) = self.acquire_cursor_link(link) {
            self.cursor_link = Some(reference);
            let index = self.cursor_page_index();
            self.cursor.hyperlink = Some(self.pages.pages[index].links.data(reference.1).clone());
        } else if explicit.is_none() {
            self.metadata.hyperlink_implicit_id = implicit;
        }
    }

    /// Page movement renews implicit cursor links. Resource growth on the same
    /// page reinstalls their existing identities; printed cells retain theirs.
    #[inline]
    pub(crate) fn sync_cursor_resources(&mut self) -> (usize, usize) {
        let location = self.cursor_location();
        if self.cursor_resources_match(location.0) {
            return location;
        }
        self.sync_cursor_resources_slow();
        self.cursor_location()
    }

    #[inline(always)]
    fn cursor_resources_match(&self, index: usize) -> bool {
        self.cursor_link.is_none()
            && self.cursor.hyperlink.is_none()
            && match self.cursor_style {
                None => self.cursor.style == Style::default(),
                Some((owner, id)) => {
                    let page = &self.pages.pages[index];
                    owner == page.serial && *page.styles.get(id) == self.cursor.style
                }
            }
    }

    fn sync_cursor_resources_slow(&mut self) {
        let index = self.cursor_page_index();
        let serial = self.pages.pages[index].serial;
        let moved = self.cursor_link.is_some_and(|(owner, _)| owner != serial);
        if moved {
            self.release_cursor_link();
            self.renew_cursor_implicit_link();
        }
        self.sync_cursor_style();
        if let Some((owner, id)) = self.cursor_link {
            let page = &self.pages.pages[self.cursor_page_index()];
            if owner == page.serial {
                let link = page.links.get(id);
                if self.cursor.hyperlink.as_ref().is_some_and(|cursor_link| {
                    link.uri == cursor_link.uri_bytes() && Some(&link.id) == cursor_link.id.as_ref()
                }) {
                    return;
                }
            }
            self.release_cursor_link();
        }
        if let Some(data) = self.cursor.hyperlink.clone() {
            let link = HyperlinkKey::from_data(&data);
            self.cursor_link = self.acquire_cursor_link(link);
            if self.cursor_link.is_none() {
                self.end_hyperlink();
            }
        }
    }

    fn acquire_link_cell(
        &mut self,
        index: usize,
        link: HyperlinkKey<'_>,
        preferred: u16,
    ) -> Result<u16, SetFull> {
        loop {
            match self.pages.pages[index].links.copy_cell(link, preferred) {
                Ok(id) => {
                    self.pages.pages[index].refresh_charge();
                    return Ok(id);
                }
                Err(error) => self.grow_resource_page(index, PageResource::for_link(error))?,
            }
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
        let absolute = self.history_len() + self.cursor.row;
        let mut location = self.cursor_location();
        match self.acquire_style(absolute, &mut location, self.cursor.style, None) {
            Ok(id) => {
                self.cursor_style = Some((self.pages.pages[location.0].serial, id));
                true
            }
            Err(_) => false,
        }
    }

    /// Cursor page changes own a reference independently of printed cells.
    /// Saved cursor values and detached renderer snapshots own none.
    pub(crate) fn sync_cursor_style(&mut self) {
        if self.cursor_style.is_none() && self.cursor.style == Style::default() {
            return;
        }
        let page = &self.pages.pages[self.cursor_page_index()];
        if let Some((owner, id)) = self.cursor_style
            && owner == page.serial
            && *page.styles.get(id) == self.cursor.style
        {
            return;
        }
        self.release_cursor_style();
        if !self.install_cursor_style() {
            self.cursor.style = Style::default();
        }
    }

    fn acquire_style(
        &mut self,
        absolute: usize,
        location: &mut (usize, usize),
        style: Style,
        preferred: Option<u16>,
    ) -> Result<u16, SetFull> {
        let index = location.0;
        let acquire = |set: &mut StyleAdmission| match preferred {
            Some(id) if id != 0 => set.acquire_with_id(style, id),
            _ => set.acquire(style),
        };
        match acquire(&mut self.pages.pages[index].styles) {
            Ok(id) => {
                self.pages.pages[index].refresh_charge();
                Ok(id)
            }
            Err(error) => {
                if self
                    .grow_resource_page(
                        index,
                        (error == SetFull::OutOfMemory).then_some(PageResource::Styles),
                    )
                    .is_err()
                {
                    self.split_resource_page(absolute)?;
                    *location = self.locate(absolute);
                }
                let index = location.0;
                let result = acquire(&mut self.pages.pages[index].styles);
                self.pages.pages[index].refresh_charge();
                result
            }
        }
    }

    fn acquire_grapheme(
        &mut self,
        absolute: usize,
        location: &mut (usize, usize),
        len: u8,
    ) -> Result<GraphemeAllocation, SetFull> {
        loop {
            let index = location.0;
            if let Ok(allocation) = self.pages.pages[index].graphemes.acquire(len) {
                return Ok(allocation);
            }
            if self
                .grow_resource_page(index, Some(PageResource::Graphemes))
                .is_err()
            {
                self.split_resource_page(absolute)?;
                *location = self.locate(absolute);
                return self.pages.pages[location.0].graphemes.acquire(len);
            }
        }
    }

    fn grow_resource_page(
        &mut self,
        index: usize,
        grow: Option<PageResource>,
    ) -> Result<(), SetFull> {
        let serial = self.pages.pages[index].serial;
        self.pages.pages[index].rebuild(grow)?;
        if self.cursor_style.is_some_and(|(owner, _)| owner == serial) {
            match self.pages.pages[index].styles.acquire(self.cursor.style) {
                Ok(id) => self.cursor_style = Some((serial, id)),
                Err(_) => {
                    self.cursor_style = None;
                    self.cursor.style = Style::default();
                }
            }
        }
        if self.cursor_link.is_some_and(|(owner, _)| owner == serial) {
            self.cursor_link = None;
            if let Some(data) = self.cursor.hyperlink.clone() {
                let link = HyperlinkKey::from_data(&data);
                match self.pages.pages[index].links.insert(link) {
                    Ok(id) => self.cursor_link = Some((serial, id)),
                    Err(_) => self.end_hyperlink(),
                }
            }
        }
        self.pages.pages[index].refresh_charge();
        Ok(())
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
        self.viewport_pin
            .iter_mut()
            .chain(self.tracked.0.values_mut().flatten())
            .chain(
                self.selection
                    .iter_mut()
                    .flat_map(|selection| [&mut selection.start, &mut selection.end]),
            )
    }

    pub(crate) fn discard_row(&mut self, id: u64) {
        self.graphics.discard_row(id);
        if self.viewport_pin.is_some_and(|point| point.row == id) {
            let point = self
                .all_rows()
                .find(|row| row.id != id)
                .map(|row| GridPoint {
                    row: row.id,
                    col: 0,
                });
            self.viewport_pin = point;
        }
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

    fn exact_resource_range_bytes(&self, start: usize, end: usize, columns: u16) -> usize {
        use crate::page_resources::BitmapAllocator;
        use std::collections::HashSet;
        let mut styles = HashSet::new();
        let mut links = HashSet::new();
        let mut linked_cells: usize = 0;
        let mut grapheme_bytes = 0;
        let mut string_bytes = 0;
        for row in self.all_rows().skip(start).take(end - start) {
            for (col, cell) in row.cells.iter().enumerate() {
                if cell.style_id() != 0 {
                    styles.insert(cell.style_id());
                }
                if let Some(grapheme) = row.grapheme(col) {
                    grapheme_bytes +=
                        BitmapAllocator::<16>::bytes_required(usize::from(grapheme.len) * 4)
                            .unwrap();
                }
                if let Some(link) = row.hyperlink(col) {
                    linked_cells += 1;
                    let uri = link.uri_bytes();
                    if links.insert((link.id.as_ref(), uri)) {
                        string_bytes += BitmapAllocator::<32>::bytes_required(uri.len()).unwrap();
                        if let Some(HyperlinkId::Explicit(id)) = &link.id {
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

    pub(crate) fn clear_prompt_for_redraw(&mut self, redraw: PromptRedraw) {
        if self.cursor.semantic == SemanticContent::Output {
            return;
        }
        let cursor = self.history_len() + self.cursor.row;
        let (start, end) = match redraw {
            PromptRedraw::None => return,
            PromptRedraw::Last => (cursor, cursor + 1),
            PromptRedraw::All => {
                let mut previous = self
                    .all_rows()
                    .rev()
                    .skip(self.height - self.cursor.row - 1);
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
                (start, self.history_len() + self.height)
            }
        };
        for absolute in start..end {
            let (index, row) = self.locate(absolute);
            self.pages.pages[index].erase(row, 0, usize::MAX, Color::Default, false);
        }
    }
}

type GridPointKey = (u64, usize);
