//! Kitty Unicode placeholder decoding, shared with renderers.
use crate::{Row, Screen, screen::Color as TerminalColor};

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

/// Pixel rectangles relative to the run's terminal cell, rounded like native
/// Kitty placeholders. An empty fragment has zero source and destination sizes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Geometry {
    pub offset: [u32; 2],
    pub source: [u32; 4],
    pub pixels: [u32; 2],
}

impl Placement {
    /// Fit a run within its resolved `placeholder_target` while preserving
    /// aspect ratio. Invalid cell/image dimensions or an oversized grid return
    /// None; a fragment outside the image returns an empty geometry.
    pub fn geometry(
        self,
        target: &super::Placement,
        image: &super::Image,
        cell: [u32; 2],
    ) -> Option<Geometry> {
        if cell.contains(&0) || image.width == 0 || image.height == 0 {
            return None;
        }
        let grid = [
            if target.columns == 0 {
                image.width.div_ceil(cell[0])
            } else {
                target.columns
            },
            if target.rows == 0 {
                image.height.div_ceil(cell[1])
            } else {
                target.rows
            },
        ];
        if grid.iter().any(|&size| size > u32::from(u16::MAX)) {
            return None;
        }
        let grid_pixels = [
            f64::from(grid[0].checked_mul(cell[0])?),
            f64::from(grid[1].checked_mul(cell[1])?),
        ];
        let dimensions = [f64::from(image.width), f64::from(image.height)];
        let (scale, padding) = if dimensions[0] * grid_pixels[1] > dimensions[1] * grid_pixels[0] {
            let scale = grid_pixels[0] / dimensions[0];
            (
                scale,
                [0.0, (grid_pixels[1] - dimensions[1] * scale) / 2.0 / scale],
            )
        } else {
            let scale = grid_pixels[1] / dimensions[1];
            (
                scale,
                [(grid_pixels[0] - dimensions[0] * scale) / 2.0 / scale, 0.0],
            )
        };
        let scaled = [
            dimensions[0] + padding[0] * 2.0,
            dimensions[1] + padding[1] * 2.0,
        ];
        let mut source = [
            scaled[0] * (f64::from(self.image_col) / f64::from(grid[0])),
            scaled[1] * (f64::from(self.image_row) / f64::from(grid[1])),
            scaled[0] * (f64::from(self.width) / f64::from(grid[0])),
            scaled[1] * (1.0 / f64::from(grid[1])),
        ];
        let mut pixels = [
            f64::from(self.width.checked_mul(cell[0])?),
            f64::from(cell[1]),
        ];
        let mut offset = [0.0; 2];
        for axis in 0..2 {
            if source[axis] < padding[axis] {
                let inset = padding[axis] - source[axis];
                source[axis + 2] -= inset;
                offset[axis] = inset * scale;
                pixels[axis] -= inset * scale;
                source[axis] = 0.0;
                if source[axis + 2] > dimensions[axis] {
                    source[axis + 2] = dimensions[axis];
                    pixels[axis] = dimensions[axis] * scale;
                }
            } else if source[axis] + source[axis + 2] > scaled[axis] - padding[axis] {
                source[axis] -= padding[axis];
                source[axis + 2] = scaled[axis] - padding[axis] - source[axis];
                source[axis + 2] -= padding[axis];
                pixels[axis] = source[axis + 2] * scale;
            } else {
                source[axis] -= padding[axis];
            }
        }
        if source[2] <= 0.0 || source[3] <= 0.0 {
            return Some(Geometry::default());
        }
        Some(Geometry {
            offset: offset.map(|value| value.round() as u32),
            source: source.map(|value| value.round() as u32),
            pixels: pixels.map(|value| value.round() as u32),
        })
    }
}

/// Adjacent cells inherit omitted indices only while their IDs remain compatible.
pub fn placements<'a>(screen: &'a Screen, row: &'a Row) -> impl Iterator<Item = Placement> + 'a {
    let mut cells = row.cells.iter().enumerate().peekable();
    std::iter::from_fn(move || {
        let (col, mut current) = loop {
            let (col, _) = cells.next()?;
            if let Some(placeholder) = Placeholder::from_cell(screen, row, col) {
                break (col, placeholder);
            }
        };
        current.row.get_or_insert(0);
        current.col.get_or_insert(0);
        let mut width = 1;
        while let Some((next_col, _)) = cells.peek() {
            let Some(next) = Placeholder::from_cell(screen, row, *next_col) else {
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
    fn from_cell(screen: &Screen, row: &Row, col: usize) -> Option<Self> {
        let cell = &row.cells[col];
        if cell.codepoint != Some(PLACEHOLDER) {
            return None;
        }
        let text = screen.cell_text(row, col);
        let mut chars = text.chars().skip(1);
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
