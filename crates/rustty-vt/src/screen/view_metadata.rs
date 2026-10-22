use super::*;
use std::collections::BTreeMap;

/// Native row anchors and tmux capture metadata omitted by the portable checkpoint.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ViewMetadata {
    /// History followed by visible rows, with their physical widths.
    rows: Vec<(u64, usize)>,
    next_row: u64,
    tmux_history: Option<TmuxHistory>,
    #[serde(with = "crate::tmux_shell::serde_markers")]
    tmux_shell: Option<BTreeMap<u64, crate::tmux_shell::Markers>>,
    #[serde(with = "super::tmux_capture_map")]
    tmux_capture_rows: BTreeMap<u64, (usize, usize)>,
    tmux_capture_known: bool,
    #[serde(deserialize_with = "super::serde_impl::deserialize_row_map")]
    tmux_row_times: BTreeMap<u64, u64>,
    #[serde(deserialize_with = "super::serde_impl::deserialize_row_map")]
    tmux_row_flags: BTreeMap<u64, u16>,
    cursor_ansi: u8,
    saved_cursor_ansi: Option<u8>,
    /// Nonzero runs: absolute row, column, length, ANSI bits, tab spelling.
    cell_runs: Vec<(usize, usize, usize, u8, i8)>,
}

impl ViewMetadata {
    /// History policy needed before loading checkpoint history pages.
    pub fn tmux_history(&self) -> Option<TmuxHistory> {
        self.tmux_history
    }
}

impl Screen {
    pub fn view_metadata(&self) -> ViewMetadata {
        let mut cell_runs = Vec::new();
        for (absolute, row) in self.all_rows().enumerate() {
            let mut start = 0;
            while start < row.cells.len() {
                let style = row.style(start);
                if style.tmux_ansi == 0 && style.tmux_tab == 0 {
                    start += 1;
                    continue;
                }
                let mut end = start + 1;
                while end < row.cells.len() {
                    let next = row.style(end);
                    if (next.tmux_ansi, next.tmux_tab) != (style.tmux_ansi, style.tmux_tab) {
                        break;
                    }
                    end += 1;
                }
                cell_runs.push((
                    absolute,
                    start,
                    end - start,
                    style.tmux_ansi,
                    style.tmux_tab,
                ));
                start = end;
            }
        }
        ViewMetadata {
            rows: self
                .all_rows()
                .map(|row| (row.id, row.cells.len()))
                .collect(),
            next_row: self.next_row,
            tmux_history: self.tmux_history,
            tmux_shell: self.tmux_shell.clone(),
            tmux_capture_rows: self.tmux_capture_rows.clone(),
            tmux_capture_known: self.tmux_capture_known,
            tmux_row_times: self.tmux_row_times.clone(),
            tmux_row_flags: self.tmux_row_flags.clone(),
            cursor_ansi: self.cursor.style.tmux_ansi,
            saved_cursor_ansi: self
                .saved_cursor
                .as_ref()
                .map(|saved| saved.cursor.style.tmux_ansi),
            cell_runs,
        }
    }

    pub(crate) fn validate_view_metadata(&self, source: &ViewMetadata) -> Result<(), &'static str> {
        if self.all_rows().len() != source.rows.len() {
            return Err("view row count mismatch");
        }
        let mut widths = BTreeMap::new();
        for (current, &(id, width)) in self.all_rows().zip(&source.rows) {
            if current.cells.len() != width {
                return Err("view row width mismatch");
            }
            if id >= source.next_row || widths.insert(id, width).is_some() {
                return Err("invalid view row identity");
            }
        }
        if source
            .tmux_history
            .is_some_and(|history| history.scrolled > self.history_len())
        {
            return Err("history scroll position exceeds retained rows");
        }
        if source
            .tmux_shell
            .iter()
            .flat_map(|markers| markers.iter())
            .any(|(id, markers)| !widths.contains_key(id) || !markers.valid())
        {
            return Err("invalid shell marker");
        }
        if source.tmux_capture_rows.len() > widths.len()
            || source
                .tmux_capture_rows
                .iter()
                .any(|(id, &(used, allocated))| {
                    widths.get(id).is_none_or(|&width| used > width)
                        || used > allocated
                        || allocated > usize::from(u16::MAX)
                })
        {
            return Err("invalid capture allocation bounds");
        }
        if source.tmux_row_times.len() > widths.len()
            || source
                .tmux_row_times
                .keys()
                .any(|id| !widths.contains_key(id))
        {
            return Err("invalid capture line timestamps");
        }
        if source.tmux_row_flags.len() > widths.len()
            || source
                .tmux_row_flags
                .iter()
                .any(|(id, flags)| !widths.contains_key(id) || flags & !0x102 != 0)
        {
            return Err("invalid capture line flags");
        }
        if source.cursor_ansi > 3
            || source.saved_cursor_ansi.is_some_and(|ansi| ansi > 3)
            || self.saved_cursor.is_some() != source.saved_cursor_ansi.is_some()
        {
            return Err("invalid cursor ANSI metadata");
        }
        let mut previous = None;
        for &(absolute, start, count, ansi, tab) in &source.cell_runs {
            let end = start
                .checked_add(count)
                .ok_or("capture cell run overflow")?;
            if count == 0
                || source
                    .rows
                    .get(absolute)
                    .is_none_or(|&(_, width)| end > width)
                || previous
                    .is_some_and(|(row, end)| absolute < row || (absolute == row && start < end))
                || ansi > 3
                || !(-1..=32).contains(&tab)
                || (ansi == 0 && tab == 0)
            {
                return Err("invalid capture cell run");
            }
            previous = Some((absolute, end));
            if tab != 0 {
                let row = self.physical_row(absolute);
                if row.cells[start..end].iter().any(|cell| {
                    cell.width() != 1
                        || cell.has_grapheme()
                        || !matches!(cell.codepoint(), None | Some(' '))
                }) {
                    return Err("invalid tab cell metadata");
                }
            }
        }
        Ok(())
    }

    pub(crate) fn apply_view_metadata(&mut self, source: &ViewMetadata) {
        let identities = self
            .all_rows()
            .zip(&source.rows)
            .map(|(row, &(id, _))| (row.id, id))
            .collect::<HashMap<_, _>>();
        for page in &mut self.pages.pages {
            for id in &mut page.row_ids[..usize::from(page.rows)] {
                *id = identities[id];
            }
        }
        self.remap_grid_rows(&identities);
        self.next_row = source.next_row;
        self.tmux_history = source.tmux_history;
        self.tmux_shell.clone_from(&source.tmux_shell);
        self.tmux_capture_rows.clone_from(&source.tmux_capture_rows);
        self.tmux_capture_known = source.tmux_capture_known;
        self.tmux_row_times.clone_from(&source.tmux_row_times);
        self.tmux_row_flags.clone_from(&source.tmux_row_flags);
        for &(absolute, start, count, ansi, tab) in &source.cell_runs {
            for col in start..start + count {
                let mut style = self.physical_row(absolute).style(col);
                style.tmux_ansi = ansi;
                style.tmux_tab = tab;
                self.set_absolute_cell_style(absolute, col, style);
            }
        }
        self.cursor.style.tmux_ansi = source.cursor_ansi;
        if let (Some(saved), Some(ansi)) = (&mut self.saved_cursor, source.saved_cursor_ansi) {
            saved.cursor.style.tmux_ansi = ansi;
        }
        self.sync_cursor_resources();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Terminal, snapshot};

    fn decoded(terminal: &Terminal) -> Terminal {
        let mut terminal = snapshot::decode_exact(
            snapshot::encode_to_vec(terminal).unwrap().as_slice(),
            snapshot::DecodeOptions {
                tmux_strings: true,
                ..Default::default()
            },
        )
        .unwrap();
        terminal.tmux_graphics = true;
        terminal
    }

    fn assert_screen(actual: &Screen, expected: &Screen) {
        assert_eq!(
            serde_json::to_value(actual.view_metadata()).unwrap(),
            serde_json::to_value(expected.view_metadata()).unwrap()
        );
        for (actual, expected) in actual.all_rows().zip(expected.all_rows()) {
            for col in 0..actual.cells.len() {
                assert_eq!(*actual.text(col), *expected.text(col));
                assert_eq!(actual.style(col), expected.style(col));
            }
        }
    }

    #[test]
    fn checkpoint_view_metadata_restores_history_shell_ansi_tabs_and_continuation() {
        let mut terminal = Terminal::new(40, 4, 20);
        terminal.tmux_graphics = true;
        terminal.set_tmux_strings(true);
        terminal.set_tmux_history_limit(20);
        terminal.set_tmux_time(1234);
        for _ in 0..30 {
            terminal.feed(b"\x1b]133;A\x07prompt\x1b]133;B\x07\x1b[31mone\ttwo\x1b]133;C\x07\r\noutput\x1b]133;D;3\x07\r\n");
        }
        terminal.feed(b"\x1b[32m\x1b7\x1b[?1049halternate\t\x1b[33m\x1b7\x1b[?1049l\x1b[3");
        let primary: ViewMetadata = serde_json::from_value(
            serde_json::to_value(terminal.primary_screen().view_metadata()).unwrap(),
        )
        .unwrap();
        let alternate = terminal.alternate_screen().map(Screen::view_metadata);
        assert!(!primary.cell_runs.is_empty());
        assert!(primary.cell_runs.iter().any(|run| run.3 != 0));
        assert!(!primary.tmux_shell.as_ref().unwrap().is_empty());
        assert!(!primary.tmux_row_times.is_empty());
        let mut restored = decoded(&terminal);
        restored
            .restore_view_metadata(&primary, alternate.as_ref())
            .unwrap();
        restored.set_tmux_time(1234);
        assert_screen(restored.primary_screen(), terminal.primary_screen());
        assert_screen(
            restored.alternate_screen().unwrap(),
            terminal.alternate_screen().unwrap(),
        );
        for input in [
            b"4mcontinued\toutput\r\n".as_slice(),
            b"\x1b[?1049h\x1b8saved\r\n",
            b"\x1b[?1049l\x1b8primary",
        ] {
            terminal.feed(input);
            restored.feed(input);
            assert_screen(restored.primary_screen(), terminal.primary_screen());
            assert_screen(
                restored.alternate_screen().unwrap(),
                terminal.alternate_screen().unwrap(),
            );
        }
        for (cols, rows) in [(7, 2), (30, 5), (15, 3)] {
            terminal.resize(cols, rows);
            let primary = terminal.primary_screen().view_metadata();
            let alternate = terminal.alternate_screen().map(Screen::view_metadata);
            let mut restored = decoded(&terminal);
            restored
                .restore_view_metadata(&primary, alternate.as_ref())
                .unwrap();
            assert_screen(restored.primary_screen(), terminal.primary_screen());
            assert_screen(
                restored.alternate_screen().unwrap(),
                terminal.alternate_screen().unwrap(),
            );
        }
    }

    #[test]
    fn invalid_view_metadata_never_partially_replaces_either_screen() {
        let mut terminal = Terminal::new(16, 2, 10);
        terminal.tmux_graphics = true;
        terminal.set_tmux_strings(true);
        terminal.feed(b"a\tb\x1b7\x1b[?1049h\x1b[Halt\x1b7\x1b[?1049l");
        let primary = terminal.primary_screen().view_metadata();
        let alternate = terminal.alternate_screen().unwrap().view_metadata();
        for (case, mutate) in [
            (|m: &mut ViewMetadata| {
                m.rows.pop();
            }) as fn(&mut ViewMetadata),
            |m| m.rows[0].0 = m.rows[1].0,
            |m| m.next_row = m.rows[0].0,
            |m| m.rows[0].1 += 1,
            |m| {
                m.tmux_history = Some(TmuxHistory {
                    scrolled: usize::MAX,
                    ..Default::default()
                })
            },
            |m| {
                m.tmux_shell = Some(BTreeMap::from([(m.next_row, Default::default())]));
            },
            |m| {
                m.tmux_shell = Some(BTreeMap::from([(
                    m.rows[0].0,
                    crate::tmux_shell::Markers {
                        prompt: Some((0, 4)),
                        ..Default::default()
                    },
                )]));
            },
            |m| {
                m.tmux_capture_rows.insert(m.next_row, (0, 0));
            },
            |m| {
                m.tmux_capture_rows
                    .insert(m.rows[0].0, (m.rows[0].1 + 1, m.rows[0].1 + 1));
            },
            |m| {
                m.tmux_capture_rows.insert(m.rows[0].0, (2, 1));
            },
            |m| {
                m.tmux_row_times.insert(m.next_row, 1);
            },
            |m| {
                m.tmux_row_flags.insert(m.rows[0].0, 4);
            },
            |m| m.cursor_ansi = 4,
            |m| m.saved_cursor_ansi = Some(4),
            |m| m.saved_cursor_ansi = None,
            |m| m.cell_runs = vec![(0, 0, 0, 1, 0)],
            |m| m.cell_runs = vec![(m.rows.len(), 0, 1, 1, 0)],
            |m| m.cell_runs = vec![(0, usize::MAX, 1, 1, 0)],
            |m| m.cell_runs = vec![(0, m.rows[0].1, 1, 1, 0)],
            |m| m.cell_runs = vec![(0, 0, 1, 4, 0)],
            |m| m.cell_runs = vec![(0, 0, 1, 0, 33)],
            |m| m.cell_runs = vec![(0, 0, 1, 0, 1)],
            |m| m.cell_runs = vec![(0, 0, 2, 1, 0), (0, 1, 1, 1, 0)],
        ]
        .into_iter()
        .enumerate()
        {
            for bad_alternate in [false, true] {
                let mut primary = primary.clone();
                let mut alternate = alternate.clone();
                mutate(if bad_alternate {
                    &mut alternate
                } else {
                    &mut primary
                });
                let mut restored = decoded(&terminal);
                let before =
                    serde_json::to_value(restored.primary_screen().view_metadata()).unwrap();
                let before_alt =
                    serde_json::to_value(restored.alternate_screen().unwrap().view_metadata())
                        .unwrap();
                assert!(
                    restored
                        .restore_view_metadata(&primary, Some(&alternate))
                        .is_err(),
                    "case {case}, bad alternate: {bad_alternate}"
                );
                assert_eq!(
                    serde_json::to_value(restored.primary_screen().view_metadata()).unwrap(),
                    before
                );
                assert_eq!(
                    serde_json::to_value(restored.alternate_screen().unwrap().view_metadata())
                        .unwrap(),
                    before_alt
                );
            }
        }
    }

    #[test]
    fn colorful_history_metadata_has_no_text_or_repeated_full_styles() {
        let mut terminal = Terminal::new(140, 50, 2000);
        terminal.tmux_graphics = true;
        terminal.set_tmux_strings(true);
        for _ in 0..1000 {
            terminal.feed(
                b"\x1b[31mred text \x1b[32mgreen text \x1b[34mblue text \x1b[0mordinary text\r\n",
            );
        }
        let metadata = terminal.screen().view_metadata();
        let wire = serde_json::to_vec(&metadata).unwrap();
        assert!(terminal.screen().history_len() > 900);
        assert!(wire.len() < 100_000, "metadata used {} bytes", wire.len());
        assert!(!wire.windows(8).any(|text| text == b"red text"));
        assert!(metadata.cell_runs.len() <= 1000);
        let mut restored = decoded(&terminal);
        restored.restore_view_metadata(&metadata, None).unwrap();
        assert_screen(restored.screen(), terminal.screen());
    }
}
