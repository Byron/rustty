//! Shared client terminal-feature identity in the pinned C registry order.

const FEATURES: &[&str] = &[
    "256",
    "appesc",
    "bpaste",
    "ccolour",
    "clipboard",
    "hyperlinks",
    "cstyle",
    "extkeys",
    "focus",
    "ignorefkeys",
    "imagescroll",
    "kittygraphics",
    "margins",
    "mouse",
    "osc7",
    "overline",
    "progressbar",
    "rectfill",
    "RGB",
    "sixel",
    "strikethrough",
    "sync",
    "title",
    "usstyle",
    "utf8",
];

/// Apply one -T argument, including case-insensitive names and suffix removal.
pub fn apply_features(mut bits: i32, value: &str) -> i32 {
    for name in value.split([':', ',']) {
        let (name, remove) = name
            .strip_suffix('@')
            .map_or((name, false), |name| (name, true));
        let Some(index) = FEATURES
            .iter()
            .position(|feature| feature.eq_ignore_ascii_case(name))
        else {
            break;
        };
        if remove {
            bits &= !(1 << index);
        } else {
            bits |= 1 << index;
        }
    }
    bits
}

/// Bit positions are the C terminal-feature registry order at the pinned source revision.
pub fn feature_names(features: i32) -> String {
    FEATURES
        .iter()
        .enumerate()
        .filter_map(|(bit, name)| (features & (1 << bit) != 0).then_some(*name))
        .collect::<Vec<_>>()
        .join(",")
}
