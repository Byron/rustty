//! Direct selection/search/tracked-reference APIs, independent of snapshots.
use rustty_vt::search::{Direction, SelectScroll, Status, TerminalSearch, Tick};
use rustty_vt::selection::{
    Adjustment, DEFAULT_LINE_WHITESPACE, DEFAULT_WORD_BOUNDARIES, SelectLine,
};
use rustty_vt::selection_gesture::{
    AutoscrollTick, Behavior, DEFAULT_BEHAVIORS, Drag, Geometry, Press, SelectionGesture,
};
use rustty_vt::{GridPoint, Screen, ScrollbackLimits, Selection, Terminal, TrackedPoint};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FormatOptions {
    emit: rustty_vt::formatter::Format,
    unwrap: Option<bool>,
    trim: Option<bool>,
    background: Option<[u8; 3]>,
    foreground: Option<[u8; 3]>,
    palette: Option<Vec<[u8; 3]>>,
    codepoint_map: Vec<FormatMap>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FormatMap {
    range: [u32; 2],
    replacement: FormatReplacement,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum FormatReplacement {
    Codepoint(u32),
    String(String),
}

impl FormatOptions {
    fn maps(&self) -> Result<Vec<rustty_vt::formatter::CodepointMap<'_>>, &'static str> {
        use rustty_vt::formatter::{CodepointMap, Replacement};
        let cp = |value| char::from_u32(value).ok_or("InvalidCodepoint");
        self.codepoint_map
            .iter()
            .map(|rule| {
                Ok(CodepointMap {
                    range: [cp(rule.range[0])?, cp(rule.range[1])?],
                    replacement: match &rule.replacement {
                        FormatReplacement::Codepoint(value) => Replacement::Codepoint(cp(*value)?),
                        FormatReplacement::String(value) => Replacement::String(value),
                    },
                })
            })
            .collect()
    }

    fn options<'a>(
        &'a self,
        selection: bool,
        maps: &'a [rustty_vt::formatter::CodepointMap<'a>],
    ) -> Result<rustty_vt::formatter::Options<'a>, &'static str> {
        Ok(rustty_vt::formatter::Options {
            emit: self.emit,
            unwrap: self.unwrap.unwrap_or(selection),
            trim: self.trim.unwrap_or(true),
            background: self.background,
            foreground: self.foreground,
            palette: self
                .palette
                .as_deref()
                .map(TryInto::try_into)
                .transpose()
                .map_err(|_| "InvalidFormatPalette")?,
            codepoint_map: maps,
        })
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Point {
    tag: String,
    x: u16,
    y: u32,
}

impl Default for Point {
    fn default() -> Self {
        Self {
            tag: "active".into(),
            x: 0,
            y: 0,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Operation {
    action: String,
    id: u32,
    point: Point,
    start: Point,
    end: Point,
    rectangle: bool,
    needle: String,
    active_dirty: Option<bool>,
    scroll: Option<bool>,
    delta: i32,
    lines: Option<usize>,
    bytes: Option<usize>,
    boundary_codepoints: Option<Vec<u32>>,
    whitespace: Option<Vec<u32>>,
    trim_line: Option<bool>,
    semantic_prompt_boundary: Option<bool>,
    adjustment: Option<Adjustment>,
    format: Option<FormatOptions>,
    format_map: bool,
    format_content: Option<String>,
    screen_extra: Option<rustty_vt::formatter::ScreenExtra>,
    terminal_extra: Option<rustty_vt::formatter::TerminalExtra>,
    gesture: Option<GestureOptions>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct GestureOptions {
    time: Option<i64>,
    xpos: f64,
    ypos: f64,
    max_distance: f64,
    repeat_interval: u64,
    behaviors: [Behavior; 3],
    geometry: Option<Geometry>,
}
impl Default for GestureOptions {
    fn default() -> Self {
        Self {
            time: None,
            xpos: 0.0,
            ypos: 0.0,
            max_distance: 10.0,
            repeat_interval: 500_000_000,
            behaviors: DEFAULT_BEHAVIORS,
            geometry: None,
        }
    }
}

struct Handle {
    id: u32,
    alternate: bool,
    point: TrackedPoint,
}

#[derive(Default)]
pub struct Context {
    handles: Vec<Handle>,
    search: TerminalSearch,
    gesture: SelectionGesture,
    gesture_used: bool,
}

impl Context {
    pub fn has_handles(&self) -> bool {
        !self.handles.is_empty() || !self.search.needle().is_empty() || self.gesture.has_anchor()
    }

    pub fn run(&mut self, terminal: &mut Terminal, op: &Operation) -> Result<Value, &'static str> {
        let mut status = "ok";
        let mut matches = None;
        let mut search_needle = None;
        let mut search_state = None;
        let mut search_tick = None;
        let mut selection_result = None;
        let columns = terminal.cols;
        match op.action.as_str() {
            "observe" => {}
            "format_selection" => {
                if terminal.screen().selection.is_none() {
                    status = "no_value";
                }
            }
            "format_screen" | "format_terminal" => {
                if op.format_content.as_deref() == Some("selection")
                    && terminal.screen().selection.is_none()
                {
                    status = "no_value";
                }
            }
            "select" => {
                let screen = terminal.screen_mut();
                match (
                    point(screen, &op.start, columns)?,
                    point(screen, &op.end, columns)?,
                ) {
                    (Some(start), Some(end)) => {
                        screen.selection = Some(Selection {
                            start,
                            end,
                            rectangular: op.rectangle,
                        })
                    }
                    _ => status = "invalid",
                }
            }
            "clear_selection" => terminal.screen_mut().selection = None,
            "adjust_selection" => {
                let adjustment = op.adjustment.ok_or("InvalidAdjustment")?;
                let screen = terminal.screen_mut();
                if let Some(mut selection) = screen.selection {
                    selection.adjust(screen, adjustment);
                    screen.selection = Some(selection);
                } else {
                    status = "no_value";
                }
            }
            "select_word"
            | "select_word_between"
            | "select_line"
            | "select_all"
            | "select_output" => {
                let screen = terminal.screen();
                let boundaries = codepoints(op.boundary_codepoints.as_deref())?;
                let boundaries = boundaries.as_deref().unwrap_or(DEFAULT_WORD_BOUNDARIES);
                let whitespace = codepoints(op.whitespace.as_deref())?;
                let selected = if op.action == "select_all" {
                    screen.select_all()
                } else if op.action == "select_word_between" {
                    match (
                        point(screen, &op.start, columns)?,
                        point(screen, &op.end, columns)?,
                    ) {
                        (Some(start), Some(end)) => {
                            screen.select_word_between(start, end, boundaries)
                        }
                        _ => {
                            status = "invalid";
                            None
                        }
                    }
                } else if let Some(point) = point(screen, &op.point, columns)? {
                    if op.action == "select_word" {
                        screen.select_word(point, boundaries)
                    } else if op.action == "select_output" {
                        screen.select_output(point)
                    } else {
                        screen.select_line(
                            point,
                            SelectLine {
                                whitespace: op.trim_line.unwrap_or(true).then_some(
                                    whitespace.as_deref().unwrap_or(DEFAULT_LINE_WHITESPACE),
                                ),
                                semantic_prompt_boundary: op
                                    .semantic_prompt_boundary
                                    .unwrap_or(true),
                            },
                        )
                    }
                } else {
                    status = "invalid";
                    None
                };
                selection_result = selected.map(|selection| json!({
                    "start": location(screen, selection.start), "end": location(screen, selection.end),
                    "rectangle": selection.rectangular,
                }));
                if selected.is_none() && status == "ok" {
                    status = "no_value";
                }
            }
            action if action.starts_with("gesture_") => {
                self.gesture_used = true;
                let defaults = GestureOptions::default();
                let options = op.gesture.as_ref().unwrap_or(&defaults);
                let boundaries = codepoints(op.boundary_codepoints.as_deref())?;
                let boundaries = boundaries.as_deref().unwrap_or(DEFAULT_WORD_BOUNDARIES);
                let geometry = options.geometry.unwrap_or(Geometry {
                    columns: u32::from(columns),
                    cell_width: 10,
                    padding_left: 5,
                    screen_height: 100,
                });
                let point = point(terminal.screen(), &op.point, columns)?;
                let selected = match action {
                    "gesture_press" => {
                        if let Some(point) = point {
                            self.gesture.press(
                                terminal,
                                Press {
                                    point,
                                    time: options.time.map(i128::from),
                                    xpos: options.xpos,
                                    ypos: options.ypos,
                                    max_distance: options.max_distance,
                                    repeat_interval: options.repeat_interval,
                                    word_boundaries: boundaries,
                                    behaviors: options.behaviors,
                                },
                            )
                        } else {
                            status = "invalid";
                            None
                        }
                    }
                    "gesture_drag" => {
                        if let Some(point) = point {
                            self.gesture.drag(
                                terminal,
                                Drag {
                                    point,
                                    xpos: options.xpos,
                                    ypos: options.ypos,
                                    rectangle: op.rectangle,
                                    word_boundaries: boundaries,
                                    geometry,
                                },
                            )
                        } else {
                            status = "invalid";
                            None
                        }
                    }
                    "gesture_release" => {
                        self.gesture.release(terminal, point);
                        None
                    }
                    "gesture_reset" => {
                        self.gesture.reset(terminal);
                        None
                    }
                    "gesture_deep_press" => self.gesture.deep_press(terminal, boundaries),
                    "gesture_autoscroll" => self.gesture.autoscroll_tick(
                        terminal,
                        AutoscrollTick {
                            viewport: [u32::from(op.point.x), op.point.y],
                            xpos: options.xpos,
                            ypos: options.ypos,
                            rectangle: op.rectangle,
                            word_boundaries: boundaries,
                            geometry,
                        },
                    ),
                    _ => return Err("UnsupportedGridAction"),
                };
                selection_result = selected.map(|selection| json!({
                    "start": location(terminal.screen(), selection.start), "end": location(terminal.screen(), selection.end),
                    "rectangle": selection.rectangular,
                }));
            }
            "track" => {
                if self.handles.iter().any(|handle| handle.id == op.id) {
                    status = "invalid";
                } else if let Some(point) = point(terminal.screen(), &op.point, columns)? {
                    let alternate = terminal.is_alternate_screen();
                    self.handles.push(Handle {
                        id: op.id,
                        alternate,
                        point: terminal.screen_mut().track(point),
                    });
                } else {
                    status = "invalid";
                }
            }
            "untrack" => {
                if let Some(index) = self.handles.iter().position(|handle| handle.id == op.id) {
                    let handle = &self.handles[index];
                    terminal.untrack(handle.point);
                    self.handles.remove(index);
                } else {
                    status = "invalid";
                }
            }
            "viewport" => terminal.screen_mut().scroll_viewport(-(op.delta as isize)),
            "limits" => terminal.set_limits(ScrollbackLimits {
                lines: op.lines,
                bytes: op.bytes,
            }),
            "search" => {
                let needle = super::unhex(&op.needle)?;
                // Native search exposes a literal needle. Retain the Rust
                // API's result order so ordering differences cannot disappear.
                matches = Some(
                    terminal
                        .screen()
                        .search_literal(&needle)
                        .into_iter()
                        .map(|value| {
                            json!({"start": location(terminal.screen(), value.start),
                           "end": location(terminal.screen(), value.end)})
                        })
                        .collect::<Vec<_>>(),
                );
            }
            "search_needle" => {
                self.search.set_needle(&super::unhex(&op.needle)?);
                search_needle = Some(super::hex(self.search.needle()));
            }
            "search_feed" => {
                self.search.feed(terminal, op.active_dirty.unwrap_or(true));
            }
            "search_viewport" => {
                matches = Some(
                    self.search
                        .viewport_matches()
                        .iter()
                        .map(|value| {
                            json!({"start": location(terminal.screen(), value.start),
                        "end": location(terminal.screen(), value.end)})
                        })
                        .collect::<Vec<_>>(),
                );
            }
            "search_status" | "search_selected" => {}
            "search_tick" => {
                search_tick = Some(match self.search.tick() {
                    Tick::Progress => "progress",
                    Tick::Blocked => "blocked",
                    Tick::Complete => "complete",
                });
            }
            "search_run" => self.search.run(terminal),
            "search_matches" | "search_match" => {
                let screen = if self.search.is_alternate_screen() {
                    terminal.alternate_screen()
                } else {
                    Some(terminal.primary_screen())
                };
                let found: Vec<_> = if op.action == "search_match" {
                    self.search.match_at(op.id as usize).into_iter().collect()
                } else {
                    self.search.matches().collect()
                };
                if op.action == "search_match" && found.is_empty() {
                    status = "no_value";
                }
                matches = Some(
                    found
                        .into_iter()
                        .map(|value| {
                            json!({
                                "start": screen.map(|screen| location(screen, value.start)),
                                "end": screen.map(|screen| location(screen, value.end)),
                            })
                        })
                        .collect::<Vec<_>>(),
                );
            }
            "search_next" | "search_prev" => {
                let direction = if op.action == "search_next" {
                    Direction::Next
                } else {
                    Direction::Previous
                };
                let scroll = if op.scroll.unwrap_or(true) {
                    SelectScroll::IfNeeded
                } else {
                    SelectScroll::None
                };
                if !self.search.select(terminal, direction, scroll) {
                    status = "no_value";
                }
            }
            _ => return Err("UnsupportedGridAction"),
        }
        if matches!(
            op.action.as_str(),
            "search_status"
                | "search_selected"
                | "search_tick"
                | "search_run"
                | "search_matches"
                | "search_match"
                | "search_next"
                | "search_prev"
        ) {
            let screen = if self.search.is_alternate_screen() {
                terminal.alternate_screen()
            } else {
                Some(terminal.primary_screen())
            };
            let selected = self.search.selected_match().map(|value| {
                json!({
                    "start": screen.map(|screen| location(screen, value.start)),
                    "end": screen.map(|screen| location(screen, value.end)),
                })
            });
            search_state = Some(json!({
                "status": match self.search.status() { Status::Running => "running", Status::FeedRequired => "feed_required", Status::Complete => "complete" },
                "tick": search_tick, "total": self.search.total_matches(),
                "selected_index": self.search.selected_index(), "selected_match": selected,
                "screen": if self.search.is_alternate_screen() { "alternate" } else { "primary" },
            }));
        }
        let mut format_map = None;
        let formatted = if matches!(
            op.action.as_str(),
            "format_selection" | "format_screen" | "format_terminal"
        ) {
            use rustty_vt::formatter::Content;
            let selection = op.action == "format_selection";
            let tag = if selection {
                "selection"
            } else {
                op.format_content.as_deref().unwrap_or("all")
            };
            let content = match tag {
                "all" => Some(Content::All),
                "none" => Some(Content::None),
                "selection" => terminal.screen().selection.map(Content::Selection),
                _ => return Err("InvalidFormatContent"),
            };
            if let Some(content) = content {
                let default = FormatOptions::default();
                let format = op.format.as_ref().unwrap_or(&default);
                let maps = format.maps()?;
                let options = format.options(selection, &maps)?;
                let mut mapped = |(bytes, map): (Vec<u8>, rustty_vt::formatter::ByteMap<'_>)| {
                    assert_eq!(bytes.len(), map.len());
                    format_map = Some(
                        (0..map.len())
                            .map(|offset| {
                                let point = map.get(offset).unwrap();
                                [point.page, point.x as usize, point.y as usize]
                            })
                            .collect::<Vec<_>>(),
                    );
                    bytes
                };
                let bytes = if op.action != "format_screen" {
                    let mut formatter = terminal.formatter(options.emit);
                    formatter.options = options;
                    formatter.content = content;
                    if !selection && let Some(extra) = op.terminal_extra {
                        formatter.extra = extra;
                    }
                    if op.format_map {
                        formatter.format_with_map().map(&mut mapped)
                    } else {
                        formatter.format()
                    }
                } else {
                    let mut formatter = terminal.screen().formatter(options.emit);
                    formatter.options = options;
                    formatter.content = content;
                    if let Some(extra) = op.screen_extra {
                        formatter.extra = extra;
                    }
                    if op.format_map {
                        formatter.format_with_map().map(&mut mapped)
                    } else {
                        formatter.format()
                    }
                };
                bytes.map(|bytes| super::hex(&bytes))
            } else {
                None
            }
        } else {
            None
        };
        let screen = terminal.screen();
        let selection = screen.selection.map(|selection| {
            json!({"start": location(screen, selection.start), "end": location(screen, selection.end),
                   "rectangle": selection.rectangular,
                   "text": screen.selection_text().map(|text| super::hex(text.as_bytes()))})
        });
        let handles = self.handles.iter().map(|handle| {
            let screen = if handle.alternate { terminal.alternate_screen() } else { Some(terminal.primary_screen()) };
            let point = screen.and_then(|screen| screen.resolve(handle.point));
            let value = screen.zip(point).map(|(screen, point)| {
                let text = screen.row_by_id(point.row).and_then(|row| row.cells.get(point.col))
                    .map(|cell| cell.text.chars().map(u32::from).collect::<Vec<_>>());
                json!({"location": location(screen, point), "text": text})
            });
            json!({"id": handle.id, "screen": if handle.alternate { "alternate" } else { "primary" }, "value": value})
        }).collect::<Vec<_>>();
        Ok(
            json!({"action": op.action, "status": status, "matches": matches, "search_needle": search_needle,
            "search_state": search_state,
            "formatted": formatted,
            "format_map": format_map,
            "active_screen": if terminal.is_alternate_screen() { "alternate" } else { "primary" },
            "viewport_top": [screen.viewport_top().col, screen.history.len().saturating_sub(screen.viewport_offset)],
            "selection": selection, "selection_result": selection_result, "tracked": handles,
            "gesture": self.gesture_used.then(|| json!({
                "click_count": self.gesture.click_count(), "behavior": self.gesture.behavior(),
                "dragged": self.gesture.dragged(), "autoscroll": self.gesture.autoscroll(),
                "anchor_retained": self.gesture.has_anchor(),
                "anchor_valid": self.gesture.anchor(terminal).is_some(),
                "anchor": self.gesture.anchor(terminal).map(|point| location(screen, point)),
            }))}),
        )
    }
}

fn codepoints(values: Option<&[u32]>) -> Result<Option<Vec<char>>, &'static str> {
    values
        .map(|values| {
            values
                .iter()
                .map(|&value| char::from_u32(value).ok_or("InvalidCodepoint"))
                .collect()
        })
        .transpose()
}

fn point(screen: &Screen, point: &Point, columns: u16) -> Result<Option<GridPoint>, &'static str> {
    // Native PageList.pin validates the logical width before resolving a page.
    // A selector may still return an endpoint beyond it on a wider stored row.
    if point.x >= columns {
        return Ok(None);
    }
    let base = match point.tag.as_str() {
        "active" => screen.history.len(),
        "viewport" => screen.history.len().saturating_sub(screen.viewport_offset),
        "screen" | "history" => 0,
        _ => return Err("InvalidPointTag"),
    };
    Ok(base
        .checked_add(point.y as usize)
        .and_then(|row| screen.point(row, usize::from(point.x))))
}

fn location(screen: &Screen, point: GridPoint) -> Value {
    let Some(row) = screen.all_rows().position(|row| row.id == point.row) else {
        return Value::Null;
    };
    let relative = |base| row.checked_sub(base).map(|row| [point.col, row]);
    json!({"screen": [point.col, row], "history": [point.col, row],
        "active": relative(screen.history.len()),
        "viewport": relative(screen.history.len().saturating_sub(screen.viewport_offset))})
}
