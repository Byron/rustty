//! POSIX glob grammar translated to the native Windows Unicode ERE engine.

fn scalar_len(input: &[u8]) -> usize {
    let maximum = input.len().min(4);
    for len in 1..=maximum {
        if std::str::from_utf8(&input[..len]).is_ok() {
            return len;
        }
    }
    1
}

fn literal(out: &mut Vec<u8>, byte: u8) {
    if b".[]\\*+?{}()^$|".contains(&byte) {
        out.push(b'\\');
    }
    out.push(byte);
}

fn bracket(pattern: &[u8]) -> Option<(Vec<u8>, usize)> {
    let mut out = vec![b'['];
    let mut cursor = 1;
    if pattern.get(cursor).is_some_and(|byte| b"!^".contains(byte)) {
        out.push(b'^');
        cursor += 1;
    }
    let mut first = true;
    while let Some(&byte) = pattern.get(cursor) {
        if byte == b']' && !first {
            out.push(byte);
            return Some((out, cursor + 1));
        }
        first = false;
        if byte == b'\\' {
            cursor += 1;
            let rest = pattern.get(cursor..).filter(|rest| !rest.is_empty())?;
            let length = scalar_len(rest);
            out.extend_from_slice(b"[.");
            out.extend_from_slice(&rest[..length]);
            out.extend_from_slice(b".]");
            cursor += length;
        } else if byte == b'['
            && pattern
                .get(cursor + 1)
                .is_some_and(|byte| b".:=".contains(byte))
        {
            let marker = pattern[cursor + 1];
            let end = pattern[cursor + 2..]
                .windows(2)
                .position(|pair| pair == [marker, b']'])?
                + cursor
                + 4;
            out.extend_from_slice(&pattern[cursor..end]);
            cursor = end;
        } else {
            out.push(byte);
            cursor += 1;
        }
    }
    None
}

pub(super) fn matches(pattern: &[u8], text: &[u8], ignore: bool) -> Result<bool, &'static str> {
    if pattern.contains(&0) {
        return Err("glob pattern contains NUL");
    }
    if text.contains(&0) {
        return Err("glob text contains NUL");
    }
    if pattern.len() > 65536 {
        return Err("glob pattern exceeds complexity limit");
    }
    let mut translated = vec![b'^'];
    let mut cursor = 0;
    while let Some(&byte) = pattern.get(cursor) {
        cursor += 1;
        match byte {
            b'*' => {
                translated.extend_from_slice(b".*");
                while pattern.get(cursor) == Some(&b'*') {
                    cursor += 1;
                }
            }
            b'?' => translated.push(b'.'),
            b'[' => {
                if let Some((class, consumed)) = bracket(&pattern[cursor - 1..]) {
                    translated.extend(class);
                    cursor += consumed - 1;
                } else {
                    literal(&mut translated, byte);
                }
            }
            b'\\' => {
                let Some(&byte) = pattern.get(cursor) else {
                    return Ok(false);
                };
                literal(&mut translated, byte);
                cursor += 1;
            }
            byte => literal(&mut translated, byte),
        }
    }
    translated.push(b'$');
    let Some(regex) = super::regex::compile(&translated, ignore)? else {
        return Ok(false);
    };
    Ok(regex
        .fold_classes(false)
        .matches_exact(text)
        .map_err(super::regex::match_error)?
        .is_some())
}
