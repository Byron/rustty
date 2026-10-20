//! Width and trimming preserve tmux style bytes for a later renderer.
use unicode_width::UnicodeWidthChar;

pub(super) struct Unit<'a> {
    pub(super) text: &'a [u8],
    pub(super) width: usize,
    pub(super) hashes: usize,
    pub(super) style: &'a [u8],
}

pub(super) fn style_end(input: &[u8], mut index: usize) -> Option<usize> {
    let mut depth = 0usize;
    while index < input.len() {
        if input[index] == b'#' && index + 1 < input.len() {
            if input[index + 1] == b'{' {
                depth += 1;
            }
            if b",#{}:".contains(&input[index + 1]) {
                index += 2;
                continue;
            }
        }
        if input[index] == b'}' {
            depth = depth.saturating_sub(1);
        }
        if input[index] == b']' && depth == 0 {
            return Some(index);
        }
        index += 1;
    }
    None
}

pub(super) fn units(input: &[u8]) -> (Vec<Unit<'_>>, bool) {
    let mut units = Vec::new();
    let mut index = 0;
    while index < input.len() && input[index] != 0 {
        let start = index;
        if input[index] == b'#' {
            while input.get(index) == Some(&b'#') {
                index += 1;
            }
            let hashes = index - start;
            let styled = input.get(index) == Some(&b'[') && hashes % 2 != 0;
            let width = if input.get(index) == Some(&b'[') {
                hashes / 2
            } else {
                hashes.div_ceil(2)
            };
            let mut style = &input[0..0];
            if styled {
                let Some(end) = style_end(input, index + 1) else {
                    units.push(Unit {
                        text: &[],
                        width,
                        hashes,
                        style,
                    });
                    return (units, false);
                };
                style = &input[index - 1..=end];
                index = end + 1;
            }
            units.push(Unit {
                text: &[],
                width,
                hashes,
                style,
            });
        } else if input[index].is_ascii() {
            index += 1;
            if input[start] > 0x1f && input[start] < 0x7f {
                units.push(Unit {
                    text: &input[start..index],
                    width: 1,
                    hashes: 0,
                    style: &[],
                });
            }
        } else {
            let length = match input[index] {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => 1,
            };
            if let Some(text) = input
                .get(index..index + length)
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
            {
                units.push(Unit {
                    text: text.as_bytes(),
                    width: text.chars().next().unwrap().width().unwrap_or(0),
                    hashes: 0,
                    style: &[],
                });
                index += length;
            } else {
                index += 1;
            }
        }
    }
    (units, true)
}

pub(super) fn width(input: &[u8]) -> usize {
    let (units, complete) = units(input);
    if complete {
        units.iter().map(|unit| unit.width).sum()
    } else {
        0
    }
}

pub(super) fn trim(input: &[u8], limit: usize, right: bool) -> Vec<u8> {
    let (units, complete) = units(input);
    let total = if complete {
        units.iter().map(|unit| unit.width).sum()
    } else {
        0
    };
    if right && total <= limit {
        return input.to_vec();
    }
    let skip = total.saturating_sub(limit);
    let mut width = 0usize;
    let mut output = Vec::new();
    for unit in units {
        if !right && width >= limit {
            break;
        }
        if unit.hashes != 0 {
            let count = if right {
                unit.width.saturating_sub(skip.saturating_sub(width))
            } else {
                unit.width.min(limit - width)
            };
            output.extend(std::iter::repeat_n(
                b'#',
                if unit.hashes == 1 { count } else { count * 2 },
            ));
            output.extend_from_slice(unit.style);
        } else if if right {
            width >= skip
        } else {
            width + unit.width <= limit
        } {
            output.extend_from_slice(unit.text);
        }
        width += unit.width;
    }
    output
}
