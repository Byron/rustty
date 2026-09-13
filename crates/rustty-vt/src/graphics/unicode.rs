//! Kitty Unicode placeholder decoding, shared with renderers.
use crate::{
    Row,
    screen::{Cell, Color as TerminalColor},
};

pub const PLACEHOLDER: char = '\u{10eeee}';

/// One contiguous placeholder run within a row of the viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub image_id: u32,
    pub placement_id: u32,
    /// Starting terminal column.
    pub col: usize,
    /// Starting fragment column/row within the image's placement grid.
    pub image_col: u32,
    pub image_row: u32,
    pub width: u32,
}

/// Adjacent cells inherit omitted indices only while their IDs remain compatible.
pub fn placements(row: &Row) -> impl Iterator<Item = Placement> + '_ {
    let mut cells = row.cells.iter().enumerate().peekable();
    std::iter::from_fn(move || {
        let (col, mut current) = loop {
            let (col, cell) = cells.next()?;
            if let Some(placeholder) = Placeholder::from_cell(cell) {
                break (col, placeholder);
            }
        };
        current.row.get_or_insert(0);
        current.col.get_or_insert(0);
        let mut width = 1;
        while let Some((_, cell)) = cells.peek() {
            let Some(next) = Placeholder::from_cell(cell) else {
                break;
            };
            if current.low != next.low
                || current.placement != next.placement
                || next.row.is_some_and(|r| Some(r) != current.row)
                || next.col.is_some_and(|c| c != current.col.unwrap() + width)
                || next.high.is_some_and(|h| Some(h) != current.high)
            {
                break;
            }
            cells.next();
            width += 1;
        }
        Some(Placement {
            image_id: current.low | (u32::from(current.high.unwrap_or(0)) << 24),
            placement_id: current.placement,
            col,
            image_col: current.col.unwrap(),
            image_row: current.row.unwrap(),
            width,
        })
    })
}

#[derive(Clone)]
struct Placeholder {
    low: u32,
    high: Option<u8>,
    placement: u32,
    row: Option<u32>,
    col: Option<u32>,
}
impl Placeholder {
    fn from_cell(cell: &Cell) -> Option<Self> {
        let mut chars = cell.text.chars();
        if chars.next()? != PLACEHOLDER {
            return None;
        }
        let mut next = || {
            chars
                .next()
                .and_then(|cp| DIACRITICS.binary_search(&(cp as u32)).ok())
                .map(|i| i as u32)
        };
        Some(Self {
            low: color_id(cell.style.foreground),
            placement: color_id(cell.style.underline_color),
            row: next(),
            col: next(),
            high: next().and_then(|i| u8::try_from(i).ok()),
        })
    }
}
fn color_id(c: TerminalColor) -> u32 {
    match c {
        TerminalColor::Default => 0,
        TerminalColor::Indexed(i) => u32::from(i),
        TerminalColor::Rgb(r, g, b) => (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b),
    }
}

include!("diacritics.rs");
