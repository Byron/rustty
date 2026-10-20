use super::*;
use std::collections::VecDeque;

pub(super) struct CaptureLine {
    cells: Vec<u8>,
    pub bounds: (usize, usize),
    pub time: u64,
    pub flags: u16,
    wrapped: bool,
    markers: bool,
    fresh: bool,
}

impl CaptureLine {
    fn width(&self) -> usize {
        self.cells.iter().map(|&w| usize::from(w)).sum()
    }
    fn grow(&mut self, count: usize, original_width: usize) {
        for requested in self.bounds.0 + 1..=count {
            self.bounds.1 = tmux_expand(self.bounds.1, requested, original_width);
        }
        self.bounds.0 = count;
    }
}

impl Screen {
    // Preserve grid_expand_line's observable spare-space policy without storing a second grid.
    pub(super) fn reflow_capture_metadata(&self, width: usize) -> Vec<CaptureLine> {
        let mut source = self
            .all_rows()
            .map(|row| {
                let bounds = self
                    .tmux_capture_bounds(row.id)
                    .unwrap_or((row.used(), row.cells.len()));
                CaptureLine {
                    cells: (0..bounds.0)
                        .map(|col| {
                            row.cells
                                .get(col)
                                .map_or(1, |cell| match row.style(col).tmux_tab {
                                    -1 => 0,
                                    width if width > 0 => width as u8,
                                    _ => cell.width(),
                                })
                        })
                        .collect(),
                    bounds,
                    time: self.tmux_row_time(row.id),
                    flags: self.tmux_line_flags(row) & 0x102,
                    wrapped: row.wrapped,
                    markers: self
                        .tmux_shell
                        .as_ref()
                        .is_some_and(|m| m.contains_key(&row.id)),
                    fresh: false,
                }
            })
            .collect::<VecDeque<_>>();
        let mut output = Vec::new();
        while let Some(mut line) = source.pop_front() {
            while line.width() > width {
                let mut consumed = 0;
                let mut cut = 0;
                for (index, &cell) in line.cells.iter().enumerate() {
                    if cut == 0 && consumed + usize::from(cell) > width {
                        cut = index;
                    }
                    consumed += usize::from(cell);
                }
                if cut == 0 {
                    cut = 1;
                }
                let rest = line.cells.split_off(cut);
                let mut next = CaptureLine {
                    cells: rest,
                    bounds: (0, 0),
                    time: 0,
                    flags: 0,
                    wrapped: line.wrapped,
                    markers: line.markers,
                    fresh: true,
                };
                next.grow(next.cells.len(), width);
                if line.fresh {
                    line.bounds = (0, 0);
                    line.grow(cut, width);
                } else {
                    line.bounds = (cut, cut);
                }
                line.wrapped = true;
                output.push(line);
                line = next;
            }
            if line.wrapped && line.width() < width {
                while let Some(next) = source.front_mut() {
                    if next.cells.is_empty() && !next.wrapped && !next.markers {
                        break;
                    }
                    let mut count = 0;
                    let mut used_width = line.width();
                    for &cell in &next.cells {
                        if used_width + usize::from(cell) > width {
                            break;
                        }
                        used_width += usize::from(cell);
                        count += 1;
                    }
                    if count == 0 && !next.cells.is_empty() {
                        break;
                    }
                    line.cells.extend(next.cells.drain(..count));
                    line.grow(line.cells.len(), width);
                    if next.cells.is_empty() {
                        line.wrapped = next.wrapped;
                        source.pop_front();
                    } else {
                        next.bounds = (next.cells.len(), next.cells.len());
                        break;
                    }
                    if !line.wrapped || used_width == width {
                        break;
                    }
                }
            }
            output.push(line);
        }
        output
    }

    pub(super) fn install_reflow_capture_metadata(&mut self, mut lines: Vec<CaptureLine>) {
        while lines.len() < self.height {
            lines.push(CaptureLine {
                cells: Vec::new(),
                bounds: (0, 0),
                time: 0,
                flags: 0,
                wrapped: false,
                markers: false,
                fresh: true,
            });
        }
        let ids = self.all_rows().map(|row| row.id).collect::<Vec<_>>();
        self.tmux_capture_rows.clear();
        self.tmux_row_times.clear();
        self.tmux_row_flags.clear();
        self.tmux_capture_known = ids.len() == lines.len();
        if !self.tmux_capture_known {
            return;
        }
        for (id, line) in ids.into_iter().zip(lines) {
            if line.bounds != (0, 0) {
                self.tmux_capture_rows.insert(id, line.bounds);
            }
            if line.time != 0 {
                self.tmux_row_times.insert(id, line.time);
            }
            if line.flags != 0 {
                self.tmux_row_flags.insert(id, line.flags);
            }
        }
    }
}
