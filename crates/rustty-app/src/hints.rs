//! Transient keyboard targets from one visible terminal viewport.
use crate::workspace::Id;
use egui::{Color32, Pos2, Rect, Vec2};
use rustty::vt::{self, search::LinkHint};
use std::{collections::HashMap, path::PathBuf};

const ALPHABET: &[u8] = b"asdfghjkl";

pub struct Source {
    pub generation: u64,
    pub identity: u64,
    pub dimensions: [usize; 2],
    pub viewport_offset: usize,
    rows: Vec<(u64, u64)>,
}
impl Source {
    pub fn new(terminal: &vt::Terminal) -> Self {
        let screen = terminal.screen();
        Self {
            generation: terminal.generation,
            identity: screen.render_identity(),
            dimensions: [usize::from(terminal.cols), usize::from(terminal.rows)],
            viewport_offset: screen.viewport_offset,
            rows: visible_rows(screen).collect(),
        }
    }
    pub fn matches(&self, terminal: &vt::Terminal) -> bool {
        let screen = terminal.screen();
        self.generation == terminal.generation
            && self.identity == screen.render_identity()
            && self.dimensions == [usize::from(terminal.cols), usize::from(terminal.rows)]
            && self.viewport_offset == screen.viewport_offset
            && self.matches_rows(screen)
    }
    pub fn matches_rows(&self, screen: &vt::Screen) -> bool {
        self.rows.iter().copied().eq(visible_rows(screen))
    }
}
fn visible_rows(screen: &vt::Screen) -> impl Iterator<Item = (u64, u64)> + '_ {
    let top = screen.history_len().saturating_sub(screen.viewport_offset);
    (top..top + screen.height()).map(|index| {
        let row = screen.physical_row(index);
        (row.id, row.revision())
    })
}

pub struct Entry {
    pub label: String,
    pub target: LinkHint,
    pub bounds: Vec<Rect>,
}
pub struct Hints {
    pub pane: Id,
    pub source: Source,
    pub rect: Rect,
    pub cell: Vec2,
    pub cwd: PathBuf,
    pub copy: bool,
    pub entries: Vec<Entry>,
    prefix: String,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Result {
    Pending,
    Cancel,
    Select(usize),
}
impl Hints {
    pub fn new(
        pane: Id,
        terminal: &vt::Terminal,
        targets: Vec<LinkHint>,
        rect: Rect,
        cell: Vec2,
        cwd: PathBuf,
        copy: bool,
    ) -> Option<Self> {
        if targets.is_empty() {
            return None;
        }
        let mut width = 1;
        let mut capacity = ALPHABET.len();
        while capacity < targets.len() {
            capacity = capacity.saturating_mul(ALPHABET.len());
            width += 1;
        }
        let screen = terminal.screen();
        let top = screen.history_len().saturating_sub(screen.viewport_offset);
        let rows: Vec<_> = (top..top + screen.height())
            .map(|index| screen.physical_row(index))
            .collect();
        let positions: HashMap<_, _> = rows
            .iter()
            .enumerate()
            .map(|(y, row)| (row.id, y))
            .collect();
        let entries = targets
            .into_iter()
            .enumerate()
            .map(|(mut index, target)| {
                let mut label = vec![ALPHABET[0]; width];
                for digit in label.iter_mut().rev() {
                    *digit = ALPHABET[index % ALPHABET.len()];
                    index /= ALPHABET.len();
                }
                let start = positions[&target.link.start.row];
                let end = positions[&target.link.end.row];
                let bounds = (start..=end)
                    .map(|y| {
                        let left = if y == start { target.link.start.col } else { 0 };
                        let right = if y == end {
                            target.link.end.col
                                + rows[y]
                                    .cells
                                    .get(target.link.end.col)
                                    .map_or(1, |cell| usize::from(cell.width().max(1)))
                        } else {
                            rows[y].cells.len()
                        };
                        Rect::from_min_size(
                            rect.min + Vec2::new(left as f32 * cell.x, y as f32 * cell.y),
                            Vec2::new(right.saturating_sub(left) as f32 * cell.x, cell.y),
                        )
                        .intersect(rect)
                    })
                    .collect();
                Entry {
                    label: String::from_utf8(label).unwrap(),
                    target,
                    bounds,
                }
            })
            .collect();
        Some(Self {
            pane,
            source: Source::new(terminal),
            rect,
            cell,
            cwd,
            copy,
            entries,
            prefix: String::new(),
        })
    }
    pub fn text(&mut self, text: &str) -> Result {
        let text = text.to_ascii_lowercase();
        if text.is_empty() || !text.bytes().all(|byte| ALPHABET.contains(&byte)) {
            return Result::Pending;
        }
        let previous = self.prefix.len();
        self.prefix.push_str(&text);
        let matching = self
            .entries
            .iter()
            .position(|entry| entry.label.starts_with(&self.prefix));
        match matching {
            Some(index) if self.entries[index].label == self.prefix => Result::Select(index),
            Some(_) => Result::Pending,
            None => {
                self.prefix.truncate(previous);
                Result::Pending
            }
        }
    }
    pub fn backspace(&mut self) {
        self.prefix.pop();
    }
    pub fn paint(&self, ui: &egui::Ui) {
        let painter = ui
            .painter()
            .with_clip_rect(self.rect.intersect(ui.clip_rect()));
        let background = Color32::from_rgb(247, 208, 93);
        let foreground = Color32::from_rgb(25, 25, 25);
        let text = format!(
            "{} link: {}{}  ·  Backspace edits  ·  Esc cancels",
            if self.copy { "Copy" } else { "Open" },
            self.prefix.to_uppercase(),
            "_".repeat(self.entries[0].label.len() - self.prefix.len())
        );
        let galley = painter.layout_no_wrap(
            text.clone(),
            egui::FontId::proportional(12.0),
            Color32::WHITE,
        );
        let size = galley.size() + Vec2::splat(8.0);
        let badge = Rect::from_min_size(
            Pos2::new(
                (self.rect.right() - size.x).max(self.rect.left()),
                self.rect.bottom() - size.y,
            ),
            size,
        )
        .intersect(self.rect);
        painter.rect_filled(badge, 3.0, Color32::from_black_alpha(235));
        painter.galley(badge.min + Vec2::splat(4.0), galley, Color32::WHITE);
        ui.interact(
            badge,
            egui::Id::new(("link-hint-mode", self.pane)),
            egui::Sense::hover(),
        )
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &text));
        for (index, entry) in self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.label.starts_with(&self.prefix))
        {
            for bounds in &entry.bounds {
                painter.hline(
                    bounds.left()..=bounds.right(),
                    bounds.bottom() - 1.0,
                    (1.0, background),
                );
            }
            let Some(bounds) = entry.bounds.first() else {
                continue;
            };
            let galley = painter.layout_no_wrap(
                entry.label.to_uppercase(),
                egui::FontId::monospace(13.0),
                foreground,
            );
            let size = galley.size() + Vec2::new(6.0, 2.0);
            let origin = Pos2::new(
                bounds
                    .left()
                    .min(self.rect.right() - size.x)
                    .max(self.rect.left()),
                bounds.top(),
            );
            let badge = Rect::from_min_size(origin, size).intersect(self.rect);
            painter.rect_filled(badge, 2.0, background);
            painter.galley(origin + Vec2::new(3.0, 1.0), galley, foreground);
            ui.interact(
                badge,
                egui::Id::new(("link-hint", self.pane, index)),
                egui::Sense::hover(),
            )
            .widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Label,
                    true,
                    format!(
                        "Link hint {}: {}",
                        entry.label.to_uppercase(),
                        entry.target.link.uri
                    ),
                )
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(count: usize) -> Hints {
        let mut terminal = vt::Terminal::new(80, 24, 0);
        terminal.feed(b"https://example.com");
        let target = vt::search::LinkMatcher::default()
            .viewport_hints(terminal.screen())
            .remove(0);
        Hints::new(
            1,
            &terminal,
            vec![target; count],
            Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 480.0)),
            Vec2::new(10.0, 20.0),
            PathBuf::new(),
            false,
        )
        .unwrap()
    }
    #[test]
    fn fixed_width_labels_do_not_select_prefixes_and_allow_correction() {
        let mut hints = fixture(82);
        assert!(hints.entries.iter().all(|entry| entry.label.len() == 3));
        assert_eq!(hints.text("a"), Result::Pending);
        assert_eq!(hints.text("z"), Result::Pending);
        assert_eq!(hints.text("l"), Result::Pending);
        hints.backspace();
        assert_eq!(hints.text("SD"), Result::Select(11));
        let labels: std::collections::HashSet<_> =
            hints.entries.iter().map(|entry| &entry.label).collect();
        assert_eq!(labels.len(), hints.entries.len());
    }
    #[test]
    fn source_rejects_mutations_replacements_and_scrolling_but_preserves_selection() {
        let mut terminal = vt::Terminal::new(20, 3, 100);
        terminal.feed(b"https://example.com\r\n\r\n\r\n");
        let source = Source::new(&terminal);
        let point = vt::GridPoint {
            row: terminal.screen().row(0).id,
            col: 0,
        };
        terminal.screen_mut().selection = Some(vt::Selection {
            start: point,
            end: point,
            rectangular: false,
        });
        assert!(source.matches(&terminal));
        terminal.screen_mut().viewport_offset = 1;
        assert!(!source.matches(&terminal));
        terminal.screen_mut().viewport_offset = 0;
        terminal.screen_mut().set_cell_text(0, 0, "x");
        assert!(!source.matches(&terminal));
        let source = Source::new(&terminal);
        terminal = terminal.clone();
        assert!(!source.matches(&terminal));
    }
}
