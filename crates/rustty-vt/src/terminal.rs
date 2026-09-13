use crate::modes::Modes;
use crate::screen::*;
use crate::unicode::{self, properties};
use base64::Engine;
use rustty_parser::{Event, Parser};

/// Host actions are returned in input order. The terminal never accesses the OS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Write(Vec<u8>),
    Title(String),
    WorkingDirectory(String),
    Bell,
    Clipboard {
        selection: String,
        data: Option<Vec<u8>>,
    },
    Notification {
        title: Vec<u8>,
        body: Vec<u8>,
    },
    CommandStart,
    CommandEnd {
        exit_code: Option<i32>,
    },
    Progress {
        state: u8,
        value: Option<u8>,
    },
    UnknownSequence(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Margins {
    pub top: usize,
    pub bottom: usize,
    pub left: usize,
    pub right: usize,
}

#[derive(Clone, Debug)]
pub struct Terminal {
    pub(crate) metadata: crate::snapshot::TerminalMetadata,
    pub cols: u16,
    pub rows: u16,
    pub width_px: u32,
    pub height_px: u32,
    pub modes: Modes,
    pub margins: Margins,
    pub palette: Vec<[u8; 3]>,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub cursor_color: Option<[u8; 3]>,
    /// Host's actual TERM name, used for XTGETTCAP TN. Unset or names longer
    /// than 128 bytes leave TN unanswered. This host setting is not persisted.
    pub terminfo_name: Option<String>,
    pub title: String,
    pub working_directory: String,
    pub generation: u64,
    pub modify_other_keys: bool,
    pub mouse_mode: u16,
    pub mouse_format: u16,
    pub(crate) primary: Screen,
    pub(crate) alternate: Option<Screen>,
    pub(crate) alternate_active: bool,
    pub(crate) parser: Parser,
    pub(crate) tabstops: Vec<bool>,
    pub(crate) previous_char: Option<char>,
    grapheme_state: u8,
    pub(crate) status_display: bool,
    dcs: Vec<u8>,
    dcs_header: (Vec<u8>, Vec<u16>, u8),
    apc: Vec<u8>,
    string_overflow: bool,
}

impl Terminal {
    /// Dimensions are clamped to one cell; `resize` rejects zero dimensions.
    pub fn new(cols: u16, rows: u16, scrollback_limit: usize) -> Self {
        Self::with_limits(
            cols,
            rows,
            ScrollbackLimits {
                bytes: None,
                lines: Some(scrollback_limit),
            },
        )
    }

    pub fn with_limits(cols: u16, rows: u16, limits: ScrollbackLimits) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            metadata: crate::snapshot::TerminalMetadata::default(),
            cols,
            rows,
            width_px: 0,
            height_px: 0,
            modes: Modes::default(),
            margins: Margins {
                top: 0,
                bottom: usize::from(rows) - 1,
                left: 0,
                right: usize::from(cols) - 1,
            },
            palette: default_palette(),
            foreground: [255; 3],
            background: [0; 3],
            cursor_color: None,
            terminfo_name: None,
            title: String::new(),
            working_directory: String::new(),
            generation: 0,
            modify_other_keys: false,
            mouse_mode: 0,
            mouse_format: 0,
            primary: Screen::new(cols.into(), rows.into(), limits),
            alternate: None,
            alternate_active: false,
            parser: Parser::new(),
            tabstops: (0..cols).map(|i| i > 0 && i % 8 == 0).collect(),
            previous_char: None,
            grapheme_state: 0,
            status_display: false,
            dcs: Vec::new(),
            dcs_header: (Vec::new(), Vec::new(), 0),
            apc: Vec::new(),
            string_overflow: false,
        }
    }

    pub fn screen(&self) -> &Screen {
        if self.alternate_active {
            self.alternate.as_ref().unwrap()
        } else {
            &self.primary
        }
    }
    pub fn screen_mut(&mut self) -> &mut Screen {
        if self.alternate_active {
            self.alternate.as_mut().unwrap()
        } else {
            &mut self.primary
        }
    }
    pub fn is_alternate_screen(&self) -> bool {
        self.alternate_active
    }
    pub fn parser(&self) -> &Parser {
        &self.parser
    }

    pub fn primary_screen(&self) -> &Screen {
        &self.primary
    }
    pub fn alternate_screen(&self) -> Option<&Screen> {
        self.alternate.as_ref()
    }
    pub fn tabstops(&self) -> &[bool] {
        &self.tabstops
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Effect> {
        let mut effects = Vec::new();
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(bytes, |event| self.handle(event, &mut effects));
        self.parser = parser;
        // Active rows can grow allocations without scrolling (graphemes and
        // hyperlinks), so reconcile the byte budget after the complete update.
        self.primary.enforce_limits();
        effects
    }

    fn ensure_row_cells(&mut self, row: usize, end: usize) {
        let columns = usize::from(self.cols);
        let cells = &mut self.screen_mut().rows[row].cells;
        if cells.len() < end.min(columns) {
            // A partial reflow can leave a physical row narrower than the
            // logical screen. Keep it intact until an edit reaches past it.
            cells.resize(columns, Cell::default());
        }
    }

    fn clamp_cursor(&mut self) {
        let columns = usize::from(self.cols);
        let screen = self.screen_mut();
        screen.cursor.col = screen
            .cursor
            .col
            .min(columns - 1)
            .min(screen.rows[screen.cursor.row].cells.len() - 1);
    }

    pub fn limits(&self) -> ScrollbackLimits {
        self.primary.limits
    }

    /// Update both policies and prune the oldest history immediately.
    pub fn set_limits(&mut self, limits: ScrollbackLimits) {
        self.primary.set_limits(limits);
        self.changed();
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        self.modes.set(true, 2026, false);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.primary
            .resize(cols.into(), rows.into(), self.modes.dec(7));
        if let Some(alt) = &mut self.alternate {
            alt.resize(cols.into(), rows.into(), false);
        }
        if cols != self.cols {
            self.tabstops = (0..cols).map(|i| i > 0 && i % 8 == 0).collect();
        }
        self.cols = cols;
        self.rows = rows;
        self.reset_margins();
        self.changed();
    }

    pub fn set_pixel_size(&mut self, width: u32, height: u32) {
        self.width_px = width;
        self.height_px = height;
    }

    pub fn reset(&mut self) {
        let limits = self.primary.limits;
        let primary_identity = self.primary.metadata.identity;
        let reflow_generation = self.primary.metadata.reflow_generation;
        let terminfo_name = self.terminfo_name.take();
        let (foreground, background, cursor, palette) = (
            self.foreground,
            self.background,
            self.cursor_color,
            self.palette.clone(),
        );
        let (width_px, height_px) = (self.width_px, self.height_px);
        let mut metadata = self.metadata.clone();
        metadata.cursor_is_default = true;
        metadata.shell_redraw = 0;
        metadata.mouse_shift_capture = None;
        metadata.password_input = false;
        metadata.title_raw = None;
        metadata.pwd_raw = None;
        let mut modes = self.modes.clone();
        modes.reset();
        *self = Self::with_limits(self.cols, self.rows, limits);
        // RIS resets the existing primary screen. A streaming restore may
        // still deliver older history into that same screen afterward.
        self.primary.metadata.identity = primary_identity;
        self.primary.metadata.reflow_generation = reflow_generation;
        self.terminfo_name = terminfo_name;
        self.width_px = width_px;
        self.height_px = height_px;
        self.metadata = metadata;
        self.modes = modes;
        self.foreground = foreground;
        self.background = background;
        self.cursor_color = cursor;
        self.palette = palette;
        self.set_cursor_style(0);
        self.changed();
    }

    /// Update configured colors while preserving application overrides.
    /// Palette entries beyond the supplied slice retain their defaults.
    pub fn set_default_colors(
        &mut self,
        foreground: [u8; 3],
        background: [u8; 3],
        cursor: Option<[u8; 3]>,
        palette: &[[u8; 3]],
    ) {
        for (entry, value) in
            self.metadata
                .colors
                .iter_mut()
                .zip([Some(background), Some(foreground), cursor])
        {
            entry[0] = value;
        }
        self.background = self.metadata.colors[0][1].unwrap_or(background);
        self.foreground = self.metadata.colors[1][1].unwrap_or(foreground);
        self.cursor_color = self.metadata.colors[2][1].or(cursor);
        for (i, &color) in palette.iter().take(256).enumerate() {
            self.metadata.original_palette[i] = color;
            if self.metadata.palette_overrides[i / 8] & (1 << (i % 8)) == 0 {
                self.palette[i] = color;
            }
        }
        self.changed();
    }

    /// Configuration changes apply immediately while the cursor follows its
    /// default; an application-selected shape persists until CSI 0 SP q/reset.
    pub fn set_default_cursor(&mut self, shape: CursorShape, blink: Option<bool>) {
        self.metadata.cursor_default_shape = crate::snapshot::cursor_shape(shape);
        self.metadata.cursor_default_blink = blink;
        if self.metadata.cursor_is_default {
            self.set_cursor_style(0);
        }
    }

    fn set_cursor_style(&mut self, value: u16) {
        self.metadata.cursor_is_default = value == 0;
        let shape = match value {
            0 => crate::snapshot::decode_shape(self.metadata.cursor_default_shape),
            3 | 4 => CursorShape::Underline,
            5 | 6 => CursorShape::Bar,
            _ => CursorShape::Block,
        };
        let blink = if value == 0 {
            self.metadata.cursor_default_blink.unwrap_or(true)
        } else {
            matches!(value, 1 | 3 | 5)
        };
        self.screen_mut().cursor.shape = shape;
        self.set_mode(true, 12, blink);
        self.changed();
    }

    fn end_hyperlink(&mut self) {
        let cursor = &mut self.screen_mut().cursor;
        cursor.hyperlink = None;
        cursor.hyperlink_id = None;
        cursor.hyperlink_raw = None;
    }

    pub fn plain_text(&self) -> String {
        let mut result = String::new();
        for row in self.screen().viewport() {
            result.push_str(&row.text());
            if !row.wrapped {
                result.push('\n');
            }
        }
        result.truncate(result.trim_end_matches('\n').len());
        result
    }

    pub fn resolve_color(&self, color: Color, default: [u8; 3]) -> [u8; 3] {
        match color {
            Color::Default => default,
            Color::Indexed(i) => self.palette[i as usize],
            Color::Rgb(r, g, b) => [r, g, b],
        }
    }

    fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
    fn reset_margins(&mut self) {
        self.margins = Margins {
            top: 0,
            bottom: self.rows as usize - 1,
            left: 0,
            right: self.cols as usize - 1,
        };
    }

    fn handle(&mut self, event: Event<'_>, effects: &mut Vec<Effect>) {
        match event {
            Event::Print(cp) => self.print(cp),
            Event::Execute(byte) => match byte {
                0x05 => {}
                0x07 => effects.push(Effect::Bell),
                0x08 => self.cursor_left(1),
                0x09 => self.tab(1, false),
                0x0a..=0x0c => {
                    self.index();
                    if self.modes.get(false, 20) {
                        self.carriage_return();
                    }
                }
                0x0d => self.carriage_return(),
                0x0e => self.screen_mut().charset.gl = 1,
                0x0f => self.screen_mut().charset.gl = 0,
                _ => {}
            },
            Event::Esc {
                intermediates,
                final_byte,
            } => self.esc(intermediates, final_byte, effects),
            Event::Csi {
                intermediates,
                params,
                colon_separators,
                final_byte,
            } => self.csi(intermediates, params, colon_separators, final_byte, effects),
            Event::Osc {
                data,
                terminated_by_bell,
            } => self.osc(data, terminated_by_bell, effects),
            Event::DcsHook {
                intermediates,
                params,
                final_byte,
            } => {
                self.dcs.clear();
                self.string_overflow = false;
                self.dcs_header = (intermediates.to_vec(), params.to_vec(), final_byte);
            }
            Event::DcsPut(byte) => {
                let limit = if self.dcs_header.0 == b"$" && self.dcs_header.2 == b'q' {
                    2
                } else {
                    1024 * 1024
                };
                if self.dcs.len() < limit {
                    self.dcs.push(byte);
                } else {
                    self.string_overflow = true;
                }
            }
            Event::DcsUnhook => self.dcs_end(effects),
            Event::ApcStart => {
                self.apc.clear();
                self.string_overflow = false;
            }
            Event::ApcPut(byte) => {
                if self.apc.len() < 64 * 1024 * 1024 {
                    self.apc.push(byte);
                } else {
                    self.string_overflow = true;
                }
            }
            Event::ApcEnd => {
                let mut apc = std::mem::take(&mut self.apc);
                if !self.string_overflow {
                    self.graphics_command(&apc, effects);
                }
                apc.clear();
                self.apc = apc;
            }
            Event::OscOverflow => {
                effects.push(Effect::UnknownSequence("OSC exceeded capture limit".into()))
            }
        }
    }

    pub fn print(&mut self, mut cp: char) {
        self.clamp_cursor();
        if self.status_display {
            return;
        }
        let charset = {
            let cs = &mut self.screen_mut().charset;
            let slot = cs.single.take().unwrap_or(cs.gl);
            cs.slots[slot]
        };
        if cp as u32 <= 255 {
            cp = map_charset(cp, charset);
        }
        let current = self.screen().cursor.clone();
        let right = if current.col > self.margins.right {
            self.cols as usize - 1
        } else {
            self.margins.right
        };
        let previous_col = if current.pending_wrap && self.modes.dec(7) {
            Some(current.col)
        } else if !self.modes.dec(7)
            && current.col == right
            && !self.screen().rows[current.row].cells[right].text.is_empty()
        {
            Some(right)
        } else {
            current.col.checked_sub(1)
        };
        let previous_col = previous_col.map(|col| {
            if self.screen().rows[current.row].cells[col].width == 0 {
                col.saturating_sub(1)
            } else {
                col
            }
        });

        if cp as u32 > 255
            && self.modes.dec(2027)
            && current.col > 0
            && let Some(col) = previous_col
        {
            let prev = self.screen().rows[current.row].cells[col].clone();
            if let Some(last) = prev.text.chars().last() {
                let old_state = self.grapheme_state;
                if !unicode::grapheme_break(last, cp, &mut self.grapheme_state) {
                    let mut width = prev.width;
                    if matches!(cp, '\u{fe0f}' | '\u{fe0e}') {
                        if !properties(last).emoji_vs_base {
                            self.grapheme_state = old_state;
                            return;
                        }
                        width = if cp == '\u{fe0f}' { 2 } else { 1 };
                    } else if !properties(cp).zero_in_grapheme {
                        width = 2;
                    }
                    self.append_grapheme(col, cp, width, right);
                    return;
                }
            }
        }
        let width = if cp as u32 <= 255 {
            1
        } else {
            unicode::codepoint_width(cp)
        };
        if width == 0 {
            if self.modes.dec(2027) {
                return;
            }
            if let Some(col) = previous_col {
                let previous = &self.screen().rows[current.row].cells[col];
                if previous.text.is_empty() {
                    return;
                }
                if matches!(cp, '\u{fe0f}' | '\u{fe0e}')
                    && previous
                        .text
                        .chars()
                        .next()
                        .is_none_or(|p| properties(p).grapheme != 11)
                {
                    return;
                }
                self.append_grapheme(col, cp, previous.width, right);
            }
            return;
        }
        self.previous_char = Some(cp);
        if current.pending_wrap && self.modes.dec(7) {
            self.print_wrap();
        }
        if self.modes.get(false, 4)
            && self.screen().cursor.col + usize::from(width) < self.cols as usize
        {
            self.insert_blanks(width.into());
        }
        if width == 2 && right.saturating_sub(self.margins.left) < 1 {
            self.put_cell(String::new(), 1, false);
        } else {
            if width == 2 && self.screen().cursor.col == right {
                if !self.modes.dec(7) {
                    return;
                }
                self.put_cell(String::new(), 1, right == self.cols as usize - 1);
                self.print_wrap();
            }
            self.put_cell(cp.to_string(), width, false);
        }
        let x = self.screen().cursor.col;
        self.ensure_row_cells(self.screen().cursor.row, x + usize::from(width) + 1);
        self.screen_mut().cursor.pending_wrap = x + width as usize > right;
        self.screen_mut().cursor.col = (x + width as usize).min(right);
        self.changed();
    }

    fn append_grapheme(&mut self, mut col: usize, cp: char, width: u8, right: usize) {
        let cursor = self.screen().cursor.clone();
        self.ensure_row_cells(cursor.row, col + usize::from(width));
        let old_width = self.screen().rows[cursor.row].cells[col].width;
        if self.screen().rows[cursor.row].cells[col]
            .text
            .chars()
            .count()
            >= 65
        {
            return;
        }
        if width > old_width && col == right {
            if !self.modes.dec(7) {
                return;
            }
            let cell = self.screen().rows[cursor.row].cells[col].clone();
            self.screen_mut().cursor.col = col;
            self.put_cell(String::new(), 1, right == self.cols as usize - 1);
            self.print_wrap();
            col = self.screen().cursor.col;
            self.put_cell(cell.text, width, false);
        } else if width != old_width {
            let row = &mut self.screen_mut().rows[cursor.row];
            row.cells[col].width = width;
            if col < right {
                row.cells[col + 1] = Cell::blank(cursor.style.background);
                if width == 2 {
                    row.cells[col + 1] = row.cells[col].clone();
                    row.cells[col + 1].text.clear();
                    row.cells[col + 1].width = 0;
                }
            }
        }
        let row = self.screen().cursor.row;
        self.screen_mut().rows[row].cells[col].text.push(cp);
        self.screen_mut().rows[row].dirty = true;
        if width != old_width {
            self.screen_mut().cursor.col = (col + width as usize).min(right);
            self.screen_mut().cursor.pending_wrap =
                width > old_width && col + width as usize > right;
        }
        self.changed();
    }

    fn put_cell(&mut self, text: String, width: u8, spacer_head: bool) {
        let cursor = self.screen().cursor.clone();
        self.ensure_row_cells(cursor.row, cursor.col + usize::from(width));
        let old_width = self.screen().rows[cursor.row].cells[cursor.col].width;
        if cursor.row > 0 && cursor.col <= 1 && old_width != width && old_width != 1 {
            let previous = &mut self.screen_mut().rows[cursor.row - 1];
            previous.cells.last_mut().unwrap().spacer_head = false;
            previous.dirty = true;
        }
        let row = &mut self.screen_mut().rows[cursor.row];
        row.erase(
            cursor.col,
            cursor.col + width as usize,
            cursor.style.background,
            false,
        );
        let cell = Cell {
            text,
            width,
            style: cursor.style,
            hyperlink: cursor.hyperlink,
            hyperlink_id: cursor.hyperlink_id,
            hyperlink_raw: cursor.hyperlink_raw,
            protected: cursor.protected,
            semantic: cursor.semantic,
            spacer_head,
        };
        row.cells[cursor.col] = cell.clone();
        if width == 2 && cursor.col + 1 < row.cells.len() {
            row.cells[cursor.col + 1] = Cell {
                text: String::new(),
                width: 0,
                ..cell
            };
        }
        row.dirty = true;
    }

    fn print_wrap(&mut self) {
        let y = self.screen().cursor.row;
        let mark_wrap = self.screen().cursor.col == usize::from(self.cols) - 1;
        if mark_wrap {
            self.screen_mut().rows[y].wrapped = true;
        }
        self.index();
        if mark_wrap {
            let y = self.screen().cursor.row;
            self.screen_mut().rows[y].wrap_continuation = true;
        }
        self.screen_mut().cursor.col = self.margins.left;
        self.ensure_row_cells(self.screen().cursor.row, self.margins.left + 1);
        self.screen_mut().cursor.pending_wrap = false;
    }

    pub fn carriage_return(&mut self) {
        let col = if self.modes.dec(6) || self.screen().cursor.col >= self.margins.left {
            self.margins.left
        } else {
            0
        };
        self.ensure_row_cells(self.screen().cursor.row, col + 1);
        self.screen_mut().cursor.col = col;
        self.screen_mut().cursor.pending_wrap = false;
        self.changed();
    }

    pub fn cursor_position(&mut self, row: usize, col: usize) {
        let (top, left, bottom, right) = if self.modes.dec(6) {
            (
                self.margins.top,
                self.margins.left,
                self.margins.bottom,
                self.margins.right,
            )
        } else {
            (0, 0, self.rows as usize - 1, self.cols as usize - 1)
        };
        let row = top.saturating_add(row.max(1) - 1).min(bottom);
        let col = left.saturating_add(col.max(1) - 1).min(right);
        self.ensure_row_cells(row, col + 1);
        let cursor = &mut self.screen_mut().cursor;
        cursor.row = row;
        cursor.col = col;
        cursor.pending_wrap = false;
        self.changed();
    }

    fn cursor_vertical(&mut self, amount: usize, down: bool) {
        let y = self.screen().cursor.row;
        let target = if down {
            let bottom = if y <= self.margins.bottom {
                self.margins.bottom
            } else {
                self.rows as usize - 1
            };
            y.saturating_add(amount.max(1)).min(bottom)
        } else {
            let top = if y >= self.margins.top {
                self.margins.top
            } else {
                0
            };
            y.saturating_sub(amount.max(1)).max(top)
        };
        self.screen_mut().cursor.row = target;
        self.clamp_cursor();
        self.screen_mut().cursor.pending_wrap = false;
        self.changed();
    }

    fn cursor_right(&mut self, amount: usize) {
        let x = self.screen().cursor.col;
        let right = if x <= self.margins.right {
            self.margins.right
        } else {
            self.cols as usize - 1
        };
        let col = x.saturating_add(amount.max(1)).min(right);
        self.ensure_row_cells(self.screen().cursor.row, col + 1);
        self.screen_mut().cursor.col = col;
        self.screen_mut().cursor.pending_wrap = false;
        self.changed();
    }

    fn cursor_left(&mut self, amount: usize) {
        let mut remaining = amount.max(1);
        let reverse = self.modes.dec(7) && (self.modes.dec(45) || self.modes.dec(1045));
        let extended = self.modes.dec(1045);
        if reverse && self.screen().cursor.pending_wrap {
            remaining -= 1;
        }
        self.screen_mut().cursor.pending_wrap = false;
        if !reverse {
            self.screen_mut().cursor.col = self.screen().cursor.col.saturating_sub(remaining);
        } else {
            let left = if self.screen().cursor.col < self.margins.left {
                0
            } else {
                self.margins.left
            };
            while remaining > 0 {
                let current = self.screen().cursor.clone();
                let n = remaining.min(current.col.saturating_sub(left));
                self.screen_mut().cursor.col -= n;
                remaining -= n;
                if remaining == 0 {
                    break;
                }
                if current.row == self.margins.top {
                    if !extended {
                        break;
                    }
                    self.screen_mut().cursor.row = self.margins.bottom;
                } else {
                    if current.row == 0 || !extended && !self.screen().rows[current.row - 1].wrapped
                    {
                        break;
                    }
                    self.screen_mut().cursor.row -= 1;
                }
                self.screen_mut().cursor.col = self.margins.right;
                self.ensure_row_cells(self.screen().cursor.row, self.margins.right + 1);
                remaining -= 1;
            }
        }
        self.changed();
    }

    pub fn index(&mut self) {
        let y = self.screen().cursor.row;
        if y == self.margins.bottom {
            let x = self.screen().cursor.col;
            if x >= self.margins.left && x <= self.margins.right {
                self.scroll_up(1, true);
            }
        } else if y + 1 < self.rows as usize {
            self.screen_mut().cursor.row += 1;
        }
        self.clamp_cursor();
        self.screen_mut().cursor.pending_wrap = false;
        self.changed();
    }

    fn reverse_index(&mut self) {
        if self.screen().cursor.row == self.margins.top {
            self.scroll_down(1);
        } else {
            self.screen_mut().cursor.row = self.screen().cursor.row.saturating_sub(1);
        }
        self.clamp_cursor();
        self.screen_mut().cursor.pending_wrap = false;
        self.changed();
    }

    fn scroll_up(&mut self, count: usize, history: bool) {
        let m = self.margins;
        let count = count.max(1).min(m.bottom - m.top + 1);
        let cols = self.cols as usize;
        let bg = self.screen().cursor.style.background;
        let full = m.left == 0 && m.right == cols - 1;
        for _ in 0..count {
            if full {
                let row = self.screen_mut().rows.remove(m.top);
                if history && m.top == 0 {
                    self.screen_mut().push_history(row);
                } else {
                    self.screen_mut().discard_row(row.id);
                }
                let blank = self.screen_mut().blank_row(cols, bg);
                self.screen_mut().rows.insert(m.bottom, blank);
            } else {
                for y in m.top..m.bottom {
                    self.copy_row_region(y + 1, y, m.left, m.right + 1, bg);
                }
                self.screen_mut().rows[m.bottom].erase(m.left, m.right + 1, bg, false);
            }
        }
        self.clamp_cursor();
        self.changed();
    }

    fn scroll_down(&mut self, count: usize) {
        let m = self.margins;
        let cols = self.cols as usize;
        let count = count.max(1).min(m.bottom - m.top + 1);
        let bg = self.screen().cursor.style.background;
        for _ in 0..count {
            if m.left == 0 && m.right == cols - 1 {
                let row = self.screen_mut().rows.remove(m.bottom);
                self.screen_mut().discard_row(row.id);
                let blank = self.screen_mut().blank_row(cols, bg);
                self.screen_mut().rows.insert(m.top, blank);
            } else {
                for y in (m.top + 1..=m.bottom).rev() {
                    self.copy_row_region(y - 1, y, m.left, m.right + 1, bg);
                }
                self.screen_mut().rows[m.top].erase(m.left, m.right + 1, bg, false);
            }
        }
        self.clamp_cursor();
        self.changed();
    }

    fn copy_row_region(
        &mut self,
        source: usize,
        destination: usize,
        start: usize,
        end: usize,
        background: Color,
    ) {
        let end = end.min(self.screen().rows[destination].cells.len());
        if start >= end {
            return;
        }
        let source = &self.screen().rows[source].cells;
        let cells: Vec<_> = (start..end)
            .map(|col| {
                source
                    .get(col)
                    .cloned()
                    .unwrap_or_else(|| Cell::blank(background))
            })
            .collect();
        let destination = &mut self.screen_mut().rows[destination];
        destination.cells[start..end].clone_from_slice(&cells);
        destination.repair_wide(background);
    }

    fn tab(&mut self, count: usize, backward: bool) {
        for _ in 0..count.min(self.cols as usize) {
            let col = self.screen().cursor.col;
            let next = if backward {
                (0..col).rev().find(|&i| self.tabstops[i]).unwrap_or(0)
            } else {
                (col + 1..self.cols as usize)
                    .find(|&i| self.tabstops[i])
                    .unwrap_or(self.cols as usize - 1)
            };
            self.screen_mut().cursor.col = next;
            self.ensure_row_cells(self.screen().cursor.row, next + 1);
        }
        self.screen_mut().cursor.pending_wrap = false;
        self.changed();
    }

    fn insert_blanks(&mut self, count: usize) {
        self.screen_mut().cursor.pending_wrap = false;
        let cur = self.screen().cursor.clone();
        if cur.col < self.margins.left || cur.col > self.margins.right {
            return;
        }
        let end = (self.margins.right + 1).min(self.screen().rows[cur.row].cells.len());
        let count = count.max(1).min(end - cur.col);
        let row = &mut self.screen_mut().rows[cur.row];
        row.cells[cur.col..end].rotate_right(count);
        row.cells[cur.col..cur.col + count].fill(Cell::blank(cur.style.background));
        row.repair_wide(cur.style.background);
        self.changed();
    }

    fn delete_chars(&mut self, count: usize) {
        let cur = self.screen().cursor.clone();
        if cur.col < self.margins.left || cur.col > self.margins.right {
            return;
        }
        let end = (self.margins.right + 1).min(self.screen().rows[cur.row].cells.len());
        let count = count.max(1).min(end - cur.col);
        self.screen_mut().split_cell_boundary(cur.col);
        self.screen_mut().split_cell_boundary(cur.col + count);
        self.screen_mut().split_cell_boundary(end);
        let row = &mut self.screen_mut().rows[cur.row];
        row.cells[cur.col..end].rotate_left(count);
        row.cells[end - count..end].fill(Cell::blank(cur.style.background));
        row.repair_wide(cur.style.background);
        row.wrapped = false;
        self.screen_mut().cursor.pending_wrap = false;
        self.changed();
    }

    pub fn erase_line(&mut self, mode: u16, protected: bool) {
        let cursor = self.screen().cursor.clone();
        let protected = protected || self.screen().iso_protection;
        let cols = self.cols as usize;
        let (start, end) = match mode {
            0 => (cursor.col, cols),
            1 => (0, cursor.col + 1),
            2 => (0, cols),
            _ => return,
        };
        let row = &mut self.screen_mut().rows[cursor.row];
        row.erase(start, end, cursor.style.background, protected);
        if mode != 1 {
            row.wrapped = false;
        }
        self.screen_mut().cursor.pending_wrap = false;
        self.changed();
    }

    pub fn erase_display(&mut self, mode: u16, protected: bool) {
        let cursor = self.screen().cursor.clone();
        let rows = self.rows as usize;
        let protected = protected || self.screen().iso_protection;
        let (start, end) = match mode {
            0 => {
                self.erase_line(0, protected);
                (cursor.row + 1, rows)
            }
            1 => {
                self.erase_line(1, protected);
                (0, cursor.row)
            }
            2 => {
                self.screen_mut().clear_visible_images();
                (0, rows)
            }
            3 => {
                self.screen_mut().clear_history();
                self.changed();
                return;
            }
            22 => {
                let m = self.margins;
                self.reset_margins();
                self.scroll_up(rows, true);
                self.margins = m;
                return;
            }
            _ => return,
        };
        for row in &mut self.screen_mut().rows[start..end] {
            row.erase(0, row.cells.len(), cursor.style.background, protected);
            row.wrapped = false;
        }
        self.screen_mut().cursor.pending_wrap = false;
        self.changed();
    }

    pub fn save_cursor(&mut self) {
        let origin = self.modes.dec(6);
        let screen = self.screen_mut();
        screen.saved_cursor = Some(SavedCursor {
            cursor: screen.cursor.clone(),
            origin,
            charset: screen.charset.clone(),
        });
    }

    pub fn restore_cursor(&mut self) {
        let saved = self.screen().saved_cursor.clone().unwrap_or(SavedCursor {
            cursor: Cursor::default(),
            origin: false,
            charset: CharsetState::default(),
        });
        let shape = self.screen().cursor.shape;
        let visible = self.screen().cursor.visible;
        let blink = self.screen().cursor.blink;
        self.screen_mut().cursor = saved.cursor;
        self.screen_mut().cursor.shape = shape;
        self.screen_mut().cursor.visible = visible;
        self.screen_mut().cursor.blink = blink;
        self.screen_mut().cursor.col = self.screen().cursor.col.min(self.cols as usize - 1);
        self.ensure_row_cells(self.screen().cursor.row, self.screen().cursor.col + 1);
        self.screen_mut().cursor.row = self.screen().cursor.row.min(self.rows as usize - 1);
        self.screen_mut().charset = saved.charset;
        self.modes.set(true, 6, saved.origin);
        self.changed();
    }

    pub fn set_mode(&mut self, private: bool, mode: u16, value: bool) {
        if !self.modes.set(private, mode, value) {
            return;
        }
        if private {
            match mode {
                3 if self.modes.dec(40) => {
                    self.resize(if value { 132 } else { 80 }, self.rows);
                    self.erase_display(2, false);
                    self.cursor_position(1, 1);
                }
                6 => self.cursor_position(1, 1),
                12 | 25 => {
                    for screen in
                        std::iter::once(&mut self.primary).chain(self.alternate.iter_mut())
                    {
                        if mode == 12 {
                            screen.cursor.blink = value;
                        } else {
                            screen.cursor.visible = value;
                        }
                    }
                }
                47 | 1047 | 1049 => self.switch_screen(mode, value),
                69 if !value => {
                    self.margins.left = 0;
                    self.margins.right = self.cols as usize - 1;
                }
                1048 => {
                    if value {
                        self.save_cursor();
                    } else {
                        self.restore_cursor();
                    }
                }
                9 | 1000 | 1002 | 1003 => self.mouse_mode = if value { mode } else { 0 },
                1005 | 1006 | 1015 | 1016 => self.mouse_format = if value { mode } else { 0 },
                _ => {}
            }
        }
        self.changed();
    }

    fn switch_screen(&mut self, mode: u16, enabled: bool) {
        if mode == 1049 && enabled {
            self.save_cursor();
        }
        if mode == 1047 && !enabled && self.alternate_active {
            self.erase_display(2, false);
        }
        let old_cursor = self.screen().cursor.clone();
        let charset = self.screen().charset.clone();
        self.end_hyperlink();
        let switched = self.alternate_active != enabled;
        if enabled && self.alternate.is_none() {
            self.alternate = Some(Screen::new(
                self.cols.into(),
                self.rows.into(),
                ScrollbackLimits::NONE,
            ));
        }
        self.alternate_active = enabled;
        self.screen_mut().charset = charset;
        self.screen_mut().selection = None;
        if mode == 1049 && enabled {
            self.erase_display(2, false);
        }
        if switched {
            self.screen_mut().cursor = old_cursor;
            self.end_hyperlink();
        }
        if mode == 1049 && !enabled {
            self.restore_cursor();
        }
        self.ensure_row_cells(self.screen().cursor.row, self.screen().cursor.col + 1);
    }

    fn esc(&mut self, intermediates: &[u8], final_byte: u8, effects: &mut Vec<Effect>) {
        match (intermediates, final_byte) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => self.index(),
            ([], b'E') => {
                self.index();
                self.carriage_return();
            }
            ([], b'M') => self.reverse_index(),
            ([], b'H') => {
                let col = self.screen().cursor.col;
                self.tabstops[col] = true;
            }
            ([], b'c') => {
                self.reset();
                effects.push(Effect::Progress {
                    state: 0,
                    value: None,
                });
            }
            ([], b'=') => self.set_mode(true, 66, true),
            ([], b'>') => self.set_mode(true, 66, false),
            ([], b'N') => self.screen_mut().charset.single = Some(2),
            ([], b'O') => self.screen_mut().charset.single = Some(3),
            ([], b'V') => {
                let screen = self.screen_mut();
                screen.cursor.protected = true;
                screen.iso_protection = true;
                screen.metadata.protected_mode = 1;
            }
            ([], b'W') => self.screen_mut().cursor.protected = false,
            ([], b'n') => self.screen_mut().charset.gl = 2,
            ([], b'o') => self.screen_mut().charset.gl = 3,
            ([], b'~') => self.screen_mut().charset.gr = 1,
            ([], b'}') => self.screen_mut().charset.gr = 2,
            ([], b'|') => self.screen_mut().charset.gr = 3,
            ([b'(' | b')' | b'*' | b'+'], b'0' | b'A' | b'B') => {
                let slot = (intermediates[0] - b'(') as usize;
                self.screen_mut().charset.slots[slot] = match final_byte {
                    b'0' => Charset::DecSpecial,
                    b'A' => Charset::British,
                    _ => Charset::Ascii,
                };
            }
            ([b'%'], b'G') => self.screen_mut().charset.slots = [Charset::Utf8; 4],
            ([b'#'], b'8') => {
                self.reset_margins();
                for row in &mut self.screen_mut().rows {
                    for cell in &mut row.cells {
                        *cell = Cell {
                            text: "E".into(),
                            ..Cell::default()
                        };
                    }
                    row.wrapped = false;
                    row.dirty = true;
                }
                self.cursor_position(1, 1);
            }
            ([], b'\\') => {}
            _ => effects.push(Effect::UnknownSequence(format!(
                "ESC {:?} {}",
                intermediates, final_byte as char
            ))),
        }
    }

    fn csi(&mut self, i: &[u8], p: &[u16], sep: u32, byte: u8, effects: &mut Vec<Effect>) {
        let n = p.first().copied().unwrap_or(0);
        let count = usize::from(n.max(1));
        let second = p.get(1).copied().unwrap_or(0);
        let private = i.first() == Some(&b'?');
        match (i, byte) {
            ([], b'A') => self.cursor_vertical(count, false),
            ([], b'B' | b'e') => self.cursor_vertical(count, true),
            ([], b'C' | b'a') => self.cursor_right(count),
            ([], b'D') => self.cursor_left(count),
            ([], b'E' | b'F') => {
                self.cursor_vertical(count, byte == b'E');
                self.carriage_return();
            }
            ([], b'G' | b'`') => self.cursor_position(self.screen().cursor.row + 1, count),
            ([], b'd') => self.cursor_position(count, self.screen().cursor.col + 1),
            ([], b'H' | b'f') => self.cursor_position(count, second.max(1).into()),
            ([], b'I') => self.tab(count, false),
            ([], b'Z') => self.tab(count, true),
            ([], b'g') => match n {
                0 => {
                    let col = self.screen().cursor.col;
                    self.tabstops[col] = false;
                }
                3 => self.tabstops.fill(false),
                _ => {}
            },
            ([], b'W') if n == 5 => {
                for (col, set) in self.tabstops.iter_mut().enumerate() {
                    *set = col > 0 && col % 8 == 0;
                }
            }
            ([], b'J') | ([b'?'], b'J') => self.erase_display(n, private),
            ([], b'K') | ([b'?'], b'K') => self.erase_line(n, private),
            ([], b'@') => self.insert_blanks(count),
            ([], b'P') => self.delete_chars(count),
            ([], b'X') => {
                let cur = self.screen().cursor.clone();
                let protected = self.screen().iso_protection;
                let end = cur.col.saturating_add(count).min(self.cols as usize);
                self.screen_mut().split_cell_boundary(cur.col);
                self.screen_mut().split_cell_boundary(end);
                self.screen_mut().rows[cur.row].erase(
                    cur.col,
                    end,
                    cur.style.background,
                    protected,
                );
                self.screen_mut().rows[cur.row].wrapped = false;
                self.screen_mut().cursor.pending_wrap = false;
                self.changed();
            }
            ([], b'L' | b'M') => {
                let cur = self.screen().cursor.clone();
                if cur.row >= self.margins.top
                    && cur.row <= self.margins.bottom
                    && cur.col >= self.margins.left
                    && cur.col <= self.margins.right
                {
                    let top = self.margins.top;
                    self.margins.top = cur.row;
                    if byte == b'L' {
                        self.scroll_down(count);
                    } else {
                        self.scroll_up(count, false);
                    }
                    self.margins.top = top;
                    self.carriage_return();
                }
            }
            ([], b'S') => self.scroll_up(count, true),
            ([], b'T') => self.scroll_down(count),
            ([], b'b') => {
                if let Some(cp) = self.previous_char {
                    for _ in 0..count {
                        self.print(cp);
                    }
                }
            }
            ([], b'm') => {
                sgr(&mut self.screen_mut().cursor.style, p, sep);
                self.changed();
            }
            ([b'>'], b'm') => self.modify_other_keys = n == 4 && second == 2,
            ([b'>'], b'n') => self.modify_other_keys = false,
            ([], b'h' | b'l') | ([b'?'], b'h' | b'l') => {
                for &mode in p {
                    self.set_mode(private, mode, byte == b'h');
                }
            }
            ([b'?'], b's') => {
                for &mode in p {
                    self.modes.save(true, mode);
                }
            }
            ([b'?'], b'r') => {
                for &mode in p {
                    let value = self.modes.restore(true, mode);
                    self.set_mode(true, mode, value);
                }
            }
            ([], b'r') => {
                let top = n.max(1) as usize - 1;
                let bottom = if second == 0 {
                    self.rows
                } else {
                    second.min(self.rows)
                } as usize
                    - 1;
                if top < bottom {
                    self.margins.top = top;
                    self.margins.bottom = bottom;
                    self.cursor_position(1, 1);
                }
            }
            ([], b's') => {
                if self.modes.dec(69) {
                    let left = n.max(1) as usize - 1;
                    let right = if second == 0 {
                        self.cols
                    } else {
                        second.min(self.cols)
                    } as usize
                        - 1;
                    if left < right {
                        self.margins.left = left;
                        self.margins.right = right;
                        self.cursor_position(1, 1);
                    }
                } else {
                    self.save_cursor();
                }
            }
            ([], b'u') => self.restore_cursor(),
            ([b' '], b'q') if n <= 6 => self.set_cursor_style(n),
            ([b'"'], b'q') if n <= 2 => {
                let screen = self.screen_mut();
                screen.cursor.protected = n == 1;
                if n == 1 {
                    screen.iso_protection = false;
                    screen.metadata.protected_mode = 2;
                }
            }
            ([b'!'], b'p') => {
                self.screen_mut().cursor.style = Style::default();
                self.screen_mut().cursor.protected = false;
                self.modes.reset();
                self.reset_margins();
                self.screen_mut().cursor.visible = true;
            }
            ([b'$'], b'p') | ([b'?', b'$'], b'p') => {
                for &mode in p {
                    effects.push(Effect::Write(
                        format!(
                            "\x1b[{}{};{}$y",
                            if private { "?" } else { "" },
                            mode,
                            self.modes.report(private, mode)
                        )
                        .into_bytes(),
                    ));
                }
            }
            ([], b'n') | ([b'?'], b'n') => match n {
                5 => effects.push(Effect::Write(b"\x1b[0n".to_vec())),
                6 => {
                    let cur = &self.screen().cursor;
                    let row = cur.row.saturating_sub(if self.modes.dec(6) {
                        self.margins.top
                    } else {
                        0
                    }) + 1;
                    let col = cur.col.saturating_sub(if self.modes.dec(6) {
                        self.margins.left
                    } else {
                        0
                    }) + 1;
                    effects.push(Effect::Write(
                        format!("\x1b[{}{row};{col}R", if private { "?" } else { "" }).into_bytes(),
                    ));
                }
                _ => {}
            },
            ([], b'c') if n == 0 => effects.push(Effect::Write(b"\x1b[?62;22c".to_vec())),
            ([b'>'], b'c') => effects.push(Effect::Write(b"\x1b[>1;10;0c".to_vec())),
            ([b'='], b'c') => effects.push(Effect::Write(b"\x1bP!|00000000\x1b\\".to_vec())),
            ([b'>'], b'q') => effects.push(Effect::Write(b"\x1bP>|rustty 0.1.0\x1b\\".to_vec())),
            ([b'?'], b'u') => effects.push(Effect::Write(
                format!("\x1b[?{}u", self.screen().kitty_keyboard.current()).into_bytes(),
            )),
            ([b'>'], b'u') => {
                self.screen_mut().kitty_keyboard.push((n & 31) as u8);
            }
            ([b'<'], b'u') => {
                self.screen_mut().kitty_keyboard.pop(count);
            }
            ([b'='], b'u') => {
                self.screen_mut().kitty_keyboard.set((n & 31) as u8, second);
            }
            ([b'$'], b'}') => self.status_display = n == 1,
            ([b'$'], b'~') => {}
            ([], b't') => match n {
                14 => effects.push(Effect::Write(
                    format!("\x1b[4;{};{}t", self.height_px, self.width_px).into_bytes(),
                )),
                16 => effects.push(Effect::Write(
                    format!(
                        "\x1b[6;{};{}t",
                        self.height_px / u32::from(self.rows),
                        self.width_px / u32::from(self.cols)
                    )
                    .into_bytes(),
                )),
                18 => effects.push(Effect::Write(
                    format!("\x1b[8;{};{}t", self.rows, self.cols).into_bytes(),
                )),
                22 | 23 => {}
                _ => {}
            },
            _ => effects.push(Effect::UnknownSequence(format!(
                "CSI {:?} {:?} {}",
                i, p, byte as char
            ))),
        }
    }

    fn osc(&mut self, data: &[u8], bell: bool, effects: &mut Vec<Effect>) {
        let split = data.iter().position(|&b| b == b';').unwrap_or(data.len());
        let Ok(number) = std::str::from_utf8(&data[..split])
            .unwrap_or("")
            .parse::<u16>()
        else {
            return;
        };
        let data = data.get(split + 1..).unwrap_or_default();
        let text = String::from_utf8_lossy(data);
        let terminator = if bell { "\x07" } else { "\x1b\\" };
        match number {
            0 | 2 => {
                let tail = data.len().saturating_sub(2047);
                self.title = String::from_utf8_lossy(&data[tail..]).into_owned();
                self.metadata.title_raw = std::str::from_utf8(&data[tail..])
                    .is_err()
                    .then(|| data[tail..].to_vec());
                effects.push(Effect::Title(self.title.clone()));
            }
            7 => {
                self.working_directory = text.into_owned();
                self.metadata.pwd_raw = std::str::from_utf8(data).is_err().then(|| data.to_vec());
                effects.push(Effect::WorkingDirectory(self.working_directory.clone()));
            }
            8 => {
                if let Some(split) = data.iter().position(|&b| b == b';') {
                    self.end_hyperlink();
                    let uri = &data[split + 1..];
                    if !uri.is_empty() {
                        let explicit = data[..split]
                            .split(|&b| b == b':')
                            .find_map(|part| part.strip_prefix(b"id=").filter(|id| !id.is_empty()));
                        let screen = self.screen_mut();
                        let id = match explicit {
                            Some(id) => HyperlinkId::Explicit(id.to_vec()),
                            None => {
                                let id = screen.metadata.hyperlink_implicit_id;
                                screen.metadata.hyperlink_implicit_id = id.wrapping_add(1);
                                HyperlinkId::Implicit(id)
                            }
                        };
                        screen.cursor.hyperlink = Some(String::from_utf8_lossy(uri).into_owned());
                        screen.cursor.hyperlink_raw =
                            std::str::from_utf8(uri).is_err().then(|| uri.to_vec());
                        screen.cursor.hyperlink_id = Some(id);
                    }
                }
            }
            9 => {
                if let Some(progress) = data.strip_prefix(b"4;")
                    && let Some(&state @ b'0'..=b'4') = progress.first()
                {
                    let state = state - b'0';
                    let value = match state {
                        0 | 3 => None,
                        _ if progress.get(1) == Some(&b';') => {
                            parse_unsigned(&progress[2..]).map(|v| v.min(100) as u8)
                        }
                        1 => Some(0),
                        _ => None,
                    };
                    effects.push(Effect::Progress { state, value });
                } else {
                    effects.push(Effect::Notification {
                        title: Vec::new(),
                        body: data.to_vec(),
                    });
                }
            }
            777 => {
                if let Some(notification) = data.strip_prefix(b"notify;")
                    && let Some(split) = notification.iter().position(|&b| b == b';')
                {
                    effects.push(Effect::Notification {
                        title: notification[..split].to_vec(),
                        body: notification[split + 1..].to_vec(),
                    });
                }
            }
            52 => {
                if let Some((selection, encoded)) = text.split_once(';') {
                    let selection = selection.to_owned();
                    if encoded == "?" {
                        effects.push(Effect::Clipboard {
                            selection,
                            data: None,
                        });
                    } else {
                        let engine = base64::engine::general_purpose::GeneralPurpose::new(
                            &base64::alphabet::STANDARD,
                            base64::engine::general_purpose::GeneralPurposeConfig::new()
                                .with_decode_padding_mode(
                                    base64::engine::DecodePaddingMode::Indifferent,
                                ),
                        );
                        if let Ok(data) = engine.decode(encoded) {
                            effects.push(Effect::Clipboard {
                                selection,
                                data: Some(data),
                            });
                        }
                    }
                }
            }
            133 => match text.split(';').next().unwrap_or("") {
                "A" => {
                    let row = self.screen().cursor.row;
                    self.screen_mut().cursor.semantic = SemanticContent::Prompt;
                    self.screen_mut().rows[row].semantic = SemanticContent::Prompt;
                }
                "B" => self.screen_mut().cursor.semantic = SemanticContent::Input,
                "C" => {
                    self.screen_mut().cursor.semantic = SemanticContent::Output;
                    effects.push(Effect::CommandStart);
                }
                "D" => effects.push(Effect::CommandEnd {
                    exit_code: text.split(';').nth(1).and_then(|v| v.parse().ok()),
                }),
                _ => {}
            },
            4 => {
                let mut parts = text.split(';');
                while let (Some(index), Some(value)) = (parts.next(), parts.next()) {
                    if let Ok(index) = index.parse::<u8>() {
                        if value == "?" {
                            let [r, g, b] = self.palette[index as usize];
                            effects.push(Effect::Write(format!("\x1b]4;{index};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{terminator}").into_bytes()));
                        } else if let Some(color) = parse_color(value) {
                            self.palette[index as usize] = color;
                            self.metadata.palette_overrides[index as usize / 8] |= 1 << (index % 8);
                            self.changed();
                        }
                    }
                }
            }
            10..=12 => {
                for (offset, value) in text.split(';').enumerate() {
                    let target = number + offset as u16;
                    if target > 12 {
                        break;
                    }
                    let color = match target {
                        10 => self.foreground,
                        11 => self.background,
                        _ => self.cursor_color.unwrap_or(self.foreground),
                    };
                    if value == "?" {
                        let [r, g, b] = color;
                        effects.push(Effect::Write(format!("\x1b]{target};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{terminator}").into_bytes()));
                    } else if let Some(color) = parse_color(value) {
                        self.metadata.colors[match target {
                            10 => 1,
                            11 => 0,
                            _ => 2,
                        }][1] = Some(color);
                        match target {
                            10 => self.foreground = color,
                            11 => self.background = color,
                            _ => self.cursor_color = Some(color),
                        }
                        self.changed();
                    }
                }
            }
            104 => {
                if text.is_empty() {
                    self.palette.clone_from(&self.metadata.original_palette);
                    self.metadata.palette_overrides = [0; 32];
                } else {
                    for index in text.split(';').filter_map(|v| v.parse::<u8>().ok()) {
                        self.palette[index as usize] =
                            self.metadata.original_palette[index as usize];
                        self.metadata.palette_overrides[index as usize / 8] &= !(1 << (index % 8));
                    }
                }
                self.changed();
            }
            110 => {
                self.metadata.colors[1][1] = None;
                self.foreground = self.metadata.colors[1][0].unwrap_or([255; 3]);
                self.changed();
            }
            111 => {
                self.metadata.colors[0][1] = None;
                self.background = self.metadata.colors[0][0].unwrap_or([0; 3]);
                self.changed();
            }
            112 => {
                self.metadata.colors[2][1] = None;
                self.cursor_color = self.metadata.colors[2][0];
                self.changed();
            }
            1 | 21 | 22 => {}
            _ => effects.push(Effect::UnknownSequence(format!("OSC {number}"))),
        }
    }

    fn dcs_end(&mut self, effects: &mut Vec<Effect>) {
        if self.string_overflow {
            return;
        }
        if self.dcs_header.0 == b"$" && self.dcs_header.2 == b'q' {
            let value = match self.dcs.as_slice() {
                b"m" => Some(sgr_report(self.screen().cursor.style)),
                b"r" => Some(format!(
                    "{};{}r",
                    self.margins.top + 1,
                    self.margins.bottom + 1
                )),
                b"s" if self.modes.dec(69) => Some(format!(
                    "{};{}s",
                    self.margins.left + 1,
                    self.margins.right + 1
                )),
                b" q" => {
                    let cursor = &self.screen().cursor;
                    let shape = match cursor.shape {
                        CursorShape::Block | CursorShape::HollowBlock => 2,
                        CursorShape::Underline => 4,
                        CursorShape::Bar => 6,
                    };
                    Some(format!("{} q", shape - u8::from(cursor.blink)))
                }
                _ => None,
            };
            effects.push(Effect::Write(match value {
                Some(value) => format!("\x1bP1$r{value}\x1b\\").into_bytes(),
                None => b"\x1bP0$r\x1b\\".to_vec(),
            }));
        } else if self.dcs_header.0 == b"+" && self.dcs_header.2 == b'q' {
            let queries = String::from_utf8_lossy(&self.dcs).to_ascii_uppercase();
            for key in queries.split(';') {
                let value = if key == "544E" {
                    let Some(name) = self
                        .terminfo_name
                        .as_ref()
                        .filter(|name| !name.is_empty() && name.len() <= 128)
                    else {
                        continue;
                    };
                    name.as_bytes()
                        .iter()
                        .map(|byte| format!("{byte:02X}"))
                        .collect::<String>()
                } else {
                    let table = crate::terminfo::CAPABILITIES;
                    let Ok(index) = table.binary_search_by_key(&key, |(name, _)| *name) else {
                        continue;
                    };
                    table[index].1.to_owned()
                };
                effects.push(Effect::Write(
                    if value.is_empty() {
                        format!("\x1bP1+r{key}\x1b\\")
                    } else {
                        format!("\x1bP1+r{key}={value}\x1b\\")
                    }
                    .into_bytes(),
                ));
            }
        } else {
            effects.push(Effect::UnknownSequence("DCS".into()));
        }
        self.dcs.clear();
    }
}

/// Match Zig's decimal unsigned parser, including internal underscore separators.
fn parse_unsigned(bytes: &[u8]) -> Option<usize> {
    if !bytes.first()?.is_ascii_digit() || !bytes.last()?.is_ascii_digit() {
        return None;
    }
    bytes
        .iter()
        .filter(|&&byte| byte != b'_')
        .try_fold(0usize, |value, &byte| {
            if !byte.is_ascii_digit() {
                return None;
            }
            value.checked_mul(10)?.checked_add(usize::from(byte - b'0'))
        })
}

fn sgr_report(style: Style) -> String {
    let mut result = String::from("0");
    for (enabled, code) in [(style.bold, 1), (style.faint, 2), (style.italic, 3)] {
        if enabled {
            result.push_str(&format!(";{code}"));
        }
    }
    match style.underline {
        Underline::None => {}
        Underline::Single => result.push_str(";4"),
        underline => result.push_str(&format!(
            ";4:{}",
            match underline {
                Underline::Double => 2,
                Underline::Curly => 3,
                Underline::Dotted => 4,
                Underline::Dashed => 5,
                _ => unreachable!(),
            }
        )),
    }
    for (enabled, code) in [
        (style.overline, 53),
        (style.blink, 5),
        (style.inverse, 7),
        (style.invisible, 8),
        (style.strikethrough, 9),
    ] {
        if enabled {
            result.push_str(&format!(";{code}"));
        }
    }
    for (color, base) in [(style.foreground, 30), (style.background, 40)] {
        match color {
            Color::Default => {}
            Color::Indexed(index) if index < 8 => {
                result.push_str(&format!(";{}", base + u16::from(index)))
            }
            Color::Indexed(index) if index < 16 => {
                result.push_str(&format!(";{}", base + 60 + u16::from(index) - 8))
            }
            Color::Indexed(index) => result.push_str(&format!(";{}:5:{index}", base + 8)),
            Color::Rgb(r, g, b) => result.push_str(&format!(";{}:2::{r}:{g}:{b}", base + 8)),
        }
    }
    result.push('m');
    result
}

fn map_charset(cp: char, set: Charset) -> char {
    if set == Charset::British && cp == '#' {
        return '£';
    }
    if set == Charset::DecSpecial && ('`'..='~').contains(&cp) {
        const SPECIAL: [char; 31] = [
            '◆', '▒', '␉', '␌', '␍', '␊', '°', '±', '␤', '␋', '┘', '┐', '┌', '└', '┼', '⎺', '⎻',
            '─', '⎼', '⎽', '├', '┤', '┴', '┬', '│', '≤', '≥', 'π', '≠', '£', '·',
        ];
        return SPECIAL[cp as usize - '`' as usize];
    }
    cp
}

pub fn default_palette() -> Vec<[u8; 3]> {
    let named: [u32; 16] = [
        0x1d1f21, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6, 0x666666,
        0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
    ];
    let mut colors: Vec<_> = named
        .into_iter()
        .map(|n| [(n >> 16) as u8, (n >> 8) as u8, n as u8])
        .collect();
    for r in 0..6 {
        for g in 0..6 {
            for b in 0..6 {
                colors.push([r, g, b].map(|n| if n == 0 { 0 } else { n * 40 + 55 }));
            }
        }
    }
    for i in 0..24 {
        colors.push([i * 10 + 8; 3]);
    }
    colors
}

pub fn parse_color(text: &str) -> Option<[u8; 3]> {
    let value = text.trim().strip_prefix('#').unwrap_or(text.trim());
    if value.len() == 6
        && let Ok(rgb) = u32::from_str_radix(value, 16)
    {
        return Some([(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8]);
    }
    if let Some(rgb) = value.strip_prefix("rgb:") {
        let parts: Vec<_> = rgb.split('/').collect();
        if parts.len() != 3 {
            return None;
        }
        let mut color = [0; 3];
        for (component, part) in color.iter_mut().zip(parts) {
            if part.is_empty() || part.len() > 4 {
                return None;
            }
            let raw = u32::from_str_radix(part, 16).ok()?;
            *component = ((raw * 255) / ((1 << (part.len() * 4)) - 1)) as u8;
        }
        return Some(color);
    }
    match value.to_ascii_lowercase().as_str() {
        "black" => Some([0; 3]),
        "white" => Some([255; 3]),
        "red" => Some([255, 0, 0]),
        "green" => Some([0, 255, 0]),
        "blue" => Some([0, 0, 255]),
        _ => None,
    }
}

fn sgr(style: &mut Style, params: &[u16], separators: u32) {
    if params.is_empty() {
        *style = Style::default();
        return;
    }
    let mut i = 0;
    while i < params.len() {
        let n = params[i];
        let colon = separators & (1 << i) != 0;
        match n {
            0 => *style = Style::default(),
            1 => style.bold = true,
            2 => style.faint = true,
            3 => style.italic = true,
            4 => {
                let kind = if colon {
                    i += 1;
                    params.get(i).copied().unwrap_or(1)
                } else {
                    1
                };
                style.underline = match kind {
                    0 => Underline::None,
                    2 => Underline::Double,
                    3 => Underline::Curly,
                    4 => Underline::Dotted,
                    5 => Underline::Dashed,
                    _ => Underline::Single,
                };
            }
            5 | 6 => style.blink = true,
            7 => style.inverse = true,
            8 => style.invisible = true,
            9 => style.strikethrough = true,
            21 => style.underline = Underline::Double,
            22 => {
                style.bold = false;
                style.faint = false;
            }
            23 => style.italic = false,
            24 => style.underline = Underline::None,
            25 => style.blink = false,
            27 => style.inverse = false,
            28 => style.invisible = false,
            29 => style.strikethrough = false,
            30..=37 => style.foreground = Color::Indexed((n - 30) as u8),
            40..=47 => style.background = Color::Indexed((n - 40) as u8),
            90..=97 => style.foreground = Color::Indexed((n - 90 + 8) as u8),
            100..=107 => style.background = Color::Indexed((n - 100 + 8) as u8),
            39 => style.foreground = Color::Default,
            49 => style.background = Color::Default,
            53 => style.overline = true,
            55 => style.overline = false,
            59 => style.underline_color = Color::Default,
            38 | 48 | 58 => {
                let color = match params.get(i + 1) {
                    Some(5) => {
                        let value = params
                            .get(i + 2)
                            .copied()
                            .filter(|&v| v <= 255)
                            .map(|v| Color::Indexed(v as u8));
                        i += 2;
                        value
                    }
                    Some(2) => {
                        let mut start = i + 2;
                        if colon {
                            let mut end = start;
                            while end < params.len() && separators & (1 << (end - 1)) != 0 {
                                end += 1;
                            }
                            if end.saturating_sub(start) >= 4 {
                                start += 1;
                            }
                        }
                        let rgb = params
                            .get(start..start + 3)
                            .filter(|v| v.iter().all(|&v| v <= 255));
                        i = start + 2;
                        rgb.map(|v| Color::Rgb(v[0] as u8, v[1] as u8, v[2] as u8))
                    }
                    _ => None,
                };
                if let Some(color) = color {
                    match n {
                        38 => style.foreground = color,
                        48 => style.background = color,
                        _ => style.underline_color = color,
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
}
