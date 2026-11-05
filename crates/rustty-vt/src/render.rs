//! Borrowed viewport data for text and graphical renderers. No fonts or frame storage.

use crate::screen::CellText;
use crate::{CursorShape, RowView, Screen, Selection};
use std::ops::RangeInclusive;

/// Native view metadata. Cell changes still use row revisions; colors, fonts,
/// focus, blink phase and other host settings belong in the caller's frame key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewKey {
    pub identity: u64,
    pub dimensions: [usize; 2],
    pub viewport_offset: usize,
    pub selection: Option<Selection>,
    pub cursor: CursorAppearance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorAppearance {
    pub x: usize,
    /// Active-screen row; RenderCursor uses viewport coordinates instead.
    pub y: usize,
    pub shape: CursorShape,
    pub visible: bool,
    pub blink: bool,
}

impl ViewKey {
    /// Rows can move or change selection without invalidating unaffected rows.
    /// Compare each RenderRow's key as well as the host's render settings.
    pub fn same_row_layout(&self, other: &Self) -> bool {
        self.identity == other.identity && self.dimensions == other.dimensions
    }
}

/// Reuses the native content revision and adds this row's selection coverage.
/// Compare keys within the same ViewKey row layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowKey {
    pub id: u64,
    revision: u64,
    selection: Option<[usize; 2]>,
}

#[derive(Clone, Debug)]
pub struct RenderRow<'a> {
    pub row: RowView<'a>,
    pub selection: Option<RangeInclusive<usize>>,
}

impl RenderRow<'_> {
    pub fn key(&self) -> RowKey {
        RowKey {
            id: self.row.id,
            revision: self.row.revision(),
            selection: self.selection.as_ref().map(|r| [*r.start(), *r.end()]),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorKey {
    position: [usize; 2],
    shape: CursorShape,
    row: u64,
    revision: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct RenderCursor<'a> {
    pub x: usize,
    pub y: usize,
    pub shape: CursorShape,
    pub blink: bool,
    row: RowView<'a>,
}

impl<'a> RenderCursor<'a> {
    pub fn key(&self) -> CursorKey {
        CursorKey {
            position: [self.x, self.y],
            shape: self.shape,
            row: self.row.id,
            revision: self.row.revision(),
        }
    }

    /// Read only when the cursor key or ViewKey row layout changes. Spacers have no cursor text;
    /// unwritten cells return one space. No allocation or frame capture.
    pub fn text(&self) -> Option<CellText<'a>> {
        let cell = self.row.cells.get(self.x)?;
        if cell.width() == 0 || self.row.cell_span(self.x).start != self.x || cell.spacer_head() {
            return None;
        }
        let text = self.row.text(self.x);
        Some(if text.is_empty() {
            CellText::scalar(Some(' '))
        } else {
            text
        })
    }
}

#[derive(Clone, Copy)]
struct SelectionBounds {
    start: (usize, usize),
    end: (usize, usize),
    rectangular: bool,
}

pub struct RenderView<'a> {
    screen: &'a Screen,
    selection: Option<SelectionBounds>,
}

impl Screen {
    /// Capture cheap native metadata without reading cell text or styles.
    pub fn render_key(&self) -> ViewKey {
        let cursor = &self.cursor;
        ViewKey {
            identity: self.render_identity(),
            dimensions: [self.columns, self.height()],
            viewport_offset: self.viewport_offset,
            selection: self.selection,
            cursor: CursorAppearance {
                x: cursor.col,
                y: cursor.row,
                shape: cursor.shape,
                visible: cursor.visible,
                blink: cursor.blink,
            },
        }
    }

    /// Resolve selection endpoints once, then borrow rows during the normal
    /// viewport walk. Rebuild after screen mutation; no cells are copied.
    pub fn render_view(&self) -> RenderView<'_> {
        let selection = self.selection.and_then(|selection| {
            let (mut start, mut end) = (None, None);
            for (y, row) in self.all_rows().enumerate() {
                if row.id == selection.start.row && selection.start.col < row.cells.len() {
                    start = Some((y, selection.start.col, row));
                }
                if row.id == selection.end.row && selection.end.col < row.cells.len() {
                    end = Some((y, selection.end.col, row));
                }
                if start.is_some() && end.is_some() {
                    break;
                }
            }
            let (a, b) = (start?, end?);
            let (start, end) = if (a.0, a.1) <= (b.0, b.1) {
                (a, b)
            } else {
                (b, a)
            };
            let mut end_position = (end.0, end.1);
            if !selection.rectangular
                && end.2.cells[end.1].spacer_head()
                && end.0 + 1 < self.history_len() + self.height()
            {
                end_position = (end.0 + 1, 0);
            }
            Some(SelectionBounds {
                start: (start.0, start.1),
                end: end_position,
                rectangular: selection.rectangular,
            })
        });
        RenderView {
            screen: self,
            selection,
        }
    }
}

impl<'a> RenderView<'a> {
    pub fn rows(&self) -> impl Iterator<Item = RenderRow<'a>> + '_ {
        let top = self
            .screen
            .history_len()
            .saturating_sub(self.screen.viewport_offset);
        self.screen.viewport().enumerate().map(move |(y, row)| {
            let selection = self.selection.and_then(|s| {
                let y = top + y;
                if y < s.start.0 || y > s.end.0 {
                    return None;
                }
                let last = row.cells.len() - 1;
                let (mut left, mut right) = if s.rectangular {
                    (
                        s.start.1.min(s.end.1).min(last),
                        s.start.1.max(s.end.1).min(last),
                    )
                } else {
                    (
                        if y == s.start.0 {
                            s.start.1.min(last)
                        } else {
                            0
                        },
                        if y == s.end.0 {
                            s.end.1.min(last)
                        } else {
                            last
                        },
                    )
                };
                left = row.cell_span(left).start;
                let span = row.cell_span(right);
                right = (span.start + usize::from(span.width) - 1).min(last);
                Some(left..=right)
            });
            RenderRow { row, selection }
        })
    }

    /// The cursor in viewport coordinates, if visible there. Its text stays lazy.
    pub fn cursor(&self) -> Option<RenderCursor<'a>> {
        let cursor = &self.screen.cursor;
        let y = cursor.row.checked_add(self.screen.viewport_offset)?;
        if !cursor.visible || y >= self.screen.height() || cursor.row >= self.screen.height() {
            return None;
        }
        Some(RenderCursor {
            x: cursor.col,
            y,
            shape: cursor.shape,
            blink: cursor.blink,
            row: self.screen.row(cursor.row),
        })
    }
}
