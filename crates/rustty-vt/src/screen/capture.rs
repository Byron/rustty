use super::*;

impl Screen {
    pub(super) fn tmux_note_rendition(&mut self, y: usize, unicode: bool) {
        let style = self.cursor.style;
        let hyperlink = self.cursor.hyperlink.is_some();
        let extended = unicode
            || hyperlink
            || style.strikethrough
            || style.overline
            || !matches!(style.underline, Underline::None | Underline::Single)
            || matches!(style.foreground, Color::Rgb(..))
            || matches!(style.background, Color::Rgb(..))
            || style.underline_color != Color::Default;
        if extended {
            let id = self.row(y).id;
            *self.tmux_row_flags.entry(id).or_default() |= 2 | if hyperlink { 0x100 } else { 0 };
        }
    }

    /// C retains allocation and hyperlink flags until the complete row is cleared.
    pub fn tmux_line_flags(&self, row: Row<'_>) -> u16 {
        let mut flags =
            self.tmux_row_flags.get(&row.id).copied().unwrap_or(0) | u16::from(row.wrapped);
        for col in 0..row.cells.len() {
            let style = row.style(col);
            if row.text(col).len() > 1
                || style.strikethrough
                || style.overline
                || !matches!(style.underline, Underline::None | Underline::Single)
                || matches!(style.foreground, Color::Rgb(..))
                || matches!(style.background, Color::Rgb(..))
                || style.underline_color != Color::Default
            {
                flags |= 2;
            }
            if row.hyperlink(col).is_some() {
                flags |= 0x102;
            }
        }
        if let Some(markers) = self.tmux_shell.as_ref().and_then(|m| m.get(&row.id)) {
            if let Some((_, kinds)) = markers.prompt {
                flags |= u16::from(kinds) << 3;
            }
            if markers.command.is_some() {
                flags |= 0x20;
            }
            if markers.start.is_some() {
                flags |= 0x40;
            }
            if markers.end.is_some() {
                flags |= 0x80;
            }
        }
        flags
    }
}
