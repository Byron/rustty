//! Native terminal gesture state, separate from application focus and click policy.
//!
//! Returned selections are not applied to the screen. Only autoscroll changes the
//! viewport. Call `reset` before abandoning a gesture and `deinit` before discarding
//! it to release its tracked anchor. `release` preserves the repeat sequence; the
//! caller must stop forwarding drag events after release. Serialize calls with
//! other terminal mutations. Anchors follow live rows and never alias a recycled
//! screen. Screen changes, reset and pruned anchors stop selection; an autoscroll
//! tick with an invalid anchor cancels the gesture.
use crate::{GridPoint, Screen, Selection, Terminal, TrackedPoint, selection::SelectLine};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Behavior {
    #[default]
    Cell,
    Word,
    Line,
    Output,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Autoscroll {
    #[default]
    None,
    Up,
    Down,
}

pub const DEFAULT_BEHAVIORS: [Behavior; 3] = [Behavior::Cell, Behavior::Word, Behavior::Line];

pub struct Press<'a> {
    /// Caller-supplied monotonic nanoseconds; absent/backwards time breaks repeats.
    pub time: Option<i128>,
    pub point: GridPoint,
    pub xpos: f64,
    pub ypos: f64,
    pub max_distance: f64,
    pub repeat_interval: u64,
    pub word_boundaries: &'a [char],
    pub behaviors: [Behavior; 3],
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Geometry {
    pub columns: u32,
    pub cell_width: u32,
    pub padding_left: u32,
    pub screen_height: u32,
}

pub struct Drag<'a> {
    pub point: GridPoint,
    pub xpos: f64,
    pub ypos: f64,
    pub rectangle: bool,
    pub word_boundaries: &'a [char],
    pub geometry: Geometry,
}

pub struct AutoscrollTick<'a> {
    /// Column and row in the viewport, resolved after scrolling by one row.
    pub viewport: [u32; 2],
    pub xpos: f64,
    pub ypos: f64,
    pub rectangle: bool,
    pub word_boundaries: &'a [char],
    pub geometry: Geometry,
}

#[derive(Debug, Default)]
pub struct SelectionGesture {
    anchor: Option<TrackedPoint>,
    click_count: u8,
    time: Option<i128>,
    behavior: Behavior,
    xpos: f64,
    ypos: f64,
    dragged: bool,
    autoscroll: Autoscroll,
}

impl SelectionGesture {
    pub fn click_count(&self) -> u8 {
        self.click_count
    }
    pub fn behavior(&self) -> Behavior {
        self.behavior
    }
    pub fn dragged(&self) -> bool {
        self.dragged
    }
    pub fn autoscroll(&self) -> Autoscroll {
        self.autoscroll
    }
    pub fn has_anchor(&self) -> bool {
        self.anchor.is_some()
    }

    pub fn anchor(&self, terminal: &Terminal) -> Option<GridPoint> {
        terminal.screen().resolve(self.anchor?)
    }

    pub fn reset(&mut self, terminal: &mut Terminal) {
        self.click_count = 0;
        self.time = None;
        self.behavior = Behavior::Cell;
        self.dragged = false;
        self.autoscroll = Autoscroll::None;
        self.untrack(terminal);
    }

    pub fn deinit(mut self, terminal: &mut Terminal) {
        self.untrack(terminal);
    }

    pub fn press(&mut self, terminal: &mut Terminal, press: Press<'_>) -> Option<Selection> {
        position(terminal.screen(), press.point)?;
        let repeat = self.click_count > 0 && self.repeats(terminal, &press);
        if repeat {
            self.click_count = (self.click_count + 1).min(3);
        } else {
            self.untrack(terminal);
            self.anchor = Some(terminal.screen_mut().track(press.point));
            self.click_count = 1;
            self.xpos = press.xpos;
            self.ypos = press.ypos;
        }
        self.time = press.time;
        self.dragged = false;
        self.autoscroll = Autoscroll::None;
        self.behavior = press.behaviors[usize::from(self.click_count - 1)];
        let screen = terminal.screen();
        match self.behavior {
            Behavior::Cell => None,
            Behavior::Word => screen.select_word(press.point, press.word_boundaries),
            Behavior::Line => screen.select_line(press.point, SelectLine::default()),
            Behavior::Output => screen.select_output(press.point),
        }
    }

    fn repeats(&self, terminal: &Terminal, press: &Press<'_>) -> bool {
        let Some((time, previous)) = press.time.zip(self.time) else {
            return false;
        };
        if time < previous
            || time.saturating_sub(previous).min(i128::from(u64::MAX))
                > i128::from(press.repeat_interval)
        {
            return false;
        }
        let distance = ((press.xpos - self.xpos).powi(2) + (press.ypos - self.ypos).powi(2)).sqrt();
        if distance > press.max_distance {
            return false;
        }
        self.anchor(terminal).is_some()
    }

    pub fn drag(&mut self, terminal: &Terminal, drag: Drag<'_>) -> Option<Selection> {
        if self.click_count == 0 {
            return None;
        }
        let anchor = self.anchor(terminal)?;
        position(terminal.screen(), drag.point)?;
        self.dragged |= anchor != drag.point;
        self.autoscroll = if drag.ypos <= 1.0 {
            Autoscroll::Up
        } else if drag.ypos > f64::from(drag.geometry.screen_height) - 1.0 {
            Autoscroll::Down
        } else {
            Autoscroll::None
        };
        let screen = terminal.screen();
        let selected = match self.behavior {
            Behavior::Cell => cell_selection(
                screen,
                anchor,
                drag.point,
                self.xpos as u32,
                drag.xpos as u32,
                drag.rectangle,
                drag.geometry,
            ),
            Behavior::Word => {
                let start = screen.select_word_between(anchor, drag.point, drag.word_boundaries)?;
                let end = screen.select_word_between(drag.point, anchor, drag.word_boundaries)?;
                let (start, end) = if before(screen, drag.point, anchor)? {
                    (end.start, start.end)
                } else {
                    (start.start, end.end)
                };
                Some(Selection {
                    start,
                    end,
                    rectangular: false,
                })
            }
            Behavior::Line => {
                let line = screen.select_line(drag.point, SelectLine::default())?;
                let mut start =
                    screen
                        .select_line(anchor, SelectLine::default())
                        .or_else(|| {
                            screen.select_line(
                                anchor,
                                SelectLine {
                                    whitespace: None,
                                    ..SelectLine::default()
                                },
                            )
                        })?;
                if before(screen, drag.point, anchor)? {
                    start.start = line.start;
                } else {
                    start.end = line.end;
                }
                Some(start)
            }
            Behavior::Output => {
                let mut start = screen.select_output(anchor)?;
                if let Some(end) = screen.select_output(drag.point) {
                    if before(screen, drag.point, anchor)? {
                        start.start = end.start;
                    } else {
                        start.end = end.end;
                    }
                }
                Some(start)
            }
        };
        self.dragged |= self.behavior == Behavior::Cell && selected.is_some();
        selected
    }

    pub fn release(&mut self, terminal: &Terminal, point: Option<GridPoint>) {
        if self.click_count == 0 {
            return;
        }
        self.dragged |=
            point.is_none() || self.anchor(terminal).is_none() || self.anchor(terminal) != point;
        self.autoscroll = Autoscroll::None;
    }

    pub fn deep_press(
        &mut self,
        terminal: &mut Terminal,
        word_boundaries: &[char],
    ) -> Option<Selection> {
        let anchor = self.anchor(terminal)?;
        let selected = terminal.screen().select_word(anchor, word_boundaries);
        self.reset(terminal);
        self.dragged = true;
        selected
    }

    pub fn autoscroll_tick(
        &mut self,
        terminal: &mut Terminal,
        tick: AutoscrollTick<'_>,
    ) -> Option<Selection> {
        if self.click_count == 0 || self.autoscroll == Autoscroll::None {
            return None;
        }
        if self.anchor(terminal).is_none() {
            self.reset(terminal);
            return None;
        }
        let screen = terminal.screen_mut();
        screen.scroll_viewport(if self.autoscroll == Autoscroll::Up {
            1
        } else {
            -1
        });
        let [x, y] = tick.viewport;
        if x >= u32::from(terminal.cols) {
            return None;
        }
        let screen = terminal.screen();
        let top = screen.history_len().saturating_sub(screen.viewport_offset);
        let point = screen.point(top.checked_add(y as usize)?, x as usize)?;
        self.drag(
            terminal,
            Drag {
                point,
                xpos: tick.xpos,
                ypos: tick.ypos,
                rectangle: tick.rectangle,
                word_boundaries: tick.word_boundaries,
                geometry: tick.geometry,
            },
        )
    }

    fn untrack(&mut self, terminal: &mut Terminal) {
        if let Some(anchor) = self.anchor.take() {
            terminal.untrack(anchor);
        }
    }
}

fn position(screen: &Screen, point: GridPoint) -> Option<(usize, usize)> {
    let y = screen
        .all_rows()
        .position(|row| row.id == point.row && point.col < row.cells.len())?;
    Some((y, point.col))
}

fn before(screen: &Screen, left: GridPoint, right: GridPoint) -> Option<bool> {
    Some(position(screen, left)? < position(screen, right)?)
}

fn step(screen: &Screen, point: GridPoint, right: bool, rectangle: bool) -> GridPoint {
    let Some((y, x)) = position(screen, point) else {
        return point;
    };
    let width = screen.row_by_id(point.row).unwrap().cells.len();
    if right {
        if x + 1 < width {
            GridPoint {
                col: x + 1,
                ..point
            }
        } else if rectangle {
            point
        } else {
            screen.point(y + 1, 0).unwrap_or(point)
        }
    } else if x > 0 {
        GridPoint {
            col: x - 1,
            ..point
        }
    } else if rectangle || y == 0 {
        point
    } else {
        screen
            .all_rows()
            .nth(y - 1)
            .map(|row| GridPoint {
                row: row.id,
                col: row.cells.len() - 1,
            })
            .unwrap_or(point)
    }
}

fn cell_selection(
    screen: &Screen,
    anchor: GridPoint,
    point: GridPoint,
    click_x: u32,
    drag_x: u32,
    rectangle: bool,
    geometry: Geometry,
) -> Option<Selection> {
    if geometry.columns == 0 || geometry.cell_width == 0 {
        return None;
    }
    let threshold = (f64::from(geometry.cell_width) * 0.6).round() as u32;
    let max_x = geometry.columns.saturating_mul(geometry.cell_width) - 1;
    let fraction =
        |x: u32| x.saturating_sub(geometry.padding_left).min(max_x) % geometry.cell_width;
    let (click, drag) = (fraction(click_x), fraction(drag_x));
    let same = anchor == point;
    let backwards = if same || rectangle && point.col == anchor.col {
        drag < click
    } else if rectangle {
        point.col < anchor.col
    } else {
        before(screen, point, anchor)?
    };
    let include_click = if backwards {
        click >= threshold
    } else {
        click < threshold
    };
    let include_drag = if backwards {
        drag < threshold
    } else {
        drag >= threshold
    };
    let start = if include_click {
        anchor
    } else {
        step(screen, anchor, !backwards, rectangle)
    };
    let end = if include_drag {
        point
    } else {
        step(screen, point, backwards, rectangle)
    };
    if !include_click
        && (same
            || end == anchor
            || rectangle && (anchor.col == point.col || end.col == anchor.col))
        || !include_drag && (start == point || rectangle && start.col == point.col)
    {
        return None;
    }
    Some(Selection {
        start,
        end,
        rectangular: rectangle,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GEOMETRY: Geometry = Geometry {
        columns: 5,
        cell_width: 10,
        padding_left: 0,
        screen_height: 100,
    };

    fn press(point: GridPoint) -> Press<'static> {
        Press {
            time: Some(0),
            point,
            xpos: 10.0 * point.col as f64,
            ypos: 50.0,
            max_distance: 10.0,
            repeat_interval: 500,
            word_boundaries: &[' '],
            behaviors: DEFAULT_BEHAVIORS,
        }
    }

    fn drag(point: GridPoint, xpos: f64, ypos: f64) -> Drag<'static> {
        Drag {
            point,
            xpos,
            ypos,
            rectangle: false,
            word_boundaries: &[' '],
            geometry: GEOMETRY,
        }
    }

    #[test]
    fn same_cell_requires_threshold_and_clamps_nonfinite_pixels() {
        let mut terminal = Terminal::new(5, 5, 100);
        let point = terminal.screen().point(1, 1).unwrap();
        let mut gesture = SelectionGesture::default();
        assert_eq!(gesture.press(&mut terminal, press(point)), None);
        for xpos in [10.0, 15.0, f64::NAN, f64::NEG_INFINITY] {
            assert_eq!(gesture.drag(&terminal, drag(point, xpos, 50.0)), None);
            assert!(!gesture.dragged());
        }
        for xpos in [16.0, f64::INFINITY] {
            let selected = gesture.drag(&terminal, drag(point, xpos, 50.0)).unwrap();
            assert_eq!((selected.start, selected.end), (point, point));
            assert!(gesture.dragged());
        }
        assert!(terminal.screen().selection.is_none());
        gesture.deinit(&mut terminal);
    }

    #[test]
    fn repeats_preserve_anchor_and_accept_native_nan_comparisons() {
        let mut terminal = Terminal::new(5, 5, 100);
        let anchor = terminal.screen().point(0, 1).unwrap();
        let next = terminal.screen().point(0, 2).unwrap();
        let mut gesture = SelectionGesture::default();
        gesture.press(&mut terminal, press(anchor));
        gesture.release(&terminal, Some(anchor));
        gesture.press(
            &mut terminal,
            Press {
                time: Some(500),
                ..press(next)
            },
        );
        assert_eq!(gesture.click_count(), 2);
        assert_eq!(gesture.anchor(&terminal), Some(anchor));
        gesture.press(
            &mut terminal,
            Press {
                time: Some(1_000),
                xpos: f64::NAN,
                ..press(next)
            },
        );
        assert_eq!(gesture.click_count(), 3);
        gesture.press(
            &mut terminal,
            Press {
                time: Some(1_001),
                xpos: 100.0,
                max_distance: f64::NAN,
                ..press(next)
            },
        );
        assert_eq!(gesture.click_count(), 3);
        gesture.press(
            &mut terminal,
            Press {
                time: None,
                ..press(next)
            },
        );
        assert_eq!(gesture.click_count(), 1);
        assert_eq!(gesture.anchor(&terminal), Some(next));
        gesture.deinit(&mut terminal);
    }

    #[test]
    fn reset_and_recycled_screens_cannot_reuse_anchors() {
        let mut terminal = Terminal::new(5, 5, 100);
        let mut gesture = SelectionGesture::default();
        for reset in [true, false] {
            if !reset {
                terminal.feed(b"\x1b[?1049h");
            }
            let point = terminal.screen().point(0, 1).unwrap();
            gesture.press(&mut terminal, press(point));
            if reset {
                terminal.reset();
            } else {
                terminal.feed(b"\x1b[?1049l");
                terminal.alternate = None;
                terminal.feed(b"\x1b[?1049h");
            }
            assert!(gesture.has_anchor());
            assert!(gesture.anchor(&terminal).is_none());
            let point = terminal.screen().point(0, 1).unwrap();
            assert_eq!(gesture.drag(&terminal, drag(point, 20.0, 0.0)), None);
            gesture.press(&mut terminal, press(point));
            assert_eq!(gesture.click_count(), 1);
            assert_eq!(gesture.anchor(&terminal), Some(point));
        }
        gesture.reset(&mut terminal);
        assert!(!gesture.has_anchor());
    }

    #[test]
    fn deep_press_consumes_even_an_empty_word() {
        let mut terminal = Terminal::new(20, 5, 100);
        terminal.feed(b"alpha beta");
        let mut gesture = SelectionGesture::default();
        let anchor = terminal.screen().point(0, 1).unwrap();
        gesture.press(&mut terminal, press(anchor));
        let word = gesture.deep_press(&mut terminal, &[' ']).unwrap();
        assert_eq!(word.start, terminal.screen().point(0, 0).unwrap());
        assert_eq!(word.end, terminal.screen().point(0, 4).unwrap());
        let blank = terminal.screen().point(1, 1).unwrap();
        gesture.press(&mut terminal, press(blank));
        assert_eq!(gesture.deep_press(&mut terminal, &[' ']), None);
        assert_eq!(gesture.click_count(), 0);
        assert!(gesture.dragged());
        assert!(!gesture.has_anchor());
        assert_eq!(gesture.drag(&terminal, drag(blank, 20.0, 0.0)), None);
    }

    #[test]
    fn autoscroll_resolves_after_scrolling_and_cancels_invalid_anchor() {
        let mut terminal = Terminal::new(5, 3, 100);
        terminal.feed(b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive");
        terminal.screen_mut().scroll_viewport(2);
        let point = terminal.screen().point(1, 1).unwrap();
        let mut gesture = SelectionGesture::default();
        gesture.press(&mut terminal, press(point));
        gesture.drag(&terminal, drag(point, 29.0, 100.0));
        let tick = || AutoscrollTick {
            viewport: [2, 2],
            xpos: 29.0,
            ypos: 100.0,
            rectangle: false,
            word_boundaries: &[' '],
            geometry: GEOMETRY,
        };
        let selected = gesture.autoscroll_tick(&mut terminal, tick()).unwrap();
        assert_eq!(terminal.screen().viewport_offset, 1);
        let row = terminal.screen().history_len() - 1 + 2;
        assert_eq!(selected.end, terminal.screen().point(row, 2).unwrap());
        terminal.reset();
        assert_eq!(gesture.autoscroll_tick(&mut terminal, tick()), None);
        assert_eq!(gesture.autoscroll(), Autoscroll::None);
        assert_eq!(gesture.click_count(), 0);
        assert!(!gesture.has_anchor());
    }
}
