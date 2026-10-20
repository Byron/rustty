//! Native Windows POSIX ERE and substitution over byte-preserving Unicode text.
use tmuxx_posix_regex::{PosixRegex, PosixRegexBuilder, compile, matcher::MatchError};

pub(super) fn compile(
    pattern: &[u8],
    ignore: bool,
) -> Result<Option<PosixRegex<'static>>, &'static str> {
    if pattern.contains(&0) {
        return Ok(None);
    }
    match PosixRegexBuilder::new(pattern)
        .with_default_classes()
        .extended(true)
        .compile()
    {
        Ok(regex) => Ok(Some(regex.case_insensitive(ignore))),
        Err(compile::Error::PatternLimit) => Err("POSIX regex pattern exceeds complexity limit"),
        Err(_) => Ok(None),
    }
}

pub(super) fn match_error(error: MatchError) -> &'static str {
    match error {
        MatchError::WorkLimit => "POSIX regex work limit exceeded",
        MatchError::InputLimit => "POSIX regex input exceeds 16 MiB",
    }
}

pub(super) fn matches(pattern: &[u8], text: &[u8], ignore: bool) -> Result<bool, &'static str> {
    if text.contains(&0) {
        return Ok(false);
    }
    let Some(regex) = compile(pattern, ignore)? else {
        return Ok(false);
    };
    Ok(!regex
        .matches(text, Some(1))
        .map_err(match_error)?
        .is_empty())
}

pub(super) fn substitute(
    pattern: &[u8],
    replacement: &[u8],
    text: &[u8],
    ignore: bool,
) -> Result<Vec<u8>, &'static str> {
    if pattern.is_empty() || text.is_empty() || text.contains(&0) {
        return Ok(text.to_vec());
    }
    let Some(regex) = compile(pattern, ignore)? else {
        return Ok(text.to_vec());
    };
    let mut out = Vec::new();
    let (mut start, mut last, mut empty) = (0, 0, false);
    let mut budget = 2_000_000;
    while start <= text.len() {
        let captures = regex
            .matches_with_budget(&text[start..], Some(1), &mut budget)
            .map_err(match_error)?;
        let Some(captures) = captures.first() else {
            out.extend_from_slice(&text[start..]);
            break;
        };
        let (begin, end) = captures[0].expect("whole match");
        let (begin, end) = (start + begin, start + end);
        out.extend_from_slice(&text[last..begin]);
        let anchored = pattern[0] == b'^';
        if anchored || empty || begin != last || begin != end {
            let mut cursor = 0;
            while cursor < replacement.len() {
                let mut byte = replacement[cursor];
                cursor += 1;
                if byte == b'\\' && cursor < replacement.len() {
                    byte = replacement[cursor];
                    cursor += 1;
                    if byte.is_ascii_digit()
                        && let Some(Some((from, to))) = captures.get((byte - b'0') as usize)
                        && from != to
                    {
                        out.extend_from_slice(&text[start + from..start + to]);
                        continue;
                    }
                }
                out.push(byte);
            }
            if anchored {
                out.extend_from_slice(&text[end..]);
                break;
            }
            last = end;
            start = end;
            empty = false;
        } else {
            last = end;
            // C tmux advances one byte after suppressing an empty match. Byte
            // identity is retained even inside a UTF-8 sequence.
            start = end + 1;
            empty = true;
        }
        if out.len() > 1024 * 1024 {
            return Err("substitution output exceeds limit");
        }
    }
    if out.len() > 1024 * 1024 {
        return Err("substitution output exceeds limit");
    }
    Ok(out)
}
