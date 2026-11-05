//! Viewport-fixed cursor overlays from Kitty's multiple-cursors protocol.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
use std::io::Write;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Color {
    /// The resolved color of the main cursor, including its reverse-video policy.
    #[default]
    Default,
    Special,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    pub(crate) fn write_parameters(self, out: &mut Vec<u8>) {
        match self {
            Self::Default => out.push(b'0'),
            Self::Special => out.push(b'1'),
            Self::Indexed(index) => write!(out, "5:{index}").unwrap(),
            Self::Rgb(r, g, b) => write!(out, "2:{r}:{g}:{b}").unwrap(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Colors {
    pub cursor: Color,
    pub text: Color,
}

/// Shape bytes are 0 (absent), 1 (block), 2 (bar), 3 (underline), or 29 (main shape).
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    pub shapes: &'a [u8],
    pub revision: u64,
}

#[derive(Clone, Debug)]
struct Grid {
    columns: usize,
    shapes: Vec<u8>,
    row_counts: Vec<u16>,
    row_revisions: Vec<u64>,
    count: usize,
}

#[derive(Clone, Debug, Default)]
pub struct State {
    grid: Option<Arc<Grid>>,
    colors: Colors,
    revision: u64,
}

fn revision() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("cursor overlay revisions exhausted")
}

fn valid_shape(shape: u8) -> bool {
    matches!(shape, 0..=3 | 29)
}

impl State {
    pub fn count(&self) -> usize {
        self.grid.as_ref().map_or(0, |grid| grid.count)
    }
    pub fn colors(&self) -> Colors {
        self.colors
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn row(&self, y: usize) -> Option<Row<'_>> {
        let grid = self.grid.as_ref()?;
        if grid.row_counts.get(y).copied().unwrap_or(0) == 0 {
            return None;
        }
        Some(Row {
            shapes: &grid.shapes[y * grid.columns..(y + 1) * grid.columns],
            revision: grid.row_revisions[y],
        })
    }
    pub(crate) fn is_default(&self) -> bool {
        self.count() == 0 && self.colors == Colors::default()
    }
    pub(crate) fn shape_bytes(&self) -> &[u8] {
        self.grid.as_ref().map_or(&[], |grid| &grid.shapes)
    }
    pub(crate) fn valid_dimensions(&self, columns: usize, rows: usize) -> bool {
        self.grid
            .as_ref()
            .is_none_or(|grid| grid.columns == columns && grid.row_counts.len() == rows)
    }

    pub(crate) fn from_bytes(
        columns: usize,
        rows: usize,
        colors: Colors,
        shapes: Vec<u8>,
    ) -> Result<Self, &'static str> {
        if columns == 0
            || rows == 0
            || columns > usize::from(u16::MAX)
            || rows > usize::from(u16::MAX)
            || !shapes.is_empty() && columns.checked_mul(rows) != Some(shapes.len())
            || shapes.iter().any(|&shape| !valid_shape(shape))
        {
            return Err("invalid extra cursor grid");
        }
        let count = shapes.iter().filter(|&&shape| shape != 0).count();
        let changed = count != 0 || colors != Colors::default();
        let revision = if changed { revision() } else { 0 };
        let grid = if count == 0 {
            None
        } else {
            let row_counts = shapes
                .chunks_exact(columns)
                .map(|row| row.iter().filter(|&&shape| shape != 0).count() as u16)
                .collect::<Vec<_>>();
            let row_revisions = row_counts
                .iter()
                .map(|&count| if count == 0 { 0 } else { revision })
                .collect();
            Some(Arc::new(Grid {
                columns,
                shapes,
                row_counts,
                row_revisions,
                count,
            }))
        };
        Ok(Self {
            grid,
            colors,
            revision,
        })
    }

    pub(crate) fn set_color(&mut self, text: bool, color: Color) -> bool {
        let target = if text {
            &mut self.colors.text
        } else {
            &mut self.colors.cursor
        };
        if *target == color {
            return false;
        }
        *target = color;
        self.revision = revision();
        true
    }

    pub(crate) fn clear(&mut self, colors: bool) -> bool {
        if self.count() == 0 && (!colors || self.colors == Colors::default()) {
            return false;
        }
        self.grid = None;
        if colors {
            self.colors = Colors::default();
        }
        self.revision = revision();
        true
    }

    pub(crate) fn set_colors(&mut self, colors: Colors) {
        if self.colors != colors {
            self.colors = colors;
            self.revision = revision();
        }
    }

    pub(crate) fn set_point(&mut self, dimensions: [usize; 2], shape: u8, y: u32, x: u32) -> bool {
        if x == 0 || y == 0 || x as usize > dimensions[0] || y as usize > dimensions[1] {
            return false;
        }
        self.set_rectangle(dimensions, shape, [y, x, y, x])
    }

    pub(crate) fn set_rectangle(
        &mut self,
        [columns, rows]: [usize; 2],
        shape: u8,
        [top, left, bottom, right]: [u32; 4],
    ) -> bool {
        if !valid_shape(shape) || shape == 0 && self.grid.is_none() {
            return false;
        }
        let top = top.max(1) as usize;
        let left = left.max(1) as usize;
        let bottom = (bottom as usize).min(rows);
        let right = (right as usize).min(columns);
        if top > bottom || left > right {
            return false;
        }
        if self.grid.is_none() {
            self.grid = Some(Arc::new(Grid {
                columns,
                shapes: vec![0; columns * rows],
                row_counts: vec![0; rows],
                row_revisions: vec![0; rows],
                count: 0,
            }));
        }
        let mut changed = false;
        for y in top - 1..bottom {
            let range = y * columns + left - 1..y * columns + right;
            if self.grid.as_ref().unwrap().shapes[range.clone()]
                .iter()
                .all(|&old| old == shape)
            {
                continue;
            }
            let grid = Arc::make_mut(self.grid.as_mut().unwrap());
            for old in &mut grid.shapes[range] {
                match (*old != 0, shape != 0) {
                    (false, true) => {
                        grid.count += 1;
                        grid.row_counts[y] += 1;
                    }
                    (true, false) => {
                        grid.count -= 1;
                        grid.row_counts[y] -= 1;
                    }
                    _ => {}
                }
                *old = shape;
            }
            self.revision = revision();
            grid.row_revisions[y] = self.revision;
            changed = true;
        }
        if self.count() == 0 {
            self.grid = None;
        }
        changed
    }

    pub(crate) fn resize(&mut self, columns: usize, rows: usize) {
        let Some(old) = self.grid.as_ref() else {
            return;
        };
        if old.columns == columns && old.row_counts.len() == rows {
            return;
        }
        let mut shapes = vec![0; columns * rows];
        let width = columns.min(old.columns);
        for y in 0..rows.min(old.row_counts.len()) {
            shapes[y * columns..y * columns + width]
                .copy_from_slice(&old.shapes[y * old.columns..y * old.columns + width]);
        }
        *self = Self::from_bytes(columns, rows, self.colors, shapes)
            .expect("valid resized cursor grid");
        if self.revision == 0 {
            self.revision = revision();
        }
    }

    pub(crate) fn positions_reply(&self) -> Vec<u8> {
        let mut out = b"\x1b[>100".to_vec();
        self.runs(|shape, y, left, right| {
            if left == right {
                write!(out, ";{shape}:2:{}:{}", y + 1, left + 1).unwrap();
            } else {
                write!(
                    out,
                    ";{shape}:4:{}:{}:{}:{}",
                    y + 1,
                    left + 1,
                    y + 1,
                    right + 1
                )
                .unwrap();
            }
        });
        out.extend_from_slice(b" q");
        out
    }

    pub(crate) fn colors_reply(&self) -> Vec<u8> {
        let mut out = b"\x1b[>101;30:".to_vec();
        self.colors.text.write_parameters(&mut out);
        out.extend_from_slice(b";40:");
        self.colors.cursor.write_parameters(&mut out);
        out.extend_from_slice(b" q");
        out
    }

    /// Each replay command stays within the ordinary parser's parameter limit.
    pub(crate) fn write_replay(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[>0;4 q\x1b[>30;");
        self.colors.text.write_parameters(out);
        out.extend_from_slice(b" q\x1b[>40;");
        self.colors.cursor.write_parameters(out);
        out.extend_from_slice(b" q");
        self.runs(|shape, y, left, right| {
            if left == right {
                write!(out, "\x1b[>{shape};2:{}:{} q", y + 1, left + 1).unwrap();
            } else {
                write!(
                    out,
                    "\x1b[>{shape};4:{}:{}:{}:{} q",
                    y + 1,
                    left + 1,
                    y + 1,
                    right + 1
                )
                .unwrap();
            }
        });
    }

    fn runs(&self, mut visit: impl FnMut(u8, usize, usize, usize)) {
        let Some(grid) = &self.grid else {
            return;
        };
        for y in 0..grid.row_counts.len() {
            let Some(row) = self.row(y) else {
                continue;
            };
            let mut start = 0;
            while start < row.shapes.len() {
                let shape = row.shapes[start];
                let mut end = start + 1;
                while end < row.shapes.len() && row.shapes[end] == shape {
                    end += 1;
                }
                if shape != 0 {
                    visit(shape, y, start, end - 1);
                }
                start = end;
            }
        }
    }
}

impl Serialize for State {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            colors: Colors,
            columns: usize,
            shapes: &'a [u8],
        }
        Wire {
            colors: self.colors,
            columns: self.grid.as_ref().map_or(0, |grid| grid.columns),
            shapes: self.shape_bytes(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for State {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            colors: Colors,
            columns: usize,
            shapes: Vec<u8>,
        }
        let wire = Wire::deserialize(deserializer)?;
        if wire.shapes.is_empty() {
            return Self::from_bytes(1, 1, wire.colors, Vec::new()).map_err(D::Error::custom);
        }
        if wire.columns == 0 || wire.shapes.len() % wire.columns != 0 {
            return Err(D::Error::custom("invalid extra cursor dimensions"));
        }
        Self::from_bytes(
            wire.columns,
            wire.shapes.len() / wire.columns,
            wire.colors,
            wire.shapes,
        )
        .map_err(D::Error::custom)
    }
}
