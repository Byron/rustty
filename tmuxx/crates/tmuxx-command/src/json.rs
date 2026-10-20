//! tmux command JSON: objects, object arrays, strings, booleans and signed integers.
//! Preserve escape spelling and sort raw keys; this is narrower than general JSON.
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
struct Token {
    kind: u8,
    start: usize,
    end: usize,
}
struct Parser<'a> {
    input: &'a [u8],
    tokens: Vec<Token>,
    index: usize,
}
fn diagnostic(input: &[u8], offset: usize, reason: &str) -> String {
    let tail = &input[offset..];
    if tail.is_empty() {
        return reason.into();
    }
    format!(
        "{reason}: {}{}",
        String::from_utf8_lossy(&tail[..tail.len().min(8)]),
        if tail.len() > 8 { "..." } else { "" }
    )
}
/// Validate and print the exact structured command language accepted by tmux.
pub fn canonicalize(input: &[u8]) -> Result<Vec<u8>, String> {
    if input.is_empty() {
        return Err("empty input".into());
    }
    let mut tokens: Vec<Token> = Vec::new();
    let mut i = 0;
    let mut quoted = false;
    let mut last = 0;
    while i < input.len() {
        let start = i;
        last = start;
        let kind = if quoted && input[i] != b'"' {
            b'v'
        } else {
            match input[i] {
                b' ' | b'\t' | b'\r' | b'\n' => {
                    i += 1;
                    continue;
                }
                b'{' | b'}' | b'[' | b']' | b'"' | b':' | b',' => input[i],
                _ => b'v',
            }
        };
        if kind == b'v' {
            match tokens.last().map(|t| t.kind) {
                Some(b'"') => {
                    while input.get(i).is_some_and(|b| *b != b'"') {
                        if input[i] < 0x20 {
                            return Err(diagnostic(input, start, "tokenization error"));
                        }
                        if input[i] == b'\\' {
                            i += 1;
                            match input.get(i) {
                                Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {}
                                Some(b'u')
                                    if input.get(i + 1..i + 5).is_some_and(|bytes| {
                                        bytes.iter().all(u8::is_ascii_hexdigit)
                                    }) =>
                                {
                                    i += 4
                                }
                                _ => return Err(diagnostic(input, start, "tokenization error")),
                            }
                        }
                        i += 1;
                    }
                    if i == input.len() {
                        return Err(diagnostic(input, start, "tokenization error"));
                    }
                }
                Some(b':') => {
                    while input.get(i).is_some_and(|b| {
                        !matches!(b, b']' | b'}' | b',') && !b.is_ascii_whitespace()
                    }) {
                        i += 1;
                    }
                    if i == input.len() {
                        return Err(diagnostic(input, start, "tokenization error"));
                    }
                }
                _ => return Err(diagnostic(input, start, "tokenization error")),
            }
        } else {
            i += 1;
        }
        tokens.push(Token {
            kind,
            start,
            end: i,
        });
        if kind == b'"' {
            quoted = !quoted;
        }
    }
    tokens.push(Token {
        kind: 0,
        start: last,
        end: last,
    });
    let mut parser = Parser {
        input,
        tokens,
        index: 0,
    };
    if parser.kind() != b'{' {
        return Err(parser.error("expected object"));
    }
    let output = parser.object(1)?;
    if parser.kind() != 0 {
        return Err(parser.error("unexpected trailing data"));
    }
    Ok(output)
}
impl Parser<'_> {
    fn kind(&self) -> u8 {
        self.tokens[self.index].kind
    }
    fn error(&self, reason: &str) -> String {
        diagnostic(self.input, self.tokens[self.index].start, reason)
    }
    fn string(&mut self, reason: &str) -> Result<Vec<u8>, String> {
        let start = self.tokens[self.index].start;
        if self.kind() != b'"' {
            return Err(self.error(reason));
        }
        self.index += 1;
        if self.kind() != b'v' {
            return Err(diagnostic(self.input, start, reason));
        }
        let token = self.tokens[self.index];
        let bytes = self.input[token.start..token.end].to_vec();
        self.index += 1;
        if self.kind() != b'"' {
            return Err(diagnostic(self.input, start, reason));
        }
        self.index += 1;
        Ok(bytes)
    }
    fn object(&mut self, depth: usize) -> Result<Vec<u8>, String> {
        if depth > 200 {
            return Err(self.error("parse depth exceeded"));
        }
        self.index += 1;
        let mut fields = BTreeMap::new();
        while self.kind() != b'}' {
            let key = self.string("invalid key")?;
            if fields.contains_key(&key) {
                return Err(self.error("duplicate key"));
            }
            if self.kind() != b':' {
                return Err(self.error("missing colon"));
            }
            self.index += 1;
            let value = match self.kind() {
                b'{' => self.object(depth + 1)?,
                b'[' => self.array(depth)?,
                b'"' => [b"\"", self.string("invalid string")?.as_slice(), b"\""].concat(),
                b'v' => {
                    let token = self.tokens[self.index];
                    let value = &self.input[token.start..token.end];
                    let result = if value.first().is_some_and(u8::is_ascii_digit)
                        || value.first() == Some(&b'-')
                            && value.get(1).is_some_and(u8::is_ascii_digit)
                    {
                        if value.starts_with(b"0") && value.len() != 1
                            || value.starts_with(b"-0") && value.len() != 2
                        {
                            return Err(self.error("invalid number"));
                        }
                        std::str::from_utf8(value)
                            .ok()
                            .and_then(|value| value.parse::<i64>().ok())
                            .ok_or_else(|| self.error("invalid number"))?
                            .to_string()
                            .into_bytes()
                    } else if matches!(value, b"true" | b"false") {
                        value.to_vec()
                    } else {
                        return Err(self.error("invalid boolean"));
                    };
                    self.index += 1;
                    result
                }
                _ => return Err(self.error("unexpected value when parsing object")),
            };
            fields.insert(key, value);
            if self.kind() == b',' && self.tokens[self.index + 1].kind != b'}' {
                self.index += 1;
            } else if self.kind() != b'}' {
                return Err(self.error("invalid object"));
            }
        }
        self.index += 1;
        let fields: Vec<_> = fields
            .into_iter()
            .map(|(key, value)| [b"\"", key.as_slice(), b"\":", value.as_slice()].concat())
            .collect();
        Ok([b"{", fields.join(&b',').as_slice(), b"}"].concat())
    }
    fn array(&mut self, depth: usize) -> Result<Vec<u8>, String> {
        self.index += 1;
        let mut values = Vec::new();
        while self.kind() != b']' {
            if self.kind() != b'{' {
                return Err(self.error("invalid array member"));
            }
            values.push(self.object(depth + 1)?);
            if self.kind() == b',' && self.tokens[self.index + 1].kind != b']' {
                self.index += 1;
            } else if self.kind() != b']' {
                return Err(self.error("invalid array"));
            }
        }
        self.index += 1;
        Ok([b"[", values.join(&b',').as_slice(), b"]"].concat())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_objects_preserve_escapes_and_reject_ambiguous_values() {
        assert_eq!(
            canonicalize(br#" {"z":[{}, {"x":false}],"a":"\u0041","n":-0} "#).unwrap(),
            br#"{"a":"\u0041","n":0,"z":[{},{"x":false}]}"#
        );
        for input in [
            br#"{"x":null}"#.as_slice(),
            br#"{"x":[1]}"#,
            br#"{"x":9223372036854775808}"#,
            br#"{"x":1,"x":2}"#,
            br#"{"x":""}"#,
            br#"{"x":"bad\q"}"#,
        ] {
            assert!(canonicalize(input).is_err(), "{input:?}");
        }
        let mut input = b"{}".to_vec();
        for _ in 1..200 {
            input = [b"{\"x\":", input.as_slice(), b"}"].concat();
        }
        assert_eq!(canonicalize(&input).unwrap(), input);
        assert!(
            canonicalize(&[b"{\"x\":", input.as_slice(), b"}"].concat())
                .unwrap_err()
                .starts_with("parse depth exceeded")
        );
    }
}
