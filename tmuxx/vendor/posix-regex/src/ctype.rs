//! Fixed Unicode 16 general categories for the Windows UTF-8 format locale.
use unicode_general_category::{GeneralCategory as Category, get_general_category};
pub fn is_alnum(c: u32) -> bool {
    is_alpha(c) || is_digit(c)
}
pub fn is_alpha(c: u32) -> bool {
    char::from_u32(c).is_some_and(|c| {
        matches!(
            get_general_category(c),
            Category::UppercaseLetter
                | Category::LowercaseLetter
                | Category::TitlecaseLetter
                | Category::ModifierLetter
                | Category::OtherLetter
                | Category::LetterNumber
        )
    })
}
pub fn is_blank(c: u32) -> bool {
    c == 9 || char::from_u32(c).is_some_and(|c| get_general_category(c) == Category::SpaceSeparator)
}
pub fn is_cntrl(c: u32) -> bool {
    char::from_u32(c).is_some_and(|c| get_general_category(c) == Category::Control)
}
pub fn is_digit(c: u32) -> bool {
    (48..=57).contains(&c)
}
pub fn is_graph(c: u32) -> bool {
    is_print(c) && !is_space(c)
}
pub fn is_lower(c: u32) -> bool {
    char::from_u32(c).is_some_and(|c| get_general_category(c) == Category::LowercaseLetter)
}
pub fn is_print(c: u32) -> bool {
    char::from_u32(c).is_some_and(|c| {
        !matches!(
            get_general_category(c),
            Category::Control
                | Category::Surrogate
                | Category::Unassigned
                | Category::LineSeparator
                | Category::ParagraphSeparator
        )
    })
}
pub fn is_punct(c: u32) -> bool {
    is_graph(c) && !is_alnum(c)
}
pub fn is_space(c: u32) -> bool {
    (9..=13).contains(&c)
        || char::from_u32(c).is_some_and(|c| {
            matches!(
                get_general_category(c),
                Category::SpaceSeparator | Category::LineSeparator | Category::ParagraphSeparator
            )
        })
}
pub fn is_upper(c: u32) -> bool {
    char::from_u32(c).is_some_and(|c| get_general_category(c) == Category::UppercaseLetter)
}
pub fn is_xdigit(c: u32) -> bool {
    is_digit(c) || (65..=70).contains(&c) || (97..=102).contains(&c)
}
pub fn is_word_boundary(c: u32) -> bool {
    !is_alnum(c) && c != 95
}
