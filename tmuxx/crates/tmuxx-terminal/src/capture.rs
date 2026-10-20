use crate::{Color, Style, Terminal};
use rustty_vt::{HyperlinkData, HyperlinkId, Row, Screen, Underline};

/// Flags accepted by the headless `capture-pane` implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct CaptureOptions {
    /// C record grammar containing native grid storage diagnostics.
    pub raw: bool,
    pub start: Option<i32>,
    pub end: Option<i32>,
    pub join: bool,
    pub escape: bool,
    pub escape_sequences: bool,
    pub alternate: bool,
    pub pending: bool,
    pub preserve_trailing: bool,
    pub trim_unused: bool,
    pub quiet: bool,
    pub number_lines: bool,
    pub hyperlinks: bool,
    pub timestamps: bool,
    pub flags: bool,
}

impl Terminal {
    /// Return bytes to store in a buffer. Printing adds a final newline when
    /// absent, including empty captures and pending input.
    pub fn capture_with(&self, options: CaptureOptions) -> Result<Vec<u8>, String> {
        if options.raw {
            return Ok(self.capture_raw());
        }
        if options.pending && !options.hyperlinks {
            let pending = self
                .inner
                .parser()
                .pending_input()
                .map_err(|e| e.to_string())?;
            return Ok(if options.escape_sequences {
                let mut escaped = Vec::new();
                for &byte in pending {
                    // The C command compares a plain char with ' '. Its target
                    // ABI is signed on Darwin/x86 GNU and unsigned on aarch64
                    // GNU, where high bytes are consequently preserved.
                    if (byte as std::ffi::c_char) >= (b' ' as std::ffi::c_char) && byte != b'\\' {
                        escaped.push(byte);
                    } else {
                        escaped.extend_from_slice(format!("\\{byte:03o}").as_bytes());
                    }
                }
                escaped
            } else {
                pending.to_vec()
            });
        }
        if options.alternate && !self.inner.is_alternate_screen() {
            return if options.quiet {
                Ok(Vec::new())
            } else {
                Err("no alternate screen".into())
            };
        }
        let primary = self.inner.primary_screen();
        let active = self.inner.screen();
        let rows: Vec<(&Screen, Row<'_>)> = if options.alternate {
            primary
                .all_rows()
                .skip(primary.history_len())
                .map(|row| (primary, row))
                .collect()
        } else if self.inner.is_alternate_screen() {
            primary
                .all_rows()
                .take(primary.history_len())
                .map(|row| (primary, row))
                .chain(active.all_rows().map(|row| (active, row)))
                .collect()
        } else {
            active.all_rows().map(|row| (active, row)).collect()
        };
        let history = if options.alternate {
            0
        } else {
            primary.history_len()
        } as i64;
        let last = rows.len() as i64 - 1;
        let mut start = (history + i64::from(options.start.unwrap_or(0))).clamp(0, last);
        let mut end =
            (history + i64::from(options.end.unwrap_or(active.height() as i32 - 1))).clamp(0, last);
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }
        let mut output = Vec::new();
        let mut previous = Style::default();
        let mut previous_link = None;
        let mut links = Vec::<&HyperlinkData>::new();
        for (index, (screen, row)) in rows
            .into_iter()
            .enumerate()
            .skip(start as usize)
            .take((end - start + 1) as usize)
        {
            if options.hyperlinks {
                let mut line = Vec::new();
                for col in 0..row.cells.len() {
                    let Some(link) = row.hyperlink(col) else {
                        continue;
                    };
                    if links.contains(&link) {
                        continue;
                    }
                    if links.len() == usize::from(self.size().0) {
                        break;
                    }
                    links.push(link);
                    if !line.is_empty() {
                        line.push(b' ');
                    }
                    line.extend_from_slice(link.uri_bytes());
                }
                if line.is_empty() {
                    continue;
                }
                prefix(&mut output, index as i64 - history, screen, row, options);
                output.extend(line);
                if !options.join || !row.wrapped {
                    output.push(b'\n');
                }
                continue;
            }
            let bounds = screen.tmux_capture_bounds(row.id);
            if bounds.is_none()
                && options.preserve_trailing
                && !options.trim_unused
                && !options.join
            {
                return Err(
                    "capture-pane -N after reflow requires tmux grid allocation metadata".into(),
                );
            }
            let used = bounds.map(|bounds| bounds.0).unwrap_or_else(|| {
                row.cells
                    .iter()
                    .enumerate()
                    .rposition(|(col, cell)| {
                        cell.codepoint().is_some()
                            || cell.width() == 0
                            || row.style(col).background != Color::Default
                    })
                    .map_or(0, |col| col + 1)
            });
            // C exposes spare allocated cells in its SGR reset transitions.
            // shortcut: erased/shrunk rows need retained allocation metadata;
            // this reproduces grid_expand_line for ordinary written rows.
            let end = if options.trim_unused || options.join {
                used
            } else if let Some((_, allocated)) = bounds {
                allocated
            } else if used == 0 {
                0
            } else if used < row.cells.len() / 4 {
                row.cells.len() / 4
            } else if used < row.cells.len() / 2 {
                row.cells.len() / 2
            } else {
                row.cells.len()
            };
            let mut line = Vec::new();
            let mut has_link = false;
            for (col, cell) in row.cells.iter().take(end).enumerate() {
                if cell.width() == 0 || cell.spacer_head() || row.style(col).tmux_tab < 0 {
                    continue;
                }
                if options.escape {
                    let style = row.style(col);
                    rendition(&mut line, previous, style, options.escape_sequences);
                    previous = style;
                    let link = row.hyperlink(col);
                    if previous_link != link {
                        if link.is_some() || has_link {
                            hyperlink(&mut line, link, options.escape_sequences);
                        }
                        has_link = link.is_some();
                        previous_link = link;
                    }
                }
                if row.style(col).tmux_tab > 0 {
                    line.push(b'\t');
                } else if cell.codepoint().is_none() {
                    line.push(b' ');
                } else if options.escape_sequences && &*row.text(col) == "\\" {
                    line.extend_from_slice(b"\\\\");
                } else {
                    line.extend_from_slice(row.text(col).as_bytes());
                }
            }
            if has_link {
                hyperlink(&mut line, None, options.escape_sequences);
            }
            if !options.join && !options.preserve_trailing {
                while line.last() == Some(&b' ') {
                    line.pop();
                }
            }
            prefix(&mut output, index as i64 - history, screen, row, options);
            output.extend(line);
            if !options.join || !row.wrapped {
                output.push(b'\n');
            }
        }
        Ok(output)
    }
}

fn prefix(output: &mut Vec<u8>, line: i64, screen: &Screen, row: Row<'_>, options: CaptureOptions) {
    if options.number_lines {
        output.extend_from_slice(format!("{line} ").as_bytes());
    }
    if options.timestamps {
        output.extend_from_slice(format!("{} ", screen.tmux_row_time(row.id)).as_bytes());
    }
    if options.flags {
        let flags = screen.tmux_line_flags(row);
        let start = output.len();
        for (mask, byte) in [
            (4, b'D'),
            (0x100, b'H'),
            (0x40, b'O'),
            (8, b'P'),
            (1, b'W'),
            (2, b'X'),
        ] {
            if flags & mask != 0 {
                output.push(byte);
            }
        }
        if output.len() == start {
            output.push(b'-');
        }
        output.push(b' ');
    }
}

fn attributes(style: Style) -> Vec<&'static str> {
    [
        (style.bold, "1"),
        (style.faint, "2"),
        (style.italic, "3"),
        (style.underline == Underline::Single, "4"),
        (style.blink, "5"),
        (style.inverse, "7"),
        (style.invisible, "8"),
        (style.strikethrough, "9"),
        (style.underline == Underline::Double, "4:2"),
        (style.underline == Underline::Curly, "4:3"),
        (style.underline == Underline::Dotted, "4:4"),
        (style.underline == Underline::Dashed, "4:5"),
        (style.overline, "5:3"),
    ]
    .into_iter()
    .filter_map(|(on, code)| on.then_some(code))
    .collect()
}

fn sgr(output: &mut Vec<u8>, codes: &str, escaped: bool) {
    output.extend_from_slice(if escaped { b"\\033[" } else { b"\x1b[" });
    output.extend_from_slice(codes.as_bytes());
    output.push(b'm');
}

fn color_code(color: Color, selector: u8, ansi: bool) -> String {
    match color {
        Color::Default if selector == 58 => String::new(),
        Color::Default => (selector + 1).to_string(),
        Color::Indexed(index) if ansi && index < 16 => {
            (selector - 8 + if index < 8 { index } else { index + 52 }).to_string()
        }
        Color::Indexed(index) => format!("{selector};5;{index}"),
        Color::Rgb(r, g, b) => format!("{selector};2;{r};{g};{b}"),
    }
}

fn rendition(output: &mut Vec<u8>, previous: Style, current: Style, escaped: bool) {
    let before = attributes(previous);
    let after = attributes(current);
    let reset = before.iter().any(|attr| !after.contains(attr))
        || (previous.underline_color != Color::Default
            && current.underline_color == Color::Default);
    let mut changed = if reset { vec!["0"] } else { Vec::new() };
    changed.extend(
        after
            .into_iter()
            .filter(|attr| reset || !before.contains(attr)),
    );
    if !changed.is_empty() {
        sgr(output, &changed.join(";"), escaped);
    }
    for (old, new, selector, bit) in [
        (previous.foreground, current.foreground, 38, 1),
        (previous.background, current.background, 48, 2),
        (previous.underline_color, current.underline_color, 58, 0),
    ] {
        let old = color_code(old, selector, previous.tmux_ansi & bit != 0);
        let new = color_code(new, selector, current.tmux_ansi & bit != 0);
        if !new.is_empty()
            && (reset || old != new)
            && !(reset && matches!(new.as_str(), "39" | "49"))
        {
            sgr(output, &new, escaped);
        }
    }
}

fn hyperlink(output: &mut Vec<u8>, link: Option<&HyperlinkData>, escaped: bool) {
    output.extend_from_slice(if escaped { b"\\033]8;" } else { b"\x1b]8;" });
    if let Some(HyperlinkId::Explicit(id)) = link.and_then(|link| link.id.as_ref()) {
        output.extend_from_slice(b"id=");
        output.extend_from_slice(id);
    }
    output.push(b';');
    if let Some(link) = link {
        output.extend_from_slice(link.uri_bytes());
    }
    output.extend_from_slice(if escaped { b"\\033\\\\" } else { b"\x1b\\" });
}
