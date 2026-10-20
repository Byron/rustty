use super::*;

impl Screen {
    /// C can split oversized or fractured tabs into rows with cells beyond the
    /// declared pane width. Keep native text visible by expanding those tabs.
    pub(super) fn expand_unreflowable_tabs(&mut self, width: usize) {
        let mut changes = Vec::new();
        for (absolute, row) in self.all_rows().enumerate() {
            let mut col = 0;
            while col < row.cells.len() {
                let style = row.style(col);
                if style.tmux_tab > 0 {
                    let span = style.tmux_tab as usize;
                    if span <= width
                        && col + span <= row.cells.len()
                        && (col + 1..col + span).all(|tail| row.style(tail).tmux_tab == -1)
                    {
                        col += span;
                        continue;
                    }
                }
                if style.tmux_tab != 0 {
                    changes.push((
                        absolute,
                        col,
                        Style {
                            tmux_tab: 0,
                            ..style
                        },
                    ));
                }
                col += 1;
            }
        }
        for (absolute, col, style) in changes {
            self.set_absolute_cell_style(absolute, col, style);
        }
    }

    pub(crate) fn retain_tab(&mut self, start: usize, end: usize) {
        if end - start > 32 {
            return;
        }
        let y = self.cursor.row;
        let row = self.row(y);
        let mut first = row.style(start);
        // C's visible-cell comparison does not include underline colour.
        first.underline_color = Color::Default;
        if (start..end).any(|col| {
            let mut style = row.style(col);
            style.underline_color = Color::Default;
            row.cells[col].width() != 1
                || row.cells[col].has_grapheme()
                || !matches!(row.cells[col].codepoint(), None | Some(' '))
                || style.tmux_tab != 0
                || style != first
                || row.hyperlink(col) != row.hyperlink(start)
        }) {
            return;
        }
        let first = row.style(start);
        for col in start..end {
            let mut style = first;
            style.tmux_tab = if col == start {
                (end - start) as i8
            } else {
                -1
            };
            self.set_cell_style(y, col, style);
        }
        self.tmux_capture_write(y, end);
        *self.tmux_row_flags.entry(self.row(y).id).or_default() |= 2;
    }

    /// A printable write breaks the touched tab, retaining its background.
    /// Erase and shift commands instead clear/move only their physical cells,
    /// like C; fragments are meaningful and must survive captures and reflow.
    pub(super) fn clear_written_tabs(&mut self, y: usize, start: usize, end: usize) -> bool {
        let row = self.row(y);
        let mut columns = Vec::new();
        for col in start..end.min(row.cells.len()) {
            if row.style(col).tmux_tab == 0 {
                continue;
            }
            let mut left = col;
            while left > 0 && row.style(left).tmux_tab < 0 {
                left -= 1;
            }
            if row.style(left).tmux_tab == 0 {
                left += 1;
            }
            let mut right = col + 1;
            while right < row.cells.len() && row.style(right).tmux_tab < 0 {
                right += 1;
            }
            for x in left..right {
                let style = row.style(x);
                columns.push((x, style.background, style.tmux_ansi & 2));
            }
        }
        if columns.is_empty() {
            return false;
        }
        columns.sort_unstable_by_key(|&(col, _, _)| col);
        columns.dedup_by_key(|value| value.0);
        for (col, background, ansi) in columns {
            let (index, row) = self.locate(self.history_len() + y);
            self.pages.pages[index].erase(row, col, col + 1, background, false);
            if ansi != 0 {
                self.set_cell_style(
                    y,
                    col,
                    Style {
                        background,
                        tmux_ansi: ansi,
                        ..Style::default()
                    },
                );
            }
        }
        true
    }
}
