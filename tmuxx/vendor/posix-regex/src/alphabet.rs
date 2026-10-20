//! Unicode characters and malformed bytes occupy disjoint matching symbols.
use alloc::{string::ToString, vec::Vec};
use core::cell::RefCell;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

pub const INVALID_BYTE: u32 = 0x11_0000;

pub fn first(input: &[u8]) -> (u32, usize) {
    let Some(&byte) = input.first() else {
        return (0, 0);
    };
    if byte.is_ascii() {
        return (u32::from(byte), 1);
    }
    for length in 2..=4.min(input.len()) {
        if let Ok(text) = core::str::from_utf8(&input[..length]) {
            if let Some(value) = text.chars().next() {
                return (value as u32, length);
            }
        }
    }
    (INVALID_BYTE + u32::from(byte), 1)
}

pub fn decode(input: &[u8]) -> (Vec<u32>, Vec<usize>) {
    let (mut symbols, mut offsets) = (Vec::new(), Vec::new());
    let mut offset = 0;
    while offset < input.len() {
        offsets.push(offset);
        let (symbol, length) = first(&input[offset..]);
        symbols.push(symbol);
        offset += length;
    }
    offsets.push(input.len());
    (symbols, offsets)
}

/// Decode only the prefix visited by a search. Repeated substitutions must not
/// rescan and allocate the entire unconsumed tail for every short match.
pub struct Input<'a> {
    bytes: &'a [u8],
    cache: RefCell<(Vec<u32>, Vec<usize>)>,
}
impl<'a> Input<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            cache: RefCell::new((Vec::new(), alloc::vec![0])),
        }
    }
    pub fn get(&self, index: usize) -> Option<u32> {
        let mut cache = self.cache.borrow_mut();
        while cache.0.len() <= index {
            let offset = *cache.1.last()?;
            if offset == self.bytes.len() {
                return None;
            }
            let (symbol, used) = first(&self.bytes[offset..]);
            cache.0.push(symbol);
            cache.1.push(offset + used);
        }
        Some(cache.0[index])
    }
    pub fn has_offset(&self, index: usize) -> bool {
        index == 0 || self.get(index - 1).is_some()
    }
    pub fn byte_offset(&self, index: usize) -> usize {
        if index > 0 {
            self.get(index - 1);
        }
        self.cache.borrow().1[index]
    }
}

pub fn lower(value: u32) -> u32 {
    char::from_u32(value)
        .and_then(|c| c.to_lowercase().next())
        .map_or(value, |c| c as u32)
}

pub fn upper(value: u32) -> u32 {
    let Some(value_char) = char::from_u32(value) else {
        return value;
    };
    let mut mapped = value_char.to_uppercase();
    let first = mapped.next();
    if mapped.next().is_some() {
        value
    } else {
        first.map_or(value, |c| c as u32)
    }
}

/// Windows' fixed Unicode collation groups canonical base letters for [=x=].
/// Ordinary literals and ranges remain exact scalar comparisons.
pub fn equivalent(left: u32, right: u32) -> bool {
    let (Some(left), Some(right)) = (char::from_u32(left), char::from_u32(right)) else {
        return left == right;
    };
    let a: Vec<_> = left
        .to_string()
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect();
    let b: Vec<_> = right
        .to_string()
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect();
    if a.is_empty() || b.is_empty() {
        left == right
    } else {
        a == b
    }
}
