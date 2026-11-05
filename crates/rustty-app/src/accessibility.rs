//! AccessKit text runs preserve the terminal's atomic text spans.
use egui::accesskit::{self as ak, Node, NodeId, TextPosition, TextSelection};
use rustty::vt::{GridPoint, Screen, Selection};

struct Row {
    id: NodeId,
    row: u64,
    columns: Vec<usize>,
    /// Only internal AccessKit chunk boundaries need rounding to a span's end.
    chunk_ends: Vec<(usize, usize)>,
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
            .rows()
            .enumerate()
            .map(|(index, row)| {
                let node_id = id.with(row.id).accesskit_id();
                let mut node = Node::new(ak::Role::TextRun);
                let mut value = String::new();
                let mut lengths = Vec::new();
                let mut positions = Vec::new();
                let mut widths = Vec::new();
                let mut columns = Vec::new();
                let mut chunk_ends = Vec::new();
                for (col, content) in row.cells.iter().enumerate() {
                    let span = row.cell_span(col);
                    if span.start != col || content.spacer_head() {
                        continue;
                    }
                    let content_text = screen.cell_text(row, col);
                    let text = if content_text.is_empty() {
                        " "
                    } else {
                        &content_text
                    };
                    // AccessKit uses u8 byte lengths. Exceptionally long cell text
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
                        if !first {
                            chunk_ends.push((columns.len(), col + usize::from(span.width)));
                        }
                        columns.push(col);
                        positions.push(col as f32 * cell[0]);
                        widths.push(if first {
                            f32::from(span.width) * cell[0]
                        } else {
                            0.0
                        });
                        remaining = &remaining[end..];
                        first = false;
                    }
                }
                if !row.wrapped && index + 1 < screen.height() {
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
                    chunk_ends,
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
            let start = result.position(screen, selection.start, reversed);
            let end = result.position(screen, selection.end, !reversed);
            result.selection = start
                .zip(end)
                .map(|(anchor, focus)| TextSelection { anchor, focus });
        } else if screen.cursor.visible {
            let point = GridPoint {
                row: screen.row(screen.cursor.row).id,
                col: screen.cursor.col,
            };
            result.selection = result
                .position(screen, point, false)
                .map(|pos| TextSelection {
                    anchor: pos,
                    focus: pos,
                });
        }
        result
    }

    fn position(&self, screen: &Screen, mut point: GridPoint, after: bool) -> Option<TextPosition> {
        let source = screen.rows().find(|row| row.id == point.row)?;
        if point.col < source.cells.len() {
            point.col = source.cell_span(point.col).start;
        }
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
            Some((index, pos.character_index))
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
        let start_row = &self.rows[first.0];
        let start_col = start_row
            .columns
            .get(first.1)
            .copied()
            .unwrap_or(start_row.width);
        let end_row = &self.rows[last.0];
        last.1 = end_row
            .chunk_ends
            .iter()
            .find(|&&(index, _)| index == last.1)
            .map(|&(_, end)| end)
            .unwrap_or_else(|| {
                end_row
                    .columns
                    .get(last.1)
                    .copied()
                    .unwrap_or(end_row.width)
            });
        if last.1 == 0 {
            last.0 = last.0.checked_sub(1)?;
            last.1 = self.rows[last.0].width;
        }
        let start = GridPoint {
            row: self.rows[first.0].row,
            col: start_col,
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
    fn accessible_cursor_and_selection_leave_compressed_history_cold() {
        let mut terminal = rustty::vt::Terminal::new(80, 2, 3000);
        terminal.feed("old history\r\n".repeat(2000).as_bytes());
        terminal.feed(b"\x1b[H\x1b]66;w=7;visible text\x07\x1b[1;5H");
        let warm_bytes = terminal.screen().owned_bytes();
        terminal.set_scrollback_compression(true);
        while terminal.compress_scrollback_step() {}
        let cold_bytes = terminal.screen().owned_bytes();
        assert!(cold_bytes < warm_bytes);
        for selected in [false, true] {
            let row = terminal.screen().row(0).id;
            terminal.screen_mut().selection = selected.then_some(Selection {
                start: GridPoint { row, col: 1 },
                end: GridPoint { row, col: 6 },
                rectangular: false,
            });
            let text = TerminalText::new(7, terminal.screen(), [0.0; 2], [10.0, 20.0]);
            assert!(text.selection.is_some());
            assert_eq!(terminal.screen().owned_bytes(), cold_bytes);
        }
    }

    #[test]
    fn explicit_width_text_is_one_accessible_selection_unit() {
        let mut terminal = rustty::vt::Terminal::new(16, 2, 0);
        terminal.feed("A\x1b]66;w=7;hello 水\x07B".as_bytes());
        let text = TerminalText::new(7, terminal.screen(), [0.0; 2], [10.0, 20.0]);
        let row = &text.rows[0];
        assert_eq!(&row.node.character_lengths()[..3], &[1, 9, 1]);
        assert_eq!(
            &row.node.character_widths().unwrap()[..3],
            &[10.0, 70.0, 10.0]
        );
        assert_eq!(&row.columns[..3], &[0, 1, 8]);
        for col in 1..8 {
            let point = GridPoint { row: row.row, col };
            let start = text.position(terminal.screen(), point, false).unwrap();
            let end = text.position(terminal.screen(), point, true).unwrap();
            assert_eq!((start.character_index, end.character_index), (1, 2));
            terminal.screen_mut().selection = text
                .selection(TextSelection {
                    anchor: start,
                    focus: end,
                })
                .unwrap();
            assert_eq!(
                terminal.screen().selection_text().as_deref(),
                Some("hello 水")
            );
        }
    }

    #[test]
    fn long_explicit_text_chunks_select_together_and_wrap_gaps_stay_empty() {
        let mut terminal = rustty::vt::Terminal::new(10, 3, 0);
        let payload = "水🙂".repeat(100);
        terminal.feed(format!("abcdef\x1b]66;w=7;{payload}\x07Z").as_bytes());
        let text = TerminalText::new(7, terminal.screen(), [0.0; 2], [10.0, 20.0]);
        let first = &text.rows[0];
        for col in 6..10 {
            assert!(terminal.screen().row(0).cells[col].spacer_head());
            let point = GridPoint {
                row: first.row,
                col,
            };
            assert_eq!(
                text.position(terminal.screen(), point, false)
                    .unwrap()
                    .character_index,
                6
            );
            assert_eq!(
                text.position(terminal.screen(), point, true)
                    .unwrap()
                    .character_index,
                6
            );
        }
        let row = &text.rows[1];
        let chunks = row.columns.partition_point(|&col| col == 0);
        assert!(chunks > 1);
        assert_eq!(
            row.node.character_lengths()[..chunks]
                .iter()
                .map(|&length| usize::from(length))
                .sum::<usize>(),
            payload.len()
        );
        assert_eq!(row.node.character_widths().unwrap()[0], 70.0);
        assert!(
            row.node.character_widths().unwrap()[1..chunks]
                .iter()
                .all(|&width| width == 0.0)
        );
        let point = GridPoint {
            row: row.row,
            col: 6,
        };
        let start = text.position(terminal.screen(), point, false).unwrap();
        let end = text.position(terminal.screen(), point, true).unwrap();
        assert_eq!((start.character_index, end.character_index), (0, chunks));
        terminal.screen_mut().selection = text
            .selection(TextSelection {
                anchor: start,
                focus: end,
            })
            .unwrap();
        assert_eq!(
            terminal.screen().selection_text().as_deref(),
            Some(payload.as_str())
        );
        for (first, last) in [(0, 1), (1, 2), (2, 1)] {
            terminal.screen_mut().selection = text
                .selection(TextSelection {
                    anchor: TextPosition {
                        node: row.id,
                        character_index: first,
                    },
                    focus: TextPosition {
                        node: row.id,
                        character_index: last,
                    },
                })
                .unwrap();
            assert_eq!(
                terminal.screen().selection_text().as_deref(),
                Some(payload.as_str())
            );
            assert_eq!(terminal.screen().selection.unwrap().end.col, 6);
        }
        assert_eq!(
            text.selection(TextSelection {
                anchor: start,
                focus: start
            }),
            Some(None)
        );
    }

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
