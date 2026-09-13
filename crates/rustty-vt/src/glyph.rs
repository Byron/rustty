//! Glyph APC registrations, following the pinned native glyph protocol.
//!
//! Entries contain validated simple TrueType outlines. Font coverage queries
//! and rasterization belong to the host; headless queries report glossary
//! coverage only. See src/terminal/apc/glyph.zig for the wire specification.
use base64::Engine;
use serde::Serialize;

pub const MAX_ENTRIES: usize = 1024;
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
pub const DEFAULT_APC_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
    pub on_curve: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Outline {
    pub contours: Vec<u16>,
    pub points: Vec<Point>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DesignMetrics {
    pub units_per_em: u32,
    pub advance_width: u32,
    pub line_height: u32,
}

/// Protocol-controlled constraints after the native normalization rules.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Constraint {
    pub size: Size,
    pub align_horizontal: Alignment,
    pub align_vertical: Alignment,
    pub pad_top: f64,
    pub pad_right: f64,
    pub pad_bottom: f64,
    pub pad_left: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Size {
    None,
    Cover,
    Stretch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Alignment {
    Start,
    Center,
    End,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entry {
    pub codepoint: u32,
    pub outline: Outline,
    pub design: DesignMetrics,
    pub width: u8,
    pub constraint: Constraint,
}

/// Per-terminal registrations in eviction order, oldest first.
///
/// A successful replacement moves to the end. The fixed 1,024-entry bound
/// keeps ordered removal inexpensive without maintaining a second index.
#[derive(Clone, Debug)]
pub struct Glyphs {
    entries: Vec<Entry>,
    enabled: bool,
    apc_limit: usize,
    dirty: bool,
}

impl Default for Glyphs {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            enabled: true,
            apc_limit: DEFAULT_APC_LIMIT,
            dirty: false,
        }
    }
}

impl Glyphs {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, codepoint: u32) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| entry.codepoint == codepoint)
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Disabling clears registrations and affects future APC recognition.
    /// A command whose identifier was already recognized can still finish.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.entries = Vec::new();
        }
    }

    pub fn apc_limit(&self) -> usize {
        self.apc_limit
    }

    /// Limits bytes after the 25a1; identifier. None restores the default.
    pub fn set_apc_limit(&mut self, limit: Option<usize>) {
        self.apc_limit = limit.unwrap_or(DEFAULT_APC_LIMIT);
    }

    /// Register/clear attempts dirty glyph state even when they fail.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    pub(crate) fn reset(&mut self) {
        self.entries = Vec::new();
        self.dirty = false;
    }

    /// Execute bytes after the APC identifier. Returns the optional wire
    /// response and whether a register/clear command was classified.
    pub(crate) fn execute(&mut self, bytes: &[u8]) -> (Option<Vec<u8>>, bool) {
        let Some((&verb, tail)) = bytes.split_first() else {
            return (None, false);
        };
        let options = if tail.is_empty() {
            &[][..]
        } else if let Some(options) = tail.strip_prefix(b";") {
            options
        } else {
            return (None, false);
        };
        let (body, changed) = match verb {
            b's' => (Some("s;fmt=glyf".to_owned()), false),
            b'q' => (
                option(options, b"cp").and_then(codepoint).map(|cp| {
                    format!(
                        "q;cp={cp:x};status={}",
                        if self.get(cp).is_some() {
                            "glossary"
                        } else {
                            ""
                        }
                    )
                }),
                false,
            ),
            b'r' => {
                let Some(split) = options.iter().rposition(|&byte| byte == b';') else {
                    return (None, false);
                };
                let (options, payload) = (&options[..split], &options[split + 1..]);
                let reply = match option(options, b"reply") {
                    Some(b"0") => 0,
                    Some(b"2") => 2,
                    _ => 1,
                };
                let cp = option(options, b"cp").and_then(codepoint);
                let result = self.register(options, payload, cp);
                let body = match (reply, result) {
                    (0, _) | (2, Ok(())) => None,
                    (_, Ok(())) => Some(format!("r;cp={:x};status=0", cp.unwrap())),
                    (_, Err(error)) => Some(format!(
                        "r;cp={:x};status=1;reason={}",
                        cp.unwrap_or(0),
                        error.reason()
                    )),
                };
                (body, true)
            }
            b'c' => {
                let result = if let Some(value) = option(options, b"cp") {
                    if let Some(cp) = codepoint(value) {
                        if private_use(cp) {
                            if let Some(index) =
                                self.entries.iter().position(|entry| entry.codepoint == cp)
                            {
                                self.entries.remove(index);
                            }
                            Ok(())
                        } else {
                            Err(Error::OutOfNamespace)
                        }
                    } else {
                        Err(Error::Malformed)
                    }
                } else {
                    self.entries = Vec::new();
                    Ok(())
                };
                (
                    Some(match result {
                        Ok(()) => "c;status=0".to_owned(),
                        Err(error) => format!("c;status=1;reason={}", error.reason()),
                    }),
                    true,
                )
            }
            _ => return (None, false),
        };
        self.dirty |= changed;
        (
            body.map(|body| format!("\x1b_25a1;{body}\x1b\\").into_bytes()),
            changed,
        )
    }

    fn register(&mut self, options: &[u8], payload: &[u8], cp: Option<u32>) -> Result<(), Error> {
        let cp = cp.ok_or(Error::Malformed)?;
        let format = option(options, b"fmt").unwrap_or(b"glyf");
        if !matches!(format, b"glyf" | b"colrv0" | b"colrv1") {
            return Err(Error::Malformed);
        }
        let units_per_em = number_option(options, b"upm", 1000)?;
        let design = DesignMetrics {
            units_per_em,
            advance_width: number_option(options, b"aw", units_per_em)?,
            line_height: number_option(options, b"lh", units_per_em)?,
        };
        if design.units_per_em == 0 || design.advance_width == 0 || design.line_height == 0 {
            return Err(Error::Malformed);
        }
        let width = match option(options, b"width") {
            None | Some(b"1") => 1,
            Some(b"2") => 2,
            _ => return Err(Error::Malformed),
        };
        let constraint = constraint(options)?;
        if format != b"glyf" {
            return Err(Error::Malformed);
        }
        let outline = decode(payload)?;
        // Native validates and decodes the entry before checking its namespace.
        if !private_use(cp) {
            return Err(Error::OutOfNamespace);
        }
        let entry = Entry {
            codepoint: cp,
            outline,
            design,
            width,
            constraint,
        };
        if let Some(index) = self.entries.iter().position(|entry| entry.codepoint == cp) {
            self.entries.remove(index);
        } else {
            self.entries
                .try_reserve(1)
                .map_err(|_| Error::OutOfMemory)?;
        }
        self.entries.push(entry);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Error {
    Malformed,
    OutOfNamespace,
    PayloadTooLarge,
    CompositeUnsupported,
    HintingUnsupported,
    OutOfMemory,
}

impl Error {
    fn reason(self) -> &'static str {
        match self {
            Self::Malformed => "malformed_payload",
            Self::OutOfNamespace => "out_of_namespace",
            Self::PayloadTooLarge => "payload_too_large",
            Self::CompositeUnsupported => "composite_unsupported",
            Self::HintingUnsupported => "hinting_unsupported",
            Self::OutOfMemory => "out_of_memory",
        }
    }
}

fn option<'a>(options: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    options
        .split(|&byte| byte == b';')
        .filter_map(|part| {
            let split = part.iter().position(|&byte| byte == b'=')?;
            (&part[..split] == key).then_some(&part[split + 1..])
        })
        .next_back()
}

fn unsigned(bytes: &[u8], radix: u32, max: u32) -> Option<u32> {
    let (negative, bytes) = if let Some(bytes) = bytes.strip_prefix(b"-") {
        (true, bytes)
    } else {
        (false, bytes.strip_prefix(b"+").unwrap_or(bytes))
    };
    if bytes.is_empty() || bytes.starts_with(b"_") || bytes.ends_with(b"_") {
        return None;
    }
    let number = bytes
        .iter()
        .copied()
        .filter(|&byte| byte != b'_')
        .try_fold(0u32, |number, byte| {
            number
                .checked_mul(radix)?
                .checked_add(char::from(byte).to_digit(radix)?)
        })?;
    (number <= max && (!negative || number == 0)).then_some(number)
}

fn codepoint(bytes: &[u8]) -> Option<u32> {
    unsigned(bytes, 16, (1 << 21) - 1)
}

fn number_option(options: &[u8], key: &[u8], default: u32) -> Result<u32, Error> {
    option(options, key).map_or(Ok(default), |bytes| {
        unsigned(bytes, 10, u32::MAX).ok_or(Error::Malformed)
    })
}

fn private_use(cp: u32) -> bool {
    matches!(cp, 0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd)
}

fn constraint(options: &[u8]) -> Result<Constraint, Error> {
    let size = match option(options, b"size") {
        None | Some(b"height" | b"advance") => Size::None,
        Some(b"contain" | b"cover") => Size::Cover,
        Some(b"stretch") => Size::Stretch,
        _ => return Err(Error::Malformed),
    };
    let alignment = |value: &[u8]| match value {
        b"start" => Ok(Alignment::Start),
        b"center" => Ok(Alignment::Center),
        b"end" => Ok(Alignment::End),
        _ => Err(Error::Malformed),
    };
    let (align_horizontal, align_vertical) = if let Some(value) = option(options, b"align") {
        let mut parts = value.split(|&byte| byte == b',');
        let horizontal = alignment(parts.next().ok_or(Error::Malformed)?)?;
        let vertical = match parts.next().ok_or(Error::Malformed)? {
            b"baseline" => Alignment::Start,
            value => alignment(value)?,
        };
        if parts.next().is_some() {
            return Err(Error::Malformed);
        }
        (horizontal, vertical)
    } else {
        (Alignment::Center, Alignment::Center)
    };
    let mut pad = [0.0; 4];
    if let Some(value) = option(options, b"pad") {
        let mut parts = value.split(|&byte| byte == b',');
        for component in &mut pad {
            *component = std::str::from_utf8(parts.next().ok_or(Error::Malformed)?)
                .ok()
                .and_then(crate::color::fraction)
                .ok_or(Error::Malformed)?;
        }
        if parts.next().is_some() {
            return Err(Error::Malformed);
        }
        if pad[0] + pad[2] >= 1.0 || pad[1] + pad[3] >= 1.0 {
            pad = [0.0; 4];
        }
    }
    Ok(Constraint {
        size,
        align_horizontal,
        align_vertical,
        pad_top: pad[0],
        pad_right: pad[1],
        pad_bottom: pad[2],
        pad_left: pad[3],
    })
}

fn decode(payload: &[u8]) -> Result<Outline, Error> {
    if !payload.len().is_multiple_of(4) {
        return Err(Error::Malformed);
    }
    let size = payload.len() / 4 * 3
        - payload
            .iter()
            .rev()
            .take(2)
            .take_while(|&&byte| byte == b'=')
            .count();
    if size > MAX_PAYLOAD_BYTES {
        return Err(Error::PayloadTooLarge);
    }
    let mut data = Vec::new();
    data.try_reserve_exact(size)
        .map_err(|_| Error::OutOfMemory)?;
    data.resize(size, 0);
    let count = base64::engine::general_purpose::STANDARD
        .decode_slice(payload, &mut data)
        .map_err(|_| Error::Malformed)?;
    data.truncate(count);
    let mut data = data.as_slice();
    let contours = read_u16(&mut data)? as i16;
    // The bounding box is not retained by native Outline.
    for _ in 0..4 {
        read_u16(&mut data)?;
    }
    if contours < 0 {
        return Err(Error::CompositeUnsupported);
    }
    if contours == 0 && data.len() < 2 {
        return Ok(Outline::default());
    }
    let mut outline = Outline::default();
    outline
        .contours
        .try_reserve_exact(contours as usize)
        .map_err(|_| Error::OutOfMemory)?;
    for _ in 0..contours {
        let end = read_u16(&mut data)?;
        if outline.contours.last().is_some_and(|&prev| prev >= end) {
            return Err(Error::Malformed);
        }
        outline.contours.push(end);
    }
    if read_u16(&mut data)? != 0 {
        return Err(Error::HintingUnsupported);
    }
    let count = outline
        .contours
        .last()
        .map_or(0, |&last| usize::from(last) + 1);
    // The pinned native Outline.Point is two i32 coordinates plus a bool,
    // padded to 12 bytes; each decoder allocation is bounded independently.
    if count * 12 > MAX_PAYLOAD_BYTES {
        return Err(Error::PayloadTooLarge);
    }
    outline
        .points
        .try_reserve_exact(count)
        .map_err(|_| Error::OutOfMemory)?;
    outline.points.resize(count, Point::default());
    let mut flags = Vec::new();
    flags
        .try_reserve_exact(count)
        .map_err(|_| Error::OutOfMemory)?;
    while flags.len() < count {
        let flag = read_u8(&mut data)?;
        flags.push(flag);
        if flag & 8 != 0 {
            let repeat = usize::from(read_u8(&mut data)?);
            if flags.len() + repeat > count {
                return Err(Error::Malformed);
            }
            flags.resize(flags.len() + repeat, flag);
        }
    }
    for (short, same) in [(2, 16), (4, 32)] {
        let mut position = 0i32;
        for (&flag, point) in flags.iter().zip(&mut outline.points) {
            let delta = if flag & short != 0 {
                let delta = i32::from(read_u8(&mut data)?);
                if flag & same != 0 { delta } else { -delta }
            } else if flag & same == 0 {
                i32::from(read_u16(&mut data)? as i16)
            } else {
                0
            };
            position = position.checked_add(delta).ok_or(Error::Malformed)?;
            if short == 2 {
                point.x = position;
            } else {
                point.y = position;
            }
            point.on_curve = flag & 1 != 0;
        }
    }
    Ok(outline)
}

fn read_u8(data: &mut &[u8]) -> Result<u8, Error> {
    let (&byte, rest) = data.split_first().ok_or(Error::Malformed)?;
    *data = rest;
    Ok(byte)
}

fn read_u16(data: &mut &[u8]) -> Result<u16, Error> {
    Ok(u16::from_be_bytes([read_u8(data)?, read_u8(data)?]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Effect, Terminal};

    const EMPTY: &[u8] = b"AAAAAAAAAAAAAA==";
    const TRIANGLE: &[u8] = b"AAEAZABkA4QDhAACAAABAQEB9P5wAyADhPzgAAA=";

    fn register(cp: u32) -> Vec<u8> {
        [format!("r;cp={cp:x};").as_bytes(), EMPTY].concat()
    }

    fn apc(command: &[u8]) -> Vec<u8> {
        [b"\x1b_25a1;", command, b"\x1b\\"].concat()
    }

    #[test]
    fn outline_coordinates_and_decoding_limits() {
        let outline = decode(TRIANGLE).unwrap();
        assert_eq!(outline.contours, [2]);
        assert_eq!(
            outline.points,
            [
                Point {
                    x: 500,
                    y: 900,
                    on_curve: true
                },
                Point {
                    x: 100,
                    y: 100,
                    on_curve: true
                },
                Point {
                    x: 900,
                    y: 100,
                    on_curve: true
                },
            ]
        );
        let data = base64::engine::general_purpose::STANDARD
            .decode(TRIANGLE)
            .unwrap();
        for end in 0..data.len() {
            let truncated = base64::engine::general_purpose::STANDARD.encode(&data[..end]);
            assert_eq!(decode(truncated.as_bytes()), Err(Error::Malformed));
        }
        let oversized =
            base64::engine::general_purpose::STANDARD.encode(vec![0; MAX_PAYLOAD_BYTES + 1]);
        assert_eq!(decode(oversized.as_bytes()), Err(Error::PayloadTooLarge));
        // One contour ending at point 5461 requests 5462 native 12-byte points.
        let many_points = base64::engine::general_purpose::STANDARD
            .encode([0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0x15, 0x55, 0, 0]);
        assert_eq!(decode(many_points.as_bytes()), Err(Error::PayloadTooLarge));
    }

    #[test]
    fn replacement_moves_fifo_but_failed_replacement_preserves_entry() {
        let mut glyphs = Glyphs::default();
        for cp in 0xe000..0xe000 + MAX_ENTRIES as u32 {
            assert!(glyphs.execute(&register(cp)).1);
        }
        glyphs.execute(b"r;cp=e000;upm=2048;width=2;AAAAAAAAAAAAAA==");
        let before = glyphs.get(0xe000).unwrap().clone();
        glyphs.clear_dirty();
        let (response, changed) = glyphs.execute(b"r;cp=e000;bad-base64");
        assert!(changed && glyphs.is_dirty());
        assert!(
            response
                .unwrap()
                .ends_with(b"reason=malformed_payload\x1b\\")
        );
        assert_eq!(glyphs.get(0xe000), Some(&before));
        glyphs.execute(&register(0xe400));
        assert_eq!(glyphs.entries().len(), MAX_ENTRIES);
        assert!(glyphs.get(0xe001).is_none());
        assert_eq!(glyphs.entries()[MAX_ENTRIES - 2], before);
        assert_eq!(glyphs.entries()[MAX_ENTRIES - 1].codepoint, 0xe400);
    }

    #[test]
    fn capture_configuration_is_latched_at_identifier() {
        let mut terminal = Terminal::new(8, 3, 0);
        terminal.feed(b"\x1b_25a1;");
        terminal.glyphs.set_enabled(false);
        terminal.glyphs.set_apc_limit(Some(0));
        let effects = terminal.feed(&[register(0xe000), b"\x1b\\".to_vec()].concat());
        assert_eq!(effects, [Effect::Write(apc(b"r;cp=e000;status=0"))]);
        assert!(terminal.glyphs.get(0xe000).is_some());
        assert!(terminal.feed(&apc(b"s")).is_empty());
        terminal.glyphs.set_enabled(true);
        assert!(terminal.feed(&apc(b"s")).is_empty());
        terminal.glyphs.set_apc_limit(Some(1));
        assert_eq!(
            terminal.feed(&apc(b"s")),
            [Effect::Write(apc(b"s;fmt=glyf"))]
        );
        assert!(terminal.feed(&apc(b"s;")).is_empty());
    }

    #[test]
    fn reset_clears_registrations_and_retains_configuration() {
        let mut terminal = Terminal::new(8, 3, 0);
        terminal.feed(&apc(&register(0xe000)));
        assert!(terminal.glyphs.is_dirty());
        terminal.glyphs.set_apc_limit(Some(64));
        terminal.reset();
        assert!(terminal.glyphs.entries().is_empty());
        assert!(!terminal.glyphs.is_dirty());
        assert_eq!(terminal.glyphs.apc_limit(), 64);
        terminal.glyphs.set_enabled(false);
        terminal.feed(b"\x1bc");
        assert!(!terminal.glyphs.enabled());
        assert!(terminal.feed(&apc(b"s")).is_empty());
    }
}
