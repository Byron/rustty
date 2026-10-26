use super::*;
use crate::graphics::{self, Graphics};
use std::io;

impl Screen {
    /// Give imported rows fresh local identities before resize or new output.
    pub(crate) fn reset_visual_row_ids(&mut self) {
        let mut rows = HashMap::new();
        self.next_row = 0;
        for page in &mut self.pages.pages {
            for id in &mut page.row_ids[..usize::from(page.rows)] {
                rows.insert(*id, self.next_row);
                *id = self.next_row;
                self.next_row += 1;
            }
        }
        self.remap_grid_rows(&rows);
        let placements = std::mem::take(&mut self.graphics.placements);
        for mut placement in placements.iter().cloned() {
            if let Some(&row) = rows.get(&placement.row) {
                placement.row = row;
            }
            self.graphics.placements.insert(placement);
        }
    }

    /// Copy display cells into a screen with no live input, cursor, or owner state.
    pub(crate) fn visual_copy(
        &self,
        foreground: [u8; 3],
        background: [u8; 3],
        palette: &[[u8; 3]],
    ) -> io::Result<Self> {
        let mut source = self.clone();
        source.release_cursor_style();
        source.end_hyperlink();
        let mut result = Self::new(self.columns, self.height, ScrollbackLimits::default());
        result.pages = source.pages;
        result.next_row = source.next_row;
        result.viewport_pin = None;
        result.graphics = Graphics::from_view_snapshot_with_limit(
            self.graphics.visual_snapshot(),
            1024 * 1024 * 1024,
        )
        .map_err(io::Error::other)?;
        // Work one page at a time so recoloring long histories stays linear.
        for page in std::mem::take(&mut result.pages).pages {
            let mut part = Self::new(self.columns, usize::from(page.rows), Default::default());
            part.pages = PageList::default();
            part.pages.push(page);
            for absolute in 0..part.all_rows().len() {
                let row = part.physical_row(absolute);
                let styles: Vec<_> = (0..row.cells.len())
                    .map(|col| {
                        let mut style = row.style(col);
                        style.background = resolved(style.background, background, palette);
                        // Placeholder colors encode IDs, rather than display colors.
                        if row.cells[col].codepoint() != Some(graphics::unicode::PLACEHOLDER) {
                            style.foreground = resolved(style.foreground, foreground, palette);
                            if style.underline_color != Color::Default {
                                style.underline_color =
                                    resolved(style.underline_color, foreground, palette);
                            }
                        }
                        style.tmux_ansi = 0;
                        style.tmux_tab = 0;
                        style
                    })
                    .collect();
                for (col, style) in styles.into_iter().enumerate() {
                    part.set_absolute_cell_style(absolute, col, style);
                }
            }
            for page in part.pages.pages {
                result.pages.push(page);
            }
        }
        result.cursor.row = result.height - 1;
        result.sync_cursor_resources();
        Ok(result)
    }

    /// Prepend imported cells while retaining live row IDs, parser anchors and cursor resources.
    pub(crate) fn prepend_visual(&mut self, mut history: Self, keep: usize) -> io::Result<()> {
        let cursor = self.history_len() + self.cursor.row;
        let end = self
            .all_rows()
            .enumerate()
            .filter_map(|(index, row)| {
                let has_image = self.graphics.placements.iter().any(|p| p.row == row.id);
                (index <= cursor || !self.row_text(row).trim().is_empty() || has_image)
                    .then_some(index + 1)
            })
            .next_back()
            .unwrap_or(cursor + 1);
        let shift = self.pages.total_rows() - end;
        let removed = self.all_rows().skip(end).map(|row| row.id).collect();
        self.pages.truncate(end);
        self.discard_ids(removed);
        let removed = history.all_rows().skip(keep).map(|row| row.id).collect();
        if keep == 0 {
            history.pages = Default::default();
        } else {
            history.pages.truncate(keep);
        }
        history.discard_ids(removed);
        let padding = self.height.saturating_sub(keep + end);
        if padding > 0 {
            let mut blank = Self::new(self.columns, padding, Default::default());
            blank.append_visual(history)?;
            history = blank;
        }
        let added = history.pages.total_rows();
        // append_visual remaps only imported identities; rotating pages keeps live anchors valid.
        let pages = self.pages.pages.len();
        self.append_visual(history)?;
        self.pages.pages.rotate_left(pages);
        self.cursor.row = cursor + added - self.history_len();
        if let Some(saved) = &mut self.saved_cursor {
            saved.cursor.row = (saved.cursor.row + shift).min(self.height - 1);
        }
        self.sync_cursor_resources();
        Ok(())
    }

    /// Append another screen without interpreting its cells as terminal input.
    pub(crate) fn append_visual(&mut self, mut source: Self) -> io::Result<()> {
        let mut image_ids = HashMap::new();
        let mut used: HashSet<_> = self.graphics.images.keys().copied().collect();
        for id in source.graphics.images.keys().copied() {
            let mut replacement = id;
            while used.contains(&replacement) {
                // Preserve the high byte encoded in a placeholder's diacritics.
                replacement = (id & 0xff00_0000) | (replacement.wrapping_add(1) & 0x00ff_ffff);
                if replacement == id {
                    return Err(io::Error::other("visual image IDs exhausted"));
                }
            }
            used.insert(replacement);
            image_ids.insert(id, replacement);
        }
        for absolute in 0..source.all_rows().len() {
            let row = source.physical_row(absolute);
            let runs: Vec<_> = graphics::unicode::placements(&source, row).collect();
            for run in runs {
                let Some(&replacement) = image_ids.get(&run.image_id) else {
                    continue;
                };
                let [_, r, g, b] = replacement.to_be_bytes();
                for col in run.col..run.col + run.width as usize {
                    let mut style = source.physical_row(absolute).style(col);
                    style.foreground = Color::Rgb(r, g, b);
                    source.set_absolute_cell_style(absolute, col, style);
                }
            }
        }
        let mut row_ids = HashMap::new();
        for mut page in source.pages.pages {
            for id in &mut page.row_ids[..usize::from(page.rows)] {
                let new = self
                    .next_row
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("visual row identities exhausted"))?;
                row_ids.insert(*id, self.next_row);
                *id = self.next_row;
                self.next_row = new;
            }
            self.pages.push(page);
        }
        for (id, mut image) in source.graphics.images.drain() {
            image.id = image_ids[&id];
            self.graphics.images.insert(image.id, image);
        }
        for mut placement in source.graphics.placements.iter().cloned() {
            placement.image_id = image_ids[&placement.image_id];
            if let Some((image, _)) = &mut placement.parent {
                *image = image_ids[image];
            }
            if let Some(&row) = row_ids.get(&placement.row) {
                placement.row = row;
            }
            self.graphics.placements.insert(placement);
        }
        self.graphics.generation = self.graphics.generation.wrapping_add(1);
        Ok(())
    }
}

fn resolved(color: Color, default: [u8; 3], palette: &[[u8; 3]]) -> Color {
    let [r, g, b] = match color {
        Color::Default => default,
        Color::Indexed(index) => palette[usize::from(index)],
        Color::Rgb(r, g, b) => [r, g, b],
    };
    Color::Rgb(r, g, b)
}
