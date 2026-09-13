//! Selection export using native plain text, VT and HTML formatting rules.
use crate::{Color, Row, Screen, Selection, Style, Terminal, Underline};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    #[default]
    Plain,
    Vt,
    Html,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub emit: Format,
    pub unwrap: bool,
    pub trim: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            emit: Format::Plain,
            unwrap: true,
            trim: true,
        }
    }
}

impl Terminal {
    /// Export a selection with its palette and current style/hyperlink state.
    /// VT output can contain opaque hyperlink bytes, so it is not always UTF-8.
    /// This does not replace the active selection or change terminal state.
    pub fn format_selection(&self, selection: Selection, options: Options) -> Option<Vec<u8>> {
        let screen = self.screen();
        let contents = screen.format_selection(selection, options)?;
        let mut out = Vec::new();
        if options.emit == Format::Html {
            out.extend_from_slice(b"<style>:root{");
        }
        if options.emit != Format::Plain {
            for (index, [r, g, b]) in self.palette.iter().enumerate() {
                let value = match options.emit {
                    Format::Vt => format!("\x1b]4;{index};rgb:{r:02x}/{g:02x}/{b:02x}\x1b\\"),
                    Format::Html => format!("--vt-palette-{index}: #{r:02x}{g:02x}{b:02x};"),
                    Format::Plain => unreachable!(),
                };
                out.extend_from_slice(value.as_bytes());
            }
        }
        if options.emit == Format::Html {
            out.extend_from_slice(b"}</style>");
        }
        out.extend_from_slice(&contents);
        if options.emit == Format::Vt {
            style_open(&mut out, screen.cursor.style, Format::Vt);
            if let Some(uri) = &screen.cursor.hyperlink {
                out.extend_from_slice(b"\x1b]8;");
                if let Some(crate::HyperlinkId::Explicit(id)) = &screen.cursor.hyperlink_id {
                    out.extend_from_slice(b"id=");
                    out.extend_from_slice(id);
                }
                out.push(b';');
                out.extend_from_slice(
                    screen
                        .cursor
                        .hyperlink_raw
                        .as_deref()
                        .unwrap_or(uri.as_bytes()),
                );
                out.extend_from_slice(b"\x1b\\");
            }
        }
        Some(out)
    }
}

impl Screen {
    /// Export inclusive bounds, retaining physical page breaks and native wide-cell rules.
    /// Screen exports reference palette indices; Terminal exports include the palette.
    pub fn format_selection(&self, selection: Selection, options: Options) -> Option<Vec<u8>> {
        let rows: Vec<_> = self.all_rows().collect();
        let position = |point: crate::GridPoint| {
            let row = rows.iter().position(|row| row.id == point.row)?;
            (point.col < rows[row].cells.len()).then_some((row, point.col))
        };
        let mut start = position(selection.start)?;
        let mut end = position(selection.end)?;
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }
        if selection.rectangular && start.1 > end.1 {
            std::mem::swap(&mut start.1, &mut end.1);
        }
        let mut out = Vec::new();
        let mut offset = 0;
        let mut trailing = (0, 0);
        for page in &self.pages.pages {
            let count = usize::from(page.rows);
            if offset <= end.0 && offset + count > start.0 {
                let top = (
                    start.0.saturating_sub(offset),
                    if selection.rectangular || start.0 >= offset {
                        start.1
                    } else {
                        0
                    },
                );
                let bottom = (
                    (end.0 - offset).min(count - 1),
                    if selection.rectangular || end.0 < offset + count {
                        end.1
                    } else {
                        usize::from(page.columns) - 1
                    },
                );
                trailing = format_page(
                    &mut out,
                    &rows[offset..offset + count],
                    top,
                    bottom,
                    selection.rectangular,
                    options,
                    trailing,
                );
            }
            offset += count;
            if offset > end.0 {
                break;
            }
        }
        Some(out)
    }
}

fn format_page(
    out: &mut Vec<u8>,
    rows: &[&Row],
    start: (usize, usize),
    mut end: (usize, usize),
    rectangle: bool,
    options: Options,
    trailing: (usize, usize),
) -> (usize, usize) {
    let (mut blank_rows, mut blank_cells) = if start == (0, 0) { trailing } else { (0, 0) };
    let width = rows[0].cells.len();
    if start.1 >= width {
        return (blank_rows, blank_cells);
    }
    end.1 = end.1.min(width - 1);
    if options.unwrap
        && !rectangle
        && rows[end.0].cells[end.1].spacer_head
        && end.0 + 1 < rows.len()
    {
        end = (end.0 + 1, 0);
    }
    if start > end {
        return (blank_rows, blank_cells);
    }
    if options.emit == Format::Html {
        out.extend_from_slice(b"<div style=\"font-family: monospace; white-space: pre;\">");
    }
    let mut style = Style::default();
    let mut hyperlink = None;
    for (y, row) in rows.iter().enumerate().take(end.0 + 1).skip(start.0) {
        let right = if rectangle || y == end.0 {
            end.1 + 1
        } else {
            width
        };
        let mut left = if rectangle || y == start.0 {
            start.1
        } else {
            0
        };
        if left > 0 {
            if row.cells[left].spacer_head {
                continue;
            }
            if row.cells[left].width == 0 {
                left -= 1;
            }
        }
        let cells = &row.cells[left..right];
        if cells.iter().all(|cell| cell.text.is_empty()) {
            blank_rows += 1;
            continue;
        }
        if blank_rows > 0 {
            if style != Style::default() {
                style_close(out, options.emit);
                style = Style::default();
            }
            for _ in 0..blank_rows {
                out.extend_from_slice(if options.emit == Format::Vt {
                    b"\r\n"
                } else {
                    b"\n"
                });
            }
            blank_rows = 0;
        }
        if !row.wrapped || !options.unwrap {
            blank_rows += 1;
        }
        if !row.wrap_continuation || !options.unwrap {
            blank_cells = 0;
        }
        for cell in cells {
            if cell.width == 0 || cell.spacer_head {
                continue;
            }
            let blank = if options.emit == Format::Plain {
                cell.text.is_empty() || (options.trim && cell.text.starts_with(' '))
            } else {
                cell.text.is_empty() && cell.width == 1 && cell.style == Style::default()
            };
            if blank {
                blank_cells += 1;
                continue;
            }
            out.extend(std::iter::repeat_n(b' ', blank_cells));
            blank_cells = 0;
            if options.emit != Format::Plain && cell.style != style {
                if style != Style::default()
                    && (options.emit == Format::Html || cell.style == Style::default())
                {
                    style_close(out, options.emit);
                }
                style = cell.style;
                if style != Style::default() {
                    style_open(out, style, options.emit);
                }
            }
            if options.emit == Format::Html {
                let link = cell.hyperlink.as_ref().map(|uri| {
                    (
                        cell.hyperlink_id.as_ref(),
                        cell.hyperlink_raw.as_deref().unwrap_or(uri.as_bytes()),
                    )
                });
                if link != hyperlink {
                    if hyperlink.is_some() {
                        out.extend_from_slice(b"</a>");
                    }
                    hyperlink = link;
                    if let Some((_, uri)) = link {
                        out.extend_from_slice(b"<a href=\"");
                        for &byte in uri {
                            html_char(out, char::from(byte));
                        }
                        out.extend_from_slice(b"\">");
                    }
                }
            }
            if cell.text.is_empty() {
                out.push(b' ');
            } else if options.emit == Format::Html {
                for cp in cell.text.chars() {
                    html_char(out, cp);
                }
            } else {
                out.extend_from_slice(cell.text.as_bytes());
            }
        }
    }
    if style != Style::default() {
        style_close(out, options.emit);
    }
    if hyperlink.is_some() {
        out.extend_from_slice(b"</a>");
    }
    if options.emit == Format::Html {
        out.extend_from_slice(b"</div>");
        blank_rows = blank_rows.saturating_sub(1);
    }
    (blank_rows, blank_cells)
}

fn html_char(out: &mut Vec<u8>, cp: char) {
    out.extend_from_slice(match cp {
        '<' => b"&lt;",
        '>' => b"&gt;",
        '&' => b"&amp;",
        '"' => b"&quot;",
        '\'' => b"&#39;",
        cp if cp.is_ascii() => {
            out.push(cp as u8);
            return;
        }
        cp => {
            out.extend_from_slice(format!("&#{};", u32::from(cp)).as_bytes());
            return;
        }
    });
}

fn style_close(out: &mut Vec<u8>, emit: Format) {
    out.extend_from_slice(match emit {
        Format::Plain => b"",
        Format::Vt => b"\x1b[0m",
        Format::Html => b"</div>",
    });
}

fn style_open(out: &mut Vec<u8>, style: Style, emit: Format) {
    let underline = match style.underline {
        Underline::None => 0,
        Underline::Single => 1,
        Underline::Double => 2,
        Underline::Curly => 3,
        Underline::Dotted => 4,
        Underline::Dashed => 5,
    };
    if emit == Format::Vt {
        out.extend_from_slice(b"\x1b[0m");
        for (enabled, code) in [
            (style.bold, 1),
            (style.faint, 2),
            (style.italic, 3),
            (style.blink, 5),
            (style.inverse, 7),
            (style.invisible, 8),
            (style.strikethrough, 9),
            (style.overline, 53),
        ] {
            if enabled {
                out.extend_from_slice(format!("\x1b[{code}m").as_bytes());
            }
        }
        if underline == 1 {
            out.extend_from_slice(b"\x1b[4m");
        } else if underline > 1 {
            out.extend_from_slice(format!("\x1b[4:{underline}m").as_bytes());
        }
        for (prefix, color) in [
            (38, style.foreground),
            (48, style.background),
            (58, style.underline_color),
        ] {
            let sequence = match color {
                Color::Default => continue,
                Color::Indexed(index) => format!("\x1b[{prefix};5;{index}m"),
                Color::Rgb(r, g, b) => format!("\x1b[{prefix};2;{r};{g};{b}m"),
            };
            out.extend_from_slice(sequence.as_bytes());
        }
    } else if emit == Format::Html {
        out.extend_from_slice(b"<div style=\"display: inline;");
        for (property, color) in [
            ("color", style.foreground),
            ("background-color", style.background),
            ("text-decoration-color", style.underline_color),
        ] {
            let value = match color {
                Color::Default => continue,
                Color::Indexed(index) => format!("{property}: var(--vt-palette-{index});"),
                Color::Rgb(r, g, b) => format!("{property}: rgb({r}, {g}, {b});"),
            };
            out.extend_from_slice(value.as_bytes());
        }
        if underline != 0 || style.strikethrough || style.overline || style.blink {
            out.extend_from_slice(b"text-decoration-line:");
            for (enabled, text) in [
                (underline != 0, " underline"),
                (style.strikethrough, " line-through"),
                (style.overline, " overline"),
                (style.blink, " blink"),
            ] {
                if enabled {
                    out.extend_from_slice(text.as_bytes());
                }
            }
            out.push(b';');
        }
        if underline != 0 {
            out.extend_from_slice(
                format!(
                    "text-decoration-style: {};",
                    ["", "solid", "double", "wavy", "dotted", "dashed"][underline]
                )
                .as_bytes(),
            );
        }
        for (enabled, text) in [
            (style.bold, "font-weight: bold;"),
            (style.italic, "font-style: italic;"),
            (style.faint, "opacity: 0.5;"),
            (style.invisible, "visibility: hidden;"),
            (style.inverse, "filter: invert(100%);"),
        ] {
            if enabled {
                out.extend_from_slice(text.as_bytes());
            }
        }
        out.extend_from_slice(b"\">");
    }
}
