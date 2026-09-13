//! Configured colors and the xterm/Kitty color protocols.

use crate::Terminal;
use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Palette(u8),
    Foreground,
    Background,
    Cursor,
}

impl Target {
    fn dynamic_index(self) -> Option<usize> {
        match self {
            Self::Background => Some(0),
            Self::Foreground => Some(1),
            Self::Cursor => Some(2),
            Self::Palette(_) => None,
        }
    }
}

impl Terminal {
    /// Actual protocol color, before any renderer fallback is applied.
    pub fn color_current(&self, target: Target) -> Option<[u8; 3]> {
        match target {
            Target::Palette(index) => Some(self.palette[usize::from(index)]),
            _ => self
                .color_override(target)
                .or_else(|| self.color_default(target)),
        }
    }

    pub fn color_default(&self, target: Target) -> Option<[u8; 3]> {
        match target {
            Target::Palette(index) => Some(self.metadata.original_palette[usize::from(index)]),
            _ => self.metadata.colors[target.dynamic_index().unwrap()][0],
        }
    }

    /// An explicit override remains present even when it equals the default.
    pub fn color_override(&self, target: Target) -> Option<[u8; 3]> {
        match target {
            Target::Palette(index) => {
                (self.metadata.palette_overrides[usize::from(index) / 8] & (1 << (index % 8)) != 0)
                    .then(|| self.palette[usize::from(index)])
            }
            _ => self.metadata.colors[target.dynamic_index().unwrap()][1],
        }
    }
}

/// Parse Ghostty's X11, hexadecimal and rgb/rgbi color specifications.
/// Only spaces and tabs are ignored at the edges.
pub fn parse(value: &str) -> Option<[u8; 3]> {
    let value = value.trim_matches([' ', '\t']);
    if value.is_empty() {
        return None;
    }
    if let Some(hex) = value.strip_prefix('#') {
        return match hex.len() {
            3 | 6 | 9 | 12 => hex_color(hex),
            _ => None,
        };
    }
    // Reuse the same MIT/X11 table as the native terminal and config parser.
    for line in include_str!("../../../src/terminal/res/rgb.txt").lines() {
        let Some(name) = line.get(12..) else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case(value) {
            return Some([
                line[..3].trim().parse().unwrap(),
                line[4..7].trim().parse().unwrap(),
                line[8..11].trim().parse().unwrap(),
            ]);
        }
    }
    if matches!(value.len(), 3 | 6) {
        return hex_color(value);
    }
    let (channels, intensity) = if let Some(channels) = value.strip_prefix("rgb:") {
        (channels, false)
    } else {
        (value.strip_prefix("rgbi:")?, true)
    };
    let mut channels = channels.split('/');
    let mut result = [0; 3];
    for channel in &mut result {
        let text = channels.next()?;
        *channel = if intensity {
            (fraction(text)? * 255.0) as u8
        } else {
            hex_channel(text)?
        };
    }
    channels.next().is_none().then_some(result)
}

fn hex_color(value: &str) -> Option<[u8; 3]> {
    if !value.is_ascii() {
        return None;
    }
    let width = value.len() / 3;
    Some([
        hex_channel(&value[..width])?,
        hex_channel(&value[width..width * 2])?,
        hex_channel(&value[width * 2..])?,
    ])
}

fn hex_channel(value: &str) -> Option<u8> {
    if !(1..=4).contains(&value.len()) {
        return None;
    }
    let color = u32::from(unsigned(value, 16)?);
    Some((color * 255 / ((1 << (value.len() * 4)) - 1)) as u8)
}

fn unsigned(value: &str, radix: u32) -> Option<u16> {
    if value.is_empty() || value.starts_with('_') || value.ends_with('_') {
        return None;
    }
    value
        .bytes()
        .filter(|&byte| byte != b'_')
        .try_fold(0u16, |number, byte| {
            number
                .checked_mul(radix as u16)?
                .checked_add(char::from(byte).to_digit(radix)? as u16)
        })
}

fn signed_index(value: &str) -> Option<u16> {
    if let Some(value) = value.strip_prefix('-') {
        return (unsigned(value, 10)? == 0).then_some(0);
    }
    unsigned(value.strip_prefix('+').unwrap_or(value), 10)
}

// Match the native fraction parser's plain-decimal syntax and fifteen-digit
// accumulation limit, including negative zero and ignored trailing precision.
pub(crate) fn fraction(value: &str) -> Option<f64> {
    let (negative, value) = if let Some(value) = value.strip_prefix('-') {
        (true, value)
    } else {
        (false, value.strip_prefix('+').unwrap_or(value))
    };
    let (integer, fractional) = value.split_once('.').unwrap_or((value, ""));
    if integer.is_empty() && fractional.is_empty() {
        return None;
    }
    let mut number = 0.0;
    for digit in integer.bytes() {
        if !digit.is_ascii_digit() {
            return None;
        }
        number = number * 10.0 + f64::from(digit - b'0');
    }
    let mut numerator = 0u64;
    let mut denominator = 1u64;
    for digit in fractional.bytes() {
        if !digit.is_ascii_digit() {
            return None;
        }
        if denominator < 1_000_000_000_000_000 {
            numerator = numerator * 10 + u64::from(digit - b'0');
            denominator *= 10;
        }
    }
    number += numerator as f64 / denominator as f64;
    if negative {
        number = -number;
    }
    (0.0..=1.0).contains(&number).then_some(number)
}

fn dynamic(number: u16) -> Option<Target> {
    match number {
        10 => Some(Target::Foreground),
        11 => Some(Target::Background),
        12 => Some(Target::Cursor),
        _ => None,
    }
}

fn change(terminal: &mut Terminal, target: Target, value: Option<[u8; 3]>) {
    match target {
        Target::Palette(index) => {
            let index = usize::from(index);
            terminal.palette[index] = value.unwrap_or(terminal.metadata.original_palette[index]);
            let mask = &mut terminal.metadata.palette_overrides[index / 8];
            if value.is_some() {
                *mask |= 1 << (index % 8);
            } else {
                *mask &= !(1 << (index % 8));
            }
        }
        _ => {
            terminal.metadata.colors[target.dynamic_index().unwrap()][1] = value;
            let current = terminal.color_current(target);
            match target {
                Target::Foreground => terminal.foreground = current.unwrap_or([255; 3]),
                Target::Background => terminal.background = current.unwrap_or([0; 3]),
                Target::Cursor => terminal.cursor_color = current,
                _ => unreachable!(),
            }
        }
    }
    terminal.changed();
}

pub(crate) fn osc(terminal: &mut Terminal, number: u16, data: &[u8], bell: bool) -> Vec<u8> {
    // These commands use the native OSC parser's fixed capture buffer. An
    // overflow discards the entire request, including its otherwise valid prefix.
    if data.len() > 2048 {
        return Vec::new();
    }
    let terminator = if bell { "\x07" } else { "\x1b\\" };
    if number == 21 {
        return kitty(terminal, data, terminator);
    }
    let mut reply = String::new();
    let mut tokens = data.split(|&byte| byte == b';').filter(|s| !s.is_empty());
    match number {
        4 | 5 => {
            while let (Some(index), Some(spec)) = (tokens.next(), tokens.next()) {
                let Some(index) = std::str::from_utf8(index).ok().and_then(signed_index) else {
                    break;
                };
                if index > if number == 4 { 260 } else { 4 } {
                    break;
                }
                let target = (number == 4 && index < 256).then_some(Target::Palette(index as u8));
                if !xterm_request(terminal, target, spec, terminator, &mut reply) {
                    break;
                }
            }
        }
        10..=19 => {
            for (number, spec) in (number..=19).zip(tokens) {
                if !xterm_request(terminal, dynamic(number), spec, terminator, &mut reply) {
                    break;
                }
            }
        }
        104 => {
            let mut valid = false;
            for index in tokens {
                let Some(index) = std::str::from_utf8(index)
                    .ok()
                    .and_then(signed_index)
                    .filter(|&index| index <= 260)
                else {
                    continue;
                };
                valid = true;
                if index < 256 {
                    change(terminal, Target::Palette(index as u8), None);
                }
            }
            if !valid {
                for index in 0..=255 {
                    let target = Target::Palette(index);
                    if terminal.color_override(target).is_some() {
                        change(terminal, target, None);
                    }
                }
            }
        }
        110..=119 if tokens.next().is_none() => {
            if let Some(target) = dynamic(number - 100) {
                change(terminal, target, None);
            }
        }
        _ => {}
    }
    reply.into_bytes()
}

fn xterm_request(
    terminal: &mut Terminal,
    target: Option<Target>,
    spec: &[u8],
    terminator: &str,
    reply: &mut String,
) -> bool {
    if spec == b"?" {
        if let Some(target) = target {
            let color = terminal.color_current(target).or_else(|| {
                (target == Target::Cursor)
                    .then(|| terminal.color_current(Target::Foreground))
                    .flatten()
            });
            if let Some([r, g, b]) = color {
                match target {
                    Target::Palette(index) => write!(reply, "\x1b]4;{index};").unwrap(),
                    _ => write!(
                        reply,
                        "\x1b]{};",
                        match target {
                            Target::Foreground => 10,
                            Target::Background => 11,
                            Target::Cursor => 12,
                            _ => unreachable!(),
                        }
                    )
                    .unwrap(),
                }
                write!(
                    reply,
                    "rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{terminator}"
                )
                .unwrap();
            }
        }
    } else {
        let Some(color) = std::str::from_utf8(spec).ok().and_then(parse) else {
            return false;
        };
        if let Some(target) = target {
            change(terminal, target, Some(color));
        }
    }
    true
}

enum KittyKey {
    Color(Target),
    Unsupported,
}

impl KittyKey {
    fn parse(key: &str) -> Option<Self> {
        Some(match key {
            "foreground" => Self::Color(Target::Foreground),
            "background" => Self::Color(Target::Background),
            "cursor" => Self::Color(Target::Cursor),
            "selection_foreground"
            | "selection_background"
            | "cursor_text"
            | "visual_bell"
            | "second_transparent_background" => Self::Unsupported,
            _ => Self::Color(Target::Palette(unsigned(key, 10)?.try_into().ok()?)),
        })
    }
}

enum Operation {
    Query,
    Set([u8; 3]),
    Reset,
}

fn kitty(terminal: &mut Terminal, data: &[u8], terminator: &str) -> Vec<u8> {
    let mut requests = Vec::new();
    for field in data.split(|&byte| byte == b';') {
        // Native Kind.max is maxInt(u8) + eight special keys.
        if requests.len() >= 2 * (255 + 8) {
            return Vec::new();
        }
        let Ok(field) = std::str::from_utf8(field) else {
            continue;
        };
        let (key, value) = field.split_once('=').unwrap_or((field, ""));
        let Some(key) = KittyKey::parse(key) else {
            continue;
        };
        let operation = match value.trim_matches(' ') {
            "" => Operation::Reset,
            "?" => Operation::Query,
            value => match parse(value) {
                Some(color) => Operation::Set(color),
                None => continue,
            },
        };
        requests.push((key, operation));
    }
    let mut reply = String::new();
    for (key, operation) in requests {
        let KittyKey::Color(target) = key else {
            continue;
        };
        match operation {
            Operation::Set(color) => change(terminal, target, Some(color)),
            Operation::Reset => change(terminal, target, None),
            Operation::Query => {
                if reply.is_empty() {
                    reply.push_str("\x1b]21");
                }
                match target {
                    Target::Palette(index) => write!(reply, ";{index}=").unwrap(),
                    _ => write!(
                        reply,
                        ";{}=",
                        match target {
                            Target::Foreground => "foreground",
                            Target::Background => "background",
                            Target::Cursor => "cursor",
                            _ => unreachable!(),
                        }
                    )
                    .unwrap(),
                }
                if let Some([r, g, b]) = terminal.color_current(target) {
                    write!(reply, "rgb:{r:02x}/{g:02x}/{b:02x}").unwrap();
                }
            }
        }
    }
    if !reply.is_empty() {
        reply.push_str(terminator);
    }
    reply.into_bytes()
}
