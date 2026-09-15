//! Unicode 17 properties and tailored grapheme rules from Ghostty's pinned uucode.
//! Regenerate the data with `tools/generate_unicode.py`; no build-time generator.

// One byte selects a deduplicated 128-scalar block of little-endian u16 values.
const BLOCK_SHIFT: usize = 7;
const BLOCK_SIZE: usize = 1 << BLOCK_SHIFT;
const INDEX_LEN: usize = 0x110000 / BLOCK_SIZE;
const DATA: &[u8] = include_bytes!("unicode_table.bin");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Properties {
    pub width: u8,
    pub zero_in_grapheme: bool,
    pub grapheme: u8,
    pub emoji_vs_base: bool,
}

#[inline]
pub(crate) fn properties(cp: char) -> Properties {
    let cp = cp as usize;
    let block = usize::from(DATA[cp >> BLOCK_SHIFT]);
    let i = INDEX_LEN + (block * BLOCK_SIZE + (cp & (BLOCK_SIZE - 1))) * 2;
    let value = u16::from_le_bytes([DATA[i], DATA[i + 1]]);
    Properties {
        width: (value & 3) as u8,
        zero_in_grapheme: value & 4 != 0,
        grapheme: ((value >> 3) & 31) as u8,
        emoji_vs_base: value & 256 != 0,
    }
}

/// Display width using exactly the baseline terminal's single-codepoint policy.
#[inline]
pub fn codepoint_width(cp: char) -> u8 {
    properties(cp).width
}

// Same discriminants as uucode GraphemeBreakNoControl, kept in the generated data.
const PREPEND: u8 = 1;
const RI: u8 = 2;
const SPACING: u8 = 3;
const L: u8 = 4;
const V: u8 = 5;
const T: u8 = 6;
const LV: u8 = 7;
const LVT: u8 = 8;
const ZWJ: u8 = 9;
const ZWNJ: u8 = 10;
const PICTO: u8 = 11;
const MOD_BASE: u8 = 12;
const MOD: u8 = 13;
const EXTEND: u8 = 14;
const LINKER: u8 = 15;
const CONSONANT: u8 = 16;

fn extend(g: u8) -> bool {
    matches!(g, ZWNJ | EXTEND | LINKER)
}
fn picto(g: u8) -> bool {
    matches!(g, PICTO | MOD_BASE)
}
fn indic_extend(g: u8) -> bool {
    matches!(g, EXTEND | ZWJ)
}

/// Controls are handled by the VT parser before this tailored segmentation.
// Sharing this with run printing must not add a call per scalar print.
#[inline(always)]
pub(crate) fn grapheme_break(a: char, b: char, state: &mut u8) -> bool {
    let a = properties(a).grapheme;
    let b = properties(b).grapheme;
    match *state {
        1 if a != RI || b != RI => *state = 0,
        2 if !matches!(a, EXTEND | LINKER | ZWNJ | ZWJ | PICTO | MOD_BASE | MOD)
            || !matches!(b, EXTEND | LINKER | ZWNJ | ZWJ | PICTO | MOD_BASE | MOD) =>
        {
            *state = 0
        }
        3 | 4
            if !matches!(a, CONSONANT | LINKER | EXTEND | ZWJ)
                || !matches!(b, CONSONANT | LINKER | EXTEND | ZWJ) =>
        {
            *state = 0
        }
        _ => {}
    }
    // Ordinary characters, including CJK ideographs, need no further joining rules.
    if a == 0 && b == 0 {
        return true;
    }
    if a == L && matches!(b, L | V | LV | LVT)
        || matches!(a, LV | V) && matches!(b, V | T)
        || matches!(a, LVT | T) && b == T
        || b == SPACING
        || a == PREPEND
    {
        return false;
    }
    if a == CONSONANT {
        if indic_extend(b) {
            *state = 3;
            return false;
        }
        if b == LINKER {
            *state = 4;
            return false;
        }
    } else if *state == 3 {
        if b == LINKER {
            *state = 4;
            return false;
        }
        if indic_extend(b) {
            return false;
        }
        *state = 0;
    } else if *state == 4 {
        if b == LINKER || indic_extend(b) {
            return false;
        }
        *state = 0;
        if b == CONSONANT {
            return false;
        }
    }
    if picto(a) {
        if extend(b) || b == ZWJ || a == MOD_BASE && b == MOD {
            *state = 2;
            return false;
        }
    } else if *state == 2 {
        if (extend(a) || a == MOD) && (extend(b) || b == ZWJ) {
            return false;
        }
        *state = 0;
        if a == ZWJ && picto(b) {
            return false;
        }
    }
    if a == RI && b == RI {
        if *state == 0 {
            *state = 1;
            return false;
        }
        *state = 0;
        return true;
    }
    !(extend(b) || b == ZWJ)
}

/// Width and UTF-8 byte length of the first terminal grapheme cluster.
pub fn grapheme_width(text: &str) -> (usize, u8) {
    let mut iter = text.char_indices();
    let Some((_, mut prev)) = iter.next() else {
        return (0, 0);
    };
    let mut width = codepoint_width(prev);
    let mut state = 0;
    for (offset, cp) in iter {
        let old = state;
        if grapheme_break(prev, cp, &mut state) {
            return (offset, width);
        }
        if cp == '\u{fe0f}' || cp == '\u{fe0e}' {
            if !properties(prev).emoji_vs_base {
                state = old;
                continue;
            }
            width = if cp == '\u{fe0f}' { 2 } else { 1 };
        } else if !properties(cp).zero_in_grapheme {
            width = 2;
        }
        prev = cp;
    }
    (text.len(), width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_properties_match_every_unicode_scalar() {
        // Retain the previous range representation as an independent oracle.
        let ranges: Vec<_> = include_bytes!("unicode.bin")
            .chunks_exact(6)
            .map(|range| {
                (
                    u32::from_le_bytes(range[..4].try_into().unwrap()),
                    u16::from_le_bytes(range[4..].try_into().unwrap()),
                )
            })
            .collect();
        assert_eq!(ranges[0].0, 0);
        assert_eq!((DATA.len() - INDEX_LEN) % (BLOCK_SIZE * 2), 0);
        let blocks = (DATA.len() - INDEX_LEN) / (BLOCK_SIZE * 2);
        assert!(DATA[..INDEX_LEN].iter().all(|&id| usize::from(id) < blocks));
        for (i, &(start, value)) in ranges.iter().enumerate() {
            let end = ranges.get(i + 1).map_or(0x110000, |&(start, _)| start);
            assert!(start < end);
            let expected = Properties {
                width: (value & 3) as u8,
                zero_in_grapheme: value & 4 != 0,
                grapheme: ((value >> 3) & 31) as u8,
                emoji_vs_base: value & 256 != 0,
            };
            for cp in (start..end).filter_map(char::from_u32) {
                assert_eq!(properties(cp), expected, "{cp:?}");
            }
        }
    }

    #[test]
    fn ordinary_boundaries_preserve_state_transitions_and_special_joins() {
        for (a, b) in [('天', '地'), ('a', '界'), ('é', 'Ｘ')] {
            for initial in 0..=u8::MAX {
                let mut state = initial;
                assert!(grapheme_break(a, b, &mut state));
                assert_eq!(
                    state,
                    if (1..=4).contains(&initial) {
                        0
                    } else {
                        initial
                    }
                );
            }
        }
        let mut state = 0;
        assert!(!grapheme_break('\u{0600}', '天', &mut state));
        assert!(!grapheme_break('天', '\u{0301}', &mut state));
    }

    #[test]
    fn baseline_width_rules() {
        for cp in [
            '\0', '\u{7f}', '\u{80}', '\u{301}', '\u{200b}', '\u{200d}', '\u{fe0f}',
        ] {
            assert_eq!(codepoint_width(cp), 0, "{cp:?}");
        }
        for cp in ['界', 'Ａ', '한', '😀', '🇦', '\u{2e3b}'] {
            assert_eq!(codepoint_width(cp), 2, "{cp:?}");
        }
        assert_eq!(codepoint_width('\u{10ffff}'), 1);
        assert_eq!(grapheme_width("👩🏽‍🚀x"), ("👩🏽‍🚀".len(), 2));
        assert_eq!(grapheme_width("🇨🇭🇺🇸"), ("🇨🇭".len(), 2));
        assert_eq!(grapheme_width("x\u{fe0f}"), (4, 1));
        assert_eq!(grapheme_width("❤\u{fe0f}"), (6, 2));
    }
}
