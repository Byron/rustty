//! Literal terminal search and regex matching with byte-to-cell coordinates.
use crate::{GridPoint, PageCapacity, Row, Screen};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

mod terminal;
pub use terminal::{Direction, SelectScroll, Status, TerminalSearch, Tick};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub start: GridPoint,
    pub end: GridPoint,
    pub text: String,
}

/// Inclusive cell endpoints of a literal byte match.
///
/// Each endpoint maps the corresponding matched byte, including matches inside
/// a UTF-8 encoding. Native whitespace coordinates are preserved as returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiteralMatch {
    pub start: GridPoint,
    pub end: GridPoint,
}

/// Cached literal matches on the pages covering the viewport.
///
/// Feed while the screen is safe to read, then read the owned matches without
/// accessing the terminal. Matches can extend beyond visible rows because the
/// native search formats whole pages and includes soft-wrapped overlap.
#[derive(Default)]
pub struct ViewportSearch {
    needle: Vec<u8>,
    list_identity: Option<u64>,
    fingerprint: Vec<(u64, u64, PageCapacity)>,
    matches: Vec<LiteralMatch>,
}

impl ViewportSearch {
    pub fn new(needle: &[u8]) -> Self {
        Self {
            needle: needle.to_vec(),
            ..Self::default()
        }
    }

    pub fn needle(&self) -> &[u8] {
        &self.needle
    }

    /// Replace the needle, clearing cached results until the next feed. An
    /// ASCII-case-equivalent needle preserves both results and original bytes.
    /// An empty needle leaves the search idle. Returns whether it changed.
    pub fn set_needle(&mut self, needle: &[u8]) -> bool {
        if self.needle.eq_ignore_ascii_case(needle) {
            return false;
        }
        self.needle.clear();
        self.needle.extend_from_slice(needle);
        self.reset();
        true
    }

    /// Clear the cache while retaining the needle; the next feed re-searches.
    pub fn reset(&mut self) {
        self.list_identity = None;
        self.fingerprint.clear();
        self.matches.clear();
    }

    /// Owned endpoints from the last feed. Feed again after layout changes
    /// before resolving these points against a live screen.
    pub fn matches(&self) -> &[LiteralMatch] {
        &self.matches
    }

    /// Refresh the viewport cache. Pass true when the active area may have
    /// changed, or when the caller does not track such changes. A historical
    /// viewport is searched again only when its covering pages change.
    /// Returns whether the cache was refreshed.
    pub fn feed(&mut self, screen: &Screen, active_dirty: bool) -> bool {
        if self.needle.is_empty() {
            return false;
        }
        let pages = &screen.pages;
        let top = screen.history.len().saturating_sub(screen.viewport_offset);
        let first = pages.page_index(top);
        let last = pages.page_index(top + screen.rows.len() - 1);
        let entries = || {
            pages
                .pages
                .range(first..=last)
                .map(|page| (page.serial, page.layout_generation, page.capacity))
        };
        let unchanged = self.list_identity == Some(pages.identity())
            && self.fingerprint.iter().copied().eq(entries());
        if unchanged {
            let active_first = pages.page_index(screen.history.len());
            let active_last = pages.pages.len() - 1;
            if !active_dirty
                || !((first..=last).contains(&active_first)
                    || (first..=last).contains(&active_last))
            {
                return false;
            }
        }
        self.list_identity = Some(pages.identity());
        self.fingerprint.clear();
        self.fingerprint.extend(entries());

        let first_row: usize = pages
            .pages
            .iter()
            .take(first)
            .map(|page| usize::from(page.rows))
            .sum();
        let row = |y: usize| {
            if y < screen.history.len() {
                &screen.history[y]
            } else {
                &screen.rows[y - screen.history.len()]
            }
        };
        let row_count = |index: usize| usize::from(pages.pages[index].rows);
        let wrapped = |index: usize, start: usize| row(start + row_count(index) - 1).wrapped;
        let page_text = |index: usize, start: usize| {
            literal_text(
                &(start..start + row_count(index))
                    .map(row)
                    .collect::<Vec<_>>(),
            )
        };
        let overlap = self.needle.len() - 1;
        let mut window = Line::default();
        let mut added = 0;
        let mut offset = first_row;
        // Native appends preceding wrapped pages in reverse physical order.
        for index in (0..first).rev() {
            offset -= row_count(index);
            if !wrapped(index, offset) {
                break;
            }
            let text = page_text(index, offset);
            added += text.text.len();
            window.append(&text);
            if added >= overlap {
                break;
            }
        }
        offset = first_row;
        for index in first..=last {
            window.append(&page_text(index, offset));
            offset += row_count(index);
        }
        if row(offset - 1).wrapped {
            added = 0;
            for index in last + 1..pages.pages.len() {
                let text = page_text(index, offset);
                added += text.text.len();
                window.append(&text);
                if added >= overlap || !wrapped(index, offset) {
                    break;
                }
                offset += row_count(index);
            }
        }
        self.matches = window.literal_matches(&self.needle);
        true
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub start: GridPoint,
    pub end: GridPoint,
    pub uri: String,
}

#[derive(Default)]
struct Line {
    text: String,
    offsets: Vec<(usize, usize, GridPoint)>,
}

impl Line {
    fn append(&mut self, other: &Self) {
        let offset = self.text.len();
        self.text.push_str(&other.text);
        self.offsets.extend(
            other
                .offsets
                .iter()
                .map(|&(start, end, point)| (start + offset, end + offset, point)),
        );
    }

    fn push(&mut self, text: &str, point: GridPoint) {
        let start = self.text.len();
        self.text.push_str(text);
        self.offsets.push((start, self.text.len(), point));
    }

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

    fn literal_matches(&self, needle: &[u8]) -> Vec<LiteralMatch> {
        self.text
            .as_bytes()
            .windows(needle.len())
            .enumerate()
            .filter(|(_, bytes)| bytes.eq_ignore_ascii_case(needle))
            .filter_map(|(offset, _)| {
                let (start, end) = self.cells(offset..offset + needle.len())?;
                Some(LiteralMatch { start, end })
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

/// Plain, trimmed, unwrapped text and point mapping used by native search.
/// Regex links deliberately use logical_lines instead: their whitespace and
/// hard-line boundaries are part of the regex matching contract.
fn literal_text(rows: &[&Row]) -> Line {
    let mut line = Line::default();
    let Some(first) = rows.first() else {
        return line;
    };
    let mut last = (0, 0);
    let mut blank_rows = 0;
    let mut blank_cells = 0;
    for (y, row) in rows.iter().enumerate() {
        if row.cells.iter().all(|cell| cell.text.is_empty()) {
            blank_rows += 1;
            continue;
        }
        let prior = last;
        for offset in 0..blank_rows {
            let (y, col) = if offset == 0 {
                prior
            } else {
                (prior.0 + offset, 0)
            };
            line.push(
                "\n",
                GridPoint {
                    row: rows[y].id,
                    col,
                },
            );
            last = (y, col);
        }
        blank_rows = usize::from(!row.wrapped);
        if !row.wrap_continuation {
            blank_cells = 0;
        }
        for (col, cell) in row.cells.iter().enumerate() {
            if cell.width == 0 || cell.spacer_head {
                continue;
            }
            if cell.text.is_empty() || cell.text.starts_with(' ') {
                blank_cells += 1;
                continue;
            }
            // PageFormatter.appendBlankPoints walks backwards from the next
            // written cell. Keep its order, even for adjacent spaces and wraps.
            let mut blank = (y, col);
            for _ in 0..blank_cells {
                if blank.1 > 0 {
                    blank.1 -= 1;
                } else if blank.0 > 0 {
                    blank.0 -= 1;
                    blank.1 = rows[blank.0].cells.len() - 1;
                }
                line.push(
                    " ",
                    GridPoint {
                        row: rows[blank.0].id,
                        col: blank.1,
                    },
                );
            }
            blank_cells = 0;
            line.push(&cell.text, GridPoint { row: row.id, col });
            last = (y, col);
        }
    }
    // SlidingWindow terminates a nonwrapped page with one newline, mapped to
    // the final emitted byte; trailing blank rows and spaces are not flushed.
    if rows.last().is_some_and(|row| !row.wrapped) {
        let point = line.offsets.last().map_or(
            GridPoint {
                row: first.id,
                col: 0,
            },
            |entry| entry.2,
        );
        line.push("\n", point);
    }
    line
}

impl Screen {
    /// Matches in terminal search order, from newest (bottom/right) to oldest.
    pub fn search(&self, regex: &Regex) -> Vec<Match> {
        let mut matches: Vec<_> = logical_lines(self)
            .into_iter()
            .flat_map(|line| line.matches(regex))
            .collect();
        matches.reverse();
        matches
    }

    /// Literal, ASCII-case-insensitive search in native page traversal order.
    ///
    /// Matches may overlap or span soft wraps and hard newlines. Empty needles
    /// produce no matches; arbitrary byte needles need not be valid UTF-8.
    /// Each physical page is trimmed separately. Active and history searches
    /// preserve their native overlap, including repeated matches and endpoints
    /// that cross pages in reverse physical order.
    pub fn search_literal(&self, needle: &[u8]) -> Vec<LiteralMatch> {
        if needle.is_empty() {
            return Vec::new();
        }
        let rows: Vec<_> = self.all_rows().collect();
        let row_positions: HashMap<_, _> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.id, index))
            .collect();
        let mut pages = Vec::with_capacity(self.pages.pages.len());
        let mut start = 0;
        let mut boundary = (0, 0);
        for (index, page) in self.pages.pages.iter().enumerate() {
            let end = start + usize::from(page.rows);
            if start <= self.history.len() && end > self.history.len() {
                boundary = (index, start);
            }
            pages.push((literal_text(&rows[start..end]), rows[end - 1].wrapped));
            start = end;
        }

        // Native ActiveSearch appends newest pages first but formats/searches
        // each one forward. Reversing results is a separate final operation.
        let mut active = Line::default();
        for (page, _) in pages[boundary.0..].iter().rev() {
            active.append(page);
        }
        for (page, wrapped) in pages[..boundary.0].iter().rev() {
            if !wrapped {
                break;
            }
            active.append(page);
            if page.text.len() >= needle.len() - 1 {
                break;
            }
        }
        let mut matches = active.literal_matches(needle);
        if self.limits.bytes == Some(0) {
            // Native removes only a leading prefix before reversing. Later
            // historical matches from a boundary page can remain after ED22.
            let prefix = matches
                .iter()
                .take_while(|found| {
                    (row_positions[&found.end.row], found.end.col) <= (self.history.len(), 0)
                })
                .count();
            matches.drain(..prefix);
            matches.reverse();
            return matches;
        }
        matches.reverse();
        if boundary.0 == 0 {
            return matches;
        }

        // Reversing this forward sequence's results is equivalent to native
        // history's reversed bytes and reversed needle, including UTF-8 parts.
        let mut history = Line::default();
        for (page, _) in &pages[..=boundary.0] {
            history.append(page);
        }
        matches.extend(
            history
                .literal_matches(needle)
                .into_iter()
                .rev()
                .filter(|found| row_positions[&found.start.row] < boundary.1),
        );
        matches
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
    fn viewport_cache_is_owned_and_needle_changes_restart_it() {
        let mut terminal = Terminal::new(20, 3, 100);
        terminal.feed(b"cat CAT cat");
        let mut search = ViewportSearch::new(b"cat");
        assert!(search.matches().is_empty());
        assert!(search.feed(terminal.screen(), false));
        let original = search.matches().to_vec();
        assert_eq!(original.len(), 3);
        assert!(!search.feed(terminal.screen(), false));
        assert!(!search.set_needle(b"CAT"));
        assert_eq!(search.needle(), b"cat");
        terminal.feed(b"\x1b[Hdog");
        assert_eq!(search.matches(), original);
        assert!(!search.feed(terminal.screen(), false));
        assert_eq!(search.matches(), original);
        assert!(search.feed(terminal.screen(), true));
        assert_eq!(search.matches().len(), 2);
        search.reset();
        assert!(search.matches().is_empty());
        assert_eq!(search.needle(), b"cat");
        assert!(search.feed(terminal.screen(), false));
        assert!(search.set_needle(b"dog"));
        assert!(search.matches().is_empty());
        assert!(search.feed(terminal.screen(), false));
        assert_eq!(search.matches().len(), 1);
        assert!(search.set_needle(b""));
        assert!(!search.feed(terminal.screen(), true));
        assert!(search.matches().is_empty());
    }

    #[test]
    fn viewport_cache_distinguishes_replaced_and_cloned_page_lists() {
        let mut terminal = Terminal::new(8, 3, 100);
        terminal.feed(b"AA");
        let mut search = ViewportSearch::new(b"A");
        assert!(search.feed(terminal.screen(), false));
        let copy = terminal.screen().clone();
        assert_ne!(copy.pages.identity(), terminal.screen().pages.identity());
        assert!(search.feed(&copy, false));
        let restored: Screen = serde_json::from_slice(&serde_json::to_vec(&copy).unwrap()).unwrap();
        assert_ne!(copy.pages.identity(), restored.pages.identity());
        assert!(search.feed(&restored, false));
        assert!(search.feed(terminal.screen(), false));
        terminal.reset();
        assert!(search.feed(terminal.screen(), false));
        assert!(search.matches().is_empty());
        terminal.feed(b"A\x1b[?47h");
        assert!(search.feed(terminal.screen(), false));
        assert!(search.matches().is_empty());
        terminal.feed(b"\x1b[?47l");
        assert!(search.feed(terminal.screen(), false));
        assert_eq!(search.matches().len(), 1);
    }

    #[test]
    fn viewport_dirty_tracking_observes_page_layout_changes() {
        let mut terminal = Terminal::new(8, 4, 100);
        terminal.feed(b"A B\r\nC A\r\nA D\r\nE A");
        let mut search = ViewportSearch::new(b"A");
        search.feed(terminal.screen(), true);
        terminal.feed(b"\x1b[2;1H\x1b[M");
        assert!(search.feed(terminal.screen(), false));
        assert_eq!(search.matches().len(), 3);
        // Moving within a page changes which rows are visible, but native
        // searches the entire page and keeps its cached results.
        terminal.feed(b"\r\nA\r\nB\r\nC");
        search.feed(terminal.screen(), true);
        terminal.screen_mut().scroll_viewport(1);
        assert!(!search.feed(terminal.screen(), false));
    }

    #[test]
    fn search_starts_with_the_latest_match_including_history_and_soft_wraps() {
        let mut terminal = Terminal::new(8, 2, 100);
        terminal.feed(b"cat cat\r\ncatcatcat\r\ncat");
        let screen = terminal.screen();
        let matches = screen.search(&Regex::new("cat").unwrap());
        let points: Vec<_> = matches
            .iter()
            .map(|found| {
                let index = screen
                    .all_rows()
                    .position(|row| row.id == found.start.row)
                    .unwrap();
                (index, found.start.col)
            })
            .collect();
        assert_eq!(points, [(3, 0), (1, 6), (1, 3), (1, 0), (0, 4), (0, 0)]);
    }
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
