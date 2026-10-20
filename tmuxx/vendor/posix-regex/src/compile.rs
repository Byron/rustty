//! The regex "compiler", which parses the regex itself.
//! Produces a matcher ready to match input.

#[cfg(feature = "no_std")]
use alloc::collections::BTreeMap as HashMap;

#[cfg(not(feature = "no_std"))]
use std::collections::HashMap;

use alloc::{borrow::Cow, vec, vec::Vec};
use core::fmt;

use crate::{PosixRegex, ctype, tree::*};

/// Repetition bounds, for example + is (1, None), and ? is (0, Some(1))
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Range(pub u32, pub Option<u32>);
impl fmt::Debug for Range {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Range(start, None) => write!(f, "{}..", start),
            Range(start, Some(end)) => write!(f, "{}..{}", start, end),
        }
    }
}

/// An item inside square brackets, like `[abc]` or `[[:digit:]]`
#[derive(Clone, Eq)]
pub enum Collation {
    Char(u32),
    Range(u32, u32),
    Equivalent(u32),
    Class(fn(u32) -> bool),
}
impl PartialEq for Collation {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Char(a), Self::Char(b)) | (Self::Equivalent(a), Self::Equivalent(b)) => a == b,
            (Self::Range(a, b), Self::Range(c, d)) => a == c && b == d,
            (Self::Class(a), Self::Class(b)) => core::ptr::fn_addr_eq(*a, *b),
            _ => false,
        }
    }
}
impl Collation {
    /// Compare this collation to a character
    pub fn matches(&self, other: u32, insensitive: bool, fold_classes: bool) -> bool {
        match *self {
            Collation::Char(me) if insensitive => {
                crate::alphabet::lower(me) == crate::alphabet::lower(other)
            }
            Collation::Char(me) => me == other,
            Collation::Range(low, high) if insensitive => {
                let range = low..=high;
                range.contains(&other)
                    || range.contains(&crate::alphabet::lower(other))
                    || range.contains(&crate::alphabet::upper(other))
            }
            Collation::Range(low, high) => (low..=high).contains(&other),
            Collation::Equivalent(me) => crate::alphabet::equivalent(me, other),
            Collation::Class(f) => {
                f(other)
                    || (insensitive
                        && fold_classes
                        && (f(crate::alphabet::lower(other)) || f(crate::alphabet::upper(other))))
            }
        }
    }
}
impl fmt::Debug for Collation {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Collation::Char(c) => {
                if let Some(c) = char::from_u32(c) {
                    write!(f, "{c:?}")
                } else {
                    write!(f, "byte({c:#x})")
                }
            }
            Collation::Range(low, high) => {
                if high.saturating_sub(low) > 128 {
                    return write!(f, "{low}..={high}");
                }
                for value in low..=high {
                    if value != low {
                        f.write_str(", ")?;
                    }
                    if let Some(value) = char::from_u32(value) {
                        write!(f, "{value:?}")?;
                    } else {
                        write!(f, "byte({value:#x})")?;
                    }
                }
                Ok(())
            }
            Collation::Equivalent(c) => write!(f, "[={c}=]"),
            Collation::Class(c) => write!(f, "{:p}", c),
        }
    }
}

/// A single "compiled" token, such as a `.` or a character literal
#[derive(Clone, PartialEq, Eq)]
pub enum Token {
    /// Internal token used to find matches that might be anywhere in the text
    InternalStart,

    Alternative,
    Empty,
    Any,
    BackRef(u32),
    Char(u32),
    End,
    Group(usize),
    OneOf {
        invert: bool,
        list: Vec<Collation>,
    },
    Root,
    Start,
    WordEnd,
    WordStart,
}
impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Token::InternalStart => write!(f, "<START>"),

            Token::Alternative => write!(f, "Alternative"),
            Token::Empty => write!(f, "<EMPTY>"),
            Token::Any => write!(f, "."),
            Token::BackRef(id) => write!(f, "\\{}", id),
            Token::Char(c) => {
                if let Some(c) = char::from_u32(c) {
                    write!(f, "{c:?}")
                } else {
                    write!(f, "byte({c:#x})")
                }
            }
            Token::End => write!(f, "$"),
            Token::Group(id) => write!(f, "Group({})", id),
            Token::OneOf { invert, ref list } => write!(f, "{{invert: {}, {:?}}}", invert, list),
            Token::Root => write!(f, "Root"),
            Token::Start => write!(f, "^"),
            Token::WordEnd => write!(f, ">"),
            Token::WordStart => write!(f, "<"),
        }
    }
}
/// An error that occurred while compiling the regex
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    EOF,
    EmptyRepetition,
    Expected(u8, Option<u8>),
    IllegalRange,
    IntegerOverflow,
    InvalidBackRef(u32),
    LeadingRepetition,
    UnclosedRepetition,
    UnexpectedToken(u8),
    UnknownClass(Vec<u8>),
    UnknownCollation,
    PatternLimit,
    UnclosedGroup,
}

/// A regex builder struct
pub struct PosixRegexBuilder<'a> {
    input: &'a [u8],
    classes: HashMap<&'a [u8], fn(u32) -> bool>,
    group_id: usize,
    builder: TreeBuilder,
    extended: bool,
    depth: usize,
    closed_groups: Vec<usize>,
}
impl<'a> PosixRegexBuilder<'a> {
    /// Create a new instance that is ready to parse the regex `input`
    pub fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            classes: HashMap::new(),
            group_id: 1,
            builder: TreeBuilder::default(),
            extended: false,
            depth: 0,
            closed_groups: Vec::new(),
        }
    }
    /// Add a custom collation class, for use within square brackets (such as `[[:digit:]]`)
    pub fn with_class(mut self, name: &'a [u8], callback: fn(u32) -> bool) -> Self {
        self.classes.insert(name, callback);
        self
    }
    /// Add all the default collation classes, like `[[:digit:]]` and `[[:alnum:]]`
    pub fn with_default_classes(mut self) -> Self {
        #[cfg(not(feature = "no_std"))]
        self.classes.reserve(12);

        self.classes.insert(b"alnum", ctype::is_alnum);
        self.classes.insert(b"alpha", ctype::is_alpha);
        self.classes.insert(b"blank", ctype::is_blank);
        self.classes.insert(b"cntrl", ctype::is_cntrl);
        self.classes.insert(b"digit", ctype::is_digit);
        self.classes.insert(b"graph", ctype::is_graph);
        self.classes.insert(b"lower", ctype::is_lower);
        self.classes.insert(b"print", ctype::is_print);
        self.classes.insert(b"punct", ctype::is_punct);
        self.classes.insert(b"space", ctype::is_space);
        self.classes.insert(b"upper", ctype::is_upper);
        self.classes.insert(b"xdigit", ctype::is_xdigit);

        self
    }
    /// Use POSIX extended regex
    pub fn extended(mut self, extended: bool) -> Self {
        self.extended = extended;
        self
    }
    /// "Compile" this regex to a struct ready to match input
    pub fn compile(self) -> Result<PosixRegex<'static>, Error> {
        let tree = self.compile_tokens()?;
        Ok(PosixRegex::new(Cow::Owned(tree)))
    }
    pub fn compile_tokens(mut self) -> Result<Tree, Error> {
        if self.input.len() > 65536 || self.input.contains(&0) {
            return Err(Error::PatternLimit);
        }
        self.builder.start_internal(Token::Root, Range(1, Some(1)));
        self.parse(false)?;
        self.builder.finish_internal();
        Ok(self.builder.finish())
    }

    fn consume(&mut self, amount: usize) {
        self.input = &self.input[amount..];
    }
    fn scalar(&mut self, first: u8) -> u32 {
        if first.is_ascii() {
            return u32::from(first);
        }
        let mut bytes = [0; 4];
        bytes[0] = first;
        let length = self.input.len().min(3);
        bytes[1..1 + length].copy_from_slice(&self.input[..length]);
        let (symbol, used) = crate::alphabet::first(&bytes[..1 + length]);
        self.consume(used - 1);
        symbol
    }
    fn take_int(&mut self) -> Result<Option<u32>, Error> {
        let mut out: Option<u32> = None;
        while let Some(&c @ b'0'..=b'9') = self.input.first() {
            self.consume(1);
            out = Some(
                out.unwrap_or(0)
                    .checked_mul(10)
                    .and_then(|out| out.checked_add((c - b'0') as u32))
                    .ok_or(Error::IntegerOverflow)?,
            );
        }
        Ok(out)
    }
    fn next(&mut self) -> Result<u8, Error> {
        self.input
            .first()
            .map(|&c| {
                self.consume(1);
                c
            })
            .ok_or(Error::EOF)
    }
    fn expect(&mut self, c: u8) -> Result<(), Error> {
        if self.input.first() != Some(&c) {
            return Err(Error::Expected(c, self.input.first().cloned()));
        }
        self.consume(1);
        Ok(())
    }
    fn parse_range(&mut self) -> Result<Range, Error> {
        let mut range = Range(1, Some(1));
        if let Some(&c) = self.input.first() {
            let new = match c {
                b'*' => Some((1, Range(0, None))),
                b'\\' => match self.input.get(1) {
                    Some(b'?') if !self.extended => Some((2, Range(0, Some(1)))),
                    Some(b'+') if !self.extended => Some((2, Range(1, None))),
                    Some(b'{') if !self.extended => {
                        self.consume(2);
                        let first = self.take_int()?.ok_or(Error::EmptyRepetition)?;
                        let mut second = Some(first);
                        if let Some(b',') = self.input.first() {
                            self.consume(1);
                            second = self.take_int()?;
                        }
                        if self.input.first() == Some(&b'}') {
                            self.consume(1);
                        } else if self.input.starts_with(br"\}") {
                            self.consume(2);
                        } else {
                            return Err(Error::UnclosedRepetition);
                        }
                        if second.map(|second| first > second).unwrap_or(false) {
                            return Err(Error::IllegalRange);
                        }
                        range = Range(first, second);
                        None
                    }
                    _ => None,
                },
                b'?' if self.extended => Some((1, Range(0, Some(1)))),
                b'+' if self.extended => Some((1, Range(1, None))),
                b'{' if self.extended => {
                    self.consume(1);
                    let first = self.take_int()?.ok_or(Error::EmptyRepetition)?;
                    let mut second = Some(first);
                    if let Some(b',') = self.input.first() {
                        self.consume(1);
                        second = self.take_int()?;
                    }
                    if self.input.first() == Some(&b'}') {
                        self.consume(1);
                    } else if self.input.starts_with(br"\}") {
                        self.consume(2);
                    } else {
                        return Err(Error::UnclosedRepetition);
                    }
                    if second.map(|second| first > second).unwrap_or(false) {
                        return Err(Error::IllegalRange);
                    }
                    range = Range(first, second);
                    None
                }
                _ => None,
            };
            if let Some((consume, new)) = new {
                range = new;
                self.consume(consume);
            }
        }
        Ok(range)
    }
    fn parse(&mut self, group: bool) -> Result<(), Error> {
        self.depth += 1;
        if self.depth > 128 || self.group_id > 1024 {
            return Err(Error::PatternLimit);
        }
        let mut closed = false;
        let mut any = false;
        self.builder
            .start_internal(Token::Alternative, Range(1, Some(1)));
        while let Ok(c) = self.next() {
            let token = match c {
                b'^' => Token::Start,
                b'$' => Token::End,
                b'.' => Token::Any,
                b'[' => {
                    let mut list = Vec::new();
                    let invert = self.input.first() == Some(&b'^');
                    if invert {
                        self.consume(1);
                    }
                    loop {
                        let atom = self.bracket_atom()?;
                        if self.input.first() == Some(&b'-') && self.input.get(1) != Some(&b']') {
                            self.consume(1);
                            let dest = self.bracket_atom()?;
                            let (Collation::Char(low), Collation::Char(high)) = (atom, dest) else {
                                return Err(Error::IllegalRange);
                            };
                            if low > high {
                                return Err(Error::IllegalRange);
                            }
                            list.push(Collation::Range(low, high));
                        } else {
                            list.push(atom);
                        }
                        if self.input.first() == Some(&b']') {
                            self.consume(1);
                            break;
                        }
                    }
                    Token::OneOf { invert, list }
                }
                b'\\'
                    if self
                        .input
                        .first()
                        .map(|&c| c.is_ascii_digit())
                        .unwrap_or(false) =>
                {
                    let id = self.take_int()?.unwrap();
                    if !self.closed_groups.contains(&(id as usize)) {
                        return Err(Error::InvalidBackRef(id));
                    }
                    Token::BackRef(id)
                }
                b'\\' => match self.next()? {
                    b'(' if !self.extended => {
                        let id = self.group_id;
                        self.group_id += 1;
                        let checkpoint = self.builder.checkpoint();
                        self.parse(true)?;
                        self.closed_groups.push(id);
                        let range = self.parse_range()?;
                        self.builder
                            .start_internal_at(checkpoint, Token::Group(id), range);
                        self.builder.finish_internal();
                        any = true;
                        continue;
                    }
                    b')' if !self.extended => {
                        if !group {
                            return Err(Error::UnexpectedToken(b')'));
                        }
                        closed = true;
                        break;
                    }
                    b'|' if !self.extended => {
                        if !any {
                            self.builder.leaf(Token::Empty, Range(1, Some(1)));
                        }
                        self.builder.finish_internal();
                        self.builder
                            .start_internal(Token::Alternative, Range(1, Some(1)));
                        any = false;
                        continue;
                    }
                    b'<' => Token::WordStart,
                    b'>' => Token::WordEnd,
                    b'a' => Token::OneOf {
                        invert: false,
                        list: vec![Collation::Class(ctype::is_alnum)],
                    },
                    b'd' => Token::OneOf {
                        invert: false,
                        list: vec![Collation::Class(ctype::is_digit)],
                    },
                    b's' => Token::OneOf {
                        invert: false,
                        list: vec![Collation::Class(ctype::is_space)],
                    },
                    b'S' => Token::OneOf {
                        invert: true,
                        list: vec![Collation::Class(ctype::is_space)],
                    },
                    b'n' => Token::Char(u32::from(b'\n')),
                    b'r' => Token::Char(u32::from(b'\r')),
                    b't' => Token::Char(u32::from(b'\t')),
                    c => Token::Char(self.scalar(c)),
                },
                b'(' if self.extended => {
                    let id = self.group_id;
                    self.group_id += 1;
                    let checkpoint = self.builder.checkpoint();
                    self.parse(true)?;
                    self.closed_groups.push(id);
                    let range = self.parse_range()?;
                    self.builder
                        .start_internal_at(checkpoint, Token::Group(id), range);
                    self.builder.finish_internal();
                    any = true;
                    continue;
                }
                b')' if self.extended => {
                    if !group {
                        return Err(Error::UnexpectedToken(b')'));
                    }
                    closed = true;
                    break;
                }
                b'|' if self.extended => {
                    if !any {
                        self.builder.leaf(Token::Empty, Range(1, Some(1)));
                    }
                    self.builder.finish_internal();
                    self.builder
                        .start_internal(Token::Alternative, Range(1, Some(1)));
                    any = false;
                    continue;
                }
                b'*' => return Err(Error::LeadingRepetition),
                b'?' | b'+' | b'{' if self.extended => return Err(Error::LeadingRepetition),
                c => Token::Char(self.scalar(c)),
            };
            let range = self.parse_range()?;
            self.builder.leaf(token, range);
            any = true;
        }
        if !any {
            self.builder.leaf(Token::Empty, Range(1, Some(1)));
        }
        self.builder.finish_internal();
        self.depth -= 1;
        if group && !closed {
            return Err(Error::UnclosedGroup);
        }
        Ok(())
    }

    fn bracket_atom(&mut self) -> Result<Collation, Error> {
        let byte = self.next()?;
        if byte != b'[' || !matches!(self.input.first(), Some(b'.' | b'=' | b':')) {
            return Ok(Collation::Char(self.scalar(byte)));
        }
        let marker = self.next()?;
        if marker == b':' {
            let end = self
                .input
                .iter()
                .position(|&c| c == b':')
                .ok_or(Error::EOF)?;
            let key = &self.input[..end];
            let class = *self
                .classes
                .get(key)
                .ok_or_else(|| Error::UnknownClass(key.to_vec()))?;
            self.consume(end + 1);
            self.expect(b']')?;
            return Ok(Collation::Class(class));
        }
        let byte = self.next()?;
        let scalar = self.scalar(byte);
        self.expect(marker)?;
        self.expect(b']')?;
        Ok(if marker == b'=' {
            Collation::Equivalent(scalar)
        } else {
            Collation::Char(scalar)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloc::{format, string::String};

    fn compile(input: &[u8]) -> String {
        format!(
            "{:?}",
            PosixRegexBuilder::new(input)
                .with_default_classes()
                .compile_tokens()
                .expect("error compiling regex")
        )
    }

    #[test]
    fn basic() {
        assert_eq!(
            compile(b"abc"),
            "\
Root 1..1
  Alternative 1..1
    'a' 1..1
    'b' 1..1
    'c' 1..1
"
        );
    }
    #[test]
    fn groups() {
        assert_eq!(
            compile(br"\(abc\|bcd\|cde\)"),
            "\
Root 1..1
  Alternative 1..1
    Group(1) 1..1
      Alternative 1..1
        'a' 1..1
        'b' 1..1
        'c' 1..1
      Alternative 1..1
        'b' 1..1
        'c' 1..1
        'd' 1..1
      Alternative 1..1
        'c' 1..1
        'd' 1..1
        'e' 1..1
"
        );
        assert_eq!(
            compile(br"\(abc\|\(bcd\|cde\)\)"),
            "\
Root 1..1
  Alternative 1..1
    Group(1) 1..1
      Alternative 1..1
        'a' 1..1
        'b' 1..1
        'c' 1..1
      Alternative 1..1
        Group(2) 1..1
          Alternative 1..1
            'b' 1..1
            'c' 1..1
            'd' 1..1
          Alternative 1..1
            'c' 1..1
            'd' 1..1
            'e' 1..1
"
        );
    }
    #[test]
    fn words() {
        assert_eq!(
            compile(br"\<word\>"),
            "\
Root 1..1
  Alternative 1..1
    < 1..1
    'w' 1..1
    'o' 1..1
    'r' 1..1
    'd' 1..1
    > 1..1
"
        );
    }
    #[test]
    fn repetitions() {
        assert_eq!(
            compile(br"yeee*"),
            "\
Root 1..1
  Alternative 1..1
    'y' 1..1
    'e' 1..1
    'e' 1..1
    'e' 0..
"
        );
        assert_eq!(
            compile(br"yee\?"),
            "\
Root 1..1
  Alternative 1..1
    'y' 1..1
    'e' 1..1
    'e' 0..1
"
        );
        assert_eq!(
            compile(br"yee\+"),
            "\
Root 1..1
  Alternative 1..1
    'y' 1..1
    'e' 1..1
    'e' 1..
"
        );
        assert_eq!(
            compile(br"ye\{2}"),
            "\
Root 1..1
  Alternative 1..1
    'y' 1..1
    'e' 2..2
"
        );
        assert_eq!(
            compile(br"ye\{2,}"),
            "\
Root 1..1
  Alternative 1..1
    'y' 1..1
    'e' 2..
"
        );
        assert_eq!(
            compile(br"ye\{2,3}"),
            "\
Root 1..1
  Alternative 1..1
    'y' 1..1
    'e' 2..3
"
        );
    }
    #[test]
    fn bracket() {
        assert_eq!(
            compile(b"[abc]"),
            "\
Root 1..1
  Alternative 1..1
    {invert: false, ['a', 'b', 'c']} 1..1
"
        );
        assert_eq!(
            compile(b"[^abc]"),
            "\
Root 1..1
  Alternative 1..1
    {invert: true, ['a', 'b', 'c']} 1..1
"
        );
        assert_eq!(
            compile(b"[]] [^]]"),
            "\
Root 1..1
  Alternative 1..1
    {invert: false, [']']} 1..1
    ' ' 1..1
    {invert: true, [']']} 1..1
"
        );
        assert_eq!(
            compile(b"[0-3] [a-c] [-1] [1-]"),
            "\
Root 1..1
  Alternative 1..1
    {invert: false, ['0', '1', '2', '3']} 1..1
    ' ' 1..1
    {invert: false, ['a', 'b', 'c']} 1..1
    ' ' 1..1
    {invert: false, ['-', '1']} 1..1
    ' ' 1..1
    {invert: false, ['1', '-']} 1..1
"
        );
        assert_eq!(
            compile(b"[[.-.]-/]"),
            "\
Root 1..1
  Alternative 1..1
    {invert: false, ['-', '.', '/']} 1..1
"
        );
        assert_eq!(
            compile(b"[[:digit:][:upper:]]"),
            format!(
                "\
Root 1..1
  Alternative 1..1
    {{invert: false, [{:p}, {:p}]}} 1..1
",
                ctype::is_digit as fn(u32) -> bool,
                ctype::is_upper as fn(u32) -> bool
            )
        );
    }
    #[test]
    fn newline() {
        assert_eq!(
            compile(br"\r\n"),
            "\
Root 1..1
  Alternative 1..1
    '\\r' 1..1
    '\\n' 1..1
"
        );
    }
    #[test]
    fn backref() {
        assert_eq!(
            compile(br"\([abc]\)\1"),
            "\
Root 1..1
  Alternative 1..1
    Group(1) 1..1
      Alternative 1..1
        {invert: false, ['a', 'b', 'c']} 1..1
    \\1 1..1
"
        )
    }

    fn compile_extended(input: &[u8]) -> String {
        format!(
            "{:?}",
            PosixRegexBuilder::new(input)
                .with_default_classes()
                .extended(true)
                .compile_tokens()
                .expect("error compiling regex")
        )
    }

    #[test]
    fn basic_extended() {
        assert_eq!(
            compile_extended(b"abc"),
            "\
Root 1..1
  Alternative 1..1
    'a' 1..1
    'b' 1..1
    'c' 1..1
"
        );
    }
    #[test]
    fn groups_extended() {
        assert_eq!(
            compile_extended(br"(abc|bcd|cde)"),
            "\
Root 1..1
  Alternative 1..1
    Group(1) 1..1
      Alternative 1..1
        'a' 1..1
        'b' 1..1
        'c' 1..1
      Alternative 1..1
        'b' 1..1
        'c' 1..1
        'd' 1..1
      Alternative 1..1
        'c' 1..1
        'd' 1..1
        'e' 1..1
"
        );
        assert_eq!(
            compile_extended(br"(abc|(bcd|cde))"),
            "\
Root 1..1
  Alternative 1..1
    Group(1) 1..1
      Alternative 1..1
        'a' 1..1
        'b' 1..1
        'c' 1..1
      Alternative 1..1
        Group(2) 1..1
          Alternative 1..1
            'b' 1..1
            'c' 1..1
            'd' 1..1
          Alternative 1..1
            'c' 1..1
            'd' 1..1
            'e' 1..1
"
        );
    }
    #[test]
    fn words_extended() {
        assert_eq!(
            compile_extended(br"\<word\>"),
            "\
Root 1..1
  Alternative 1..1
    < 1..1
    'w' 1..1
    'o' 1..1
    'r' 1..1
    'd' 1..1
    > 1..1
"
        );
    }

    #[test]
    fn repetitions_extended() {
        assert_eq!(
            compile_extended(br"yeee*"),
            "\
Root 1..1
  Alternative 1..1
    'y' 1..1
    'e' 1..1
    'e' 1..1
    'e' 0..
"
        );
        assert_eq!(
            compile_extended(br"yee?"),
            "\
Root 1..1
  Alternative 1..1
    'y' 1..1
    'e' 1..1
    'e' 0..1
"
        );
        assert_eq!(
            compile_extended(br"yee+"),
            "\
Root 1..1
  Alternative 1..1
    'y' 1..1
    'e' 1..1
    'e' 1..
"
        );
        assert_eq!(
            compile_extended(br"ye{2}"),
            "\
Root 1..1
  Alternative 1..1
    'y' 1..1
    'e' 2..2
"
        );
        assert_eq!(
            compile_extended(br"ye{2,}"),
            "\
Root 1..1
  Alternative 1..1
    'y' 1..1
    'e' 2..
"
        );
        assert_eq!(
            compile_extended(br"ye{2,3}"),
            "\
Root 1..1
  Alternative 1..1
    'y' 1..1
    'e' 2..3
"
        );
    }
    #[test]
    fn bracket_extended() {
        assert_eq!(
            compile_extended(b"[abc]"),
            "\
Root 1..1
  Alternative 1..1
    {invert: false, ['a', 'b', 'c']} 1..1
"
        );
        assert_eq!(
            compile_extended(b"[^abc]"),
            "\
Root 1..1
  Alternative 1..1
    {invert: true, ['a', 'b', 'c']} 1..1
"
        );
        assert_eq!(
            compile_extended(b"[]] [^]]"),
            "\
Root 1..1
  Alternative 1..1
    {invert: false, [']']} 1..1
    ' ' 1..1
    {invert: true, [']']} 1..1
"
        );
        assert_eq!(
            compile_extended(b"[0-3] [a-c] [-1] [1-]"),
            "\
Root 1..1
  Alternative 1..1
    {invert: false, ['0', '1', '2', '3']} 1..1
    ' ' 1..1
    {invert: false, ['a', 'b', 'c']} 1..1
    ' ' 1..1
    {invert: false, ['-', '1']} 1..1
    ' ' 1..1
    {invert: false, ['1', '-']} 1..1
"
        );
        assert_eq!(
            compile_extended(b"[[.-.]-/]"),
            "\
Root 1..1
  Alternative 1..1
    {invert: false, ['-', '.', '/']} 1..1
"
        );
        assert_eq!(
            compile_extended(b"[[:digit:][:upper:]]"),
            format!(
                "\
Root 1..1
  Alternative 1..1
    {{invert: false, [{:p}, {:p}]}} 1..1
",
                ctype::is_digit as fn(u32) -> bool,
                ctype::is_upper as fn(u32) -> bool
            )
        );
    }
    #[test]
    fn newline_extended() {
        assert_eq!(
            compile_extended(br"\r\n"),
            "\
Root 1..1
  Alternative 1..1
    '\\r' 1..1
    '\\n' 1..1
"
        );
    }
    #[test]
    fn backref_extended() {
        assert_eq!(
            compile_extended(br"([abc])\1"),
            "\
Root 1..1
  Alternative 1..1
    Group(1) 1..1
      Alternative 1..1
        {invert: false, ['a', 'b', 'c']} 1..1
    \\1 1..1
"
        )
    }
}
