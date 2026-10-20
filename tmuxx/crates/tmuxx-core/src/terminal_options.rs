use super::*;

pub(super) fn colour(value: &str) -> Option<[u8; 3]> {
    let value = value.to_ascii_lowercase();
    let ansi = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    let index = value
        .strip_prefix("colour")
        .or_else(|| value.strip_prefix("color"))
        .and_then(|v| v.parse::<u8>().ok())
        .or_else(|| ansi.iter().position(|name| *name == value).map(|i| i as u8))
        .or_else(|| {
            value
                .parse::<u8>()
                .ok()
                .filter(|n| n.to_string() == value)
                .and_then(|n| match n {
                    0..=7 => Some(n),
                    90..=97 => Some(n - 82),
                    _ => None,
                })
        })
        .or_else(|| {
            value
                .strip_prefix("bright")
                .and_then(|v| ansi.iter().position(|name| *name == v))
                .map(|i| i as u8 + 8)
        });
    if let Some(index) = index {
        let ansi = [
            0x000000u32,
            0x800000,
            0x008000,
            0x808000,
            0x000080,
            0x800080,
            0x008080,
            0xc0c0c0,
            0x808080,
            0xff0000,
            0x00ff00,
            0xffff00,
            0x0000ff,
            0xff00ff,
            0x00ffff,
            0xffffff,
        ];
        if index < 16 {
            let rgb = ansi[usize::from(index)];
            return Some([(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8]);
        }
        return Some(Terminal::default_palette()[usize::from(index)]);
    }
    if value.starts_with('#') {
        return (value.len() == 7 && value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit))
            .then(|| tmuxx_terminal::parse_color(&value))
            .flatten();
    }
    if value.trim() != value
        || value.contains([':', '/'])
        || value.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    tmuxx_terminal::parse_color(&value)
}
fn width_override(value: &str) -> Option<(u32, u32, u8)> {
    let (codepoint, width) = value.split_once('=')?;
    let width = width.parse::<u8>().ok().filter(|w| *w <= 2)?;
    let (start, end) = if let Some(codepoint) = codepoint.strip_prefix("U+") {
        let (start, end) = codepoint
            .split_once("-U+")
            .unwrap_or((codepoint, codepoint));
        (
            u32::from_str_radix(start, 16).ok()?,
            u32::from_str_radix(end, 16).ok()?,
        )
    } else {
        let mut chars = codepoint.chars();
        let cp = chars.next()? as u32;
        if chars.next().is_some() {
            return None;
        }
        (cp, cp)
    };
    (start > 0 && start <= end && end <= i32::MAX as u32).then_some((start, end, width))
}
impl Engine {
    pub(super) fn synchronize_terminal_options(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        let widths = self
            .option_array(None, "codepoint-widths")
            .iter()
            .filter_map(|(_, value)| width_override(value))
            .collect::<Vec<_>>();
        let vs16 = self.global_option("variation-selector-always-wide") != Some("off");
        let extended = match self.global_option("extended-keys") {
            Some("always") => 2,
            Some("on") => 1,
            _ => 0,
        };
        for id in self.panes.keys().copied().collect::<Vec<_>>() {
            let alternate = self.pane_option(id, "alternate-screen") != Some("off");
            let scroll = self.pane_option(id, "scroll-on-clear") != Some("off");
            let title = self.pane_option(id, "allow-set-title") != Some("off");
            let style = [
                "default",
                "blinking-block",
                "block",
                "blinking-underline",
                "underline",
                "blinking-bar",
                "bar",
            ]
            .iter()
            .position(|value| Some(*value) == self.pane_option(id, "cursor-style"))
            .unwrap_or(0) as u16;
            let color = colour(&self.cursor_default_color(id));
            let mut palette = Terminal::default_palette();
            for (index, value) in self.pane_option_array(id, "pane-colours") {
                if let (Ok(index), Some(color)) = (index.parse::<u8>(), colour(&value)) {
                    palette[usize::from(index)] = color;
                }
            }
            let pane_style = self.pane_style(id);
            let terminal = &mut self.panes.get_mut(&id).unwrap().terminal;
            terminal.set_extended_keys(extended);
            terminal.set_allow_set_title(title);
            terminal.set_width_options(&widths, vs16);
            terminal.set_screen_options(alternate, scroll);
            terminal.set_cursor_options(style, color, &palette);
            effects.extend(
                terminal
                    .set_pane_style(pane_style)
                    .into_iter()
                    .filter_map(|effect| match effect {
                        tmuxx_terminal::Effect::Write(data) => {
                            Some(Effect::Write { pane: id, data })
                        }
                        _ => None,
                    }),
            );
        }
        effects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command(engine: &mut Engine, context: &mut ClientContext, source: &str) {
        for command in tmuxx_command::parse(source.as_bytes()).unwrap() {
            engine.execute(&command, context).unwrap();
        }
    }
    #[test]
    fn screen_cursor_palette_options_apply_and_application_overrides_survive() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        command(&mut engine, &mut context, "new-session -d ''");
        command(
            &mut engine,
            &mut context,
            "set-option -p alternate-screen off; set-option -p scroll-on-clear off; set-option -p cursor-style bar; set-option -p cursor-colour '#123456'; set-option -p pane-colours[5] '#abcdef'",
        );
        engine.feed(0, b"main\x1b[?1049halternate\x1b[?1049l");
        let state = engine.panes[&0].terminal.snapshot();
        assert!(!state.alternate_active);
        assert!(state.alternate.is_none());
        assert_eq!(state.cursor_color, Some([0x12, 0x34, 0x56]));
        assert_eq!(state.palette[5], [0xab, 0xcd, 0xef]);
        assert_eq!(state.primary.cursor.shape, tmuxx_terminal::CursorShape::Bar);
        engine.feed(0, b"\x1b[2J");
        assert_eq!(
            engine.panes[&0].terminal.snapshot().primary.history_len(),
            0
        );
        command(
            &mut engine,
            &mut context,
            "set-option -p scroll-on-clear on; set-option -p alternate-screen on",
        );
        engine.feed(0, b"\x1b[Hhistory\x1b[2J");
        assert_eq!(
            engine.panes[&0].terminal.snapshot().primary.history_len(),
            1
        );
        engine.feed(0, b"\x1b]12;#654321\x07\x1b]4;5;#fedcba\x07\x1b[3 q");
        command(
            &mut engine,
            &mut context,
            "set-option -p cursor-style block; set-option -p cursor-colour '#112233'; set-option -p pane-colours[5] '#223344'",
        );
        let state = engine.panes[&0].terminal.snapshot();
        assert_eq!(state.cursor_color, Some([0x65, 0x43, 0x21]));
        assert_eq!(state.palette[5], [0xfe, 0xdc, 0xba]);
        assert_eq!(
            state.primary.cursor.shape,
            tmuxx_terminal::CursorShape::Underline
        );
        engine.feed(0, b"\x1b]112\x07\x1b]104;5\x07\x1b[0 q");
        let state = engine.panes[&0].terminal.snapshot();
        assert_eq!(state.cursor_color, Some([0x11, 0x22, 0x33]));
        assert_eq!(state.palette[5], [0x22, 0x33, 0x44]);
        assert_eq!(
            state.primary.cursor.shape,
            tmuxx_terminal::CursorShape::Block
        );
    }
}
