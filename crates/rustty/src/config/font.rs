//! Ghostty font selection settings, independent of any font backend.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FontStyleRequest {
    #[default]
    Default,
    Disabled,
    Named(String),
}

impl FontStyleRequest {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "" | "default" => Ok(Self::Default),
            "false" => Ok(Self::Disabled),
            _ if value.contains('\0') => Err("font style contains NUL"),
            _ => Ok(Self::Named(value.to_owned())),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontVariation {
    pub tag: [u8; 4],
    pub value: f64,
}

impl FontVariation {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        let (tag, value) = value
            .split_once('=')
            .ok_or("variation requires axis=value")?;
        let tag = tag
            .trim()
            .as_bytes()
            .try_into()
            .map_err(|_| "font axis must contain four bytes")?;
        let value = value
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .ok_or("font variation requires a finite number")?;
        Ok(Self { tag, value })
    }
}

/// Inclusive codepoint mapping. Later entries take precedence on overlap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodepointMap {
    pub start: u32,
    pub end: u32,
    pub family: String,
}

impl CodepointMap {
    pub fn parse(value: &str) -> Result<Vec<Self>, &'static str> {
        let (ranges, family) = value
            .split_once('=')
            .ok_or("codepoint mapping requires ranges=family")?;
        let family = family.trim();
        if family.contains('\0') {
            return Err("font family contains NUL");
        }
        if ranges.trim().is_empty() {
            return Ok(Vec::new());
        }
        ranges
            .split(',')
            .map(|range| {
                let (start, end) = range.split_once('-').unwrap_or((range, range));
                let start = codepoint(start)?;
                let end = codepoint(end)?;
                if start > end {
                    return Err("codepoint range is reversed");
                }
                Ok(Self {
                    start,
                    end,
                    family: family.to_owned(),
                })
            })
            .collect()
    }
}

fn codepoint(value: &str) -> Result<u32, &'static str> {
    let value = value
        .trim()
        .strip_prefix("U+")
        .ok_or("codepoint requires U+hex syntax")?;
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid hexadecimal codepoint");
    }
    // Ghostty stores ranges as u21; unused scalar values in a range never match
    // printable text, but need not make an otherwise valid range fail to load.
    u32::from_str_radix(value, 16)
        .ok()
        .filter(|v| *v <= 0x1f_ffff)
        .ok_or("codepoint exceeds the supported range")
}

pub(super) fn append_variation(
    values: &mut Vec<FontVariation>,
    value: &str,
) -> Result<(), &'static str> {
    if value.is_empty() {
        values.clear();
    } else {
        values.push(FontVariation::parse(value)?);
    }
    Ok(())
}

pub(super) fn synthetic_styles(value: &str) -> Result<[bool; 3], &'static str> {
    if let Ok(enabled) = super::parse_bool(value) {
        return Ok([enabled; 3]);
    }
    let mut result = [true; 3];
    for (name, enabled) in super::feature_flags(value) {
        result[match name {
            "bold" => 0,
            "italic" => 1,
            "bold-italic" => 2,
            _ => return Err("unknown synthetic font style"),
        }] = enabled;
    }
    Ok(result)
}
