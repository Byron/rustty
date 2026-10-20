//! Byte-oriented tmux command syntax, without command execution or process IO.

pub mod json;

mod arguments;
pub use arguments::Args;
pub mod registry;

use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub args: Vec<Vec<u8>>,
    /// Parsed brace arguments, indexed in `args`, remain distinct from strings.
    pub blocks: BTreeMap<usize, Vec<Command>>,
}
impl Command {
    pub fn new(args: Vec<Vec<u8>>) -> Self {
        Self {
            args,
            blocks: BTreeMap::new(),
        }
    }
    pub fn argument_block(&self, args: &Args, index: usize) -> Option<&[Command]> {
        self.blocks
            .get(&(self.args.len() - args.rest.len() + index))
            .map(Vec::as_slice)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub offset: usize,
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl Error {
    pub fn at(input: &[u8], offset: usize, message: impl Into<String>) -> Self {
        let prefix = &input[..offset.min(input.len())];
        Self {
            offset,
            line: 1 + prefix.iter().filter(|&&b| b == b'\n').count(),
            column: 1 + prefix.iter().rev().take_while(|&&b| b != b'\n').count(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.column, self.message)
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Word(Vec<u8>),
    Separator,
    Newline,
    OpenBrace,
    CloseBrace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub offset: usize,
}

/// A reader accepts its environment lookup on each call so preparation may
/// update assignments between tokens without reading the process environment.
pub struct Reader<'a> {
    input: &'a [u8],
    offset: usize,
    home: fn(&[u8]) -> Option<Vec<u8>>,
}

impl<'a> Reader<'a> {
    pub fn new(input: &'a [u8]) -> Self {
        Self::with_home(input, |_| None)
    }

    /// Resolve an account home without giving the parser OS dependencies.
    /// An empty username requests the current account when HOME is absent/empty.
    pub fn with_home(input: &'a [u8], home: fn(&[u8]) -> Option<Vec<u8>>) -> Self {
        Self {
            input,
            offset: 0,
            home,
        }
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn next(
        &mut self,
        lookup: &mut impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> Result<Option<Token>, Error> {
        self.next_inner(lookup, false)
    }

    /// Unquoted `#{...}` is a format only in a conditional, otherwise `#`
    /// at the start of a token introduces a comment.
    pub fn next_condition(
        &mut self,
        lookup: &mut impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> Result<Option<Token>, Error> {
        self.next_inner(lookup, true)
    }

    fn next_inner(
        &mut self,
        lookup: &mut impl FnMut(&[u8]) -> Option<Vec<u8>>,
        condition: bool,
    ) -> Result<Option<Token>, Error> {
        loop {
            match self.input.get(self.offset) {
                Some(b' ' | b'\t') => self.offset += 1,
                Some(b'\\') if self.input.get(self.offset + 1) == Some(&b'\n') => {
                    self.offset += 2;
                }
                Some(b'\r') if self.input.get(self.offset + 1) == Some(&b'\n') => {
                    self.offset += 1;
                }
                Some(b'#') if !(condition && self.input.get(self.offset + 1) == Some(&b'{')) => {
                    self.comment();
                }
                _ => break,
            }
        }
        let Some(&b) = self.input.get(self.offset) else {
            return Ok(None);
        };
        let offset = self.offset;
        let kind = match b {
            b';' => TokenKind::Separator,
            b'\n' => TokenKind::Newline,
            b'{' => TokenKind::OpenBrace,
            b'}' => TokenKind::CloseBrace,
            _ => {
                let value = if condition && self.input[self.offset..].starts_with(b"#{") {
                    self.format_token()?
                } else {
                    self.word(lookup)?
                };
                return Ok(Some(Token {
                    kind: TokenKind::Word(value),
                    offset,
                }));
            }
        };
        self.offset += 1;
        Ok(Some(Token { kind, offset }))
    }

    fn comment(&mut self) {
        let mut backslashes = 0;
        while let Some(&b) = self.input.get(self.offset) {
            if b == b'\n' && backslashes % 2 == 0 {
                break;
            }
            backslashes = if b == b'\\' { backslashes + 1 } else { 0 };
            self.offset += 1;
        }
    }

    fn format_token(&mut self) -> Result<Vec<u8>, Error> {
        let start = self.offset;
        self.offset += 2;
        let mut depth = 1;
        while let Some(&b) = self.input.get(self.offset) {
            if b == b'\n' || b == 0 {
                return Err(Error::at(
                    self.input,
                    self.offset,
                    "unterminated conditional format",
                ));
            }
            if self.input[self.offset..].starts_with(b"#{") {
                depth += 1;
                self.offset += 2;
            } else if b == b'#' && self.input.get(self.offset + 1).is_some() {
                self.offset += 2;
            } else {
                self.offset += 1;
                if b == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(self.input[start..self.offset].to_vec());
                    }
                }
            }
        }
        Err(Error::at(
            self.input,
            start,
            "unterminated conditional format",
        ))
    }

    fn word(
        &mut self,
        lookup: &mut impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> Result<Vec<u8>, Error> {
        let start = self.offset;
        let mut quote = None;
        let mut value = Vec::new();
        let mut segment_start = true;
        while let Some(&b) = self.input.get(self.offset) {
            if b == 0 {
                return Err(Error::at(self.input, self.offset, "NUL in command"));
            }
            if quote.is_none() && matches!(b, b' ' | b'\t' | b'\n' | b';' | b'}') {
                break;
            }
            if quote.is_none() && b == b'\r' && self.input.get(self.offset + 1) == Some(&b'\n') {
                self.offset += 1;
                break;
            }
            if matches!(b, b'\'' | b'"') && (quote.is_none() || quote == Some(b)) {
                quote = if quote.is_none() { Some(b) } else { None };
                segment_start = true;
                self.offset += 1;
                continue;
            }
            if quote == Some(b'\'') && b == b'\\' {
                let count = self.input[self.offset..]
                    .iter()
                    .take_while(|&&b| b == b'\\')
                    .count();
                if count % 2 == 1 && self.input.get(self.offset + count) == Some(&b'\n') {
                    value.extend(std::iter::repeat_n(b'\\', count - 1));
                    self.offset += count + 1;
                    continue;
                }
            }
            if quote != Some(b'\'') && b == b'\\' {
                self.escape(&mut value)?;
            } else if quote != Some(b'\'') && b == b'$' {
                self.variable(&mut value, lookup)?;
            } else if quote != Some(b'\'') && segment_start && b == b'~' {
                self.offset += 1;
                let name_start = self.offset;
                while self
                    .input
                    .get(self.offset)
                    .is_some_and(|b| !b"/ \t\n\"'".contains(b))
                {
                    if self.offset - name_start == 1022 {
                        return Err(Error::at(
                            self.input,
                            name_start - 1,
                            "user name is too long",
                        ));
                    }
                    self.offset += 1;
                }
                let name = &self.input[name_start..self.offset];
                let home = if name.is_empty() {
                    lookup(b"HOME").filter(|value| !value.is_empty())
                } else {
                    None
                }
                .or_else(|| (self.home)(name))
                .ok_or_else(|| Error::at(self.input, name_start - 1, "syntax error"))?;
                value.extend(home);
            } else {
                value.push(b);
                self.offset += 1;
                if b == b'\n' && quote.is_some() {
                    while self
                        .input
                        .get(self.offset)
                        .is_some_and(|b| matches!(b, b' ' | b'\t'))
                    {
                        self.offset += 1;
                    }
                    if self.input.get(self.offset) == Some(&b'#')
                        && self
                            .input
                            .get(self.offset + 1)
                            .is_none_or(|b| !b",#{}:".contains(b))
                    {
                        self.comment();
                    }
                }
            }
            segment_start = false;
        }
        if quote.is_some() {
            return Err(Error::at(self.input, start, "unterminated quote"));
        }
        if value.contains(&0) {
            return Err(Error::at(self.input, start, "NUL in argument"));
        }
        Ok(value)
    }

    fn escape(&mut self, value: &mut Vec<u8>) -> Result<(), Error> {
        let start = self.offset;
        self.offset += 1;
        let Some(&b) = self.input.get(self.offset) else {
            return Err(Error::at(self.input, start, "unfinished escape"));
        };
        self.offset += 1;
        if b == b'\n' {
            return Ok(());
        }
        if (b'0'..=b'7').contains(&b) {
            let tail = self.input.get(self.offset..self.offset + 2);
            if b > b'3' || !tail.is_some_and(|s| s.iter().all(|b| (b'0'..=b'7').contains(b))) {
                return Err(Error::at(self.input, start, "invalid octal escape"));
            }
            let tail = tail.unwrap();
            value.push((b - b'0') * 64 + (tail[0] - b'0') * 8 + (tail[1] - b'0'));
            self.offset += 2;
            return Ok(());
        }
        if b == b'u' || b == b'U' {
            let count = if b == b'u' { 4 } else { 8 };
            let raw = self
                .input
                .get(self.offset..self.offset + count)
                .ok_or_else(|| Error::at(self.input, start, "incomplete Unicode escape"))?;
            let scalar = raw
                .iter()
                .try_fold(0u32, |n, &b| {
                    (b as char)
                        .to_digit(16)
                        .and_then(|d| n.checked_mul(16)?.checked_add(d))
                })
                .and_then(char::from_u32)
                .ok_or_else(|| Error::at(self.input, start, "invalid Unicode escape"))?;
            value.extend_from_slice(scalar.encode_utf8(&mut [0; 4]).as_bytes());
            self.offset += count;
            return Ok(());
        }
        value.push(match b {
            b'a' => 7,
            b'b' => 8,
            b'e' => 27,
            b'f' => 12,
            b's' => b' ',
            b'v' => 11,
            b'r' => b'\r',
            b'n' => b'\n',
            b't' => b'\t',
            _ => b,
        });
        Ok(())
    }

    fn variable(
        &mut self,
        value: &mut Vec<u8>,
        lookup: &mut impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> Result<(), Error> {
        let dollar = self.offset;
        self.offset += 1;
        let braced = self.input.get(self.offset) == Some(&b'{');
        if braced {
            self.offset += 1;
        }
        let start = self.offset;
        if !braced && !self.input.get(start).is_some_and(|&b| variable_start(b)) {
            value.push(b'$');
            return Ok(());
        }
        while self
            .input
            .get(self.offset)
            .is_some_and(|&b| variable_continue(b))
        {
            self.offset += 1;
        }
        let name = &self.input[start..self.offset];
        if braced {
            if name.is_empty() || self.input.get(self.offset) != Some(&b'}') {
                return Err(Error::at(
                    self.input,
                    dollar,
                    "invalid environment variable",
                ));
            }
            self.offset += 1;
        }
        if let Some(bytes) = lookup(name) {
            value.extend(bytes);
        }
        Ok(())
    }
}

pub fn variable_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}
pub fn variable_continue(b: u8) -> bool {
    variable_start(b) || b.is_ascii_digit()
}

pub fn parse(input: &[u8]) -> Result<Vec<Command>, Error> {
    parse_with(input, |_| None)
}

pub fn parse_with(
    input: &[u8],
    lookup: impl FnMut(&[u8]) -> Option<Vec<u8>>,
) -> Result<Vec<Command>, Error> {
    parse_with_home(input, lookup, |_| None)
}

pub fn parse_with_home(
    input: &[u8],
    mut lookup: impl FnMut(&[u8]) -> Option<Vec<u8>>,
    home: fn(&[u8]) -> Option<Vec<u8>>,
) -> Result<Vec<Command>, Error> {
    parse_commands(&mut Reader::with_home(input, home), &mut lookup, false, 0)
}

fn parse_commands(
    reader: &mut Reader<'_>,
    lookup: &mut impl FnMut(&[u8]) -> Option<Vec<u8>>,
    nested: bool,
    depth: usize,
) -> Result<Vec<Command>, Error> {
    if depth > 100 {
        return Err(Error::at(
            reader.input,
            reader.offset,
            "command nesting limit exceeded",
        ));
    }
    let mut commands = Vec::new();
    let mut args = Vec::new();
    let mut blocks = BTreeMap::new();
    while let Some(token) = reader.next(lookup)? {
        match token.kind {
            TokenKind::Word(value) => {
                if args.is_empty()
                    && value.starts_with(b"%")
                    && value.get(1).is_some_and(u8::is_ascii_alphabetic)
                {
                    return Err(Error::at(
                        reader.input,
                        token.offset,
                        "directives require configuration preparation",
                    ));
                }
                args.push(value);
            }
            TokenKind::OpenBrace => {
                if args.is_empty() {
                    return Err(Error::at(
                        reader.input,
                        token.offset,
                        "command group requires a command argument",
                    ));
                }
                let block = parse_commands(reader, lookup, true, depth + 1)?;
                args.push(serialize(&block));
                blocks.insert(args.len() - 1, block);
            }
            TokenKind::CloseBrace if nested => {
                if !args.is_empty() {
                    commands.push(Command { args, blocks });
                }
                return Ok(commands);
            }
            TokenKind::CloseBrace => {
                return Err(Error::at(
                    reader.input,
                    token.offset,
                    "unexpected closing brace",
                ));
            }
            TokenKind::Separator | TokenKind::Newline => {
                if !args.is_empty() {
                    commands.push(Command {
                        args: std::mem::take(&mut args),
                        blocks: std::mem::take(&mut blocks),
                    });
                }
            }
        }
    }
    if nested {
        return Err(Error::at(
            reader.input,
            reader.offset,
            "unterminated command group",
        ));
    }
    if !args.is_empty() {
        commands.push(Command { args, blocks });
    }
    Ok(commands)
}

/// Already-tokenized argv is not reparsed as shell text. Only a trailing `;`
/// separates commands; a preceding backslash quotes that final separator.
pub fn parse_argv<I, B>(argv: I) -> Result<Vec<Command>, Error>
where
    I: IntoIterator<Item = B>,
    B: AsRef<[u8]>,
{
    let mut commands = Vec::new();
    let mut args = Vec::new();
    for arg in argv {
        let mut arg = arg.as_ref().to_vec();
        if arg.contains(&0) {
            return Err(Error::at(&arg, 0, "NUL in argument"));
        }
        let mut separator = false;
        if arg.last() == Some(&b';') {
            arg.pop();
            if arg.last() == Some(&b'\\') {
                *arg.last_mut().unwrap() = b';';
            } else {
                separator = true;
            }
        }
        if !separator || !arg.is_empty() {
            args.push(arg);
        }
        if separator && !args.is_empty() {
            commands.push(Command {
                blocks: Default::default(),
                args: std::mem::take(&mut args),
            });
        }
    }
    if !args.is_empty() {
        commands.push(Command::new(args));
    }
    Ok(commands)
}

/// Quote one tmux argument, not one POSIX-shell or Windows-shell argument.
pub fn quote_argument(arg: &[u8]) -> Vec<u8> {
    let mut output = vec![b'"'];
    for &b in arg {
        match b {
            b'\\' | b'"' | b'$' | b'~' => {
                output.push(b'\\');
                output.push(b);
            }
            0..=31 | 127 => output.extend(format!("\\{b:03o}").bytes()),
            _ => output.push(b),
        }
    }
    output.push(b'"');
    output
}

/// The human-readable spelling used by tmux's `args_escape`.
pub fn display_argument(arg: &[u8]) -> Vec<u8> {
    if arg.is_empty() {
        return b"''".to_vec();
    }
    let quote = if arg.iter().any(|b| b" #';${}%".contains(b)) {
        Some(b'"')
    } else if arg.contains(&b'"') {
        Some(b'\'')
    } else {
        None
    };
    if arg.len() == 1 && arg[0] != b' ' && (quote.is_some() || arg[0] == b'~') {
        return vec![b'\\', arg[0]];
    }
    let mut out = Vec::new();
    if let Some(quote) = quote {
        out.push(quote);
    }
    if arg[0] == b'~' {
        out.push(b'\\');
    }
    for (i, &b) in arg.iter().enumerate() {
        match b {
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            8 => out.extend_from_slice(b"\\b"),
            7 => out.extend_from_slice(b"\\a"),
            11 => out.extend_from_slice(b"\\v"),
            12 => out.extend_from_slice(b"\\f"),
            0 if !arg.get(i + 1).is_some_and(|b| (b'0'..=b'7').contains(b)) => {
                out.extend_from_slice(b"\\0")
            }
            0..=31 | 127 => out.extend(format!("\\{b:03o}").bytes()),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'"' if quote == Some(b'"') => {
                out.push(b'\\');
                out.push(b);
            }
            b'$' if quote == Some(b'"')
                && arg
                    .get(i + 1)
                    .is_some_and(|b| b.is_ascii_alphabetic() || matches!(b, b'_' | b'{')) =>
            {
                out.push(b'\\');
                out.push(b);
            }
            _ => out.push(b),
        }
    }
    if let Some(quote) = quote {
        out.push(quote);
    }
    out
}

pub fn serialize(commands: &[Command]) -> Vec<u8> {
    let mut output = Vec::new();
    for (i, command) in commands.iter().enumerate() {
        if i != 0 {
            output.extend_from_slice(b" ; ");
        }
        for (j, arg) in command.args.iter().enumerate() {
            if j != 0 {
                output.push(b' ');
            }
            if let Some(block) = command.blocks.get(&j) {
                output.extend_from_slice(b"{ ");
                output.extend(serialize(block));
                output.extend_from_slice(b" }");
            } else {
                output.extend(quote_argument(arg));
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_octal_byte_decodes_without_overflow() {
        for byte in 0u8..=255 {
            for source in [
                format!("display \\{byte:03o}"),
                format!("display \"\\{byte:03o}\""),
            ] {
                let parsed = super::parse(source.as_bytes());
                if byte == 0 {
                    assert_eq!(parsed.unwrap_err().message, "NUL in argument");
                } else {
                    assert_eq!(parsed.unwrap()[0].args[1], [byte], "{source}");
                }
            }
        }
    }

    use super::*;

    #[test]
    fn quotes_comments_sequences_and_arbitrary_bytes() {
        let parsed =
            parse(b"display 'a b' \"c\\nd\" e\\;f; # ignored\nrename-window \xff").unwrap();
        assert_eq!(
            parsed[0].args,
            [
                b"display".to_vec(),
                b"a b".to_vec(),
                b"c\nd".to_vec(),
                b"e;f".to_vec()
            ]
        );
        assert_eq!(parsed[1].args[1], vec![255]);
        assert_eq!(parse(&serialize(&parsed)).unwrap(), parsed);
    }

    #[test]
    fn all_non_nul_bytes_survive_serialization() {
        let commands = vec![Command {
            blocks: Default::default(),
            args: vec![b"display".to_vec(), (1..=255).collect(), Vec::new()],
        }];
        assert_eq!(parse(&serialize(&commands)).unwrap(), commands);
    }

    #[test]
    fn environment_and_groups_are_prepared_once() {
        let commands = parse_with(b"if-shell true { display \"$V\"; display '~' }", |name| {
            (name == b"V").then(|| b"$OTHER; quoted".to_vec())
        })
        .unwrap();
        let nested = parse(&commands[0].args[2]).unwrap();
        assert_eq!(nested[0].args[1], b"$OTHER; quoted");
        assert_eq!(nested[1].args[1], b"~");
        assert_eq!(commands[0].blocks[&2], nested);
        assert_eq!(parse(&serialize(&commands)).unwrap(), commands);
        assert!(
            parse_argv([
                b"if-shell".as_slice(),
                b"true",
                commands[0].args[2].as_slice()
            ])
            .unwrap()[0]
                .blocks
                .is_empty()
        );
        let nested = parse(b"if-shell -F 1 { run-shell -C { display-message -p '#{command}' } }; display-message -p done").unwrap();
        assert!(nested[0].blocks[&3][0].blocks.contains_key(&2));
        assert!(nested[1].blocks.is_empty());
        assert_eq!(parse(&serialize(&nested)).unwrap(), nested);
    }

    #[test]
    fn tilde_uses_supplied_accounts_and_preserves_token_boundaries() {
        let home = |name: &[u8]| match name {
            b"" => Some(b"/fallback".to_vec()),
            b"alice" => Some(b"/home/alice $literal".to_vec()),
            _ => None,
        };
        let source = b"display ~ ~/bin ~alice/bin '~alice' middle~alice \"~alice\"";
        let command = parse_with_home(source, |name| (name == b"HOME").then(Vec::new), home)
            .unwrap()
            .remove(0);
        assert_eq!(
            command.args[1..],
            [
                b"/fallback".to_vec(),
                b"/fallback/bin".to_vec(),
                b"/home/alice $literal/bin".to_vec(),
                b"~alice".to_vec(),
                b"middle~alice".to_vec(),
                b"/home/alice $literal".to_vec()
            ]
        );
        assert_eq!(
            parse_with_home(b"display ~", |_| Some(b"/configured".to_vec()), home).unwrap()[0].args
                [1],
            b"/configured"
        );
        assert!(parse_with_home(b"display ~missing", |_| None, home).is_err());
        for length in [1022, 1023] {
            let source = format!("display ~{}", "a".repeat(length));
            let result =
                parse_with_home(source.as_bytes(), |_| None, |_| Some(b"/account".to_vec()));
            assert_eq!(result.is_ok(), length == 1022);
            if let Err(error) = result {
                assert_eq!(error.message, "user name is too long");
            }
        }
    }

    #[test]
    fn argv_preserves_shell_tokenization() {
        let commands = parse_argv([
            b"display".as_slice(),
            b"a b;",
            b"display",
            b"literal\\;",
            b"x;y",
        ])
        .unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].args[1], b"a b");
        assert_eq!(commands[1].args[1], b"literal;");
        assert_eq!(commands[1].args[2], b"x;y");
    }

    #[test]
    fn continuation_precedes_quotes_and_comments() {
        let commands =
            parse(b"display 'a\\\nb'\n# continued \\\ndisplay ignored\ndisplay kept").unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].args[1], b"ab");
        assert_eq!(commands[1].args[1], b"kept");
    }

    #[test]
    fn escapes_and_precise_errors() {
        assert_eq!(
            parse(b"display \\033 \\u754c \\U0001f600").unwrap()[0].args[1..],
            [vec![27], "界".as_bytes().to_vec(), "😀".as_bytes().to_vec()]
        );
        let error = parse(b"display ok\n  display \\777").unwrap_err();
        assert_eq!((error.line, error.column), (2, 11));
        for input in [
            b"display 'bad".as_slice(),
            b"display { bad",
            b"display }",
            b"display \\000",
        ] {
            assert!(parse(input).is_err(), "{input:?}");
        }
    }
}
