//! AccessKit text runs use grapheme boundaries from the terminal cells.
use egui::accesskit::{self as ak, Node, NodeId, TextPosition, TextSelection};
use rustty::vt::{GridPoint, Screen, Selection};

struct Row {
    id: NodeId,
    row: u64,
    columns: Vec<usize>,
    width: usize,
    node: Node,
}

pub struct TerminalText {
    pub id: egui::Id,
    rows: Vec<Row>,
    selection: Option<TextSelection>,
}

impl TerminalText {
    pub fn new(pane: u64, screen: &Screen, origin: [f32; 2], cell: [f32; 2]) -> Self {
        let id = egui::Id::new(("terminal", pane));
        let rows = screen
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let node_id = id.with(row.id).accesskit_id();
                let mut node = Node::new(ak::Role::TextRun);
                let mut value = String::new();
                let mut lengths = Vec::new();
                let mut positions = Vec::new();
                let mut widths = Vec::new();
                let mut columns = Vec::new();
                for (col, content) in row.cells.iter().enumerate() {
                    if content.width == 0 || content.spacer_head {
                        continue;
                    }
                    let text = if content.text.is_empty() {
                        " "
                    } else {
                        &content.text
                    };
                    // AccessKit uses u8 byte lengths. Exceptionally long graphemes
                    // need several units, all of which still select the same cell.
                    let mut remaining = text;
                    let mut first = true;
                    while !remaining.is_empty() {
                        let mut end = remaining.len().min(255);
                        while !remaining.is_char_boundary(end) {
                            end -= 1;
                        }
                        value.push_str(&remaining[..end]);
                        lengths.push(end as u8);
                        columns.push(col);
                        positions.push(col as f32 * cell[0]);
                        widths.push(if first {
                            f32::from(content.width) * cell[0]
                        } else {
                            0.0
                        });
                        remaining = &remaining[end..];
                        first = false;
                    }
                }
                if !row.wrapped && index + 1 < screen.rows.len() {
                    value.push('\n');
                    lengths.push(1);
                    columns.push(row.cells.len());
                    positions.push(row.cells.len() as f32 * cell[0]);
                    widths.push(0.0);
                }
                node.set_value(value);
                node.set_character_lengths(lengths);
                node.set_character_positions(positions);
                node.set_character_widths(widths);
                node.set_text_direction(ak::TextDirection::LeftToRight);
                let y = origin[1] + index as f32 * cell[1];
                node.set_bounds(ak::Rect {
                    x0: origin[0].into(),
                    y0: y.into(),
                    x1: (origin[0] + row.cells.len() as f32 * cell[0]).into(),
                    y1: (y + cell[1]).into(),
                });
                Row {
                    id: node_id,
                    row: row.id,
                    columns,
                    width: row.cells.len(),
                    node,
                }
            })
            .collect();
        let mut result = Self {
            id,
            rows,
            selection: None,
        };
        if let Some(selection) = screen.selection {
            let order = |point: GridPoint| {
                (
                    result.rows.iter().position(|row| row.row == point.row),
                    point.col,
                )
            };
            let reversed = order(selection.start) > order(selection.end);
            let start = result.position(selection.start, reversed);
            let end = result.position(selection.end, !reversed);
            result.selection = start
                .zip(end)
                .map(|(anchor, focus)| TextSelection { anchor, focus });
        } else if screen.cursor.visible {
            let point = GridPoint {
                row: screen.rows[screen.cursor.row].id,
                col: screen.cursor.col,
            };
            result.selection = result.position(point, false).map(|pos| TextSelection {
                anchor: pos,
                focus: pos,
            });
        }
        result
    }

    fn position(&self, point: GridPoint, after: bool) -> Option<TextPosition> {
        let row = self.rows.iter().find(|row| row.row == point.row)?;
        let index = row.columns.partition_point(|&col| {
            if after {
                col <= point.col
            } else {
                col < point.col
            }
        });
        Some(TextPosition {
            node: row.id,
            character_index: index,
        })
    }

    pub fn contains(&self, node: NodeId) -> bool {
        self.id.accesskit_id() == node || self.rows.iter().any(|row| row.id == node)
    }

    /// Invalid or stale native positions must not select unrelated terminal cells.
    pub fn selection(&self, range: TextSelection) -> Option<Option<Selection>> {
        let boundary = |pos: TextPosition| {
            let index = self.rows.iter().position(|row| row.id == pos.node)?;
            let row = &self.rows[index];
            if pos.character_index > row.columns.len() {
                return None;
            }
            Some((
                index,
                row.columns
                    .get(pos.character_index)
                    .copied()
                    .unwrap_or(row.width),
            ))
        };
        let anchor = boundary(range.anchor)?;
        let focus = boundary(range.focus)?;
        if anchor == focus {
            return Some(None);
        }
        let (first, mut last) = if anchor < focus {
            (anchor, focus)
        } else {
            (focus, anchor)
        };
        if last.1 == 0 {
            last.0 = last.0.checked_sub(1)?;
            last.1 = self.rows[last.0].width;
        }
        let start = GridPoint {
            row: self.rows[first.0].row,
            col: first.1,
        };
        let end = GridPoint {
            row: self.rows[last.0].row,
            col: last.1.saturating_sub(1),
        };
        Some(Some(Selection {
            start,
            end,
            rectangular: false,
        }))
    }

    /// Append children after egui has built the widget tree, avoiding a second
    /// parent for native text nodes that have no corresponding egui widget.
    pub fn append_to(&self, update: &mut ak::TreeUpdate) {
        let Some((_, parent)) = update
            .nodes
            .iter_mut()
            .find(|(id, _)| *id == self.id.accesskit_id())
        else {
            return;
        };
        parent.set_role(ak::Role::Terminal);
        parent.clear_value();
        parent.set_children(self.rows.iter().map(|row| row.id).collect::<Vec<_>>());
        parent.add_action(ak::Action::SetTextSelection);
        if let Some(selection) = self.selection {
            parent.set_text_selection(selection);
        }
        update
            .nodes
            .extend(self.rows.iter().map(|row| (row.id, row.node.clone())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessible_selection_preserves_wide_graphemes_and_rejects_stale_positions() {
        let mut terminal = rustty::vt::Terminal::new(12, 2, 0);
        terminal.feed("A界e\u{301}B".as_bytes());
        let text = TerminalText::new(7, terminal.screen(), [0.0; 2], [10.0, 20.0]);
        let row = &text.rows[0];
        assert_eq!(&row.node.character_lengths()[..4], &[1, 3, 3, 1]);
        assert_eq!(&row.columns[..4], &[0, 1, 3, 4]);
        let start = TextPosition {
            node: row.id,
            character_index: 1,
        };
        let end = TextPosition {
            node: row.id,
            character_index: 3,
        };
        terminal.screen_mut().selection = text
            .selection(TextSelection {
                anchor: start,
                focus: end,
            })
            .unwrap();
        assert_eq!(
            terminal.screen().selection_text().as_deref(),
            Some("界e\u{301}")
        );
        assert_eq!(
            text.selection(TextSelection {
                anchor: end,
                focus: start
            }),
            Some(terminal.screen().selection)
        );
        assert_eq!(
            text.selection(TextSelection {
                anchor: start,
                focus: start
            }),
            Some(None)
        );
        assert!(
            text.selection(TextSelection {
                anchor: start,
                focus: TextPosition {
                    character_index: 1000,
                    ..end
                }
            })
            .is_none()
        );
        assert!(
            text.selection(TextSelection {
                anchor: start,
                focus: TextPosition {
                    node: NodeId(1),
                    ..end
                }
            })
            .is_none()
        );
        let mut update = ak::TreeUpdate {
            nodes: vec![(text.id.accesskit_id(), Node::new(ak::Role::Terminal))],
            tree: None,
            tree_id: ak::TreeId::ROOT,
            focus: text.id.accesskit_id(),
        };
        text.append_to(&mut update);
        assert_eq!(update.nodes[0].1.children().len(), 2);
        assert_eq!(update.nodes[1].1.role(), ak::Role::TextRun);
    }
}
