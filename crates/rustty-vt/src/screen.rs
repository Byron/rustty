//! Owned screen storage. Rows retain identity when they enter scrollback.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

#[path = "screen/serde.rs"]
mod serde_impl;

use crate::page_list::{Page, PageAllocationInfo, PageList};
use crate::page_resources::{
    GraphemeAdmission, GraphemeAllocation, Hyperlink, HyperlinkAdmission, HyperlinkFull, SetFull,
    StyleAdmission,
};

#[derive(Clone, Copy)]
enum PageResource {
    Styles,
    Graphemes,
    Links,
    Strings,
}

impl PageResource {
    fn for_link(error: HyperlinkFull) -> Option<Self> {
        match error {
            HyperlinkFull::Strings => Some(Self::Strings),
            HyperlinkFull::Set(SetFull::NeedsRehash) => None,
            HyperlinkFull::Set(SetFull::OutOfMemory) | HyperlinkFull::Map => Some(Self::Links),
        }
    }
}

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

    fn storage_bytes(&self) -> usize {
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
    fn deref(&self) -> &str {
        self.as_str()
    }
}

/// Cell attributes and an inline scalar. Grapheme handles require the owning
/// Screen; use Screen::cell_text or snapshot_viewport to retain detached text.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct Cell {
    #[serde(skip)]
    pub(crate) style_id: u16,
    #[serde(skip)]
    pub(crate) link_id: u16,
    #[serde(skip)]
    pub(crate) grapheme: Option<GraphemeAllocation>,
    /// Inline scalar; absent for unwritten cells and wide-glyph continuations.
    #[serde(skip)]
    pub codepoint: Option<char>,
    /// Zero denotes the continuation of a two-cell glyph.
    pub width: u8,
    pub style: Style,
    #[serde(flatten, with = "hyperlink_serde")]
    pub hyperlink: Option<Arc<HyperlinkData>>,
    pub protected: bool,
    pub semantic: SemanticContent,
    /// Padding before a wide glyph that wrapped at the right edge.
    pub spacer_head: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            style_id: 0,
            link_id: 0,
            grapheme: None,
            codepoint: None,
            width: 1,
            style: Style::default(),
            hyperlink: None,
            protected: false,
            semantic: SemanticContent::Output,
            spacer_head: false,
        }
    }
}

impl Cell {
    pub(crate) fn clear_link(&mut self) {
        self.link_id = 0;
        self.hyperlink = None;
    }

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
        self.codepoint.is_none() && self.width != 0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct Row {
    #[serde(skip)]
    pub(crate) resource_page: Option<u64>,
    pub id: u64,
    #[serde(skip)]
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
    sources: Vec<(Option<u64>, u16, Style, u16)>,
    graphemes: Vec<Option<Arc<str>>>,
}

impl Row {
    pub(crate) fn new(id: u64, cols: usize, background: Color) -> Self {
        Self {
            resource_page: None,
            id,
            cells: std::iter::repeat_with(|| Cell::blank(background))
                .take(cols)
                .collect(),
            wrapped: false,
            wrap_continuation: false,
            semantic: SemanticContent::Output,
            dirty: true,
        }
    }

    pub(crate) fn storage_bytes(&self) -> usize {
        // Shared payloads are charged once per row, conservatively again across
        // rows, so cached history charges remain additive as owners come and go.
        let mut bytes = size_of::<Self>()
            .saturating_add(self.cells.capacity().saturating_mul(size_of::<Cell>()));
        let mut links = None::<HashSet<_>>;
        for cell in &self.cells {
            if let Some(allocation) = cell.grapheme {
                // Conservative full UTF-8 payload plus Arc counters. Shared
                // clusters are charged per cell so row totals stay additive.
                bytes = bytes
                    .saturating_add(2 * size_of::<usize>() + 4 * (usize::from(allocation.len) + 1));
            }
            if let Some(link) = &cell.hyperlink
                && links
                    .get_or_insert_with(HashSet::new)
                    .insert(Arc::as_ptr(link))
            {
                bytes = bytes.saturating_add(link.storage_bytes());
            }
        }
        bytes
    }

    pub(crate) fn used(&self) -> usize {
        self.cells
            .iter()
            .rposition(|c| {
                c.codepoint.is_some() || c.width == 0 || c.style.background != Color::Default
            })
            .map_or(0, |i| i + 1)
    }

    pub(crate) fn repair_wide(&mut self, background: Color) {
        for col in 0..self.cells.len() {
            // A copied spacer head becomes an ordinary blank cell when the
            // row grows. Its style, hyperlink and semantic content survive.
            if self.cells[col].spacer_head && col + 1 != self.cells.len() {
                self.cells[col].spacer_head = false;
            }
            if self.cells[col].width == 0 && (col == 0 || self.cells[col - 1].width != 2)
                || self.cells[col].width == 2
                    && (col + 1 == self.cells.len() || self.cells[col + 1].width != 0)
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
    pub rows: Vec<Row>,
    #[serde(skip)]
    pub history: VecDeque<Row>,
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
    /// Authoritative only while the host memory cap is enabled.
    #[serde(skip)]
    pub(crate) history_bytes: usize,
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
            viewport_pin: Some(GridPoint { row: 0, col: 0 }),
            kitty_keyboard: KittyKeyboard::default(),
            saved_cursor: None,
            charset: CharsetState::default(),
            iso_protection: false,
            limits,
            memory_limit: None,
            history_bytes: 0,
            pages: PageList::new(cols as u16, rows),
            cursor_style: None,
            cursor_link: None,
            next_row: rows as u64,
            tracked: TrackedPoints::default(),
        }
    }

    pub fn all_rows(&self) -> impl DoubleEndedIterator<Item = &Row> {
        self.history.iter().chain(self.rows.iter())
    }

    /// Resolve text against its owning screen. Ordinary scalar reads allocate nothing.
    #[inline]
    pub fn cell_text<'a>(&'a self, row: &Row, col: usize) -> CellText<'a> {
        let cell = &row.cells[col];
        if cell.codepoint.is_some()
            && let Some(allocation) = cell.grapheme
        {
            let page = self
                .pages
                .pages
                .iter()
                .find(|page| Some(page.serial) == row.resource_page)
                .expect("grapheme row must have a live owning page");
            CellText(CellTextStorage::Grapheme(page.graphemes.text(allocation)))
        } else {
            CellText::scalar(cell.codepoint)
        }
    }

    pub fn row_text(&self, row: &Row) -> String {
        let mut result = String::new();
        for (col, cell) in row.cells.iter().enumerate() {
            if cell.width == 0 || cell.spacer_head {
                continue;
            }
            if cell.codepoint.is_none() {
                result.push(' ');
            } else {
                result.push_str(&self.cell_text(row, col));
            }
        }
        result.truncate(result.trim_end_matches(' ').len());
        result
    }

    /// Replace the text of an active cell, retaining its other attributes.
    /// Clusters have the same 64-suffix-scalar limit as printed text.
    pub fn set_cell_text(&mut self, row: usize, col: usize, text: &str) {
        let mut chars = text.chars();
        let codepoint = chars.next();
        let suffix_len = chars.count();
        assert!(suffix_len <= 64, "cell grapheme is too long");
        self.edit_row(row, |line| {
            line.cells[col].codepoint = codepoint;
            line.cells[col].grapheme = None;
            line.dirty = true;
        });
        if suffix_len != 0 {
            let absolute = self.history.len() + row;
            let allocation = self
                .acquire_grapheme(absolute, suffix_len as u8)
                .expect("cell grapheme must fit after page growth");
            let index = self.pages.page_index(absolute);
            self.pages.pages[index]
                .graphemes
                .set_text(allocation, Arc::from(text));
            self.rows[row].cells[col].grapheme = Some(allocation);
        }
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
        let mut pages = self.pages.clone_range(
            self.history.len().saturating_sub(self.viewport_offset),
            self.rows.len(),
        );
        let rows: Vec<_> = self
            .viewport()
            .map(|source| {
                let mut row = source.clone();
                for cell in &mut row.cells {
                    cell.style_id = 0;
                    cell.link_id = 0;
                }
                row
            })
            .collect();
        for page in &mut pages.pages {
            let source = self
                .pages
                .pages
                .iter()
                .find(|source| source.serial == page.serial)
                .unwrap();
            page.graphemes = source.graphemes.clone_subset(
                rows.iter()
                    .filter(|row| row.resource_page == Some(page.serial))
                    .flat_map(|row| row.cells.iter().filter_map(|cell| cell.grapheme)),
            );
        }
        Self {
            metadata: self.metadata.clone(),
            graphics: self.graphics.snapshot(self),
            columns: self.columns,
            rows,
            history: VecDeque::new(),
            cursor,
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
            history_bytes: 0,
            pages,
            cursor_style: None,
            cursor_link: None,
            next_row: self.next_row,
            tracked: TrackedPoints::default(),
        }
    }

    pub fn scroll_viewport(&mut self, rows: isize) {
        let previous = self.viewport_offset;
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
        } else if self.viewport_offset < self.history.len()
            || previous > 0
                && previous < self.history.len()
                && previous.saturating_add_signed(rows) == self.history.len()
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

    fn release_link_cell(&mut self, serial: u64, id: u16) {
        if let Some(page) = self
            .pages
            .pages
            .iter_mut()
            .find(|page| page.serial == serial)
        {
            page.links.release_cell(id);
        }
    }

    pub(crate) fn release_row_resources(&mut self, row: &Row) {
        if let Some(serial) = row.resource_page
            && let Some(page) = self
                .pages
                .pages
                .iter_mut()
                .find(|page| page.serial == serial)
        {
            for cell in &row.cells {
                page.styles.release(cell.style_id);
                page.links.release_cell(cell.link_id);
                if let Some(grapheme) = cell.grapheme {
                    page.graphemes.release(grapheme);
                }
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
        let row = if absolute < self.history.len() {
            &mut self.history[absolute]
        } else {
            &mut self.rows[absolute - self.history.len()]
        };
        Self::edit_owned_row(&mut self.pages.pages[index], row, edit)
    }

    fn edit_owned_row<R>(page: &mut Page, row: &mut Row, edit: impl FnOnce(&mut Row) -> R) -> R {
        row.resource_page = Some(page.serial);
        let mut counts: HashMap<u16, isize> = HashMap::new();
        let mut links: HashMap<u16, isize> = HashMap::new();
        let mut graphemes = HashSet::new();
        for cell in &row.cells {
            if cell.link_id != 0 {
                *links.entry(cell.link_id).or_default() -= 1;
            }
            if cell.style_id != 0 {
                *counts.entry(cell.style_id).or_default() -= 1;
            }
            if let Some(grapheme) = cell.grapheme {
                graphemes.insert(grapheme);
            }
        }
        let result = edit(row);
        for cell in &row.cells {
            if cell.link_id != 0 {
                *links.entry(cell.link_id).or_default() += 1;
            }
            if cell.style_id != 0 {
                *counts.entry(cell.style_id).or_default() += 1;
            }
            if let Some(grapheme) = cell.grapheme {
                graphemes.remove(&grapheme);
            }
        }
        let styles = &mut page.styles;
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
        for grapheme in graphemes {
            page.graphemes.release(grapheme);
        }
        for (id, count) in links {
            if count < 0 {
                for _ in 0..-count {
                    page.links.release_cell(id);
                }
            } else {
                for _ in 0..count {
                    page.links.retain_moved_cell(id);
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
        row.resource_page = Some(serial);
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
                self.pages.pages[index].links.release_cell(cell.link_id);
                if let Some(grapheme) = cell.grapheme {
                    self.pages.pages[index].graphemes.release(grapheme);
                }
                *cell = Cell::blank(background);
            }
        }
        row.dirty = true;
    }

    fn cursor_page_index(&self) -> usize {
        self.pages
            .page_index_from_end(self.rows.len() - 1 - self.cursor.row)
    }

    /// Write the eligible ASCII prefix; the caller handles complex cells and wraps.
    pub(crate) fn write_cursor_ascii(&mut self, bytes: &[u8]) -> usize {
        self.sync_cursor_resources();
        let index = self.cursor_page_index();
        let page = &mut self.pages.pages[index];
        let row = &mut self.rows[self.cursor.row];
        let style_id = self.cursor_style.map_or(0, |(_, id)| id);
        let mut written = 0;
        for (cell, &byte) in row.cells[self.cursor.col..].iter_mut().zip(bytes) {
            if cell.width != 1
                || cell.grapheme.is_some()
                || cell.link_id != 0
                || cell.hyperlink.is_some()
            {
                break;
            }
            if cell.style_id != style_id {
                page.styles.release(cell.style_id);
                page.styles.retain(style_id);
            }
            cell.codepoint = Some(char::from(byte));
            cell.style_id = style_id;
            cell.style = self.cursor.style;
            cell.protected = self.cursor.protected;
            cell.semantic = self.cursor.semantic;
            cell.spacer_head = false;
            written += 1;
        }
        if written != 0 {
            row.resource_page = Some(page.serial);
            row.dirty = true;
        }
        written
    }

    /// Consume simple Unicode cells in one row, stopping before a scalar fallback.
    /// The caller handles wraps and enables this only with full horizontal margins.
    pub(crate) fn write_cursor_utf8(
        &mut self,
        text: &str,
        right: usize,
        graphemes: bool,
        state: &mut u8,
    ) -> (usize, usize) {
        // Combining-heavy overwrites usually fall back immediately. Reject them
        // before looking up a page or checking its cursor resource handles.
        if self.rows[self.cursor.row].cells[self.cursor.col]
            .grapheme
            .is_some()
            || text
                .chars()
                .next()
                .is_none_or(|cp| cp as u32 > 255 && crate::unicode::codepoint_width(cp) == 0)
        {
            return (0, 0);
        }
        let index = self.cursor_page_index();
        let page = &mut self.pages.pages[index];
        // Ignored or capped scalar anchors may not have synchronized a public
        // cursor edit. Let scalar printing do that after its grapheme check.
        if self.cursor_link.is_some() {
            return (0, 0);
        }
        let style_id = match self.cursor_style {
            Some((owner, id))
                if owner == page.serial && *page.styles.get(id) == self.cursor.style =>
            {
                id
            }
            None if self.cursor.style == Style::default() => 0,
            _ => return (0, 0),
        };
        let row = &mut self.rows[self.cursor.row];
        let mut col = self.cursor.col;
        let (mut bytes, mut scalars) = (0, 0);
        for cp in text.chars() {
            let width = if cp as u32 <= 255 {
                1
            } else {
                crate::unicode::codepoint_width(cp)
            };
            let end = col + usize::from(width);
            if width == 0 || end > right + 1 {
                break;
            }
            let mut next_state = *state;
            if cp as u32 > 255 && graphemes && col > 0 {
                let mut previous = col - 1;
                if row.cells[previous].width == 0 {
                    previous = previous.saturating_sub(1);
                }
                let previous = &row.cells[previous];
                if previous.grapheme.is_some()
                    || previous.codepoint.is_some_and(|last| {
                        !crate::unicode::grapheme_break(last, cp, &mut next_state)
                    })
                {
                    break;
                }
            }
            let cells = &mut row.cells[col..end];
            let same_width = cells[0].width == width && (width == 1 || cells[1].width == 0);
            if (!same_width && !cells.iter().all(|cell| cell.width == 1))
                || cells.iter().any(|cell| {
                    cell.grapheme.is_some() || cell.link_id != 0 || cell.hyperlink.is_some()
                })
            {
                break;
            }
            for (offset, cell) in cells.iter_mut().enumerate() {
                if cell.style_id != style_id {
                    page.styles.release(cell.style_id);
                    page.styles.retain(style_id);
                }
                cell.codepoint = (offset == 0).then_some(cp);
                cell.width = if offset == 0 { width } else { 0 };
                cell.style_id = style_id;
                cell.style = self.cursor.style;
                cell.protected = self.cursor.protected;
                cell.semantic = self.cursor.semantic;
                cell.spacer_head = false;
            }
            *state = next_state;
            col = end;
            bytes += cp.len_utf8();
            scalars += 1;
            if col > right {
                break;
            }
        }
        if bytes != 0 {
            row.resource_page = Some(page.serial);
            row.dirty = true;
            self.cursor.col = col.min(right);
            self.cursor.pending_wrap = col > right;
        }
        (bytes, scalars)
    }

    /// Replace printed cells directly, keeping references whose style is unchanged.
    /// Physical row extension must precede this: it can rebuild resource tables.
    pub(crate) fn write_cursor_cell(
        &mut self,
        codepoint: Option<char>,
        width: u8,
        spacer_head: bool,
    ) {
        self.sync_cursor_resources();
        let y = self.cursor.row;
        let col = self.cursor.col;
        let old_width = self.rows[y].cells[col].width;
        if y > 0 && col <= 1 && old_width != width && old_width != 1 {
            let previous = &mut self.rows[y - 1];
            previous.cells.last_mut().unwrap().spacer_head = false;
            previous.dirty = true;
        }
        let index = self.cursor_page_index();
        let page = &mut self.pages.pages[index];
        let row = &mut self.rows[y];
        row.resource_page = Some(page.serial);
        let style_id = self.cursor_style.map_or(0, |(_, id)| id);
        let end = (col + usize::from(width)).min(row.cells.len());
        // Retire both halves when a write overlaps an existing wide glyph.
        let clear_start = col - usize::from(old_width == 0 && col > 0);
        let clear_end = end + usize::from(end < row.cells.len() && row.cells[end - 1].width == 2);
        for (offset, cell) in row.cells[clear_start..clear_end].iter_mut().enumerate() {
            let x = clear_start + offset;
            let replacement_style = if x >= col && x < end { style_id } else { 0 };
            if cell.style_id != replacement_style {
                page.styles.release(cell.style_id);
                page.styles.retain(replacement_style);
            }
            page.links.release_cell(cell.link_id);
            if let Some(grapheme) = cell.grapheme {
                page.graphemes.release(grapheme);
            }
            *cell = if x < col || x >= end {
                Cell::blank(self.cursor.style.background)
            } else {
                Cell {
                    style_id,
                    link_id: 0,
                    grapheme: None,
                    codepoint: if x == col { codepoint } else { None },
                    width: if x == col { width } else { 0 },
                    style: self.cursor.style,
                    hyperlink: self.cursor.hyperlink.clone(),
                    protected: self.cursor.protected,
                    semantic: self.cursor.semantic,
                    spacer_head,
                }
            };
        }
        row.dirty = true;
        if self.cursor.hyperlink.is_some() {
            // Installing links can grow the page, so retain no page borrow here.
            self.set_cell_cursor_hyperlink(col);
            if width == 2 && col + 1 < self.rows[y].cells.len() {
                self.set_cell_cursor_hyperlink(col + 1);
            }
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

    fn acquire_cursor_link(&mut self, link: &Hyperlink) -> Option<(u64, u16)> {
        loop {
            let index = self.cursor_page_index();
            match self.pages.pages[index].links.insert(link) {
                Ok(id) => return Some((self.pages.pages[index].serial, id)),
                Err(error) => self
                    .grow_resource_page(index, PageResource::for_link(error))
                    .ok()?,
            }
        }
    }

    pub(crate) fn start_hyperlink(&mut self, uri: &[u8], explicit: Option<&[u8]>) {
        let id = if let Some(id) = explicit {
            HyperlinkId::Explicit(id.to_vec())
        } else {
            let id = self.metadata.hyperlink_implicit_id;
            self.metadata.hyperlink_implicit_id = id.wrapping_add(1);
            HyperlinkId::Implicit(id)
        };
        let link = Hyperlink {
            id,
            uri: uri.to_vec(),
        };
        self.end_hyperlink();
        if let Some(reference) = self.acquire_cursor_link(&link) {
            self.cursor_link = Some(reference);
            self.cursor.hyperlink = Some(Arc::new(HyperlinkData::new(uri, Some(link.id))));
        } else if explicit.is_none() {
            self.metadata.hyperlink_implicit_id =
                self.metadata.hyperlink_implicit_id.wrapping_sub(1);
        }
    }

    /// Page movement renews implicit cursor links. Resource growth on the same
    /// page reinstalls their existing identities; printed cells retain theirs.
    pub(crate) fn sync_cursor_resources(&mut self) {
        if self.cursor_style.is_none()
            && self.cursor_link.is_none()
            && self.cursor.style == Style::default()
            && self.cursor.hyperlink.is_none()
        {
            return;
        }
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
        if let Some(link) = Hyperlink::from_cursor(&self.cursor) {
            self.cursor_link = self.acquire_cursor_link(&link);
            if self.cursor_link.is_none() {
                self.end_hyperlink();
            }
        }
    }

    pub(crate) fn set_cell_cursor_hyperlink(&mut self, col: usize) {
        while let Some((_, id)) = self.cursor_link {
            let index = self.cursor_page_index();
            if self.pages.pages[index].links.retain_cell(id).is_ok() {
                self.rows[self.cursor.row].cells[col].link_id = id;
                return;
            }
            // Match native map growth's extra URI reservation. Page rebuilding
            // can drop a cursor link that no longer fits alongside live cells.
            while let Some(link) = Hyperlink::from_cursor(&self.cursor) {
                if self.pages.pages[index].links.reserve_uri(link.uri.len()) {
                    break;
                }
                if self
                    .grow_resource_page(index, Some(PageResource::Strings))
                    .is_err()
                {
                    self.rows[self.cursor.row].cells[col].clear_link();
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
        self.rows[self.cursor.row].cells[col].clear_link();
    }

    fn acquire_link_cell(
        &mut self,
        absolute: usize,
        link: &Hyperlink,
        preferred: u16,
    ) -> Result<u16, SetFull> {
        loop {
            let index = self.pages.page_index(absolute);
            match self.pages.pages[index].links.copy_cell(link, preferred) {
                Ok(id) => return Ok(id),
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
        let absolute = self.history.len() + self.cursor.row;
        match self.acquire_style(absolute, self.cursor.style, None) {
            Ok(id) => {
                self.cursor_style = Some((self.pages.pages[self.cursor_page_index()].serial, id));
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

    pub(crate) fn retain_cursor_style_for_cell(&mut self) -> u16 {
        self.sync_cursor_style();
        let id = self.cursor_style.map_or(0, |(_, id)| id);
        let index = self.cursor_page_index();
        self.pages.pages[index].styles.retain(id);
        self.rows[self.cursor.row].resource_page = Some(self.pages.pages[index].serial);
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
                    .grow_resource_page(
                        index,
                        (error == SetFull::OutOfMemory).then_some(PageResource::Styles),
                    )
                    .is_err()
                {
                    self.split_resource_page(absolute)?;
                }
                let index = self.pages.page_index(absolute);
                acquire(&mut self.pages.pages[index].styles)
            }
        }
    }

    fn acquire_grapheme(
        &mut self,
        absolute: usize,
        len: u8,
    ) -> Result<GraphemeAllocation, SetFull> {
        loop {
            let index = self.pages.page_index(absolute);
            if let Ok(allocation) = self.pages.pages[index].graphemes.acquire(len) {
                return Ok(allocation);
            }
            if self
                .grow_resource_page(index, Some(PageResource::Graphemes))
                .is_err()
            {
                self.split_resource_page(absolute)?;
                let index = self.pages.page_index(absolute);
                return self.pages.pages[index].graphemes.acquire(len);
            }
        }
    }

    pub(crate) fn append_grapheme(&mut self, col: usize, cp: char) -> Result<(), SetFull> {
        let y = self.cursor.row;
        let absolute = self.history.len() + y;
        let mut index = self.cursor_page_index();
        let previous = self.rows[y].cells[col].grapheme;
        assert!(previous.is_none_or(|allocation| allocation.len < 64));
        // Save the text before page growth can move its owner. A base scalar
        // plus 64 suffix scalars needs at most 260 UTF-8 bytes.
        let mut bytes = [0; 4 * (64 + 1)];
        let text = self.cell_text(&self.rows[y], col);
        let len = text.len();
        bytes[..len].copy_from_slice(text.as_bytes());
        let len = len + cp.encode_utf8(&mut bytes[len..]).len();
        let allocation = match self.pages.pages[index].graphemes.append(previous) {
            Ok(allocation) => allocation,
            Err(_) => {
                if self
                    .grow_resource_page(index, Some(PageResource::Graphemes))
                    .is_err()
                {
                    self.split_resource_page(absolute)?;
                }
                index = self.cursor_page_index();
                self.pages.pages[index]
                    .graphemes
                    .append(self.rows[y].cells[col].grapheme)?
            }
        };
        self.rows[y].resource_page = Some(self.pages.pages[index].serial);
        self.rows[y].cells[col].grapheme = Some(allocation);
        self.pages.pages[index].graphemes.set_text(
            allocation,
            Arc::from(std::str::from_utf8(&bytes[..len]).unwrap()),
        );
        self.rows[y].dirty = true;
        Ok(())
    }

    pub(crate) fn move_wrapped_grapheme(&mut self, source_col: usize, suffix: &str) {
        let absolute = self.history.len() + self.cursor.row;
        let source = absolute.checked_sub(1).and_then(|source| {
            let before =
                (source < self.history.len()).then(|| self.history[source].storage_bytes());
            let allocation = self.physical_row_mut(source).cells[source_col]
                .grapheme
                .take()?;
            if let Some(before) = before {
                self.history_bytes = self
                    .history_bytes
                    .saturating_sub(before)
                    .saturating_add(self.history[source].storage_bytes());
            }
            Some((self.pages.page_index(source), allocation))
        });
        let destination_index = self.pages.page_index(absolute);
        if let Some((source_index, allocation)) = source
            && source_index == destination_index
        {
            let cell = &mut self.rows[self.cursor.row].cells[self.cursor.col];
            cell.grapheme = Some(allocation);
            let mut text = String::with_capacity(4 + suffix.len());
            text.push(cell.codepoint.expect("wrapped grapheme has a base"));
            text.push_str(suffix);
            self.pages.pages[destination_index]
                .graphemes
                .set_text(allocation, Arc::from(text));
        } else {
            // A host memory cap may evict the source row during wrapping.
            // The saved suffix still belongs to the active destination cell.
            for cp in suffix.chars() {
                if self.append_grapheme(self.cursor.col, cp).is_err() {
                    break;
                }
            }
            if let Some((source_index, allocation)) = source {
                self.pages.pages[source_index].graphemes.release(allocation);
            }
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
            for cell in &row.cells {
                if cell.style_id != 0 {
                    styles.insert(cell.style_id);
                }
                if let Some(grapheme) = cell.grapheme {
                    grapheme_bytes +=
                        BitmapAllocator::<16>::bytes_required(usize::from(grapheme.len) * 4)
                            .unwrap();
                }
                if let Some(link) = &cell.hyperlink {
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

    fn split_resource_page(&mut self, absolute: usize) -> Result<(), SetFull> {
        let index = self.pages.page_index(absolute);
        let (page, relative) = self.pages.page_at(absolute);
        let start = absolute - relative;
        let end = start + usize::from(page.rows);
        let above = self.exact_resource_range_bytes(start, absolute + 1, page.columns);
        let below = self.exact_resource_range_bytes(absolute, end, page.columns);
        let split = if above < below && absolute + 1 < end {
            relative + 1
        } else {
            relative
        };
        let referenced_cursor = self.cursor_style.is_some() || self.cursor_link.is_some();
        if !self.pages.split(index, split as u16) {
            return Err(SetFull::OutOfMemory);
        }
        if split != 0 {
            for row in start + split..end {
                self.sync_resource_row(row);
            }
        }
        if referenced_cursor {
            self.sync_cursor_resources();
        }
        Ok(())
    }

    fn rebuild_resource_page<'a>(
        page: &mut Page,
        rows: impl Iterator<Item = &'a mut Row>,
        grow: Option<PageResource>,
    ) -> Result<(), SetFull> {
        let mut capacity = page.capacity;
        if let Some(resource) = grow {
            let (old, default, maximum, used) = match resource {
                PageResource::Styles => (
                    u64::from(capacity.styles),
                    16,
                    u64::from(u16::MAX),
                    page.styles.count() as u64,
                ),
                PageResource::Graphemes => (
                    u64::from(capacity.grapheme_bytes),
                    1024,
                    u64::from(u32::MAX),
                    page.graphemes.used_bytes() as u64,
                ),
                PageResource::Links => (
                    u64::from(capacity.hyperlink_bytes),
                    192,
                    u64::from(u16::MAX),
                    0,
                ),
                PageResource::Strings => (
                    u64::from(capacity.string_bytes),
                    2048,
                    u64::from(u32::MAX),
                    0,
                ),
            };
            if old == maximum {
                return Err(SetFull::OutOfMemory);
            }
            let assign = |cap: &mut crate::page_layout::PageCapacity, value: u64| match resource {
                PageResource::Styles => cap.styles = value as u16,
                PageResource::Graphemes => cap.grapheme_bytes = value as u32,
                PageResource::Links => cap.hyperlink_bytes = value as u16,
                PageResource::Strings => cap.string_bytes = value as u32,
            };
            let increased = if old == 0 {
                default
            } else {
                (old * 2).min(maximum)
            };
            assign(&mut capacity, increased);
            capacity.layout().map_err(|_| SetFull::OutOfMemory)?;
            if used != 0 && page.rows != 0 {
                let density = used * u64::from(capacity.rows) / u64::from(page.rows);
                let projected = (density + density / 4).min(old * 32).min(maximum);
                if projected > increased {
                    let mut projected_capacity = capacity;
                    assign(&mut projected_capacity, projected);
                    if projected_capacity.layout().is_ok() {
                        capacity = projected_capacity;
                    }
                }
            }
        }
        let serial = page.serial;
        let layout = capacity.metadata().unwrap();
        let mut styles = StyleAdmission::new(layout.styles_layout);
        let mut graphemes = GraphemeAdmission::new(
            layout.grapheme_alloc_layout,
            layout.grapheme_map_layout.capacity as usize,
        );
        let mut links = HyperlinkAdmission::new(
            layout.hyperlink_set_layout,
            layout.string_alloc_layout,
            layout.hyperlink_map_layout.capacity as usize * 80 / 100,
        );
        let mut pending = Vec::new();
        for row in rows.filter(|row| row.resource_page == Some(serial)) {
            for cell in &mut row.cells {
                if cell.style_id == 0 && cell.grapheme.is_none() && cell.link_id == 0 {
                    continue;
                }
                let grapheme = cell
                    .grapheme
                    .map(|allocation| {
                        let next = graphemes.acquire(allocation.len)?;
                        graphemes.set_text(next, page.graphemes.text_arc(allocation));
                        Ok(next)
                    })
                    .transpose()?;
                let link_id = if cell.link_id == 0 {
                    0
                } else {
                    links
                        .copy_cell(page.links.get(cell.link_id), cell.link_id)
                        .map_err(|_| SetFull::OutOfMemory)?
                };
                let id = if cell.style_id == 0 {
                    0
                } else {
                    styles.acquire_with_id(*page.styles.get(cell.style_id), cell.style_id)?
                };
                pending.push((cell, id, grapheme, link_id));
            }
        }
        // Reordering live entries can itself exhaust a collision chain. Until
        // every admission succeeds, cell IDs must still address the old set.
        for (cell, id, grapheme, link_id) in pending {
            cell.style_id = id;
            cell.grapheme = grapheme;
            cell.link_id = link_id;
        }
        page.capacity = capacity;
        page.styles = styles;
        page.graphemes = graphemes;
        page.links = links;
        page.layout_generation = page.layout_generation.wrapping_add(1);
        Ok(())
    }

    fn grow_resource_page(
        &mut self,
        index: usize,
        grow: Option<PageResource>,
    ) -> Result<(), SetFull> {
        let serial = self.pages.pages[index].serial;
        Self::rebuild_resource_page(
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
        if self.cursor_link.is_some_and(|(owner, _)| owner == serial) {
            self.cursor_link = None;
            if let Some(link) = Hyperlink::from_cursor(&self.cursor) {
                match self.pages.pages[index].links.insert(&link) {
                    Ok(id) => self.cursor_link = Some((serial, id)),
                    Err(_) => self.end_hyperlink(),
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
        line.resource_page = Some(self.pages.pages[index].serial);
        if source_id == 0 {
            return Ok(0);
        }
        match self.pages.pages[index]
            .styles
            .acquire_with_id(style, source_id)
        {
            Ok(id) => Ok(id),
            Err(error) => {
                Self::rebuild_resource_page(
                    &mut self.pages.pages[index],
                    output.iter_mut().chain(std::iter::once(line)),
                    (error == SetFull::OutOfMemory).then_some(PageResource::Styles),
                )?;
                self.pages.pages[index]
                    .styles
                    .acquire_with_id(style, source_id)
            }
        }
    }

    fn reflow_link(&mut self, output: &mut [Row], line: &mut Row, col: usize, preferred: u16) {
        let Some(link) = Hyperlink::from_cell(&line.cells[col]) else {
            return;
        };
        let index = self.pages.page_index(output.len());
        loop {
            match self.pages.pages[index].links.reflow_cell(&link, preferred) {
                Ok(id) => {
                    line.cells[col].link_id = id;
                    return;
                }
                Err(error) => {
                    Self::rebuild_resource_page(
                        &mut self.pages.pages[index],
                        output.iter_mut().chain(std::iter::once(&mut *line)),
                        PageResource::for_link(error),
                    )
                    .expect("reflow hyperlinks must fit after page growth");
                }
            }
        }
    }

    /// Rehome complete rows after physical page movement. Same-page rotations
    /// keep their IDs; crossing a page copies with addWithId before releasing
    /// the source. The direction follows the native copy operation. The newly
    /// inserted blank row has no resources to scan or transfer.
    pub(crate) fn sync_resource_pages(&mut self, reverse: bool, blank: usize) {
        debug_assert!(self.rows[blank].cells.iter().all(|cell| {
            cell.style_id == 0
                && cell.link_id == 0
                && cell.grapheme.is_none()
                && cell.hyperlink.is_none()
        }));
        let index = self.pages.page_index_from_end(self.rows.len() - 1 - blank);
        self.rows[blank].resource_page = Some(self.pages.pages[index].serial);
        // History insertion leaves surviving rows on their original pages.
        // Check whole page ranges before paying for individual owner lookups.
        let mut end = self.rows.len();
        for page in self.pages.pages.iter().rev() {
            let start = end.saturating_sub(usize::from(page.rows));
            if self.rows[start..end]
                .iter()
                .any(|row| row.resource_page != Some(page.serial))
            {
                break;
            }
            if start == 0 {
                self.sync_cursor_resources();
                return;
            }
            end = start;
        }
        let count = self.rows.len();
        for offset in 0..count {
            let absolute = self.history.len() + if reverse { count - 1 - offset } else { offset };
            self.sync_resource_row(absolute);
        }
        self.sync_cursor_resources();
    }

    fn sync_resource_row(&mut self, absolute: usize) {
        let index = self
            .pages
            .page_index_from_end(self.history.len() + self.rows.len() - 1 - absolute);
        let serial = self.pages.pages[index].serial;
        let row = self.all_rows().nth(absolute).unwrap();
        if row.resource_page == Some(serial) {
            return;
        }
        // Fresh scroll rows and resource-free copies need only an owner. Keep
        // inline attributes; unowned hyperlink payloads still need adoption.
        if row.cells.iter().all(|cell| {
            cell.style_id == 0
                && cell.link_id == 0
                && cell.grapheme.is_none()
                && cell.hyperlink.is_none()
        }) {
            self.physical_row_mut(absolute).resource_page = Some(serial);
            return;
        }
        let previous = row.resource_page;
        let old_page =
            previous.and_then(|serial| self.pages.pages.iter().find(|page| page.serial == serial));
        let old_styles = old_page.map(|page| &page.styles);
        let resources: Vec<_> = row
            .cells
            .iter()
            .map(|cell| {
                let value = if cell.style_id != 0 {
                    old_styles.map_or(cell.style, |set| *set.get(cell.style_id))
                } else {
                    Style::default()
                };
                (
                    cell.style_id,
                    value,
                    cell.grapheme.map(|allocation| {
                        (
                            allocation,
                            old_page
                                .expect("grapheme has an owner")
                                .graphemes
                                .text_arc(allocation),
                        )
                    }),
                    cell.link_id,
                    Hyperlink::from_cell(cell),
                )
            })
            .collect();
        let row = self.physical_row_mut(absolute);
        row.resource_page = Some(serial);
        for cell in &mut row.cells {
            cell.style_id = 0;
            cell.grapheme = None;
            cell.link_id = 0;
        }
        for (col, (old_id, style, grapheme, old_link_id, link)) in resources.into_iter().enumerate()
        {
            if let Some((grapheme, text)) = &grapheme {
                let allocation = self.acquire_grapheme(absolute, grapheme.len).ok();
                if let Some(allocation) = allocation {
                    let index = self.pages.page_index(absolute);
                    self.pages.pages[index]
                        .graphemes
                        .set_text(allocation, text.clone());
                }
                self.physical_row_mut(absolute).cells[col].grapheme = allocation;
            }
            if let Some(link) = link {
                let id = self
                    .acquire_link_cell(absolute, &link, old_link_id)
                    .unwrap_or(0);
                let cell = &mut self.physical_row_mut(absolute).cells[col];
                cell.link_id = id;
                if id == 0 {
                    cell.clear_link();
                }
            }
            if old_id != 0 {
                let id = self
                    .acquire_style(absolute, style, Some(old_id))
                    .unwrap_or(0);
                self.physical_row_mut(absolute).cells[col].style_id = id;
                if id == 0 {
                    self.physical_row_mut(absolute).cells[col].style = Style::default();
                }
            }
            if let Some(previous) = previous {
                self.release_style(previous, old_id);
                self.release_link_cell(previous, old_link_id);
                if let Some((grapheme, _)) = grapheme
                    && let Some(page) = self
                        .pages
                        .pages
                        .iter_mut()
                        .find(|page| page.serial == previous)
                {
                    page.graphemes.release(grapheme);
                }
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
        let sources = row
            .cells
            .iter()
            .enumerate()
            .map(|(column, cell)| {
                let owner = if column < end { Some(source) } else { recycled }
                    .and_then(|row| row.resource_page);
                let value = if cell.style_id == 0 {
                    Style::default()
                } else {
                    owner
                        .and_then(|serial| {
                            self.pages.pages.iter().find(|page| page.serial == serial)
                        })
                        .map_or(cell.style, |page| *page.styles.get(cell.style_id))
                };
                (owner, cell.style_id, value, cell.link_id)
            })
            .collect();
        let graphemes = row
            .cells
            .iter()
            .enumerate()
            .map(|(col, cell)| {
                cell.grapheme.map(|allocation| {
                    let owner = if col < end { Some(source) } else { recycled }.unwrap();
                    self.pages
                        .pages
                        .iter()
                        .find(|page| Some(page.serial) == owner.resource_page)
                        .unwrap()
                        .graphemes
                        .text_arc(allocation)
                })
            })
            .collect();
        RowCopy {
            row,
            sources,
            graphemes,
        }
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
        let mut transferred = HashSet::new();
        for (y, mut copy) in copies {
            let index = self.pages.page_index(self.history.len() + y);
            let serial = self.pages.pages[index].serial;
            copy.row.resource_page = Some(serial);
            let mut graphemes = Vec::new();
            for ((cell, &(owner, id, _, link_id)), text) in copy
                .row
                .cells
                .iter_mut()
                .zip(&copy.sources)
                .zip(copy.graphemes)
            {
                if owner == Some(serial) {
                    self.pages.pages[index].styles.retain(id);
                    self.pages.pages[index].links.retain_moved_cell(link_id);
                    if let Some(grapheme) = cell.grapheme {
                        assert!(transferred.insert((serial, grapheme)));
                    }
                    graphemes.push(None);
                } else {
                    cell.style_id = 0;
                    cell.link_id = 0;
                    graphemes.push(
                        cell.grapheme
                            .take()
                            .map(|grapheme| (grapheme.len, text.unwrap())),
                    );
                }
            }
            displaced.push(std::mem::replace(&mut self.rows[y], copy.row));
            pending.push((y, serial, copy.sources, graphemes));
        }
        for mut row in displaced.into_iter().chain(discarded) {
            if let Some(owner) = row.resource_page {
                for cell in &mut row.cells {
                    if cell
                        .grapheme
                        .is_some_and(|allocation| transferred.remove(&(owner, allocation)))
                    {
                        cell.grapheme = None;
                    }
                }
            }
            self.release_row_resources(&row);
        }
        assert!(
            transferred.is_empty(),
            "copied grapheme still has a source owner"
        );
        for (y, serial, sources, graphemes) in pending {
            for (column, ((owner, source_id, style, link_id), grapheme)) in
                sources.into_iter().zip(graphemes).enumerate()
            {
                if let Some((len, text)) = grapheme {
                    let allocation = self
                        .acquire_grapheme(self.history.len() + y, len)
                        .expect("copied grapheme must fit after page growth");
                    let index = self.pages.page_index(self.history.len() + y);
                    self.pages.pages[index].graphemes.set_text(allocation, text);
                    self.rows[y].cells[column].grapheme = Some(allocation);
                }
                if owner != Some(serial)
                    && let Some(link) = Hyperlink::from_cell(&self.rows[y].cells[column])
                {
                    let id = self
                        .acquire_link_cell(self.history.len() + y, &link, link_id)
                        .unwrap_or(0);
                    self.rows[y].cells[column].link_id = id;
                    if id == 0 {
                        self.rows[y].cells[column].clear_link();
                    }
                }
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
        if source_serial == destination_serial {
            // Native margin scrolling moves cells within a page. Duplicating
            // their suffixes would needlessly exhaust a full grapheme map.
            self.edit_row(destination, |row| {
                row.cells[start..end].fill(Cell::default())
            });
            for col in start..end {
                self.rows[destination].cells[col] =
                    std::mem::take(&mut self.rows[source].cells[col]);
            }
            self.rows[source].dirty = true;
            self.edit_row(destination, |row| row.repair_wide(background));
            return;
        }
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
                let text = cell.grapheme.map(|allocation| {
                    self.pages.pages[source_index]
                        .graphemes
                        .text_arc(allocation)
                });
                (cell, style, text)
            })
            .collect();
        self.edit_row(destination, |row| {
            row.cells[start..end].fill(Cell::default())
        });
        for (offset, (mut cell, style, text)) in cells.into_iter().enumerate() {
            let source_id = cell.style_id;
            let source_link = std::mem::take(&mut cell.link_id);
            let grapheme = cell.grapheme.take();
            cell.style_id = 0;
            self.rows[destination].cells[start + offset] = cell;
            if let Some(grapheme) = grapheme {
                let allocation = self
                    .acquire_grapheme(self.history.len() + destination, grapheme.len)
                    .expect("copied grapheme must fit after page growth");
                let index = self.pages.page_index(self.history.len() + destination);
                self.pages.pages[index]
                    .graphemes
                    .set_text(allocation, text.unwrap());
                self.rows[destination].cells[start + offset].grapheme = Some(allocation);
            }
            if let Some(link) = Hyperlink::from_cell(&self.rows[destination].cells[start + offset])
            {
                let id = self
                    .acquire_link_cell(self.history.len() + destination, &link, source_link)
                    .unwrap_or(0);
                self.rows[destination].cells[start + offset].link_id = id;
                if id == 0 {
                    self.rows[destination].cells[start + offset].clear_link();
                }
            }
            if source_id != 0 {
                let id = self
                    .acquire_style(self.history.len() + destination, style, Some(source_id))
                    .unwrap_or(0);
                self.rows[destination].cells[start + offset].style_id = id;
                if id == 0 {
                    self.rows[destination].cells[start + offset].style = Style::default();
                }
            }
        }
        self.edit_row(destination, |row| row.repair_wide(background));
    }

    fn resize_owned_columns(
        &mut self,
        contents: &mut [Row],
        columns: usize,
        spacer_heads: &[bool],
    ) {
        let old_pages = self.pages.clone();
        for (absolute, row) in contents.iter_mut().enumerate() {
            let index = old_pages.page_index(absolute);
            let source = &old_pages.pages[index];
            Self::edit_owned_row(&mut self.pages.pages[index], row, |row| {
                row.cells.resize(columns, Cell::default());
                row.repair_wide(Color::Default);
            });
            if columns > usize::from(source.columns)
                && (columns > usize::from(source.capacity.cols) || spacer_heads[index])
            {
                row.wrapped = false;
                row.wrap_continuation = false;
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
            if row.resource_page == Some(serial) {
                continue;
            }
            let previous = row.resource_page;
            let source =
                previous.and_then(|owner| old_pages.pages.iter().find(|page| page.serial == owner));
            let resources: Vec<_> = row
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
                        cell.grapheme.take().map(|allocation| {
                            (
                                allocation,
                                source
                                    .expect("grapheme has an owner")
                                    .graphemes
                                    .text_arc(allocation),
                            )
                        }),
                        std::mem::take(&mut cell.link_id),
                        Hyperlink::from_cell(cell),
                    )
                })
                .collect();
            row.resource_page = Some(serial);
            for (col, (source_id, style, grapheme, source_link, link)) in
                resources.into_iter().enumerate()
            {
                if let Some((grapheme, text)) = &grapheme {
                    let allocation = loop {
                        if let Ok(allocation) = pages.pages[index].graphemes.acquire(grapheme.len) {
                            break allocation;
                        }
                        Self::rebuild_resource_page(
                            &mut pages.pages[index],
                            contents.iter_mut(),
                            Some(PageResource::Graphemes),
                        )
                        .expect("copied page resources must fit after growth");
                    };
                    pages.pages[index]
                        .graphemes
                        .set_text(allocation, text.clone());
                    contents[absolute].cells[col].grapheme = Some(allocation);
                }
                if let Some(link) = link {
                    let id = loop {
                        match pages.pages[index].links.copy_cell(&link, source_link) {
                            Ok(id) => break id,
                            Err(error) => Self::rebuild_resource_page(
                                &mut pages.pages[index],
                                contents.iter_mut(),
                                PageResource::for_link(error),
                            )
                            .expect("copied hyperlinks must fit after page growth"),
                        }
                    };
                    contents[absolute].cells[col].link_id = id;
                }
                if source_id != 0 {
                    let acquired = pages.pages[index].styles.acquire_with_id(style, source_id);
                    let id = match acquired {
                        Ok(id) => id,
                        Err(error) => Self::rebuild_resource_page(
                            &mut pages.pages[index],
                            contents.iter_mut(),
                            (error == SetFull::OutOfMemory).then_some(PageResource::Styles),
                        )
                        .and_then(|()| pages.pages[index].styles.acquire_with_id(style, source_id))
                        .unwrap_or(0),
                    };
                    contents[absolute].cells[col].style_id = id;
                    if id == 0 {
                        contents[absolute].cells[col].style = Style::default();
                    }
                }
                if let Some(owner) = previous
                    && let Some(page) = pages.pages.iter_mut().find(|page| page.serial == owner)
                {
                    page.styles.release(source_id);
                    page.links.release_cell(source_link);
                    if let Some((grapheme, _)) = grapheme {
                        page.graphemes.release(grapheme);
                    }
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
        let range = absolute - page_row..absolute - page_row + usize::from(page.rows);
        let index = self.pages.page_index(absolute);
        self.release_cursor_style();
        if copy_rows
            && self
                .cursor_link
                .is_some_and(|(owner, _)| owner == self.pages.pages[index].serial)
        {
            self.release_cursor_link();
        }
        let old_pages = self.pages.clone();
        let mut contents: Vec<_> = self.history.drain(..).chain(self.rows.drain(..)).collect();
        for row in &mut contents[range.clone()] {
            Self::edit_owned_row(&mut self.pages.pages[index], row, |row| {
                row.cells.resize(columns, Cell::default());
                row.repair_wide(Color::Default);
            });
            if copy_rows {
                // Native page copying into a wider blank row preserves the
                // destination's wrap flags because the copied range is partial.
                row.wrapped = false;
                row.wrap_continuation = false;
            }
        }
        self.pages
            .extend_page(absolute, columns as u16, spacer_head);
        Self::rehome_contents(&mut self.pages, &mut contents, &old_pages);
        self.rows = contents.split_off(history);
        self.history = contents.into();
        if range.start < history {
            self.history_bytes = self.history.iter().map(Row::storage_bytes).sum();
        }
        self.sync_cursor_resources();
        self.enforce_memory_limit();
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

    pub(crate) fn push_history(&mut self, row: Row) {
        if self.limits.bytes == Some(0) {
            self.release_row_resources(&row);
            self.discard_row(row.id);
            return;
        }
        self.retain_history(row);
    }

    // ED22 grows native page storage even when ordinary scrollback is disabled.
    pub(crate) fn retain_history(&mut self, row: Row) {
        if self.memory_limit.is_some() {
            self.history_bytes = self.history_bytes.saturating_add(row.storage_bytes());
        }
        self.history.push_back(row);
        if self.viewport_offset > 0 {
            self.viewport_offset += 1;
        }
        let removed = self
            .pages
            .grow(self.columns as u16, self.rows.len(), self.limits);
        self.discard_history_prefix(removed);
        self.enforce_memory_limit();
    }

    /// Charged bytes in history rows; container spare capacity and graphics are
    /// excluded. Text and hyperlink allocations are charged at their capacity.
    /// Without a host memory cap, this scans the current rows on demand.
    pub fn history_bytes(&self) -> usize {
        if self.memory_limit.is_some() {
            self.history_bytes
        } else {
            self.history.iter().map(Row::storage_bytes).sum()
        }
    }

    pub(crate) fn enforce_memory_limit(&mut self) {
        let Some(limit) = self.memory_limit else {
            return;
        };
        let mut bytes = self.history_bytes;
        let mut removed = 0;
        for row in &self.history {
            if bytes <= limit {
                break;
            }
            bytes = bytes.saturating_sub(row.storage_bytes());
            removed += 1;
        }
        if removed > 0 {
            // Rust rows own their cell and string allocations independently;
            // the native page-size floor must not exempt them from this cap.
            self.pages.remove_prefix(removed);
            self.discard_history_prefix(removed);
        }
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
            self.release_row_resources(&row);
            if self.memory_limit.is_some() {
                self.history_bytes = self.history_bytes.saturating_sub(row.storage_bytes());
            }
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
        let viewport_top = (self.viewport_offset > 0).then(|| self.viewport_top());
        if self.viewport_offset > 0 && self.viewport_offset < self.history.len() {
            self.viewport_pin = viewport_top;
        }
        if self.viewport_pin.is_none() {
            let point = self.all_rows().next().map(|row| GridPoint {
                row: row.id,
                col: 0,
            });
            self.viewport_pin = point;
        }
        let viewport_pinned = viewport_top.is_some() && viewport_top == self.viewport_pin;
        let viewport_at_top = self.viewport_offset > 0 && !viewport_pinned;
        if viewport_top.is_none() {
            self.viewport_pin_column = 0;
        }
        self.release_cursor_style();
        self.release_cursor_link();
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
            // Independent blank rows may be discarded at the end. Keep their
            // identities for anchors, but allocate cells only when retained.
            let mut line = self.blank_row(0, Color::Default);
            let mut x: usize = 0;
            let mut pin_x: usize = 0;
            let mut written_rows = 0;
            // Source pages are immutable during reflow. Compute their resized
            // capacities once and walk their rows without rescanning the list.
            let mut source_rows = source_pages.pages.iter().flat_map(|page| {
                let capacity = page.adjusted_capacity(cols as u16, true);
                std::iter::repeat_n((page, capacity), usize::from(page.rows))
            });
            for old in &contents {
                wanted.clear();
                let (source_page, capacity) = source_rows.next().expect("source row has a page");
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
                // A discarded blank continuation does not end the reflowed
                // line. Native defers a hard break only for independent rows.
                if used == 0 && old.wrap_continuation {
                    continue;
                }
                // Graphics anchors need coordinates but do not retain blank
                // cells. Index them once instead of scanning every placement
                // for every source row.
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
                // Source columns are visited in order. Only actual consumers
                // need a map entry; ordinary untracked cells never hash.
                let mut record = |old_col, point| {
                    if wanted.get(next_point) == Some(&old_col) {
                        map.insert((old.id, old_col), point);
                        next_point += 1;
                    }
                };
                if used > 0 {
                    if line.cells.is_empty() {
                        line.cells = vec![Cell::default(); cols];
                    }
                    while self.pages.total_rows() <= output.len() {
                        self.pages.reflow_row(capacity);
                    }
                }
                line.semantic = old.semantic;
                let mut wide_tail = None;
                for (old_col, cell) in old.cells.iter().take(used).enumerate() {
                    if cell.width == 0 {
                        record(
                            old_col,
                            wide_tail.unwrap_or(GridPoint {
                                row: line.id,
                                col: x.saturating_sub(1),
                            }),
                        );
                        continue;
                    }
                    if cell.spacer_head {
                        record(
                            old_col,
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
                    record(
                        old_col,
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
                    let source_id = cell.style_id;
                    let source_link = cell.link_id;
                    let grapheme = cell.grapheme;
                    let mut cell = cell.clone();
                    cell.style_id = 0;
                    cell.link_id = 0;
                    cell.grapheme = None;
                    if cols == 1 && cell.width == 2 {
                        cell.codepoint = None;
                    }
                    cell.width = width as u8;
                    line.cells[x] = cell;
                    let index = self.pages.page_index(output.len());
                    line.resource_page = Some(self.pages.pages[index].serial);
                    if let Some(grapheme) = grapheme.filter(|_| line.cells[x].codepoint.is_some()) {
                        let allocation = loop {
                            if let Ok(allocation) =
                                self.pages.pages[index].graphemes.acquire(grapheme.len)
                            {
                                break allocation;
                            }
                            Self::rebuild_resource_page(
                                &mut self.pages.pages[index],
                                output.iter_mut().chain(std::iter::once(&mut line)),
                                Some(PageResource::Graphemes),
                            )
                            .expect("reflow graphemes must fit after page growth");
                        };
                        self.pages.pages[index]
                            .graphemes
                            .set_text(allocation, source_page.graphemes.text_arc(grapheme));
                        line.cells[x].grapheme = Some(allocation);
                    }
                    self.reflow_link(&mut output, &mut line, x, source_link);
                    let native_id = self
                        .reflow_style(&mut output, &mut line, source_style, source_id)
                        .unwrap_or(0);
                    let cell = &mut line.cells[x];
                    cell.style_id = native_id;
                    if native_id == 0 && source_style != Style::default() {
                        cell.style = Style::default();
                    }
                    if width == 2 {
                        let index = self.pages.page_index(output.len());
                        self.pages.pages[index].styles.retain(cell.style_id);
                        let mut cell = cell.clone();
                        cell.codepoint = None;
                        cell.grapheme = None;
                        cell.link_id = 0;
                        cell.width = 0;
                        line.cells[x + 1] = cell;
                        self.reflow_link(&mut output, &mut line, x + 1, source_link);
                    }
                    wide_tail = Some(GridPoint {
                        row: line.id,
                        col: (x + 1).min(cols - 1),
                    });
                    x += width;
                }
                // Map only requested trailing blanks, including graphics
                // anchors that intentionally did not extend the copied text.
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
                    output.push(line);
                    line = self.blank_row(0, Color::Default);
                    x = 0;
                }
            }
            if output.len() < written_rows {
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
            // Drop only deferred blank rows. Blanks copied from wrapped source
            // rows, cursor positions or other pins are meaningful output.
            while output.len() > written_rows {
                let row = output.pop().unwrap();
                self.release_row_resources(&row);
            }
            for row in &mut output {
                if row.cells.is_empty() {
                    row.cells = vec![Cell::default(); cols];
                }
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
            self.resize_owned_columns(&mut contents, cols, &spacer_heads);
            mapped_cursor.col = mapped_cursor.col.min(cols - 1);
            if let Some(point) = &mut self.viewport_pin {
                point.col = point.col.min(cols - 1);
            }
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
        self.enforce_memory_limit();
        self.viewport_offset = self.viewport_offset.min(self.history.len());
        if viewport_pinned && let Some(point) = self.viewport_pin {
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
        } else if viewport_at_top {
            self.viewport_offset = self.history.len();
            self.viewport_pin_column = 0;
        }
        // Native resize reattaches the cursor hyperlink to its new page,
        // assigning a new implicit identity while printed links keep theirs.
        self.renew_cursor_implicit_link();
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
                    && !self.viewport_pin.is_some_and(|p| p.row == r.id)
                    && !self
                        .tracked
                        .0
                        .values()
                        .any(|p| p.is_some_and(|p| p.row == r.id))
                    && r.cells.iter().all(|c| c.codepoint.is_none())
            })
        {
            let row = contents.pop().unwrap();
            self.release_row_resources(&row);
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
            self.release_row_resources(&row);
            self.discard_row(row.id);
        }
        if self.viewport_pin.is_none() {
            self.viewport_pin = contents.first().map(|row| GridPoint {
                row: row.id,
                col: 0,
            });
        }
    }
}

type GridPointKey = (u64, usize);

#[cfg(test)]
mod text_tests {
    use super::{CellText, CellTextStorage};

    #[test]
    fn inline_text_matches_utf8_for_every_scalar() {
        let empty = CellText::scalar(None);
        assert_eq!(empty.as_str(), "");
        assert_eq!(empty.chars().next(), None);
        for cp in (0..=0x10ffff).filter_map(char::from_u32) {
            let text = CellText::scalar(Some(cp));
            assert_eq!(text.as_str(), cp.encode_utf8(&mut [0; 4]));
            assert_eq!(text.chars().next(), Some(cp));
            assert_eq!(text.chars().next_back(), Some(cp));
            assert_eq!(text.chars().count(), 1);
        }
        for value in ["", "a\u{301}", "👩\u{200d}💻"] {
            let text = CellText(CellTextStorage::Grapheme(value));
            assert_eq!(text.as_str(), value);
            assert!(text.chars().eq(value.chars()));
            assert!(text.chars().rev().eq(value.chars().rev()));
        }
    }
}

#[cfg(test)]
mod resource_tests {
    use super::*;
    use crate::Terminal;
    use crate::page_layout::PageCapacity;

    #[test]
    fn shared_hyperlink_storage_is_charged_once_per_row_by_allocation() {
        let mut row = Row::new(0, 80, Color::Default);
        let empty = row.storage_bytes();
        let link = Arc::new(HyperlinkData::new(
            b"https://example.org/\xff",
            Some(HyperlinkId::Explicit(b"shared".to_vec())),
        ));
        row.cells[0].hyperlink = Some(link.clone());
        let one_link = row.storage_bytes();
        assert!(one_link > empty + size_of::<HyperlinkData>());
        for cell in &mut row.cells {
            cell.hyperlink = Some(link.clone());
        }
        assert_eq!(row.storage_bytes(), one_link);
        let snapshot = row.clone();
        drop(link);
        assert_eq!(row.storage_bytes(), one_link);
        drop(snapshot);
        assert_eq!(row.storage_bytes(), one_link);

        // Equal values in independent allocations must both be charged.
        row.cells[1].hyperlink = Some(Arc::new(HyperlinkData::new(
            b"https://example.org/\xff",
            Some(HyperlinkId::Explicit(b"shared".to_vec())),
        )));
        assert_eq!(row.storage_bytes() - one_link, one_link - empty);
    }

    #[test]
    fn uncapped_history_accounting_survives_eviction_and_policy_changes() {
        let mut terminal = Terminal::new(80, 2, 100);
        terminal.feed(b"\x1b[?2027h");
        let input = "\x1b]8;id=shared;https://example.org\x1b\\a\u{301}👩‍💻\x1b]8;;\x1b\\\r\n";
        for limit in [None, Some(16_384), None, Some(8_192)] {
            terminal.set_scrollback_memory_limit(limit);
            terminal.feed(input.repeat(300).as_bytes());
            let screen = terminal.screen();
            let actual: usize = screen.history.iter().map(Row::storage_bytes).sum();
            assert!(actual > 0);
            assert_eq!(screen.history_bytes(), actual);
            assert!(limit.is_none_or(|limit| actual <= limit));
            let json = serde_json::to_value(screen).unwrap();
            assert_eq!(json["history_bytes"], actual);
            let restored: Screen = serde_json::from_value(json).unwrap();
            assert_eq!(restored.history.len(), screen.history.len());
            assert_eq!(
                restored.history_bytes(),
                restored
                    .history
                    .iter()
                    .map(Row::storage_bytes)
                    .sum::<usize>()
            );
            assert_references(screen);
        }
    }

    #[test]
    fn deserialized_history_recounts_payloads_that_no_longer_share_storage() {
        let mut terminal = Terminal::new(80, 2, 1000);
        terminal.feed(b"\x1b]8;id=shared;");
        terminal.feed(&vec![b'x'; 1024]);
        terminal.feed(b"\x07");
        terminal.feed(&[b'a'; 79]);
        terminal.feed(b"\x1b]8;;\x07\r\n\r\n");
        let original = terminal.screen().history_bytes();
        let mut json = serde_json::to_value(terminal.screen()).unwrap();
        json["history_bytes"] = serde_json::json!(0);
        let restored: Screen = serde_json::from_value(json).unwrap();
        let actual: usize = restored.history.iter().map(Row::storage_bytes).sum();
        assert!(actual > 5 * original);
        assert_eq!(restored.history_bytes(), actual);
        *terminal.screen_mut() = restored;
        // Reapplying host policy also repairs externally replaced/stale rows.
        terminal.screen_mut().history_bytes = 0;
        terminal.set_scrollback_memory_limit(Some(original));
        assert!(terminal.screen().history.is_empty());
        assert_eq!(terminal.screen().history_bytes(), 0);
    }

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
            row.resource_page = Some(original.serial);
        }
        for (cell, style) in rows[0].cells.iter_mut().zip(colliding) {
            cell.style = style;
            cell.style_id = original.styles.acquire(style).unwrap();
        }
        rows[1].cells[0].style = ordinary;
        rows[1].cells[0].style_id = ordinary_id;
        rows[0].cells[0].codepoint = Some('a');
        let grapheme = original.graphemes.acquire(5).unwrap();
        original
            .graphemes
            .set_text(grapheme, Arc::from("a\u{301}\u{302}\u{303}\u{304}\u{305}"));
        rows[0].cells[0].grapheme = Some(grapheme);
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
                Screen::rebuild_resource_page(
                    &mut page,
                    copied_rows.iter_mut(),
                    grow.then_some(PageResource::Styles)
                ),
                Err(SetFull::OutOfMemory)
            );
            assert_eq!(page.capacity, capacity);
            assert_eq!(copied_rows[0].cells[0].grapheme, Some(grapheme));
            page.graphemes.assert_allocations(std::iter::once(grapheme));
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
            assert_eq!(viewport.row_text(row), source.row_text(original));
            assert_eq!(row.resource_page, original.resource_page);
            for (cell, original) in row.cells.iter().zip(&original.cells) {
                assert_eq!(cell.style, original.style);
            }
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
        let mut graphemes: HashMap<u64, Vec<GraphemeAllocation>> = HashMap::new();
        for (absolute, row) in screen.all_rows().enumerate() {
            let serial = screen.pages.page_at(absolute).0.serial;
            for (col, cell) in row.cells.iter().enumerate() {
                assert_eq!(
                    screen.cell_text(row, col).chars().count().saturating_sub(1),
                    cell.grapheme
                        .map_or(0, |allocation| usize::from(allocation.len))
                );
                if let Some(allocation) = cell.grapheme {
                    assert_eq!(row.resource_page, Some(serial));
                    graphemes.entry(serial).or_default().push(allocation);
                }
                if cell.style_id != 0 {
                    assert_eq!(row.resource_page, Some(serial));
                    *expected.entry((serial, cell.style_id)).or_default() += 1;
                }
                if let Some(link) = Hyperlink::from_cell(cell) {
                    assert_eq!(row.resource_page, Some(serial));
                    assert_eq!(
                        screen.pages.page_at(absolute).0.links.get(cell.link_id),
                        &link
                    );
                } else {
                    assert_eq!(cell.link_id, 0);
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
            page.links.assert_references(
                screen
                    .all_rows()
                    .filter(|row| row.resource_page == Some(page.serial))
                    .flat_map(|row| row.cells.iter().map(|cell| cell.link_id)),
                screen
                    .cursor_link
                    .filter(|(serial, _)| *serial == page.serial)
                    .map(|(_, id)| id),
            );
            page.graphemes.assert_allocations(
                graphemes
                    .remove(&page.serial)
                    .unwrap_or_default()
                    .into_iter(),
            );
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
        assert!(
            graphemes.is_empty(),
            "cells referenced a retired grapheme page"
        );
    }

    #[test]
    fn printing_replaces_cells_without_leaking_page_resources() {
        let mut terminal = Terminal::new(8, 3, 0);
        terminal.feed(b"\x1b[31m\x1b]8;id=shared;https://example.org\x1b\\");
        terminal.screen_mut().cursor.protected = true;
        terminal.screen_mut().cursor.semantic = SemanticContent::Input;
        terminal.feed("界\u{301}界\u{301}".as_bytes());
        let snapshot = terminal.screen().snapshot_viewport();
        let style_id = terminal.screen().rows[0].cells[0].style_id;
        let link_id = terminal.screen().rows[0].cells[0].link_id;
        assert_references(terminal.screen());

        // The style and link stay the same, but the suffix and attributes do not.
        terminal.cursor_position(1, 1);
        terminal.screen_mut().cursor.protected = false;
        terminal.screen_mut().cursor.semantic = SemanticContent::Output;
        terminal.print('語');
        let cell = &terminal.screen().rows[0].cells[0];
        assert_eq!(cell.style_id, style_id);
        assert_eq!(cell.link_id, link_id);
        assert!(cell.grapheme.is_none());
        assert!(!cell.protected);
        assert_eq!(cell.semantic, SemanticContent::Output);
        assert_references(terminal.screen());

        // Overlap the old tail and the next head: both outside halves go blank.
        terminal.cursor_position(1, 2);
        terminal.print('文');
        let row = &terminal.screen().rows[0];
        assert!(row.cells[0].codepoint.is_none());
        assert_eq!(row.cells[1].codepoint, Some('文'));
        assert_eq!(row.cells[1].width, 2);
        assert_eq!(row.cells[2].width, 0);
        assert!(row.cells[3].codepoint.is_none());
        assert_references(terminal.screen());

        // A default, unlinked narrow write clears the old head and its resources.
        terminal.feed(b"\x1b[0m\x1b]8;;\x1b\\\x1b[1;3HX");
        let row = &terminal.screen().rows[0];
        assert!(row.cells[1].codepoint.is_none());
        assert_eq!(row.cells[2].codepoint, Some('X'));
        assert_eq!(row.cells[2].style, Style::default());
        assert!(row.cells.iter().all(|cell| cell.link_id == 0));
        assert_references(terminal.screen());

        terminal.feed("\x1b[2;8H界".as_bytes());
        assert!(terminal.screen().rows[1].cells[7].spacer_head);
        terminal.feed(b"\x1b[3;1HX");
        assert!(!terminal.screen().rows[1].cells[7].spacer_head);
        assert_eq!(terminal.screen().rows[2].cells[1].width, 1);
        assert_references(terminal.screen());
        assert_eq!(snapshot.row_text(&snapshot.rows[0]), "界\u{301}界\u{301}");
        assert!(snapshot.rows[0].cells[0].protected);
        assert_eq!(snapshot.rows[0].cells[0].semantic, SemanticContent::Input);
        assert!(snapshot.rows[0].cells[0].hyperlink.is_some());

        // Derive the page boundary from the native layout, without ABI constants.
        let boundary = usize::from(PageCapacity::initial(128).unwrap().rows);
        let mut terminal = Terminal::new(128, (boundary + 1) as u16, 0);
        terminal.cursor_position(boundary, 1);
        terminal.feed("\x1b[31m\x1b]8;;https://example.org\x1b\\a\u{301}".as_bytes());
        let snapshot = terminal.screen().snapshot_viewport();
        let original_link = terminal.screen().cursor.hyperlink.clone().unwrap();
        let original_owner = terminal.screen().cursor_style.unwrap().0;
        terminal.cursor_position(boundary + 1, 1);
        terminal.feed("b\u{301}".as_bytes());
        assert_ne!(terminal.screen().cursor_style.unwrap().0, original_owner);
        assert_ne!(
            terminal.screen().cursor.hyperlink.as_ref().unwrap().id,
            original_link.id
        );
        assert_references(terminal.screen());
        terminal.cursor_position(boundary, 1);
        terminal.print('C');
        assert_references(terminal.screen());
        assert_eq!(snapshot.row_text(&snapshot.rows[boundary - 1]), "a\u{301}");
        assert_eq!(
            snapshot.rows[boundary - 1].cells[0]
                .hyperlink
                .as_ref()
                .unwrap()
                .id,
            original_link.id
        );
        let data = crate::snapshot::encode_to_vec(&terminal).unwrap();
        let restored = crate::snapshot::decode(data.as_slice(), Default::default()).unwrap();
        assert_references(restored.screen());
    }

    #[test]
    fn printable_runs_replace_resources_and_cross_pages() {
        let boundary = usize::from(PageCapacity::initial(32).unwrap().rows);
        let mut terminal = Terminal::new(32, 8, boundary + 16);
        terminal.feed(b"\x1b[?2027h");
        for _ in 0..boundary + 3 {
            terminal.feed(
                "\x1b[31m\x1b]8;;https://example.org\x1b\\界a\u{301}\x1b]8;;\x1b\\ plain\r\n"
                    .as_bytes(),
            );
        }
        let screen = terminal.screen();
        assert_ne!(
            screen.pages.page_at(screen.history.len()).0.serial,
            screen
                .pages
                .page_at(screen.history.len() + screen.rows.len() - 1)
                .0
                .serial,
        );
        assert_references(terminal.screen());
        for style in [b"\x1b[32m".as_slice(), b"\x1b[0m", b"\x1b[1;34m"] {
            terminal.feed(b"\x1b[H");
            terminal.feed(style);
            terminal.feed("x界".repeat(88).as_bytes());
            assert_references(terminal.screen());
        }
        let data = crate::snapshot::encode_to_vec(&terminal).unwrap();
        let restored = crate::snapshot::decode(data.as_slice(), Default::default()).unwrap();
        assert_references(restored.screen());
    }

    #[test]
    fn rehoming_inline_rows_preserves_attributes_and_adopts_untracked_links() {
        let mut terminal = Terminal::new(4, 2, 0);
        let screen = terminal.screen_mut();
        screen.rows[0].cells[0] = Cell {
            codepoint: Some('X'),
            style: Style {
                background: Color::Indexed(4),
                ..Style::default()
            },
            protected: true,
            semantic: SemanticContent::Input,
            ..Cell::default()
        };
        assert!(screen.rows[0].resource_page.is_none());
        screen.sync_resource_row(0);
        let cell = &screen.rows[0].cells[0];
        assert_eq!(cell.style_id, 0);
        assert_eq!(cell.codepoint, Some('X'));
        assert_eq!(cell.style.background, Color::Indexed(4));
        assert!(cell.protected);
        assert_eq!(cell.semantic, SemanticContent::Input);
        assert_eq!(
            screen.rows[0].resource_page,
            Some(screen.pages.page_at(0).0.serial)
        );
        assert_references(screen);

        // Public cells may carry a hyperlink payload without a native handle.
        let link = Arc::new(HyperlinkData::new(
            b"https://example.org",
            Some(HyperlinkId::Explicit(b"public".to_vec())),
        ));
        screen.rows[1].cells[0].hyperlink = Some(link.clone());
        assert_eq!(screen.rows[1].cells[0].link_id, 0);
        screen.sync_resource_row(1);
        assert_ne!(screen.rows[1].cells[0].link_id, 0);
        assert!(Arc::ptr_eq(
            screen.rows[1].cells[0].hyperlink.as_ref().unwrap(),
            &link
        ));
        assert_references(screen);
    }

    #[test]
    fn scrolling_adopts_unowned_public_links_before_skipping_resource_moves() {
        let boundary = PageCapacity::initial(128).unwrap().rows;
        let mut terminal = Terminal::with_limits(128, boundary + 1, ScrollbackLimits::default());
        let link = Arc::new(HyperlinkData::new(
            b"https://example.org",
            Some(HyperlinkId::Explicit(b"public".to_vec())),
        ));
        let row = &mut terminal.screen_mut().rows[1];
        assert!(row.resource_page.is_none());
        row.cells[0].codepoint = Some('P');
        row.cells[0].hyperlink = Some(link.clone());
        terminal.feed(format!("\x1b[{};1H\n", boundary + 1).as_bytes());
        let cell = &terminal.screen().rows[0].cells[0];
        assert_eq!(cell.codepoint, Some('P'));
        assert_ne!(cell.link_id, 0);
        assert!(Arc::ptr_eq(cell.hyperlink.as_ref().unwrap(), &link));
        assert_references(terminal.screen());

        terminal.feed(&vec![b'\n'; usize::from(boundary)]);
        assert_references(terminal.screen());
        let encoded = crate::snapshot::encode_to_vec(&terminal).unwrap();
        let restored = crate::snapshot::decode(encoded.as_slice(), Default::default()).unwrap();
        assert_references(restored.screen());
    }

    #[test]
    fn fresh_scroll_rows_preserve_background_and_resource_references() {
        for (command, blank) in [
            ("\x1b[48;1H\n", 47),
            ("\x1b[2;48r\x1b[48;1H\x1bD", 47),
            ("\x1b[1;47r\x1b[S", 46),
            ("\x1b[H\x1b[T", 0),
        ] {
            let mut terminal = Terminal::with_limits(1024, 48, ScrollbackLimits::default());
            terminal.feed(b"\x1b[?2027h\x1b[31m\x1b]8;id=shared;https://example.org\x1b\\");
            for row in 1..=48 {
                terminal.feed(format!("\x1b[{row};1Ha\u{301}界").as_bytes());
            }
            terminal.feed(b"\x1b[44m");
            terminal.feed(command.as_bytes());
            let screen = terminal.screen();
            assert!(screen.rows[blank].cells.iter().all(|cell| {
                cell.codepoint.is_none()
                    && cell.width == 1
                    && cell.style.background == Color::Indexed(4)
                    && cell.style_id == 0
                    && cell.hyperlink.is_none()
            }));
            assert_references(screen);
            let encoded = crate::snapshot::encode_to_vec(&terminal).unwrap();
            let restored = crate::snapshot::decode(encoded.as_slice(), Default::default()).unwrap();
            assert_references(restored.screen());
        }
    }

    #[test]
    fn live_hyperlink_allocations_survive_mutations_and_restore() {
        let mut terminal = Terminal::new(1024, 2, 100);
        terminal.feed(format!("\x1b]8;;https://example.org\x1b\\{}", "a".repeat(102)).as_bytes());
        assert_eq!(
            terminal.screen().pages.pages[0].capacity.hyperlink_bytes,
            192
        );
        terminal.feed(b"b");
        assert_eq!(
            terminal.screen().pages.pages[0].capacity.hyperlink_bytes,
            384
        );
        assert_references(terminal.screen());

        for columns in [80, 1024] {
            let mut terminal = Terminal::with_limits(columns, 4, ScrollbackLimits::default());
            for id in 0..160 {
                terminal.feed(format!("\x1b]8;id={id};https://example.org/{id}\x1b\\\x1b[31ma\u{301}\x1b]8;;\x1b\\").as_bytes());
                assert_references(terminal.screen());
            }
            for sequence in [
                "\x1b[H\x1b[20@",
                "\x1b[20P",
                "\x1b[20X",
                "\x1b[2S",
                "\x1b[2T",
                "\x1b[2;4r\x1b[4;1H\n",
                "\x1b[r\x1b[?69h\x1b[2;79s\x1b[2S",
                "\x1b[?69l\x1b[H\x1b]8;;https://example.org\x1b\\界\u{301}",
            ] {
                terminal.feed(sequence.as_bytes());
                assert_references(terminal.screen());
            }
            for (cols, rows) in [(24, 6), (120, 3), (3, 4), (80, 4)] {
                terminal.resize(cols, rows);
                assert_references(terminal.screen());
            }
            let data = crate::snapshot::encode_to_vec(&terminal).unwrap();
            let mut restored =
                crate::snapshot::decode(data.as_slice(), Default::default()).unwrap();
            assert_references(restored.screen());
            restored.feed(b"\x1b[H\x1b[2Jtext\x1b]8;;\x1b\\");
            assert_references(restored.screen());
            let viewport = restored.screen().snapshot_viewport();
            assert!(
                viewport
                    .all_rows()
                    .all(|row| row.cells.iter().all(|cell| cell.link_id == 0))
            );
            for page in &viewport.pages.pages {
                page.links.assert_references(std::iter::empty(), None);
            }
        }
    }

    #[test]
    fn live_grapheme_allocations_survive_mutations_and_restore() {
        let mut terminal = Terminal::new(1024, 2, 100);
        terminal.feed("a\u{301}".repeat(512).as_bytes());
        assert_eq!(
            terminal.screen().pages.pages[0].capacity.grapheme_bytes,
            8192
        );
        terminal.feed("a\u{301}".as_bytes());
        assert_eq!(
            terminal.screen().pages.pages[0].capacity.grapheme_bytes,
            235520
        );
        assert_references(terminal.screen());

        for columns in [80, 1024] {
            for length in [5, 64] {
                let cluster = format!("a{}", "\u{301}".repeat(length));
                let mut terminal = Terminal::with_limits(columns, 4, ScrollbackLimits::default());
                terminal.feed(format!("\x1b[31m{}", cluster.repeat(513)).as_bytes());
                assert_references(terminal.screen());
                for sequence in [
                    "\x1b[H\x1b[2@",
                    "\x1b[2P",
                    "\x1b[20X",
                    "\x1b[2S",
                    "\x1b[2T",
                    "\x1b[2;4r\x1b[4;1H\n",
                    "\x1b[r\x1b[?69h\x1b[2;79s\x1b[2S",
                    "\x1b[?69l\x1b[H",
                    "\x1b[?2027h\x1b[1;80H☺\u{200d}❤",
                ] {
                    terminal.feed(sequence.as_bytes());
                    assert_references(terminal.screen());
                }
                for (cols, rows) in [(24, 6), (120, 3), (3, 4), (80, 4)] {
                    terminal.resize(cols, rows);
                    assert_references(terminal.screen());
                }
                let data = crate::snapshot::encode_to_vec(&terminal).unwrap();
                let mut restored =
                    crate::snapshot::decode(data.as_slice(), Default::default()).unwrap();
                assert_references(restored.screen());
                restored.feed(format!("\x1b[H\x1b[2J{}", cluster.repeat(513)).as_bytes());
                assert_references(restored.screen());
                let viewport = restored.screen().snapshot_viewport();
                let expected: Vec<_> = restored
                    .screen()
                    .viewport()
                    .map(|row| restored.screen().row_text(row))
                    .collect();
                restored.feed(b"\x1b[H\x1b[2J");
                assert_eq!(
                    viewport
                        .all_rows()
                        .map(|row| viewport.row_text(row))
                        .collect::<Vec<_>>(),
                    expected
                );
                assert_references(restored.screen());
            }
        }
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
