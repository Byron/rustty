use crate::modes::Modes;
use crate::query::{self, Query};
use crate::screen::*;
use crate::unicode::{self, properties};
use crate::{clipboard, dnd};
use base64::Engine;
use rustty_parser::{Event, MAX_OSC_BYTES, Parser};

/// Host actions are returned in input order. The terminal never accesses the OS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Write(Vec<u8>),
    Title(Vec<u8>),
    WorkingDirectory(Vec<u8>),
    Bell,
    ClipboardRead(clipboard::Read),
    ClipboardWrite(clipboard::Write),
    /// Drag target registration, acceptance, or drop completion changed.
    DragAndDrop(dnd::Event),
    /// Serviced by built-in query defaults or the synchronous host callbacks.
    Query(Query),
    Notification {
        title: Vec<u8>,
        body: Vec<u8>,
    },
    /// Rustty host extension, enabled by `Terminal::shell_command_events`.
    CommandStart,
    /// Rustty host extension, enabled by `Terminal::shell_command_events`.
    CommandEnd {
        exit_code: Option<i32>,
    },
    Progress {
        state: u8,
        value: Option<u8>,
    },
    UnknownSequence(String),
}

/// Synchronous host callbacks used by `Terminal::feed_with_handler`.
/// Clipboard callbacks are separate from `effect`; they are never reported
/// twice. OSC 52 writes need no acknowledgement; unanswered reads return empty.
pub trait EffectHandler {
    fn effect(&mut self, effect: Effect);

    /// Observe DND state before the next parser event can change it.
    /// The default forwards only the event, like deferred `Terminal::feed`.
    fn drag_and_drop(&mut self, event: dnd::Event, _state: Option<&dnd::State>) {
        self.effect(Effect::DragAndDrop(event));
    }

    fn color_scheme(&mut self) -> Option<query::ColorScheme> {
        None
    }
    fn device_attributes(&mut self) -> Option<query::DeviceAttributes> {
        None
    }
    fn enquiry(&mut self) -> Vec<u8> {
        Vec::new()
    }
    fn size(&mut self) -> Option<query::Size> {
        None
    }
    fn xtversion(&mut self) -> Vec<u8> {
        Vec::new()
    }

    fn clipboard_read_enabled(&self) -> bool {
        true
    }

    fn clipboard_write_enabled(&self) -> bool {
        true
    }

    fn clipboard_read(&mut self, _request: &clipboard::Read) -> clipboard::ReadResult {
        clipboard::ReadResult::Denied
    }

    fn clipboard_write(&mut self, _request: &clipboard::Write) -> clipboard::WriteResult {
        clipboard::WriteResult::Denied
    }
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
    pub terminfo_name: Option<Vec<u8>>,
    pub query_defaults: query::Defaults,
    /// Allow CSI 21 t to report the raw title. Disabled by default.
    pub title_report: bool,
    /// Emit command lifecycle effects for OSC 133 C/D. Disabled for native VT
    /// compatibility; application sessions opt in. Retained through reset,
    /// but not persisted in terminal snapshots.
    pub shell_command_events: bool,
    /// Externally owned view visibility, retained through reset.
    pub visible: bool,
    /// Maximum total decoded bytes in a Kitty clipboard write transaction.
    /// A transfer retains the value that was configured when it began.
    pub clipboard_write_limit: usize,
    pub glyphs: crate::glyph::Glyphs,
    /// OSC 72 state, allocated on registration and omitted from snapshots.
    pub kitty_dnd: Option<dnd::State>,
    pub(crate) clipboard: clipboard::kitty::State,
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
    apc_glyph_limit: Option<usize>,
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
        let mut parser = Parser::new();
        // Native capture starts after the command prefix. The raw parser also
        // retains that prefix; OSC 5522 has the longest allocating one.
        parser.set_osc_limit(MAX_OSC_BYTES + b"5522;".len());
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
            query_defaults: query::Defaults::default(),
            title_report: false,
            shell_command_events: false,
            visible: true,
            clipboard_write_limit: 64 * 1024 * 1024,
            glyphs: crate::glyph::Glyphs::default(),
            kitty_dnd: None,
            clipboard: clipboard::kitty::State::default(),
            title: String::new(),
            working_directory: String::new(),
            generation: 0,
            modify_other_keys: false,
            mouse_mode: 0,
            mouse_format: 0,
            primary: Screen::new(cols.into(), rows.into(), limits),
            alternate: None,
            alternate_active: false,
            parser,
            tabstops: (0..cols).map(|i| i > 0 && i % 8 == 0).collect(),
            previous_char: None,
            grapheme_state: 0,
            status_display: false,
            dcs: Vec::new(),
            dcs_header: (Vec::new(), Vec::new(), 0),
            apc: Vec::new(),
            apc_glyph_limit: None,
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

    /// Release a tracked cell even when its screen is inactive or was reset.
    pub fn untrack(&mut self, point: TrackedPoint) {
        self.primary.untrack(point);
        if let Some(screen) = &mut self.alternate {
            screen.untrack(point);
        }
    }

    pub fn tabstops(&self) -> &[bool] {
        &self.tabstops
    }

    /// Raw title bytes, including non-UTF-8 bytes retained by direct setters
    /// or a stream title truncated in the middle of a character.
    pub fn title_bytes(&self) -> &[u8] {
        self.metadata
            .title_raw
            .as_deref()
            .unwrap_or(self.title.as_bytes())
    }

    pub fn working_directory_bytes(&self) -> &[u8] {
        self.metadata
            .pwd_raw
            .as_deref()
            .unwrap_or(self.working_directory.as_bytes())
    }

    /// Replace raw host state without stream length limits or host effects.
    /// An empty value clears the title; `title` remains its display string.
    pub fn set_title(&mut self, title: &[u8]) {
        self.title = String::from_utf8_lossy(title).into_owned();
        self.metadata.title_raw = std::str::from_utf8(title).is_err().then(|| title.to_vec());
        self.changed();
    }

    /// Replace the raw working directory without parsing or decoding its URI.
    /// An empty value clears it. This direct setter emits no host effects.
    pub fn set_working_directory(&mut self, directory: &[u8]) {
        self.working_directory = String::from_utf8_lossy(directory).into_owned();
        self.metadata.pwd_raw = std::str::from_utf8(directory)
            .is_err()
            .then(|| directory.to_vec());
        self.changed();
    }

    pub fn shell_prompt_redraw(&self) -> PromptRedraw {
        match self.metadata.shell_redraw {
            1 => PromptRedraw::None,
            2 => PromptRedraw::Last,
            _ => PromptRedraw::All,
        }
    }

    /// Feed input with runtime `query_defaults` and return deferred host effects.
    /// Query replies use the current terminal geometry. For fully headless or
    /// readonly hosts, use `feed_with_handler` and opt into the needed callbacks.
    /// DND effects carry event tags; use `feed_with_handler` to observe the state
    /// at each event instead of the final state after the complete input batch.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Effect> {
        let mut effects = Vec::new();
        let mut pending = Vec::new();
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(bytes, |event| {
            self.handle(event, &mut pending, true, true);
            for effect in pending.drain(..) {
                match effect {
                    Effect::Query(query) => {
                        if let Some(reply) = self.query_defaults.reply(query, self.query_size()) {
                            effects.push(Effect::Write(reply));
                        }
                    }
                    effect => effects.push(effect),
                }
            }
        });
        self.parser = parser;
        effects
    }

    /// Feed bytes while servicing host queries, clipboard requests, and DND
    /// state callbacks before the next parser event. Replies go to `handler.effect`.
    /// Hosts that need deferred consent can use `feed` and `Read::reply` instead.
    pub fn feed_with_handler(&mut self, bytes: &[u8], handler: &mut impl EffectHandler) {
        let mut effects = Vec::new();
        let mut parser = std::mem::take(&mut self.parser);
        let read_enabled = handler.clipboard_read_enabled();
        let write_enabled = handler.clipboard_write_enabled();
        parser.advance(bytes, |event| {
            self.handle(event, &mut effects, write_enabled, read_enabled);
            for effect in effects.drain(..) {
                match effect {
                    Effect::DragAndDrop(event) => {
                        handler.drag_and_drop(event, self.kitty_dnd.as_ref());
                    }
                    Effect::ClipboardRead(request) => {
                        let result = if read_enabled {
                            handler.clipboard_read(&request)
                        } else if request.protocol == clipboard::Protocol::Osc52 {
                            continue;
                        } else {
                            clipboard::ReadResult::Denied
                        };
                        handler.effect(Effect::Write(self.reply_clipboard_read(request, result)));
                    }
                    Effect::ClipboardWrite(request) => {
                        if write_enabled {
                            let result = handler.clipboard_write(&request);
                            if let Some(reply) = self.reply_clipboard_write(request, result) {
                                handler.effect(Effect::Write(reply));
                            }
                        }
                    }
                    Effect::Query(query) => {
                        let reply = match query {
                            Query::ColorScheme => {
                                handler.color_scheme().map(query::ColorScheme::encode)
                            }
                            Query::DeviceAttributes(kind) => {
                                handler.device_attributes().map(|a| a.encode(kind))
                            }
                            Query::Enquiry => query::enquiry(&handler.enquiry()),
                            Query::Size(style) => handler.size().map(|s| s.encode(style)),
                            Query::Xtversion => query::xtversion(&handler.xtversion()),
                        };
                        if let Some(reply) = reply {
                            handler.effect(Effect::Write(reply));
                        }
                    }
                    effect => handler.effect(effect),
                }
            }
        });
        self.parser = parser;
    }

    /// Complete a deferred clipboard request, including an optional grant.
    pub fn reply_clipboard_read(
        &mut self,
        request: clipboard::Read,
        result: clipboard::ReadResult,
    ) -> Vec<u8> {
        self.clipboard.remember_read(&request, &result);
        request.reply_result(result)
    }

    /// Complete a deferred write. OSC 52 has no acknowledgement bytes.
    pub fn reply_clipboard_write(
        &mut self,
        request: clipboard::Write,
        result: clipboard::WriteResult,
    ) -> Option<Vec<u8>> {
        self.clipboard.remember_write(&request, result);
        request.reply(result)
    }

    fn ensure_row_cells(&mut self, row: usize, end: usize) {
        let columns = usize::from(self.cols);
        if self.screen().rows[row].cells.len() < end.min(columns) {
            // A partial reflow can leave a physical row narrower than the
            // logical screen. Extend its actual page when an edit reaches
            // past it, so subsequent snapshots keep valid PAGE dimensions.
            self.screen_mut().extend_physical_row(row, columns);
        }
    }

    fn ensure_active_columns(&mut self) {
        let columns = usize::from(self.cols);
        for row in 0..usize::from(self.rows) {
            self.ensure_row_cells(row, columns);
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
        let prompt_redraw = self.shell_prompt_redraw();
        self.primary
            .resize(cols.into(), rows.into(), self.modes.dec(7));
        self.primary.clear_prompt_for_redraw(prompt_redraw);
        self.primary.sync_cursor_style();
        if let Some(alt) = &mut self.alternate {
            alt.resize(cols.into(), rows.into(), false);
            alt.sync_cursor_style();
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

    /// Current complete cell geometry for the built-in query responder.
    pub fn query_size(&self) -> query::Size {
        query::Size {
            rows: self.rows,
            columns: self.cols,
            cell_width: self.width_px / u32::from(self.cols),
            cell_height: self.height_px / u32::from(self.rows),
        }
    }

    /// Resize with optional cell pixel geometry. Supplying complete geometry
    /// emits the mode-2048 report when enabled, even if grid dimensions match.
    pub fn resize_with_cell_size(
        &mut self,
        cols: u16,
        rows: u16,
        cell_size: Option<(u32, u32)>,
    ) -> Vec<Effect> {
        if cols == 0 || rows == 0 {
            return Vec::new();
        }
        self.resize(cols, rows);
        let Some((cell_width, cell_height)) = cell_size else {
            return Vec::new();
        };
        self.set_pixel_size(
            u32::from(cols).saturating_mul(cell_width),
            u32::from(rows).saturating_mul(cell_height),
        );
        if self.modes.dec(2048) {
            vec![Effect::Write(
                query::Size {
                    rows,
                    columns: cols,
                    cell_width,
                    cell_height,
                }
                .encode(query::SizeStyle::InBand),
            )]
        } else {
            Vec::new()
        }
    }

    /// Reset terminal state while preserving the input stream's pending bytes.
    /// A host reset does not cancel a partially received escape sequence.
    pub fn reset(&mut self) {
        let input = (
            std::mem::take(&mut self.parser),
            std::mem::take(&mut self.dcs),
            std::mem::take(&mut self.dcs_header),
            std::mem::take(&mut self.apc),
            self.apc_glyph_limit,
            self.string_overflow,
        );
        let limits = self.primary.limits;
        let primary_identity = self.primary.metadata.identity;
        let terminfo_name = self.terminfo_name.take();
        let query_defaults = self.query_defaults.clone();
        let title_report = self.title_report;
        let shell_command_events = self.shell_command_events;
        let visible = self.visible;
        let clipboard = std::mem::take(&mut self.clipboard);
        // RIS interrupts chunking while retaining the drag registration and data.
        let mut kitty_dnd = self.kitty_dnd.take();
        if let Some(state) = &mut kitty_dnd {
            state.chunking = dnd::Chunking::default();
        }
        let clipboard_write_limit = self.clipboard_write_limit;
        let mut glyphs = std::mem::take(&mut self.glyphs);
        glyphs.reset();
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
        (
            self.parser,
            self.dcs,
            self.dcs_header,
            self.apc,
            self.apc_glyph_limit,
            self.string_overflow,
        ) = input;
        // RIS resets the existing primary screen. A streaming restore may
        // still deliver older history into that same screen afterward.
        self.primary.metadata.identity = primary_identity;
        self.terminfo_name = terminfo_name;
        self.query_defaults = query_defaults;
        self.title_report = title_report;
        self.shell_command_events = shell_command_events;
        self.visible = visible;
        self.clipboard = clipboard;
        self.kitty_dnd = kitty_dnd;
        self.clipboard_write_limit = clipboard_write_limit;
        self.glyphs = glyphs;
        self.width_px = width_px;
        self.height_px = height_px;
        self.metadata = metadata;
        self.modes = modes;
        self.foreground = foreground;
        self.background = background;
        self.cursor_color = cursor;
        self.palette = palette;
        self.set_cursor_style(0);
        self.set_mode(true, 25, self.modes.dec(25));
        self.changed();
    }

    /// Set a host-configurable mode's current value and reset default. Modes
    /// with transition side effects require their semantic API instead.
    pub fn set_default_mode(&mut self, private: bool, mode: u16, value: bool) -> bool {
        if !Modes::default_configurable(private, mode) {
            return false;
        }
        self.modes.set_default(private, mode, value);
        self.set_mode(private, mode, value);
        true
    }

    /// Update configured colors while preserving application overrides.
    /// `None` clears a configured dynamic color without inventing a query value.
    /// Palette entries beyond the supplied slice retain their defaults.
    pub fn set_default_colors(
        &mut self,
        foreground: Option<[u8; 3]>,
        background: Option<[u8; 3]>,
        cursor: Option<[u8; 3]>,
        palette: &[[u8; 3]],
    ) {
        for (entry, value) in self
            .metadata
            .colors
            .iter_mut()
            .zip([background, foreground, cursor])
        {
            entry[0] = value;
        }
        self.background = self.metadata.colors[0][1].or(background).unwrap_or([0; 3]);
        self.foreground = self.metadata.colors[1][1]
            .or(foreground)
            .unwrap_or([255; 3]);
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

    pub(crate) fn changed(&mut self) {
        self.screen_mut().sync_cursor_style();
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

    fn handle(
        &mut self,
        event: Event<'_>,
        effects: &mut Vec<Effect>,
        clipboard_write_enabled: bool,
        clipboard_read_enabled: bool,
    ) {
        match event {
            Event::Print(cp) => self.print(cp),
            // Raw C1 controls emitted outside UTF-8 ground-state decoding are
            // equivalent to their seven-bit ESC forms.
            Event::Execute(byte) if byte > 0x7f => self.esc(&[], byte - 0x40, effects),
            Event::Execute(byte) => match byte {
                0x05 => effects.push(Effect::Query(Query::Enquiry)),
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
            } => self.csi(
                intermediates,
                params,
                colon_separators,
                final_byte,
                effects,
                clipboard_read_enabled,
            ),
            Event::Osc {
                data,
                terminated_by_bell,
            } => self.osc(
                data,
                terminated_by_bell,
                effects,
                clipboard_write_enabled,
                clipboard_read_enabled,
            ),
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
                self.apc_glyph_limit = None;
                self.string_overflow = false;
            }
            Event::ApcPut(byte) => {
                if self.string_overflow {
                    // A disabled or over-limit glyph command stays ignored.
                } else if self
                    .apc_glyph_limit
                    .map_or(self.apc.len() >= 64 * 1024 * 1024, |limit| {
                        self.apc.len() - 5 >= limit
                    })
                {
                    self.string_overflow = true;
                } else {
                    self.apc.push(byte);
                    if self.apc.as_slice() == b"25a1;" {
                        self.apc_glyph_limit = Some(self.glyphs.apc_limit());
                        self.string_overflow = !self.glyphs.enabled();
                    }
                }
            }
            Event::ApcEnd => {
                let mut apc = std::mem::take(&mut self.apc);
                if !self.string_overflow {
                    if self.apc_glyph_limit.is_some() {
                        let (reply, changed) = self.glyphs.execute(&apc[5..]);
                        if let Some(reply) = reply {
                            effects.push(Effect::Write(reply));
                        }
                        if changed {
                            self.changed();
                        }
                    } else {
                        self.graphics_command(&apc, effects);
                    }
                }
                apc.clear();
                self.apc = apc;
            }
            Event::OscOverflow => {
                effects.push(Effect::UnknownSequence("OSC exceeded capture limit".into()))
            }
        }
    }

    pub fn print(&mut self, cp: char) {
        if self.status_display {
            return;
        }
        self.ensure_row_cells(self.screen().cursor.row, usize::from(self.cols));
        self.clamp_cursor();
        let current = self.screen().cursor.clone();
        let right = if current.col > self.margins.right {
            self.cols as usize - 1
        } else {
            self.margins.right
        };
        let previous_col = if current.pending_wrap && self.modes.dec(7) {
            Some(current.col)
        } else if self.modes.dec(2027)
            && !self.modes.dec(7)
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
            if cell.text.chars().nth(1).is_some() {
                // Native moves existing grapheme data without printing a
                // spacer head, so the pending single shift reaches the base.
                let spacer_head = right == self.cols as usize - 1;
                let row = &mut self.screen_mut().rows[cursor.row];
                row.cells[col].text.clear();
                row.cells[col].width = 1;
                row.cells[col].spacer_head = spacer_head;
                row.dirty = true;
            } else {
                self.put_cell(String::new(), 1, right == self.cols as usize - 1);
            }
            self.print_wrap();
            col = self.screen().cursor.col;
            self.put_cell(cell.text, width, false);
        } else if width != old_width {
            if width == 2 {
                // Widening writes a spacer tail, which consumes the shift.
                self.screen_mut().charset.single = None;
            }
            self.screen_mut().edit_row(cursor.row, |row| {
                row.cells[col].width = width;
                if col < right {
                    row.cells[col + 1] = Cell::blank(cursor.style.background);
                    if width == 2 {
                        row.cells[col + 1] = row.cells[col].clone();
                        row.cells[col + 1].text.clear();
                        row.cells[col + 1].width = 0;
                    }
                }
            });
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

    fn put_cell(&mut self, mut text: String, width: u8, spacer_head: bool) {
        // Map only when writing a cell. Width, combining behavior, and REP
        // use the original scalar; even an empty spacer consumes one shift.
        let charset = &mut self.screen_mut().charset;
        let slot = charset.single.take().unwrap_or(charset.gl);
        if let Some(cp) = text.chars().next() {
            let mapped = map_charset(cp, charset.slots[slot]);
            if mapped == '\0' {
                // Native cells reserve codepoint zero for empty content.
                text.clear();
            } else if cp != mapped {
                text.replace_range(..cp.len_utf8(), &mapped.to_string());
            }
        }
        let cursor = self.screen().cursor.clone();
        self.ensure_row_cells(cursor.row, cursor.col + usize::from(width));
        let old_width = self.screen().rows[cursor.row].cells[cursor.col].width;
        if cursor.row > 0 && cursor.col <= 1 && old_width != width && old_width != 1 {
            let previous = &mut self.screen_mut().rows[cursor.row - 1];
            previous.cells.last_mut().unwrap().spacer_head = false;
            previous.dirty = true;
        }
        self.screen_mut().erase_row_cells(
            cursor.row,
            cursor.col,
            cursor.col + width as usize,
            cursor.style.background,
            false,
        );
        let style_id = self.screen_mut().retain_cursor_style_for_cell();
        if width == 2 {
            self.screen_mut().retain_cursor_style_for_cell();
        }
        let row = &mut self.screen_mut().rows[cursor.row];
        let cell = Cell {
            style_id,
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
        let semantic = self.screen().cursor.semantic;
        let clear_eol = self.screen().metadata.cursor_clear_eol;
        let mark_wrap = self.screen().cursor.col == usize::from(self.cols) - 1;
        if mark_wrap {
            self.screen_mut().rows[y].wrapped = true;
        }
        self.index();
        let screen = self.screen_mut();
        screen.cursor.semantic = semantic;
        screen.metadata.cursor_clear_eol = clear_eol;
        if semantic == SemanticContent::Prompt {
            screen.rows[screen.cursor.row].semantic = SemanticContent::Input;
        }
        if mark_wrap {
            let y = self.screen().cursor.row;
            self.screen_mut().rows[y].wrap_continuation = true;
        }
        self.screen_mut().cursor.col = self.margins.left;
        self.ensure_row_cells(self.screen().cursor.row, usize::from(self.cols));
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
        self.ensure_row_cells(target, self.screen().cursor.col + 1);
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
                self.index_scroll();
            }
        } else if y + 1 < self.rows as usize {
            self.ensure_row_cells(y + 1, self.screen().cursor.col + 1);
            self.screen_mut().cursor.row += 1;
        }
        self.clamp_cursor();
        self.screen_mut().cursor.pending_wrap = false;
        let screen = self.screen_mut();
        if screen.cursor.semantic != SemanticContent::Output {
            if screen.metadata.cursor_clear_eol {
                screen.cursor.semantic = SemanticContent::Output;
                screen.metadata.cursor_clear_eol = false;
            } else {
                screen.rows[screen.cursor.row].semantic = SemanticContent::Input;
            }
        }
        self.changed();
    }

    fn reverse_index(&mut self) {
        let cursor = &self.screen().cursor;
        if cursor.row == self.margins.top
            && cursor.col >= self.margins.left
            && cursor.col <= self.margins.right
        {
            self.scroll_down(1);
        } else {
            self.cursor_vertical(1, false);
        }
    }

    fn index_scroll(&mut self) {
        self.ensure_active_columns();
        let m = self.margins;
        let cols = usize::from(self.cols);
        let full = m.left == 0 && m.right == cols - 1;
        if full && self.rows == 1 && self.screen().limits.bytes == Some(0) {
            let screen = self.screen_mut();
            let background = screen.cursor.style.background;
            screen.edit_row(0, |row| {
                *row = Row::new(row.id, row.cells.len(), background)
            });
            return;
        }
        if !full || (m.top == 0 && (self.screen().limits.bytes != Some(0) || m.bottom == 0)) {
            self.scroll_up(1, true);
            if full && self.screen().limits.bytes != Some(0) {
                let screen = self.screen_mut();
                let bottom = screen.history.len() + m.bottom;
                let at_end = m.bottom + 1 == screen.rows.len();
                let columns = usize::from(if at_end {
                    screen.pages.pages.back().unwrap().columns
                } else {
                    screen.pages.page_at(bottom).0.columns
                });
                if screen.rows[m.bottom].cells.len() != columns {
                    screen.rows[m.bottom] = Row::new(
                        screen.rows[m.bottom].id,
                        columns,
                        screen.cursor.style.background,
                    );
                }
                if at_end {
                    return;
                }
                // History insertion moves the suffix down through the page
                // list. At each boundary, native code copies into that page's
                // recycled last row (or a fresh blank row after growing).
                let mut page_start = 0;
                let mut copies = Vec::new();
                for page in &screen.pages.pages {
                    let page_end = page_start + usize::from(page.rows);
                    if page_start > bottom {
                        let target = page_start - screen.history.len();
                        let columns = usize::from(page.columns);
                        let row = &screen.rows[target];
                        if row.cells.len() != columns || cols < columns {
                            copies.push((
                                target,
                                screen.prepare_row_copy(
                                    target,
                                    (page_end - screen.history.len() < screen.rows.len())
                                        .then_some(page_end - screen.history.len()),
                                    columns,
                                    cols,
                                ),
                            ));
                        }
                    }
                    page_start = page_end;
                }
                screen.install_row_copies(copies, None);
            }
            return;
        }

        // Full-width IND rotates complete rows, unlike SU/DL which detach
        // wrapped lines and keep pins at their physical coordinates.
        let screen = self.screen_mut();
        let top = screen.history.len() + m.top;
        let bottom = screen.history.len() + m.bottom;
        let (page, page_row) = screen.pages.page_at(top);
        let mut copies = Vec::new();
        if top - page_row + usize::from(page.rows) <= bottom {
            let mut page_start = 0;
            for page in &screen.pages.pages {
                let page_end = page_start + usize::from(page.rows);
                if page_end > bottom {
                    break;
                }
                if page_end > top {
                    let source = page_end - screen.history.len();
                    let columns = usize::from(page.columns);
                    let row = &screen.rows[source];
                    if row.cells.len() != columns {
                        let recycled = page_start.max(top) - screen.history.len();
                        copies.push((
                            source - 1,
                            screen.prepare_row_copy(source, Some(recycled), columns, columns),
                        ));
                    }
                }
                page_start = page_end;
            }
        }
        let erased = screen.rows[m.top].id;
        screen.pages.invalidate_layout(top, bottom);
        let replacement = if page_row == 0 {
            screen.rows[m.top + 1].id
        } else {
            screen.all_rows().nth(top - 1).unwrap().id
        };
        for point in screen.grid_points_mut() {
            if point.row == erased {
                point.row = replacement;
                if page_row == 0 {
                    point.col = 0;
                }
            }
        }
        let blank = screen.blank_row(
            screen.rows[m.bottom].cells.len(),
            screen.cursor.style.background,
        );
        let row = screen.rows.remove(m.top);
        screen.rows.insert(m.bottom, blank);
        screen.discard_row(row.id);
        screen.install_row_copies(copies, Some(row));
        screen.sync_style_pages(false);
    }

    fn scroll_up(&mut self, count: usize, history: bool) {
        self.ensure_active_columns();
        let m = self.margins;
        let count = count.max(1).min(m.bottom - m.top + 1);
        let cols = self.cols as usize;
        let bg = self.screen().cursor.style.background;
        let full = m.left == 0 && m.right == cols - 1;
        let alternate = self.alternate_active;
        let shift_history =
            history && m.top == 0 && full && (!alternate || m.bottom + 1 == usize::from(self.rows));
        if !shift_history {
            self.prepare_row_shift();
        }
        for _ in 0..count {
            if full {
                let screen = self.screen_mut();
                let blank = screen.blank_row(cols, bg);
                let pins = if !shift_history {
                    // IL/DL-style movement copies contents between physical
                    // rows without moving their tracked coordinates.
                    (m.top..=m.bottom)
                        .map(|y| {
                            (
                                screen.rows[y].id,
                                if y == m.bottom {
                                    blank.id
                                } else {
                                    screen.rows[y + 1].id
                                },
                            )
                        })
                        .collect()
                } else if alternate {
                    // The no-scrollback fast path clamps pins scrolled off
                    // the top to the first surviving row.
                    [(
                        screen.rows[0].id,
                        screen.rows.get(1).map_or(blank.id, |row| row.id),
                    )]
                    .into_iter()
                    .collect()
                } else {
                    // Native history insertion shifts the whole page list.
                    // Restoring content below the margin leaves pins in place.
                    (m.bottom + 1..screen.rows.len())
                        .map(|y| {
                            (
                                screen.rows[y].id,
                                if y == m.bottom + 1 {
                                    blank.id
                                } else {
                                    screen.rows[y - 1].id
                                },
                            )
                        })
                        .collect()
                };
                screen.remap_grid_rows(&pins);
                let row = screen.rows.remove(m.top);
                screen.rows.insert(m.bottom, blank);
                if shift_history {
                    screen.push_history(row);
                    if screen.limits.bytes == Some(0) {
                        screen.pages.invalidate_layout(
                            screen.history.len() + m.top,
                            screen.history.len() + m.bottom,
                        );
                    } else if m.bottom + 1 < screen.rows.len() {
                        screen.pages.invalidate_layout(
                            screen.history.len() + m.bottom + 1,
                            screen.history.len() + screen.rows.len() - 1,
                        );
                    }
                } else {
                    screen.release_row_styles(&row);
                    screen.discard_row(row.id);
                }
                screen.sync_style_pages(false);
            } else {
                for y in m.top..m.bottom {
                    self.copy_row_region(y + 1, y, m.left, m.right + 1, bg);
                }
                self.screen_mut()
                    .erase_row_cells(m.bottom, m.left, m.right + 1, bg, false);
            }
        }
        self.clamp_cursor();
        self.changed();
    }

    fn scroll_down(&mut self, count: usize) {
        self.ensure_active_columns();
        let m = self.margins;
        let cols = self.cols as usize;
        let count = count.max(1).min(m.bottom - m.top + 1);
        let bg = self.screen().cursor.style.background;
        self.prepare_row_shift();
        for _ in 0..count {
            if m.left == 0 && m.right == cols - 1 {
                let screen = self.screen_mut();
                let blank = screen.blank_row(cols, bg);
                let pins = (m.top..=m.bottom)
                    .map(|y| {
                        (
                            screen.rows[y].id,
                            if y == m.top {
                                blank.id
                            } else {
                                screen.rows[y - 1].id
                            },
                        )
                    })
                    .collect();
                screen.remap_grid_rows(&pins);
                let row = screen.rows.remove(m.bottom);
                screen.release_row_styles(&row);
                screen.discard_row(row.id);
                screen.rows.insert(m.top, blank);
                screen.sync_style_pages(true);
            } else {
                for y in (m.top + 1..=m.bottom).rev() {
                    self.copy_row_region(y - 1, y, m.left, m.right + 1, bg);
                }
                self.screen_mut()
                    .erase_row_cells(m.top, m.left, m.right + 1, bg, false);
            }
        }
        self.clamp_cursor();
        self.changed();
    }

    fn prepare_row_shift(&mut self) {
        let m = self.margins;
        let right_edge = m.right + 1 == usize::from(self.cols);
        if m.left == 0 && right_edge {
            let screen = self.screen_mut();
            screen.pages.invalidate_layout(
                screen.history.len() + m.top,
                screen.history.len() + m.bottom,
            );
        }
        for row in &mut self.screen_mut().rows[m.top..=m.bottom] {
            if m.left == 0 && right_edge {
                row.wrapped = false;
                row.wrap_continuation = false;
            }
            if (right_edge || m.left < 2)
                && let Some(cell) = row.cells.last_mut()
            {
                cell.spacer_head = false;
            }
            // Split glyphs lose their text, but the cells outside the moved
            // region retain their attributes and hyperlink identity.
            for boundary in [m.left, m.right + 1] {
                if boundary > 0 && row.cells.get(boundary).is_some_and(|cell| cell.width == 0) {
                    row.cells[boundary - 1].text.clear();
                    row.cells[boundary - 1].width = 1;
                    row.cells[boundary].width = 1;
                }
            }
            row.dirty = true;
        }
    }

    fn copy_row_region(
        &mut self,
        source: usize,
        destination: usize,
        start: usize,
        end: usize,
        background: Color,
    ) {
        self.screen_mut()
            .copy_row_cells(source, destination, start, end, background);
    }

    fn tab(&mut self, count: usize, backward: bool) {
        for _ in 0..count.min(self.cols as usize) {
            let col = self.screen().cursor.col;
            let next = if backward {
                let left = if self.modes.dec(6) {
                    self.margins.left
                } else {
                    0
                };
                if col <= left {
                    break;
                }
                (left..col)
                    .rev()
                    .find(|&i| self.tabstops[i])
                    .unwrap_or(left)
            } else {
                let right = self.margins.right;
                if col >= right {
                    break;
                }
                (col + 1..=right)
                    .find(|&i| self.tabstops[i])
                    .unwrap_or(right)
            };
            self.screen_mut().cursor.col = next;
            self.ensure_row_cells(self.screen().cursor.row, next + 1);
        }
        self.changed();
    }

    fn insert_blanks(&mut self, count: usize) {
        self.screen_mut().cursor.pending_wrap = false;
        let cur = self.screen().cursor.clone();
        if cur.col < self.margins.left || cur.col > self.margins.right {
            return;
        }
        self.ensure_row_cells(cur.row, usize::from(self.cols));
        let end = self.margins.right + 1;
        let count = count.max(1).min(end - cur.col);
        self.screen_mut().edit_row(cur.row, |row| {
            row.cells[cur.col..end].rotate_right(count);
            row.cells[cur.col..cur.col + count].fill(Cell::blank(cur.style.background));
            row.repair_wide(cur.style.background);
        });
        self.changed();
    }

    fn delete_chars(&mut self, count: usize) {
        let cur = self.screen().cursor.clone();
        if cur.col < self.margins.left || cur.col > self.margins.right {
            return;
        }
        self.ensure_row_cells(cur.row, usize::from(self.cols));
        let end = self.margins.right + 1;
        let count = count.max(1).min(end - cur.col);
        self.screen_mut().split_cell_boundary(cur.col);
        self.screen_mut().split_cell_boundary(cur.col + count);
        self.screen_mut().split_cell_boundary(end);
        self.screen_mut().edit_row(cur.row, |row| {
            row.cells[cur.col..end].rotate_left(count);
            row.cells[end - count..end].fill(Cell::blank(cur.style.background));
            row.repair_wide(cur.style.background);
        });
        self.screen_mut().cursor_reset_wrap();
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
        self.ensure_row_cells(cursor.row, cols);
        if mode != 1 {
            self.screen_mut().cursor_reset_wrap();
        }
        self.screen_mut().erase_row_cells(
            cursor.row,
            start,
            end,
            cursor.style.background,
            protected,
        );
        self.screen_mut().cursor.pending_wrap = false;
        self.changed();
    }

    pub fn erase_display(&mut self, mode: u16, protected: bool) {
        if matches!(mode, 0..=2 | 22) {
            self.ensure_active_columns();
        }
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
                if !self.alternate_active
                    && self
                        .screen()
                        .rows
                        .last()
                        .is_some_and(|row| row.semantic != SemanticContent::Output)
                {
                    self.scroll_clear();
                }
                let cell = [
                    self.width_px / u32::from(self.cols),
                    self.height_px / u32::from(self.rows),
                ];
                self.screen_mut().clear_visible_images(cell);
                (0, rows)
            }
            3 => {
                self.screen_mut().clear_history();
                self.changed();
                return;
            }
            22 => {
                self.scroll_clear();
                return;
            }
            _ => return,
        };
        for y in start..end {
            self.screen_mut()
                .erase_row_cells(y, 0, usize::MAX, cursor.style.background, protected);
            let row = &mut self.screen_mut().rows[y];
            if !protected {
                row.wrapped = false;
                row.wrap_continuation = false;
                row.semantic = SemanticContent::Output;
            }
        }
        self.screen_mut().cursor.pending_wrap = false;
        self.changed();
    }

    fn scroll_clear(&mut self) {
        self.ensure_active_columns();
        let cell = [
            self.width_px / u32::from(self.cols),
            self.height_px / u32::from(self.rows),
        ];
        let cols = usize::from(self.cols);
        let count = self
            .screen()
            .rows
            .iter()
            .rposition(|row| {
                row.cells.iter().take(cols).any(|cell| {
                    !cell.text.is_empty()
                        || cell.width != 1
                        || cell.spacer_head
                        || cell.style.background != Color::Default
                })
            })
            .map_or(0, |row| row + 1);
        let screen = self.screen_mut();
        for _ in 0..count {
            let row = screen.rows.remove(0);
            let blank = screen.blank_row(cols, Color::Default);
            screen.rows.push(blank);
            screen.retain_history(row);
        }
        if screen.cursor.row < count {
            screen.cursor.row = 0;
            screen.cursor.col = 0;
        } else {
            screen.cursor.row -= count;
        }
        screen.cursor.pending_wrap = false;
        screen.clear_visible_images(cell);
        self.clamp_cursor();
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
        let cols = self.cols as usize;
        let rows = self.rows as usize;
        let screen = self.screen_mut();
        // DECRC restores only saved attributes. Hyperlinks, semantic content,
        // and cursor appearance retain their current state.
        screen.set_cursor_style(saved.cursor.style);
        screen.cursor.col = saved.cursor.col.min(cols - 1);
        screen.cursor.row = saved.cursor.row.min(rows - 1);
        screen.cursor.protected = saved.cursor.protected;
        screen.cursor.pending_wrap = saved.cursor.pending_wrap;
        screen.charset = saved.charset;
        self.ensure_row_cells(self.screen().cursor.row, self.screen().cursor.col + 1);
        self.modes.set(true, 6, saved.origin);
        self.changed();
    }

    pub fn set_mode(&mut self, private: bool, mode: u16, value: bool) {
        if !self.modes.set(private, mode, value) {
            return;
        }
        if private {
            match mode {
                3 => {
                    if self.modes.dec(40) {
                        self.resize(if value { 132 } else { 80 }, self.rows);
                        self.erase_display(2, false);
                        self.cursor_position(1, 1);
                    } else {
                        self.modes.set(true, 3, false);
                    }
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

    fn set_stream_mode(
        &mut self,
        private: bool,
        mode: u16,
        value: bool,
        effects: &mut Vec<Effect>,
    ) {
        self.set_mode(private, mode, value);
        if private && value {
            match mode {
                2048 => effects.push(Effect::Query(Query::Size(query::SizeStyle::InBand))),
                2033 => effects.push(Effect::Write(query::visibility(self.visible))),
                _ => {}
            }
        }
    }

    fn switch_screen(&mut self, mode: u16, enabled: bool) {
        if mode == 1049 && enabled {
            self.save_cursor();
        }
        if mode == 1047 && !enabled && self.alternate_active {
            self.erase_display(2, false);
        }
        if self.alternate_active == enabled {
            if mode == 1049 {
                if enabled {
                    self.erase_display(2, false);
                } else {
                    self.restore_cursor();
                }
            }
            return;
        }
        let old_cursor = self.screen().cursor.clone();
        let clear_eol = self.screen().metadata.cursor_clear_eol;
        let hyperlink_implicit_id = self.screen().metadata.hyperlink_implicit_id;
        let charset = self.screen().charset.clone();
        self.end_hyperlink();
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
        // Mode 1049 restores the primary cursor on exit without copying the
        // alternate cursor's appearance. Legacy modes copy in both directions.
        if mode != 1049 || enabled {
            self.screen_mut().cursor = old_cursor;
            self.screen_mut().metadata.cursor_clear_eol = clear_eol;
            self.screen_mut().metadata.hyperlink_implicit_id = hyperlink_implicit_id;
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
            ([], b'Z') => effects.push(Effect::Query(Query::DeviceAttributes(
                query::AttributeKind::Primary,
            ))),
            ([], b'H') => {
                let col = self.screen().cursor.col;
                self.tabstops[col] = true;
            }
            ([], b'c') => {
                self.reset();
                self.clipboard.clear_grants();
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
            ([b'#'], b'8') => {
                self.ensure_active_columns();
                let style = Style {
                    foreground: self.screen().cursor.style.foreground,
                    background: self.screen().cursor.style.background,
                    ..Style::default()
                };
                self.screen_mut().release_cursor_style();
                self.screen_mut().cursor.style = style;
                self.screen_mut().sync_cursor_style();
                self.modes.set(true, 6, false);
                self.reset_margins();
                self.cursor_position(1, 1);
                for y in 0..usize::from(self.rows) {
                    self.screen_mut()
                        .erase_row_cells(y, 0, usize::MAX, Color::Default, false);
                }
                for y in 0..usize::from(self.rows) {
                    self.screen_mut().cursor.row = y;
                    self.screen_mut().sync_cursor_style();
                    let count = self.screen().rows[y].cells.len();
                    let mut style_id = 0;
                    for _ in 0..count {
                        style_id = self.screen_mut().retain_cursor_style_for_cell();
                    }
                    let style = self.screen().cursor.style;
                    let row = &mut self.screen_mut().rows[y];
                    row.cells.fill(Cell {
                        style_id,
                        text: "E".into(),
                        style,
                        ..Cell::default()
                    });
                    row.wrapped = false;
                    row.wrap_continuation = false;
                    row.semantic = SemanticContent::Output;
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

    fn csi(
        &mut self,
        i: &[u8],
        p: &[u16],
        sep: u32,
        byte: u8,
        effects: &mut Vec<Effect>,
        clipboard_read_enabled: bool,
    ) {
        if sep != 0 && byte != b'm' {
            return;
        }
        // Validate before dispatch: excess parameters invalidate the whole
        // command, rather than applying just the first value(s).
        match (i, byte, p.len()) {
            (
                [],
                b'@'
                | b'A'..=b'G'
                | b'I'..=b'M'
                | b'P'
                | b'S'
                | b'T'
                | b'W'
                | b'X'
                | b'Z'
                | b'`'
                | b'a'
                | b'b'
                | b'd'
                | b'e'
                | b'j'
                | b'k',
                2..,
            )
            | ([], b'H' | b'f' | b'r' | b's', 3..)
            | ([], b'g', 0 | 2..)
            | ([b'?'], b'J' | b'K', 2..)
            | ([b'"'], b'q', 2..)
            | ([b'>'], b'm', 3..) => return,
            _ => {}
        }
        let n = p.first().copied().unwrap_or(0);
        let count = usize::from(n.max(1));
        let second = p.get(1).copied().unwrap_or(0);
        let private = i.first() == Some(&b'?');
        match (i, byte) {
            ([], b'A' | b'k') => self.cursor_vertical(count, false),
            ([], b'B') => self.cursor_vertical(count, true),
            ([], b'C') => self.cursor_right(count),
            ([], b'D' | b'j') => self.cursor_left(count),
            ([], b'E' | b'F') => {
                self.cursor_vertical(count, byte == b'E');
                self.carriage_return();
            }
            ([], b'G' | b'`') => self.cursor_position(self.screen().cursor.row + 1, count),
            ([], b'd') => self.cursor_position(count, self.screen().cursor.col + 1),
            ([], b'a' | b'e') => {
                // HPR/VPR retain an explicit zero and apply the same origin
                // offsets and bounds as absolute cursor positioning.
                let amount = usize::from(p.first().copied().unwrap_or(1));
                let cursor = &self.screen().cursor;
                self.cursor_position(
                    cursor.row + 1 + if byte == b'e' { amount } else { 0 },
                    cursor.col + 1 + if byte == b'a' { amount } else { 0 },
                );
            }
            ([], b'H' | b'f') => self.cursor_position(count, second.max(1).into()),
            ([], b'I' | b'Z') => {
                self.tab(usize::from(p.first().copied().unwrap_or(1)), byte == b'Z')
            }
            ([], b'g') => match n {
                0 => {
                    let col = self.screen().cursor.col;
                    self.tabstops[col] = false;
                }
                3 => self.tabstops.fill(false),
                _ => {}
            },
            ([], b'W') => match n {
                0 | 2 => {
                    let col = self.screen().cursor.col;
                    self.tabstops[col] = n == 0;
                }
                5 => self.tabstops.fill(false),
                _ => {}
            },
            ([b'?'], b'W') if p == [5] => {
                for (col, set) in self.tabstops.iter_mut().enumerate() {
                    *set = col > 0 && col % 8 == 0;
                }
            }
            ([], b'J') | ([b'?'], b'J') => self.erase_display(n, private),
            ([], b'K') | ([b'?'], b'K') => self.erase_line(n, private),
            ([], b'@') => self.insert_blanks(count),
            ([], b'P') => self.delete_chars(count),
            ([], b'X') => {
                self.ensure_row_cells(self.screen().cursor.row, usize::from(self.cols));
                let cur = self.screen().cursor.clone();
                let protected = self.screen().iso_protection;
                let end = cur.col.saturating_add(count).min(self.cols as usize);
                self.screen_mut().split_cell_boundary(cur.col);
                self.screen_mut().split_cell_boundary(end);
                self.screen_mut().cursor_reset_wrap();
                self.screen_mut().erase_row_cells(
                    cur.row,
                    cur.col,
                    end,
                    cur.style.background,
                    protected,
                );
                self.changed();
            }
            ([], b'L' | b'M') => {
                let cur = self.screen().cursor.clone();
                if (p.is_empty() || n != 0)
                    && cur.row >= self.margins.top
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
            ([], b'S' | b'T') => {
                // Scrolling defaults to one only when the parameter is omitted.
                if p.is_empty() || n != 0 {
                    if byte == b'S' {
                        self.scroll_up(count, true);
                    } else {
                        self.scroll_down(count);
                    }
                }
            }
            ([], b'b') => {
                if let Some(cp) = self.previous_char {
                    for _ in 0..count {
                        self.print(cp);
                    }
                }
            }
            ([], b'm') => {
                sgr(self.screen_mut(), p, sep);
                self.changed();
            }
            ([b'>'], b'm') => self.modify_other_keys = n == 4 && second == 2,
            ([b'>'], b'n') => self.modify_other_keys = false,
            ([], b'h' | b'l') | ([b'?'], b'h' | b'l') => {
                for &mode in p {
                    self.set_stream_mode(private, mode, byte == b'h', effects);
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
                    self.set_stream_mode(true, mode, value, effects);
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
            ([b' '], b'q') => {
                if p.len() <= 1 && n <= 6 {
                    self.set_cursor_style(n);
                }
            }
            ([b'"'], b'q') if n <= 2 => {
                let screen = self.screen_mut();
                screen.cursor.protected = n == 1;
                if n == 1 {
                    screen.iso_protection = false;
                    screen.metadata.protected_mode = 2;
                }
            }
            // The pinned libghostty stream ignores DECSTR and ANSI DECRQM.
            ([b'!'] | [b'$'], b'p') => {}
            ([b'?', b'$'], b'p') => {
                if p.len() != 1 {
                    return;
                }
                let report = if n == 5522 && !clipboard_read_enabled {
                    0
                } else {
                    self.modes.report(true, n)
                };
                effects.push(Effect::Write(
                    format!("\x1b[?{};{report}$y", n & 0x7fff).into_bytes(),
                ));
            }
            ([], b'n') | ([b'?'], b'n') if p.len() == 1 => match (private, n) {
                (false, 5) => effects.push(Effect::Write(b"\x1b[0n".to_vec())),
                (false, 6) => {
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
                    effects.push(Effect::Write(format!("\x1b[{row};{col}R").into_bytes()));
                }
                (true, 996) => effects.push(Effect::Query(Query::ColorScheme)),
                (true, 998) => effects.push(Effect::Write(query::visibility(self.visible))),
                _ => {}
            },
            ([], b'c') => effects.push(Effect::Query(Query::DeviceAttributes(
                query::AttributeKind::Primary,
            ))),
            ([b'>'], b'c') => effects.push(Effect::Query(Query::DeviceAttributes(
                query::AttributeKind::Secondary,
            ))),
            ([b'='], b'c') => effects.push(Effect::Query(Query::DeviceAttributes(
                query::AttributeKind::Tertiary,
            ))),
            ([b'>'], b'q') => effects.push(Effect::Query(Query::Xtversion)),
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
                14 if p.len() == 1 => {
                    effects.push(Effect::Query(Query::Size(query::SizeStyle::TextPixels)))
                }
                16 if p.len() == 1 => {
                    effects.push(Effect::Query(Query::Size(query::SizeStyle::CellPixels)))
                }
                18 if p.len() == 1 => {
                    effects.push(Effect::Query(Query::Size(query::SizeStyle::Cells)))
                }
                21 if p.len() == 1 && self.title_report => {
                    let mut reply = b"\x1b]l".to_vec();
                    reply.extend_from_slice(self.title_bytes());
                    reply.extend_from_slice(b"\x1b\\");
                    effects.push(Effect::Write(reply));
                }
                22 | 23 => {}
                _ => {}
            },
            _ => effects.push(Effect::UnknownSequence(format!(
                "CSI {:?} {:?} {}",
                i, p, byte as char
            ))),
        }
    }

    fn osc(
        &mut self,
        data: &[u8],
        bell: bool,
        effects: &mut Vec<Effect>,
        clipboard_write_enabled: bool,
        clipboard_read_enabled: bool,
    ) {
        let split = data.iter().position(|&b| b == b';').unwrap_or(data.len());
        let Ok(number) = std::str::from_utf8(&data[..split])
            .unwrap_or("")
            .parse::<u16>()
        else {
            return;
        };
        if data[..split] != *number.to_string().as_bytes() {
            return;
        }
        let has_separator = split < data.len();
        if matches!(number, 4 | 5 | 10..=19 | 21 | 104 | 105 | 110..=119) {
            let reply = crate::color::osc(
                self,
                number,
                data.get(split + 1..).unwrap_or_default(),
                bell,
            );
            if !reply.is_empty() {
                effects.push(Effect::Write(reply));
            }
            return;
        }
        let data = data.get(split + 1..).unwrap_or_default();
        match number {
            // These fixed captures append a NUL before dispatch.
            0 | 2 | 7 | 8 | 777 | 1337 if !has_separator || data.len() >= 2048 => return,
            9 | 133 if !has_separator || data.len() > 2048 => return,
            // Allocating captures count only bytes after the numeric prefix.
            // OSC 52 and 66 reserve a byte for the parser's trailing NUL.
            52 | 66 if !has_separator || data.len() >= MAX_OSC_BYTES => return,
            72 | 99 | 5522 if !has_separator || data.len() > MAX_OSC_BYTES => return,
            _ => {}
        }
        match number {
            0 | 2 => {
                // The stream validates the full title before the host handler
                // keeps its first 1024 bytes, even across a UTF-8 boundary.
                if std::str::from_utf8(data).is_err() {
                    return;
                }
                self.set_title(&data[..data.len().min(1024)]);
                effects.push(Effect::Title(self.title_bytes().to_vec()));
            }
            7 => {
                self.set_working_directory(data);
                effects.push(Effect::WorkingDirectory(data.to_vec()));
            }
            8 => {
                if let Some(split) = data.iter().position(|&b| b == b';') {
                    let uri = &data[split + 1..];
                    let params = &data[..split];
                    let mut explicit = None;
                    let mut start = 0;
                    while start < params.len() {
                        // Native option traversal searches past the first
                        // byte, so an empty field prefixes the next option.
                        let end = params[start + 1..]
                            .iter()
                            .position(|&byte| matches!(byte, b':' | 0))
                            .map_or(params.len(), |offset| start + 1 + offset);
                        let option = &params[start..end];
                        let Some(equal) = option.iter().position(|&byte| byte == b'=') else {
                            break;
                        };
                        if option[..equal] == *b"id" && equal + 1 < option.len() {
                            explicit = Some(&option[equal + 1..]);
                        }
                        start = end + 1;
                    }
                    if uri.is_empty() && explicit.is_some() {
                        return;
                    }
                    self.end_hyperlink();
                    if !uri.is_empty() {
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
            9 => self.osc9(data, effects),
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
            1337 => {
                if let Some(split) = data.iter().position(|&byte| byte == b'=')
                    && data[..split].eq_ignore_ascii_case(b"CurrentDir")
                    && split + 1 < data.len()
                {
                    let directory = &data[split + 1..];
                    self.set_working_directory(directory);
                    effects.push(Effect::WorkingDirectory(directory.to_vec()));
                }
            }
            52 => {
                if let Some(split @ 0..=1) = data.iter().position(|&b| b == b';') {
                    let location =
                        clipboard::Location::from_selector(if split == 0 { b'c' } else { data[0] });
                    let encoded = &data[split + 1..];
                    if encoded == b"?" {
                        effects.push(Effect::ClipboardRead(clipboard::Read::osc52(
                            location,
                            if bell {
                                clipboard::Terminator::Bell
                            } else {
                                clipboard::Terminator::St
                            },
                        )));
                    } else {
                        let engine = base64::engine::general_purpose::GeneralPurpose::new(
                            &base64::alphabet::STANDARD,
                            base64::engine::general_purpose::GeneralPurposeConfig::new()
                                .with_decode_allow_trailing_bits(true)
                                .with_decode_padding_mode(if encoded.ends_with(b"=") {
                                    base64::engine::DecodePaddingMode::RequireCanonical
                                } else {
                                    base64::engine::DecodePaddingMode::RequireNone
                                }),
                        );
                        if let Ok(data) = engine.decode(encoded) {
                            let contents = if encoded.is_empty() {
                                Vec::new()
                            } else {
                                vec![clipboard::Content {
                                    mime: b"text/plain".to_vec(),
                                    data: data.into(),
                                }]
                            };
                            effects.push(Effect::ClipboardWrite(clipboard::Write::osc52(
                                location, contents,
                            )));
                        }
                    }
                }
            }
            5522 => self.clipboard.handle(
                data,
                if bell {
                    clipboard::Terminator::Bell
                } else {
                    clipboard::Terminator::St
                },
                self.clipboard_write_limit,
                clipboard_write_enabled,
                clipboard_read_enabled,
                effects,
            ),
            72 => dnd::handle(&mut self.kitty_dnd, data, bell, effects),
            133 => self.osc133(data, effects),
            1 | 22 => {}
            _ => effects.push(Effect::UnknownSequence(format!("OSC {number}"))),
        }
    }

    fn semantic_fresh_line(&mut self) {
        let col = self.screen().cursor.col;
        let left = if col < self.margins.left {
            0
        } else {
            self.margins.left
        };
        if col != left {
            self.carriage_return();
            self.index();
        }
    }

    fn semantic_prompt(&mut self, continuation: bool) {
        let screen = self.screen_mut();
        screen.cursor.semantic = SemanticContent::Prompt;
        screen.metadata.cursor_clear_eol = false;
        screen.rows[screen.cursor.row].semantic = if continuation {
            SemanticContent::Input
        } else {
            SemanticContent::Prompt
        };
    }

    fn osc133(&mut self, data: &[u8], effects: &mut Vec<Effect>) {
        let Some(&action) = data.first() else { return };
        if data.len() > 1 && (action == b'L' || data[1] != b';') {
            return;
        }
        let options = data.get(2..).unwrap_or_default();
        // The first matching key wins, even when its value is invalid.
        let option = |prefix| {
            options
                .split(|&byte| byte == b';')
                .find_map(|part| part.strip_prefix(prefix))
        };
        match action {
            b'L' => self.semantic_fresh_line(),
            b'A' | b'N' | b'P' => {
                if action != b'P' {
                    self.semantic_fresh_line();
                }
                self.semantic_prompt(matches!(option(b"k=".as_slice()), Some(b"c" | b"s")));
                if action != b'P' {
                    if let Some(redraw) = match option(b"redraw=") {
                        Some(b"0") => Some(1),
                        Some(b"1") => Some(0),
                        Some(b"last") => Some(2),
                        _ => None,
                    } {
                        self.metadata.shell_redraw = redraw;
                    }
                    let click = match option(b"click_events=") {
                        Some(b"1") => Some([1, 0]),
                        Some(b"2") => Some([1, 1]),
                        _ => match option(b"cl=") {
                            Some(b"line") => Some([2, 0]),
                            Some(b"m") => Some([2, 1]),
                            Some(b"v") => Some([2, 2]),
                            Some(b"w") => Some([2, 3]),
                            _ => None,
                        },
                    };
                    if let Some(click) = click {
                        self.screen_mut().metadata.semantic_click = click;
                    }
                }
            }
            b'B' | b'I' => {
                let screen = self.screen_mut();
                screen.cursor.semantic = SemanticContent::Input;
                screen.metadata.cursor_clear_eol = action == b'I';
            }
            b'C' | b'D' => {
                let screen = self.screen_mut();
                screen.cursor.semantic = SemanticContent::Output;
                screen.metadata.cursor_clear_eol = false;
                if action == b'C' && screen.cursor.col == 0 {
                    screen.rows[screen.cursor.row].semantic = SemanticContent::Output;
                }
                if self.shell_command_events {
                    effects.push(if action == b'C' {
                        Effect::CommandStart
                    } else {
                        Effect::CommandEnd {
                            exit_code: options
                                .split(|&byte| byte == b';')
                                .next()
                                .and_then(|value| std::str::from_utf8(value).ok())
                                .and_then(|value| value.parse().ok()),
                        }
                    });
                }
            }
            _ => {}
        }
    }

    fn osc9(&mut self, data: &[u8], effects: &mut Vec<Effect>) {
        if data.starts_with(b"12") {
            self.semantic_fresh_line();
            self.semantic_prompt(false);
            return;
        }
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
            return;
        }
        if let Some(directory) = data.strip_prefix(b"9;") {
            if data.len() < 2048 {
                self.set_working_directory(directory);
                effects.push(Effect::WorkingDirectory(directory.to_vec()));
            }
            return;
        }
        if matches!(
            data,
            [b'1', b';', ..]
                | [b'1', b'0']
                | [b'1', b'0', b';', b'0'..=b'3', ..]
                | [b'1', b'1', b';', ..]
                | [b'2' | b'3' | b'6' | b'7' | b'8', b';', ..]
                | [b'5', ..]
        ) {
            // Recognized ConEmu extensions are ignored by libghostty.
            return;
        }
        if data.len() < 2048 {
            effects.push(Effect::Notification {
                title: Vec::new(),
                body: data.to_vec(),
            });
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
                    name.iter()
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
    if matches!(set, Charset::British | Charset::DecSpecial) && cp as u32 > 255 {
        return ' ';
    }
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

fn sgr(screen: &mut Screen, params: &[u16], separators: u32) {
    let mut style = screen.cursor.style;
    if params.is_empty() {
        screen.set_cursor_style(Style::default());
        return;
    }
    let is_colon = |index: usize| separators & (1 << index) != 0;
    let colon_count = |start: usize| {
        (start..params.len() - 1)
            .take_while(|&index| is_colon(index))
            .count()
    };
    let mut i = 0;
    while i < params.len() {
        let slice = &params[i..];
        let n = slice[0];
        let colon = is_colon(i);
        i += 1;
        if colon && !matches!(n, 4 | 38 | 48 | 58) {
            // Unknown colon groups form one attribute. Their values must not
            // accidentally reset or enable unrelated styles.
            while is_colon(i) {
                i += 1;
            }
            i += 1;
            continue;
        }
        match n {
            0 => style = Style::default(),
            1 => style.bold = true,
            2 => style.faint = true,
            3 => style.italic = true,
            4 => {
                let kind = if colon {
                    if slice.len() < 2 {
                        continue;
                    }
                    if is_colon(i) {
                        i += colon_count(i) + 1;
                        continue;
                    }
                    i += 1;
                    slice[1]
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
                let color = match slice.get(1) {
                    Some(5) if slice.len() >= 3 => {
                        i += 2;
                        Some(Color::Indexed(slice[2] as u8))
                    }
                    Some(2) if slice.len() >= 5 => {
                        let start = if colon {
                            match colon_count(i) {
                                3 => 2,
                                4 => 3, // Skip the optional colorspace.
                                count => {
                                    i += count + 1;
                                    continue;
                                }
                            }
                        } else {
                            2
                        };
                        i += start + 2;
                        Some(Color::Rgb(
                            slice[start] as u8,
                            slice[start + 1] as u8,
                            slice[start + 2] as u8,
                        ))
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
        screen.set_cursor_style(style);
        style = screen.cursor.style;
    }
}
