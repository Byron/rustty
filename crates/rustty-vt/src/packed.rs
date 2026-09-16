//! Ghostty's cell and row bit layouts. Resource identities belong to the page.
use crate::screen::{Color, SemanticContent};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(transparent)]
pub struct Cell(u64);

const _: () = assert!(size_of::<Cell>() == 8 && align_of::<Cell>() == 8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ContentTag {
    Codepoint = 0,
    Grapheme = 1,
    BackgroundPalette = 2,
    BackgroundRgb = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Wide {
    Narrow = 0,
    Wide = 1,
    SpacerTail = 2,
    SpacerHead = 3,
}

impl Cell {
    pub(crate) const CONTENT_MASK: u64 = (1 << 26) - 1;
    pub(crate) const STYLE_MASK: u64 = 0xffff << 26;
    pub(crate) const WIDE_MASK: u64 = 3 << 42;
    pub(crate) const HYPERLINK_MASK: u64 = 1 << 45;

    #[inline]
    pub fn bits(self) -> u64 {
        self.0
    }

    #[inline]
    pub(crate) fn from_bits(bits: u64) -> Self {
        Self(bits & ((1 << 48) - 1))
    }

    #[inline]
    pub fn content_tag(self) -> ContentTag {
        match self.0 & 3 {
            0 => ContentTag::Codepoint,
            1 => ContentTag::Grapheme,
            2 => ContentTag::BackgroundPalette,
            _ => ContentTag::BackgroundRgb,
        }
    }

    #[inline]
    pub fn codepoint(self) -> Option<char> {
        if self.0 & 2 != 0 {
            return None;
        }
        match (self.0 >> 2) as u32 & 0x1f_ffff {
            0 => None,
            cp => Some(char::from_u32(cp).unwrap_or('\u{fffd}')),
        }
    }

    #[inline]
    pub fn style_id(self) -> u16 {
        (self.0 >> 26) as u16
    }

    #[inline]
    pub fn wide(self) -> Wide {
        match (self.0 >> 42) & 3 {
            0 => Wide::Narrow,
            1 => Wide::Wide,
            2 => Wide::SpacerTail,
            _ => Wide::SpacerHead,
        }
    }

    #[inline]
    pub fn width(self) -> u8 {
        match self.wide() {
            Wide::Narrow | Wide::SpacerHead => 1,
            Wide::Wide => 2,
            Wide::SpacerTail => 0,
        }
    }

    #[inline]
    pub fn spacer_head(self) -> bool {
        self.wide() == Wide::SpacerHead
    }

    #[inline]
    pub fn protected(self) -> bool {
        self.0 & (1 << 44) != 0
    }

    #[inline]
    pub fn has_hyperlink(self) -> bool {
        self.0 & Self::HYPERLINK_MASK != 0
    }

    #[inline]
    pub fn has_grapheme(self) -> bool {
        self.content_tag() == ContentTag::Grapheme
    }

    #[inline]
    pub fn semantic(self) -> SemanticContent {
        match (self.0 >> 46) & 3 {
            1 => SemanticContent::Input,
            2 => SemanticContent::Prompt,
            _ => SemanticContent::Output,
        }
    }

    #[inline]
    pub fn background(self) -> Option<Color> {
        match self.content_tag() {
            ContentTag::BackgroundPalette => Some(Color::Indexed((self.0 >> 2) as u8)),
            ContentTag::BackgroundRgb => Some(Color::Rgb(
                (self.0 >> 2) as u8,
                (self.0 >> 10) as u8,
                (self.0 >> 18) as u8,
            )),
            _ => None,
        }
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.codepoint().is_none() && self.width() != 0
    }

    #[inline]
    pub(crate) fn blank(background: Color) -> Self {
        let mut cell = Self::default();
        cell.set_background(background);
        cell
    }

    #[inline]
    pub(crate) fn set_codepoint(&mut self, cp: Option<char>) {
        self.0 = (self.0 & !Self::CONTENT_MASK) | (u64::from(cp.map_or(0, u32::from)) << 2);
    }

    #[inline]
    pub(crate) fn set_background(&mut self, color: Color) {
        let content = match color {
            Color::Default => 0,
            Color::Indexed(index) => 2 | (u64::from(index) << 2),
            Color::Rgb(r, g, b) => {
                3 | (u64::from(r) << 2) | (u64::from(g) << 10) | (u64::from(b) << 18)
            }
        };
        self.0 = (self.0 & !Self::CONTENT_MASK) | content;
    }

    #[inline]
    pub(crate) fn set_style_id(&mut self, id: u16) {
        self.0 = (self.0 & !Self::STYLE_MASK) | (u64::from(id) << 26);
    }

    #[inline]
    pub(crate) fn set_wide(&mut self, wide: Wide) {
        self.0 = (self.0 & !Self::WIDE_MASK) | ((wide as u64) << 42);
    }

    #[inline]
    pub(crate) fn set_width(&mut self, width: u8) {
        self.set_wide(match width {
            0 => Wide::SpacerTail,
            1 => Wide::Narrow,
            2 => Wide::Wide,
            _ => panic!("invalid terminal cell width"),
        });
    }

    #[inline]
    pub(crate) fn set_spacer_head(&mut self, value: bool) {
        if value {
            self.set_wide(Wide::SpacerHead);
        } else if self.spacer_head() {
            self.set_wide(Wide::Narrow);
        }
    }

    #[inline]
    pub(crate) fn set_protected(&mut self, value: bool) {
        self.0 = (self.0 & !(1 << 44)) | (u64::from(value) << 44);
    }

    #[inline]
    pub(crate) fn set_hyperlink(&mut self, value: bool) {
        self.0 = (self.0 & !Self::HYPERLINK_MASK) | (u64::from(value) << 45);
    }

    #[inline]
    pub(crate) fn set_grapheme(&mut self, value: bool) {
        debug_assert!(self.0 & 2 == 0);
        self.0 = (self.0 & !3) | u64::from(value);
    }

    #[inline]
    pub(crate) fn set_semantic(&mut self, value: SemanticContent) {
        let value = match value {
            SemanticContent::Output => 0,
            SemanticContent::Input => 1,
            SemanticContent::Prompt => 2,
        };
        self.0 = (self.0 & !(3 << 46)) | (value << 46);
    }
}

/// Offsets are cell slots in the page's typed allocation. The remaining bits
/// have Ghostty's row layout; logical row identity is stored beside the header.
#[derive(Clone, Copy, Debug, Default)]
#[repr(transparent)]
pub(crate) struct RowHeader(u64);

const _: () = assert!(size_of::<RowHeader>() == 8 && align_of::<RowHeader>() == 8);

impl RowHeader {
    pub const WRAPPED: u64 = 1 << 32;
    pub const CONTINUATION: u64 = 1 << 33;
    pub const GRAPHEME: u64 = 1 << 34;
    pub const STYLED: u64 = 1 << 35;
    pub const HYPERLINK: u64 = 1 << 36;
    pub const PLACEHOLDER: u64 = 1 << 39;
    pub const DIRTY: u64 = 1 << 40;
    pub const MANAGED: u64 = Self::GRAPHEME | Self::STYLED | Self::HYPERLINK;

    #[inline]
    pub fn new(offset: u32) -> Self {
        Self(u64::from(offset) | Self::DIRTY)
    }

    #[inline]
    pub fn offset(self) -> usize {
        self.0 as u32 as usize
    }

    #[inline]
    pub fn has(self, flag: u64) -> bool {
        self.0 & flag != 0
    }

    #[inline]
    pub fn set(&mut self, flag: u64, value: bool) {
        self.0 = (self.0 & !flag) | if value { flag } else { 0 };
    }

    #[inline]
    pub fn semantic(self) -> SemanticContent {
        match (self.0 >> 37) & 3 {
            1 => SemanticContent::Prompt,
            2 => SemanticContent::Input,
            _ => SemanticContent::Output,
        }
    }

    #[inline]
    pub fn set_semantic(&mut self, value: SemanticContent) {
        let value = match value {
            SemanticContent::Output => 0,
            SemanticContent::Prompt => 1,
            SemanticContent::Input => 2,
        };
        self.0 = (self.0 & !(3 << 37)) | (value << 37);
    }

    #[inline]
    pub fn reset(&mut self) {
        *self = Self::new(self.offset() as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_cell_layout_and_independent_fields() {
        let blank = Cell::default();
        assert_eq!(blank.bits(), 0);
        assert_eq!(blank.width(), 1);
        assert!(blank.is_empty());
        for cp in ['a', '界', '\u{10ffff}'] {
            for wide in [Wide::Narrow, Wide::Wide, Wide::SpacerTail, Wide::SpacerHead] {
                for (semantic, native) in [
                    (SemanticContent::Output, 0),
                    (SemanticContent::Input, 1),
                    (SemanticContent::Prompt, 2),
                ] {
                    let mut cell = blank;
                    cell.set_codepoint(Some(cp));
                    cell.set_style_id(u16::MAX);
                    cell.set_wide(wide);
                    cell.set_protected(true);
                    cell.set_hyperlink(true);
                    cell.set_grapheme(true);
                    cell.set_semantic(semantic);
                    assert_eq!(
                        cell.bits(),
                        1 | ((cp as u64) << 2)
                            | (0xffff << 26)
                            | ((wide as u64) << 42)
                            | (1 << 44)
                            | (1 << 45)
                            | (native << 46)
                    );
                    assert_eq!(cell.codepoint(), Some(cp));
                    assert_eq!(cell.style_id(), u16::MAX);
                    assert_eq!(cell.wide(), wide);
                    assert_eq!(cell.semantic(), semantic);
                    assert!(cell.protected() && cell.has_hyperlink() && cell.has_grapheme());
                    cell.set_style_id(0);
                    cell.set_protected(false);
                    cell.set_hyperlink(false);
                    cell.set_grapheme(false);
                    assert_eq!(cell.codepoint(), Some(cp));
                    assert_eq!(cell.bits() >> 48, 0);
                }
            }
        }
        assert_eq!(Cell::from_bits(u64::MAX).bits() >> 48, 0);
    }

    #[test]
    fn background_union_and_row_reset() {
        for color in [
            Color::Indexed(0),
            Color::Indexed(255),
            Color::Rgb(0, 127, 255),
        ] {
            let mut cell = Cell::blank(color);
            assert_eq!(cell.background(), Some(color));
            assert_eq!(cell.codepoint(), None);
            assert!(cell.is_empty());
            cell.set_style_id(321);
            assert_eq!(cell.background(), Some(color));
            cell.set_codepoint(Some('x'));
            assert_eq!(cell.background(), None);
            assert_eq!(cell.style_id(), 321);
        }
        let mut row = RowHeader::new(u32::MAX);
        row.set(
            RowHeader::MANAGED
                | RowHeader::PLACEHOLDER
                | RowHeader::WRAPPED
                | RowHeader::CONTINUATION,
            true,
        );
        row.set_semantic(SemanticContent::Input);
        assert_eq!(row.semantic(), SemanticContent::Input);
        row.reset();
        assert_eq!(row.offset(), u32::MAX as usize);
        assert!(!row.has(
            RowHeader::MANAGED
                | RowHeader::PLACEHOLDER
                | RowHeader::WRAPPED
                | RowHeader::CONTINUATION
        ));
        assert!(row.has(RowHeader::DIRTY));
        assert_eq!(row.semantic(), SemanticContent::Output);
    }
}
