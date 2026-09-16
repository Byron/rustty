//! Selection queries over physical rows, including retained history.

use crate::{Cell, GridPoint, Row, Screen, Selection, SemanticContent};

pub const DEFAULT_WORD_BOUNDARIES: &[char] = &[
    '\0', ' ', '\t', '\'', '"', '│', '`', '|', ':', ';', ',', '(', ')', '[', ']', '{', '}', '<',
    '>', '$',
];
pub const DEFAULT_LINE_WHITESPACE: &[char] = &['\0', ' ', '\t'];

/// Physical-grid motions for a selection's logical end, including scrollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Adjustment {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    BeginningOfLine,
    EndOfLine,
}

impl Selection {
    /// Move the logical end without changing the anchor or rectangular mode.
    /// Horizontal motions skip unwritten cells; down skips unwritten rows.
    /// Other motions may land on blank cells or wide-character spacers.
    /// An endpoint outside this screen is left unchanged.
    pub fn adjust(&mut self, screen: &Screen, adjustment: Adjustment) {
        let grid = Grid(screen);
        let Some(original) = grid.position(self.end) else {
            return;
        };
        let last_written = || {
            (0..grid.len())
                .rev()
                .find(|&y| {
                    grid.row(y)
                        .cells
                        .iter()
                        .any(|cell| codepoint(cell).is_some())
                })
                .map(|y| (y, grid.row(y).cells.len() - 1))
                .unwrap_or(original)
        };
        let at_row = |y| (y, original.1.min(grid.row(y).cells.len() - 1));
        let end = match adjustment {
            Adjustment::Left | Adjustment::Right => {
                let mut current = original;
                let mut found = original;
                while let Some(next) = if adjustment == Adjustment::Left {
                    grid.previous(current)
                } else {
                    grid.next(current)
                } {
                    if codepoint(grid.cell(next)).is_some() {
                        found = next;
                        break;
                    }
                    current = next;
                }
                found
            }
            Adjustment::Up => original.0.checked_sub(1).map(at_row).unwrap_or((0, 0)),
            Adjustment::Down => {
                let mut column = original.1;
                let mut found = (original.0, grid.row(original.0).cells.len() - 1);
                for y in original.0 + 1..grid.len() {
                    column = column.min(grid.row(y).cells.len() - 1);
                    if grid
                        .row(y)
                        .cells
                        .iter()
                        .any(|cell| codepoint(cell).is_some())
                    {
                        found = (y, column);
                        break;
                    }
                }
                found
            }
            Adjustment::Home => (0, 0),
            Adjustment::End => last_written(),
            Adjustment::PageUp => original
                .0
                .checked_sub(screen.height())
                .map(at_row)
                .unwrap_or((0, 0)),
            Adjustment::PageDown => original
                .0
                .checked_add(screen.height())
                .filter(|&y| y < grid.len())
                .map(at_row)
                .unwrap_or_else(last_written),
            Adjustment::BeginningOfLine => (original.0, 0),
            Adjustment::EndOfLine => (original.0, grid.row(original.0).cells.len() - 1),
        };
        self.end = GridPoint {
            row: grid.row(end.0).id,
            col: end.1,
        };
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SelectLine<'a> {
    /// `None` includes unwritten cells. An empty set trims only unwritten cells.
    pub whitespace: Option<&'a [char]>,
    /// Stop at any prompt/input/output transition, including within one row.
    pub semantic_prompt_boundary: bool,
}

impl Default for SelectLine<'_> {
    fn default() -> Self {
        Self {
            whitespace: Some(DEFAULT_LINE_WHITESPACE),
            semantic_prompt_boundary: true,
        }
    }
}

impl Screen {
    /// Select the contiguous run with the clicked codepoint's boundary class.
    /// Unwritten cells cannot be selected. These queries leave the active
    /// selection unchanged; callers can apply the returned inclusive endpoints.
    pub fn select_word(&self, point: GridPoint, boundaries: &[char]) -> Option<Selection> {
        let grid = Grid(self);
        grid.word(grid.position(point)?, boundaries)
    }

    /// Select the first word encountered from `start` toward `end`, inclusive.
    /// The returned word itself may extend beyond the two search endpoints.
    pub fn select_word_between(
        &self,
        start: GridPoint,
        end: GridPoint,
        boundaries: &[char],
    ) -> Option<Selection> {
        let grid = Grid(self);
        let mut current = grid.position(start)?;
        let end = grid.position(end)?;
        let forward = current < end;
        loop {
            if let Some(selection) = grid.word(current, boundaries) {
                return Some(selection);
            }
            if current == end {
                return None;
            }
            current = if forward {
                grid.next(current)
            } else {
                grid.previous(current)
            }?;
        }
    }

    /// Select a logical line through soft wraps, with optional semantic bounds
    /// and endpoint whitespace trimming. Returns none for a trimmed empty line.
    pub fn select_line(&self, point: GridPoint, options: SelectLine<'_>) -> Option<Selection> {
        let grid = Grid(self);
        let clicked = grid.position(point)?;
        let semantic = options
            .semantic_prompt_boundary
            .then(|| grid.cell(clicked).semantic());

        let mut start = (clicked.0, 0);
        'start: {
            if let Some(semantic) = semantic {
                for x in (0..=clicked.1).rev() {
                    if grid.row(clicked.0).cells[x].semantic() != semantic {
                        start.1 = x + 1;
                        break 'start;
                    }
                }
            }
            while start.0 > 0 {
                let y = start.0 - 1;
                let row = grid.row(y);
                if !row.wrapped {
                    break;
                }
                if let Some(semantic) = semantic {
                    for x in (0..row.cells.len()).rev() {
                        if row.cells[x].semantic() != semantic {
                            break 'start;
                        }
                        start = (y, x);
                    }
                } else {
                    start = (y, 0);
                }
            }
        }

        let mut y = clicked.0;
        let mut end = 'end: loop {
            let row = grid.row(y);
            let from = if y == clicked.0 { clicked.1 } else { 0 };
            if let Some(semantic) = semantic {
                for x in from..row.cells.len() {
                    if row.cells[x].semantic() != semantic {
                        break 'end grid.previous((y, x))?;
                    }
                }
            }
            if !row.wrapped {
                break (y, row.cells.len() - 1);
            }
            y += 1;
            if y == grid.len() {
                return None;
            }
        };

        if let Some(whitespace) = options.whitespace {
            let start_limit = start.0;
            let end_limit = end.0;
            // Native cellIterator bounds the final row, not the final column.
            // Keep that behavior when semantic boundaries fall within a row.
            while !codepoint(grid.cell(start)).is_some_and(|c| !whitespace.contains(&c)) {
                start = grid.next(start)?;
                if start.0 > end_limit {
                    return None;
                }
            }
            while !codepoint(grid.cell(end)).is_some_and(|c| !whitespace.contains(&c)) {
                end = grid.previous(end)?;
                if end.0 < start_limit {
                    return None;
                }
            }
        }
        Some(grid.selection(start, end))
    }

    /// Select all written content, excluding surrounding NUL, space, and tab.
    pub fn select_all(&self) -> Option<Selection> {
        let written =
            |cell: &Cell| codepoint(cell).is_some_and(|c| !DEFAULT_LINE_WHITESPACE.contains(&c));
        let start = self.all_rows().find_map(|row| {
            row.cells
                .iter()
                .position(written)
                .map(|col| GridPoint { row: row.id, col })
        })?;
        let end = self.all_rows().rev().find_map(|row| {
            row.cells
                .iter()
                .rposition(written)
                .map(|col| GridPoint { row: row.id, col })
        })?;
        Some(Selection {
            start,
            end,
            rectangular: false,
        })
    }

    /// Select command output bounded by shell integration's prompt markers.
    /// Explicit spaces count as output; unwritten trailing cells do not.
    /// Returns none on prompt/input cells or when no prompt bounds the output.
    /// Like the other queries, this leaves the active selection unchanged.
    pub fn select_output(&self, point: GridPoint) -> Option<Selection> {
        let grid = Grid(self);
        let clicked = grid.position(point)?;
        if grid.cell(clicked).semantic() != SemanticContent::Output {
            return None;
        }

        let prompt = (0..=clicked.0)
            .rev()
            .find(|&y| grid.row(y).semantic != SemanticContent::Output);
        let Some(mut prompt) = prompt else {
            let next = (clicked.0..grid.len())
                .find(|&y| grid.row(y).semantic != SemanticContent::Output)?;
            let mut end = grid.previous((next, 0))?;
            while codepoint(grid.cell(end)).is_none() {
                let Some(previous) = grid.previous(end) else {
                    break;
                };
                end = previous;
            }
            // Native includes the origin even when this entire prefix is blank.
            return Some(grid.selection((0, 0), end));
        };

        if grid.row(prompt).semantic == SemanticContent::Input {
            let mut y = prompt;
            while let Some(previous) = y.checked_sub(1) {
                match grid.row(previous).semantic {
                    SemanticContent::Output => {
                        prompt = y;
                        break;
                    }
                    SemanticContent::Prompt => {
                        prompt = previous;
                        break;
                    }
                    SemanticContent::Input => y = previous,
                }
            }
            // A continuation group reaching the retained top keeps the first
            // encountered row, matching native PromptIterator::nextLeftUp.
        }

        let mut after = prompt + 1;
        while after < grid.len() && grid.row(after).semantic == SemanticContent::Input {
            after += 1;
        }
        let limit = (after..grid.len())
            .find(|&y| grid.row(y).semantic != SemanticContent::Output)
            .unwrap_or(grid.len());
        let mut bounds = None;
        'output: for y in prompt..limit {
            for (x, cell) in grid.row(y).cells.iter().enumerate() {
                if cell.semantic() != SemanticContent::Output {
                    if bounds.is_some() {
                        break 'output;
                    }
                } else if codepoint(cell).is_some() {
                    let (_, end) = bounds.get_or_insert(((y, x), (y, x)));
                    *end = (y, x);
                }
            }
        }
        bounds.map(|(start, end)| grid.selection(start, end))
    }
}

type Position = (usize, usize);

/// Constant-time row access while walking cells; row IDs only locate endpoints.
struct Grid<'a>(&'a Screen);

impl Grid<'_> {
    fn len(&self) -> usize {
        self.0.history_len() + self.0.height()
    }

    fn row(&self, y: usize) -> Row<'_> {
        self.0.physical_row(y)
    }

    fn cell(&self, point: Position) -> &Cell {
        &self.row(point.0).cells[point.1]
    }

    fn position(&self, point: GridPoint) -> Option<Position> {
        let y = self.0.all_rows().position(|row| row.id == point.row)?;
        (point.col < self.row(y).cells.len()).then_some((y, point.col))
    }

    fn next(&self, (y, x): Position) -> Option<Position> {
        if x + 1 < self.row(y).cells.len() {
            Some((y, x + 1))
        } else {
            (y + 1 < self.len()).then_some((y + 1, 0))
        }
    }

    fn previous(&self, (y, x): Position) -> Option<Position> {
        if x > 0 {
            Some((y, x - 1))
        } else {
            y.checked_sub(1).map(|y| (y, self.row(y).cells.len() - 1))
        }
    }

    fn selection(&self, start: Position, end: Position) -> Selection {
        let point = |(y, col)| GridPoint {
            row: self.row(y).id,
            col,
        };
        Selection {
            start: point(start),
            end: point(end),
            rectangular: false,
        }
    }

    fn word(&self, point: Position, boundaries: &[char]) -> Option<Selection> {
        let boundary = boundaries.contains(&codepoint(self.cell(point))?);
        let same =
            |cell: &Cell| codepoint(cell).is_some_and(|c| boundaries.contains(&c) == boundary);
        let mut end = point;
        while let Some(next) = self.next(end) {
            if !same(self.cell(next)) {
                break;
            }
            end = next;
            // Match native's forward scan: this check follows the initial cell.
            if end.1 + 1 == self.row(end.0).cells.len() && !self.row(end.0).wrapped {
                break;
            }
        }
        let mut start = point;
        while let Some(previous) = self.previous(start) {
            let row = self.row(previous.0);
            if (previous.1 + 1 == row.cells.len() && !row.wrapped) || !same(self.cell(previous)) {
                break;
            }
            start = previous;
        }
        Some(self.selection(start, end))
    }
}

fn codepoint(cell: &Cell) -> Option<char> {
    cell.codepoint().filter(|&c| c != '\0')
}
