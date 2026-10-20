//! Context-supplied tmux format expansion. This crate never starts shell jobs.

mod color;
mod fuzzy;
#[cfg(unix)]
mod locale;
#[cfg(windows)]
#[path = "windows_locale.rs"]
mod locale;
#[cfg(unix)]
mod regex;
#[cfg(windows)]
#[path = "windows_regex.rs"]
mod regex;
#[cfg(any(unix, windows))]
mod time;
mod width;
#[cfg(windows)]
mod windows_glob;
#[cfg(windows)]
mod windows_paths;

use std::fmt;
use unicode_width::UnicodeWidthChar;

/// A runtime lookup can resolve this prefix followed by a raw #() command.
/// The NUL prefix cannot collide with a tmux field or option name.
pub const JOB_PREFIX: &[u8] = b"\0tmuxx-job:";

/// Match the same POSIX regex or glob used by C pane-content formats.
pub fn content_match(
    pattern: &[u8],
    text: &[u8],
    regex: bool,
    ignore: bool,
) -> Result<bool, Error> {
    if regex {
        #[cfg(unix)]
        return Ok(self::regex::matches(pattern, text, ignore));
        #[cfg(windows)]
        return self::regex::matches(pattern, text, ignore).map_err(|message| error(0, message));
        #[cfg(not(any(unix, windows)))]
        return Err(error(0, "POSIX matching is unavailable on this platform"));
    }
    let pattern = [b"*", pattern, b"*"].concat();
    glob_match_case(&pattern, text, ignore).map_err(|message| error(0, message))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "format byte {}: {}", self.offset, self.message)
    }
}
impl std::error::Error for Error {}

fn error(offset: usize, message: impl Into<String>) -> Error {
    Error {
        offset,
        message: message.into(),
    }
}

pub fn truthy(value: &[u8]) -> bool {
    !value.is_empty() && value != b"0"
}

pub fn expand(
    input: &[u8],
    mut lookup: impl FnMut(&[u8]) -> Option<Vec<u8>>,
) -> Result<Vec<u8>, Error> {
    expand_inner(input, &mut lookup, 0, 0, false)
}

/// Expand strftime directives before formats, as display-message does.
pub fn expand_time(
    input: &[u8],
    lookup: impl FnMut(&[u8]) -> Option<Vec<u8>>,
) -> Result<Vec<u8>, Error> {
    #[cfg(any(unix, windows))]
    {
        expand(&time::expand(input, time::now()), lookup)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (input, lookup);
        Err(error(0, "time formatting requires a Unix platform"))
    }
}

/// Configuration conditions suppress job expressions, matching FORMAT_NOJOBS.
pub fn expand_no_jobs(
    input: &[u8],
    mut lookup: impl FnMut(&[u8]) -> Option<Vec<u8>>,
) -> Result<Vec<u8>, Error> {
    expand_inner(input, &mut lookup, 0, 0, true)
}

fn expand_inner(
    input: &[u8],
    lookup: &mut dyn FnMut(&[u8]) -> Option<Vec<u8>>,
    depth: usize,
    base: usize,
    no_jobs: bool,
) -> Result<Vec<u8>, Error> {
    if depth > 100 {
        return Err(error(base, "format nesting limit exceeded"));
    }
    let mut output = Vec::new();
    let mut i = 0;
    let mut style_end = None;
    while i < input.len() {
        if input[i] != b'#' {
            output.push(input[i]);
            i += 1;
            continue;
        }
        if i + 1 == input.len() {
            break;
        }
        match input[i + 1] {
            b'{' => {
                let Some(end) = matching_end(input, i + 2) else {
                    break;
                };
                let result =
                    expression(&input[i + 2..end], lookup, depth + 1, base + i + 2, no_jobs);
                match result {
                    Ok(value) => output.extend(value),
                    Err(error) if error.message == "invalid format syntax" => break,
                    Err(error) => return Err(error),
                }
                i = end + 1;
            }
            b'(' => {
                let mut nesting = 1;
                i += 2;
                let start = i;
                while i < input.len() && nesting != 0 {
                    match input[i] {
                        b'(' => nesting += 1,
                        b')' => nesting -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                if nesting != 0 {
                    break;
                }
                if !no_jobs {
                    let key = [JOB_PREFIX, &input[start..i - 1]].concat();
                    output.extend(lookup(&key).ok_or_else(|| {
                        error(
                            base + start - 2,
                            "shell-job formats require a runtime and are unsupported",
                        )
                    })?);
                }
            }
            b'#' | b'[' => {
                let mut end = i + 1;
                while input.get(end) == Some(&b'#') {
                    end += 1;
                }
                if input.get(end) == Some(&b'[') {
                    style_end = width::style_end(input, end + 1);
                    output.extend_from_slice(&input[i..=end]);
                    i = end + 1;
                } else if input[i + 1] == b'#' {
                    output.push(b'#');
                    i += 2;
                } else {
                    output.push(b'#');
                    i += 1;
                }
            }
            b',' | b'}' => {
                output.push(input[i + 1]);
                i += 2;
            }
            alias if alias_key(alias).is_some() && style_end.is_none_or(|end| i > end) => {
                output.extend(lookup(alias_key(alias).unwrap()).unwrap_or_default());
                i += 2;
            }
            _ => {
                output.push(b'#');
                i += 1;
            }
        }
    }
    Ok(output)
}

fn alias_key(alias: u8) -> Option<&'static [u8]> {
    Some(match alias {
        b'S' => b"session_name",
        b'W' => b"window_name",
        b'I' => b"window_index",
        b'P' => b"pane_index",
        b'D' => b"pane_id",
        b'T' => b"pane_title",
        b'H' => b"host",
        b'h' => b"host_short",
        b'F' => b"window_flags",
        _ => return None,
    })
}

fn matching_end(input: &[u8], mut i: usize) -> Option<usize> {
    let mut depth = 1;
    while i < input.len() {
        if input[i] == b'#' && i + 1 < input.len() {
            if input[i + 1] == b'{' {
                depth += 1;
            }
            i += 2;
        } else {
            if input[i] == b'}' {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            i += 1;
        }
    }
    None
}

fn split(input: &[u8], separator: u8) -> Vec<&[u8]> {
    let mut parts = Vec::new();
    let mut start = 0;
    while let Some(next) = skip(input, start, &[separator]) {
        parts.push(&input[start..next]);
        start = next + 1;
    }
    parts.push(&input[start..]);
    parts
}

/// Split loop branches while keeping nested formats and escaped commas intact.
pub fn loop_branches(input: &[u8]) -> (&[u8], Option<&[u8]>) {
    match skip(input, 0, b",") {
        Some(index) => (&input[..index], Some(&input[index + 1..])),
        None => (input, None),
    }
}

struct Modifier {
    name: Vec<u8>,
    arguments: Vec<Vec<u8>>,
}
impl Modifier {
    fn flags(&self) -> &[u8] {
        self.arguments
            .first()
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

fn skip(input: &[u8], start: usize, delimiters: &[u8]) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = start;
    while i < input.len() {
        if input[i] == b'#' && input.get(i + 1) == Some(&b'{') {
            depth += 1;
        }
        if input[i] == b'#' && input.get(i + 1).is_some_and(|b| b",#{}:".contains(b)) {
            i += 2;
            continue;
        }
        if input[i] == b'}' {
            depth -= 1;
        }
        if depth == 0 && delimiters.contains(&input[i]) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn unescape(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut depth = 0i32;
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'#' && input.get(i + 1) == Some(&b'{') {
            depth += 1;
        }
        if depth == 0 && input[i] == b'#' && input.get(i + 1).is_some_and(|b| b",#{}:".contains(b))
        {
            i += 1;
            output.push(input[i]);
            i += 1;
            continue;
        }
        if input[i] == b'}' {
            depth -= 1;
        }
        output.push(input[i]);
        i += 1;
    }
    output
}

type RawModifiers<'a> = Vec<(Vec<u8>, Vec<&'a [u8]>)>;

fn parse_modifiers(input: &[u8]) -> Option<(RawModifiers<'_>, &[u8])> {
    let end = |byte: Option<&u8>| byte.is_some_and(|b| matches!(b, b';' | b':'));
    let mut modifiers = Vec::new();
    let mut i = 0;
    while i < input.len() && input[i] != b':' {
        if input[i] == b';' {
            i += 1;
        }
        if input.get(i) == Some(&b':') {
            break;
        }
        let name = *input.get(i)?;
        if b"labdnwETSWPOVL!<>A".contains(&name) && end(input.get(i + 1)) {
            modifiers.push((vec![name], Vec::new()));
            i += 1;
            continue;
        }
        if input
            .get(i..i + 2)
            .is_some_and(|s| matches!(s, b"||" | b"&&" | b"!!" | b"!=" | b"==" | b"<=" | b">="))
            && end(input.get(i + 2))
        {
            modifiers.push((input[i..i + 2].to_vec(), Vec::new()));
            i += 2;
            continue;
        }
        if !b"ImCLNPSOVst=pReqWcA".contains(&name) {
            return None;
        }
        i += 1;
        let mut arguments = Vec::new();
        if end(input.get(i)) {
        } else if !input.get(i)?.is_ascii_punctuation() || input[i] == b'-' {
            let next = skip(input, i, b":;")?;
            arguments.push(&input[i..next]);
            i = next;
        } else {
            let delimiter = input[i];
            loop {
                if input[i] == delimiter && end(input.get(i + 1)) {
                    i += 1;
                    break;
                }
                let next = skip(input, i + 1, &[delimiter, b';', b':'])?;
                arguments.push(&input[i + 1..next]);
                i = next;
                if end(input.get(i)) {
                    break;
                }
            }
        }
        modifiers.push((vec![name], arguments));
    }
    (input.get(i) == Some(&b':')).then(|| (modifiers, &input[i + 1..]))
}

fn expression(
    input: &[u8],
    lookup: &mut dyn FnMut(&[u8]) -> Option<Vec<u8>>,
    depth: usize,
    base: usize,
    no_jobs: bool,
) -> Result<Vec<u8>, Error> {
    let Some((raw, body)) = parse_modifiers(input) else {
        return value(input, &[], lookup, depth, base, no_jobs);
    };
    if raw.len() > 128 {
        return Err(error(base, "too many format modifiers"));
    }
    let modifiers = raw
        .into_iter()
        .map(|(name, arguments)| {
            Ok(Modifier {
                name,
                arguments: arguments
                    .into_iter()
                    .map(|argument| expand_inner(&unescape(argument), lookup, depth, base, no_jobs))
                    .collect::<Result<_, Error>>()?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    compound(&modifiers, body, lookup, depth, base, no_jobs)
}

fn quote_modifier(modifier: &Modifier) -> Option<&'static [u8]> {
    if modifier.name != b"q" {
        return None;
    }
    let flags = modifier.flags();
    if modifier.arguments.is_empty() {
        Some(b"q")
    } else if flags.contains(&b's') {
        Some(b"q/s")
    } else if flags.contains(&b'h') || flags.contains(&b'e') {
        Some(b"q/h")
    } else if flags.contains(&b'a') {
        Some(b"q/a")
    } else {
        None
    }
}

fn find_value(
    body: &[u8],
    modifiers: &[Modifier],
    lookup: &mut dyn FnMut(&[u8]) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let mut value = lookup(body)?;
    if modifiers.iter().any(|m| m.name == b"t") {
        let flags = modifiers
            .iter()
            .filter(|m| m.name == b"t")
            .flat_map(|m| m.flags())
            .copied()
            .collect::<Vec<_>>();
        let template = modifiers
            .iter()
            .rev()
            .find(|m| {
                m.name == b"t"
                    && m.flags().contains(&b'f')
                    && !m.flags().iter().any(|b| b"prd".contains(b))
                    && m.arguments.len() >= 2
            })
            .map(|m| m.arguments[1].as_slice());
        #[cfg(any(unix, windows))]
        {
            value = time::format(&value, &flags, template);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (flags, template);
            value.clear();
        }
    } else {
        if modifiers.iter().any(|m| m.name == b"b") {
            value = path_basename(&value);
        }
        if modifiers.iter().any(|m| m.name == b"d") {
            value = path_dirname(&value);
        }
        for quote in [b"q".as_slice(), b"q/s", b"q/h", b"q/a"] {
            if modifiers.iter().any(|m| quote_modifier(m) == Some(quote)) {
                value = quote_value(quote, &value);
            }
        }
    }
    Some(value)
}

fn value(
    body: &[u8],
    modifiers: &[Modifier],
    lookup: &mut dyn FnMut(&[u8]) -> Option<Vec<u8>>,
    depth: usize,
    base: usize,
    no_jobs: bool,
) -> Result<Vec<u8>, Error> {
    if let Some(rest) = body.strip_prefix(b"?") {
        let parts = split(rest, b',');
        let (pairs, remainder) = parts.as_chunks::<2>();
        for pair in pairs {
            let condition = if let Some(value) = find_value(pair[0], modifiers, lookup) {
                value
            } else {
                let expanded = expand_inner(pair[0], lookup, depth, base, no_jobs)?;
                if expanded == pair[0] {
                    Vec::new()
                } else {
                    expanded
                }
            };
            if truthy(&condition) {
                return expand_inner(pair[1], lookup, depth, base, no_jobs);
            }
        }
        return expand_inner(
            remainder.first().copied().unwrap_or_default(),
            lookup,
            depth,
            base,
            no_jobs,
        );
    }
    if body.windows(2).any(|bytes| bytes == b"#{") {
        expand_inner(body, lookup, depth, base, no_jobs)
    } else {
        Ok(find_value(body, modifiers, lookup).unwrap_or_default())
    }
}

fn pair(body: &[u8], base: usize) -> Result<[&[u8]; 2], Error> {
    let index = skip(body, 0, b",").ok_or_else(|| error(base, "invalid format syntax"))?;
    Ok([&body[..index], &body[index + 1..]])
}

fn compound(
    modifiers: &[Modifier],
    body: &[u8],
    lookup: &mut dyn FnMut(&[u8]) -> Option<Vec<u8>>,
    depth: usize,
    base: usize,
    no_jobs: bool,
) -> Result<Vec<u8>, Error> {
    // C selects one primary operation by priority, independently of source order.
    let rank = |m: &Modifier| -> Option<usize> {
        Some(match m.name.as_slice() {
            b"I" if m.flags().iter().any(|b| b"fce".contains(b)) => 0,
            b"A" => 1,
            b"l" => 2,
            b"a" => 3,
            b"c" => 4,
            b"S" => 5,
            b"W" => 6,
            b"P" => 7,
            b"L" => 8,
            b"O" => 9,
            b"V" => 10,
            b"N" if m.arguments.is_empty() || m.flags().contains(&b'w') => 11,
            b"N" if m.flags().contains(&b's') => 12,
            b"C" => 13,
            b"R" => 14,
            b"!" => 15,
            b"!!" => 16,
            b"&&" | b"||" => 17,
            b"m" | b"==" | b"!=" | b"<" | b">" | b"<=" | b">=" => 18,
            b"e" if (1..=3).contains(&m.arguments.len()) && !body.starts_with(b"?") => 19,
            _ => return None,
        })
    };
    let primary = modifiers
        .iter()
        .rev()
        .filter_map(|m| rank(m).map(|rank| (rank, m)))
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, m)| m);
    let mut value = if primary.is_some_and(|m| m.name == b"c") {
        let flags = modifiers
            .iter()
            .filter(|m| m.name == b"c")
            .flat_map(|m| m.flags())
            .copied()
            .collect::<Vec<_>>();
        color::format(&expand_inner(body, lookup, depth, base, no_jobs)?, &flags)
    } else if let Some(primary) = primary {
        operator(primary, body, lookup, depth, base, no_jobs)?
    } else {
        value(body, modifiers, lookup, depth, base, no_jobs)?
    };
    if modifiers.iter().any(|m| m.name == b"E") {
        value = expand_inner(&value, lookup, depth, base, no_jobs)?;
    } else if modifiers.iter().any(|m| m.name == b"T") {
        #[cfg(any(unix, windows))]
        {
            value = expand_inner(
                &time::expand(&value, time::now()),
                lookup,
                depth,
                base,
                no_jobs,
            )?;
        }
        #[cfg(not(any(unix, windows)))]
        return Err(error(base, "time formatting requires a Unix platform"));
    }
    for modifier in modifiers
        .iter()
        .filter(|m| m.name == b"s" && m.arguments.len() >= 2)
    {
        let pattern = expand_inner(&modifier.arguments[0], lookup, depth, base, no_jobs)?;
        let replacement = expand_inner(&modifier.arguments[1], lookup, depth, base, no_jobs)?;
        let ignore = modifier
            .arguments
            .get(2)
            .is_some_and(|arg| arg.contains(&b'i'));
        #[cfg(unix)]
        {
            value = regex::substitute(&pattern, &replacement, &value, ignore)
                .ok_or_else(|| error(base, "substitution output exceeds limit"))?;
        }
        #[cfg(windows)]
        {
            value = regex::substitute(&pattern, &replacement, &value, ignore)
                .map_err(|message| error(base, message))?;
        }
        #[cfg(not(any(unix, windows)))]
        return Err(error(
            base,
            "POSIX substitution is unavailable on this platform",
        ));
    }
    for kind in *b"=p" {
        if let Some(modifier) = modifiers
            .iter()
            .rev()
            .find(|m| m.name == [kind] && !m.arguments.is_empty())
        {
            let marker = modifiers
                .iter()
                .rev()
                .find(|m| m.name == [kind] && m.arguments.len() > 1)
                .map(|m| m.arguments[1].as_slice())
                .unwrap_or_default();
            value = width_modifier(kind, &modifier.arguments[0], marker, value, base)?;
        }
    }
    if modifiers.iter().any(|m| m.name == b"n") {
        value = value.len().to_string().into_bytes();
    }
    if modifiers.iter().any(|m| m.name == b"w") {
        value = width::width(&value).to_string().into_bytes();
    }
    Ok(value)
}

fn operator(
    modifier: &Modifier,
    body: &[u8],
    lookup: &mut dyn FnMut(&[u8]) -> Option<Vec<u8>>,
    depth: usize,
    base: usize,
    no_jobs: bool,
) -> Result<Vec<u8>, Error> {
    let name = modifier.name.as_slice();
    let flags = modifier.flags();
    match name {
        b"I" | b"A" => Ok(Vec::new()), // No physical tty or status renderer in native/control contexts.
        b"l" => Ok(unescape(body)),
        b"a" => {
            let value = expand_inner(body, lookup, depth, base, no_jobs)?;
            let code = std::str::from_utf8(&value)
                .ok()
                .and_then(|s| s.trim_start().parse::<u8>().ok())
                .filter(|n| (32..=126).contains(n));
            Ok(code.map_or_else(Vec::new, |n| vec![n]))
        }
        b"c" => Ok(color::format(
            &expand_inner(body, lookup, depth, base, no_jobs)?,
            flags,
        )),
        b"S" | b"W" | b"P" | b"L" | b"O" | b"V" | b"N" | b"C" => {
            let flags = if matches!(name, b"O" | b"V") && modifier.arguments.len() != 1 {
                b""
            } else {
                flags
            };
            let key = [name, b"/", flags, b":", body].concat();
            lookup(&key).ok_or_else(|| error(base, "contextual formats require an engine context"))
        }
        b"R" => {
            let [left, right] = pair(body, base)?;
            let value = expand_inner(left, lookup, depth, base, no_jobs)?;
            let count = expand_inner(right, lookup, depth, base, no_jobs)?;
            let count = std::str::from_utf8(&count)
                .ok()
                .and_then(|s| s.trim_start().parse::<usize>().ok());
            Ok(match count {
                Some(n) if n <= 10000 && value.len().checked_mul(n).is_some_and(|n| n <= 65536) => {
                    value.repeat(n)
                }
                _ => Vec::new(),
            })
        }
        b"!" | b"!!" => {
            let value = expand_inner(body, lookup, depth, base, no_jobs)?;
            Ok(if truthy(&value) == (name == b"!!") {
                b"1".to_vec()
            } else {
                b"0".to_vec()
            })
        }
        b"&&" | b"||" => {
            let all = name == b"&&";
            for part in split(body, b',') {
                if truthy(&expand_inner(part, lookup, depth, base, no_jobs)?) != all {
                    return Ok(if all { b"0".to_vec() } else { b"1".to_vec() });
                }
            }
            Ok(if all { b"1".to_vec() } else { b"0".to_vec() })
        }
        b"e" => {
            let Ok(parts) = pair(body, base) else {
                return Ok(Vec::new());
            };
            let values = parts
                .into_iter()
                .map(|part| expand_inner(part, lookup, depth, base, no_jobs))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(arithmetic(&modifier.arguments, &values))
        }
        _ => {
            let [left, right] = pair(body, base)?;
            let left = expand_inner(left, lookup, depth, base, no_jobs)?;
            let right = expand_inner(right, lookup, depth, base, no_jobs)?;
            if name == b"m" && (flags.contains(&b'z') || flags.contains(&b'p')) {
                return Ok(fuzzy::matches(&left, &right, flags.contains(&b'p')));
            }
            let matched = match name {
                b"==" => left == right,
                b"!=" => left != right,
                b"<" => left < right,
                b">" => left > right,
                b"<=" => left <= right,
                b">=" => left >= right,
                b"m" if flags.contains(&b'r') => {
                    #[cfg(unix)]
                    {
                        regex::matches(&left, &right, flags.contains(&b'i'))
                    }
                    #[cfg(windows)]
                    {
                        regex::matches(&left, &right, flags.contains(&b'i'))
                            .map_err(|message| error(base, message))?
                    }
                    #[cfg(not(any(unix, windows)))]
                    return Err(error(
                        base,
                        "POSIX matching is unavailable on this platform",
                    ));
                }
                b"m" => glob_match_case(&left, &right, flags.contains(&b'i'))
                    .map_err(|message| error(base, message))?,
                _ => unreachable!(),
            };
            Ok(if matched {
                b"1".to_vec()
            } else {
                b"0".to_vec()
            })
        }
    }
}

fn quote_value(modifier: &[u8], value: &[u8]) -> Vec<u8> {
    match modifier {
        b"q" => {
            let mut quoted = Vec::new();
            for &b in value {
                if b"|&;<>(){}$`\\\"'*?[# =%\n\t".contains(&b) {
                    quoted.push(b'\\');
                }
                quoted.push(b);
            }
            quoted
        }
        b"q/s" => {
            let mut quoted = vec![b'\''];
            for &b in value {
                if b == b'\'' {
                    quoted.extend_from_slice(b"'\\''");
                } else {
                    quoted.push(b);
                }
            }
            quoted.push(b'\'');
            quoted
        }
        b"q/h" => {
            let mut quoted = Vec::new();
            for &b in value {
                quoted.push(b);
                if b == b'#' {
                    quoted.push(b);
                }
            }
            quoted
        }
        b"q/a" => tmuxx_command::display_argument(value),
        _ => unreachable!(),
    }
}

fn arithmetic(args: &[Vec<u8>], values: &[Vec<u8>]) -> Vec<u8> {
    if values.len() != 2 || args.len() > 3 {
        return Vec::new();
    }
    let floating = args.get(1).is_some_and(|flags| flags.contains(&b'f'));
    let precision = if let Some(raw) = args.get(2) {
        match std::str::from_utf8(raw).ok().and_then(|s| {
            s.trim_start_matches(|c: char| c.is_ascii_whitespace())
                .parse::<i32>()
                .ok()
        }) {
            Some(n) if (-100..=100).contains(&n) => n,
            _ => return Vec::new(),
        }
    } else if floating {
        2
    } else {
        0
    };
    let number = |v: &[u8]| -> Option<f64> {
        #[cfg(unix)]
        let n = {
            let text = std::ffi::CString::new(v).ok()?;
            let mut end = std::ptr::null_mut();
            let n = locale::with(|| unsafe { libc::strtod(text.as_ptr(), &mut end) })?;
            if unsafe { *end } != 0 {
                return None;
            }
            n
        };
        #[cfg(not(unix))]
        let n = {
            #[cfg(windows)]
            {
                locale::number(v)?
            }
            #[cfg(not(windows))]
            {
                let s = std::str::from_utf8(v).ok()?;
                if s.is_empty() {
                    0.0
                } else {
                    s.trim_start().parse().ok()?
                }
            }
        };
        if floating { Some(n) } else { integer(n) }
    };
    let (Some(left), Some(right)) = (number(&values[0]), number(&values[1])) else {
        return Vec::new();
    };
    let result = match args[0].as_slice() {
        b"+" => left + right,
        b"-" => left - right,
        b"*" => left * right,
        b"/" => left / right,
        b"m" | b"%" => left % right,
        b"==" => f64::from((left - right).abs() < 1e-9),
        b"!=" => f64::from((left - right).abs() > 1e-9),
        b"<" => f64::from(left < right),
        b">" => f64::from(left > right),
        b"<=" => f64::from(left <= right),
        b">=" => f64::from(left >= right),
        _ => return Vec::new(),
    };
    let result = if floating {
        result
    } else if let Some(n) = integer(result) {
        n
    } else {
        return Vec::new();
    };
    #[cfg(unix)]
    {
        let mut output = vec![0u8; 512];
        // A finite f64 needs at most309 integer digits plus100 decimal places.
        let length = locale::with(|| unsafe {
            libc::snprintf(
                output.as_mut_ptr().cast(),
                output.len(),
                c"%.*f".as_ptr(),
                precision,
                result,
            )
        })
        .unwrap_or(-1);
        if length < 0 || length as usize >= output.len() {
            return Vec::new();
        }
        output.truncate(length as usize);
        output
    }
    #[cfg(windows)]
    {
        locale::number_text(result, precision).unwrap_or_default()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let precision = if precision < 0 { 6 } else { precision as usize };
        format!("{result:.precision$}").into_bytes()
    }
}

fn integer(value: f64) -> Option<f64> {
    (value.is_finite() && (-9223372036854775808.0..9223372036854775808.0).contains(&value))
        .then(|| value.trunc())
}

fn width_modifier(
    kind: u8,
    raw: &[u8],
    marker: &[u8],
    value: Vec<u8>,
    base: usize,
) -> Result<Vec<u8>, Error> {
    let width = std::str::from_utf8(raw)
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|n| (-10000..=10000).contains(n))
        .unwrap_or(0);
    if width == 0 {
        return Ok(value);
    }
    let columns = width.unsigned_abs() as usize;
    if kind == b'p' {
        let original_width: usize = std::str::from_utf8(&value)
            .map_err(|_| error(base, "padding requires UTF-8"))?
            .chars()
            .map(|c| c.width().unwrap_or(0))
            .sum();
        if original_width >= columns {
            return Ok(value);
        }
        let mut padding = vec![b' '; columns - original_width];
        if width < 0 {
            padding.extend(value);
            Ok(padding)
        } else {
            let mut result = value;
            result.extend(padding);
            Ok(result)
        }
    } else {
        let mut trimmed = width::trim(&value, columns, width < 0);
        let marker = if trimmed != value { marker } else { b"" };
        if width < 0 {
            let mut result = marker.to_vec();
            result.extend(trimmed);
            Ok(result)
        } else {
            trimmed.extend_from_slice(marker);
            Ok(trimmed)
        }
    }
}

#[cfg(windows)]
fn path_basename(path: &[u8]) -> Vec<u8> {
    windows_paths::basename(path)
}

#[cfg(not(windows))]
fn path_basename(path: &[u8]) -> Vec<u8> {
    if path.is_empty() {
        return b".".to_vec();
    }
    let end = path.iter().rposition(|&b| b != b'/').map_or(0, |i| i + 1);
    if end == 0 {
        return b"/".to_vec();
    }
    let start = path[..end]
        .iter()
        .rposition(|&b| b == b'/')
        .map_or(0, |i| i + 1);
    path[start..end].to_vec()
}

#[cfg(windows)]
fn path_dirname(path: &[u8]) -> Vec<u8> {
    windows_paths::dirname(path)
}

#[cfg(not(windows))]
fn path_dirname(path: &[u8]) -> Vec<u8> {
    let end = path.iter().rposition(|&b| b != b'/').map_or(0, |i| i + 1);
    if end == 0 {
        return if path.is_empty() {
            b".".to_vec()
        } else {
            b"/".to_vec()
        };
    }
    let Some(slash) = path[..end].iter().rposition(|&b| b == b'/') else {
        return b".".to_vec();
    };
    let end = path[..slash]
        .iter()
        .rposition(|&b| b != b'/')
        .map_or(0, |i| i + 1);
    if end == 0 {
        b"/".to_vec()
    } else {
        path[..end].to_vec()
    }
}

/// Match tmux globs using the platform's POSIX text contract.
pub fn glob_match(pattern: &[u8], text: &[u8]) -> Result<bool, &'static str> {
    glob_match_case(pattern, text, false)
}

#[cfg(unix)]
fn glob_match_case(pattern: &[u8], text: &[u8], ignore: bool) -> Result<bool, &'static str> {
    let pattern = std::ffi::CString::new(pattern).map_err(|_| "glob pattern contains NUL")?;
    let text = std::ffi::CString::new(text).map_err(|_| "glob text contains NUL")?;
    locale::with(|| unsafe {
        libc::fnmatch(
            pattern.as_ptr(),
            text.as_ptr(),
            if ignore { libc::FNM_CASEFOLD } else { 0 },
        ) == 0
    })
    .ok_or("UTF-8 locale is unavailable")
}

#[cfg(windows)]
fn glob_match_case(pattern: &[u8], text: &[u8], ignore: bool) -> Result<bool, &'static str> {
    windows_glob::matches(pattern, text, ignore)
}

#[cfg(not(any(unix, windows)))]
fn glob_match_case(pattern: &[u8], text: &[u8], ignore: bool) -> Result<bool, &'static str> {
    let (pattern, text) = if ignore {
        (pattern.to_ascii_lowercase(), text.to_ascii_lowercase())
    } else {
        (pattern.to_vec(), text.to_vec())
    };
    let (pattern, text) = (pattern.as_slice(), text.as_slice());
    fn atom(pattern: &[u8], at: usize, byte: u8) -> Option<usize> {
        match *pattern.get(at)? {
            b'?' => Some(at + 1),
            b'\\' => (pattern.get(at + 1) == Some(&byte)).then_some(at + 2),
            b'[' => {
                let mut i = at + 1;
                let invert = pattern.get(i).is_some_and(|c| matches!(c, b'!' | b'^'));
                i += usize::from(invert);
                let first = i;
                let mut matched = false;
                while i < pattern.len() && (pattern[i] != b']' || i == first) {
                    let mut low = pattern[i];
                    if low == b'\\' && i + 1 < pattern.len() {
                        i += 1;
                        low = pattern[i];
                    }
                    i += 1;
                    if pattern.get(i) == Some(&b'-')
                        && pattern.get(i + 1).is_some_and(|c| *c != b']')
                    {
                        i += 1;
                        let mut high = pattern[i];
                        if high == b'\\' && i + 1 < pattern.len() {
                            i += 1;
                            high = pattern[i];
                        }
                        i += 1;
                        matched |= (low..=high).contains(&byte);
                    } else {
                        matched |= low == byte;
                    }
                }
                if i == pattern.len() {
                    return (byte == b'[').then_some(at + 1);
                }
                (matched != invert).then_some(i + 1)
            }
            literal => (literal == byte).then_some(at + 1),
        }
    }
    let (mut p, mut t, mut star, mut retry) = (0, 0, None, 0);
    while t < text.len() {
        if pattern.get(p) == Some(&b'*') {
            p += 1;
            star = Some(p);
            retry = t;
        } else if let Some(next) = atom(pattern, p, text[t]) {
            p = next;
            t += 1;
        } else if let Some(s) = star {
            retry += 1;
            t = retry;
            p = s;
        } else {
            return Ok(false);
        }
    }
    while pattern.get(p) == Some(&b'*') {
        p += 1;
    }
    Ok(p == pattern.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn unicode_posix_matching_preserves_embedding_thread_locale() {
        let before = unsafe { libc::uselocale(std::ptr::null_mut()) };
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    let before = unsafe { libc::uselocale(std::ptr::null_mut()) };
                    assert!(glob_match(b"?", "界".as_bytes()).unwrap());
                    assert!(glob_match_case("é".as_bytes(), "É".as_bytes(), true).unwrap());
                    assert!(content_match("é".as_bytes(), "xÉx".as_bytes(), true, true).unwrap());
                    assert_eq!(unsafe { libc::uselocale(std::ptr::null_mut()) }, before);
                });
            }
        });
        assert_eq!(unsafe { libc::uselocale(std::ptr::null_mut()) }, before);
    }

    #[test]
    #[cfg(windows)]
    fn unicode_posix_matching_preserves_embedding_thread_locale() {
        unsafe extern "C" {
            fn _configthreadlocale(mode: i32) -> i32;
        }
        fn locale() -> (std::ffi::CString, i32) {
            let name = unsafe { libc::setlocale(libc::LC_ALL, std::ptr::null()) };
            assert!(!name.is_null());
            (
                unsafe { std::ffi::CStr::from_ptr(name) }.to_owned(),
                unsafe { _configthreadlocale(0) },
            )
        }
        let before = locale();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    let before = locale();
                    assert!(glob_match(b"?", "界".as_bytes()).unwrap());
                    assert!(glob_match_case("é".as_bytes(), "É".as_bytes(), true).unwrap());
                    assert!(content_match("é".as_bytes(), "xÉx".as_bytes(), true, true).unwrap());
                    assert!(!content_match(b"[", b"text", true, false).unwrap());
                    assert_eq!(locale(), before);
                });
            }
        });
        assert_eq!(locale(), before);
    }

    fn lookup(key: &[u8]) -> Option<Vec<u8>> {
        Some(match key {
            b"session_name" => b"work".to_vec(),
            b"pane_id" => b"%7".to_vec(),
            b"@path" => b"/tmp/work/".to_vec(),
            b"@quoted" => b"a'b $c".to_vec(),
            b"@nested" => b"#{session_name}".to_vec(),
            _ => return None,
        })
    }

    #[test]
    fn fields_aliases_escaping_and_conditionals() {
        assert_eq!(
            expand(b"#S/#D #{missing} ## #, #}", lookup).unwrap(),
            b"work/%7  # , }"
        );
        assert_eq!(
            expand(b"#{?#{==:#{session_name},work},yes#,please,no}", lookup).unwrap(),
            b"yes,please"
        );
        assert_eq!(
            expand(b"#{?missing,no,session_name,yes,default}", lookup).unwrap(),
            b"yes"
        );
        assert_eq!(
            expand(b"#{?missing,no,default}", lookup).unwrap(),
            b"default"
        );
        assert_eq!(
            expand(b"#{l:#{session_name}}", lookup).unwrap(),
            b"#{session_name}"
        );
    }

    #[test]
    fn operators_and_pure_modifiers() {
        for (input, expected) in [
            (b"#{&&:1,1,0}".as_slice(), b"0".as_slice()),
            (b"#{||:0,0,yes}", b"1"),
            (b"#{!:0}", b"1"),
            (b"#{m:*ork,work}", b"1"),
            (b"#{m:a?c,ac}", b"0"),
            (b"#{b:@path}", b"work"),
            (b"#{d:@path}", b"/tmp"),
            (b"#{E:@nested}", b"work"),
            (b"#{q/s:@quoted}", b"'a'\\''b $c'"),
        ] {
            assert_eq!(expand(input, lookup).unwrap(), expected, "{input:?}");
        }
    }

    #[test]
    fn unsupported_features_fail_without_execution() {
        assert!(expand(b"#(echo bad)", lookup).is_err());
        assert_eq!(
            expand(b"#{?missing,#(not-evaluated),safe}", lookup).unwrap(),
            b"safe"
        );
        assert_eq!(expand(b"\xff#{pane_id}", lookup).unwrap(), b"\xff%7");
    }

    #[test]
    fn required_width_arithmetic_and_repeat_limits() {
        for (input, expected) in [
            ("#{n:#{l:中文}}", "6"),
            ("#{w:#{l:中文}}", "4"),
            ("#{=/1/x:#{l:中文}}", "x"),
            ("#{p-6:#{l:中文}}", "  中文"),
            ("#{e|/|f|3:1,3}", "0.333"),
            ("#{e|/|:5,0}", ""),
            ("#{e|-|:1e30,1e30}", ""),
            ("#{R:x,10001}", ""),
            ("#{n:#{R:abcdefgh,8192}}", "65536"),
            ("#{R:abcdefgh,8193}", ""),
            ("#{a:98}", "b"),
            ("#{a:200}", ""),
        ] {
            assert_eq!(
                expand(input.as_bytes(), lookup).unwrap(),
                expected.as_bytes(),
                "{input}"
            );
        }
    }

    #[test]
    fn preparation_jobs_and_boolean_short_circuit() {
        assert_eq!(
            expand_no_jobs(b"before#(echo (nested))after", lookup).unwrap(),
            b"beforeafter"
        );
        assert_eq!(expand(b"#{&&:0,#(unused)}", lookup).unwrap(), b"0");
        assert_eq!(expand(b"#{||:1,#(unused)}", lookup).unwrap(), b"1");
        assert_eq!(
            expand(b"#{q:value}", |_| Some(b"!~]=%".to_vec())).unwrap(),
            b"!~]\\=\\%"
        );
    }

    #[test]
    fn recursive_lookup_is_bounded() {
        assert!(expand(b"#{E:loop}", |_| Some(b"#{E:loop}".to_vec())).is_err());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn posix_regex_longest_match_captures_and_empty_matches() {
        for (input, expected) in [
            (r"#{m/r:^a+$,aba}", "0"),
            (r"#{m/ri:^AB,abC}", "1"),
            (r"#{m/r:[,abc}", "0"),
            (r"#{s/(a|ab)/<\1>/:value}", "<ab><a>"),
            (r"#{s/^a/X/:value}", "Xba"),
            (r"#{s/x*/Z/:value}", "aZbZaZ"),
            (r"#{s/[/X/:value}", "aba"),
        ] {
            assert_eq!(
                expand(input.as_bytes(), |_| Some(b"aba".to_vec())).unwrap(),
                expected.as_bytes(),
                "{input}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_globs_use_unicode_posix_classes_ranges_and_literal_escapes() {
        for (pattern, text, ignore, expected) in [
            ("?", "界", false, true),
            ("??", "😀", false, false),
            ("é", "É", true, true),
            ("[[:alpha:]]*", "界É", false, true),
            ("[[:digit:]]", "１", false, false),
            ("[[:upper:]]", "a", true, false),
            ("[A-z]", "_", true, true),
            ("[!é-ê]", "ê", false, false),
            ("[[.界.]]", "界", false, true),
            ("[[=e=]]", "é", false, true),
            (r"[a\-z]", "-", false, true),
            (r"[a\-z]", "b", false, false),
            (r"[\]]", "]", false, true),
            (r"[\^]", "^", false, true),
            (r"[\界]", "界", false, true),
            (r"[a-\z]", "c", false, true),
            (r"a\*b", "a*b", false, true),
            ("[broken", "[broken", false, true),
            ("x\\", "x\\", false, false),
            ("*", "a\nb", false, true),
            ("a?", "ab\n", false, false),
        ] {
            assert_eq!(
                glob_match_case(pattern.as_bytes(), text.as_bytes(), ignore).unwrap(),
                expected,
                "{pattern:?} on {text:?}"
            );
        }
        assert!(glob_match(b"\xff", b"\xff").unwrap());
        assert!(!glob_match(b"\xff", "�".as_bytes()).unwrap());
        assert!(glob_match(b"*", b"nul\0byte").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_regex_substitution_preserves_captures_unicode_and_byte_progression() {
        for (pattern, replacement, text, expected) in [
            ("(界|界面)(😀?)", r"<\1/\2>", "界面😀", "<界面/😀>"),
            ("(a*)(a*)", r"[\1][\2]", "aa", "[aa][2]"),
            ("(a)?(b)", r"\1-\2", "b", "1-b"),
            ("()", "X", "é", "\u{c3}X\u{a9}X"),
        ] {
            let actual = regex::substitute(
                pattern.as_bytes(),
                replacement.as_bytes(),
                text.as_bytes(),
                false,
            )
            .unwrap();
            let expected = if pattern == "()" {
                vec![0xc3, b'X', 0xa9, b'X']
            } else {
                expected.as_bytes().to_vec()
            };
            assert_eq!(actual, expected, "{pattern:?} on {text:?}");
        }
        assert!(content_match("(é)\\1".as_bytes(), "Éé".as_bytes(), true, true).unwrap());
        assert!(
            content_match(
                "界?".as_bytes(),
                "prefix界面suffix".as_bytes(),
                false,
                false
            )
            .unwrap()
        );
        assert!(!content_match(b"[", b"content", true, false).unwrap());
        assert_eq!(
            regex::substitute(b"\xff", b"X", b"\xff\xfe", false).unwrap(),
            b"X\xfe"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_pattern_and_output_limits_propagate_as_errors() {
        assert!(
            content_match(&b"a".repeat(65537), b"a", true, false)
                .unwrap_err()
                .message
                .contains("complexity limit")
        );
        assert!(
            glob_match(&b"?".repeat(65537), b"a")
                .unwrap_err()
                .contains("complexity limit")
        );
        assert!(
            content_match(b"(a|aa)*b", &b"a".repeat(48), true, false)
                .unwrap_err()
                .message
                .contains("work limit")
        );
        assert!(
            regex::substitute(b"a", &b"x".repeat(1024 * 1024), b"aa", false)
                .unwrap_err()
                .contains("output exceeds limit")
        );
        assert!(
            regex::substitute(b".", b"x", &b"a".repeat(500_000), false)
                .unwrap_err()
                .contains("work limit")
        );
        assert!(
            content_match(b"x", &vec![b'x'; 16 * 1024 * 1024 + 1], true, false)
                .unwrap_err()
                .message
                .contains("input exceeds")
        );
    }
}
