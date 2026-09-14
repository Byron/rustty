//! Plain text, VT and HTML exports with optional terminal/screen state replay.
use crate::{Color, GridPoint, Row, Screen, Selection, Style, Terminal, Underline};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    #[default]
    Plain,
    Vt,
    Html,
}

#[derive(Clone, Copy, Debug)]
pub struct CodepointMap<'a> {
    /// Inclusive Unicode range. The last matching entry takes precedence.
    pub range: [char; 2],
    pub replacement: Replacement<'a>,
}

#[derive(Clone, Copy, Debug)]
pub enum Replacement<'a> {
    Codepoint(char),
    String(&'a str),
}

#[derive(Clone, Copy, Debug)]
pub struct Options<'a> {
    pub emit: Format,
    pub unwrap: bool,
    pub trim: bool,
    pub background: Option<[u8; 3]>,
    pub foreground: Option<[u8; 3]>,
    /// Resolve indexed cell colors to RGB without changing terminal state.
    pub palette: Option<&'a [[u8; 3]; 256]>,
    pub codepoint_map: &'a [CodepointMap<'a>],
}
impl Default for Options<'_> {
    fn default() -> Self {
        Self {
            unwrap: true,
            ..Self::new(Format::Plain)
        }
    }
}

impl Options<'_> {
    /// Native full-export defaults. Selection convenience defaults additionally
    /// unwrap soft-wrapped rows; formatter constructors preserve screen rows.
    pub const fn new(emit: Format) -> Self {
        Self {
            emit,
            unwrap: false,
            trim: true,
            background: None,
            foreground: None,
            palette: None,
            codepoint_map: &[],
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub enum Content {
    #[default]
    All,
    None,
    Selection(Selection),
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScreenExtra {
    pub cursor: bool,
    pub style: bool,
    pub hyperlink: bool,
    pub protection: bool,
    pub kitty_keyboard: bool,
    pub charsets: bool,
}

impl ScreenExtra {
    pub const NONE: Self = Self {
        cursor: false,
        style: false,
        hyperlink: false,
        protection: false,
        kitty_keyboard: false,
        charsets: false,
    };
    pub const STYLES: Self = Self {
        style: true,
        hyperlink: true,
        ..Self::NONE
    };
    pub const ALL: Self = Self {
        cursor: true,
        style: true,
        hyperlink: true,
        protection: true,
        kitty_keyboard: true,
        charsets: true,
    };
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalExtra {
    pub palette: bool,
    pub modes: bool,
    pub scrolling_region: bool,
    pub tabstops: bool,
    pub pwd: bool,
    pub keyboard: bool,
    pub screen: ScreenExtra,
}

impl TerminalExtra {
    pub const NONE: Self = Self {
        palette: false,
        modes: false,
        scrolling_region: false,
        tabstops: false,
        pwd: false,
        keyboard: false,
        screen: ScreenExtra::NONE,
    };
    pub const STYLES: Self = Self {
        palette: true,
        screen: ScreenExtra::STYLES,
        ..Self::NONE
    };
    pub const ALL: Self = Self {
        palette: true,
        modes: true,
        scrolling_region: true,
        tabstops: true,
        pwd: true,
        keyboard: true,
        screen: ScreenExtra::ALL,
    };
}

impl Default for TerminalExtra {
    fn default() -> Self {
        Self::STYLES
    }
}

/// A physical page and coordinates within it at the time of an export.
/// Native formatting can map carried blank lines beyond a page's actual rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PagePosition {
    pub page: usize,
    pub x: u32,
    pub y: u32,
}

/// One source position per output byte, including UTF-8, escapes and extras.
/// The screen borrow keeps these positions valid until the map is dropped.
/// Coordinates use eight bytes per output byte, plus one entry per page run.
pub struct ByteMap<'a> {
    screen: &'a Screen,
    points: Vec<[u32; 2]>,
    pages: Vec<(usize, usize)>,
    current_page: usize,
}

impl ByteMap<'_> {
    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn get(&self, offset: usize) -> Option<PagePosition> {
        let &[x, y] = self.points.get(offset)?;
        let run = self.pages.partition_point(|&(start, _)| start <= offset);
        let &(_, page) = self.pages.get(run.checked_sub(1)?)?;
        Some(PagePosition { page, x, y })
    }

    /// Resolve a byte to a cell. Returns None for an out-of-range byte or a
    /// native blank-line coordinate that falls outside its physical page.
    pub fn point(&self, offset: usize) -> Option<GridPoint> {
        let position = self.get(offset)?;
        let page = self.screen.pages.pages.get(position.page)?;
        if position.x >= u32::from(page.columns) || position.y >= u32::from(page.rows) {
            return None;
        }
        let top: usize = self
            .screen
            .pages
            .pages
            .iter()
            .take(position.page)
            .map(|page| usize::from(page.rows))
            .sum();
        self.screen
            .point(top + position.y as usize, position.x as usize)
    }

    fn fill(&mut self, end: usize, point: [u32; 2]) {
        if end == self.len() {
            return;
        }
        if self.pages.last().map(|&(_, page)| page) != Some(self.current_page) {
            self.pages.push((self.len(), self.current_page));
        }
        self.points.resize(end, point);
    }
}

struct Output<'a> {
    bytes: Vec<u8>,
    map: Option<ByteMap<'a>>,
}

impl<'a> Output<'a> {
    fn new(screen: Option<&'a Screen>) -> Self {
        Self {
            bytes: Vec::new(),
            map: screen.map(|screen| ByteMap {
                screen,
                points: Vec::new(),
                pages: Vec::new(),
                current_page: 0,
            }),
        }
    }

    fn map_to(&mut self, point: [u32; 2]) {
        if let Some(map) = &mut self.map {
            map.fill(self.bytes.len(), point);
        }
    }

    fn map_last(&mut self) {
        if let Some(map) = &mut self.map {
            let last = map
                .get(map.len().saturating_sub(1))
                .unwrap_or(PagePosition {
                    page: 0,
                    x: 0,
                    y: 0,
                });
            map.current_page = last.page;
            map.fill(self.bytes.len(), [last.x, last.y]);
        }
    }
}

/// Export one screen, including history, without changing it. Extras affect
/// only VT output. HTML retains one outer wrapper per physical page.
pub struct ScreenFormatter<'a> {
    pub screen: &'a Screen,
    pub options: Options<'a>,
    pub content: Content,
    pub extra: ScreenExtra,
}

impl<'a> ScreenFormatter<'a> {
    pub fn new(screen: &'a Screen, emit: Format) -> Self {
        Self {
            screen,
            options: Options::new(emit),
            content: Content::All,
            extra: ScreenExtra::NONE,
        }
    }

    /// None indicates invalid selection bounds. No-content exports still emit
    /// requested extras; all-content exports always include the entire screen.
    pub fn format(&self) -> Option<Vec<u8>> {
        let mut out = Output::new(None);
        self.format_into(&mut out)?;
        Some(out.bytes)
    }

    pub fn format_with_map(&self) -> Option<(Vec<u8>, ByteMap<'a>)> {
        let mut out = Output::new(Some(self.screen));
        self.format_into(&mut out)?;
        Some((out.bytes, out.map.unwrap()))
    }

    fn format_into(&self, out: &mut Output<'a>) -> Option<()> {
        match self.content {
            Content::None => {}
            Content::Selection(selection) => {
                self.screen
                    .format_selection_into(selection, self.options, out)?;
            }
            Content::All => self.screen.format_selection_into(
                Selection {
                    start: self.screen.point(0, 0)?,
                    end: self.screen.point(
                        self.screen.history.len() + self.screen.rows.len() - 1,
                        self.screen.rows.last()?.cells.len() - 1,
                    )?,
                    rectangular: false,
                },
                self.options,
                out,
            )?,
        };
        if self.options.emit == Format::Vt {
            screen_extra(&mut out.bytes, self.screen, self.options, self.extra);
            out.map_last();
        }
        Some(())
    }
}

/// Export the currently active screen. By default styled exports include the
/// palette and current style/hyperlink; use ALL extras for state reconstruction.
/// Format both screens separately after a no-content terminal export if needed.
pub struct TerminalFormatter<'a> {
    pub terminal: &'a Terminal,
    pub options: Options<'a>,
    pub content: Content,
    pub extra: TerminalExtra,
}

impl<'a> TerminalFormatter<'a> {
    pub fn new(terminal: &'a Terminal, emit: Format) -> Self {
        Self {
            terminal,
            options: Options::new(emit),
            content: Content::All,
            extra: TerminalExtra::STYLES,
        }
    }

    pub fn format(&self) -> Option<Vec<u8>> {
        let mut out = Output::new(None);
        self.format_into(&mut out)?;
        Some(out.bytes)
    }

    pub fn format_with_map(&self) -> Option<(Vec<u8>, ByteMap<'a>)> {
        let mut out = Output::new(Some(self.terminal.screen()));
        self.format_into(&mut out)?;
        Some((out.bytes, out.map.unwrap()))
    }

    fn format_into(&self, out: &mut Output<'a>) -> Option<()> {
        let terminal = self.terminal;
        let emit = self.options.emit;
        if self.extra.palette {
            palette(&mut out.bytes, terminal, emit);
        }
        if emit == Format::Vt {
            if self.extra.modes {
                for ((private, number), current) in terminal.modes.changed() {
                    out.bytes.extend_from_slice(
                        format!(
                            "\x1b[{}{number}{}",
                            if private { "?" } else { "" },
                            if current { "h" } else { "l" }
                        )
                        .as_bytes(),
                    );
                }
            }
            if self.extra.tabstops {
                out.bytes.extend_from_slice(b"\x1b[3g");
                for (col, &enabled) in terminal.tabstops.iter().enumerate() {
                    if enabled {
                        out.bytes
                            .extend_from_slice(format!("\x1b[{}G\x1bH", col + 1).as_bytes());
                    }
                }
                out.bytes.extend_from_slice(b"\x1b[H");
            }
        }
        out.map_to([0, 0]);
        ScreenFormatter {
            screen: terminal.screen(),
            options: self.options,
            content: self.content,
            extra: ScreenExtra::NONE,
        }
        .format_into(out)?;
        if emit == Format::Vt {
            if self.extra.scrolling_region {
                let region = terminal.margins;
                if region.top != 0 || region.bottom != usize::from(terminal.rows) - 1 {
                    out.bytes.extend_from_slice(
                        format!("\x1b[{};{}r", region.top + 1, region.bottom + 1).as_bytes(),
                    );
                }
                if region.left != 0 || region.right != usize::from(terminal.cols) - 1 {
                    out.bytes.extend_from_slice(
                        format!("\x1b[{};{}s", region.left + 1, region.right + 1).as_bytes(),
                    );
                }
            }
            if self.extra.keyboard && terminal.modify_other_keys {
                out.bytes.extend_from_slice(b"\x1b[>4;2m");
            }
            if self.extra.pwd && !terminal.working_directory_bytes().is_empty() {
                out.bytes.extend_from_slice(b"\x1b]7;");
                out.bytes
                    .extend_from_slice(terminal.working_directory_bytes());
                out.bytes.extend_from_slice(b"\x1b\\");
            }
            screen_extra(
                &mut out.bytes,
                terminal.screen(),
                self.options,
                self.extra.screen,
            );
            out.map_last();
        }
        Some(())
    }
}

impl Terminal {
    pub fn formatter(&self, emit: Format) -> TerminalFormatter<'_> {
        TerminalFormatter::new(self, emit)
    }

    /// Export a selection with its palette and current style/hyperlink state.
    /// VT output can contain opaque hyperlink bytes, so it is not always UTF-8.
    /// This does not replace the active selection or change terminal state.
    pub fn format_selection(&self, selection: Selection, options: Options<'_>) -> Option<Vec<u8>> {
        TerminalFormatter {
            terminal: self,
            options,
            content: Content::Selection(selection),
            extra: TerminalExtra::STYLES,
        }
        .format()
    }
}

impl Screen {
    pub fn formatter(&self, emit: Format) -> ScreenFormatter<'_> {
        ScreenFormatter::new(self, emit)
    }

    /// Export inclusive bounds, retaining physical page breaks and native wide-cell rules.
    /// Screen exports reference palette indices; Terminal exports include the palette.
    pub fn format_selection(&self, selection: Selection, options: Options<'_>) -> Option<Vec<u8>> {
        ScreenFormatter {
            screen: self,
            options,
            content: Content::Selection(selection),
            extra: ScreenExtra::NONE,
        }
        .format()
    }

    fn format_selection_into(
        &self,
        selection: Selection,
        options: Options<'_>,
        out: &mut Output<'_>,
    ) -> Option<()> {
        let rows: Vec<_> = self.all_rows().collect();
        let position = |point: crate::GridPoint| {
            let row = rows.iter().position(|row| row.id == point.row)?;
            (point.col < rows[row].cells.len()).then_some((row, point.col))
        };
        let mut start = position(selection.start)?;
        let mut end = position(selection.end)?;
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }
        if selection.rectangular && start.1 > end.1 {
            std::mem::swap(&mut start.1, &mut end.1);
        }
        let mut offset = 0;
        let mut trailing = (0, 0);
        for (page_index, page) in self.pages.pages.iter().enumerate() {
            let count = usize::from(page.rows);
            if offset <= end.0 && offset + count > start.0 {
                let top = (
                    start.0.saturating_sub(offset),
                    if selection.rectangular || start.0 >= offset {
                        start.1
                    } else {
                        0
                    },
                );
                let bottom = (
                    (end.0 - offset).min(count - 1),
                    if selection.rectangular || end.0 < offset + count {
                        end.1
                    } else {
                        usize::from(page.columns) - 1
                    },
                );
                if let Some(map) = &mut out.map {
                    map.current_page = page_index;
                }
                trailing = format_page(
                    out,
                    &rows[offset..offset + count],
                    top,
                    bottom,
                    selection.rectangular,
                    options,
                    trailing,
                );
            }
            offset += count;
            if offset > end.0 {
                break;
            }
        }
        Some(())
    }
}

fn palette(out: &mut Vec<u8>, terminal: &Terminal, emit: Format) {
    if emit == Format::Plain {
        return;
    }
    if emit == Format::Html {
        out.extend_from_slice(b"<style>:root{");
    }
    for (index, [r, g, b]) in terminal.palette.iter().enumerate() {
        let value = match emit {
            Format::Vt => format!("\x1b]4;{index};rgb:{r:02x}/{g:02x}/{b:02x}\x1b\\"),
            Format::Html => format!("--vt-palette-{index}: #{r:02x}{g:02x}{b:02x};"),
            Format::Plain => unreachable!(),
        };
        out.extend_from_slice(value.as_bytes());
    }
    if emit == Format::Html {
        out.extend_from_slice(b"}</style>");
    }
}

fn screen_extra(out: &mut Vec<u8>, screen: &Screen, options: Options<'_>, extra: ScreenExtra) {
    let cursor = &screen.cursor;
    if extra.cursor {
        let wrapped = cursor.pending_wrap && cursor.col == screen.columns - 1;
        let col = if wrapped && screen.rows[cursor.row].cells[cursor.col].width == 0 {
            cursor.col - 1
        } else {
            cursor.col
        };
        out.extend_from_slice(format!("\x1b[{};{}H", cursor.row + 1, col + 1).as_bytes());
        if wrapped {
            let point = crate::GridPoint {
                row: screen.rows[cursor.row].id,
                col: cursor.col,
            };
            // CUP clears pending wrap. Replay the edge cell, including a wide
            // tail's leading cell, before restoring the requested cursor state.
            if let Some(bytes) = screen.format_selection(
                Selection {
                    start: point,
                    end: point,
                    rectangular: false,
                },
                options,
            ) {
                out.extend_from_slice(&bytes);
            }
        }
    }
    if extra.style {
        style_open(out, cursor.style, Format::Vt, None);
    }
    if extra.hyperlink
        && let Some(uri) = &cursor.hyperlink
    {
        out.extend_from_slice(b"\x1b]8;");
        if let Some(crate::HyperlinkId::Explicit(id)) = &cursor.hyperlink_id {
            out.extend_from_slice(b"id=");
            out.extend_from_slice(id);
        }
        out.push(b';');
        out.extend_from_slice(cursor.hyperlink_raw.as_deref().unwrap_or(uri.as_bytes()));
        out.extend_from_slice(b"\x1b\\");
    }
    if extra.protection && cursor.protected {
        out.extend_from_slice(b"\x1b[1\"q");
    }
    let flags = screen.kitty_keyboard.current();
    if extra.kitty_keyboard && flags != 0 {
        out.extend_from_slice(format!("\x1b[={flags};1u").as_bytes());
    }
    if extra.charsets {
        use crate::screen::Charset;
        for (slot, charset) in screen.charset.slots.iter().enumerate() {
            let final_byte = match charset {
                Charset::Utf8 => continue,
                Charset::Ascii => b'B',
                Charset::British => b'A',
                Charset::DecSpecial => b'0',
            };
            out.extend_from_slice(&[0x1b, b"()*+"[slot], final_byte]);
        }
        out.extend_from_slice(match screen.charset.gl {
            1 => b"\x0e",
            2 => b"\x1bn",
            3 => b"\x1bo",
            _ => b"",
        });
        out.extend_from_slice(match screen.charset.gr {
            1 => b"\x1b~",
            3 => b"\x1b|",
            _ => b"",
        });
    }
}

fn format_page(
    out: &mut Output<'_>,
    rows: &[&Row],
    start: (usize, usize),
    mut end: (usize, usize),
    rectangle: bool,
    options: Options<'_>,
    trailing: (usize, usize),
) -> (usize, usize) {
    let (mut blank_rows, mut blank_cells) = if start == (0, 0) { trailing } else { (0, 0) };
    let width = rows[0].cells.len();
    if start.1 >= width {
        return (blank_rows, blank_cells);
    }
    end.1 = end.1.min(width - 1);
    if options.unwrap
        && !rectangle
        && rows[end.0].cells[end.1].spacer_head
        && end.0 + 1 < rows.len()
    {
        end = (end.0 + 1, 0);
    }
    if start > end {
        return (blank_rows, blank_cells);
    }
    let map_base = out.bytes.len();
    if options.emit == Format::Html {
        out.bytes
            .extend_from_slice(b"<div style=\"font-family: monospace; white-space: pre;");
        for (property, color) in [
            ("background-color", options.background),
            ("color", options.foreground),
        ] {
            if let Some([r, g, b]) = color {
                out.bytes
                    .extend_from_slice(format!("{property}: #{r:02x}{g:02x}{b:02x};").as_bytes());
            }
        }
        out.bytes.extend_from_slice(b"\">");
    } else if options.emit == Format::Vt {
        for (code, color) in [(10, options.foreground), (11, options.background)] {
            if let Some([r, g, b]) = color {
                out.bytes.extend_from_slice(
                    format!("\x1b]{code};rgb:{r:02x}/{g:02x}/{b:02x}\x1b\\").as_bytes(),
                );
            }
        }
    }
    out.map_to([0, 0]);
    let mut style = Style::default();
    let mut hyperlink = None;
    for (y, row) in rows.iter().enumerate().take(end.0 + 1).skip(start.0) {
        let right = if rectangle || y == end.0 {
            end.1 + 1
        } else {
            width
        };
        let mut left = if rectangle || y == start.0 {
            start.1
        } else {
            0
        };
        if left > 0 {
            if row.cells[left].spacer_head {
                continue;
            }
            if row.cells[left].width == 0 {
                left -= 1;
            }
        }
        let cells = &row.cells[left..right];
        if cells.iter().all(|cell| cell.text.is_empty()) {
            blank_rows += 1;
            continue;
        }
        if blank_rows > 0 {
            if style != Style::default() {
                style_close(&mut out.bytes, options.emit);
                out.map_last();
                style = Style::default();
            }
            // Only this page's output can supply the preceding newline point.
            let previous = out
                .map
                .as_ref()
                .and_then(|map| map.points.get(map_base..)?.last())
                .copied()
                .unwrap_or([0, 0]);
            for offset in 0..blank_rows {
                out.bytes.extend_from_slice(if options.emit == Format::Vt {
                    b"\r\n"
                } else {
                    b"\n"
                });
                out.map_to(if offset == 0 {
                    previous
                } else {
                    [0, previous[1] + offset as u32]
                });
            }
            blank_rows = 0;
        }
        if !row.wrapped || !options.unwrap {
            blank_rows += 1;
        }
        if !row.wrap_continuation || !options.unwrap {
            blank_cells = 0;
        }
        for (index, cell) in cells.iter().enumerate() {
            let point = [(left + index) as u32, y as u32];
            if cell.width == 0 || cell.spacer_head {
                continue;
            }
            let blank = if options.emit == Format::Plain {
                cell.text.is_empty() || (options.trim && cell.text.starts_with(' '))
            } else {
                cell.text.is_empty() && cell.width == 1 && cell.style == Style::default()
            };
            if blank {
                blank_cells += 1;
                continue;
            }
            out.bytes.extend(std::iter::repeat_n(b' ', blank_cells));
            if let Some(map) = &mut out.map {
                // Native maps materialized blanks backwards from the next
                // printed cell, even when the run crosses a wrapped row.
                let mut blank = point;
                for _ in 0..blank_cells {
                    if blank[0] > 0 {
                        blank[0] -= 1;
                    } else if blank[1] > 0 {
                        blank[1] -= 1;
                        blank[0] = width as u32 - 1;
                    }
                    map.fill(map.len() + 1, blank);
                }
            }
            blank_cells = 0;
            if options.emit != Format::Plain && cell.style != style {
                if style != Style::default()
                    && (options.emit == Format::Html || cell.style == Style::default())
                {
                    style_close(&mut out.bytes, options.emit);
                    out.map_last();
                }
                style = cell.style;
                if style != Style::default() {
                    style_open(&mut out.bytes, style, options.emit, options.palette);
                    out.map_to(point);
                }
            }
            if options.emit == Format::Html {
                let link = cell.hyperlink.as_ref().map(|uri| {
                    (
                        cell.hyperlink_id.as_ref(),
                        cell.hyperlink_raw.as_deref().unwrap_or(uri.as_bytes()),
                    )
                });
                if link != hyperlink {
                    if hyperlink.is_some() {
                        out.bytes.extend_from_slice(b"</a>");
                        out.map_last();
                    }
                    hyperlink = link;
                    if let Some((_, uri)) = link {
                        out.bytes.extend_from_slice(b"<a href=\"");
                        for &byte in uri {
                            html_char(&mut out.bytes, char::from(byte));
                        }
                        out.bytes.extend_from_slice(b"\">");
                        out.map_to(point);
                    }
                }
            }
            if cell.text.is_empty() {
                out.bytes.push(b' ');
            } else if !options.codepoint_map.is_empty() || options.emit == Format::Html {
                for cp in cell.text.chars() {
                    let replacement = options
                        .codepoint_map
                        .iter()
                        .rev()
                        .find(|rule| rule.range[0] <= cp && cp <= rule.range[1])
                        .map(|rule| rule.replacement)
                        .unwrap_or(Replacement::Codepoint(cp));
                    match replacement {
                        Replacement::Codepoint(cp) => write_char(&mut out.bytes, cp, options.emit),
                        Replacement::String(text) => {
                            for cp in text.chars() {
                                write_char(&mut out.bytes, cp, options.emit);
                            }
                        }
                    }
                }
            } else {
                out.bytes.extend_from_slice(cell.text.as_bytes());
            }
            out.map_to(point);
        }
    }
    if style != Style::default() {
        style_close(&mut out.bytes, options.emit);
    }
    if hyperlink.is_some() {
        out.bytes.extend_from_slice(b"</a>");
    }
    if options.emit == Format::Html {
        out.bytes.extend_from_slice(b"</div>");
        blank_rows = blank_rows.saturating_sub(1);
    }
    out.map_last();
    (blank_rows, blank_cells)
}

fn write_char(out: &mut Vec<u8>, cp: char, emit: Format) {
    if emit == Format::Html {
        html_char(out, cp);
    } else {
        out.extend_from_slice(cp.encode_utf8(&mut [0; 4]).as_bytes());
    }
}

fn html_char(out: &mut Vec<u8>, cp: char) {
    out.extend_from_slice(match cp {
        '<' => b"&lt;",
        '>' => b"&gt;",
        '&' => b"&amp;",
        '"' => b"&quot;",
        '\'' => b"&#39;",
        cp if cp.is_ascii() => {
            out.push(cp as u8);
            return;
        }
        cp => {
            out.extend_from_slice(format!("&#{};", u32::from(cp)).as_bytes());
            return;
        }
    });
}

fn style_close(out: &mut Vec<u8>, emit: Format) {
    out.extend_from_slice(match emit {
        Format::Plain => b"",
        Format::Vt => b"\x1b[0m",
        Format::Html => b"</div>",
    });
}

fn style_open(out: &mut Vec<u8>, mut style: Style, emit: Format, palette: Option<&[[u8; 3]; 256]>) {
    if let Some(palette) = palette {
        for color in [
            &mut style.foreground,
            &mut style.background,
            &mut style.underline_color,
        ] {
            if let Color::Indexed(index) = *color {
                let [r, g, b] = palette[usize::from(index)];
                *color = Color::Rgb(r, g, b);
            }
        }
    }
    let underline = match style.underline {
        Underline::None => 0,
        Underline::Single => 1,
        Underline::Double => 2,
        Underline::Curly => 3,
        Underline::Dotted => 4,
        Underline::Dashed => 5,
    };
    if emit == Format::Vt {
        out.extend_from_slice(b"\x1b[0m");
        for (enabled, code) in [
            (style.bold, 1),
            (style.faint, 2),
            (style.italic, 3),
            (style.blink, 5),
            (style.inverse, 7),
            (style.invisible, 8),
            (style.strikethrough, 9),
            (style.overline, 53),
        ] {
            if enabled {
                out.extend_from_slice(format!("\x1b[{code}m").as_bytes());
            }
        }
        if underline == 1 {
            out.extend_from_slice(b"\x1b[4m");
        } else if underline > 1 {
            out.extend_from_slice(format!("\x1b[4:{underline}m").as_bytes());
        }
        for (prefix, color) in [
            (38, style.foreground),
            (48, style.background),
            (58, style.underline_color),
        ] {
            let sequence = match color {
                Color::Default => continue,
                Color::Indexed(index) => format!("\x1b[{prefix};5;{index}m"),
                Color::Rgb(r, g, b) => format!("\x1b[{prefix};2;{r};{g};{b}m"),
            };
            out.extend_from_slice(sequence.as_bytes());
        }
    } else if emit == Format::Html {
        out.extend_from_slice(b"<div style=\"display: inline;");
        for (property, color) in [
            ("color", style.foreground),
            ("background-color", style.background),
            ("text-decoration-color", style.underline_color),
        ] {
            let value = match color {
                Color::Default => continue,
                Color::Indexed(index) => format!("{property}: var(--vt-palette-{index});"),
                Color::Rgb(r, g, b) => format!("{property}: rgb({r}, {g}, {b});"),
            };
            out.extend_from_slice(value.as_bytes());
        }
        if underline != 0 || style.strikethrough || style.overline || style.blink {
            out.extend_from_slice(b"text-decoration-line:");
            for (enabled, text) in [
                (underline != 0, " underline"),
                (style.strikethrough, " line-through"),
                (style.overline, " overline"),
                (style.blink, " blink"),
            ] {
                if enabled {
                    out.extend_from_slice(text.as_bytes());
                }
            }
            out.push(b';');
        }
        if underline != 0 {
            out.extend_from_slice(
                format!(
                    "text-decoration-style: {};",
                    ["", "solid", "double", "wavy", "dotted", "dashed"][underline]
                )
                .as_bytes(),
            );
        }
        for (enabled, text) in [
            (style.bold, "font-weight: bold;"),
            (style.italic, "font-style: italic;"),
            (style.faint, "opacity: 0.5;"),
            (style.invisible, "visibility: hidden;"),
            (style.inverse, "filter: invert(100%);"),
        ] {
            if enabled {
                out.extend_from_slice(text.as_bytes());
            }
        }
        out.extend_from_slice(b"\">");
    }
}
