use super::{LiteralMatch, ViewportSearch, literal_text};
use crate::screen::OwnedTrackedPoint;
use crate::{GridPoint, PageCapacity, Screen, Terminal};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    FeedRequired,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    Progress,
    Blocked,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Next,
    Previous,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectScroll {
    #[default]
    IfNeeded,
    None,
}

/// Persistent literal search over both live terminal screens.
///
/// Feed with exclusive terminal access, then tick and inspect owned results
/// without borrowing the terminal. History is copied one window at a time;
/// active contents are refreshed during feed. Complete means caught up as of
/// the last feed, so feed periodically while the terminal is changing.
/// Dropping or resetting search releases its internally tracked references.
#[derive(Default)]
pub struct TerminalSearch {
    viewport: ViewportSearch,
    screens: [Option<ScreenSearch>; 2],
    alternate: bool,
}

impl TerminalSearch {
    pub fn new(needle: &[u8]) -> Self {
        Self {
            viewport: ViewportSearch::new(needle),
            ..Self::default()
        }
    }

    pub fn needle(&self) -> &[u8] {
        self.viewport.needle()
    }

    /// ASCII-case-equivalent needles preserve the original bytes and state.
    /// An empty needle returns to an idle, complete search.
    pub fn set_needle(&mut self, needle: &[u8]) -> bool {
        if !self.viewport.set_needle(needle) {
            return false;
        }
        self.screens = [None, None];
        self.alternate = false;
        true
    }

    pub fn reset(&mut self) {
        self.viewport.reset();
        self.screens = [None, None];
        self.alternate = false;
    }

    pub fn status(&self) -> Status {
        if self.needle().is_empty() {
            return Status::Complete;
        }
        let mut status = Status::Complete;
        let mut any = false;
        for search in self.screens.iter().flatten() {
            any = true;
            match search.state {
                State::History => return Status::Running,
                State::HistoryFeed => status = Status::FeedRequired,
                State::Complete => {}
            }
        }
        if any { status } else { Status::FeedRequired }
    }

    pub fn tick(&mut self) -> Tick {
        let needle = self.viewport.needle();
        let mut result = Tick::Complete;
        for search in self.screens.iter_mut().flatten() {
            match search.tick(needle) {
                Tick::Progress => result = Tick::Progress,
                Tick::Blocked if result == Tick::Complete => result = Tick::Blocked,
                _ => {}
            }
        }
        result
    }

    pub fn feed(&mut self, terminal: &mut Terminal, active_dirty: bool) {
        if self.needle().is_empty() {
            return;
        }
        self.alternate = terminal.alternate_active;
        let needle = self.viewport.needle();
        for (index, screen) in [Some(&mut terminal.primary), terminal.alternate.as_mut()]
            .into_iter()
            .enumerate()
        {
            let Some(screen) = screen else {
                self.screens[index] = None;
                continue;
            };
            if self.screens[index]
                .as_ref()
                .is_none_or(|search| search.identity != screen.metadata.identity)
            {
                self.screens[index] = Some(ScreenSearch::new(screen, needle));
            }
            if active_dirty && index == usize::from(self.alternate) {
                self.screens[index].as_mut().unwrap().reload(screen, needle);
            }
            // Cached native highlights retain physical page coordinates even
            // when a clean feed skips matching changed active contents.
            self.screens[index]
                .as_mut()
                .unwrap()
                .refresh_active_points(screen);
        }
        self.viewport.feed(terminal.screen(), active_dirty);
        let needle = self.viewport.needle();
        for (search, screen) in self
            .screens
            .iter_mut()
            .zip([Some(&mut terminal.primary), terminal.alternate.as_mut()])
        {
            if let (Some(search), Some(screen)) = (search, screen)
                && search.state != State::History
            {
                search.feed(screen, needle);
            }
        }
    }

    /// Refresh and finish all currently available history.
    pub fn run(&mut self, terminal: &mut Terminal) {
        self.feed(terminal, true);
        loop {
            match self.status() {
                Status::Running => {
                    self.tick();
                }
                Status::FeedRequired => self.feed(terminal, true),
                Status::Complete => return,
            }
        }
    }

    /// The active screen as of the last feed; reads never inspect the terminal.
    pub fn is_alternate_screen(&self) -> bool {
        self.alternate
    }

    pub fn total_matches(&self) -> usize {
        self.active().map_or(0, ScreenSearch::len)
    }

    /// Newest first, retaining native page overlap and duplicate matches.
    /// Feed after layout changes before resolving endpoints on a live screen.
    pub fn match_at(&self, index: usize) -> Option<LiteralMatch> {
        self.active()?.match_at(index).map(|value| value.bounds)
    }

    pub fn matches(&self) -> impl ExactSizeIterator<Item = LiteralMatch> + '_ {
        (0..self.total_matches()).map(|index| self.match_at(index).unwrap())
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.active()?
            .selected
            .as_ref()
            .map(|selected| selected.index)
    }

    pub fn selected_match(&self) -> Option<LiteralMatch> {
        self.match_at(self.selected_index()?)
    }

    pub fn viewport_matches(&self) -> &[LiteralMatch] {
        self.viewport.matches()
    }

    fn active(&self) -> Option<&ScreenSearch> {
        self.screens[usize::from(self.alternate)].as_ref()
    }

    /// Select with wrapping in either direction. This refreshes active matches
    /// and reconciles screen lifetimes first, but does not finish the history
    /// search. Terminal text selection is independent and remains unchanged.
    pub fn select(
        &mut self,
        terminal: &mut Terminal,
        direction: Direction,
        scroll: SelectScroll,
    ) -> bool {
        self.feed(terminal, false);
        let Some(search) = self.screens[usize::from(self.alternate)].as_mut() else {
            return false;
        };
        let screen = terminal.screen_mut();
        search.select(screen, self.viewport.needle(), direction);
        let Some(selected) = search
            .selected
            .as_ref()
            .and_then(|selected| search.match_at(selected.index))
        else {
            return false;
        };
        if scroll == SelectScroll::IfNeeded {
            let top = screen.history.len().saturating_sub(screen.viewport_offset);
            let bottom = top + screen.rows.len();
            let mut offset = 0;
            let visible = screen.pages.pages.iter().any(|page| {
                let visible = selected.chunks.iter().any(|chunk| {
                    chunk.page.serial == page.serial
                        && offset + usize::from(chunk.start) < bottom
                        && top < offset + usize::from(chunk.end)
                });
                offset += usize::from(page.rows);
                visible
            });
            if !visible && let Some(row) = row_index(screen, selected.bounds.start) {
                screen.viewport_offset = if screen.limits.bytes == Some(0) {
                    0
                } else {
                    screen.history.len().saturating_sub(row)
                };
                screen.viewport_pin_column = if row > 0 && screen.viewport_offset > 0 {
                    screen.viewport_pin = Some(selected.bounds.start);
                    selected.bounds.start.col
                } else {
                    0
                };
            }
        }
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PageStamp {
    list: u64,
    serial: u64,
    layout: u64,
    capacity: PageCapacity,
}

impl PageStamp {
    fn new(screen: &Screen, index: usize) -> Self {
        let page = &screen.pages.pages[index];
        Self {
            list: screen.pages.identity(),
            serial: page.serial,
            layout: page.layout_generation,
            capacity: page.capacity,
        }
    }

    fn valid(self, screen: &Screen) -> bool {
        self.list == screen.pages.identity()
            && screen.pages.pages.iter().any(|page| {
                page.serial == self.serial
                    && page.layout_generation == self.layout
                    && page.capacity == self.capacity
            })
    }
}

struct Chunk {
    page: PageStamp,
    start: u16,
    end: u16,
}

struct Found {
    bounds: LiteralMatch,
    chunks: Vec<Chunk>,
}

impl Found {
    fn valid(&self, screen: &Screen) -> bool {
        self.chunks.iter().all(|chunk| chunk.page.valid(screen))
    }
}

struct WindowPage {
    page: PageStamp,
    row_ids: Vec<u64>,
    len: usize,
}

/// Owned bytes and page coordinates. Like the native window, whole pages are
/// retained until searched, with enough trailing bytes for cross-page matches.
#[derive(Default)]
struct Window {
    bytes: Vec<u8>,
    cells: Vec<(u16, u16)>,
    pages: Vec<WindowPage>,
    offset: usize,
}

impl Window {
    fn append(&mut self, screen: &Screen, index: usize, reverse: bool) -> usize {
        let start: usize = screen
            .pages
            .pages
            .iter()
            .take(index)
            .map(|page| usize::from(page.rows))
            .sum();
        let count = usize::from(screen.pages.pages[index].rows);
        let rows: Vec<_> = (start..start + count)
            .map(|row| screen_row(screen, row))
            .collect();
        let line = literal_text(screen, &rows);
        let len = line.text.len();
        if len == 0 {
            return 0;
        }
        let positions: HashMap<_, _> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.id, index as u16))
            .collect();
        let old_len = self.bytes.len();
        self.bytes.extend_from_slice(line.text.as_bytes());
        for (start, end, point) in line.offsets {
            self.cells.extend(std::iter::repeat_n(
                (positions[&point.row], point.col as u16),
                end - start,
            ));
        }
        if reverse {
            self.bytes[old_len..].reverse();
            self.cells[old_len..].reverse();
        }
        self.pages.push(WindowPage {
            page: PageStamp::new(screen, index),
            row_ids: rows.iter().map(|row| row.id).collect(),
            len,
        });
        len
    }

    fn drain(&mut self, needle: &[u8], reverse: bool) -> Vec<Found> {
        if self.bytes.len() < needle.len() {
            return Vec::new();
        }
        let reversed;
        let needle = if reverse {
            reversed = needle.iter().rev().copied().collect::<Vec<_>>();
            &reversed
        } else {
            needle
        };
        let found = self.bytes[self.offset..]
            .windows(needle.len())
            .enumerate()
            .filter(|(_, bytes)| bytes.eq_ignore_ascii_case(needle))
            .map(|(start, _)| self.highlight(start + self.offset, needle.len(), reverse))
            .collect();
        let keep = needle.len() - 1;
        if keep == 0 {
            self.bytes.clear();
            self.cells.clear();
            self.pages.clear();
            self.offset = 0;
        } else {
            let mut bytes = 0;
            let count = self
                .pages
                .iter()
                .take_while(|page| {
                    if bytes + page.len > self.bytes.len() - keep {
                        return false;
                    }
                    bytes += page.len;
                    true
                })
                .count();
            self.pages.drain(..count);
            self.bytes.drain(..bytes);
            self.cells.drain(..bytes);
            self.offset = self.bytes.len() - keep;
        }
        found
    }

    fn highlight(&self, start: usize, len: usize, reverse: bool) -> Found {
        let end = start + len - 1;
        let mut offset = 0;
        let mut chunks = Vec::new();
        let mut bounds = Vec::with_capacity(2);
        for page in &self.pages {
            if start < offset + page.len && end >= offset {
                let first = start.max(offset);
                let last = end.min(offset + page.len - 1);
                let (first_y, first_x) = self.cells[first];
                let (last_y, last_x) = self.cells[last];
                if first == start {
                    bounds.push(GridPoint {
                        row: page.row_ids[usize::from(first_y)],
                        col: first_x.into(),
                    });
                }
                if last == end {
                    bounds.push(GridPoint {
                        row: page.row_ids[usize::from(last_y)],
                        col: last_x.into(),
                    });
                }
                chunks.push(Chunk {
                    page: page.page,
                    start: if reverse {
                        if last == end { last_y } else { 0 }
                    } else if first == start {
                        first_y
                    } else {
                        0
                    },
                    end: if reverse {
                        if first == start {
                            first_y + 1
                        } else {
                            page.row_ids.len() as u16
                        }
                    } else if last == end {
                        last_y + 1
                    } else {
                        page.row_ids.len() as u16
                    },
                });
            }
            offset += page.len;
            if offset > end {
                break;
            }
        }
        if reverse {
            bounds.reverse();
            chunks.reverse();
        }
        Found {
            bounds: LiteralMatch {
                start: bounds[0],
                end: bounds[1],
            },
            chunks,
        }
    }
}

fn screen_row(screen: &Screen, row: usize) -> &crate::Row {
    if row < screen.history.len() {
        &screen.history[row]
    } else {
        &screen.rows[row - screen.history.len()]
    }
}

fn row_index(screen: &Screen, point: GridPoint) -> Option<usize> {
    screen.all_rows().position(|row| row.id == point.row)
}

fn point_page(screen: &Screen, point: &OwnedTrackedPoint) -> Option<usize> {
    Some(
        screen
            .pages
            .page_index(row_index(screen, point.resolve(screen)?)?),
    )
}

fn page_point(screen: &Screen, index: usize, last: bool) -> GridPoint {
    let start: usize = screen
        .pages
        .pages
        .iter()
        .take(index)
        .map(|page| usize::from(page.rows))
        .sum();
    let page = &screen.pages.pages[index];
    GridPoint {
        row: screen_row(
            screen,
            start + if last { usize::from(page.rows) - 1 } else { 0 },
        )
        .id,
        col: if last {
            usize::from(page.columns) - 1
        } else {
            0
        },
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    History,
    HistoryFeed,
    Complete,
}

struct Selected {
    index: usize,
    start: OwnedTrackedPoint,
    end: OwnedTrackedPoint,
}

struct History {
    start: OwnedTrackedPoint,
    frontier: OwnedTrackedPoint,
    boundary: PageStamp,
    window: Window,
}

impl History {
    fn new(screen: &mut Screen, index: usize) -> Self {
        let mut window = Window::default();
        window.append(screen, index, true);
        Self {
            start: screen.track_owned(page_point(screen, index, false)),
            frontier: screen.track_owned(page_point(screen, index, true)),
            boundary: PageStamp::new(screen, index),
            window,
        }
    }

    fn feed(&mut self, screen: &mut Screen, needle: &[u8]) -> bool {
        let Some(index) = point_page(screen, &self.frontier) else {
            return false;
        };
        let mut fed = 0;
        for index in (0..index).rev() {
            fed += self.window.append(screen, index, true);
            self.frontier = screen.track_owned(page_point(screen, index, true));
            if fed >= needle.len() {
                break;
            }
        }
        fed > 0
    }
}

struct ScreenSearch {
    identity: u64,
    dimensions: (usize, usize),
    state: State,
    active: Vec<Found>,
    history_results: Vec<Found>,
    history: Option<History>,
    selected: Option<Selected>,
}

impl ScreenSearch {
    fn refresh_active_points(&mut self, screen: &Screen) {
        let mut pages: HashMap<_, Option<usize>> = self
            .active
            .iter()
            .flat_map(|found| [&found.chunks[0], found.chunks.last().unwrap()])
            .filter(|chunk| chunk.page.list == screen.pages.identity())
            .map(|chunk| (chunk.page.serial, None))
            .collect();
        let mut remaining = pages.len();
        if remaining == 0 {
            return;
        }
        // Active pages normally lie at the newest end. Resolve only captured
        // endpoints, without allocating a map of the whole history on feed.
        let mut offset = screen.history.len() + screen.rows.len();
        for page in screen.pages.pages.iter().rev() {
            offset -= usize::from(page.rows);
            if let Some(start) = pages.get_mut(&page.serial) {
                *start = Some(offset);
                remaining -= 1;
                if remaining == 0 {
                    break;
                }
            }
        }
        for found in &mut self.active {
            for (point, chunk, row) in [
                (
                    &mut found.bounds.start,
                    &found.chunks[0],
                    found.chunks[0].start,
                ),
                (
                    &mut found.bounds.end,
                    found.chunks.last().unwrap(),
                    found.chunks.last().unwrap().end - 1,
                ),
            ] {
                if chunk.page.list == screen.pages.identity()
                    && let Some(Some(offset)) = pages.get(&chunk.page.serial)
                    && let Some(current) = screen.point(offset + usize::from(row), point.col)
                {
                    *point = current;
                }
            }
        }
    }

    fn new(screen: &mut Screen, needle: &[u8]) -> Self {
        let mut result = Self {
            identity: screen.metadata.identity,
            dimensions: (screen.columns, screen.rows.len()),
            state: State::History,
            active: Vec::new(),
            history_results: Vec::new(),
            history: None,
            selected: None,
        };
        result.reload(screen, needle);
        result
    }

    fn reset_dimensions(&mut self, screen: &mut Screen, needle: &[u8]) -> bool {
        if self.dimensions == (screen.columns, screen.rows.len()) {
            return false;
        }
        *self = Self::new(screen, needle);
        true
    }

    fn len(&self) -> usize {
        self.active.len() + self.history_results.len()
    }

    fn match_at(&self, index: usize) -> Option<&Found> {
        if index < self.active.len() {
            self.active.get(self.active.len() - 1 - index)
        } else {
            self.history_results.get(index - self.active.len())
        }
    }

    fn tick(&mut self, needle: &[u8]) -> Tick {
        match self.state {
            State::History => {
                if let Some(history) = &mut self.history {
                    self.history_results.extend(
                        history
                            .window
                            .drain(needle, true)
                            .into_iter()
                            .filter(|found| found.chunks[0].page.serial != history.boundary.serial),
                    );
                    self.state = State::HistoryFeed;
                } else {
                    self.state = State::Complete;
                }
                Tick::Progress
            }
            State::HistoryFeed => Tick::Blocked,
            State::Complete => Tick::Complete,
        }
    }

    fn feed(&mut self, screen: &mut Screen, needle: &[u8]) {
        self.reset_dimensions(screen, needle);
        let Some(history) = &mut self.history else {
            self.state = State::Complete;
            return;
        };
        if history.feed(screen, needle) {
            self.state = State::History;
        } else {
            self.state = State::Complete;
            self.prune(screen);
        }
    }

    fn prune(&mut self, screen: &Screen) {
        let mut index = self.active.len();
        self.history_results.retain(|found| {
            if found.valid(screen) {
                index += 1;
                return true;
            }
            if let Some(selected) = &mut self.selected {
                if selected.index == index {
                    self.selected = None;
                } else if selected.index > index {
                    selected.index -= 1;
                }
            }
            false
        });
    }

    fn select(&mut self, screen: &mut Screen, needle: &[u8], direction: Direction) {
        self.reload(screen, needle);
        self.prune(screen);
        self.choose(screen, direction);
    }

    fn choose(&mut self, screen: &mut Screen, direction: Direction) {
        let len = self.len();
        if len == 0 {
            self.selected = None;
            return;
        }
        let index = match (self.selected.as_ref(), direction) {
            (None, Direction::Next) => 0,
            (None, Direction::Previous) => len - 1,
            (Some(selected), Direction::Next) => (selected.index + 1) % len,
            (Some(selected), Direction::Previous) => (selected.index + len - 1) % len,
        };
        let found = self.match_at(index).unwrap().bounds;
        self.selected = Some(Selected {
            index,
            start: screen.track_owned(found.start),
            end: screen.track_owned(found.end),
        });
    }

    fn reload(&mut self, screen: &mut Screen, needle: &[u8]) {
        if self.reset_dimensions(screen, needle) {
            return;
        }
        if let Some(selected) = &self.selected
            && selected.index >= self.active.len()
            && self
                .match_at(selected.index)
                .is_none_or(|found| !found.valid(screen))
        {
            self.selected = None;
        }
        let select_previous = self.selected.as_ref().is_some_and(|selected| {
            selected.start.resolve(screen).is_none() || selected.end.resolve(screen).is_none()
        });
        if select_previous {
            self.selected = None;
        }

        let boundary = screen.pages.page_index(screen.history.len());
        let mut active = Window::default();
        for index in (boundary..screen.pages.pages.len()).rev() {
            active.append(screen, index, false);
        }
        for index in (0..boundary).rev() {
            let end = page_point(screen, index, true);
            if !screen.row_by_id(end.row).unwrap().wrapped {
                break;
            }
            if active.append(screen, index, false) >= needle.len() - 1 {
                break;
            }
        }

        if screen.limits.bytes != Some(0) {
            let previous = self
                .history
                .as_ref()
                .and_then(|history| point_page(screen, &history.start));
            match previous {
                None => {
                    self.history_results.clear();
                    self.history = Some(History::new(screen, boundary));
                }
                Some(previous) if previous != boundary => {
                    let mut window = Window::default();
                    for index in previous..=boundary {
                        window.append(screen, index, false);
                    }
                    let boundary_stamp = PageStamp::new(screen, boundary);
                    let mut added: Vec<_> = window
                        .drain(needle, false)
                        .into_iter()
                        .filter(|found| found.chunks[0].page.serial != boundary_stamp.serial)
                        .collect();
                    added.reverse();
                    if let Some(selected) = &mut self.selected
                        && selected.index >= self.active.len()
                    {
                        selected.index += added.len();
                    }
                    added.append(&mut self.history_results);
                    self.history_results = added;
                    let history = self.history.as_mut().unwrap();
                    history.start = screen.track_owned(page_point(screen, boundary, false));
                    history.boundary = boundary_stamp;
                }
                Some(_) => {}
            }
        } else {
            self.history = None;
            self.history_results.clear();
        }

        let old_len = self.active.len();
        self.active = active.drain(needle, false);
        if screen.limits.bytes == Some(0) {
            let prefix = self
                .active
                .iter()
                .take_while(|found| {
                    (
                        row_index(screen, found.bounds.end).unwrap(),
                        found.bounds.end.col,
                    ) <= (screen.history.len(), 0)
                })
                .count();
            self.active.drain(..prefix);
        }
        if let Some(selected) = &mut self.selected {
            if selected.index >= old_len {
                selected.index = selected.index - old_len + self.active.len();
            } else {
                let start = selected.start.resolve(screen);
                let end = selected.end.resolve(screen);
                if let Some(index) = self.active.iter().position(|found| {
                    Some(found.bounds.start) == start && Some(found.bounds.end) == end
                }) {
                    selected.index = self.active.len() - 1 - index;
                } else {
                    self.selected = None;
                    self.prune(screen);
                    self.choose(screen, Direction::Next);
                }
            }
        }
        if select_previous {
            self.prune(screen);
            self.choose(screen, Direction::Previous);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history() -> Terminal {
        let mut terminal = Terminal::new(1024, 4, usize::MAX);
        terminal.feed(&b"A\r\n".repeat(180));
        terminal
    }

    #[test]
    fn history_requires_incremental_feeds_and_tick_owns_its_data() {
        let mut terminal = history();
        let expected = terminal.screen().search_literal(b"A");
        let mut search = TerminalSearch::new(b"A");
        assert_eq!(search.status(), Status::FeedRequired);
        search.feed(&mut terminal, false);
        assert!(search.total_matches() < expected.len());
        let mut feeds = 0;
        while search.status() != Status::Complete {
            if search.status() == Status::FeedRequired {
                feeds += 1;
                search.feed(&mut terminal, false);
            } else {
                assert_eq!(search.tick(), Tick::Progress);
            }
        }
        assert!(feeds > 2);
        assert_eq!(search.matches().collect::<Vec<_>>(), expected);

        search.reset();
        search.feed(&mut terminal, false);
        search.tick();
        search.feed(&mut terminal, false);
        let before = search.total_matches();
        drop(terminal);
        assert_eq!(search.tick(), Tick::Progress);
        assert!(search.total_matches() > before);
    }

    #[test]
    fn dropping_search_does_not_leave_pins_affecting_reflow() {
        let mut terminal = history();
        let mut search = TerminalSearch::new(b"A");
        search.run(&mut terminal);
        search.select(&mut terminal, Direction::Previous, SelectScroll::None);
        let mut expected = terminal.clone();
        drop(search);
        terminal.resize(5, 4);
        expected.resize(5, 4);
        let actual = serde_json::to_value(terminal.screen()).unwrap();
        let expected = serde_json::to_value(expected.screen()).unwrap();
        for field in ["history", "rows"] {
            assert_eq!(actual[field], expected[field]);
        }
    }

    #[test]
    fn selection_wraps_without_replacing_text_selection() {
        let mut terminal = Terminal::new(8, 4, usize::MAX);
        terminal.feed(b"A B A");
        let point = terminal.screen().point(0, 2).unwrap();
        terminal.screen_mut().selection = Some(crate::Selection {
            start: point,
            end: point,
            rectangular: false,
        });
        let mut search = TerminalSearch::new(b"A");
        for expected in [0, 1, 0] {
            assert!(search.select(&mut terminal, Direction::Next, SelectScroll::None));
            assert_eq!(search.selected_index(), Some(expected));
        }
        assert_eq!(terminal.screen().selection.unwrap().start, point);
        search.set_needle(b"a");
        assert_eq!(search.selected_index(), Some(0));
        assert_eq!(search.needle(), b"A");
        search.set_needle(b"");
        assert_eq!(search.selected_index(), None);
        assert_eq!(search.status(), Status::Complete);
    }

    #[test]
    fn returning_to_active_viewport_discards_the_search_pin_column() {
        let mut terminal = Terminal::new(80, 4, usize::MAX);
        terminal.feed(&b"....A\r\n".repeat(300));
        let mut search = TerminalSearch::new(b"A");
        search.run(&mut terminal);
        for _ in 0..7 {
            search.select(&mut terminal, Direction::Next, SelectScroll::IfNeeded);
        }
        assert_eq!(terminal.screen().viewport_top().col, 4);
        // Application callers can return to active by setting this public
        // offset. A subsequent ordinary scroll must start with column zero.
        terminal.screen_mut().viewport_offset = 0;
        terminal.screen_mut().scroll_viewport(1);
        assert_eq!(terminal.screen().viewport_top().col, 0);
    }
}
