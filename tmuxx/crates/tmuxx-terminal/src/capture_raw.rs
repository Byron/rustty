//! Native grid diagnostics use tmux's G/L/C record grammar. Page slots, decoded
//! glyphs and expanded tabs describe this backend, not C's packed-cell allocator.
use crate::{Color, Style, Terminal};
use rustty_vt::{HyperlinkId, Row, Screen, Underline};
use std::fmt::Write;

impl Terminal {
    pub(super) fn capture_raw(&self) -> Vec<u8> {
        let primary = self.inner.primary_screen();
        let active = self.inner.screen();
        let history = primary.history_len();
        let limit = primary.tmux_history.as_ref().map_or(0, |state| state.limit);
        let (columns, height) = self.size();
        let mut output = format!("G {columns}x{height} ({history}/{limit})\n").into_bytes();
        let rows: Vec<(&Screen, Row<'_>)> = if self.inner.is_alternate_screen() {
            primary
                .history()
                .map(|row| (primary, row))
                .chain(active.all_rows().map(|row| (active, row)))
                .collect()
        } else {
            primary.all_rows().map(|row| (primary, row)).collect()
        };
        for (y, (screen, row)) in rows.into_iter().enumerate() {
            let position = if y < history {
                "-".into()
            } else {
                (y - history).to_string()
            };
            let flags = screen.tmux_line_flags(row);
            let used = screen.tmux_capture_bounds(row.id).map_or_else(
                || {
                    row.cells
                        .iter()
                        .enumerate()
                        .rposition(|(x, cell)| {
                            cell.codepoint().is_some()
                                || cell.width() == 0
                                || row.style(x).background != Color::Default
                        })
                        .map_or(0, |x| x + 1)
                },
                |bounds| bounds.0,
            );
            let mut line = format!(
                "\tL {y} ({position}) flags={}[{flags:x}] {used}/{}",
                names(
                    flags,
                    &[
                        (1, "WRAPPED"),
                        (2, "EXTENDED"),
                        (4, "DEAD"),
                        (8, "START_PROMPT"),
                        (16, "SECOND_PROMPT"),
                        (32, "START_COMMAND"),
                        (64, "START_OUTPUT"),
                        (128, "END_OUTPUT"),
                        (256, "HYPERLINK"),
                    ]
                ),
                row.cells.len()
            );
            if let Some(markers) = screen
                .tmux_shell_markers()
                .and_then(|markers| markers.get(&row.id))
            {
                write!(
                    line,
                    " osc133={},{},{},{},{}",
                    markers.prompt.map_or(0, |p| p.0),
                    markers.command.unwrap_or(0),
                    markers.start.unwrap_or(0),
                    markers.end.map_or(0, |p| p.0),
                    markers.end.map_or(0, |p| p.1)
                )
                .unwrap();
            }
            line.push('\n');
            output.extend_from_slice(line.as_bytes());
            for (x, cell) in row.cells.iter().enumerate().take(usize::from(columns)) {
                let style = row.style(x);
                let padding = cell.width() == 0 || cell.spacer_head();
                let text = if padding {
                    String::new()
                } else if cell.codepoint().is_none() {
                    " ".into()
                } else {
                    row.text(x).to_string()
                };
                let mut flags = if padding {
                    4
                } else if cell.codepoint().is_none() {
                    64
                } else {
                    0
                };
                if matches!(style.foreground, Color::Indexed(_)) && style.tmux_ansi & 1 == 0 {
                    flags |= 1;
                }
                if matches!(style.background, Color::Indexed(_)) && style.tmux_ansi & 2 == 0 {
                    flags |= 2;
                }
                let attributes = attributes(style);
                let line = format!(
                    "\t\tC {y},{x} data=({},{},{}) flags={}[{flags:x}] attr={}[{attributes:x}] fg={} bg={} us={} link=",
                    if padding { 0 } else { cell.width() },
                    text.len(),
                    visible(&text),
                    names(
                        flags,
                        &[
                            (1, "FG256"),
                            (2, "BG256"),
                            (4, "PADDING"),
                            (8, "EXTENDED"),
                            (16, "SELECTED"),
                            (64, "CLEARED"),
                            (128, "TAB"),
                            (32, "NOPALETTE")
                        ]
                    ),
                    names(
                        attributes,
                        &[
                            (128, "CHARSET"),
                            (1, "BRIGHT"),
                            (2, "DIM"),
                            (4, "UNDERSCORE"),
                            (8, "BLINK"),
                            (16, "REVERSE"),
                            (32, "HIDDEN"),
                            (64, "ITALICS"),
                            (256, "STRIKETHROUGH"),
                            (512, "UNDERSCORE_2"),
                            (1024, "UNDERSCORE_3"),
                            (2048, "UNDERSCORE_4"),
                            (4096, "UNDERSCORE_5"),
                            (8192, "OVERLINE")
                        ]
                    ),
                    color(style.foreground, style.tmux_ansi & 1 != 0),
                    color(style.background, style.tmux_ansi & 2 != 0),
                    color(style.underline_color, false)
                );
                output.extend_from_slice(line.as_bytes());
                output.extend_from_slice(
                    row.hyperlink(x)
                        .map_or(b"NONE".as_slice(), |link| link.uri_bytes()),
                );
                output.extend_from_slice(b" linkid=");
                output.extend_from_slice(
                    match row.hyperlink(x).and_then(|link| link.id.as_ref()) {
                        Some(HyperlinkId::Explicit(id)) if !id.is_empty() => id,
                        _ => b"NONE",
                    },
                );
                output.push(b'\n');
            }
        }
        output
    }
}

fn names(bits: u16, names: &[(u16, &str)]) -> String {
    let names = names
        .iter()
        .filter_map(|(mask, name)| (bits & mask != 0).then_some(*name))
        .collect::<Vec<_>>();
    if names.is_empty() {
        "NONE".into()
    } else {
        names.join(",")
    }
}

fn attributes(style: Style) -> u16 {
    let mut attributes = 0;
    for (enabled, bit) in [
        (style.bold, 1),
        (style.faint, 2),
        (style.blink, 8),
        (style.inverse, 16),
        (style.invisible, 32),
        (style.italic, 64),
        (style.strikethrough, 256),
        (style.overline, 8192),
    ] {
        if enabled {
            attributes |= bit;
        }
    }
    attributes
        | match style.underline {
            Underline::None => 0,
            Underline::Single => 4,
            Underline::Double => 512,
            Underline::Curly => 1024,
            Underline::Dotted => 2048,
            Underline::Dashed => 4096,
        }
}

fn color(color: Color, ansi: bool) -> String {
    let (name, value) = match color {
        Color::Default => ("default".into(), 8),
        Color::Indexed(index) if ansi && index < 16 => {
            let name = [
                "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
            ][usize::from(index % 8)];
            if index < 8 {
                (name.into(), u32::from(index))
            } else {
                (format!("bright{name}"), 82 + u32::from(index))
            }
        }
        Color::Indexed(index) => (format!("colour{index}"), 0x1000000 | u32::from(index)),
        Color::Rgb(r, g, b) => (
            format!("#{r:02x}{g:02x}{b:02x}"),
            0x2000000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b),
        ),
    };
    format!("{name}[{value:x}]")
}

fn visible(text: &str) -> String {
    let mut result = String::new();
    for ch in text.chars() {
        match ch {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            '\x07' => result.push_str("\\a"),
            '\x08' => result.push_str("\\b"),
            '\x0b' => result.push_str("\\v"),
            '\x0c' => result.push_str("\\f"),
            ch if ch.is_ascii_control() => write!(result, "\\{:03o}", ch as u32).unwrap(),
            ch => result.push(ch),
        }
    }
    result
}
