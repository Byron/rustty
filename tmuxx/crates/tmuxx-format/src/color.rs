//! Colour formatting for contexts without a physical client terminal.
enum Color {
    Basic(u8),
    Indexed(u8),
    Rgb([u8; 3]),
    Default,
    Theme(u8),
}
const ANSI: [u32; 16] = [
    0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xc0c0c0, 0x808080,
    0xff0000, 0x00ff00, 0xffff00, 0x0000ff, 0xff00ff, 0x00ffff, 0xffffff,
];
fn rgb(value: u32) -> [u8; 3] {
    [(value >> 16) as u8, (value >> 8) as u8, value as u8]
}
fn palette(index: u8) -> [u8; 3] {
    if index < 16 {
        return rgb(ANSI[usize::from(index)]);
    }
    if index >= 232 {
        return [8 + (index - 232) * 10; 3];
    }
    let n = index - 16;
    let levels = [0, 95, 135, 175, 215, 255];
    [
        levels[usize::from(n / 36)],
        levels[usize::from(n / 6 % 6)],
        levels[usize::from(n % 6)],
    ]
}
fn parse(value: &str) -> Option<Color> {
    let value = value.to_ascii_lowercase();
    if let Some(hex) = value.strip_prefix('#') {
        return (hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| {
                u32::from_str_radix(hex, 16)
                    .ok()
                    .map(|n| Color::Rgb(rgb(n)))
            })
            .flatten();
    }
    if let Some(index) = value
        .strip_prefix("colour")
        .or_else(|| value.strip_prefix("color"))
    {
        return index.trim_start().parse::<u8>().ok().map(Color::Indexed);
    }
    if matches!(value.as_str(), "default" | "terminal") {
        return Some(Color::Default);
    }
    for (name, index) in [
        ("themeblack", 0),
        ("themewhite", 7),
        ("themelightgrey", 7),
        ("themedarkgrey", 0),
        ("themegreen", 2),
        ("themeyellow", 3),
        ("themered", 1),
        ("themeblue", 4),
        ("themecyan", 6),
        ("thememagenta", 5),
    ] {
        if name == value {
            return Some(Color::Theme(index));
        }
    }
    let names = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    for (index, name) in names.iter().enumerate() {
        if value == *name || value == index.to_string() {
            return Some(Color::Basic(index as u8));
        }
        if value == format!("bright{name}") || value == (90 + index).to_string() {
            return Some(Color::Basic(90 + index as u8));
        }
    }
    // Reuse the licensed X11 table used by the shared terminal parser.
    for line in include_str!("../../../../src/terminal/res/rgb.txt").lines() {
        if line
            .get(12..)
            .is_some_and(|name| name.trim().eq_ignore_ascii_case(&value))
        {
            return Some(Color::Rgb([
                line[..3].trim().parse().ok()?,
                line[4..7].trim().parse().ok()?,
                line[8..11].trim().parse().ok()?,
            ]));
        }
    }
    None
}
pub(super) fn valid(value: &str) -> bool {
    parse(value).is_some()
}

pub(super) fn format(value: &[u8], flags: &[u8]) -> Vec<u8> {
    let Ok(value) = std::str::from_utf8(value) else {
        return Vec::new();
    };
    let escape = flags.contains(&b'f') || flags.contains(&b'b');
    if escape && value.eq_ignore_ascii_case("none") {
        return b"\x1b[0m".to_vec();
    }
    let Some(color) = parse(value) else {
        return Vec::new();
    };
    if escape {
        let offset = if flags.contains(&b'b') { 40 } else { 30 };
        return match color {
            Color::Default => format!("\x1b[{}m", offset + 9),
            Color::Basic(n) | Color::Theme(n) => format!(
                "\x1b[{}m",
                if n >= 90 {
                    usize::from(n) + offset - 30
                } else {
                    usize::from(n) + offset
                }
            ),
            Color::Indexed(n) => format!("\x1b[{};5;{n}m", offset + 8),
            Color::Rgb([r, g, b]) => format!("\x1b[{};2;{r};{g};{b}m", offset + 8),
        }
        .into_bytes();
    }
    let [r, g, b] = match color {
        Color::Rgb(rgb) => rgb,
        Color::Indexed(n) => palette(n),
        Color::Basic(n) => palette(if n >= 90 { n - 82 } else { n }),
        Color::Default | Color::Theme(_) => return Vec::new(),
    };
    format!("{r:02x}{g:02x}{b:02x}").into_bytes()
}
