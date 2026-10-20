use super::*;

impl Screen {
    pub(super) fn tmux_note_rendition(&mut self, y: usize, unicode: bool) {
        if !self.tmux_capture_enabled {
            self.tmux_capture_known = false;
            return;
        }
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

#[cfg(test)]
mod tests {
    use crate::{Screen, Terminal, snapshot};

    #[test]
    fn tmux_capture_survives_screen_switches_reset_and_checkpoint_restore() {
        let mut terminal = Terminal::new(8, 2, 0);
        terminal.set_tmux_strings(true);
        for transition in [b"".as_slice(), b"\x1b[?1049h", b"\x1b[?1049l", b"\x1bc"] {
            terminal.feed(transition);
            terminal.feed("\r\x1b[2Ké\rx ".as_bytes());
            let primary: Screen =
                serde_json::from_value(serde_json::to_value(terminal.primary_screen()).unwrap())
                    .unwrap();
            let alternate: Option<Screen> = terminal.alternate_screen().map(|screen| {
                serde_json::from_value(serde_json::to_value(screen).unwrap()).unwrap()
            });
            terminal = snapshot::decode_exact(
                snapshot::encode_to_vec(&terminal).unwrap().as_slice(),
                snapshot::DecodeOptions {
                    tmux_strings: true,
                    ..Default::default()
                },
            )
            .unwrap();
            terminal
                .restore_tmux_capture_metadata(&primary, alternate.as_ref())
                .unwrap();
            terminal.feed(b"z");
            let screen = terminal.screen();
            let row = screen.row(0);
            assert_eq!(screen.row_text(row), "x z");
            assert_eq!(screen.tmux_capture_bounds(row.id), Some((3, 4)));
            assert_eq!(screen.tmux_line_flags(row), 2);
        }
    }

    #[test]
    fn enabling_tmux_capture_does_not_invent_untracked_bounds() {
        let mut terminal = Terminal::new(8, 2, 0);
        terminal.feed(b"first");
        terminal = snapshot::decode_exact(
            snapshot::encode_to_vec(&terminal).unwrap().as_slice(),
            snapshot::DecodeOptions::default(),
        )
        .unwrap();
        terminal.feed(b" ");
        terminal.set_tmux_strings(true);
        terminal.feed(b"x");
        let screen = terminal.screen();
        assert_eq!(screen.tmux_capture_bounds(screen.row(0).id), None);
        terminal.reset();
        terminal.feed(b"x");
        let screen = terminal.screen();
        assert_eq!(screen.tmux_capture_bounds(screen.row(0).id), Some((1, 2)));
        terminal.set_tmux_strings(false);
        terminal.feed(b" ");
        terminal.set_tmux_strings(true);
        terminal.feed(b"y");
        let screen = terminal.screen();
        assert_eq!(screen.tmux_capture_bounds(screen.row(0).id), None);
    }
}
