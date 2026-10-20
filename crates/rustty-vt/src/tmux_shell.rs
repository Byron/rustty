//! tmux OSC 133 row markers and command output ranges (input.c/grid-reader.c).
use crate::{Effect, GridPoint, Screen, Selection, SemanticContent, Terminal};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Markers {
    /// Column and retained primary (1) / secondary (2) prompt flags.
    pub prompt: Option<(usize, u8)>,
    pub command: Option<usize>,
    pub start: Option<usize>,
    /// Exclusive end column and exit status.
    pub end: Option<(usize, u8)>,
}
pub(crate) mod serde_markers {
    use super::*;
    use serde::{Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(
        markers: &Option<BTreeMap<u64, Markers>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        markers
            .as_ref()
            .map(|m| m.iter().collect::<Vec<_>>())
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<BTreeMap<u64, Markers>>, D::Error> {
        let Some(entries) = Option::<Vec<(u64, Markers)>>::deserialize(deserializer)? else {
            return Ok(None);
        };
        let mut markers = BTreeMap::new();
        for (row, value) in entries {
            if markers.insert(row, value).is_some() {
                return Err(D::Error::custom("duplicate shell marker row"));
            }
        }
        Ok(Some(markers))
    }
}

impl Markers {
    fn columns(&self) -> impl Iterator<Item = usize> {
        self.prompt
            .map(|p| p.0)
            .into_iter()
            .chain(self.command)
            .chain(self.start)
            .chain(self.end.map(|p| p.0))
    }
    fn primary(&self) -> Option<usize> {
        self.prompt.filter(|p| p.1 & 1 != 0).map(|p| p.0)
    }
}
impl Terminal {
    pub(crate) fn tmux_osc133(&mut self, data: &[u8], effects: &mut Vec<Effect>) {
        let Some(&action) = data.first() else {
            return;
        };
        if !matches!(action, b'A' | b'N' | b'P' | b'B' | b'I' | b'C' | b'D') {
            return;
        }
        let screen = self.screen_mut();
        let row = screen.row(screen.cursor.row).id;
        let col = screen.cursor.col;
        let markers = screen
            .tmux_shell
            .get_or_insert_with(BTreeMap::new)
            .entry(row)
            .or_default();
        match action {
            b'A' | b'N' | b'P' => {
                let secondary = action == b'P'
                    && data
                        .windows(4)
                        .position(|v| v == b";k=s")
                        .is_some_and(|at| data.get(at + 4).is_none_or(|&next| next == b';'));
                let flags = markers.prompt.map_or(0, |p| p.1) | if secondary { 2 } else { 1 };
                markers.prompt = Some((col, flags));
                screen.cursor.semantic = SemanticContent::Prompt;
                if action != b'P' {
                    effects.push(Effect::ShellPrompt);
                }
            }
            b'B' | b'I' => {
                markers.command = Some(col);
                screen.cursor.semantic = SemanticContent::Input;
            }
            b'C' => {
                markers.start = Some(col);
                screen.cursor.semantic = SemanticContent::Output;
                effects.push(Effect::TmuxCommandStart {
                    command_line: command_line(data),
                });
                if self.shell_command_events {
                    effects.push(Effect::CommandStart);
                }
            }
            b'D' => {
                let status = exit_status(data);
                markers.end = Some((col, status));
                screen.cursor.semantic = SemanticContent::Output;
                effects.push(Effect::CommandEnd {
                    exit_code: Some(i32::from(status)),
                });
            }
            _ => unreachable!(),
        }
    }
}
fn exit_status(data: &[u8]) -> u8 {
    if data.get(1) != Some(&b';') {
        return 0;
    }
    let field = data[2..].split(|&b| b == b';').next().unwrap_or_default();
    if field.is_empty() || field.contains(&b'=') {
        return 0;
    }
    std::str::from_utf8(field)
        .ok()
        .and_then(|s| s.trim_start().parse::<u8>().ok())
        .unwrap_or(255)
}
fn command_line(data: &[u8]) -> Option<Vec<u8>> {
    if data.get(1) != Some(&b';') {
        return None;
    }
    let data = data[2..]
        .split(|&b| b == b';')
        .find_map(|s| s.strip_prefix(b"cmdline_url="))?;
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let mut b = data[at];
        at += 1;
        if b == b'%' {
            let pair = data.get(at..at + 2)?;
            b = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
            if b == 0 {
                return None;
            }
            at += 2;
        }
        if out.len() == 16384 {
            return None;
        }
        out.push(b);
    }
    std::str::from_utf8(&out).ok()?;
    Some(out)
}

impl Screen {
    pub fn tmux_shell_markers(&self) -> Option<&BTreeMap<u64, Markers>> {
        self.tmux_shell.as_ref()
    }
    pub(crate) fn tmux_shell_points(&self) -> Vec<(u64, usize)> {
        self.tmux_shell
            .iter()
            .flat_map(|m| m.iter())
            .flat_map(|(&row, m)| m.columns().map(move |col| (row, col)))
            .collect()
    }
    pub(crate) fn reflow_tmux_shell(
        &mut self,
        points: &HashMap<(u64, usize), GridPoint>,
        rows: &[u64],
    ) {
        let Some(old) = self.tmux_shell.take() else {
            return;
        };
        let mut new = BTreeMap::<u64, Markers>::new();
        for row in rows {
            let Some(m) = old.get(row) else {
                continue;
            };
            let field = |col| points.get(&(*row, col)).copied();
            if let Some((col, secondary)) = m.prompt
                && let Some(p) = field(col)
            {
                let next = new.entry(p.row).or_default();
                next.prompt = Some((p.col, next.prompt.map_or(0, |p| p.1) | secondary));
            }
            if let Some(col) = m.command
                && let Some(p) = field(col)
            {
                new.entry(p.row).or_default().command = Some(p.col);
            }
            if let Some(col) = m.start
                && let Some(p) = field(col)
            {
                new.entry(p.row).or_default().start = Some(p.col);
            }
            if let Some((col, status)) = m.end
                && let Some(p) = field(col)
            {
                new.entry(p.row).or_default().end = Some((p.col, status));
            }
        }
        self.tmux_shell = Some(new);
    }
    pub(crate) fn restore_tmux_shell(&mut self, source: &Screen) -> Result<(), &'static str> {
        let rows = source
            .all_rows()
            .map(|r| r.id)
            .collect::<std::collections::HashSet<_>>();
        if source
            .tmux_shell
            .iter()
            .flat_map(|m| m.iter())
            .any(|(id, m)| {
                !rows.contains(id)
                    || m.prompt.is_some_and(|p| p.1 == 0 || p.1 > 3)
                    || m.columns().any(|col| col > usize::from(u16::MAX))
            })
        {
            return Err("invalid shell marker");
        }
        self.tmux_shell.clone_from(&source.tmux_shell);
        Ok(())
    }
    /// An exclusive end allows an OSC 133 D at column zero to retain a newline.
    pub fn tmux_output_range(&self, point: GridPoint) -> Option<(GridPoint, GridPoint)> {
        let rows = self.all_rows().collect::<Vec<_>>();
        let cursor = (rows.iter().position(|r| r.id == point.row)?, point.col);
        let empty = Markers::default();
        let markers = self.tmux_shell.as_ref()?;
        let m = |y: usize| markers.get(&rows[y].id).unwrap_or(&empty);
        let prompt = (0..=cursor.0)
            .filter_map(|y| {
                m(y).primary()
                    .filter(|&x| y < cursor.0 || x <= cursor.1)
                    .map(|x| (y, x))
            })
            .last();
        let mut start = None;
        let mut end = None;
        let mut stopped = false;
        for y in prompt.map_or(0, |p| p.0)..rows.len() {
            let row = m(y);
            let next_prompt = row.primary().filter(|_| prompt.is_none_or(|p| p.0 != y));
            let in_range = |x| {
                if let Some((py, px)) = prompt
                    && py == y
                {
                    x >= px
                } else {
                    next_prompt.is_none_or(|px| x < px)
                }
            };
            if let Some(x) = row.start.filter(|&x| in_range(x)) {
                start = Some((y, x));
            }
            if start.is_none()
                && prompt.is_none()
                && let Some((x, _)) = row.end
                && next_prompt.is_none_or(|px| x <= px)
            {
                start = Some((0, 0));
            }
            if let Some((sy, sx)) = start
                && let Some((x, _)) = row.end
                && prompt.is_none_or(|(py, px)| py != y || x >= px)
                && (sy != y || x >= sx)
            {
                end = Some((y, x));
                break;
            }
            if next_prompt.is_some() {
                stopped = true;
                break;
            }
        }
        if let Some(start) = start {
            if end.is_none() && stopped {
                return None;
            }
            let end = end.unwrap_or_else(|| {
                let y = rows.iter().rposition(|r| r.used() != 0).unwrap_or(0);
                (y, rows[y].used())
            });
            return Some((
                GridPoint {
                    row: rows[start.0].id,
                    col: start.1,
                },
                GridPoint {
                    row: rows[end.0].id,
                    col: end.1,
                },
            ));
        }
        // A prompt without output selects the most recent completed command.
        let mut pending = None;
        let mut previous = None;
        let mut have_prompt = false;
        for y in 0..=cursor.0 {
            let row = m(y);
            let begin = row.start.filter(|&x| y < cursor.0 || x <= cursor.1);
            let finish = row
                .end
                .map(|p| p.0)
                .filter(|&x| y < cursor.0 || x <= cursor.1);
            let end_first = begin.zip(finish).is_some_and(|(b, e)| e < b);
            if let Some(x) = begin
                && !end_first
            {
                pending = Some((y, x));
            }
            if pending.is_none()
                && !have_prompt
                && let Some((x, _)) = row.end
                && row.primary().is_none_or(|p| x <= p)
            {
                pending = Some((0, 0));
            }
            if let Some(start) = pending
                && let Some(x) = finish
            {
                previous = Some((start, (y, x)));
                pending = None;
            }
            if let Some(x) = begin
                && end_first
            {
                pending = Some((y, x));
            }
            if let Some(prompt) = row.primary() {
                if begin.is_none_or(|x| x < prompt) {
                    pending = None;
                }
                have_prompt = true;
            }
        }
        previous.map(|(s, e)| {
            (
                GridPoint {
                    row: rows[s.0].id,
                    col: s.1,
                },
                GridPoint {
                    row: rows[e.0].id,
                    col: e.1,
                },
            )
        })
    }
    pub fn tmux_output_text(&self, point: GridPoint) -> Option<String> {
        let (start, end) = self.tmux_output_range(point)?;
        let mut out = self.tmux_grid_text(start, end, false)?;
        if out.ends_with('\n') {
            out.pop();
        }
        (!out.is_empty()).then_some(out)
    }
    fn tmux_grid_text(
        &self,
        start: GridPoint,
        end: GridPoint,
        rectangular: bool,
    ) -> Option<String> {
        let rows = self.all_rows().collect::<Vec<_>>();
        let first = rows.iter().position(|r| r.id == start.row)?;
        let last = rows.iter().position(|r| r.id == end.row)?;
        if first > last {
            return None;
        }
        let mut out = String::new();
        for (y, row) in rows.iter().enumerate().take(last + 1).skip(first) {
            let used = copy_length(*row);
            let from = if y == first || rectangular {
                start.col
            } else {
                0
            };
            let to = if y == last || rectangular {
                end.col
            } else {
                row.cells.len()
            };
            if from > to {
                continue;
            }
            let to = to.min(used);
            for x in from.min(used)..to {
                if row.cells[x].width() == 0 {
                    continue;
                }
                if row.cells[x].codepoint().is_none() {
                    out.push(' ');
                } else {
                    out.push_str(&row.text(x));
                }
            }
            if !row.wrapped || to != used {
                out.push('\n');
            }
        }
        Some(out)
    }
    pub(crate) fn tmux_selection_text(&self, mut selection: Selection) -> Option<String> {
        let rows = self.all_rows().collect::<Vec<_>>();
        let position = |p: GridPoint| rows.iter().position(|r| r.id == p.row).map(|y| (y, p.col));
        if position(selection.start)? > position(selection.end)? {
            std::mem::swap(&mut selection.start, &mut selection.end);
        }
        if selection.rectangular && selection.start.col > selection.end.col {
            std::mem::swap(&mut selection.start.col, &mut selection.end.col);
        }
        let row = rows[position(selection.end)?.0];
        let used = copy_length(row);
        selection.end.col = selection.end.col.min(used) + 1;
        let mut out = self.tmux_grid_text(selection.start, selection.end, selection.rectangular)?;
        if selection.end.col <= used
            && (!row.wrapped || selection.end.col != used)
            && out.ends_with('\n')
        {
            out.pop();
        }
        (!out.is_empty()).then_some(out)
    }
    pub(crate) fn tmux_select_output(&self, point: GridPoint) -> Option<Selection> {
        let (start, mut end) = self.tmux_output_range(point)?;
        self.tmux_output_text(point)?;
        let rows = self.all_rows().collect::<Vec<_>>();
        let y = rows.iter().position(|r| r.id == end.row)?;
        end.col = end.col.min(copy_length(rows[y]));
        if end.col == 0 && y > 0 {
            end.row = rows[y - 1].id;
            end.col = copy_length(rows[y - 1]);
            if rows[y - 1].wrapped {
                end.col = end.col.saturating_sub(1);
            }
        } else {
            end.col = end.col.saturating_sub(1);
            while end.col > 0 && rows[y].cells[end.col].width() == 0 {
                end.col -= 1;
            }
        }
        Some(Selection {
            start,
            end,
            rectangular: false,
        })
    }
}
fn copy_length(row: crate::Row<'_>) -> usize {
    if row.wrapped {
        row.cells.len()
    } else {
        row.cells
            .iter()
            .rposition(|c| c.codepoint().is_some_and(|cp| cp != ' ') || c.width() == 0)
            .map_or(0, |x| x + 1)
    }
}
