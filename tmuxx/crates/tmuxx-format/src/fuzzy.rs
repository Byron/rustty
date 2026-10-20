/*
 * Copyright (c) 2026 Nicholas Marriott <nicholas.marriott@gmail.com>
 *
 * Permission to use, copy, modify, and distribute this software for any
 * purpose with or without fee is hereby granted, provided that the above
 * copyright notice and this permission notice appear in all copies.
 *
 * THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
 * WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
 * MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
 * ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
 * WHATSOEVER RESULTING FROM LOSS OF MIND, USE, DATA OR PROFITS, WHETHER
 * IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING
 * OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
 */
//! Retained format matching semantics from tmux fuzzy.c.
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

struct Visible {
    cp: char,
    align: usize,
    offset: usize,
    width: usize,
}
struct Budget {
    start: Instant,
    steps: usize,
}
impl Budget {
    fn check(&mut self) -> Option<()> {
        self.steps += 1;
        (!self.steps.is_multiple_of(10_000) || self.start.elapsed() < Duration::from_millis(100))
            .then_some(())
    }
}

fn scan(text: &[u8], budget: &mut Budget) -> Option<(Vec<Visible>, [usize; 4])> {
    let (units, _) = super::width::units(text);
    let mut chars = Vec::new();
    let mut widths = [0; 4];
    let mut align = 0;
    for unit in units {
        budget.check()?;
        for cp in std::iter::repeat_n('#', if unit.hashes > 0 { unit.width } else { 0 })
            .chain(std::str::from_utf8(unit.text).unwrap_or_default().chars())
        {
            let width = cp.width().unwrap_or(0);
            chars.push(Visible {
                cp,
                align,
                offset: widths[align],
                width,
            });
            widths[align] += width;
        }
        if !unit.style.is_empty() {
            let style = &unit.style[2..unit.style.len() - 1];
            if let Some(next) = style_alignment(style, align) {
                align = next;
            }
        }
    }
    Some((chars, widths))
}

// A failed style parse rolls back all alignment changes in that style.
fn style_alignment(style: &[u8], mut align: usize) -> Option<usize> {
    let number = |value: &str, maximum: u32| {
        value
            .trim_start()
            .parse::<u32>()
            .ok()
            .is_some_and(|n| n <= maximum)
    };
    let attributes = |value: &str| {
        matches!(value, "none" | "default")
            || (!value.starts_with('|')
                && !value.ends_with('|')
                && value.split('|').filter(|s| !s.is_empty()).all(|s| {
                    matches!(
                        s,
                        "acs"
                            | "bright"
                            | "bold"
                            | "dim"
                            | "underscore"
                            | "blink"
                            | "reverse"
                            | "hidden"
                            | "italics"
                            | "strikethrough"
                            | "double-underscore"
                            | "curly-underscore"
                            | "dotted-underscore"
                            | "dashed-underscore"
                            | "overline"
                    )
                }))
    };
    for token in style
        .split(|b| b" ,\n".contains(b))
        .filter(|s| !s.is_empty())
    {
        if token.len() > 255 {
            return None;
        }
        let token = std::str::from_utf8(token).ok()?.to_ascii_lowercase();
        let valid = if let Some(value) = token.strip_prefix("align=") {
            align = match value {
                "left" => 0,
                "centre" => 1,
                "right" => 2,
                "absolute-centre" => 3,
                _ => return None,
            };
            true
        } else if token == "noalign" {
            align = 0;
            true
        } else if matches!(
            token.as_str(),
            "default"
                | "ignore"
                | "noignore"
                | "push-default"
                | "pop-default"
                | "set-default"
                | "nl"
                | "nolist"
                | "norange"
                | "none"
        ) {
            true
        } else if let Some(value) = token.strip_prefix("list=") {
            matches!(value, "on" | "focus" | "left-marker" | "right-marker")
        } else if let Some(value) = token.strip_prefix("range=").filter(|s| !s.is_empty()) {
            let (kind, arg) = value
                .split_once('|')
                .map_or((value, None), |(kind, arg)| (kind, Some(arg)));
            if arg == Some("") {
                false
            } else {
                match kind {
                    "left" | "right" => arg.is_none(),
                    "control" => arg.is_some_and(|arg| number(arg, 9)),
                    "window" => arg.is_some_and(|arg| number(arg, u32::MAX)),
                    "pane" => arg
                        .and_then(|arg| arg.strip_prefix('%'))
                        .is_some_and(|arg| number(arg, u32::MAX)),
                    "session" => arg
                        .and_then(|arg| arg.strip_prefix('$'))
                        .is_some_and(|arg| number(arg, u32::MAX)),
                    "user" => arg.is_some(),
                    _ => true,
                }
            }
        } else if let Some(value) = token
            .strip_prefix("fill=")
            .or_else(|| token.strip_prefix("fg="))
            .or_else(|| token.strip_prefix("bg="))
            .or_else(|| token.strip_prefix("us="))
        {
            super::color::valid(value)
        } else if let Some(value) = token.strip_prefix("dim=") {
            number(value.strip_suffix('%').unwrap_or(value), 100)
        } else if let Some(value) = token.strip_prefix("width=") {
            value
                .strip_suffix('%')
                .map_or_else(|| number(value, u32::MAX), |value| number(value, 100))
        } else if let Some(value) = token.strip_prefix("pad=") {
            number(value, u32::MAX)
        } else if token.starts_with("link=") {
            true
        } else if let Some(value) = token.strip_prefix("no") {
            matches!(value, "link" | "attr") || attributes(value)
        } else {
            attributes(&token)
        };
        if !valid {
            return None;
        }
    }
    Some(align)
}

fn boundary(cp: char) -> bool {
    " -_/.:".contains(cp)
}
fn equal(a: char, b: char, fold: bool) -> bool {
    if fold && a.is_ascii() && b.is_ascii() {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

fn term(
    raw: &str,
    chars: &[Visible],
    fold: bool,
    marked: &mut [bool],
    budget: &mut Budget,
) -> Option<Option<i64>> {
    let inverse = raw.starts_with('!');
    let raw = if inverse { &raw[1..] } else { raw };
    let exact = raw.starts_with('\'') || raw.starts_with('^');
    let prefix = raw.starts_with('^');
    let raw = if exact { &raw[1..] } else { raw };
    let suffix = raw.ends_with('$');
    let raw = if suffix { &raw[..raw.len() - 1] } else { raw };
    if raw.is_empty() {
        return Some(None);
    }
    let needle = raw.chars().collect::<Vec<_>>();
    if needle.len() > chars.len() {
        return Some(inverse.then_some(0));
    }
    let mut positions = Vec::new();
    let mut score = 0;
    if inverse || exact || suffix {
        if prefix && suffix && needle.len() != chars.len() {
            return Some(inverse.then_some(0));
        }
        let start = if suffix {
            chars.len() - needle.len()
        } else {
            0
        };
        let end = if prefix || suffix {
            start + 1
        } else {
            chars.len() - needle.len() + 1
        };
        let mut best = None;
        for index in start..end {
            let mut matched = true;
            for (offset, cp) in needle.iter().enumerate() {
                budget.check()?;
                if !equal(*cp, chars[index + offset].cp, fold) {
                    matched = false;
                    break;
                }
            }
            if matched {
                let value = 1000
                    + needle.len() as i64 * 6
                    + i64::from(prefix) * 200
                    + i64::from(suffix) * 100
                    + if index == 0 {
                        12
                    } else if boundary(chars[index - 1].cp) {
                        8
                    } else {
                        0
                    }
                    - index.min(10) as i64
                    - if !prefix && !suffix {
                        (chars.len() - index - needle.len()) as i64
                    } else {
                        0
                    };
                if best.is_none_or(|(_, best_score)| value > best_score) {
                    best = Some((index, value));
                }
            }
        }
        if inverse {
            return Some(best.is_none().then_some(0));
        }
        let Some((start, value)) = best else {
            return Some(None);
        };
        positions.extend(start..start + needle.len());
        score = value;
    } else {
        let mut index = 0;
        for cp in &needle {
            while index < chars.len() && !equal(*cp, chars[index].cp, fold) {
                budget.check()?;
                index += 1;
            }
            if index == chars.len() {
                return Some(None);
            }
            positions.push(index);
            index += 1;
        }
        index = *positions.last().unwrap();
        for pi in (0..needle.len()).rev() {
            while !equal(needle[pi], chars[index].cp, fold) {
                budget.check()?;
                index -= 1;
            }
            positions[pi] = index;
            if pi > 0 {
                index -= 1;
            }
        }
        score += if positions[0] == 0 {
            12
        } else {
            i64::from(boundary(chars[positions[0] - 1].cp)) * 8 - positions[0].min(10) as i64
        };
        for pair in positions.windows(2) {
            score += if pair[1] == pair[0] + 1 {
                6
            } else {
                i64::from(boundary(chars[pair[1] - 1].cp)) * 8
            };
        }
        score -= (positions.last().unwrap() - positions[0] + 1 - positions.len()) as i64;
    }
    for index in positions {
        marked[index] = true;
    }
    Some(Some(score))
}

pub(super) fn matches(pattern: &[u8], text: &[u8], positions: bool) -> Vec<u8> {
    fn run(pattern: &[u8], text: &[u8], positions: bool) -> Option<Vec<u8>> {
        let pattern = std::str::from_utf8(pattern).ok()?;
        if pattern.bytes().all(|b| matches!(b, b' ' | b'|')) {
            return Some(if positions { Vec::new() } else { b"1".to_vec() });
        }
        let mut budget = Budget {
            start: Instant::now(),
            steps: 0,
        };
        let (chars, widths) = scan(text, &mut budget)?;
        let fold = !pattern.bytes().any(|b| b.is_ascii_uppercase());
        let mut best: Option<(i64, Vec<bool>)> = None;
        for group in pattern
            .split('|')
            .filter(|s| !s.trim_matches(' ').is_empty())
        {
            let mut marked = vec![false; chars.len()];
            let mut score = 0;
            let mut matched = true;
            for raw in group.split(' ').filter(|s| !s.is_empty()) {
                match term(raw, &chars, fold, &mut marked, &mut budget)? {
                    Some(value) => score += value,
                    None => {
                        matched = false;
                        break;
                    }
                }
            }
            if matched && best.as_ref().is_none_or(|(value, _)| score > *value) {
                best = Some((score, marked));
            }
        }
        let (_, marked) = best?;
        if !positions {
            return Some(b"1".to_vec());
        }
        let width = super::width::width(text).max(1);
        let [mut left, mut centre, mut right, absolute] = widths;
        let mut excess = (left + centre + right).saturating_sub(width);
        let take = centre.min(excess);
        centre -= take;
        excess -= take;
        let take = right.min(excess);
        right -= take;
        excess -= take;
        left = left.saturating_sub(excess);
        let visible = [left, centre, right, absolute.min(width)];
        let start = [
            0,
            left + (width - right - left) / 2 - centre / 2,
            width - right,
            (width - visible[3]) / 2,
        ];
        let source = [0, widths[1] / 2 - centre / 2, widths[2] - right, 0];
        let mut columns = std::collections::BTreeSet::new();
        for (cp, matched) in chars.iter().zip(marked) {
            budget.check()?;
            if !matched
                || cp.offset < source[cp.align]
                || cp.offset >= source[cp.align] + visible[cp.align]
            {
                continue;
            }
            let column = start[cp.align] + cp.offset - source[cp.align];
            columns.extend(column..(column + cp.width).min(width));
        }
        Some(
            columns
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",")
                .into_bytes(),
        )
    }
    run(pattern, text, positions)
        .unwrap_or_else(|| if positions { Vec::new() } else { b"0".to_vec() })
}
