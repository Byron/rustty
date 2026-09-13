//! GHOSTSNP version 1 terminal persistence.
//!
//! The encoder streams active rows before history. Decode budgets bound record
//! sizes and the total cells restored; PAGE allocation hints are advisory and
//! do not control native allocation. Version 1 excludes graphics and selection.
//! Physical PAGE widths are preserved on restore and during in-bounds edits.
//! Column resizes reflow them; edits beyond a narrow row extend it safely.
use crate::modes::Modes;
use crate::screen::{Charset, CharsetState, KittyKeyboard, SavedCursor};
use crate::{
    Cell, Color, Cursor, CursorShape, HyperlinkId, Margins, Row, Screen, ScrollbackLimits,
    SemanticContent, Style, Terminal, Underline, default_palette,
};
use std::collections::HashMap;
use std::io::{self, Read, Write};

const MAGIC: &[u8; 10] = b"GHOSTSNP\x01\x00";

fn invalid(message: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn u16_bytes(out: &mut Vec<u8>, value: usize) -> io::Result<()> {
    out.extend_from_slice(
        &u16::try_from(value)
            .map_err(|_| invalid("snapshot u16 overflow"))?
            .to_le_bytes(),
    );
    Ok(())
}
fn u32_bytes(out: &mut Vec<u8>, value: usize) -> io::Result<()> {
    out.extend_from_slice(
        &u32::try_from(value)
            .map_err(|_| invalid("snapshot u32 overflow"))?
            .to_le_bytes(),
    );
    Ok(())
}
fn bytes_string(out: &mut Vec<u8>, value: &[u8]) -> io::Result<()> {
    u32_bytes(out, value.len())?;
    out.extend_from_slice(value);
    Ok(())
}
fn record(out: &mut impl Write, tag: u16, payload: &[u8]) -> io::Result<()> {
    let mut header = [0u8; 10];
    header[..2].copy_from_slice(&tag.to_le_bytes());
    let len = u32::try_from(payload.len()).map_err(|_| invalid("snapshot record overflow"))?;
    header[2..6].copy_from_slice(&len.to_le_bytes());
    let crc = crc32c::crc32c_append(crc32c::crc32c(&header[..6]), payload);
    header[6..].copy_from_slice(&crc.to_le_bytes());
    out.write_all(&header)?;
    out.write_all(payload)
}
fn optional_bool(value: Option<bool>) -> u8 {
    value.map_or(0, |b| 1 + u8::from(b))
}
fn semantic(value: SemanticContent) -> u8 {
    match value {
        SemanticContent::Output => 0,
        SemanticContent::Input => 1,
        SemanticContent::Prompt => 2,
    }
}
pub(crate) fn cursor_shape(shape: CursorShape) -> u8 {
    match shape {
        CursorShape::Bar => 0,
        CursorShape::Block => 1,
        CursorShape::Underline => 2,
        CursorShape::HollowBlock => 3,
    }
}
fn charset(state: &CharsetState) -> u16 {
    let mut bits = 0;
    for (i, set) in state.slots.iter().enumerate() {
        let v: u16 = match set {
            Charset::Utf8 => 0,
            Charset::Ascii => 1,
            Charset::British => 2,
            Charset::DecSpecial => 3,
        };
        bits |= v << (2 * i);
    }
    bits | ((state.gl as u16) << 8)
        | ((state.gr as u16) << 10)
        | ((state.single.map_or(0, |v| v + 1) as u16) << 12)
}
fn color(out: &mut Vec<u8>, value: Color) {
    out.extend_from_slice(&match value {
        Color::Default => [0, 0, 0, 0],
        Color::Indexed(i) => [1, i, 0, 0],
        Color::Rgb(r, g, b) => [2, r, g, b],
    });
}
fn style(out: &mut Vec<u8>, value: Style) {
    color(out, value.foreground);
    color(out, value.background);
    color(out, value.underline_color);
    let flags = [
        value.bold,
        value.italic,
        value.faint,
        value.blink,
        value.inverse,
        value.invisible,
        value.strikethrough,
        value.overline,
    ]
    .iter()
    .enumerate()
    .fold(0u16, |flags, (i, &v)| flags | (u16::from(v) << i));
    let underline: u16 = match value.underline {
        Underline::None => 0,
        Underline::Single => 1,
        Underline::Double => 2,
        Underline::Curly => 3,
        Underline::Dotted => 4,
        Underline::Dashed => 5,
    };
    out.extend_from_slice(&(flags | (underline << 8)).to_le_bytes());
    out.extend_from_slice(&[0, 0]);
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Link {
    id: HyperlinkId,
    uri: Vec<u8>,
}
impl Link {
    fn from_cell(cell: &Cell) -> Option<Self> {
        cell.hyperlink.as_ref().map(|uri| Self {
            id: cell
                .hyperlink_id
                .clone()
                .unwrap_or(HyperlinkId::Implicit(0)),
            uri: cell
                .hyperlink_raw
                .clone()
                .unwrap_or_else(|| uri.as_bytes().to_vec()),
        })
    }
    fn from_cursor(cursor: &Cursor) -> Option<Self> {
        cursor.hyperlink.as_ref().map(|uri| Self {
            id: cursor
                .hyperlink_id
                .clone()
                .unwrap_or(HyperlinkId::Implicit(0)),
            uri: cursor
                .hyperlink_raw
                .clone()
                .unwrap_or_else(|| uri.as_bytes().to_vec()),
        })
    }
    fn encode(&self, out: &mut Vec<u8>) -> io::Result<()> {
        if self.uri.is_empty() {
            return Err(invalid("empty snapshot hyperlink URI"));
        }
        match &self.id {
            HyperlinkId::Implicit(id) => {
                out.push(1);
                out.extend_from_slice(&id.to_le_bytes());
            }
            HyperlinkId::Explicit(id) => {
                if id.is_empty() {
                    return Err(invalid("empty snapshot hyperlink ID"));
                }
                out.push(2);
                bytes_string(out, id)?;
            }
        }
        bytes_string(out, &self.uri)
    }
}

fn encode_terminal(t: &Terminal) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    u16_bytes(&mut out, t.cols.into())?;
    u16_bytes(&mut out, t.rows.into())?;
    out.extend_from_slice(&t.width_px.to_le_bytes());
    out.extend_from_slice(&t.height_px.to_le_bytes());
    for value in [
        t.margins.top,
        t.margins.bottom,
        t.margins.left,
        t.margins.right,
    ] {
        u16_bytes(&mut out, value)?;
    }
    out.push(u8::from(t.status_display));
    u16_bytes(&mut out, usize::from(t.alternate_active))?;
    u16_bytes(&mut out, 1 + usize::from(t.alternate.is_some()))?;
    out.extend_from_slice(&t.previous_char.map_or(u32::MAX, u32::from).to_le_bytes());
    let m = &t.metadata;
    out.extend_from_slice(&[
        u8::from(m.cursor_is_default),
        m.cursor_default_shape,
        optional_bool(m.cursor_default_blink),
        m.shell_redraw,
        u8::from(t.modify_other_keys),
    ]);
    out.push(match t.mouse_mode {
        9 => 1,
        1000 => 2,
        1002 => 3,
        1003 => 4,
        _ => 0,
    });
    out.push(match t.mouse_format {
        1005 => 1,
        1006 => 2,
        1015 => 3,
        1016 => 4,
        _ => 0,
    });
    out.extend_from_slice(&[
        optional_bool(m.mouse_shift_capture),
        m.mouse_shape,
        u8::from(m.password_input),
    ]);
    for bits in t.modes.packed() {
        out.extend_from_slice(&bits.to_le_bytes());
    }
    for (i, current) in [Some(t.background), Some(t.foreground), t.cursor_color]
        .into_iter()
        .enumerate()
    {
        let [default, mut overridden] = m.colors[i];
        let fallback = match i {
            0 => Some([0; 3]),
            1 => Some([255; 3]),
            _ => None,
        };
        if overridden.or(default).or(fallback) != current {
            overridden = current;
        }
        for value in [default, overridden] {
            out.push(u8::from(value.is_some()));
            out.extend_from_slice(&value.unwrap_or([0; 3]));
        }
    }
    for value in [t.limits().bytes, t.limits().lines] {
        out.extend_from_slice(&value.map_or(u64::MAX, |v| v as u64).to_le_bytes());
    }
    debug_assert_eq!(out.len(), 103);
    for chunk in t.tabstops.chunks(8) {
        out.push(
            chunk
                .iter()
                .enumerate()
                .fold(0u8, |bits, (i, &v)| bits | (u8::from(v) << i)),
        );
    }
    if m.original_palette.len() != 256 || t.palette.len() != 256 {
        return Err(invalid("snapshot palette must have 256 colors"));
    }
    for rgb in &m.original_palette {
        out.extend_from_slice(rgb);
    }
    let mut mask = m.palette_overrides;
    for i in 0..256 {
        if t.palette[i] != m.original_palette[i] {
            mask[i / 8] |= 1 << (i % 8);
        }
    }
    out.extend_from_slice(&mask);
    for i in 0..256 {
        if mask[i / 8] & (1 << (i % 8)) != 0 {
            out.extend_from_slice(&t.palette[i]);
        }
    }
    for (raw, text) in [(&m.pwd_raw, &t.working_directory), (&m.title_raw, &t.title)] {
        let bytes = raw
            .as_ref()
            .filter(|bytes| String::from_utf8_lossy(bytes) == *text)
            .map_or(text.as_bytes(), |bytes| bytes.as_slice());
        bytes_string(&mut out, bytes)?;
    }
    Ok(out)
}

fn encode_screen(screen: &Screen, key: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    u16_bytes(&mut out, key)?;
    u16_bytes(&mut out, screen.rows.len())?;
    out.extend_from_slice(&(screen.history.len() as u64).to_le_bytes());
    let c = &screen.cursor;
    u16_bytes(&mut out, c.col)?;
    u16_bytes(&mut out, c.row)?;
    let m = &screen.metadata;
    out.push(cursor_shape(c.shape));
    out.push(
        u8::from(c.pending_wrap)
            | (u8::from(c.protected) << 1)
            | (semantic(c.semantic) << 2)
            | (u8::from(m.cursor_clear_eol) << 4),
    );
    style(&mut out, c.style);
    out.extend_from_slice(&m.hyperlink_implicit_id.to_le_bytes());
    out.extend_from_slice(&charset(&screen.charset).to_le_bytes());
    out.push(if screen.iso_protection {
        1
    } else {
        m.protected_mode
    });
    out.push(screen.kitty_keyboard.index);
    out.extend_from_slice(&screen.kitty_keyboard.flags);
    out.extend_from_slice(&m.semantic_click);
    out.push(u8::from(screen.saved_cursor.is_some()));
    debug_assert_eq!(out.len(), 53);
    if let Some(saved) = &screen.saved_cursor {
        u16_bytes(&mut out, saved.cursor.col)?;
        u16_bytes(&mut out, saved.cursor.row)?;
        style(&mut out, saved.cursor.style);
        out.push(
            u8::from(saved.cursor.protected)
                | (u8::from(saved.cursor.pending_wrap) << 1)
                | (u8::from(saved.origin) << 2),
        );
        out.extend_from_slice(&charset(&saved.charset).to_le_bytes());
    }
    if let Some(link) = Link::from_cursor(c) {
        link.encode(&mut out)?;
    } else {
        out.push(0);
    }
    Ok(out)
}

fn encode_page(row: &Row) -> io::Result<Vec<u8>> {
    let mut styles = Vec::new();
    let mut style_ids = HashMap::new();
    let mut links = Vec::new();
    let mut link_ids = HashMap::new();
    let mut words = Vec::with_capacity(row.cells.len());
    let mut suffixes = Vec::new();
    for (col, cell) in row.cells.iter().enumerate() {
        let mut chars = cell.text.chars();
        let cp = chars.next().map_or(0, u32::from);
        let tail: Vec<_> = chars.map(u32::from).collect();
        let mut kind = u64::from(!tail.is_empty());
        let mut content = u64::from(cp);
        if !tail.is_empty() {
            suffixes.push((col, tail));
        }
        let blank_style = Style {
            background: cell.style.background,
            ..Style::default()
        };
        let mut style_value = cell.style;
        if cell.text.is_empty() && cell.style == blank_style && cell.width == 1 && !cell.spacer_head
        {
            match cell.style.background {
                Color::Indexed(index) => {
                    kind = 2;
                    content = index.into();
                    style_value = Style::default();
                }
                Color::Rgb(r, g, b) => {
                    kind = 3;
                    content = u64::from(r) | (u64::from(g) << 8) | (u64::from(b) << 16);
                    style_value = Style::default();
                }
                Color::Default => {}
            }
        }
        let style_id = if style_value == Style::default() {
            0
        } else {
            *style_ids.entry(style_value).or_insert_with(|| {
                styles.push(style_value);
                styles.len()
            })
        };
        let link_id = if cell.hyperlink.is_some() {
            let link = Link::from_cell(cell).unwrap();
            *link_ids.entry(link.clone()).or_insert_with(|| {
                links.push(link);
                links.len()
            })
        } else {
            0
        };
        if style_id > u16::MAX as usize || link_id > 511 {
            return Err(invalid("snapshot row exceeds native table capacity"));
        }
        let width = if cell.spacer_head {
            3
        } else {
            match cell.width {
                2 => 1,
                0 => 2,
                _ => 0,
            }
        };
        words.push(
            kind | (content << 2)
                | ((style_id as u64) << 26)
                | (width << 42)
                | (u64::from(cell.protected) << 44)
                | (u64::from(link_id != 0) << 45)
                | (u64::from(semantic(cell.semantic)) << 46)
                | ((link_id as u64) << 48),
        );
    }
    let mut out = Vec::new();
    u16_bytes(&mut out, row.cells.len())?;
    u16_bytes(&mut out, 1)?;
    u16_bytes(&mut out, styles.len())?;
    u16_bytes(&mut out, links.len())?;
    u16_bytes(&mut out, styles.len().max(16))?;
    u16_bytes(&mut out, links.len().saturating_mul(128).max(192))?;
    let grapheme_bytes = suffixes
        .iter()
        .map(|(_, cps)| (cps.len() + 1) * 8 + 128)
        .sum::<usize>();
    u32_bytes(
        &mut out,
        grapheme_bytes
            .max(1024)
            .checked_next_power_of_two()
            .ok_or_else(|| invalid("snapshot grapheme capacity overflow"))?,
    )?;
    let string_bytes = links
        .iter()
        .map(|l| {
            l.uri.len()
                + match &l.id {
                    HyperlinkId::Explicit(id) => id.len(),
                    _ => 0,
                }
                + 32
        })
        .sum::<usize>();
    u32_bytes(
        &mut out,
        string_bytes
            .saturating_mul(2)
            .max(2048)
            .checked_next_power_of_two()
            .ok_or_else(|| invalid("snapshot string capacity overflow"))?,
    )?;
    for (i, value) in styles.into_iter().enumerate() {
        u16_bytes(&mut out, i + 1)?;
        style(&mut out, value);
    }
    for (i, value) in links.into_iter().enumerate() {
        u16_bytes(&mut out, i + 1)?;
        value.encode(&mut out)?;
    }
    let count = words.iter().rposition(|&v| v != 0).map_or(0, |i| i + 1);
    let word_or = words.iter().fold(0u64, |a, &b| a | b);
    let width = if word_or & !0x3fc == 0 {
        0
    } else if word_or & !0x3fffc == 0 {
        1
    } else if word_or <= u64::from(u32::MAX) {
        2
    } else {
        3
    };
    let row_semantic = match row.semantic {
        SemanticContent::Prompt => 1,
        SemanticContent::Input => 2,
        _ => 0,
    };
    out.push(
        u8::from(row.wrapped)
            | (u8::from(row.wrap_continuation) << 1)
            | (row_semantic << 2)
            | (width << 4),
    );
    u16_bytes(&mut out, count)?;
    for word in words.into_iter().take(count) {
        let word = if width < 2 { word >> 2 } else { word };
        out.extend_from_slice(&word.to_le_bytes()[..1usize << width]);
    }
    u32_bytes(&mut out, suffixes.len())?;
    for (col, cps) in suffixes {
        u16_bytes(&mut out, 0)?;
        u16_bytes(&mut out, col)?;
        u16_bytes(&mut out, cps.len())?;
        for cp in cps {
            out.extend_from_slice(&cp.to_le_bytes());
        }
    }
    Ok(out)
}

/// Stream one complete snapshot. Graphics and presentation state are omitted,
/// as specified by version 1. Completed records may be written before an error.
pub fn encode(terminal: &Terminal, destination: &mut impl Write) -> io::Result<()> {
    let continuation = terminal.parser.continuation().map_err(invalid)?;
    rustty_parser::Parser::validate_continuation(&continuation).map_err(invalid)?;
    destination.write_all(MAGIC)?;
    record(destination, 1, &encode_terminal(terminal)?)?;
    let screens = [Some(&terminal.primary), terminal.alternate.as_ref()];
    for (key, screen) in screens.iter().enumerate() {
        if let Some(screen) = screen {
            record(destination, 2, &encode_screen(screen, key)?)?;
            for row in &screen.rows {
                record(destination, 3, &encode_page(row)?)?;
            }
        }
    }
    record(destination, 7, &continuation)?;
    record(destination, 5, &[])?;
    for (key, screen) in screens.iter().enumerate() {
        if let Some(screen) = screen {
            let mut header = Vec::new();
            u16_bytes(&mut header, key)?;
            u32_bytes(&mut header, screen.history.len())?;
            record(destination, 4, &header)?;
            for row in screen.history.iter().rev() {
                record(destination, 3, &encode_page(row)?)?;
            }
        }
    }
    record(destination, 6, &[])
}

pub fn encode_to_vec(terminal: &Terminal) -> io::Result<Vec<u8>> {
    let mut result = Vec::new();
    encode(terminal, &mut result)?;
    Ok(result)
}

/// Allocation policies are checked before trusting wire lengths or dimensions.
#[derive(Clone, Copy, Debug)]
pub struct DecodeOptions {
    pub max_record_bytes: usize,
    pub max_continuation_bytes: usize,
    pub max_cells: usize,
}
impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            max_record_bytes: 8 * 1024 * 1024,
            max_continuation_bytes: 8 * 1024 * 1024,
            max_cells: 1024 * 1024,
        }
    }
}

struct Slice<'a> {
    data: &'a [u8],
    offset: usize,
}
impl<'a> Slice<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }
    fn take(&mut self, count: usize) -> io::Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or_else(|| invalid("snapshot length overflow"))?;
        let result = self
            .data
            .get(self.offset..end)
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        self.offset = end;
        Ok(result)
    }
    fn array<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn string(&mut self) -> io::Result<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    fn finish(&self) -> io::Result<()> {
        if self.offset == self.data.len() {
            Ok(())
        } else {
            Err(invalid("snapshot payload has trailing bytes"))
        }
    }
}

fn decode_bool(value: u8) -> Option<bool> {
    match value {
        1 => Some(false),
        2 => Some(true),
        _ => None,
    }
}
pub(crate) fn decode_shape(value: u8) -> CursorShape {
    match value {
        0 => CursorShape::Bar,
        2 => CursorShape::Underline,
        3 => CursorShape::HollowBlock,
        _ => CursorShape::Block,
    }
}
fn decode_semantic(value: u8) -> SemanticContent {
    match value {
        1 => SemanticContent::Input,
        2 => SemanticContent::Prompt,
        _ => SemanticContent::Output,
    }
}
fn decode_charset(raw: u16) -> CharsetState {
    let slots = std::array::from_fn(|i| match (raw >> (i * 2)) & 3 {
        1 => Charset::Ascii,
        2 => Charset::British,
        3 => Charset::DecSpecial,
        _ => Charset::Utf8,
    });
    let single = usize::from((raw >> 12) & 7);
    CharsetState {
        slots,
        gl: usize::from((raw >> 8) & 3),
        gr: usize::from((raw >> 10) & 3),
        single: (1..=4).contains(&single).then(|| single - 1),
    }
}
fn decode_color(bytes: &[u8]) -> Option<Color> {
    match bytes {
        [0, 0, 0, 0] => Some(Color::Default),
        [1, i, 0, 0] => Some(Color::Indexed(*i)),
        [2, r, g, b] => Some(Color::Rgb(*r, *g, *b)),
        _ => None,
    }
}
fn decode_style(reader: &mut Slice<'_>) -> io::Result<Style> {
    let bytes = reader.take(16)?;
    let flags = u16::from_le_bytes(bytes[12..14].try_into().unwrap());
    let (Some(foreground), Some(background), Some(underline_color)) = (
        decode_color(&bytes[..4]),
        decode_color(&bytes[4..8]),
        decode_color(&bytes[8..12]),
    ) else {
        return Ok(Style::default());
    };
    if flags & 0xf800 != 0 || bytes[14..16] != [0, 0] || (flags >> 8) & 7 > 5 {
        return Ok(Style::default());
    }
    Ok(Style {
        foreground,
        background,
        underline_color,
        bold: flags & 1 != 0,
        italic: flags & 2 != 0,
        faint: flags & 4 != 0,
        blink: flags & 8 != 0,
        inverse: flags & 16 != 0,
        invisible: flags & 32 != 0,
        strikethrough: flags & 64 != 0,
        overline: flags & 128 != 0,
        underline: match (flags >> 8) & 7 {
            1 => Underline::Single,
            2 => Underline::Double,
            3 => Underline::Curly,
            4 => Underline::Dotted,
            5 => Underline::Dashed,
            _ => Underline::None,
        },
    })
}
fn decode_link(reader: &mut Slice<'_>, allow_none: bool) -> io::Result<Option<Link>> {
    let kind = reader.u8()?;
    let id = match kind {
        0 if allow_none => return Ok(None),
        1 => HyperlinkId::Implicit(reader.u32()?),
        2 => HyperlinkId::Explicit(reader.string()?.to_vec()),
        _ => return Err(invalid("unknown snapshot hyperlink kind")),
    };
    let uri = reader.string()?.to_vec();
    if uri.is_empty() || matches!(&id,HyperlinkId::Explicit(id) if id.is_empty()) {
        return Ok(None);
    }
    Ok(Some(Link { id, uri }))
}
fn assign_link(cell: &mut Cell, link: &Link) {
    cell.hyperlink = Some(String::from_utf8_lossy(&link.uri).into_owned());
    cell.hyperlink_raw = std::str::from_utf8(&link.uri)
        .is_err()
        .then(|| link.uri.clone());
    cell.hyperlink_id = Some(link.id.clone());
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryProgress {
    /// Zero for primary, one for alternate.
    pub screen: u8,
    /// Zero when a validated page was dropped after live state changed.
    pub rows: usize,
    pub remaining_pages: u32,
}
struct Sequence {
    key: usize,
    remaining: u32,
    apply: bool,
}

/// Read through READY first, then apply one history page per `next_history`.
/// Failed decoders cannot resume. FINISH leaves following transport bytes unread.
pub struct Decoder<R> {
    source: R,
    options: DecodeOptions,
    started: bool,
    failed: bool,
    finished: bool,
    cols: u16,
    identities: [Option<u64>; 2],
    reflow_generations: [u64; 2],
    history_rows: [u64; 2],
    history_seen: [bool; 2],
    history_remaining: usize,
    sequence: Option<Sequence>,
    cells: usize,
}
impl<R: Read> Decoder<R> {
    pub fn new(source: R, options: DecodeOptions) -> Self {
        Self {
            source,
            options,
            started: false,
            failed: false,
            finished: false,
            cols: 0,
            identities: [None; 2],
            reflow_generations: [0; 2],
            history_rows: [0; 2],
            history_seen: [false; 2],
            history_remaining: 0,
            sequence: None,
            cells: 0,
        }
    }
    pub fn history_rows(&self) -> [u64; 2] {
        self.history_rows
    }
    pub fn into_inner(self) -> R {
        self.source
    }
    fn record(&mut self, expected: u16) -> io::Result<Vec<u8>> {
        let mut header = [0u8; 10];
        self.source.read_exact(&mut header)?;
        let tag = u16::from_le_bytes(header[..2].try_into().unwrap());
        if tag != expected {
            return Err(invalid("unexpected snapshot record tag"));
        }
        let len = u32::from_le_bytes(header[2..6].try_into().unwrap()) as usize;
        if len > self.options.max_record_bytes
            || tag == 7 && len > self.options.max_continuation_bytes
        {
            return Err(invalid("snapshot record exceeds allocation policy"));
        }
        let mut payload = Vec::new();
        payload
            .try_reserve_exact(len)
            .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
        payload.resize(len, 0);
        self.source.read_exact(&mut payload)?;
        let actual = crc32c::crc32c_append(crc32c::crc32c(&header[..6]), &payload);
        if actual != u32::from_le_bytes(header[6..].try_into().unwrap()) {
            return Err(invalid("snapshot checksum mismatch"));
        }
        Ok(payload)
    }
    fn marker(&mut self, tag: u16) -> io::Result<()> {
        if self.record(tag)?.is_empty() {
            Ok(())
        } else {
            Err(invalid("snapshot marker is not empty"))
        }
    }

    pub fn ready(&mut self) -> io::Result<Terminal> {
        if self.started || self.failed {
            return Err(invalid("snapshot decoder already started"));
        }
        self.started = true;
        let result = self.ready_inner();
        self.failed = result.is_err();
        result
    }
    fn ready_inner(&mut self) -> io::Result<Terminal> {
        let mut magic = [0u8; 10];
        self.source.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(invalid("invalid GHOSTSNP magic or version"));
        }
        let payload = self.record(1)?;
        let mut r = Slice::new(&payload);
        let cols = r.u16()?;
        let rows = r.u16()?;
        if cols == 0 || rows == 0 || usize::from(cols) * usize::from(rows) > self.options.max_cells
        {
            return Err(invalid("invalid or excessive snapshot dimensions"));
        }
        let width = r.u32()?;
        let height = r.u32()?;
        let mut margins = Margins {
            top: r.u16()?.into(),
            bottom: r.u16()?.into(),
            left: r.u16()?.into(),
            right: r.u16()?.into(),
        };
        if margins.top > margins.bottom || margins.bottom >= usize::from(rows) {
            margins.top = 0;
            margins.bottom = usize::from(rows) - 1;
        }
        if margins.left > margins.right || margins.right >= usize::from(cols) {
            margins.left = 0;
            margins.right = usize::from(cols) - 1;
        }
        let status = r.u8()? == 1;
        let active = r.u16()?;
        let screen_count = r.u16()?;
        if !matches!(screen_count, 1 | 2) {
            return Err(invalid("invalid snapshot screen count"));
        }
        let previous_char = char::from_u32(r.u32()?);
        let mut m = TerminalMetadata {
            cursor_is_default: r.u8()? == 1,
            cursor_default_shape: match r.u8()? {
                v @ 0..=3 => v,
                _ => 1,
            },
            cursor_default_blink: decode_bool(r.u8()?),
            shell_redraw: match r.u8()? {
                v @ 0..=2 => v,
                _ => 0,
            },
            ..TerminalMetadata::default()
        };
        let modify_other_keys = r.u8()? == 1;
        let mouse_mode = match r.u8()? {
            1 => 9,
            2 => 1000,
            3 => 1002,
            4 => 1003,
            _ => 0,
        };
        let mouse_format = match r.u8()? {
            1 => 1005,
            2 => 1006,
            3 => 1015,
            4 => 1016,
            _ => 0,
        };
        m.mouse_shift_capture = decode_bool(r.u8()?);
        m.mouse_shape = match r.u8()? {
            v @ 0..=33 => v,
            _ => 8,
        };
        m.password_input = r.u8()? == 1;
        let modes = Modes::from_packed([r.u64()?, r.u64()?, r.u64()?]);
        for color in &mut m.colors {
            for value in color {
                let present = r.u8()? == 1;
                let rgb = r.array()?;
                *value = present.then_some(rgb);
            }
        }
        let policy = |raw: u64| {
            if raw == u64::MAX {
                None
            } else {
                Some(usize::try_from(raw).unwrap_or(usize::MAX))
            }
        };
        let limits = ScrollbackLimits {
            bytes: policy(r.u64()?),
            lines: policy(r.u64()?),
        };
        let mut terminal = Terminal::with_limits(cols, rows, limits);
        terminal.width_px = width;
        terminal.height_px = height;
        terminal.margins = margins;
        terminal.status_display = status;
        terminal.previous_char = previous_char;
        terminal.modify_other_keys = modify_other_keys;
        terminal.mouse_mode = mouse_mode;
        terminal.mouse_format = mouse_format;
        terminal.modes = modes;
        let tabs = r.take(usize::from(cols).div_ceil(8))?;
        terminal.tabstops = (0..usize::from(cols))
            .map(|i| tabs[i / 8] & (1 << (i % 8)) != 0)
            .collect();
        m.original_palette.clear();
        for _ in 0..256 {
            m.original_palette.push(r.array()?);
        }
        m.palette_overrides = r.array()?;
        terminal.palette = m.original_palette.clone();
        for i in 0..256 {
            if m.palette_overrides[i / 8] & (1 << (i % 8)) != 0 {
                terminal.palette[i] = r.array()?;
            }
        }
        for (raw, text) in [
            (&mut m.pwd_raw, &mut terminal.working_directory),
            (&mut m.title_raw, &mut terminal.title),
        ] {
            let bytes = r.string()?;
            *text = String::from_utf8_lossy(bytes).into_owned();
            *raw = std::str::from_utf8(bytes).is_err().then(|| bytes.to_vec());
        }
        r.finish()?;
        terminal.background = m.colors[0][1].or(m.colors[0][0]).unwrap_or([0; 3]);
        terminal.foreground = m.colors[1][1].or(m.colors[1][0]).unwrap_or([255; 3]);
        terminal.cursor_color = m.colors[2][1].or(m.colors[2][0]);
        terminal.metadata = m;
        let mut seen = [false; 2];
        for _ in 0..screen_count {
            let (key, screen, extent) = self.screen(cols, rows, limits, &terminal.modes)?;
            if key >= usize::from(screen_count) || seen[key] {
                return Err(invalid("unexpected or duplicate snapshot screen"));
            }
            seen[key] = true;
            self.history_rows[key] = extent;
            self.identities[key] = Some(screen.metadata.identity);
            self.reflow_generations[key] = screen.metadata.reflow_generation;
            if key == 0 {
                terminal.primary = screen;
            } else {
                terminal.alternate = Some(screen);
            }
        }
        terminal.alternate_active = active == 1 && screen_count == 2;
        let continuation = self.record(7)?;
        rustty_parser::Parser::validate_continuation(&continuation).map_err(invalid)?;
        self.marker(5)?;
        terminal
            .parser
            .set_continuation_limit(self.options.max_continuation_bytes);
        if !terminal.feed(&continuation).is_empty() {
            return Err(invalid("snapshot continuation emitted an effect"));
        }
        self.cols = cols;
        self.history_remaining = screen_count.into();
        Ok(terminal)
    }

    fn screen(
        &mut self,
        cols: u16,
        rows: u16,
        limits: ScrollbackLimits,
        modes: &Modes,
    ) -> io::Result<(usize, Screen, u64)> {
        let payload = self.record(2)?;
        let mut r = Slice::new(&payload);
        let key = usize::from(r.u16()?);
        if key > 1 {
            return Err(invalid("invalid snapshot screen key"));
        }
        let count = r.u16()?;
        if count == 0 {
            return Err(invalid("snapshot screen has no pages"));
        }
        let extent = r.u64()?;
        let x = usize::from(r.u16()?);
        let y = usize::from(r.u16()?);
        let shape = match r.u8()? {
            v @ 0..=3 => v,
            _ => 1,
        };
        let flags = r.u8()?;
        let pen = decode_style(&mut r)?;
        let mut screen = Screen::new(
            cols.into(),
            rows.into(),
            if key == 0 {
                limits
            } else {
                ScrollbackLimits::NONE
            },
        );
        screen.cursor = Cursor {
            col: x.min(usize::from(cols) - 1),
            row: y.min(usize::from(rows) - 1),
            shape: decode_shape(shape),
            visible: modes.dec(25),
            blink: modes.dec(12),
            pending_wrap: flags & 1 != 0 && x.min(usize::from(cols) - 1) == usize::from(cols) - 1,
            protected: flags & 2 != 0,
            semantic: decode_semantic((flags >> 2) & 3),
            style: pen,
            ..Cursor::default()
        };
        let m = &mut screen.metadata;
        m.cursor_clear_eol = flags & 16 != 0;
        m.hyperlink_implicit_id = r.u32()?;
        screen.charset = decode_charset(r.u16()?);
        m.protected_mode = match r.u8()? {
            v @ 0..=2 => v,
            _ => 0,
        };
        screen.iso_protection = m.protected_mode == 1;
        let index = match r.u8()? {
            v @ 0..=7 => v,
            _ => 0,
        };
        let mut keyboard_flags = r.array()?;
        for flags in &mut keyboard_flags {
            *flags &= 31;
        }
        screen.kitty_keyboard = KittyKeyboard {
            flags: keyboard_flags,
            index,
        };
        let click = r.array::<2>()?;
        m.semantic_click = if matches!(click, [0, 0] | [1, 0..=1] | [2, 0..=3]) {
            click
        } else {
            [0, 0]
        };
        let has_saved = r.u8()? != 0;
        if has_saved {
            let col = usize::from(r.u16()?).min(usize::from(cols) - 1);
            let row = usize::from(r.u16()?).min(usize::from(rows) - 1);
            let style = decode_style(&mut r)?;
            let flags = r.u8()?;
            let charset = decode_charset(r.u16()?);
            screen.saved_cursor = Some(SavedCursor {
                cursor: Cursor {
                    col,
                    row,
                    style,
                    protected: flags & 1 != 0,
                    pending_wrap: flags & 2 != 0 && col == usize::from(cols) - 1,
                    ..screen.cursor.clone()
                },
                origin: flags & 4 != 0,
                charset,
            });
        }
        if let Some(link) = decode_link(&mut r, true)? {
            screen.cursor.hyperlink = Some(String::from_utf8_lossy(&link.uri).into_owned());
            screen.cursor.hyperlink_raw =
                std::str::from_utf8(&link.uri).is_err().then_some(link.uri);
            screen.cursor.hyperlink_id = Some(link.id);
        }
        r.finish()?;
        let mut contents = Vec::new();
        for _ in 0..count {
            contents.extend(self.page()?);
        }
        if contents.len() < usize::from(rows) {
            return Err(invalid("snapshot pages do not cover active rows"));
        }
        for (i, row) in contents.iter_mut().enumerate() {
            row.id = i as u64;
        }
        screen.next_row = contents.len() as u64;
        screen.rows = contents.split_off(contents.len() - usize::from(rows));
        let physical_cols = screen.rows[screen.cursor.row].cells.len();
        screen.cursor.col = x.min(physical_cols - 1);
        screen.cursor.pending_wrap = flags & 1 != 0 && screen.cursor.col == physical_cols - 1;
        screen.history = contents.into();
        screen.history_bytes = screen.history.iter().map(Row::storage_bytes).sum();
        screen.enforce_limits();
        Ok((key, screen, extent))
    }

    fn page(&mut self) -> io::Result<Vec<Row>> {
        let payload = self.record(3)?;
        self.decode_page(&payload)
    }
    fn decode_page(&mut self, payload: &[u8]) -> io::Result<Vec<Row>> {
        let mut r = Slice::new(payload);
        let cols = usize::from(r.u16()?);
        let rows = usize::from(r.u16()?);
        let style_count = r.u16()?;
        let link_count = r.u16()?;
        r.take(12)?;
        self.cells = self
            .cells
            .checked_add(
                cols.checked_mul(rows)
                    .ok_or_else(|| invalid("snapshot cell count overflow"))?,
            )
            .ok_or_else(|| invalid("snapshot cell count overflow"))?;
        if cols == 0 || rows == 0 || self.cells > self.options.max_cells {
            return Err(invalid("invalid or excessive snapshot page dimensions"));
        }
        let mut styles = HashMap::new();
        for _ in 0..style_count {
            let id = r.u16()?;
            let value = decode_style(&mut r)?;
            if id != 0 {
                styles.entry(id).or_insert(value);
            }
        }
        let mut links = HashMap::new();
        for _ in 0..link_count {
            let id = r.u16()?;
            let value = decode_link(&mut r, false)?;
            if id != 0 {
                links.entry(id).or_insert(value);
            }
        }
        let mut result = Vec::with_capacity(rows);
        for y in 0..rows {
            let flags = r.u8()?;
            let count = usize::from(r.u16()?);
            if count > cols {
                return Err(invalid("snapshot cell count exceeds row width"));
            }
            let width = usize::from((flags >> 4) & 3);
            let mut row = Row::new(y as u64, cols, Color::Default);
            row.wrapped = flags & 1 != 0;
            row.wrap_continuation = flags & 2 != 0;
            row.semantic = match (flags >> 2) & 3 {
                1 => SemanticContent::Prompt,
                2 => SemanticContent::Input,
                _ => SemanticContent::Output,
            };
            for cell in row.cells.iter_mut().take(count) {
                let bytes = r.take(1 << width)?;
                let mut data = [0u8; 8];
                data[..bytes.len()].copy_from_slice(bytes);
                let mut word = u64::from_le_bytes(data);
                if width < 2 {
                    word <<= 2;
                }
                let kind = word & 3;
                let content = ((word >> 2) & 0xffffff) as u32;
                let style_id = ((word >> 26) & 0xffff) as u16;
                cell.style = styles.get(&style_id).copied().unwrap_or_default();
                match kind {
                    0 | 1 => {
                        if content != 0 {
                            cell.text
                                .push(char::from_u32(content).unwrap_or('\u{fffd}'));
                        }
                    }
                    2 => cell.style.background = Color::Indexed(content as u8),
                    _ => {
                        cell.style.background =
                            Color::Rgb(content as u8, (content >> 8) as u8, (content >> 16) as u8)
                    }
                }
                cell.width = match (word >> 42) & 3 {
                    1 => 2,
                    2 => 0,
                    _ => 1,
                };
                cell.spacer_head = (word >> 42) & 3 == 3;
                cell.protected = word & (1 << 44) != 0;
                cell.semantic = decode_semantic(((word >> 46) & 3) as u8);
                let id = (word >> 48) as u16;
                if let Some(Some(link)) = links.get(&id) {
                    assign_link(cell, link);
                }
            }
            for x in 0..cols {
                if x > 0 && row.cells[x - 1].width == 2 && row.cells[x].width != 0 {
                    row.cells[x - 1].width = 1;
                }
                if row.cells[x].width == 2 && x + 1 == cols
                    || row.cells[x].width == 0 && (x == 0 || row.cells[x - 1].width != 2)
                {
                    row.cells[x].width = 1;
                }
                if row.cells[x].spacer_head && (x + 1 != cols || !row.wrapped) {
                    row.cells[x].spacer_head = false;
                }
            }
            result.push(row);
        }
        let entries = r.u32()?;
        let mut assigned = std::collections::HashSet::new();
        for _ in 0..entries {
            let row = usize::from(r.u16()?);
            let col = usize::from(r.u16()?);
            let count = usize::from(r.u16()?);
            let bytes = r.take(
                count
                    .checked_mul(4)
                    .ok_or_else(|| invalid("snapshot suffix overflow"))?,
            )?;
            if let Some(cell) = result.get_mut(row).and_then(|r| r.cells.get_mut(col))
                && !cell.text.is_empty()
                && !assigned.contains(&(row, col))
            {
                for bytes in bytes.as_chunks::<4>().0.iter() {
                    if let Some(cp) = char::from_u32(u32::from_le_bytes(*bytes))
                        && cp != '\0'
                    {
                        cell.text.push(cp);
                        assigned.insert((row, col));
                    }
                }
            }
        }
        r.finish()?;
        Ok(result)
    }

    pub fn next_history(&mut self, terminal: &mut Terminal) -> io::Result<Option<HistoryProgress>> {
        if !self.started || self.failed {
            return Err(invalid("snapshot decoder is not ready"));
        }
        if self.finished {
            return Ok(None);
        }
        let result = self.next_inner(terminal);
        self.failed = result.is_err();
        result
    }
    fn next_inner(&mut self, t: &mut Terminal) -> io::Result<Option<HistoryProgress>> {
        loop {
            if let Some(mut sequence) = self.sequence.take()
                && sequence.remaining > 0
            {
                let payload = self.record(3)?;
                sequence.remaining -= 1;
                let target = if sequence.key == 0 {
                    Some(&mut t.primary)
                } else {
                    t.alternate.as_mut()
                };
                let mut count = 0;
                if sequence.apply
                    && t.cols == self.cols
                    && let Some(screen) = target.filter(|s| {
                        Some(s.metadata.identity) == self.identities[sequence.key]
                            && s.metadata.reflow_generation == self.reflow_generations[sequence.key]
                    })
                {
                    let mut rows = self.decode_page(&payload)?;
                    let bytes = rows.iter().map(Row::storage_bytes).sum::<usize>();
                    let allowed = screen.limits.bytes != Some(0)
                        && screen
                            .limits
                            .lines
                            .is_none_or(|max| rows.len() + screen.history.len() <= max)
                        && screen
                            .limits
                            .bytes
                            .is_none_or(|max| bytes.saturating_add(screen.storage_bytes()) <= max);
                    if allowed {
                        count = rows.len();
                        for row in &mut rows {
                            row.id = screen.next_row;
                            screen.next_row = screen.next_row.wrapping_add(1);
                        }
                        for row in rows.into_iter().rev() {
                            screen.history.push_front(row);
                        }
                        screen.history_bytes = screen.history_bytes.saturating_add(bytes);
                    }
                }
                if count == 0 {
                    sequence.apply = false;
                }
                let progress = HistoryProgress {
                    screen: sequence.key as u8,
                    rows: count,
                    remaining_pages: sequence.remaining,
                };
                self.sequence = Some(sequence);
                return Ok(Some(progress));
            }
            if self.history_remaining == 0 {
                self.marker(6)?;
                self.finished = true;
                return Ok(None);
            }
            let payload = self.record(4)?;
            let mut r = Slice::new(&payload);
            let key = usize::from(r.u16()?);
            let remaining = r.u32()?;
            r.finish()?;
            if key > 1 || self.identities[key].is_none() || self.history_seen[key] {
                return Err(invalid("unexpected or duplicate snapshot history key"));
            }
            self.history_seen[key] = true;
            self.history_remaining -= 1;
            self.sequence = Some(Sequence {
                key,
                remaining,
                apply: true,
            });
        }
    }
}

/// Restore a complete snapshot. Trailing bytes belong to the containing stream.
pub fn decode(source: impl Read, options: DecodeOptions) -> io::Result<Terminal> {
    let mut decoder = Decoder::new(source, options);
    let mut terminal = decoder.ready()?;
    while decoder.next_history(&mut terminal)?.is_some() {}
    Ok(terminal)
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalMetadata {
    pub title_raw: Option<Vec<u8>>,
    pub pwd_raw: Option<Vec<u8>>,
    pub colors: [[Option<[u8; 3]>; 2]; 3],
    pub original_palette: Vec<[u8; 3]>,
    pub palette_overrides: [u8; 32],
    pub cursor_is_default: bool,
    pub cursor_default_shape: u8,
    pub cursor_default_blink: Option<bool>,
    pub shell_redraw: u8,
    pub mouse_shift_capture: Option<bool>,
    pub mouse_shape: u8,
    pub password_input: bool,
}
impl Default for TerminalMetadata {
    fn default() -> Self {
        Self {
            title_raw: None,
            pwd_raw: None,
            colors: [[None; 2]; 3],
            original_palette: default_palette(),
            palette_overrides: [0; 32],
            cursor_is_default: true,
            cursor_default_shape: 1,
            cursor_default_blink: Some(false),
            shell_redraw: 0,
            mouse_shift_capture: None,
            mouse_shape: 8,
            password_input: false,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ScreenMetadata {
    pub identity: u64,
    pub reflow_generation: u64,
    pub hyperlink_implicit_id: u32,
    pub protected_mode: u8,
    pub semantic_click: [u8; 2],
    pub cursor_clear_eol: bool,
}
impl Default for ScreenMetadata {
    fn default() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self {
            identity: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            reflow_generation: 0,
            hyperlink_implicit_id: 0,
            protected_mode: 0,
            semantic_click: [0; 2],
            cursor_clear_eol: false,
        }
    }
}
