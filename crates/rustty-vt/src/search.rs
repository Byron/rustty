//! Regex matching works on unwrapped lines and maps byte offsets back to cells.
use crate::{GridPoint, Screen};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub start: GridPoint,
    pub end: GridPoint,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub start: GridPoint,
    pub end: GridPoint,
    pub uri: String,
}

struct Line {
    text: String,
    offsets: Vec<(usize, usize, GridPoint)>,
}

impl Line {
    fn cells(&self, range: Range<usize>) -> Option<(GridPoint, GridPoint)> {
        let start = self
            .offsets
            .partition_point(|&(_, end, _)| end <= range.start);
        let end = self
            .offsets
            .partition_point(|&(start, _, _)| start < range.end)
            .checked_sub(1)?;
        Some((self.offsets.get(start)?.2, self.offsets.get(end)?.2))
    }
    fn matches(&self, regex: &Regex) -> Vec<Match> {
        regex
            .find_iter(&self.text)
            .filter(|m| !m.is_empty())
            .filter_map(|m| {
                let (start, end) = self.cells(m.range())?;
                Some(Match {
                    start,
                    end,
                    text: m.as_str().to_owned(),
                })
            })
            .collect()
    }
}

impl Link {
    pub fn contains(&self, screen: &Screen, point: GridPoint) -> bool {
        let position = |point: GridPoint| {
            screen
                .all_rows()
                .position(|row| row.id == point.row)
                .map(|row| (row, point.col))
        };
        match (position(self.start), position(self.end), position(point)) {
            (Some(start), Some(end), Some(point)) => start <= point && point <= end,
            _ => false,
        }
    }
}

fn logical_lines(screen: &Screen) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut line = Line {
        text: String::new(),
        offsets: Vec::new(),
    };
    for row in screen.all_rows() {
        let used = if row.wrapped {
            row.cells.len()
        } else {
            row.used()
        };
        for (col, cell) in row.cells.iter().enumerate().take(used) {
            if cell.width == 0 || cell.spacer_head {
                continue;
            }
            let start = line.text.len();
            if cell.text.is_empty() {
                line.text.push(' ');
            } else {
                line.text.push_str(&cell.text);
            }
            line.offsets
                .push((start, line.text.len(), GridPoint { row: row.id, col }));
        }
        if !row.wrapped {
            lines.push(line);
            line = Line {
                text: String::new(),
                offsets: Vec::new(),
            };
        }
    }
    if !line.text.is_empty() {
        lines.push(line);
    }
    lines
}

impl Screen {
    pub fn search(&self, regex: &Regex) -> Vec<Match> {
        logical_lines(self)
            .into_iter()
            .flat_map(|line| line.matches(regex))
            .collect()
    }
}

/// Compile once at config load; cached results are scoped to logical-line text.
pub struct LinkMatcher {
    patterns: Vec<Regex>,
    cache: HashMap<u64, (String, Vec<Range<usize>>)>,
}

impl LinkMatcher {
    pub fn new(patterns: impl IntoIterator<Item = String>) -> Result<Self, regex::Error> {
        Ok(Self {
            patterns: patterns
                .into_iter()
                .map(|p| Regex::new(&p))
                .collect::<Result<_, _>>()?,
            cache: HashMap::new(),
        })
    }
    pub fn links(&mut self, screen: &Screen) -> Vec<Link> {
        let mut result = Vec::new();
        let mut present = HashSet::new();
        for line in logical_lines(screen) {
            let Some(&(_, _, first)) = line.offsets.first() else {
                continue;
            };
            present.insert(first.row);
            let links = self
                .cache
                .entry(first.row)
                .or_insert_with(|| (String::new(), Vec::new()));
            let changed = links.0 != line.text;
            if changed {
                links.1 = self
                    .patterns
                    .iter()
                    .flat_map(|pattern| pattern.find_iter(&line.text))
                    .filter(|m| !m.is_empty())
                    .map(|m| m.range())
                    .collect();
            }
            // Text can stay identical while reflow or wide-cell edits change
            // its cell mapping. Cache regex offsets, then project current cells.
            result.extend(links.1.iter().filter_map(|range| {
                let (start, end) = line.cells(range.clone())?;
                Some(Link {
                    start,
                    end,
                    uri: line.text[range.clone()].to_owned(),
                })
            }));
            if changed {
                links.0 = line.text;
            }
        }
        self.cache.retain(|id, _| present.contains(id));
        // Explicit OSC 8 links take precedence over regex matches on those cells.
        let row_indices: HashMap<_, _> = screen
            .all_rows()
            .enumerate()
            .map(|(index, row)| (row.id, index))
            .collect();
        for row in screen.all_rows() {
            let mut col = 0;
            while col < row.cells.len() {
                let Some(uri) = &row.cells[col].hyperlink else {
                    col += 1;
                    continue;
                };
                let start = col;
                while col + 1 < row.cells.len()
                    && row.cells[col + 1].hyperlink.as_ref() == Some(uri)
                {
                    col += 1;
                }
                let end = col;
                result.retain(|link| {
                    let first = (row_indices[&link.start.row], link.start.col);
                    let last = (row_indices[&link.end.row], link.end.col);
                    let row = row_indices[&row.id];
                    last < (row, start) || first > (row, end)
                });
                result.push(Link {
                    start: GridPoint {
                        row: row.id,
                        col: start,
                    },
                    end: GridPoint {
                        row: row.id,
                        col: end,
                    },
                    uri: uri.clone(),
                });
                col += 1;
            }
        }
        result
    }
}

impl Default for LinkMatcher {
    fn default() -> Self {
        Self::new([
            r#"(?i)(?-u:\b)(?:https?|ftp|file|mailto|ssh)://[^\s<>\x00-\x1f\x7f\"']+"#.to_owned(),
        ])
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Terminal;
    #[test]
    fn cached_links_follow_current_cells_and_explicit_links_across_wraps() {
        let mut terminal = Terminal::new(40, 3, 10);
        terminal.feed("界https://example.org".as_bytes());
        let mut matcher = LinkMatcher::default();
        let original = matcher.links(terminal.screen());
        assert_eq!(original[0].start.col, 2);
        let mut changed = terminal.screen().clone();
        // Same logical text and row identity, but a different leading-cell width.
        changed.rows[0].cells.remove(1);
        changed.rows[0].cells[0].width = 1;
        let moved = matcher.links(&changed);
        assert_eq!(moved[0].start.col, 1);
        assert_eq!(moved[0].end.col + 1, original[0].end.col);
        terminal = Terminal::new(10, 3, 10);
        terminal.feed(b"https://x.\x1b]8;;https://actual\x07org\x1b]8;;\x07");
        let links = matcher.links(terminal.screen());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].uri, "https://actual");
        assert!(links[0].contains(terminal.screen(), links[0].end));
    }
    #[test]
    fn matching_crosses_soft_wraps_and_keeps_cell_coordinates() {
        let mut t = Terminal::new(8, 4, 10);
        t.feed("界https://example.org".as_bytes());
        let mut matcher = LinkMatcher::default();
        let links = matcher.links(t.screen());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].uri, "https://example.org");
        assert_eq!(links[0].start.col, 2);
        let matches = t.screen().search(&Regex::new("example").unwrap());
        assert_eq!(matches.len(), 1);
        t.feed(b"\x1b[1;3Hnope");
        assert!(matcher.links(t.screen()).is_empty());
        t.feed(b"\x1b[2J\x1b[H\x1b]8;;https://actual\x07example\x1b]8;;\x07");
        assert_eq!(matcher.links(t.screen())[0].uri, "https://actual");
    }
}
