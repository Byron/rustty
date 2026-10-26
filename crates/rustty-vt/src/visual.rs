//! Display-only terminal handover. No parser input or live process state is restored.
use crate::{Screen, Terminal, ViewMetadata, graphics, snapshot};
use serde::{Deserialize, Serialize};
use std::{fmt::Write as _, io};

pub const RESTORED_MARKER: &str = "Restored Rustty display. A new shell starts below.";

#[derive(Serialize, Deserialize)]
pub struct VisualSnapshot {
    version: u32,
    #[serde(with = "serde_bytes")]
    checkpoint: Vec<u8>,
    screen: ViewMetadata,
    graphics: graphics::ViewSnapshot,
}

impl VisualSnapshot {
    /// Capture all primary rows and, when active, append the alternate display.
    /// Cloning before conversion leaves the source's output and input untouched.
    pub fn capture(source: &Terminal) -> io::Result<Self> {
        let mut terminal = Terminal::with_limits(source.cols, source.rows, Default::default());
        terminal.primary =
            source
                .primary
                .visual_copy(source.foreground, source.background, &source.palette)?;
        if source.alternate_active {
            terminal.primary.append_visual(source.screen().visual_copy(
                source.foreground,
                source.background,
                &source.palette,
            )?)?;
        }
        Ok(Self {
            version: 1,
            checkpoint: snapshot::encode_without_graphics_to_vec(&terminal)?,
            screen: terminal.primary.view_metadata(),
            graphics: terminal.primary.graphics.visual_snapshot(),
        })
    }

    fn decode(self) -> io::Result<Terminal> {
        if self.version != 1 {
            return Err(io::Error::other("unsupported visual snapshot version"));
        }
        let mut terminal = snapshot::decode_exact(
            self.checkpoint.as_slice(),
            snapshot::DecodeOptions {
                max_cells: 16 * 1024 * 1024,
                ..Default::default()
            },
        )?;
        if terminal.alternate.is_some() {
            return Err(io::Error::other("visual snapshot must contain one screen"));
        }
        terminal
            .restore_view_metadata(&self.screen, None)
            .map_err(io::Error::other)?;
        terminal
            .restore_view_graphics_with_limit(self.graphics, None, 1024 * 1024 * 1024)
            .map_err(io::Error::other)?;
        terminal.primary.graphics = graphics::Graphics::from_view_snapshot_with_limit(
            terminal.primary.graphics.visual_snapshot(),
            1024 * 1024 * 1024,
        )
        .map_err(io::Error::other)?;
        // The checkpoint is only a cell carrier. Rebuild owner state and IDs so
        // even a validated extreme counter cannot overflow during new output.
        let mut fresh = Terminal::with_limits(terminal.cols, terminal.rows, Default::default());
        fresh.primary.pages = terminal.primary.pages;
        fresh.primary.graphics = terminal.primary.graphics;
        fresh.primary.viewport_pin = None;
        fresh.primary.cursor.row = usize::from(fresh.rows) - 1;
        fresh.primary.reset_visual_row_ids();
        Ok(fresh)
    }

    /// Fully validate before a host replaces windows or starts replacement shells.
    pub fn validate(self) -> io::Result<()> {
        self.decode().map(|_| ())
    }

    /// Install historical display cells into a fresh terminal before child output.
    pub fn restore_into(self, terminal: &mut Terminal) -> io::Result<()> {
        let mut restored = self.decode()?;
        restored.resize(terminal.cols, terminal.rows);
        // Keep fresh-shell defaults, parser state, modes, keyboard and host policy.
        let mut screen = Screen::new(
            usize::from(terminal.cols),
            usize::from(terminal.rows),
            Default::default(),
        );
        screen.pages = restored.primary.pages;
        screen.next_row = restored.primary.next_row;
        screen.viewport_pin = None;
        screen.graphics = restored.primary.graphics;
        screen.cursor = terminal.primary.cursor.clone();
        screen.cursor.row = usize::from(terminal.rows) - 1;
        screen.cursor.col = 0;
        screen.sync_cursor_resources();
        let limits = terminal.primary.limits;
        let memory_limit = terminal.primary.memory_limit;
        terminal.primary = screen;
        // Import itself must not discard the oldest restored rows. Subsequent
        // output uses the receiving session's normal scrollback policy.
        terminal.primary.limits = limits;
        terminal.primary.memory_limit = memory_limit;
        Ok(())
    }
}

/// Join saved history to a live shell after its initial output has settled.
/// Matching rendered rows replace the saved tail; differing output gets a band.
pub fn finish_restore(history: &mut Terminal, terminal: &mut Terminal) -> io::Result<()> {
    history.resize(terminal.cols, terminal.rows);
    history.background = terminal.background;
    history.primary.limits = Default::default();
    history.primary.memory_limit = None;
    let displayed_rows = |screen: &Screen| {
        screen
            .all_rows()
            .enumerate()
            .filter_map(|(index, row)| {
                let has_image = screen
                    .graphics
                    .placements
                    .iter()
                    .any(|placement| placement.row == row.id && !placement.virtual_placement);
                (!screen.row_text(row).trim().is_empty() || has_image).then_some(index + 1)
            })
            .next_back()
            .unwrap_or(0)
    };
    let old_end = displayed_rows(&history.primary);
    let new_end = displayed_rows(&terminal.primary);
    let matches = !terminal.alternate_active
        && new_end > 0
        && new_end <= old_end
        && terminal.primary.graphics.placements.is_empty()
        && history
            .primary
            .all_rows()
            .skip(old_end - new_end)
            .take(new_end)
            .zip(terminal.primary.all_rows().take(new_end))
            .all(|(old, new)| {
                history.primary.row_text(old) == terminal.primary.row_text(new)
                    && !history
                        .primary
                        .graphics
                        .placements
                        .iter()
                        .any(|p| p.row == old.id)
            });
    if old_end > 0 && !matches && (new_end > 0 || terminal.alternate_active) {
        insert_restored_marker(history);
    }
    let keep = if matches {
        old_end - new_end
    } else {
        displayed_rows(&history.primary)
    };
    terminal.primary.prepend_visual(
        std::mem::replace(
            &mut history.primary,
            Screen::new(
                usize::from(terminal.cols),
                usize::from(terminal.rows),
                Default::default(),
            ),
        ),
        keep,
    )?;
    terminal.generation = terminal.generation.wrapping_add(1);
    Ok(())
}

fn insert_restored_marker(terminal: &mut Terminal) {
    let padding = usize::from(terminal.cols).saturating_sub(RESTORED_MARKER.len());
    let dark = terminal.background.map(u16::from).iter().sum::<u16>() < 3 * 128;
    let colors = [12, 20].map(|offset| {
        terminal.background.map(|channel| {
            if dark {
                channel.saturating_add(offset)
            } else {
                channel.saturating_sub(offset)
            }
        })
    });
    let blocks = |count: usize| {
        let mut blocks = String::new();
        for col in 0..count {
            let [r, g, b] = colors[col % 2];
            write!(blocks, "\x1b[38;2;{r};{g};{b}m█").unwrap();
        }
        blocks
    };
    let left = padding / 2;
    let right = padding - left;
    let left = if left > 1 {
        format!("{} ", blocks(left - 1))
    } else {
        blocks(left)
    };
    let right = if right > 1 {
        format!(" {}", blocks(right - 1))
    } else {
        blocks(right)
    };
    terminal
        .feed(format!("\r\n\x1b[1m{left}\x1b[39m{RESTORED_MARKER}{right}\x1b[0m\r\n").as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Color, CursorShape, formatter::Format};

    fn text(terminal: &Terminal) -> String {
        let mut formatter = terminal.formatter(Format::Plain);
        formatter.options.unwrap = true;
        String::from_utf8(formatter.format().unwrap()).unwrap()
    }

    fn wire(source: &Terminal) -> VisualSnapshot {
        let bytes = serde_json::to_vec(&VisualSnapshot::capture(source).unwrap()).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn matching_shell_output_replaces_the_tail_across_repeated_restores() {
        for (cols, rows, prompt) in [
            (80, 4, "user@host> "),
            (7, 3, "directory\r\nwide 界 prompt> "),
        ] {
            let mut source = Terminal::new(cols, rows, 1000);
            source.feed(b"old transcript\r\n");
            source.feed(prompt.as_bytes());
            for _ in 0..3 {
                let before = text(&source);
                let mut history = Terminal::new(cols, rows, 1000);
                wire(&source).restore_into(&mut history).unwrap();
                assert!(!text(&history).contains(RESTORED_MARKER));
                let mut shell = Terminal::new(cols, rows, 1000);
                shell.shell_command_events = true;
                shell.feed(b"\x1b]133;A\x07\x1b[32m");
                for byte in prompt.as_bytes() {
                    shell.feed(&[*byte]);
                }
                shell.feed(b"\x1b[0m\x1b]133;B\x07\x1b[?2004h");
                let point = shell.primary.track(crate::GridPoint {
                    row: shell.primary.row(shell.primary.cursor.row).id,
                    col: shell.primary.cursor.col,
                });
                finish_restore(&mut history, &mut shell).unwrap();
                assert_eq!(text(&shell).trim(), before.trim());
                assert!(!text(&shell).contains(RESTORED_MARKER));
                assert!(shell.modes.dec(2004));
                assert_eq!(
                    shell.primary.resolve(point).unwrap().row,
                    shell.primary.row(shell.primary.cursor.row).id
                );
                source = shell.clone();
                let effects = shell.feed(b"typed command\x1b]133;C\x07");
                assert!(effects.iter().any(|effect| matches!(effect,
                    crate::Effect::CommandStart { command_line } if command_line.as_deref() == Some(b"typed command"))));
                assert!(text(&shell).contains("typed command"));
            }
        }
    }

    #[test]
    fn silent_children_and_resized_matching_prompts_do_not_add_a_band() {
        let mut source = Terminal::new(80, 4, 1000);
        source.feed(b"previous command\r\nlong directory prompt> ");
        for output in [b"".as_slice(), b"long directory prompt> "] {
            let mut history = Terminal::new(7, 4, 1000);
            wire(&source).restore_into(&mut history).unwrap();
            let mut shell = Terminal::new(7, 4, 1000);
            shell.feed(output);
            finish_restore(&mut history, &mut shell).unwrap();
            let display = text(&shell);
            assert!(!display.contains(RESTORED_MARKER), "{display:?}");
            assert_eq!(display.matches("long directory prompt>").count(), 1);
            assert!(display.contains("previous command"));
        }
    }

    #[test]
    fn different_startup_output_keeps_history_and_live_parser_state() {
        for cols in [7, 80] {
            let mut source = Terminal::new(cols, 4, 1000);
            source.feed(b"old transcript\r\nold prompt> ");
            let mut history = Terminal::new(cols, 4, 1000);
            wire(&source).restore_into(&mut history).unwrap();
            let mut shell = Terminal::new(cols, 4, 1000);
            shell.feed(b"new prompt> \x1b[31m\x1b]8;;https://example.org\x07\x1b[");
            let link = shell.primary.cursor.hyperlink.clone();
            finish_restore(&mut history, &mut shell).unwrap();
            shell.feed(b"1mbold");
            let output = text(&shell);
            assert!(output.find("old transcript").unwrap() < output.find(RESTORED_MARKER).unwrap());
            assert!(output.find(RESTORED_MARKER).unwrap() < output.find("new prompt").unwrap());
            assert_eq!(shell.primary.cursor.style.foreground, Color::Indexed(1));
            assert!(shell.primary.cursor.style.bold);
            assert!(shell.primary.cursor.hyperlink.is_some());
            assert_eq!(shell.primary.cursor.hyperlink, link);
        }
    }

    #[test]
    fn restored_band_is_centered_and_leaves_new_output_unstyled() {
        for cols in [1, 24, 47, 48, 49, 50, 51, 52, 53, 80, 81, 120] {
            let mut source = Terminal::new(80, 3, 1000);
            source.feed(b"old display");
            let mut destination = Terminal::new(cols, 3, 1000);
            wire(&source).restore_into(&mut destination).unwrap();
            insert_restored_marker(&mut destination);
            assert_eq!(destination.screen().cursor.style, Default::default());
            destination.feed(b"new prompt");

            let displayed = text(&destination);
            let lines: Vec<_> = displayed.lines().filter(|line| !line.is_empty()).collect();
            assert_eq!(lines.len(), 3, "unexpected display: {displayed:?}");
            assert_eq!(lines[0], "old display");
            assert_eq!(lines[2], "new prompt");
            let banner = lines[1];
            assert_eq!(
                banner.chars().count(),
                usize::from(cols).max(RESTORED_MARKER.len())
            );
            let (left, right) = banner.split_once(RESTORED_MARKER).unwrap();
            assert!(left.chars().count().abs_diff(right.chars().count()) <= 1);
            assert!(
                left.chars()
                    .chain(right.chars())
                    .all(|ch| ch == '█' || ch == ' ')
            );
            if usize::from(cols) <= RESTORED_MARKER.len() {
                assert_eq!(banner, RESTORED_MARKER);
                assert!(
                    destination
                        .screen()
                        .all_rows()
                        .all(|row| row.cells().len() <= usize::from(cols))
                );
            }
            let cells: Vec<_> = destination
                .screen()
                .all_rows()
                .flat_map(|row| {
                    row.cells()
                        .iter()
                        .enumerate()
                        .filter_map(move |(col, cell)| {
                            cell.codepoint().map(|ch| (ch, row.style(col)))
                        })
                })
                .collect();
            for (ch, style) in &cells["old display".len()..][..banner.chars().count()] {
                assert!(style.bold);
                assert!(!style.inverse);
                if *ch != '█' && *ch != ' ' {
                    assert_eq!(style.foreground, Color::Default);
                }
            }
            for (_, style) in &cells["old display".len() + banner.chars().count()..] {
                assert_eq!(*style, Default::default());
            }
        }
    }

    #[test]
    fn restored_band_alternates_subtle_shades_of_the_receiving_background() {
        for (background, colors) in [
            ([0; 3], [[12; 3], [20; 3]]),
            ([24, 32, 40], [[36, 44, 52], [44, 52, 60]]),
            ([255; 3], [[243; 3], [235; 3]]),
            ([232, 240, 248], [[220, 228, 236], [212, 220, 228]]),
            ([255, 0, 128], [[255, 12, 140], [255, 20, 148]]),
        ] {
            let mut source = Terminal::new(80, 3, 1000);
            source.set_default_colors(None, Some([100; 3]), None, &[]);
            source.feed(b"old display");
            let mut destination = Terminal::new(80, 3, 1000);
            destination.set_default_colors(None, Some(background), None, &[]);
            wire(&source).restore_into(&mut destination).unwrap();
            insert_restored_marker(&mut destination);
            let row = destination
                .screen()
                .all_rows()
                .find(|row| destination.screen().row_text(row).contains(RESTORED_MARKER))
                .unwrap();
            let mut block = 0;
            for (col, cell) in row.cells().iter().enumerate() {
                if cell.codepoint() == Some('█') {
                    let [r, g, b] = colors[block % 2];
                    assert_eq!(row.style(col).foreground, Color::Rgb(r, g, b));
                    assert_eq!(row.style(col).background, Color::Default);
                    block += 1;
                } else {
                    block = 0;
                }
            }
            assert!(block > 1);
        }
    }

    #[test]
    fn empty_display_omits_banner_but_scrollback_keeps_it() {
        for (history, alternate) in [(false, false), (false, true), (true, false), (true, true)] {
            let mut source = Terminal::new(80, 3, 1000);
            if history {
                source.feed(b"old history");
            }
            source.feed(b"\r\n\r\n\r\n\r\n\x1b[2J\x1b[H   ");
            if alternate {
                source.feed(b"\x1b[?1049h");
            }
            assert!(
                source.screen().viewport().all(|row| source
                    .screen()
                    .row_text(row)
                    .trim()
                    .is_empty())
            );
            let mut destination = Terminal::new(120, 3, 1000);
            wire(&source).restore_into(&mut destination).unwrap();
            let mut fresh = Terminal::new(120, 3, 1000);
            fresh.feed(b"new prompt");
            finish_restore(&mut destination, &mut fresh).unwrap();
            destination = fresh;
            let displayed = text(&destination);
            assert_eq!(displayed.contains("old history"), history);
            assert!(displayed.contains("new prompt"));
            assert_eq!(displayed.contains(RESTORED_MARKER), history);
            assert_eq!(destination.screen().cursor.style, Default::default());
        }
    }

    #[test]
    fn alternate_display_follows_all_primary_rows_without_live_state() {
        let mut source = Terminal::new(100, 3, 100);
        source.feed(b"oldest\r\nsecond\r\nthird\r\nprimary prompt");
        source.feed(b"\x1b[?1049h\x1b[?2004h\x1b[?1003h\x1b[?25l\x1b[4;2H");
        source.feed(b"\x1b[31m\x1b]8;id=a;https://example.org\x07TUI");
        source.feed(b"\x1b]52;c;unfinished");
        let before = snapshot::encode_to_vec(&source).unwrap();
        let mut destination = Terminal::new(100, 3, 100);
        destination.set_default_cursor(CursorShape::Bar, Some(false));
        wire(&source).restore_into(&mut destination).unwrap();
        assert_eq!(snapshot::encode_to_vec(&source).unwrap(), before);
        assert!(source.is_alternate_screen());
        assert!(!destination.is_alternate_screen());
        assert!(!destination.modes.dec(2004));
        assert_eq!(destination.mouse_mode, 0);
        assert!(destination.screen().cursor.visible);
        assert_eq!(destination.screen().cursor.shape, CursorShape::Bar);
        assert!(destination.screen().cursor.hyperlink.is_none());
        assert_eq!(destination.screen().cursor.style, Default::default());
        let mut fresh = Terminal::new(100, 3, 100);
        fresh.feed(b"new prompt");
        finish_restore(&mut destination, &mut fresh).unwrap();
        destination = fresh;
        let displayed = text(&destination);
        let expected = [
            "oldest",
            "second",
            "third",
            "primary prompt",
            "TUI",
            RESTORED_MARKER,
            "new prompt",
        ];
        let mut previous = 0;
        for item in expected {
            let position = displayed
                .find(item)
                .unwrap_or_else(|| panic!("missing {item:?} in {displayed:?}"));
            assert!(position >= previous);
            previous = position;
        }
        let linked = destination
            .screen()
            .all_rows()
            .find(|row| destination.screen().row_text(row).contains("TUI"))
            .unwrap();
        assert_eq!(linked.hyperlink(1).unwrap().uri, "https://example.org");
    }

    #[test]
    fn historical_colors_and_graphemes_survive_new_theme_and_resize() {
        let mut source = Terminal::new(20, 3, 20);
        source.set_default_colors(Some([12, 34, 56]), Some([6, 5, 4]), None, &[]);
        source.palette[1] = [90, 80, 70];
        source.feed("\x1b[31;1mred a\u{301}界\x1b[0m\r\nplain".as_bytes());
        let mut destination = Terminal::new(30, 4, 20);
        destination.set_default_colors(Some([200; 3]), Some([1; 3]), None, &[]);
        wire(&source).restore_into(&mut destination).unwrap();
        assert_eq!(destination.foreground, [200; 3]);
        assert_eq!(destination.background, [1; 3]);
        let colored = destination
            .screen()
            .all_rows()
            .find(|row| destination.screen().row_text(row).contains("red"))
            .unwrap();
        assert_eq!(colored.style(0).foreground, Color::Rgb(90, 80, 70));
        assert_eq!(colored.style(0).background, Color::Rgb(6, 5, 4));
        assert!(colored.style(0).bold);
        assert_eq!(colored.text(4).as_str(), "a\u{301}");
        let plain = destination
            .screen()
            .all_rows()
            .find(|row| destination.screen().row_text(row).contains("plain"))
            .unwrap();
        assert_eq!(plain.style(0).foreground, Color::Rgb(12, 34, 56));
        destination.feed(b"new");
        let cursor = &destination.screen().cursor;
        assert_eq!(
            destination.screen().row(cursor.row).style(0).foreground,
            Color::Default
        );
    }

    #[test]
    fn history_pages_survive_tmux_limits_and_reflow() {
        let mut source = Terminal::new(100, 3, 1000);
        source.set_tmux_strings(true);
        source.set_tmux_history_limit(500);
        for row in 0..800 {
            source.feed(format!("{row:04}: {}\r\n", "wide history ".repeat(5)).as_bytes());
        }
        source.resize(47, 5);
        let restored = wire(&source).decode().unwrap();
        assert_eq!(restored.primary.history_len(), source.primary.history_len());
        let rows = |screen: &Screen| {
            screen
                .all_rows()
                .map(|row| screen.row_text(row))
                .collect::<Vec<_>>()
        };
        assert_eq!(rows(&restored.primary), rows(&source.primary));
        assert!(restored.primary.tmux_history.is_none());
    }

    #[test]
    fn image_id_collisions_and_unicode_placeholders_keep_both_screens() {
        let mut source = Terminal::new(100, 4, 100);
        source.feed(b"\x1b_Ga=T,i=1,f=32,s=1,v=1;AQID/w==\x1b\\");
        source.feed(b"\x1b[?1049h");
        source.feed(b"\x1b_Ga=T,i=1,f=32,s=1,v=1;BAYI/w==\x1b\\");
        source.feed(b"\x1b[38;2;0;0;1m");
        source.feed("\u{10eeee}".as_bytes());
        source.feed(b"\x1b_Gi=3,f=32,s=1,v=1,m=1;AQI=\x1b\\");
        let mut destination = Terminal::new(100, 4, 100);
        wire(&source).restore_into(&mut destination).unwrap();
        let graphics = &destination.screen().graphics;
        assert_eq!(graphics.images.len(), 2);
        assert_eq!(graphics.images[&1].pixels.as_slice(), &[1, 2, 3, 255]);
        assert_eq!(graphics.images[&2].pixels.as_slice(), &[4, 6, 8, 255]);
        assert_eq!(graphics.placements.len(), 2);
        let mut anchors: Vec<_> = graphics.placements.iter().map(|p| p.row).collect();
        anchors.sort();
        anchors.dedup();
        assert_eq!(anchors.len(), 2);
        let placeholder = destination
            .screen()
            .all_rows()
            .flat_map(|row| graphics::unicode::placements(destination.screen(), row))
            .next()
            .unwrap();
        assert_eq!(placeholder.image_id, 2);
        assert!(destination.feed(b"plain").is_empty());
    }

    #[test]
    fn malformed_visuals_fail_before_replacing_destination() {
        let source = Terminal::new(10, 2, 10);
        let mut destination = Terminal::new(10, 2, 10);
        destination.feed(b"keep");
        let before = snapshot::encode_to_vec(&destination).unwrap();
        let mut invalid = wire(&source);
        invalid.checkpoint.push(0);
        assert!(invalid.restore_into(&mut destination).is_err());
        assert_eq!(snapshot::encode_to_vec(&destination).unwrap(), before);
        let mut invalid = wire(&source);
        invalid.version += 1;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn extreme_imported_row_ids_are_normalized_before_resize_and_new_output() {
        let mut source = Terminal::new(10, 2, 10);
        source.feed(b"history\r\nlast row");
        let mut value = serde_json::to_value(wire(&source)).unwrap();
        value["screen"]["next_row"] = u64::MAX.into();
        let rows = value["screen"]["rows"].as_array_mut().unwrap();
        let count = rows.len() as u64;
        for (index, row) in rows.iter_mut().enumerate() {
            row[0] = (u64::MAX - count + index as u64).into();
        }
        for (cols, rows) in [(1, 1), (37, 11)] {
            let visual: VisualSnapshot = serde_json::from_value(value.clone()).unwrap();
            visual.validate().unwrap();
            let visual: VisualSnapshot = serde_json::from_value(value.clone()).unwrap();
            let mut destination = Terminal::new(cols, rows, 1000);
            visual.restore_into(&mut destination).unwrap();
            assert!(destination.primary.next_row < 1000);
            for _ in 0..100 {
                destination.feed(b"next\r\n");
            }
            assert!(destination.primary.next_row < 2000);
            assert!(text(&destination).contains("next"));
        }
    }
}
